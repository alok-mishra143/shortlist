use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::StatefulInteractiveElement;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Icon, IconName, Theme, ThemeMode, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::{Animation, AnimationExt, FontWeight};
use markdown_tools::{BlockKind, TextEdit};
use shortlist_ai::OpenCode;
use shortlist_core::{Candidate, ChatMessage, ChatSession, ModelRef, human_relative, now_ms};
use shortlist_embed::Embedder;
use shortlist_parse::parse_resume;
use shortlist_pipeline::{build_context, build_prompt};
use shortlist_store::Store;

mod markdown_tools;

struct ImportProgress {
    total: usize,
    done: usize,
    current: String,
    step: String,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Route {
    Candidates,
    Chat,
    Settings,
}

#[derive(Clone, Copy)]
struct Palette {
    bg: u32,
    sidebar: u32,
    card: u32,
    ink: u32,
    muted: u32,
    border: u32,
    accent: u32,
    accent_soft: u32,
    chip: u32,
    danger: u32,
    success: u32,
    on_accent: u32,
    hover: u32,
}

impl Palette {
    fn light() -> Self {
        Self {
            bg: 0xFBFAF7,
            sidebar: 0xF3F1EA,
            card: 0xFFFFFF,
            ink: 0x1B1B19,
            muted: 0x716D63,
            border: 0xE4E0D6,
            accent: 0x1F5C43,
            accent_soft: 0xE7F0EA,
            chip: 0xF1EFE8,
            danger: 0xA33B2B,
            success: 0x2E7D5B,
            on_accent: 0xFFFFFF,
            hover: 0xE9E5DC,
        }
    }

    fn dark() -> Self {
        Self {
            bg: 0x000000,
            sidebar: 0x000000,
            card: 0x0B0B0B,
            ink: 0xF5F5F5,
            muted: 0x9A9A9A,
            border: 0x222222,
            accent: 0x7ED3AE,
            accent_soft: 0x10251A,
            chip: 0x161616,
            danger: 0xF08C7A,
            success: 0x6FCF97,
            on_accent: 0x07120C,
            hover: 0x1A1A1A,
        }
    }
}

struct ShortlistApp {
    route: Route,
    store: Arc<Store>,
    candidates: Vec<Candidate>,
    library_candidates: Vec<Candidate>,
    search: Entity<InputState>,
    composer: Entity<InputState>,
    sessions: Vec<ChatSession>,
    active_session: Option<String>,
    messages: Vec<ChatMessage>,
    models: Vec<ModelRef>,
    selected_model: Option<ModelRef>,
    embedder: Arc<Mutex<Option<Arc<Embedder>>>>,
    opencode: String,
    index_status: String,
    busy: bool,
    notice: Option<String>,
    confirm_clear: bool,
    confirm_delete: Option<String>,
    import: Option<ImportProgress>,
    choosing_files: bool,
    dark: bool,
    palette: Palette,
    sidebar_open: bool,
    selected_candidate: Option<String>,
    reextracting: Option<String>,
    streaming: Option<String>,
    _subs: Vec<Subscription>,
}

impl ShortlistApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = Arc::new(
            Store::open(&shortlist_core::db_path())
                .unwrap_or_else(|_| Store::open_in_memory().expect("in-memory store")),
        );

        repair_candidate_names(&store);

        if std::env::var("SHORTLIST_DEMO").ok().as_deref() == Some("1")
            && store.count().unwrap_or(0) == 0
        {
            let _ = store.seed_demo();
        }

        let candidates = store.list_candidates("", 500).unwrap_or_default();
        let library_candidates = store.list_candidates("", 5000).unwrap_or_default();
        let sessions = store.list_sessions().unwrap_or_default();
        let active_session = sessions.first().map(|s| s.id.clone());
        let messages = active_session
            .as_deref()
            .and_then(|id| store.messages(id).ok())
            .unwrap_or_default();

        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search name, skill, or company"));
        let composer = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Ask anything about your candidates…")
        });

        let search_sub = cx.subscribe_in(&search, window, |this, _state, event, _window, cx| {
            if matches!(event, InputEvent::Change) {
                this.reload_candidates(cx);
            }
        });
        let composer_sub = cx.subscribe_in(&composer, window, |this, state, event, window, cx| {
            if let InputEvent::PressEnter { shift, .. } = event {
                if !*shift {
                    let value = state.read(cx).value().to_string();
                    if !value.trim().is_empty() {
                        this.send(value, window, cx);
                    }
                }
            }
        });
        let route = match std::env::var("SHORTLIST_ROUTE").ok().as_deref() {
            Some("chat") => Route::Chat,
            Some("settings") => Route::Settings,
            _ => Route::Candidates,
        };

        let dark = store.get_setting("theme").ok().flatten().as_deref() == Some("dark");
        let palette = if dark {
            Palette::dark()
        } else {
            Palette::light()
        };
        if dark {
            Theme::change(ThemeMode::Dark, None, cx);
            apply_dark_surfaces(cx);
        }
        let sidebar_open = store.get_setting("sidebar_open").ok().flatten().as_deref() != Some("0");

        let app = Self {
            route,
            store,
            candidates,
            library_candidates,
            search,
            composer,
            sessions,
            active_session,
            messages,
            models: Vec::new(),
            selected_model: None,
            embedder: Arc::new(Mutex::new(None)),
            opencode: "Checking OpenCode…".into(),
            index_status: "Semantic index not built".into(),
            busy: false,
            notice: None,
            confirm_clear: false,
            confirm_delete: None,
            import: None,
            choosing_files: false,
            dark,
            palette,
            sidebar_open,
            selected_candidate: None,
            reextracting: None,
            streaming: None,
            _subs: vec![search_sub, composer_sub],
        };
        cx.defer_in(window, |this, _window, cx| {
            this.load_opencode(cx);
            this.load_embedder(cx);
        });

        if let Ok(id) = std::env::var("SHORTLIST_OPEN_CANDIDATE") {
            cx.defer_in(window, move |this, _window, cx| {
                this.selected_candidate = Some(id.clone());
                cx.notify();
            });
        }

        if let Ok(id) = std::env::var("SHORTLIST_NOTE_DIALOG") {
            cx.defer_in(window, move |this, window, cx| {
                this.open_note_dialog(id.clone(), window, cx);
            });
        }

        if std::env::var("SHORTLIST_SETTINGS_DIALOG").is_ok() {
            cx.defer_in(window, |this, window, cx| {
                this.open_model_dialog(window, cx);
            });
        }

        if let Ok(prompt) = std::env::var("SHORTLIST_SMOKE_CHAT") {
            cx.defer_in(window, move |this, window, cx| {
                if this.selected_model.is_none() {
                    this.selected_model = Some(ModelRef {
                        id: "deepseek-v4.1-flash".into(),
                        provider_id: "opencode-go".into(),
                        name: "DeepSeek V4.1 Flash".into(),
                    });
                }
                this.new_chat(cx);
                this.send(prompt, window, cx);
            });
        }

        app
    }

    fn reload_candidates(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_string();
        self.candidates = self.store.list_candidates(&query, 500).unwrap_or_default();
        cx.notify();
    }

    fn reload_library(&mut self, cx: &mut Context<Self>) {
        self.library_candidates = self.store.list_candidates("", 5000).unwrap_or_default();
        cx.notify();
    }

    fn reload_sessions(&mut self, cx: &mut Context<Self>) {
        self.sessions = self.store.list_sessions().unwrap_or_default();
        cx.notify();
    }

    fn reload_messages(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = &self.active_session {
            self.messages = self.store.messages(id).unwrap_or_default();
        } else {
            self.messages.clear();
        }
        cx.notify();
    }

    fn load_opencode(&mut self, cx: &mut Context<Self>) {
        let version = OpenCode.version();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { OpenCode.models().map_err(|e| e.to_string()) })
                .await;
            this.update(cx, |this, _cx| match result {
                Ok(models) => {
                    this.opencode = format!("OpenCode {version} · {} models", models.len());
                    let saved = this.store.get_setting("model").ok().flatten();
                    let selected = saved
                        .and_then(|key| models.iter().find(|m| m.key() == key).cloned())
                        .or_else(|| default_model(&models));
                    this.selected_model = selected;
                    this.models = models;
                }
                Err(e) => {
                    this.opencode = format!("OpenCode unavailable · {e}");
                }
            })
            .ok();
        })
        .detach();
    }

    fn load_embedder(&mut self, cx: &mut Context<Self>) {
        let slot = self.embedder.clone();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { Embedder::load().map(Arc::new).map_err(|e| e.to_string()) })
                .await;
            let message = match loaded {
                Ok(emb) => {
                    *slot.lock().unwrap() = Some(emb);
                    "e5-small-v2 ready".to_string()
                }
                Err(e) => format!("Embeddings unavailable · {e}"),
            };
            this.update(cx, |this, cx| {
                this.index_status = message;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn import(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.import.is_some() || self.choosing_files {
            return;
        }

        // Ask through GPUI's own platform picker. A blocking dialog (rfd's
        // `pick_files`, which calls `NSOpenPanel::runModal`) spins a nested
        // AppKit run loop that re-enters GPUI's event loop and aborts with
        // "AppCell already borrowed".
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Import resumes".into()),
        });
        self.choosing_files = true;
        self.notice = None;
        cx.notify();

        let store = self.store.clone();

        cx.spawn(async move |this, cx| {
            let files = match picked.await {
                Ok(Ok(Some(files))) => files,
                Ok(Ok(None)) => {
                    this.update(cx, |this, cx| {
                        this.choosing_files = false;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Ok(Err(error)) => {
                    this.update(cx, |this, cx| {
                        this.choosing_files = false;
                        this.notice = Some(format!("Could not open the file picker: {error}"));
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Err(_) => {
                    this.update(cx, |this, cx| {
                        this.choosing_files = false;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            if files.is_empty() {
                this.update(cx, |this, cx| {
                    this.choosing_files = false;
                    cx.notify();
                })
                .ok();
                return;
            }

            let cancel = Arc::new(AtomicBool::new(false));
            let prepared = this.update(cx, |this, cx| {
                this.choosing_files = false;
                this.import = Some(ImportProgress {
                    total: files.len(),
                    done: 0,
                    current: String::new(),
                    step: "Preparing".into(),
                    cancel: cancel.clone(),
                });
                cx.notify();
                (
                    this.embedder.lock().unwrap().clone(),
                    this.selected_model.clone(),
                )
            });
            let Ok((embedder, model)) = prepared else {
                return;
            };

            let mut added = 0usize;
            let mut skipped = 0usize;
            let mut first_error: Option<String> = None;
            let mut stopped = false;

            for path in files {
                if cancel.load(Ordering::Relaxed) {
                    stopped = true;
                    break;
                }
                let file_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("file")
                    .to_string();

                this.update(cx, |this, cx| {
                    if let Some(progress) = &mut this.import {
                        progress.current = file_name.clone();
                        progress.step = "Extracting text".into();
                    }
                    cx.notify();
                })
                .ok();

                let parse_path = path.clone();
                let parsed = cx
                    .background_executor()
                    .spawn(async move {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            parse_resume(&parse_path)
                        }))
                    })
                    .await;

                if cancel.load(Ordering::Relaxed) {
                    stopped = true;
                    break;
                }

                let mut candidate = match parsed {
                    Ok(Ok(candidate)) => candidate,
                    Ok(Err(error)) => {
                        skipped += 1;
                        if first_error.is_none() {
                            first_error = Some(format!("{file_name}: {error}"));
                        }
                        this.update(cx, |this, cx| {
                            if let Some(progress) = &mut this.import {
                                progress.done += 1;
                                progress.step = "Skipped".into();
                            }
                            cx.notify();
                        })
                        .ok();
                        continue;
                    }
                    Err(_) => {
                        skipped += 1;
                        if first_error.is_none() {
                            first_error = Some(format!("{file_name}: the parser crashed"));
                        }
                        this.update(cx, |this, cx| {
                            if let Some(progress) = &mut this.import {
                                progress.done += 1;
                                progress.step = "Skipped".into();
                            }
                            cx.notify();
                        })
                        .ok();
                        continue;
                    }
                };

                if let Some(ai_model) = &model {
                    this.update(cx, |this, cx| {
                        if let Some(progress) = &mut this.import {
                            progress.step = "Reading with AI".into();
                        }
                        cx.notify();
                    })
                    .ok();
                    let text_for_ai = candidate.text.clone();
                    let ai_model = ai_model.clone();
                    let extracted = cx
                        .background_executor()
                        .spawn(async move {
                            shortlist_ai::OpenCode
                                .extract_profile(Some(&ai_model), &text_for_ai)
                                .ok()
                        })
                        .await;
                    if let Some(profile) = extracted {
                        merge_extracted(&mut candidate, profile);
                    }
                    if cancel.load(Ordering::Relaxed) {
                        stopped = true;
                        break;
                    }
                }

                this.update(cx, |this, cx| {
                    if let Some(progress) = &mut this.import {
                        progress.step = "Building embeddings".into();
                    }
                    cx.notify();
                })
                .ok();

                let chunks = shortlist_core::chunk_text(&candidate.text, 180, 28);
                let store_for_task = store.clone();
                let embedder_for_task = embedder.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        if let Some(embedder) = &embedder_for_task {
                            match embedder.embed_passages(&chunks) {
                                Ok(vectors) => {
                                    store_for_task.insert_candidate(&candidate, &[])?;
                                    store_for_task.save_chunks_with_vectors(
                                        &candidate.id,
                                        &chunks,
                                        &vectors,
                                        shortlist_embed::MODEL_ID,
                                    )?;
                                }
                                Err(_) => {
                                    store_for_task.insert_candidate(&candidate, &chunks)?;
                                }
                            }
                        } else {
                            store_for_task.insert_candidate(&candidate, &chunks)?;
                        }
                        Ok::<(), anyhow::Error>(())
                    })
                    .await;

                match result {
                    Ok(()) => added += 1,
                    Err(error) => {
                        skipped += 1;
                        if first_error.is_none() {
                            first_error = Some(format!("{file_name}: {error}"));
                        }
                    }
                }

                this.update(cx, |this, cx| {
                    if let Some(progress) = &mut this.import {
                        progress.done += 1;
                        progress.step = "Saved".into();
                    }
                    this.reload_candidates(cx);
                })
                .ok();
            }

            this.update(cx, |this, cx| {
                let total = this.import.as_ref().map(|p| p.total).unwrap_or(0);
                this.import = None;
                this.notice = Some(if stopped {
                    format!(
                        "Import stopped after {} of {total} resumes.",
                        added + skipped
                    )
                } else if skipped == 0 {
                    format!("Imported {added} resume(s).")
                } else {
                    format!(
                        "Imported {added}, skipped {skipped}. {}",
                        first_error.unwrap_or_default()
                    )
                });
                this.reload_candidates(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn build_index(&mut self, cx: &mut Context<Self>) {
        let Some(emb) = self.embedder.lock().unwrap().clone() else {
            self.notice = Some("Embeddings are still loading. Try again in a moment.".into());
            cx.notify();
            return;
        };
        let store = self.store.clone();
        self.busy = true;
        self.index_status = "Building semantic index…".into();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let chunks = store.chunks_missing_vectors().map_err(|e| e.to_string())?;
                    let total = chunks.len();
                    let mut done = 0usize;
                    for batch in chunks.chunks(24) {
                        let texts: Vec<String> =
                            batch.iter().map(|(_, _, text)| text.clone()).collect();
                        let vectors = emb.embed_passages(&texts).map_err(|e| e.to_string())?;
                        for ((id, _, _), vector) in batch.iter().zip(vectors.iter()) {
                            let _ = store.set_chunk_vector(id, vector, shortlist_embed::MODEL_ID);
                        }
                        done += batch.len();
                    }
                    Ok::<(usize, usize), String>((done, total))
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                this.index_status = match result {
                    Ok((done, total)) => format!("Semantic index ready · {done}/{total} chunks"),
                    Err(e) => format!("Index failed · {e}"),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn new_chat(&mut self, cx: &mut Context<Self>) {
        let model = self.selected_model.clone();
        match OpenCode.create_session("New chat", model.as_ref()) {
            Ok(opencode_id) => match self.store.create_session("New chat") {
                Ok(session) => {
                    let _ = self
                        .store
                        .set_session_opencode_id(&session.id, &opencode_id);
                    self.active_session = Some(session.id.clone());
                    self.messages.clear();
                    self.notice = None;
                    self.reload_sessions(cx);
                }
                Err(e) => self.notice = Some(e.to_string()),
            },
            Err(e) => self.notice = Some(format!("Could not start a chat: {e}")),
        }
        cx.notify();
    }

    fn select_session(&mut self, id: String, cx: &mut Context<Self>) {
        self.active_session = Some(id);
        self.reload_messages(cx);
    }

    fn delete_chat(&mut self, id: String, cx: &mut Context<Self>) {
        let opencode_id = self
            .sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.opencode_id.clone())
            .unwrap_or_default();

        match self.store.delete_session(&id) {
            Ok(()) => {
                if self.active_session.as_deref() == Some(id.as_str()) {
                    self.active_session = None;
                }
                self.reload_sessions(cx);
                if self.active_session.is_none() {
                    self.active_session = self.sessions.first().map(|s| s.id.clone());
                }
                self.reload_messages(cx);
            }
            Err(e) => {
                self.notice = Some(format!("Could not delete chat: {e}"));
                cx.notify();
            }
        }

        if !opencode_id.is_empty() {
            cx.spawn(async move |_this, cx| {
                cx.background_executor()
                    .spawn(async move {
                        let _ = OpenCode.delete_session(&opencode_id);
                    })
                    .await;
            })
            .detach();
        }
    }

    fn select_model(&mut self, model: ModelRef, cx: &mut Context<Self>) {
        let _ = self.store.set_setting("model", &model.key());
        self.selected_model = Some(model.clone());
        if let Some(session_id) = &self.active_session {
            if let Some(session) = self.sessions.iter().find(|s| s.id == *session_id) {
                if !session.opencode_id.is_empty() {
                    let sid = session.opencode_id.clone();
                    cx.spawn(async move |_this, cx| {
                        cx.background_executor()
                            .spawn(async move {
                                let _ = OpenCode.set_model(&sid, &model);
                            })
                            .await;
                    })
                    .detach();
                }
            }
        }
        cx.notify();
    }

    fn send(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let local_id = match &self.active_session {
            Some(id) => id.clone(),
            None => {
                self.new_chat(cx);
                match &self.active_session {
                    Some(id) => id.clone(),
                    None => return,
                }
            }
        };
        let session = match self.sessions.iter().find(|s| s.id == local_id) {
            Some(s) => s.clone(),
            None => return,
        };
        if session.opencode_id.is_empty() {
            self.notice = Some("The chat is still being created. Try again in a second.".into());
            cx.notify();
            return;
        }
        let _ = self.store.add_message(&local_id, "user", &text);
        if let Some(session) = self.sessions.iter().find(|s| s.id == local_id) {
            if session.title == "New chat" {
                let title: String = text.chars().take(44).collect();
                let _ = self.store.rename_session(&local_id, &title);
            }
        }
        let _ = self.store.set_setting("composer_draft", "");
        self.composer
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.reload_messages(cx);

        self.busy = true;
        self.streaming = Some(String::new());
        self.notice = Some("Thinking…".into());
        let store = self.store.clone();
        let embedder = self.embedder.lock().unwrap().clone();
        let sid = session.opencode_id.clone();
        let local = local_id.clone();
        let model = self.selected_model.clone();
        let query = text.clone();
        let stream_state = Arc::new(Mutex::new(shortlist_ai::StreamState::default()));

        let state_for_task = stream_state.clone();
        let task = cx.background_executor().spawn(async move {
            if let Some(model) = &model {
                let _ = OpenCode.set_model(&sid, model);
            }
            let context = build_context(&store, embedder.as_deref(), &query, 4);
            let prompt = build_prompt(&context, &query);
            let result = OpenCode.stream_prompt(&sid, &prompt, state_for_task.clone());
            if let Err(e) = &result {
                let mut snapshot = state_for_task.lock().unwrap();
                snapshot.done = true;
                if snapshot.error.is_none() {
                    snapshot.error = Some(e.to_string());
                }
            }
            result
        });

        cx.spawn(async move |this, cx| {
            let started = now_ms();
            let mut last = String::new();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                let snapshot = stream_state.lock().unwrap().clone();
                if snapshot.text != last {
                    last = snapshot.text.clone();
                    let text = snapshot.text.clone();
                    this.update(cx, |this, cx| {
                        this.streaming = Some(text);
                        cx.notify();
                    })
                    .ok();
                }
                if snapshot.done {
                    break;
                }
                if now_ms() - started > 240_000 {
                    let mut state = stream_state.lock().unwrap();
                    state.done = true;
                    state.error = Some("Timed out waiting for the model.".into());
                    break;
                }
            }

            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(answer) => {
                        let _ = this.store.add_message(&local, "assistant", &answer);
                        this.notice = None;
                    }
                    Err(e) => {
                        let _ = this.store.add_message(
                            &local,
                            "assistant",
                            &format!("⚠ {e}\n\nPick another model in Settings if this one is not available."),
                        );
                        this.notice = Some("The model call failed.".into());
                    }
                }
                this.streaming = None;
                this.busy = false;
                this.reload_messages(cx);
                this.reload_sessions(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn clear_all(&mut self, cx: &mut Context<Self>) {
        let _ = self.store.clear_all();
        self.candidates.clear();
        self.library_candidates.clear();
        self.sessions.clear();
        self.messages.clear();
        self.active_session = None;
        self.confirm_clear = false;
        self.notice = Some("All candidates and chats removed.".into());
        cx.notify();
    }

    fn load_demo(&mut self, cx: &mut Context<Self>) {
        match self.store.seed_demo() {
            Ok(n) => {
                self.notice = Some(format!("Added {n} demo candidates."));
                self.reload_candidates(cx);
                self.reload_library(cx);
            }
            Err(e) => self.notice = Some(e.to_string()),
        }
    }
}

fn default_model(models: &[ModelRef]) -> Option<ModelRef> {
    models
        .iter()
        .find(|m| m.provider_id == "opencode-go" && m.id == "deepseek-v4.1-flash")
        .cloned()
        .or_else(|| {
            models
                .iter()
                .find(|m| m.provider_id == "opencode-go")
                .cloned()
        })
        .or_else(|| models.first().cloned())
}

impl Render for ShortlistApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_row()
            .bg(rgb(self.palette.bg))
            .text_color(rgb(self.palette.ink))
            .font_family(ui_font())
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .child(match self.route {
                        Route::Candidates => self.candidates_view(cx).into_any_element(),
                        Route::Chat => self.chat_view(cx).into_any_element(),
                        Route::Settings => self.settings_view(cx).into_any_element(),
                    }),
            )
            .when(self.selected_candidate.is_some(), |el| {
                el.child(self.candidate_detail(cx))
            })
    }
}

impl ShortlistApp {
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = !self.sidebar_open;
        let open = self.sidebar_open;
        div()
            .id(if open {
                "sidebar-open"
            } else {
                "sidebar-closed"
            })
            .h_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .bg(rgb(self.palette.sidebar))
            .border_r_1()
            .border_color(rgb(self.palette.border))
            .px_2()
            .pt_5()
            .pb_4()
            .child(self.brand_row(collapsed, cx))
            .child(div().h(px(24.)))
            .child(self.nav_item(
                "Candidates",
                Route::Candidates,
                Some(self.candidates.len()),
                IconName::User,
                cx,
            ))
            .child(self.nav_item("Chat", Route::Chat, None, IconName::Bot, cx))
            .child(self.nav_item("Settings", Route::Settings, None, IconName::Settings, cx))
            .child(div().flex_1())
            .child(self.theme_toggle(collapsed, cx))
            .with_animation(
                if open {
                    "sidebar-open-anim"
                } else {
                    "sidebar-closed-anim"
                },
                Animation::new(Duration::from_millis(220)),
                move |el, t| {
                    let width = if open {
                        76.0 + 148.0 * t
                    } else {
                        224.0 - 148.0 * t
                    };
                    el.w(px(width))
                },
            )
    }

    fn brand_row(&self, collapsed: bool, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .size(px(28.))
                    .rounded_md()
                    .bg(rgb(self.palette.accent))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(self.palette.on_accent))
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("S"),
            )
            .when(!collapsed, |el| {
                el.child(
                    div()
                        .flex_1()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Shortlist"),
                )
            })
            .child(
                div()
                    .id("collapse-sidebar")
                    .ml_auto()
                    .p_1()
                    .rounded_md()
                    .child(
                        Icon::new(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::PanelLeft
                        })
                        .text_color(rgb(self.palette.muted))
                        .w(px(16.))
                        .h(px(16.)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar_open = !this.sidebar_open;
                        let _ = this
                            .store
                            .set_setting("sidebar_open", if this.sidebar_open { "1" } else { "0" });
                        cx.notify();
                    })),
            )
    }

    fn nav_item(
        &self,
        label: &'static str,
        route: Route,
        count: Option<usize>,
        icon: IconName,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.route == route;
        let collapsed = !self.sidebar_open;
        let icon_color = if active {
            self.palette.accent
        } else {
            self.palette.muted
        };
        div()
            .id(SharedString::from(format!("nav-{label}")))
            .flex()
            .items_center()
            .gap_3()
            .px_2()
            .py_2()
            .mb_1()
            .rounded_md()
            .bg(if active {
                rgb(self.palette.accent_soft)
            } else {
                rgb(self.palette.sidebar)
            })
            .child(
                div()
                    .w(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(icon)
                            .text_color(rgb(icon_color))
                            .w(px(16.))
                            .h(px(16.)),
                    ),
            )
            .when(!collapsed, |el| {
                el.child(
                    div()
                        .flex_1()
                        .text_sm()
                        .font_weight(if active {
                            FontWeight::MEDIUM
                        } else {
                            FontWeight::NORMAL
                        })
                        .child(label),
                )
            })
            .when(!collapsed && count.is_some(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(rgb(self.palette.muted))
                        .child(count.map(|c| c.to_string()).unwrap_or_default()),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.route = route;
                cx.notify();
            }))
    }

    fn theme_toggle(&self, collapsed: bool, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("theme-toggle")
            .flex()
            .items_center()
            .gap_3()
            .px_2()
            .py_2()
            .rounded_md()
            .child(
                div()
                    .w(px(18.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(if self.dark {
                            IconName::Sun
                        } else {
                            IconName::Moon
                        })
                        .text_color(rgb(self.palette.muted))
                        .w(px(16.))
                        .h(px(16.)),
                    ),
            )
            .when(!collapsed, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(rgb(self.palette.muted))
                        .child(if self.dark { "Light mode" } else { "Dark mode" }),
                )
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx)))
    }

    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        self.dark = !self.dark;
        self.palette = if self.dark {
            Palette::dark()
        } else {
            Palette::light()
        };
        Theme::change(
            if self.dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            None,
            cx,
        );
        if self.dark {
            apply_dark_surfaces(cx);
        }
        let _ = self
            .store
            .set_setting("theme", if self.dark { "dark" } else { "light" });
        cx.notify();
    }

    fn page_header(&self, title: &str, subtitle: String) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title.to_string()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(self.palette.muted))
                            .child(subtitle),
                    ),
            )
    }

    fn candidates_view(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.candidates.len();
        let importing = self.import.is_some();
        let choosing = self.choosing_files;
        let compact = self.selected_candidate.is_some();
        let query = self.search.read(cx).value().to_string();
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .px_8()
            .pt_7()
            .pb_4()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Candidates"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(self.palette.muted))
                                    .child(format!(
                                        "{count} people · private, local, and searchable"
                                    )),
                            ),
                    )
                    .child(if importing || choosing {
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(self.palette.chip))
                            .text_xs()
                            .text_color(rgb(self.palette.muted))
                            .child(
                                Icon::new(IconName::Loader)
                                    .text_color(rgb(self.palette.muted))
                                    .w(px(14.))
                                    .h(px(14.)),
                            )
                            .child(if choosing {
                                "Choosing files…"
                            } else {
                                "Importing…"
                            })
                            .into_any_element()
                    } else {
                        div()
                            .id("import-resumes")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_4()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(self.palette.accent))
                            .text_color(rgb(self.palette.on_accent))
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .child(
                                Icon::new(IconName::Plus)
                                    .text_color(rgb(self.palette.on_accent))
                                    .w(px(14.))
                                    .h(px(14.)),
                            )
                            .child("Import resumes")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.import(window, cx);
                            }))
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py(px(6.))
                            .rounded_md()
                            .bg(rgb(self.palette.card))
                            .border_1()
                            .border_color(rgb(self.palette.border))
                            .child(
                                Icon::new(IconName::Search)
                                    .text_color(rgb(self.palette.muted))
                                    .w(px(15.))
                                    .h(px(15.)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .child(Input::new(&self.search).appearance(false)),
                            ),
                    )
                    .child(div().text_xs().text_color(rgb(self.palette.muted)).child(
                        if query.is_empty() {
                            format!("{count} in view")
                        } else {
                            format!("{count} matches")
                        },
                    )),
            )
            .when(self.notice.is_some(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(rgb(self.palette.muted))
                        .child(self.notice.clone().unwrap_or_default()),
                )
            })
            .when(self.import.is_some(), |el| el.child(self.import_banner(cx)))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(self.palette.border))
                    .bg(rgb(self.palette.card))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(self.table_header(compact))
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.))
                            .id("candidate-list")
                            .overflow_y_scroll()
                            .child(if self.candidates.is_empty() {
                                self.empty_candidates(cx)
                            } else {
                                div()
                                    .flex()
                                    .flex_col()
                                    .children(self.candidates.iter().enumerate().map(
                                        |(idx, candidate)| {
                                            self.candidate_row(idx, candidate, compact, cx)
                                        },
                                    ))
                                    .into_any_element()
                            }),
                    ),
            )
    }

    fn import_banner(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(progress) = &self.import else {
            return div().into_any_element();
        };
        let pct = if progress.total == 0 {
            0.0
        } else {
            progress.done as f32 / progress.total as f32
        };
        let stopping = progress.cancel.load(Ordering::Relaxed);
        let current = if progress.current.is_empty() {
            "Preparing…".to_string()
        } else {
            progress.current.clone()
        };
        div()
            .p_4()
            .rounded_lg()
            .bg(rgb(self.palette.card))
            .border_1()
            .border_color(rgb(self.palette.border))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(format!(
                                        "Importing {} of {}",
                                        (progress.done + 1).min(progress.total),
                                        progress.total
                                    )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(self.palette.muted))
                                    .child(current),
                            ),
                    )
                    .child(
                        div()
                            .id("stop-import")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(self.palette.danger))
                            .text_xs()
                            .text_color(rgb(self.palette.danger))
                            .child(if stopping { "Stopping…" } else { "Stop" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(progress) = &this.import {
                                    progress.cancel.store(true, Ordering::Relaxed);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(self.import_bar(pct))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(self.palette.muted))
                            .child(progress.step.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(self.palette.muted))
                            .child(format!("{:.0}%", pct * 100.0)),
                    ),
            )
            .into_any_element()
    }

    fn import_bar(&self, pct: f32) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(2.))
            .children((0..28).map(|index| {
                let filled = (index as f32 / 28.0) < pct;
                div().h(px(4.)).flex_1().rounded_full().bg(if filled {
                    rgb(self.palette.accent)
                } else {
                    rgb(self.palette.chip)
                })
            }))
    }

    fn empty_candidates(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .p_10()
            .child(
                div()
                    .size(px(44.))
                    .rounded_full()
                    .bg(rgb(self.palette.accent_soft))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(IconName::User)
                            .text_color(rgb(self.palette.accent))
                            .w(px(20.))
                            .h(px(20.)),
                    ),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("No resumes yet"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(self.palette.muted))
                    .child("Import a PDF or DOCX to build your private library."),
            )
            .child(
                div()
                    .id("empty-import")
                    .px_4()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(self.palette.accent))
                    .text_color(rgb(self.palette.on_accent))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Import resumes")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.import(window, cx);
                    })),
            )
            .into_any_element()
    }

    fn delete_candidate(&mut self, id: String, cx: &mut Context<Self>) {
        match self.store.delete_candidate(&id) {
            Ok(()) => {
                if self.selected_candidate.as_deref() == Some(id.as_str()) {
                    self.selected_candidate = None;
                }
                self.confirm_delete = None;
                self.notice = Some("Candidate removed.".into());
                self.reload_candidates(cx);
                self.reload_library(cx);
            }
            Err(error) => {
                self.notice = Some(format!("Could not delete candidate: {error}"));
                cx.notify();
            }
        }
    }

    fn table_header(&self, compact: bool) -> AnyElement {
        let group = div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .items_center()
            .gap(px(TABLE_GAP))
            .child(div().w(px(TABLE_AVATAR)))
            .when(compact, |el| {
                el.child(div().flex_1().min_w(px(0.)).child("Candidate"))
                    .child(div().w(px(TABLE_COMPACT_SKILLS)).child("Skills"))
            })
            .when(!compact, |el| {
                el.child(div().w(px(TABLE_CANDIDATE)).child("Candidate"))
                    .child(div().flex_1().min_w(px(0.)).child("Skills"))
                    .child(div().w(px(TABLE_LOCATION)).child("Location"))
                    .child(div().w(px(TABLE_EXP)).child("Exp"))
                    .child(div().w(px(TABLE_ADDED)).child("Added"))
            });
        div()
            .flex()
            .items_center()
            .gap(px(TABLE_GAP))
            .px_4()
            .py_2()
            .bg(rgb(self.palette.chip))
            .border_b_1()
            .border_color(rgb(self.palette.border))
            .text_xs()
            .text_color(rgb(self.palette.muted))
            .child(group)
            .child(div().w(px(TABLE_ACTIONS)))
            .into_any_element()
    }

    fn candidate_row(
        &self,
        idx: usize,
        c: &Candidate,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let select_id = c.id.clone();
        let delete_id = c.id.clone();
        let confirm = self.confirm_delete.as_deref() == Some(c.id.as_str());
        let selected = self.selected_candidate.as_deref() == Some(c.id.as_str());
        let hover = self.palette.hover;
        let accent_soft = self.palette.accent_soft;
        let skill_limit = if compact { 1 } else { 2 };

        let group = div()
            .id(SharedString::from(format!("candidate-select-{idx}")))
            .flex_1()
            .min_w(px(0.))
            .flex()
            .items_center()
            .gap(px(TABLE_GAP))
            .child(
                div()
                    .size(px(TABLE_AVATAR))
                    .rounded_full()
                    .bg(rgb(self.palette.accent_soft))
                    .border_1()
                    .border_color(rgb(self.palette.border))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(self.palette.accent))
                    .child(c.initials()),
            )
            .when(compact, |el| {
                el.child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .overflow_hidden()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .overflow_hidden()
                                .child(c.name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(self.palette.muted))
                                .overflow_hidden()
                                .child(c.headline.clone()),
                        ),
                )
                .child(
                    div()
                        .w(px(TABLE_COMPACT_SKILLS))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .gap_1()
                        .children(c.skills.iter().take(skill_limit).map(|skill| {
                            div()
                                .px_1()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(self.palette.chip))
                                .text_xs()
                                .text_color(rgb(self.palette.muted))
                                .child(skill.clone())
                        }))
                        .when(c.skills.len() > skill_limit, |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(self.palette.muted))
                                    .child(format!("+{}", c.skills.len() - skill_limit)),
                            )
                        }),
                )
            })
            .when(!compact, |el| {
                el.child(
                    div()
                        .w(px(TABLE_CANDIDATE))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .overflow_hidden()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .overflow_hidden()
                                .child(c.name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(self.palette.muted))
                                .overflow_hidden()
                                .child(c.headline.clone()),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .gap_1()
                        .children(c.skills.iter().take(skill_limit).map(|skill| {
                            div()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(self.palette.chip))
                                .text_xs()
                                .text_color(rgb(self.palette.muted))
                                .child(skill.clone())
                        }))
                        .when(c.skills.len() > skill_limit, |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(self.palette.muted))
                                    .child(format!("+{}", c.skills.len() - skill_limit)),
                            )
                        }),
                )
                .child(
                    div()
                        .w(px(TABLE_LOCATION))
                        .overflow_hidden()
                        .text_xs()
                        .text_color(rgb(self.palette.muted))
                        .child(if c.location.is_empty() {
                            "—".to_string()
                        } else {
                            c.location.clone()
                        }),
                )
                .child(
                    div().w(px(TABLE_EXP)).child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(self.palette.chip))
                            .text_xs()
                            .text_color(rgb(self.palette.muted))
                            .child(format!("{:.0} yrs", c.years)),
                    ),
                )
                .child(
                    div()
                        .w(px(TABLE_ADDED))
                        .text_xs()
                        .text_color(rgb(self.palette.muted))
                        .child(human_relative(c.created_at)),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected_candidate = Some(select_id.clone());
                cx.notify();
            }));

        div()
            .id(SharedString::from(format!("candidate-row-{idx}")))
            .flex()
            .items_center()
            .gap(px(TABLE_GAP))
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(self.palette.border))
            .bg(if selected {
                rgb(self.palette.accent_soft)
            } else {
                rgb(self.palette.card)
            })
            .hover(move |style| {
                style.bg(if selected {
                    rgb(accent_soft)
                } else {
                    rgb(hover)
                })
            })
            .child(group)
            .child(
                div()
                    .w(px(TABLE_ACTIONS))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .when(confirm, |el| {
                        el.child(
                            div()
                                .id(SharedString::from(format!("confirm-delete-{idx}")))
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(self.palette.danger))
                                .text_xs()
                                .text_color(rgb(self.palette.danger))
                                .child("Delete")
                                .on_click(cx.listener({
                                    let id = delete_id.clone();
                                    move |this, _, _, cx| {
                                        this.delete_candidate(id.clone(), cx);
                                    }
                                })),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("cancel-delete-{idx}")))
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .text_xs()
                                .text_color(rgb(self.palette.muted))
                                .child("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_delete = None;
                                    cx.notify();
                                })),
                        )
                    })
                    .when(!confirm, |el| {
                        el.child(
                            div()
                                .id(SharedString::from(format!("delete-candidate-{idx}")))
                                .size(px(26.))
                                .rounded_md()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    Icon::default()
                                        .data(TRASH_ICON)
                                        .text_color(rgb(self.palette.danger))
                                        .w(px(14.))
                                        .h(px(14.)),
                                )
                                .on_click(cx.listener({
                                    let id = delete_id.clone();
                                    move |this, _, _, cx| {
                                        this.confirm_delete = Some(id.clone());
                                        cx.notify();
                                    }
                                })),
                        )
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .text_color(rgb(self.palette.muted))
                                .w(px(14.))
                                .h(px(14.)),
                        )
                    }),
            )
            .into_any_element()
    }

    fn candidate_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(c) = self
            .selected_candidate
            .as_ref()
            .and_then(|id| self.library_candidates.iter().find(|c| &c.id == id))
        else {
            return div().into_any_element();
        };
        let candidate_id = c.id.clone();
        let reextracting = self.reextracting.as_deref() == Some(c.id.as_str());
        let summary = if c.summary.trim().is_empty() {
            "No summary extracted yet.".to_string()
        } else {
            c.summary.clone()
        };
        let experience = if c.years > 0.0 {
            format!("{:.1} years", c.years)
        } else {
            "—".into()
        };
        let location = if c.location.is_empty() {
            "—".into()
        } else {
            c.location.clone()
        };
        let email = if c.email.is_empty() {
            "—".into()
        } else {
            c.email.clone()
        };
        let phone = if c.phone.is_empty() {
            "—".into()
        } else {
            c.phone.clone()
        };
        let file = if c.file_name.is_empty() {
            "—".into()
        } else {
            c.file_name.clone()
        };
        let added = human_relative(c.created_at);
        let skills = c.skills.clone();

        div()
            .id("candidate-detail")
            .h_full()
            .overflow_hidden()
            .bg(rgb(self.palette.card))
            .border_l_1()
            .border_color(rgb(self.palette.border))
            .child(
                div()
                    .w(px(430.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(div().h(px(4.)).w_full().bg(rgb(self.palette.accent)))
                    .child(
                        div()
                            .p_5()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .border_b_1()
                            .border_color(rgb(self.palette.border))
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .justify_between()
                                    .gap_4()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .child(
                                                div()
                                                    .size(px(54.))
                                                    .rounded_full()
                                                    .bg(rgb(self.palette.accent_soft))
                                                    .border_1()
                                                    .border_color(rgb(self.palette.border))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .text_lg()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(self.palette.accent))
                                                    .child(c.initials()),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(rgb(self.palette.muted))
                                                            .child("Candidate profile"),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xl()
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .child(c.name.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(rgb(self.palette.muted))
                                                            .child(c.headline.clone()),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("close-detail")
                                            .p_1()
                                            .rounded_md()
                                            .child(
                                                div()
                                                    .text_lg()
                                                    .text_color(rgb(self.palette.muted))
                                                    .child("×"),
                                            )
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.selected_candidate = None;
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("reextract-candidate")
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px_3()
                                            .py_2()
                                            .rounded_md()
                                            .bg(if reextracting {
                                                rgb(self.palette.chip)
                                            } else {
                                                rgb(self.palette.accent_soft)
                                            })
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(if reextracting {
                                                rgb(self.palette.muted)
                                            } else {
                                                rgb(self.palette.accent)
                                            })
                                            .child(
                                                Icon::new(IconName::Bot)
                                                    .text_color(if reextracting {
                                                        rgb(self.palette.muted)
                                                    } else {
                                                        rgb(self.palette.accent)
                                                    })
                                                    .w(px(14.))
                                                    .h(px(14.)),
                                            )
                                            .child(if reextracting {
                                                "Reading with AI…"
                                            } else {
                                                "Re-read with AI"
                                            })
                                            .on_click(cx.listener({
                                                let id = candidate_id.clone();
                                                move |this, _, _, cx| {
                                                    this.reextract_candidate(id.clone(), cx);
                                                }
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id("open-note")
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px_3()
                                            .py_2()
                                            .rounded_md()
                                            .bg(rgb(self.palette.card))
                                            .border_1()
                                            .border_color(rgb(self.palette.border))
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(rgb(self.palette.ink))
                                            .child(
                                                Icon::new(IconName::FileText)
                                                    .text_color(rgb(self.palette.ink))
                                                    .w(px(14.))
                                                    .h(px(14.)),
                                            )
                                            .child("Note")
                                            .on_click(cx.listener({
                                                let id = candidate_id.clone();
                                                move |this, _, window, cx| {
                                                    this.open_note_dialog(id.clone(), window, cx);
                                                }
                                            })),
                                    )
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(self.palette.muted))
                                    .child(
                                        "Notes stay on this device. AI re-read fixes name, role, skills, and location.",
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.))
                            .id(SharedString::from(format!(
                                "detail-scroll-{}",
                                candidate_id
                            )))
                            .overflow_y_scroll()
                            .p_5()
                            .flex()
                            .flex_col()
                            .gap_5()
                            .child(
                                div()
                                    .p_4()
                                    .rounded_lg()
                                    .bg(rgb(self.palette.bg))
                                    .border_1()
                                    .border_color(rgb(self.palette.border))
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .child(self.detail_label("At a glance"))
                                    .child(self.info_row("Location", location))
                                    .child(self.info_row("Experience", experience))
                                    .child(self.info_row("Email", email))
                                    .child(self.info_row("Phone", phone))
                                    .child(self.info_row("Added", added))
                                    .child(self.info_row("File", file)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(self.detail_label("Skills"))
                                    .child(if skills.is_empty() {
                                        div()
                                            .text_sm()
                                            .text_color(rgb(self.palette.muted))
                                            .child("No skills extracted yet.")
                                            .into_any_element()
                                    } else {
                                        div()
                                            .flex()
                                            .flex_row()
                                            .flex_wrap()
                                            .gap_2()
                                            .children(skills.iter().map(|skill| {
                                                div()
                                                    .px_2()
                                                    .py_1()
                                                    .rounded_md()
                                                    .bg(rgb(self.palette.chip))
                                                    .text_xs()
                                                    .text_color(rgb(self.palette.ink))
                                                    .child(skill.clone())
                                            }))
                                            .into_any_element()
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(self.detail_label("Summary"))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(if c.summary.trim().is_empty() {
                                                rgb(self.palette.muted)
                                            } else {
                                                rgb(self.palette.ink)
                                            })
                                            .child(summary),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(self.detail_label("Resume text"))
                                    .child(
                                        div()
                                            .p_4()
                                            .rounded_lg()
                                            .bg(rgb(self.palette.bg))
                                            .border_1()
                                            .border_color(rgb(self.palette.border))
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .children(c.text.lines().map(|line| {
                                                div()
                                                    .w_full()
                                                    .text_xs()
                                                    .text_color(rgb(self.palette.ink))
                                                    .child(if line.trim().is_empty() {
                                                        " ".to_string()
                                                    } else {
                                                        line.to_string()
                                                    })
                                            })),
                                    ),
                            ),
                    )
                    .with_animation(
                        SharedString::from(format!("detail-content-{}", candidate_id)),
                        Animation::new(Duration::from_millis(180)),
                        |el, t| el.opacity(0.2 + 0.8 * t),
                    ),
            )
            .with_animation(
                "candidate-detail-anim",
                Animation::new(Duration::from_millis(240)),
                |el, t| el.w(px(430. * t)),
            )
            .into_any_element()
    }

    fn detail_label(&self, label: &str) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(2.))
                    .h(px(12.))
                    .rounded_full()
                    .bg(rgb(self.palette.accent)),
            )
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(self.palette.muted))
                    .child(label.to_string()),
            )
    }

    fn info_row(&self, label: &str, value: String) -> impl IntoElement {
        div()
            .flex()
            .items_start()
            .gap_3()
            .child(
                div()
                    .w(px(84.))
                    .text_xs()
                    .text_color(rgb(self.palette.muted))
                    .child(label.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(rgb(self.palette.ink))
                    .child(value),
            )
    }

    fn reextract_candidate(&mut self, id: String, cx: &mut Context<Self>) {
        if self.reextracting.is_some() {
            return;
        }
        let Some(model) = self.selected_model.clone() else {
            self.notice = Some("Pick a model in Settings first.".into());
            cx.notify();
            return;
        };
        let Some(candidate) = self
            .library_candidates
            .iter()
            .find(|candidate| candidate.id == id)
            .cloned()
        else {
            return;
        };
        self.reextracting = Some(id);
        self.notice = Some("Reading the resume with AI…".into());
        cx.notify();

        let store = self.store.clone();
        let text = candidate.text.clone();
        cx.spawn(async move |this, cx| {
            let extracted = cx
                .background_executor()
                .spawn(async move {
                    shortlist_ai::OpenCode
                        .extract_profile(Some(&model), &text)
                        .ok()
                })
                .await;
            this.update(cx, |this, cx| {
                match extracted {
                    Some(profile) => {
                        let mut updated = candidate.clone();
                        merge_extracted(&mut updated, profile);
                        match store.update_candidate(&updated) {
                            Ok(()) => {
                                this.notice = Some("Profile refreshed with AI.".into());
                            }
                            Err(error) => {
                                this.notice = Some(format!("Could not save profile: {error}"));
                            }
                        }
                    }
                    None => {
                        this.notice =
                            Some("AI extraction failed; kept the existing profile.".into());
                    }
                }
                this.reextracting = None;
                this.reload_candidates(cx);
                this.reload_library(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn chat_view(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_row()
            .child(self.chat_rail(cx))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.chat_header(cx))
                    .child(self.chat_thread(cx))
                    .child(self.composer_view(cx)),
            )
    }

    fn new_chat_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = self.palette.hover;
        div()
            .id("new-chat")
            .h(px(36.))
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .rounded_md()
            .bg(rgb(self.palette.card))
            .border_1()
            .border_color(rgb(self.palette.border))
            .text_color(rgb(self.palette.ink))
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .hover(move |style| style.bg(rgb(hover)))
            .child(
                Icon::new(IconName::Plus)
                    .text_color(rgb(self.palette.accent))
                    .w(px(15.))
                    .h(px(15.)),
            )
            .child("New chat")
            .on_click(cx.listener(|this, _, _, cx| this.new_chat(cx)))
    }

    fn chat_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(236.))
            .h_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .bg(rgb(self.palette.sidebar))
            .border_r_1()
            .border_color(rgb(self.palette.border))
            .child(self.new_chat_button(cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .id("session-list")
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(self.sessions.iter().enumerate().map(|(idx, s)| {
                        let active = self.active_session.as_deref() == Some(s.id.as_str());
                        let id = s.id.clone();
                        let select_id = s.id.clone();
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(if active {
                                rgb(self.palette.card)
                            } else {
                                rgb(self.palette.sidebar)
                            })
                            .border_1()
                            .border_color(if active {
                                rgb(self.palette.border)
                            } else {
                                rgb(self.palette.sidebar)
                            })
                            .child(
                                div()
                                    .id(SharedString::from(format!("session-{idx}")))
                                    .flex_1()
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .px_1()
                                    .py_1()
                                    .child(
                                        div()
                                            .w_full()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(s.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(self.palette.muted))
                                            .child(human_relative(s.updated_at)),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select_session(select_id.clone(), cx);
                                    })),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("delete-{idx}")))
                                    .size(px(24.))
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        Icon::default()
                                            .data(TRASH_ICON)
                                            .text_color(rgb(self.palette.danger))
                                            .w(px(13.))
                                            .h(px(13.)),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete_chat(id.clone(), cx);
                                    })),
                            )
                    })),
            )
    }

    fn chat_header(&self, _cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .active_session
            .as_ref()
            .and_then(|id| self.sessions.iter().find(|s| s.id == *id))
            .map(|s| s.title.clone())
            .unwrap_or_else(|| "New chat".into());
        div()
            .h(px(72.))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_6()
            .px_6()
            .border_b_1()
            .border_color(rgb(self.palette.border))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(self.palette.muted))
                            .child("Answers grounded in your candidate library"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(6.))
                            .rounded_full()
                            .bg(if self.selected_model.is_some() {
                                rgb(self.palette.success)
                            } else {
                                rgb(self.palette.danger)
                            }),
                    )
                    .child(
                        div()
                            .max_w(px(260.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(rgb(self.palette.muted))
                            .child(
                                self.selected_model
                                    .as_ref()
                                    .map(|m| m.name.clone())
                                    .unwrap_or_else(|| "No model selected".into()),
                            ),
                    ),
            )
    }

    fn chat_thread(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_h(px(0.))
            .id("chat-thread")
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_6()
            .px_10()
            .py_8()
            .when(self.messages.is_empty() && !self.busy, |el| {
                el.items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(520.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Ask your candidate library"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(self.palette.muted))
                                    .child("Try “Who has shipped a Rust service?” or “Summarise the strongest backend profiles.”"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(self.palette.muted))
                                    .child("Answers use local embeddings and the files you imported."),
                            ),
                    )
            })
            .children(self.messages.iter().map(|m| self.message_bubble(m, cx)))
            .when(self.busy, |el| el.child(self.streaming_bubble(cx)))
    }

    fn message_bubble(&self, m: &ChatMessage, cx: &mut Context<Self>) -> AnyElement {
        if m.role == "user" {
            div()
                .w_full()
                .flex()
                .flex_row()
                .justify_end()
                .items_start()
                .gap_3()
                .child(
                    div()
                        .w_full()
                        .max_w(px(640.))
                        .px_4()
                        .py_3()
                        .rounded_lg()
                        .bg(rgb(self.palette.accent))
                        .text_color(rgb(self.palette.on_accent))
                        .text_sm()
                        .child(m.text.clone()),
                )
                .child(self.user_avatar())
                .into_any_element()
        } else {
            div()
                .w_full()
                .flex()
                .flex_row()
                .items_start()
                .gap_3()
                .child(self.assistant_avatar())
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(self.palette.muted))
                                .child("Shortlist"),
                        )
                        .child(
                            div()
                                .w_full()
                                .max_w(px(700.))
                                .px_4()
                                .py_3()
                                .rounded_lg()
                                .bg(rgb(self.palette.card))
                                .border_1()
                                .border_color(rgb(self.palette.border))
                                .text_sm()
                                .child(self.assistant_body(&m.text, cx)),
                        ),
                )
                .into_any_element()
        }
    }

    fn assistant_avatar(&self) -> impl IntoElement {
        div()
            .size(px(28.))
            .rounded_md()
            .bg(rgb(self.palette.accent))
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(self.palette.on_accent))
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .child("S")
    }

    fn user_avatar(&self) -> impl IntoElement {
        div()
            .size(px(28.))
            .rounded_full()
            .bg(rgb(self.palette.chip))
            .border_1()
            .border_color(rgb(self.palette.border))
            .flex()
            .items_center()
            .justify_center()
            .child(
                Icon::new(IconName::User)
                    .text_color(rgb(self.palette.muted))
                    .w(px(14.))
                    .h(px(14.)),
            )
    }

    fn assistant_body(&self, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let mut blocks: Vec<AnyElement> = Vec::new();
        for raw in text.lines() {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                blocks.push(div().h(px(5.)).into_any_element());
                continue;
            }
            if trimmed.starts_with("```") {
                continue;
            }
            if trimmed == "---" || trimmed == "***" {
                blocks.push(
                    div()
                        .h(px(1.))
                        .my_1()
                        .bg(rgb(self.palette.border))
                        .into_any_element(),
                );
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("### ") {
                blocks.push(self.rich_line(rest, true, cx));
            } else if let Some(rest) = trimmed.strip_prefix("## ") {
                blocks.push(self.rich_line(rest, true, cx));
            } else if let Some(rest) = trimmed.strip_prefix("# ") {
                blocks.push(self.rich_line(rest, true, cx));
            } else if let Some(rest) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
            {
                blocks.push(
                    div()
                        .w_full()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap_2()
                        .child(
                            div()
                                .size(px(5.))
                                .rounded_full()
                                .bg(rgb(self.palette.accent))
                                .mt(px(7.)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(self.rich_line(rest, false, cx)),
                        )
                        .into_any_element(),
                );
            } else if let Some(rest) = trimmed.strip_prefix("> ") {
                blocks.push(
                    div()
                        .w_full()
                        .border_l_2()
                        .border_color(rgb(self.palette.accent))
                        .pl_3()
                        .py_1()
                        .child(self.rich_line(rest, false, cx))
                        .into_any_element(),
                );
            } else {
                blocks.push(self.rich_line(trimmed, false, cx));
            }
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .children(blocks)
            .into_any_element()
    }

    fn rich_line(&self, text: &str, heading: bool, cx: &mut Context<Self>) -> AnyElement {
        let spans = shortlist_core::candidate_spans(&self.library_candidates, text);
        if spans.is_empty() {
            let cleaned = text.replace("**", "").replace('`', "").replace('*', "");
            let element = if heading {
                div()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(self.palette.ink))
                    .child(cleaned)
            } else {
                div()
                    .text_sm()
                    .text_color(rgb(self.palette.ink))
                    .child(cleaned)
            };
            return element.into_any_element();
        }
        let mut fragments: Vec<AnyElement> = Vec::new();
        let mut last = 0;
        let mut bold = false;
        for span in spans {
            self.push_words(&mut fragments, &text[last..span.start], heading, &mut bold);
            fragments.push(self.mention_chip(&span, cx));
            last = span.end;
        }
        self.push_words(&mut fragments, &text[last..], heading, &mut bold);
        div()
            .w_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_1()
            .children(fragments)
            .into_any_element()
    }

    fn push_words(&self, out: &mut Vec<AnyElement>, text: &str, heading: bool, bold: &mut bool) {
        let mut parts = text.split("**").peekable();
        while let Some(part) = parts.next() {
            let cleaned = part.replace('`', "").replace('*', "");
            for word in cleaned.split_whitespace() {
                let mut element = div().child(word.to_string());
                element = if heading {
                    element
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(self.palette.ink))
                } else if *bold {
                    element
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(self.palette.ink))
                } else {
                    element.text_sm().text_color(rgb(self.palette.ink))
                };
                out.push(element.into_any_element());
            }
            if parts.peek().is_some() {
                *bold = !*bold;
            }
        }
    }

    fn mention_chip(
        &self,
        span: &shortlist_core::CandidateSpan,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let candidate = self.library_candidates.iter().find(|c| c.id == span.id);
        let name = candidate
            .map(|c| c.name.clone())
            .unwrap_or_else(|| span.name.clone());
        let initial = name
            .chars()
            .next()
            .map(|ch| ch.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string());
        let id = span.id.clone();
        let hover = self.palette.hover;
        div()
            .id(SharedString::from(format!("mention-{}", span.id)))
            .flex()
            .items_center()
            .gap_1()
            .pl(px(2.))
            .pr(px(5.))
            .py(px(2.))
            .rounded_full()
            .bg(rgb(self.palette.chip))
            .border_1()
            .border_color(rgb(self.palette.border))
            .hover(move |style| style.bg(rgb(hover)))
            .child(
                div()
                    .size(px(16.))
                    .rounded_full()
                    .bg(rgb(self.palette.accent_soft))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(self.palette.accent))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .child(initial),
            )
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(self.palette.ink))
                    .child(name),
            )
            .child(
                Icon::new(IconName::ChevronRight)
                    .text_color(rgb(self.palette.muted))
                    .w(px(10.))
                    .h(px(10.)),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected_candidate = Some(id.clone());
                cx.notify();
            }))
            .into_any_element()
    }

    fn thinking_dots(&self) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_1()
            .children((0..3).map(|index| {
                let delay = index as f32 * 0.18;
                div()
                    .size(px(6.))
                    .rounded_full()
                    .bg(rgb(self.palette.accent))
                    .with_animation(
                        SharedString::from(format!("thinking-dot-{index}")),
                        Animation::new(Duration::from_millis(900)).repeat(),
                        move |el, t| {
                            let phase = (t + delay).fract();
                            let wave = (phase * std::f32::consts::PI).sin().max(0.0);
                            el.opacity(0.2 + 0.8 * wave)
                        },
                    )
            }))
    }

    fn streaming_bubble(&self, cx: &mut Context<Self>) -> AnyElement {
        let text = self.streaming.clone().unwrap_or_default();
        div()
            .w_full()
            .flex()
            .flex_row()
            .items_start()
            .gap_3()
            .child(self.assistant_avatar())
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(self.palette.muted)).child(
                        if text.trim().is_empty() {
                            "Shortlist · thinking"
                        } else {
                            "Shortlist · writing"
                        },
                    ))
                    .child(
                        div()
                            .w_full()
                            .max_w(px(700.))
                            .px_4()
                            .py_3()
                            .rounded_lg()
                            .bg(rgb(self.palette.card))
                            .border_1()
                            .border_color(rgb(self.palette.border))
                            .text_sm()
                            .child(if text.trim().is_empty() {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(self.thinking_dots())
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(self.palette.muted))
                                            .child("Thinking…"),
                                    )
                                    .into_any_element()
                            } else {
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(self.assistant_body(&text, cx))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(rgb(self.palette.accent))
                                            .child("▍")
                                            .with_animation(
                                                "stream-cursor",
                                                Animation::new(Duration::from_millis(700)).repeat(),
                                                |el, t| {
                                                    el.opacity(if t < 0.5 { 1.0 } else { 0.15 })
                                                },
                                            ),
                                    )
                                    .into_any_element()
                            }),
                    ),
            )
            .into_any_element()
    }

    fn composer_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .px_6()
            .pt_4()
            .pb_6()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(840.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .w_full()
                            .rounded_lg()
                            .bg(rgb(self.palette.card))
                            .border_1()
                            .border_color(rgb(self.palette.border))
                            .px_4()
                            .pt_3()
                            .pb_2()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .w_full()
                                    .child(Input::new(&self.composer).appearance(false)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .child(self.composer_send_button(cx)),
                            ),
                    ),
            )
    }

    fn composer_send_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("composer-send")
            .size(px(34.))
            .rounded_full()
            .bg(if self.busy {
                rgb(self.palette.chip)
            } else {
                rgb(self.palette.accent)
            })
            .flex()
            .items_center()
            .justify_center()
            .child(if self.busy {
                div()
                    .size(px(8.))
                    .rounded_full()
                    .bg(rgb(self.palette.muted))
                    .with_animation(
                        "send-busy",
                        Animation::new(Duration::from_millis(700)).repeat(),
                        |el, t| el.opacity(0.3 + 0.7 * (t * std::f32::consts::PI).sin().abs()),
                    )
                    .into_any_element()
            } else {
                Icon::new(IconName::ArrowUp)
                    .text_color(rgb(self.palette.on_accent))
                    .w(px(16.))
                    .h(px(16.))
                    .into_any_element()
            })
            .on_click(cx.listener(|this, _, window, cx| {
                let value = this.composer.read(cx).value().to_string();
                if !value.trim().is_empty() {
                    this.send(value, window, cx);
                }
            }))
    }

    fn settings_view(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_h(px(0.))
            .id("settings-scroll")
            .overflow_y_scroll()
            .px_8()
            .py_8()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(760.))
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(self.page_header("Settings", "Connection, index, and local data".into()))
                    .child(self.connection_card(cx))
                    .child(self.index_card(cx))
                    .child(self.data_card(cx))
                    .child(self.appearance_card(cx))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(self.palette.muted))
                            .child(self.notice.clone().unwrap_or_default()),
                    ),
            )
    }

    fn section_header(&self, icon: IconName, title: &str, subtitle: &str) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .size(px(34.))
                    .rounded_md()
                    .bg(rgb(self.palette.accent_soft))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(icon)
                            .text_color(rgb(self.palette.accent))
                            .w(px(16.))
                            .h(px(16.)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title.to_string()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(self.palette.muted))
                            .child(subtitle.to_string()),
                    ),
            )
    }

    fn connection_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self
            .selected_model
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "No model selected".into());
        let dot = if self.models.is_empty() {
            self.palette.danger
        } else {
            self.palette.success
        };
        let hover = self.palette.hover;
        div()
            .id("settings-open-models")
            .p_5()
            .rounded_lg()
            .bg(rgb(self.palette.card))
            .border_1()
            .border_color(rgb(self.palette.border))
            .flex()
            .flex_col()
            .gap_4()
            .child(self.section_header(IconName::Bot, "OpenCode", "Local runtime and model access"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().size(px(6.)).rounded_full().bg(rgb(dot)))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(self.opencode.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(self.palette.muted))
                                    .child(format!("Active model · {model}")),
                            ),
                    )
                    .child(
                        div().flex().items_center().gap_2().child(
                            Icon::new(IconName::ChevronRight)
                                .text_color(rgb(self.palette.muted))
                                .w(px(16.))
                                .h(px(16.)),
                        ),
                    ),
            )
            .hover(move |style| style.bg(rgb(hover)))
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_model_dialog(window, cx);
            }))
    }

    fn index_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = self.palette.hover;
        div()
            .p_5()
            .rounded_lg()
            .bg(rgb(self.palette.card))
            .border_1()
            .border_color(rgb(self.palette.border))
            .flex()
            .flex_col()
            .gap_4()
            .child(self.section_header(
                IconName::HardDrive,
                "Semantic index",
                "Local e5-small-v2 embeddings",
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(self.palette.muted))
                    .child(self.index_status.clone()),
            )
            .child(
                div()
                    .id("settings-build-index")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(self.palette.accent_soft))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(self.palette.accent))
                    .child(
                        Icon::new(IconName::RefreshCw)
                            .text_color(rgb(self.palette.accent))
                            .w(px(14.))
                            .h(px(14.)),
                    )
                    .child("Build / refresh index")
                    .hover(move |style| style.bg(rgb(hover)))
                    .on_click(cx.listener(|this, _, _, cx| this.build_index(cx))),
            )
    }

    fn data_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = self.palette.hover;
        div()
            .p_5()
            .rounded_lg()
            .bg(rgb(self.palette.card))
            .border_1()
            .border_color(rgb(self.palette.border))
            .flex()
            .flex_col()
            .gap_4()
            .child(self.section_header(
                IconName::Folder,
                "Local data",
                "Everything stays in one SQLite file",
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(self.palette.muted))
                    .child(shortlist_core::db_path().display().to_string()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("settings-demo")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(self.palette.chip))
                            .text_xs()
                            .text_color(rgb(self.palette.ink))
                            .child(
                                Icon::new(IconName::Plus)
                                    .text_color(rgb(self.palette.muted))
                                    .w(px(14.))
                                    .h(px(14.)),
                            )
                            .child("Load demo candidates")
                            .hover(move |style| style.bg(rgb(hover)))
                            .on_click(cx.listener(|this, _, _, cx| this.load_demo(cx))),
                    )
                    .child(
                        div()
                            .id("settings-clear")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(self.palette.danger))
                            .text_xs()
                            .text_color(rgb(self.palette.danger))
                            .child(
                                Icon::new(IconName::TriangleAlert)
                                    .text_color(rgb(self.palette.danger))
                                    .w(px(14.))
                                    .h(px(14.)),
                            )
                            .child(if self.confirm_clear {
                                "Confirm clear"
                            } else {
                                "Clear all data"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.confirm_clear {
                                    this.clear_all(cx);
                                } else {
                                    this.confirm_clear = true;
                                    this.notice = Some(
                                        "Click again to remove every candidate and chat. This cannot be undone."
                                            .into(),
                                    );
                                    cx.notify();
                                }
                            })),
                    ),
            )
    }

    fn appearance_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = self.palette.hover;
        div()
            .p_5()
            .rounded_lg()
            .bg(rgb(self.palette.card))
            .border_1()
            .border_color(rgb(self.palette.border))
            .flex()
            .flex_col()
            .gap_4()
            .child(self.section_header(IconName::Palette, "Appearance", "Theme and interface"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Icon::new(if self.dark {
                                    IconName::Moon
                                } else {
                                    IconName::Sun
                                })
                                .text_color(rgb(self.palette.muted))
                                .w(px(16.))
                                .h(px(16.)),
                            )
                            .child(div().text_sm().child(if self.dark {
                                "Dark mode"
                            } else {
                                "Light mode"
                            })),
                    )
                    .child(
                        div()
                            .id("settings-theme")
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(self.palette.chip))
                            .text_xs()
                            .text_color(rgb(self.palette.ink))
                            .child(if self.dark {
                                "Switch to light"
                            } else {
                                "Switch to dark"
                            })
                            .hover(move |style| style.bg(rgb(hover)))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx))),
                    ),
            )
    }

    fn open_model_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parent = cx.weak_entity();
        let models = self.models.clone();
        let selected = self.selected_model.as_ref().map(|m| m.key());
        let palette = self.palette;
        let picker = cx.new(|cx| ModelPicker::new(parent, models, selected, palette, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Choose a model")
                .w(px(680.))
                .child(picker.clone())
        });
    }

    fn open_note_dialog(
        &mut self,
        candidate_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let candidate = self
            .library_candidates
            .iter()
            .find(|candidate| candidate.id == candidate_id)
            .cloned()
            .or_else(|| self.store.candidate(&candidate_id).ok().flatten());
        let Some(candidate) = candidate else {
            return;
        };
        let body = self.store.note(&candidate_id).ok().flatten();
        let palette = self.palette;
        let store = self.store.clone();
        let parent = cx.weak_entity();
        let view =
            cx.new(|cx| NoteDialog::new(parent, store, candidate_id, body, palette, window, cx));
        let name = candidate.name.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(div().child(format!("Note · {name}")))
                .w(px(720.))
                .overlay_closable(false)
                .keyboard(false)
                .close_button(false)
                .child(view.clone())
        });
    }
}

struct ModelPicker {
    parent: WeakEntity<ShortlistApp>,
    models: Vec<ModelRef>,
    selected: Option<String>,
    palette: Palette,
    search: Entity<InputState>,
    _subs: Vec<Subscription>,
}

impl ModelPicker {
    fn new(
        parent: WeakEntity<ShortlistApp>,
        models: Vec<ModelRef>,
        selected: Option<String>,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search models by name or provider…")
        });
        let sub = cx.subscribe_in(&search, window, |_this, _state, event, _window, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            parent,
            models,
            selected,
            palette,
            search,
            _subs: vec![sub],
        }
    }
}

impl Render for ModelPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().to_lowercase();
        let filtered: Vec<ModelRef> = self
            .models
            .iter()
            .filter(|m| {
                query.is_empty()
                    || m.name.to_lowercase().contains(&query)
                    || m.key().to_lowercase().contains(&query)
            })
            .cloned()
            .collect();
        let count = filtered.len();
        let palette = self.palette;
        let selected_key = self.selected.clone();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted))
                    .child(format!("{count} connected models")),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py(px(6.))
                    .rounded_md()
                    .bg(rgb(palette.bg))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .child(
                        Icon::new(IconName::Search)
                            .text_color(rgb(palette.muted))
                            .w(px(15.))
                            .h(px(15.)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(Input::new(&self.search).appearance(false)),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.bg))
                    .p_2()
                    .child(
                        div()
                            .h(px(380.))
                            .id("dialog-model-list")
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .when(filtered.is_empty(), |el| {
                                el.child(
                                    div()
                                        .h(px(340.))
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .justify_center()
                                        .gap_2()
                                        .child(
                                            Icon::new(IconName::Search)
                                                .text_color(rgb(palette.muted))
                                                .w(px(20.))
                                                .h(px(20.)),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(rgb(palette.muted))
                                                .child("No models match"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(palette.muted))
                                                .child("Try a provider name or model id."),
                                        ),
                                )
                            })
                            .children(filtered.into_iter().enumerate().map(|(idx, m)| {
                                let selected = selected_key.as_deref() == Some(m.key().as_str());
                                let model = m.clone();
                                let parent = self.parent.clone();
                                div()
                                    .id(SharedString::from(format!("dialog-model-{idx}")))
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(if selected {
                                        rgb(palette.accent_soft)
                                    } else {
                                        rgb(palette.bg)
                                    })
                                    .border_1()
                                    .border_color(if selected {
                                        rgb(palette.accent)
                                    } else {
                                        rgb(palette.bg)
                                    })
                                    .hover(move |style| {
                                        style.bg(if selected {
                                            rgb(palette.accent_soft)
                                        } else {
                                            rgb(palette.chip)
                                        })
                                    })
                                    .child(
                                        div()
                                            .size(px(30.))
                                            .rounded_md()
                                            .bg(if selected {
                                                rgb(palette.accent)
                                            } else {
                                                rgb(palette.chip)
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                Icon::new(IconName::Cpu)
                                                    .text_color(if selected {
                                                        rgb(palette.on_accent)
                                                    } else {
                                                        rgb(palette.muted)
                                                    })
                                                    .w(px(15.))
                                                    .h(px(15.)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(if selected {
                                                        rgb(palette.accent)
                                                    } else {
                                                        rgb(palette.ink)
                                                    })
                                                    .child(m.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(palette.muted))
                                                    .child(format!("{}/{}", m.provider_id, m.id)),
                                            ),
                                    )
                                    .when(selected, |el| {
                                        el.child(
                                            Icon::new(IconName::CircleCheck)
                                                .text_color(rgb(palette.accent))
                                                .w(px(16.))
                                                .h(px(16.)),
                                        )
                                    })
                                    .on_click(cx.listener(move |_this, _, window, cx| {
                                        let _ = parent.update(cx, |app, cx| {
                                            app.select_model(model.clone(), cx)
                                        });
                                        window.close_dialog(cx);
                                    }))
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Bot)
                            .text_color(rgb(palette.muted))
                            .w(px(12.))
                            .h(px(12.)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted))
                            .child("Models come from your OpenCode installation."),
                    ),
            )
    }
}

fn merge_extracted(candidate: &mut Candidate, profile: shortlist_ai::ExtractedProfile) {
    let name = profile.name.trim();
    if !name.is_empty() && !shortlist_parse::is_bad_name(name) {
        candidate.name = name.to_string();
    }
    if !profile.headline.trim().is_empty() {
        candidate.headline = profile.headline.trim().to_string();
    }
    if !profile.location.trim().is_empty() {
        candidate.location = profile.location.trim().to_string();
    }
    if !profile.email.trim().is_empty() {
        candidate.email = profile.email.trim().to_string();
    }
    if !profile.phone.trim().is_empty() {
        candidate.phone = profile.phone.trim().to_string();
    }
    if let Some(years) = profile.years {
        if years > 0.0 && years < 60.0 {
            candidate.years = years;
        }
    }
    let skills: Vec<String> = profile
        .skills
        .into_iter()
        .map(|skill| skill.trim().to_string())
        .filter(|skill| !skill.is_empty())
        .take(24)
        .collect();
    if !skills.is_empty() {
        candidate.skills = skills;
    }
    if !profile.summary.trim().is_empty() {
        candidate.summary = profile.summary.trim().to_string();
    }
}

fn repair_candidate_names(store: &Store) {
    if let Ok(all) = store.list_candidates("", 5000) {
        for candidate in all {
            if shortlist_parse::is_bad_name(&candidate.name) {
                let fixed = shortlist_parse::derive_name(&candidate.file_name, &candidate.text);
                if !fixed.trim().is_empty() && fixed != candidate.name {
                    let _ = store.update_candidate_name(&candidate.id, &fixed);
                }
            }
        }
    }
}

fn apply_dark_surfaces(cx: &mut App) {
    Theme::update(cx, |theme| {
        theme.background = rgb(0x000000).into();
        theme.sidebar = rgb(0x000000).into();
        theme.popover = rgb(0x0B0B0B).into();
        theme.input = rgb(0x0B0B0B).into();
        theme.secondary = rgb(0x111111).into();
    });
}

fn ui_font() -> SharedString {
    if cfg!(target_os = "macos") {
        "SF Pro Text".into()
    } else if cfg!(target_os = "windows") {
        "Segoe UI".into()
    } else {
        "Inter".into()
    }
}

// Shared candidate-table metrics. Header and rows read the same values, so
// columns stay aligned at any window width. `Skills` is the only fluid column.
const TABLE_AVATAR: f32 = 40.;
const TABLE_CANDIDATE: f32 = 220.;
const TABLE_LOCATION: f32 = 130.;
const TABLE_EXP: f32 = 80.;
const TABLE_ADDED: f32 = 84.;
const TABLE_ACTIONS: f32 = 100.;
const TABLE_COMPACT_SKILLS: f32 = 120.;
const TABLE_GAP: f32 = 12.;

/// Lucide `trash-2`, embedded directly so it does not depend on the
/// bundled icon subset used by the component library.
const TRASH_ICON: &[u8] = include_bytes!("../assets/icons/trash.svg");

struct NoteDialog {
    parent: WeakEntity<ShortlistApp>,
    store: Arc<Store>,
    candidate_id: String,
    palette: Palette,
    editor: Entity<EditorState>,
    had_note: bool,
    _subs: Vec<Subscription>,
}

impl NoteDialog {
    fn new(
        parent: WeakEntity<ShortlistApp>,
        store: Arc<Store>,
        candidate_id: String,
        body: Option<String>,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = body.map(note_text_from_stored).unwrap_or_default();
        let had_note = !text.trim().is_empty();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .searchable(false)
                .soft_wrap(true)
                .default_value(text)
        });
        let sub = cx.subscribe_in(
            &editor,
            window,
            |_this, _editor, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        );

        Self {
            parent,
            store,
            candidate_id,
            palette,
            editor,
            had_note,
            _subs: vec![sub],
        }
    }

    fn markdown(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    fn apply_edit(&mut self, edit: TextEdit, window: &mut Window, cx: &mut Context<Self>) {
        let TextEdit { text, selection } = edit;
        self.editor.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_selected_range(selection, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn set_block(&mut self, kind: BlockKind, window: &mut Window, cx: &mut Context<Self>) {
        let (text, selection) = {
            let state = self.editor.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        let edit = markdown_tools::set_block(&text, selection, kind);
        self.apply_edit(edit, window, cx);
    }

    fn toggle_inline(&mut self, marker: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (text, selection) = {
            let state = self.editor.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        let edit = markdown_tools::toggle_inline(&text, selection, marker);
        self.apply_edit(edit, window, cx);
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let markdown = self.markdown(cx);
        let empty = markdown.trim().is_empty();
        let result = if empty {
            self.store.delete_note(&self.candidate_id)
        } else {
            self.store.save_note(&self.candidate_id, &markdown)
        };
        self.report(result.is_ok(), empty, cx);
        window.close_dialog(cx);
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.store.delete_note(&self.candidate_id);
        self.report(result.is_ok(), true, cx);
        window.close_dialog(cx);
    }

    fn report(&self, ok: bool, removed: bool, cx: &mut Context<Self>) {
        if let Some(parent) = self.parent.upgrade() {
            let message = if ok {
                if removed {
                    "Note removed."
                } else {
                    "Note saved."
                }
            } else {
                "Could not save the note."
            };
            parent.update(cx, |app, cx| {
                app.notice = Some(message.into());
                cx.notify();
            });
        }
    }
}

impl Render for NoteDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette;
        let markdown = self.markdown(cx);
        let selection = self.editor.read(cx).selected_range();
        let block = markdown_tools::block_at(&markdown, selection.start);
        let bold = markdown_tools::is_wrapped(&markdown, selection.clone(), "**");
        let italic = markdown_tools::is_wrapped(&markdown, selection.clone(), "*");
        let strike = markdown_tools::is_wrapped(&markdown, selection.clone(), "~~");
        let code = markdown_tools::is_wrapped(&markdown, selection.clone(), "`");
        let words = markdown.split_whitespace().count();

        let mut heading_buttons = Vec::with_capacity(6);
        const HEADING_LABELS: [&str; 6] = ["H1", "H2", "H3", "H4", "H5", "H6"];
        for level in 1..=6u8 {
            heading_buttons.push(note_button(
                SharedString::from(format!("note-h{level}")),
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(HEADING_LABELS[(level - 1) as usize])
                    .into_any_element(),
                matches!(block, BlockKind::Heading(current) if current == level),
                palette,
                cx,
                move |this, window, cx| this.set_block(BlockKind::Heading(level), window, cx),
            ));
        }

        let paragraph = note_button(
            SharedString::from("note-paragraph"),
            div().child("¶").into_any_element(),
            matches!(block, BlockKind::Paragraph),
            palette,
            cx,
            |this, window, cx| this.set_block(BlockKind::Paragraph, window, cx),
        );
        let bullet = note_button(
            SharedString::from("note-bullet"),
            div().child("•").into_any_element(),
            matches!(block, BlockKind::Bullet),
            palette,
            cx,
            |this, window, cx| this.set_block(BlockKind::Bullet, window, cx),
        );
        let numbered = note_button(
            SharedString::from("note-numbered"),
            div().child("1.").into_any_element(),
            matches!(block, BlockKind::Numbered),
            palette,
            cx,
            |this, window, cx| this.set_block(BlockKind::Numbered, window, cx),
        );
        let quote = note_button(
            SharedString::from("note-quote"),
            div().child("❝").into_any_element(),
            matches!(block, BlockKind::Quote),
            palette,
            cx,
            |this, window, cx| this.set_block(BlockKind::Quote, window, cx),
        );

        let bold_button = note_button(
            SharedString::from("note-bold"),
            div()
                .font_weight(FontWeight::BOLD)
                .child("B")
                .into_any_element(),
            bold,
            palette,
            cx,
            |this, window, cx| this.toggle_inline("**", window, cx),
        );
        let italic_button = note_button(
            SharedString::from("note-italic"),
            div().italic().child("I").into_any_element(),
            italic,
            palette,
            cx,
            |this, window, cx| this.toggle_inline("*", window, cx),
        );
        let strike_button = note_button(
            SharedString::from("note-strike"),
            div().line_through().child("S").into_any_element(),
            strike,
            palette,
            cx,
            |this, window, cx| this.toggle_inline("~~", window, cx),
        );
        let code_button = note_button(
            SharedString::from("note-code"),
            div().font_family("Menlo").child("Code").into_any_element(),
            code,
            palette,
            cx,
            |this, window, cx| this.toggle_inline("`", window, cx),
        );

        let divider = || {
            div()
                .w(px(1.))
                .h(px(18.))
                .bg(rgb(palette.border))
                .into_any_element()
        };

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .children(heading_buttons)
                    .child(paragraph)
                    .child(divider())
                    .child(bold_button)
                    .child(italic_button)
                    .child(strike_button)
                    .child(code_button)
                    .child(divider())
                    .child(bullet)
                    .child(numbered)
                    .child(quote),
            )
            .child(
                div()
                    .w_full()
                    .h(px(470.))
                    .flex()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.card))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .flex()
                            .flex_col()
                            .bg(rgb(palette.bg))
                            .child(self.pane_header("MARKDOWN", "Write", palette))
                            .child(
                                div().flex_1().min_h(px(0.)).px_3().py_2().child(
                                    Editor::new(&self.editor)
                                        .appearance(false)
                                        .bordered(false)
                                        .font_family(ui_font())
                                        .text_size(px(13.))
                                        .w_full()
                                        .h_full(),
                                ),
                            ),
                    )
                    .child(div().w(px(1.)).h_full().bg(rgb(palette.border)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .flex()
                            .flex_col()
                            .bg(rgb(palette.card))
                            .child(self.pane_header("PREVIEW", "Rendered", palette))
                            .child(
                                div()
                                    .flex_1()
                                    .min_h(px(0.))
                                    .p_4()
                                    .when(markdown.trim().is_empty(), |el| {
                                        el.flex().items_center().justify_center().child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .items_center()
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(rgb(palette.muted))
                                                        .child("Your preview appears here"),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(rgb(palette.muted))
                                                        .child(
                                                            "Markdown: # heading, **bold**, - list",
                                                        ),
                                                ),
                                        )
                                    })
                                    .when(!markdown.trim().is_empty(), |el| {
                                        el.child(
                                            TextView::markdown("note-preview", markdown.clone())
                                                .selectable(true)
                                                .scrollable(true),
                                        )
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted))
                            .child(format!(
                                "{words} words · Markdown · stored only on this device"
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(self.had_note || !markdown.trim().is_empty(), |el| {
                                el.child(
                                    div()
                                        .id("note-delete")
                                        .px_3()
                                        .py_2()
                                        .rounded_md()
                                        .text_xs()
                                        .text_color(rgb(palette.danger))
                                        .child("Delete note")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.delete(window, cx);
                                        })),
                                )
                            })
                            .child(
                                div()
                                    .id("note-cancel")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(rgb(palette.border))
                                    .text_xs()
                                    .text_color(rgb(palette.muted))
                                    .child("Cancel")
                                    .on_click(cx.listener(|_, _, window, cx| {
                                        window.close_dialog(cx);
                                    })),
                            )
                            .child(
                                div()
                                    .id("note-save")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(palette.accent))
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(palette.on_accent))
                                    .child("Save note")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save(window, cx);
                                    })),
                            ),
                    ),
            )
    }
}

impl NoteDialog {
    fn pane_header(&self, title: &'static str, hint: &'static str, palette: Palette) -> AnyElement {
        div()
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted))
                    .child(title),
            )
            .child(div().text_xs().text_color(rgb(palette.muted)).child(hint))
            .into_any_element()
    }
}

fn note_button(
    id: SharedString,
    label: AnyElement,
    active: bool,
    palette: Palette,
    cx: &mut Context<NoteDialog>,
    on_click: impl Fn(&mut NoteDialog, &mut Window, &mut Context<NoteDialog>) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(26.))
        .min_w(px(30.))
        .px_2()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .bg(if active {
            rgb(palette.accent_soft)
        } else {
            rgb(palette.card)
        })
        .border_1()
        .border_color(if active {
            rgb(palette.accent)
        } else {
            rgb(palette.border)
        })
        .text_xs()
        .text_color(if active {
            rgb(palette.accent)
        } else {
            rgb(palette.ink)
        })
        .child(label)
        .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
        .into_any_element()
}

/// Notes used to be stored as JSON runs; recover their text when reopened.
fn note_text_from_stored(body: String) -> String {
    if body.trim_start().starts_with("{\"v\"") {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(text) = value.get("text").and_then(|value| value.as_str()) {
                return text.to_string();
            }
        }
    }
    body
}

fn main() {
    application().with_assets(assets::Assets).run(|cx| {
        init(cx);
        let bounds = Bounds::centered(None, size(px(1240.), px(820.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Shortlist".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        open_window(options, cx, |window, cx| {
            cx.new(|cx| ShortlistApp::new(window, cx))
        })
        .expect("open window");
        cx.activate(true);
    });
}
