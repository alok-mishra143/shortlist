#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::Command;
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
use markdown_table::{Alignment, Table};
use markdown_tools::{BlockKind, TextEdit};
use shortlist_ai::OpenCode;
use shortlist_core::{Candidate, ChatMessage, ChatSession, ModelRef, human_relative, now_ms};
use shortlist_embed::Embedder;
use shortlist_parse::parse_resume;
use shortlist_store::{DbTable, Store};
use shortlist_update::{CheckResult, ReleaseInfo};

mod markdown_table;
mod markdown_tools;

// GPUI does not infer a pointer cursor from `on_click`, so every clickable
// surface below sets `.cursor_pointer()` explicitly; disabled actions use
// `.cursor_not_allowed()` so the cursor matches what the click will do.

struct ImportProgress {
    total: usize,
    done: usize,
    current: String,
    step: String,
    cancel: Arc<AtomicBool>,
}

#[derive(Default)]
struct ToolView {
    lines: Vec<String>,
    done: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Route {
    Candidates,
    Chat,
    Developer,
    Settings,
}

enum UpdatePhase {
    Idle,
    Checking,
    Current {
        latest: String,
    },
    Available(ReleaseInfo),
    Downloading {
        release: ReleaseInfo,
        received: u64,
        total: Option<u64>,
    },
    Installing {
        version: String,
    },
    Failed {
        message: String,
        release: Option<ReleaseInfo>,
    },
}

impl UpdatePhase {
    fn busy(&self) -> bool {
        matches!(
            self,
            UpdatePhase::Checking
                | UpdatePhase::Downloading { .. }
                | UpdatePhase::Installing { .. }
        )
    }

    fn release(&self) -> Option<&ReleaseInfo> {
        match self {
            UpdatePhase::Available(release)
            | UpdatePhase::Downloading { release, .. }
            | UpdatePhase::Failed {
                release: Some(release),
                ..
            } => Some(release),
            _ => None,
        }
    }
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

/// A table cell is either a muted header label or body content, and it
/// inherits the column alignment declared by the Markdown separator row.
#[derive(Clone, Copy)]
enum TableCellKind {
    Header(Alignment),
    Body(Alignment),
}

/// Status phrases for the indeterminate model call. A long reasoning turn
/// cycles them so the bubble reads as active work, not a frozen card.
const WORKING_PHRASES: [&str; 5] = [
    "Cooking your answer",
    "Finding your answer",
    "Reading the evidence",
    "Checking the shortlist",
    "Writing it up",
];

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
    mcp_status: Option<String>,
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
    dev_table: String,
    dev_tables: Vec<(String, i64)>,
    dev_data: Option<DbTable>,
    update_phase: UpdatePhase,
    update_packaged: bool,
    update_auto: bool,
    update_checked_at: Option<i64>,
    update_banner_hidden: bool,
    update_cancel: Arc<AtomicBool>,
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
            Some("developer") => Route::Developer,
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
        let dev_tables = store.table_counts().unwrap_or_default();
        let dev_data = store.inspect_table("candidates", 60).ok();
        let update_packaged = shortlist_update::is_packaged_install();
        let update_auto = store
            .get_setting("auto_update_check")
            .ok()
            .flatten()
            .as_deref()
            != Some("0");
        let update_checked_at = store
            .get_setting("update_checked_at")
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok());

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
            mcp_status: None,
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
            dev_table: "candidates".into(),
            dev_tables,
            dev_data,
            update_phase: UpdatePhase::Idle,
            update_packaged,
            update_auto,
            update_checked_at,
            update_banner_hidden: false,
            update_cancel: Arc::new(AtomicBool::new(false)),
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

        if update_auto {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(2200))
                    .await;
                this.update(cx, |this, cx| this.check_for_updates(false, cx))
                    .ok();
            })
            .detach();
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

    fn register_mcp(&mut self, cx: &mut Context<Self>) {
        if mcp_binary().is_none() {
            self.mcp_status = Some(
                "shortlist-mcp is not built. Run `cargo build -p shortlist-mcp` first.".into(),
            );
            cx.notify();
            return;
        }
        self.mcp_status = Some("Registering Shortlist tools with OpenCode…".into());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ensure_mcp_registered() })
                .await;
            this.update(cx, |this, cx| {
                this.mcp_status = Some(match result {
                    Ok(true) => "Shortlist MCP tools registered with OpenCode.".into(),
                    Ok(false) => "Shortlist MCP tools are already registered.".into(),
                    Err(error) => format!("MCP tools not registered: {error}"),
                });
                cx.notify();
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

                let chunks = shortlist_core::chunk_text(&candidate.index_text(), 180, 28);
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
                // The drawer, note lookup, and chat chips read from the full
                // library, so a newly imported candidate must land there too.
                this.reload_library(cx);
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
        // The harness owns model sessions and creates one per call, so a chat is
        // a local row only. That keeps New chat instant and lets local lookups
        // work with no OpenCode service at all.
        match self.store.create_session("New chat") {
            Ok(session) => {
                self.active_session = Some(session.id.clone());
                self.messages.clear();
                self.notice = None;
                self.reload_sessions(cx);
            }
            Err(e) => self.notice = Some(e.to_string()),
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
        // History before this turn; the harness compacts it to a few short lines.
        let history: Vec<(String, String)> = self
            .store
            .messages(&local_id)
            .unwrap_or_default()
            .into_iter()
            .map(|message| (message.role, message.text))
            .collect();
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
        self.notice = Some("Cooking your answer…".into());
        let store = self.store.clone();
        let embedder = self.embedder.lock().unwrap().clone();
        let local = local_id.clone();
        let model = self.selected_model.clone();
        let query = text.clone();
        let progress = Arc::new(Mutex::new(ToolView::default()));
        let task_progress = progress.clone();

        let task = cx.background_executor().spawn(async move {
            let callback_progress = task_progress.clone();
            let progress_line = move |line: &str| {
                if let Ok(mut view) = callback_progress.lock() {
                    view.lines.push(format!("↳ {line}"));
                    if view.lines.len() > 8 {
                        view.lines.remove(0);
                    }
                }
            };
            let result = shortlist_pipeline::ask_with_tools_report(
                &store,
                embedder.as_deref(),
                model.as_ref(),
                &query,
                &history,
                Some(&progress_line),
            );
            if let Ok(mut view) = task_progress.lock() {
                view.done = true;
            }
            result.map(|report| (report.answer, report.direct))
        });

        cx.spawn(async move |this, cx| {
            let started = now_ms();
            let mut last = String::new();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                let (text, done) = {
                    let view = progress.lock().unwrap();
                    (view.lines.join("\n"), view.done)
                };
                if text != last {
                    last = text.clone();
                    this.update(cx, |this, cx| {
                        this.streaming = Some(text);
                        cx.notify();
                    })
                    .ok();
                }
                if done {
                    break;
                }
                if now_ms() - started > 240_000 {
                    this.update(cx, |this, cx| {
                        this.notice = Some("Timed out waiting for the model.".into());
                        cx.notify();
                    })
                    .ok();
                    break;
                }
            }

            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((answer, direct)) => {
                        let _ = this.store.add_message(&local, "assistant", &answer);
                        this.notice = if direct {
                            Some("Answered locally · no model call".into())
                        } else {
                            None
                        };
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

    fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Self>) {
        if self.update_phase.busy() {
            return;
        }
        self.update_phase = UpdatePhase::Checking;
        self.update_banner_hidden = false;
        if manual {
            self.notice = None;
        }
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { shortlist_update::check_for_update() })
                .await;
            this.update(cx, |this, cx| {
                let now = now_ms();
                this.update_checked_at = Some(now);
                let _ = this
                    .store
                    .set_setting("update_checked_at", &now.to_string());
                this.update_phase = match result {
                    Ok(CheckResult::UpToDate { latest, .. }) => UpdatePhase::Current {
                        latest: latest.to_string(),
                    },
                    Ok(CheckResult::Available(release)) => UpdatePhase::Available(release),
                    Err(error) => {
                        if manual {
                            UpdatePhase::Failed {
                                message: format!("{error:#}"),
                                release: None,
                            }
                        } else {
                            // A quiet launch check should never nag when the
                            // machine is offline. Settings still offers a
                            // manual retry with the full error.
                            UpdatePhase::Idle
                        }
                    }
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn auto_install_ready(&self, release: &ReleaseInfo) -> bool {
        self.update_packaged && release.asset.is_some() && release.checksums_url.is_some()
    }

    fn start_update(&mut self, cx: &mut Context<Self>) {
        if self.update_phase.busy() {
            return;
        }
        let Some(release) = self.update_phase.release().cloned() else {
            return;
        };
        if release.asset.is_none() {
            self.notice =
                Some("This release has no build for this Mac. Download it from GitHub.".into());
            cx.notify();
            return;
        }
        if release.checksums_url.is_none() {
            self.notice =
                Some("This release has no checksum file, so it must be installed manually.".into());
            cx.notify();
            cx.open_url(&release.page_url);
            return;
        }
        if !self.update_packaged {
            cx.open_url(&release.page_url);
            return;
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let background_cancel = cancel.clone();
        self.update_cancel = cancel.clone();
        let total_hint = release.asset.as_ref().map(|asset| asset.size);
        self.update_phase = UpdatePhase::Downloading {
            release: release.clone(),
            received: 0,
            total: total_hint,
        };
        self.update_banner_hidden = false;
        self.notice = None;
        cx.notify();

        let progress = Arc::new(Mutex::new((0u64, total_hint, false)));
        let background_progress = progress.clone();
        let background_release = release.clone();
        let work_root = shortlist_core::data_dir().join("updates");
        let task = cx.background_executor().spawn(async move {
            let mut on_progress = |received: u64, total: Option<u64>| {
                if let Ok(mut value) = background_progress.lock() {
                    value.0 = received;
                    if total.is_some() {
                        value.1 = total;
                    }
                }
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                shortlist_update::download_and_stage(
                    &background_release,
                    &work_root,
                    &background_cancel,
                    &mut on_progress,
                )
            }))
            .map_err(|_| "The updater stopped unexpectedly. Try again.".to_string())
            .and_then(|result| result.map_err(|error| format!("{error:#}")));
            if let Ok(mut value) = background_progress.lock() {
                value.2 = true;
            }
            result
        });

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                let (received, total, done) = *progress.lock().unwrap();
                this.update(cx, |this, cx| {
                    if let UpdatePhase::Downloading {
                        received: current,
                        total: current_total,
                        ..
                    } = &mut this.update_phase
                        && (*current != received || *current_total != total)
                    {
                        *current = received;
                        *current_total = total;
                        cx.notify();
                    }
                })
                .ok();
                if done {
                    break;
                }
            }

            let result = task.await;
            if cancel.load(Ordering::Relaxed) {
                if let Ok(staged) = &result {
                    let _ = std::fs::remove_dir_all(&staged.work_dir);
                }
                this.update(cx, |this, cx| {
                    this.update_phase = UpdatePhase::Available(release);
                    this.notice = Some("Update cancelled.".into());
                    cx.notify();
                })
                .ok();
                return;
            }

            let staged = match result {
                Ok(staged) => staged,
                Err(message) => {
                    this.update(cx, |this, cx| {
                        this.update_phase = UpdatePhase::Failed {
                            message,
                            release: Some(release),
                        };
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            let version = staged.version.to_string();
            this.update(cx, |this, cx| {
                this.update_phase = UpdatePhase::Installing {
                    version: version.clone(),
                };
                cx.notify();
            })
            .ok();
            cx.background_executor()
                .timer(Duration::from_millis(350))
                .await;

            let data_dir = shortlist_core::data_dir();
            let install = cx
                .background_executor()
                .spawn(async move { shortlist_update::install_and_restart(&staged, &data_dir) })
                .await;
            match install {
                Ok(()) => {
                    this.update(cx, |_this, cx| cx.quit()).ok();
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    this.update(cx, |this, cx| {
                        this.update_phase = UpdatePhase::Failed {
                            message,
                            release: Some(release),
                        };
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn toggle_auto_update(&mut self, cx: &mut Context<Self>) {
        self.update_auto = !self.update_auto;
        let _ = self.store.set_setting(
            "auto_update_check",
            if self.update_auto { "1" } else { "0" },
        );
        cx.notify();
    }
}

fn platform_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
}

fn arch_name() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "Apple Silicon",
        ("macos", "x86_64") => "Intel",
        (_, "aarch64") => "ARM64",
        (_, "x86_64") => "x64",
        (_, other) => other,
    }
}

fn human_bytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

fn progress_summary(received: u64, total: Option<u64>) -> String {
    match total.filter(|total| *total > 0) {
        Some(total) => format!(
            "{}% · {} of {}",
            ((received as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as u32,
            human_bytes(received),
            human_bytes(total)
        ),
        None => human_bytes(received),
    }
}

fn progress_fraction(received: u64, total: Option<u64>) -> f32 {
    total
        .filter(|total| *total > 0)
        .map(|total| (received as f32 / total as f32).clamp(0.0, 1.0))
        .unwrap_or(0.0)
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar_width = if self.sidebar_open {
            SIDEBAR_WIDTH_OPEN
        } else {
            SIDEBAR_WIDTH_CLOSED
        };
        let room = (f32::from(window.viewport_size().width) - sidebar_width).max(0.);
        let detail_open = self.selected_candidate.is_some();
        // Keep a usable list beside the drawer. When the window is narrower
        // than that, the drawer takes the content area on its own instead of
        // squeezing the list or spilling past the window edge.
        let side_by_side = room >= DETAIL_MIN_WIDTH + LIST_MIN_WIDTH;
        let detail_width = if side_by_side {
            DETAIL_WIDTH.min(room - LIST_MIN_WIDTH)
        } else {
            room
        };
        let detail = |this: &Self, cx: &mut Context<Self>| this.candidate_detail(detail_width, cx);

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
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(self.update_banner(cx))
                    .child(if detail_open && !side_by_side {
                        div()
                            .flex_1()
                            .min_h(px(0.))
                            .flex()
                            .child(detail(self, cx))
                            .into_any_element()
                    } else {
                        match self.route {
                            Route::Candidates => self.candidates_view(cx).into_any_element(),
                            Route::Chat => self.chat_view(cx).into_any_element(),
                            Route::Developer => self.developer_view(cx).into_any_element(),
                            Route::Settings => self.settings_view(cx).into_any_element(),
                        }
                    }),
            )
            .when(detail_open && side_by_side, |el| el.child(detail(self, cx)))
    }
}

impl ShortlistApp {
    fn update_banner(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.update_banner_hidden {
            return div().into_any_element();
        }

        let (title, detail) = match &self.update_phase {
            UpdatePhase::Available(release) => (
                format!("Shortlist {} is available", release.version),
                if self.auto_install_ready(release) {
                    format!(
                        "You're on {}. It downloads, installs, and reopens automatically.",
                        shortlist_update::current_version()
                    )
                } else {
                    "Download it from GitHub, or use a packaged build to install in place."
                        .to_string()
                },
            ),
            UpdatePhase::Downloading {
                release,
                received,
                total,
            } => (
                format!("Downloading Shortlist {}…", release.version),
                format!(
                    "{} · Shortlist restarts when the update is ready.",
                    progress_summary(*received, *total)
                ),
            ),
            UpdatePhase::Installing { version } => (
                format!("Installing Shortlist {version}…"),
                "Shortlist is about to restart.".to_string(),
            ),
            UpdatePhase::Failed {
                message,
                release: Some(release),
            } => (
                format!("Couldn't update to {}", release.version),
                message
                    .lines()
                    .next()
                    .unwrap_or("Unknown update error")
                    .to_string(),
            ),
            _ => return div().into_any_element(),
        };

        let actions: AnyElement = match &self.update_phase {
            UpdatePhase::Available(release) if release.asset.is_some() => div()
                .flex()
                .items_center()
                .gap_2()
                .child(self.action_primary(
                    "banner-update",
                    if self.auto_install_ready(release) {
                        "Update & restart"
                    } else {
                        "Download update"
                    },
                    cx,
                ))
                .child(self.action_ghost("banner-later", "Later", cx, |this, cx| {
                    this.update_banner_hidden = true;
                    cx.notify();
                }))
                .into_any_element(),
            UpdatePhase::Available(release) => div()
                .flex()
                .items_center()
                .gap_2()
                .child(self.action_open("banner-release", "View release", &release.page_url, cx))
                .child(self.action_ghost("banner-later", "Later", cx, |this, cx| {
                    this.update_banner_hidden = true;
                    cx.notify();
                }))
                .into_any_element(),
            UpdatePhase::Downloading { .. } => self
                .action_ghost("banner-cancel", "Cancel", cx, |this, cx| {
                    this.update_cancel.store(true, Ordering::Relaxed);
                    cx.notify();
                })
                .into_any_element(),
            UpdatePhase::Failed {
                release: Some(release),
                ..
            } => div()
                .flex()
                .items_center()
                .gap_2()
                .child(self.action_primary("banner-retry", "Try again", cx))
                .child(self.action_open("banner-notes", "Release notes", &release.page_url, cx))
                .child(
                    self.action_ghost("banner-dismiss", "Dismiss", cx, |this, cx| {
                        this.update_banner_hidden = true;
                        cx.notify();
                    }),
                )
                .into_any_element(),
            _ => div().into_any_element(),
        };

        let progress = match &self.update_phase {
            UpdatePhase::Downloading {
                received, total, ..
            } => Some(progress_fraction(*received, *total)),
            _ => None,
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_8()
            .py_3()
            .bg(rgb(self.palette.accent_soft))
            .border_b_1()
            .border_color(rgb(self.palette.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                Icon::new(IconName::RefreshCw)
                                    .text_color(rgb(self.palette.accent))
                                    .w(px(16.))
                                    .h(px(16.)),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_ellipsis()
                                            .child(title),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(self.palette.muted))
                                            .text_ellipsis()
                                            .child(detail),
                                    ),
                            ),
                    )
                    .child(actions),
            )
            .when_some(progress, |el, fraction| {
                el.child(self.update_progress_bar(fraction))
            })
            .into_any_element()
    }

    fn update_progress_bar(&self, fraction: f32) -> impl IntoElement {
        div()
            .h(px(3.))
            .w_full()
            .rounded_full()
            .bg(rgb(self.palette.chip))
            .child(
                div()
                    .h_full()
                    .rounded_full()
                    .bg(rgb(self.palette.accent))
                    .w(relative(fraction.clamp(0.02, 1.0))),
            )
    }

    fn action_primary(&self, id: &'static str, label: &str, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(id)
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(self.palette.accent))
            .text_color(rgb(self.palette.on_accent))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .child(label.to_string())
            .hover(|style| style.opacity(0.9))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| this.start_update(cx)))
            .into_any_element()
    }

    fn action_ghost(
        &self,
        id: &'static str,
        label: &str,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let hover = self.palette.hover;
        div()
            .id(id)
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(self.palette.border))
            .bg(rgb(self.palette.card))
            .text_xs()
            .text_color(rgb(self.palette.ink))
            .child(label.to_string())
            .hover(move |style| style.bg(rgb(hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
            .into_any_element()
    }

    fn action_open(
        &self,
        id: &'static str,
        label: &str,
        url: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let url = url.to_string();
        self.action_ghost(id, label, cx, move |_this, cx| cx.open_url(&url))
    }

    fn update_check_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let hover = self.palette.hover;
        let busy = self.update_phase.busy();
        div()
            .id("settings-check-updates")
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(self.palette.chip))
            .text_xs()
            .text_color(rgb(self.palette.ink))
            .when(busy, |el| el.opacity(0.55))
            .child(
                Icon::new(IconName::RefreshCw)
                    .text_color(rgb(self.palette.muted))
                    .w(px(14.))
                    .h(px(14.)),
            )
            .child(
                if matches!(self.update_phase, UpdatePhase::Current { .. }) {
                    "Check again"
                } else {
                    "Check for updates"
                },
            )
            .hover(move |style| style.bg(rgb(hover)))
            .when(busy, |el| el.cursor_not_allowed())
            .when(!busy, |el| el.cursor_pointer())
            .on_click(cx.listener(|this, _, _, cx| this.check_for_updates(true, cx)))
            .into_any_element()
    }

    fn update_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = shortlist_update::current_version().to_string();
        let hover = self.palette.hover;
        let (status, tone) = match &self.update_phase {
            UpdatePhase::Idle => (
                "Check GitHub for a newer release. Updates install in place and restart Shortlist."
                    .to_string(),
                self.palette.muted,
            ),
            UpdatePhase::Checking => (
                "Checking GitHub for a newer release…".to_string(),
                self.palette.accent,
            ),
            UpdatePhase::Current { latest } => (
                format!("You're up to date. The latest release is {latest}."),
                self.palette.success,
            ),
            UpdatePhase::Available(release) => (
                format!(
                    "Version {} is available. You're on {current}.",
                    release.version
                ),
                self.palette.accent,
            ),
            UpdatePhase::Downloading {
                release,
                received,
                total,
            } => (
                format!(
                    "Downloading {} · {}",
                    release.version,
                    progress_summary(*received, *total)
                ),
                self.palette.accent,
            ),
            UpdatePhase::Installing { version } => (
                format!("Installing {version} and restarting…"),
                self.palette.accent,
            ),
            UpdatePhase::Failed { message, .. } => (message.clone(), self.palette.danger),
        };

        let release = self.update_phase.release().cloned();
        let notes: Vec<String> = release
            .as_ref()
            .map(|release| {
                release
                    .notes
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .take(3)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let progress = match &self.update_phase {
            UpdatePhase::Downloading {
                received, total, ..
            } => Some((*received, *total)),
            _ => None,
        };

        let actions: AnyElement = match (&self.update_phase, release.as_ref()) {
            (UpdatePhase::Available(release), Some(_)) if release.asset.is_some() => div()
                .flex()
                .items_center()
                .gap_2()
                .child(if self.auto_install_ready(release) {
                    self.action_primary("settings-update", "Update & restart", cx)
                } else {
                    self.action_open(
                        "settings-download",
                        "Download from GitHub",
                        &release.page_url,
                        cx,
                    )
                })
                .child(self.update_check_button(cx))
                .into_any_element(),
            (UpdatePhase::Available(release), _) => div()
                .flex()
                .items_center()
                .gap_2()
                .child(self.action_open("settings-release", "View release", &release.page_url, cx))
                .child(self.update_check_button(cx))
                .into_any_element(),
            (UpdatePhase::Downloading { .. }, _) => self
                .action_ghost("settings-cancel-update", "Cancel update", cx, |this, cx| {
                    this.update_cancel.store(true, Ordering::Relaxed);
                    cx.notify();
                })
                .into_any_element(),
            (
                UpdatePhase::Failed {
                    release: Some(release),
                    ..
                },
                _,
            ) => div()
                .flex()
                .items_center()
                .gap_2()
                .child(self.action_primary("settings-retry", "Try again", cx))
                .child(self.action_open(
                    "settings-failed-notes",
                    "Release notes",
                    &release.page_url,
                    cx,
                ))
                .into_any_element(),
            (UpdatePhase::Installing { .. }, _) => div().into_any_element(),
            _ => self.update_check_button(cx).into_any_element(),
        };

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
                IconName::RefreshCw,
                "Updates",
                "New versions install from GitHub Releases",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(format!("Shortlist v{current}")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(self.palette.muted))
                                    .child(format!("{} · {}", platform_name(), arch_name())),
                            ),
                    )
                    .child(div().text_xs().text_color(rgb(tone)).child(status)),
            )
            .when_some(self.update_checked_at, |el, checked| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(rgb(self.palette.muted))
                        .child(format!("Last checked {}.", human_relative(checked))),
                )
            })
            .when_some(progress, |el, (received, total)| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(self.update_progress_bar(progress_fraction(received, total)))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(self.palette.muted))
                                .child(progress_summary(received, total)),
                        ),
                )
            })
            .when(!notes.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .child("What's new".to_string()),
                        )
                        .children(notes.into_iter().map(|line| {
                            div()
                                .text_xs()
                                .text_color(rgb(self.palette.muted))
                                .child(format!("· {line}"))
                        })),
                )
            })
            .child(actions)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .pt_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(self.palette.muted))
                            .child("Check automatically when Shortlist opens".to_string()),
                    )
                    .child(
                        div()
                            .id("settings-auto-update")
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(self.palette.chip))
                            .text_xs()
                            .text_color(rgb(self.palette.ink))
                            .child(if self.update_auto { "On" } else { "Off" })
                            .hover(move |style| style.bg(rgb(hover)))
                            .cursor_pointer()
                            .on_click(
                                cx.listener(|this, _, _, cx| this.toggle_auto_update(cx)),
                            ),
                    ),
            )
            .when(!self.update_packaged, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(rgb(self.palette.muted))
                        .child(
                            "This is a development build, so automatic install is unavailable. Packaged releases update in place."
                                .to_string(),
                        ),
                )
            })
    }

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
            .flex_none()
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
            .child(self.nav_item("Developer", Route::Developer, None, IconName::HardDrive, cx))
            .child(self.nav_item("Settings", Route::Settings, None, IconName::Settings, cx))
            .child(div().flex_1())
            .when(!collapsed, |el| {
                el.child(
                    div()
                        .id("sidebar-version")
                        .px_2()
                        .pb_1()
                        .text_xs()
                        .text_color(rgb(self.palette.muted))
                        .child(format!("v{}", shortlist_update::current_version()))
                        .hover(|style| style.opacity(0.7))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.route = Route::Settings;
                            cx.notify();
                        })),
                )
            })
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
                        SIDEBAR_WIDTH_CLOSED + (SIDEBAR_WIDTH_OPEN - SIDEBAR_WIDTH_CLOSED) * t
                    } else {
                        SIDEBAR_WIDTH_OPEN - (SIDEBAR_WIDTH_OPEN - SIDEBAR_WIDTH_CLOSED) * t
                    };
                    el.w(px(width))
                },
            )
    }

    fn brand_row(&self, collapsed: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = self.palette.hover;
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
                    .hover(move |style| style.bg(rgb(hover)))
                    .cursor_pointer()
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
        let hover = self.palette.hover;
        let accent_soft = self.palette.accent_soft;
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
                rgb(accent_soft)
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
            .hover(move |style| style.bg(if active { rgb(accent_soft) } else { rgb(hover) }))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.route = route;
                if route == Route::Developer {
                    this.load_dev(cx);
                } else {
                    cx.notify();
                }
            }))
    }

    fn theme_toggle(&self, collapsed: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = self.palette.hover;
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
            .hover(move |style| style.bg(rgb(hover)))
            .cursor_pointer()
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
                            .hover(|style| style.opacity(0.9))
                            .cursor_pointer()
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
                            .hover(|style| style.opacity(0.8))
                            .cursor_pointer()
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
                    .hover(|style| style.opacity(0.9))
                    .cursor_pointer()
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
            .cursor_pointer()
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
                                .hover(|style| style.opacity(0.8))
                                .cursor_pointer()
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
                                .hover(move |style| style.bg(rgb(hover)))
                                .cursor_pointer()
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
                                .hover(move |style| style.bg(rgb(hover)))
                                .cursor_pointer()
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

    fn candidate_detail(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(c) = self
            .selected_candidate
            .as_ref()
            .and_then(|id| self.library_candidates.iter().find(|c| &c.id == id))
        else {
            return div().into_any_element();
        };
        let candidate_id = c.id.clone();
        let hover = self.palette.hover;
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
        let roles = c.experience.clone();
        let education = c.education.clone();
        let certifications = c.certifications.clone();

        div()
            .id("candidate-detail")
            .h_full()
            .flex_none()
            .overflow_hidden()
            .bg(rgb(self.palette.card))
            .border_l_1()
            .border_color(rgb(self.palette.border))
            .child(
                div()
                    .w(px(width))
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
                                            .flex_1()
                                            .min_w(px(0.))
                                            .overflow_hidden()
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .child(
                                                div()
                                                    .flex_none()
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
                                                    .flex_1()
                                                    .min_w(px(0.))
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
                                                            .w_full()
                                                            .overflow_hidden()
                                                            .whitespace_nowrap()
                                                            .text_ellipsis()
                                                            .text_xl()
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .child(c.name.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .w_full()
                                                            .overflow_hidden()
                                                            .whitespace_nowrap()
                                                            .text_ellipsis()
                                                            .text_sm()
                                                            .text_color(rgb(self.palette.muted))
                                                            .child(c.headline.clone()),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id("close-detail")
                                            .flex_none()
                                            .p_1()
                                            .rounded_md()
                                            .child(
                                                div()
                                                    .text_lg()
                                                    .text_color(rgb(self.palette.muted))
                                                    .child("×"),
                                            )
                                            .hover(move |style| style.bg(rgb(hover)))
                                            .cursor_pointer()
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
                                            .hover(|style| style.opacity(0.9))
                                            .cursor_pointer()
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
                                            .hover(move |style| style.bg(rgb(hover)))
                                            .cursor_pointer()
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
                                        "Notes stay on this device. Re-read extracts name, roles, education, and skills, then rebuilds the search embeddings.",
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
                                    .child(self.detail_label("Experience"))
                                    .child(if roles.is_empty() {
                                        div()
                                            .text_sm()
                                            .text_color(rgb(self.palette.muted))
                                            .child("No roles extracted yet. Use Re-read with AI.")
                                            .into_any_element()
                                    } else {
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_2()
                                            .children(roles.iter().map(|role| {
                                                div()
                                                    .p_3()
                                                    .rounded_lg()
                                                    .bg(rgb(self.palette.bg))
                                                    .border_1()
                                                    .border_color(rgb(self.palette.border))
                                                    .flex()
                                                    .flex_col()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .font_weight(FontWeight::MEDIUM)
                                                            .child(if role.title.is_empty() {
                                                                "Role".to_string()
                                                            } else {
                                                                role.title.clone()
                                                            }),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(rgb(self.palette.muted))
                                                            .child(role.label()),
                                                    )
                                                    .when(!role.description.is_empty(), |el| {
                                                        el.child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(rgb(self.palette.ink))
                                                                .child(role.description.clone()),
                                                        )
                                                    })
                                            }))
                                            .into_any_element()
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(self.detail_label("Education & certifications"))
                                    .child(if education.is_empty() && certifications.is_empty() {
                                        div()
                                            .text_sm()
                                            .text_color(rgb(self.palette.muted))
                                            .child("Nothing extracted yet.")
                                            .into_any_element()
                                    } else {
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .children(education.iter().map(|item| {
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(self.palette.ink))
                                                    .child(item.label())
                                            }))
                                            .children(certifications.iter().map(|item| {
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(self.palette.muted))
                                                    .child(format!("Certification · {item}"))
                                            }))
                                            .into_any_element()
                                    }),
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
                move |el, t| el.w(px(width * t)),
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

    /// Rebuild the chunk rows and their vectors after a note or profile edit.
    /// Runs in the background; failures surface as a notice.
    fn reindex_candidate(&mut self, id: String, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let embedder = self.embedder.lock().unwrap().clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let candidate = store
                        .candidate(&id)
                        .map_err(|error| error.to_string())?
                        .ok_or_else(|| "candidate not found".to_string())?;
                    let note = store.note(&id).ok().flatten();
                    let chunks = shortlist_core::chunk_text(
                        &candidate.index_text_with_note(note.as_deref()),
                        180,
                        28,
                    );
                    let vectors = match &embedder {
                        Some(embedder) => embedder
                            .embed_passages(&chunks)
                            .map_err(|error| format!("embedding failed: {error}"))?,
                        None => Vec::new(),
                    };
                    store
                        .save_chunks_with_vectors(
                            &id,
                            &chunks,
                            &vectors,
                            if vectors.is_empty() {
                                ""
                            } else {
                                shortlist_embed::MODEL_ID
                            },
                        )
                        .map_err(|error| error.to_string())?;
                    Ok::<(), String>(())
                })
                .await;
            if let Err(error) = result {
                this.update(cx, |this, cx| {
                    this.notice = Some(format!("Search index was not refreshed: {error}"));
                    cx.notify();
                })
                .ok();
            } else {
                this.update(cx, |this, cx| {
                    if this.route == Route::Developer {
                        this.load_dev(cx);
                    }
                })
                .ok();
            }
        })
        .detach();
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
        let embedder = self.embedder.lock().unwrap().clone();
        let text = candidate.text.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let profile = shortlist_ai::OpenCode
                        .extract_profile(Some(&model), &text)
                        .map_err(|error| format!("AI extraction failed: {error}"))?;
                    let mut updated = candidate.clone();
                    merge_extracted(&mut updated, profile);
                    let note = store.note(&updated.id).ok().flatten();
                    let chunks = shortlist_core::chunk_text(
                        &updated.index_text_with_note(note.as_deref()),
                        180,
                        28,
                    );
                    if let Some(embedder) = embedder {
                        match embedder.embed_passages(&chunks) {
                            Ok(vectors) => {
                                store
                                    .update_candidate(&updated)
                                    .map_err(|error| error.to_string())?;
                                store
                                    .save_chunks_with_vectors(
                                        &updated.id,
                                        &chunks,
                                        &vectors,
                                        shortlist_embed::MODEL_ID,
                                    )
                                    .map_err(|error| error.to_string())?;
                            }
                            Err(error) => {
                                // Keep the refreshed profile even if the model
                                // file disappeared; fall back to text chunks.
                                store
                                    .insert_candidate(&updated, &chunks)
                                    .map_err(|error| error.to_string())?;
                                return Err(format!("profile saved; embedding failed: {error}"));
                            }
                        }
                    } else {
                        store
                            .insert_candidate(&updated, &chunks)
                            .map_err(|error| error.to_string())?;
                    }
                    Ok::<usize, String>(chunks.len())
                })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(chunks) => {
                        this.notice = Some(format!(
                            "Profile refreshed with AI · {chunks} chunks rebuilt."
                        ));
                    }
                    Err(error) => {
                        this.notice = Some(error);
                    }
                }
                this.reextracting = None;
                this.reload_candidates(cx);
                this.reload_library(cx);
                if this.route == Route::Developer {
                    this.load_dev(cx);
                }
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
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| this.new_chat(cx)))
    }

    fn chat_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hover = self.palette.hover;
        div()
            .w(px(236.))
            .h_full()
            .flex_none()
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
                                    .hover(move |style| style.bg(rgb(hover)))
                                    .cursor_pointer()
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
                                    .hover(move |style| style.bg(rgb(hover)))
                                    .cursor_pointer()
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
        let lines: Vec<&str> = text.lines().collect();
        let mut blocks: Vec<AnyElement> = Vec::new();
        let mut index = 0;

        while index < lines.len() {
            let trimmed = lines[index].trim();

            // A table is only a table when a separator row follows the header,
            // so trying this on every pipe-containing line is safe.
            if trimmed.contains('|')
                && let Some((table, next)) = markdown_table::parse(&lines, index)
            {
                blocks.push(self.table_block(&table, cx));
                index = next;
                continue;
            }

            index += 1;

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

    /// Renders a Markdown table as a bordered, zebra-striped grid. Columns are
    /// sized from their content, so a long "Why" column stays readable while
    /// rank and gap columns stay compact.
    fn table_block(&self, table: &Table, cx: &mut Context<Self>) -> AnyElement {
        let columns = table.header.len();
        if columns == 0 {
            return div().into_any_element();
        }
        let weights = self.table_column_weights(table);
        let palette = self.palette;
        let alignments: Vec<Alignment> = (0..columns)
            .map(|column| {
                table
                    .alignments
                    .get(column)
                    .copied()
                    .unwrap_or(Alignment::Left)
            })
            .collect();

        let mut header_cells = Vec::with_capacity(columns);
        for column in 0..columns {
            header_cells.push(self.table_cell(
                weights[column],
                table.header.get(column).map(String::as_str).unwrap_or(""),
                TableCellKind::Header(alignments[column]),
                column + 1 < columns,
                cx,
            ));
        }
        let header = div()
            .w_full()
            .flex()
            .flex_row()
            .items_stretch()
            .bg(rgb(palette.chip))
            .border_b_1()
            .border_color(rgb(palette.border))
            .children(header_cells);

        let mut rows = Vec::with_capacity(table.rows.len());
        for (row_index, row) in table.rows.iter().enumerate() {
            let mut cells = Vec::with_capacity(columns);
            for column in 0..columns {
                cells.push(self.table_cell(
                    weights[column],
                    row.get(column).map(String::as_str).unwrap_or(""),
                    TableCellKind::Body(alignments[column]),
                    column + 1 < columns,
                    cx,
                ));
            }
            rows.push(
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_stretch()
                    .when(row_index % 2 == 1, |el| el.bg(rgb(palette.chip)))
                    .when(row_index + 1 < table.rows.len(), |el| {
                        el.border_b_1().border_color(rgb(palette.border))
                    })
                    .children(cells)
                    .into_any_element(),
            );
        }

        div()
            .w_full()
            .my_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(header)
            .children(rows)
            .into_any_element()
    }

    fn table_cell(
        &self,
        weight: f32,
        text: &str,
        kind: TableCellKind,
        divider: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cell = div()
            .flex_grow(weight)
            .flex_basis(px(0.))
            .min_w(px(0.))
            .px_2()
            .py_2()
            .when(divider, |el| {
                el.border_r_1().border_color(rgb(self.palette.border))
            });

        match kind {
            TableCellKind::Header(align) => {
                let label = div()
                    .w_full()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(self.palette.muted))
                    .child(plain_inline(text));
                let label = match align {
                    Alignment::Left => label.text_left(),
                    Alignment::Center => label.text_center(),
                    Alignment::Right => label.text_right(),
                };
                cell.child(label).into_any_element()
            }
            TableCellKind::Body(align) => cell
                .child(self.rich_line_aligned(text, false, align, cx))
                .into_any_element(),
        }
    }

    /// Relative column widths: wider content earns more space, with a bump for
    /// columns that render candidate chips and a cap for short value columns.
    fn table_column_weights(&self, table: &Table) -> Vec<f32> {
        table
            .column_widths()
            .into_iter()
            .enumerate()
            .map(|(column, width)| {
                let mut weight = 10.0 + (width.min(400) as f32).sqrt() * 5.0;
                if table
                    .rows
                    .iter()
                    .all(|row| is_short_value(row.get(column).map(String::as_str).unwrap_or("")))
                {
                    weight = weight.min(18.0);
                }
                let has_candidate = table.rows.iter().any(|row| {
                    row.get(column).is_some_and(|cell| {
                        !shortlist_core::candidate_spans(&self.library_candidates, cell).is_empty()
                    })
                });
                if has_candidate {
                    weight = weight.max(34.0);
                }
                weight.clamp(14.0, 72.0)
            })
            .collect()
    }

    fn rich_line(&self, text: &str, heading: bool, cx: &mut Context<Self>) -> AnyElement {
        self.rich_line_aligned(text, heading, Alignment::Left, cx)
    }

    fn rich_line_aligned(
        &self,
        text: &str,
        heading: bool,
        align: Alignment,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let spans = shortlist_core::candidate_spans(&self.library_candidates, text);
        if spans.is_empty() {
            let cleaned = plain_inline(text);
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
            let element = element.w_full();
            let element = match align {
                Alignment::Left => element.text_left(),
                Alignment::Center => element.text_center(),
                Alignment::Right => element.text_right(),
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
        let row = div()
            .w_full()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_1();
        let row = match align {
            Alignment::Left => row.justify_start(),
            Alignment::Center => row.justify_center(),
            Alignment::Right => row.justify_end(),
        };
        row.children(fragments).into_any_element()
    }

    fn push_words(&self, out: &mut Vec<AnyElement>, text: &str, heading: bool, bold: &mut bool) {
        let mut parts = text.split("**").peekable();
        while let Some(part) = parts.next() {
            let cleaned = plain_inline(part);
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
            .cursor_pointer()
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

    /// The rotating status line: it fades each phrase in and out, while the
    /// dots keep a small continuous pulse so the turn never looks stuck.
    fn working_phrase(&self) -> impl IntoElement {
        let ink = self.palette.ink;
        let count = WORKING_PHRASES.len() as f32;
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgb(self.palette.accent_soft))
            .self_start()
            .child(self.thinking_dots())
            .child(div().with_animation(
                "working-phrase",
                Animation::new(Duration::from_millis(10_000)).repeat(),
                move |_el, t| {
                    let position = t * count;
                    let index = (position as usize).min(WORKING_PHRASES.len() - 1);
                    let local = position.fract();
                    let fade = 0.55 + 0.45 * (local * std::f32::consts::PI).sin();
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(ink))
                        .opacity(fade)
                        .child(format!("{}…", WORKING_PHRASES[index]))
                },
            ))
    }

    fn streaming_bubble(&self, _cx: &mut Context<Self>) -> AnyElement {
        let text = self.streaming.clone().unwrap_or_default();
        let progress: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        let muted = self.palette.muted;
        let progress_block = (!progress.is_empty()).then(|| {
            div().w_full().flex().flex_col().gap_1().children(
                progress
                    .into_iter()
                    .map(move |line| div().text_xs().text_color(rgb(muted)).child(line)),
            )
        });
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
                            .child("Shortlist · cooking"),
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
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(self.working_phrase())
                            .children(progress_block),
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
            .hover(|style| style.opacity(if self.busy { 1.0 } else { 0.9 }))
            .when(self.busy, |el| el.cursor_not_allowed())
            .when(!self.busy, |el| el.cursor_pointer())
            .on_click(cx.listener(|this, _, window, cx| {
                let value = this.composer.read(cx).value().to_string();
                if !value.trim().is_empty() {
                    this.send(value, window, cx);
                }
            }))
    }

    fn load_dev(&mut self, cx: &mut Context<Self>) {
        self.dev_tables = self.store.table_counts().unwrap_or_default();
        match self.store.inspect_table(&self.dev_table, 60) {
            Ok(table) => {
                self.dev_data = Some(table);
                self.notice = None;
            }
            Err(error) => {
                self.dev_data = None;
                self.notice = Some(format!("Could not inspect {}: {error}", self.dev_table));
            }
        }
        cx.notify();
    }

    fn select_dev_table(&mut self, table: String, cx: &mut Context<Self>) {
        self.dev_table = table;
        self.load_dev(cx);
    }

    fn developer_view(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let tables = self.dev_tables.clone();
        let active = self.dev_table.clone();
        let data = self.dev_data.clone();
        let hover = self.palette.hover;
        let accent_soft = self.palette.accent_soft;
        let refresh = div()
            .id("dev-refresh")
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
            .child("Refresh")
            .hover(|style| style.opacity(0.9))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| this.load_dev(cx)));

        let actions = div().flex().items_center().gap_2().child(refresh);

        let tabs = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_2()
            .children(tables.iter().map(|(name, count)| {
                let selected = name == &active;
                let target = name.clone();
                div()
                    .id(SharedString::from(format!("dev-table-{name}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(if selected {
                        rgb(self.palette.accent_soft)
                    } else {
                        rgb(self.palette.card)
                    })
                    .border_1()
                    .border_color(if selected {
                        rgb(self.palette.accent_soft)
                    } else {
                        rgb(self.palette.border)
                    })
                    .text_xs()
                    .font_weight(if selected {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(if selected {
                        rgb(self.palette.accent)
                    } else {
                        rgb(self.palette.ink)
                    })
                    .child(name.clone())
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(self.palette.muted))
                            .child(count.to_string()),
                    )
                    .hover(move |style| {
                        style.bg(if selected {
                            rgb(accent_soft)
                        } else {
                            rgb(hover)
                        })
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_dev_table(target.clone(), cx);
                    }))
            }));

        let body = match &data {
            Some(table) => {
                let schema = table
                    .columns
                    .iter()
                    .map(|column| {
                        format!(
                            "{} {}{}",
                            column.name,
                            if column.kind.is_empty() {
                                "ANY"
                            } else {
                                column.kind.as_str()
                            },
                            if column.primary { " PK" } else { "" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" · ");
                let shown = table.rows.len();
                let total = table.total;
                let rows = table.rows.iter().enumerate().map(|(index, row)| {
                    let primary = row
                        .first()
                        .map(|value| {
                            if value.chars().count() > 22 {
                                format!("{}…", value.chars().take(22).collect::<String>())
                            } else {
                                value.clone()
                            }
                        })
                        .unwrap_or_default();
                    div()
                        .p_4()
                        .rounded_lg()
                        .bg(rgb(self.palette.card))
                        .border_1()
                        .border_color(rgb(self.palette.border))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_3()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!("row {} · {primary}", index + 1)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(self.palette.muted))
                                        .child(format!("{} columns", table.columns.len())),
                                ),
                        )
                        .children(table.columns.iter().zip(row.iter()).map(|(column, value)| {
                            let muted = value == "NULL";
                            let embedding = column.name == "embedding";
                            div()
                                .flex()
                                .items_start()
                                .gap_3()
                                .py_1()
                                .child(
                                    div()
                                        .w(px(150.))
                                        .flex_shrink_0()
                                        .text_xs()
                                        .font_family(mono_font())
                                        .text_color(rgb(self.palette.muted))
                                        .child(column.name.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .text_xs()
                                        .font_family(mono_font())
                                        .text_color(if muted {
                                            rgb(self.palette.muted)
                                        } else if embedding {
                                            rgb(self.palette.accent)
                                        } else {
                                            rgb(self.palette.ink)
                                        })
                                        .child(value.clone()),
                                )
                        }))
                });

                vec![
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(self.palette.muted))
                                .child(format!(
                                    "Showing {shown} of {total} rows · newest first · schema"
                                )),
                        )
                        .child(
                            div()
                                .p_3()
                                .rounded_md()
                                .bg(rgb(self.palette.bg))
                                .border_1()
                                .border_color(rgb(self.palette.border))
                                .text_xs()
                                .font_family(mono_font())
                                .text_color(rgb(self.palette.muted))
                                .child(schema),
                        )
                        .into_any_element(),
                    div()
                        .text_xs()
                        .text_color(rgb(self.palette.muted))
                        .child(
                            "Embeddings are BLOBs. The preview shows dimensions, L2 norm, and the first eight values. Text is clipped at 600 characters. Register MCP in Settings to expose the same tools to OpenCode natively.",
                        )
                        .into_any_element(),
                ]
                .into_iter()
                .chain(rows.map(|row| row.into_any_element()))
                .collect::<Vec<_>>()
            }
            None => vec![
                div()
                    .text_sm()
                    .text_color(rgb(self.palette.muted))
                    .child(
                        self.notice
                            .clone()
                            .unwrap_or_else(|| "No data loaded.".into()),
                    )
                    .into_any_element(),
            ],
        };

        div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_10()
                    .pt_8()
                    .pb_5()
                    .border_b_1()
                    .border_color(rgb(self.palette.border))
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_6()
                    .child(
                        self.page_header(
                            "Developer",
                            "Read-only view of SQLite: parsed fields, UUIDs, and embedding vectors"
                                .into(),
                        ),
                    )
                    .child(actions),
            )
            .child(div().px_10().py_4().child(tabs))
            .child(
                div()
                    .id("developer-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px_10()
                    .pb_10()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(body),
            )
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
            .items_start()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(760.))
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(self.page_header(
                        "Settings",
                        "Connection, index, updates, and local data".into(),
                    ))
                    .child(self.connection_card(cx))
                    .child(self.mcp_card(cx))
                    .child(self.index_card(cx))
                    .child(self.data_card(cx))
                    .child(self.update_card(cx))
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
            .cursor_pointer()
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_model_dialog(window, cx);
            }))
    }

    fn mcp_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                IconName::Bot,
                "OpenCode tools",
                "Native MCP access to the candidate library",
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(self.palette.muted))
                    .child(
                        "Register the local MCP server once and OpenCode can call stats, find, screen, get, read, grep, note, and compare directly. OpenCode reloads the config without a restart.",
                    ),
            )
            .child(
                div()
                    .id("settings-register-mcp")
                    .flex()
                    .items_center()
                    .gap_2()
                    .self_start()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(self.palette.accent_soft))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(self.palette.accent))
                    .child(
                        Icon::new(IconName::Bot)
                            .text_color(rgb(self.palette.accent))
                            .w(px(14.))
                            .h(px(14.)),
                    )
                    .child("Register MCP")
                    .hover(move |style| style.bg(rgb(hover)))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.register_mcp(cx))),
            )
            .when(self.mcp_status.is_some(), |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(rgb(self.palette.muted))
                        .child(self.mcp_status.clone().unwrap_or_default()),
                )
            })
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
                    .cursor_pointer()
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
                            .cursor_pointer()
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
                            .hover(|style| style.opacity(0.8))
                            .cursor_pointer()
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
                            .cursor_pointer()
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
                                    .cursor_pointer()
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
    let experience: Vec<shortlist_core::Experience> = profile
        .experience
        .into_iter()
        .filter(|role| !role.title.trim().is_empty())
        .take(10)
        .map(|mut role| {
            role.title = role.title.trim().chars().take(120).collect();
            role.company = role.company.trim().chars().take(120).collect();
            role.start = role.start.trim().chars().take(40).collect();
            role.end = role.end.trim().chars().take(40).collect();
            role.description = role.description.trim().chars().take(420).collect();
            role.years = role.years.clamp(0.0, 60.0);
            role
        })
        .collect();
    if !experience.is_empty() {
        candidate.experience = experience;
    }
    let education: Vec<shortlist_core::Education> = profile
        .education
        .into_iter()
        .filter(|item| !item.degree.trim().is_empty() || !item.institution.trim().is_empty())
        .take(4)
        .map(|mut item| {
            item.degree = item.degree.trim().chars().take(160).collect();
            item.institution = item.institution.trim().chars().take(160).collect();
            item.year = item.year.trim().chars().take(20).collect();
            item
        })
        .collect();
    if !education.is_empty() {
        candidate.education = education;
    }
    let certifications: Vec<String> = profile
        .certifications
        .into_iter()
        .map(|item| item.trim().chars().take(120).collect::<String>())
        .filter(|item| !item.is_empty())
        .take(10)
        .collect();
    if !certifications.is_empty() {
        candidate.certifications = certifications;
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

fn mcp_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    ["shortlist-mcp", "shortlist-mcp.exe"]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.exists())
        .or_else(|| {
            let bundled = dir.join("../Resources/shortlist-mcp");
            bundled.exists().then_some(bundled)
        })
}

/// Register the local MCP server with OpenCode once. Best-effort: if the
/// helper binary is not built, the harness text protocol still works.
fn ensure_mcp_registered() -> Result<bool, String> {
    let output = Command::new("opencode")
        .args(["mcp", "list"])
        .output()
        .map_err(|error| format!("opencode mcp list: {error}"))?;
    let listed = String::from_utf8_lossy(&output.stdout).to_lowercase();
    if listed.contains("shortlist") {
        return Ok(false);
    }
    let binary = mcp_binary().ok_or_else(|| "shortlist-mcp is not built".to_string())?;
    let data_dir = shortlist_core::data_dir();
    let data_dir = if data_dir.is_absolute() {
        data_dir
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&data_dir))
            .unwrap_or(data_dir)
    };
    std::fs::create_dir_all(&data_dir).ok();
    let env = format!("SHORTLIST_DATA_DIR={}", data_dir.display());
    let output = Command::new("opencode")
        .args(["mcp", "add", "shortlist", "--global", "--env", &env, "--"])
        .arg(&binary)
        .output()
        .map_err(|error| format!("opencode mcp add: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        });
    }
    let _ = configure_native_mcp();
    Ok(true)
}

/// Code Mode is OpenCode's default for MCP. Shortlist exposes eight small tools,
/// and they work better as direct native tool definitions.
fn configure_native_mcp() -> Result<(), String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(home).join(".config"))
        .join("opencode");
    let path = config_dir.join("opencode.json");
    let text = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
    let mut config: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| error.to_string())?;
    let server = config
        .get_mut("mcp")
        .and_then(|mcp| mcp.get_mut("servers"))
        .and_then(|servers| servers.get_mut("shortlist"))
        .ok_or_else(|| "shortlist MCP entry not found".to_string())?;
    server["codemode"] = serde_json::json!(false);
    std::fs::write(
        path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&config).map_err(|error| error.to_string())?
        ),
    )
    .map_err(|error| error.to_string())
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

fn mono_font() -> SharedString {
    if cfg!(target_os = "macos") {
        "SF Mono".into()
    } else if cfg!(target_os = "windows") {
        "Consolas".into()
    } else {
        "monospace".into()
    }
}

/// Removes inline Markdown markers before plain-text fragments are rendered.
fn plain_inline(text: &str) -> String {
    text.replace("**", "")
        .replace("~~", "")
        .replace('`', "")
        .replace('*', "")
}

/// True for rank-like cells (`1`, `2`, `—`) whose column should stay narrow.
fn is_short_value(cell: &str) -> bool {
    let cell = cell.trim();
    cell.chars().count() <= 4
        && cell
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '#' | '-' | '—' | '.' | '%' | '+'))
}

// Shared shell metrics. The sidebar keeps a fixed width, and the candidate
// drawer only shrinks into the content area when the window has no room left
// for the list beside it.
const SIDEBAR_WIDTH_OPEN: f32 = 224.;
const SIDEBAR_WIDTH_CLOSED: f32 = 76.;
const DETAIL_WIDTH: f32 = 430.;
const DETAIL_MIN_WIDTH: f32 = 300.;
const LIST_MIN_WIDTH: f32 = 280.;

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

/// Pause after the last edit before the note is written to SQLite.
const NOTE_AUTOSAVE_DELAY: Duration = Duration::from_millis(500);

/// The note pane shows one surface at a time, like Obsidian's editing and
/// reading views, instead of splitting the editor and preview side by side.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NoteMode {
    Write,
    Preview,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NoteSaveStatus {
    Saved,
    Saving,
    Failed,
}

struct NoteDialog {
    parent: WeakEntity<ShortlistApp>,
    store: Arc<Store>,
    candidate_id: String,
    palette: Palette,
    editor: Entity<EditorState>,
    mode: NoteMode,
    /// The last Markdown committed to SQLite, so a debounced save can skip
    /// no-op writes.
    saved: String,
    status: NoteSaveStatus,
    /// Bumped on every edit; an older debounce sees a stale token and stops.
    save_generation: u64,
    /// The note changed since the search index was last rebuilt.
    index_dirty: bool,
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
        let saved = text.clone();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .searchable(false)
                .soft_wrap(true)
                .default_value(text)
        });
        cx.defer_in(window, |this, window, cx| {
            this.editor.update(cx, |state, cx| state.focus(window, cx));
        });
        let sub = cx.subscribe_in(
            &editor,
            window,
            |this, _editor, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.schedule_autosave(cx);
                }
            },
        );

        Self {
            parent,
            store,
            candidate_id,
            palette,
            editor,
            mode: NoteMode::Write,
            saved,
            status: NoteSaveStatus::Saved,
            save_generation: 0,
            index_dirty: false,
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
        // `set_value` intentionally suppresses input events, so formatting
        // buttons must ask for an autosave themselves.
        self.schedule_autosave(cx);
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

    fn set_mode(&mut self, mode: NoteMode, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        match mode {
            NoteMode::Write => {
                self.editor.update(cx, |state, cx| state.focus(window, cx));
            }
            NoteMode::Preview => {
                // Commit before reading, then drop focus so keystrokes cannot
                // change the hidden source buffer.
                self.save_now(cx);
                window.blur(cx);
            }
        }
        cx.notify();
    }

    /// Saves after a short pause so typing stays smooth and the note is never
    /// left waiting for a Save button.
    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        let current = self.markdown(cx);
        if current == self.saved {
            if self.status != NoteSaveStatus::Saved {
                self.status = NoteSaveStatus::Saved;
                cx.notify();
            }
            return;
        }

        self.status = NoteSaveStatus::Saving;
        self.save_generation = self.save_generation.wrapping_add(1);
        let generation = self.save_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTE_AUTOSAVE_DELAY).await;
            this.update(cx, |this, cx| {
                if this.save_generation == generation {
                    this.save_now(cx);
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Writes the current Markdown immediately. Returns `false` when SQLite
    /// rejected the write, so closing can keep the dialog open.
    fn save_now(&mut self, cx: &mut Context<Self>) -> bool {
        self.save_generation = self.save_generation.wrapping_add(1);
        let markdown = self.markdown(cx);
        if markdown == self.saved {
            self.status = NoteSaveStatus::Saved;
            cx.notify();
            return true;
        }

        let empty = markdown.trim().is_empty();
        let result = if empty {
            self.store.delete_note(&self.candidate_id)
        } else {
            self.store.save_note(&self.candidate_id, &markdown)
        };

        match result {
            Ok(()) => {
                self.had_note = !empty;
                self.saved = markdown;
                self.status = NoteSaveStatus::Saved;
                self.index_dirty = true;
                cx.notify();
                true
            }
            Err(_) => {
                // Keep `saved` untouched so the next edit or close retries.
                self.status = NoteSaveStatus::Failed;
                cx.notify();
                false
            }
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.save_now(cx) {
            return;
        }
        self.reindex_if_dirty(cx);
        window.close_dialog(cx);
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A pending autosave must not resurrect the note after this delete.
        self.save_generation = self.save_generation.wrapping_add(1);
        match self.store.delete_note(&self.candidate_id) {
            Ok(()) => {
                self.saved.clear();
                self.had_note = false;
                self.index_dirty = false;
                self.schedule_reindex(cx);
                self.notify_parent("Note removed.", cx);
                window.close_dialog(cx);
            }
            Err(_) => {
                self.status = NoteSaveStatus::Failed;
                cx.notify();
            }
        }
    }

    fn reindex_if_dirty(&mut self, cx: &mut Context<Self>) {
        if self.index_dirty {
            self.index_dirty = false;
            self.schedule_reindex(cx);
        }
    }

    fn schedule_reindex(&self, cx: &mut Context<Self>) {
        if let Some(parent) = self.parent.upgrade() {
            let candidate_id = self.candidate_id.clone();
            parent.update(cx, |app, cx| {
                app.reindex_candidate(candidate_id, cx);
            });
        }
    }

    fn notify_parent(&self, message: &'static str, cx: &mut Context<Self>) {
        if let Some(parent) = self.parent.upgrade() {
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
        let write_mode = self.mode == NoteMode::Write;

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
            div().child("“").into_any_element(),
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

        let toolbar = div()
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
            .child(quote);

        let body = if write_mode {
            div()
                .w_full()
                .flex_1()
                .min_h(px(0.))
                .px_3()
                .py_2()
                .child(
                    Editor::new(&self.editor)
                        .appearance(false)
                        .bordered(false)
                        .font_family(ui_font())
                        .text_size(px(13.))
                        .w_full()
                        .h_full(),
                )
                .into_any_element()
        } else if markdown.trim().is_empty() {
            div()
                .w_full()
                .flex_1()
                .min_h(px(0.))
                .p_6()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .items_center()
                        .child(
                            Icon::new(IconName::FileText)
                                .text_color(rgb(palette.muted))
                                .w(px(22.))
                                .h(px(22.)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(palette.muted))
                                .child("Nothing to read yet"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(palette.muted))
                                .child("Switch to Write and start typing."),
                        ),
                )
                .into_any_element()
        } else {
            div()
                .w_full()
                .flex_1()
                .min_h(px(0.))
                .p_6()
                .child(
                    TextView::markdown("note-preview", markdown.clone())
                        .selectable(true)
                        .scrollable(true),
                )
                .into_any_element()
        };

        let (status_label, status_color) = match self.status {
            NoteSaveStatus::Saved => ("Saved", palette.success),
            NoteSaveStatus::Saving => ("Saving…", palette.muted),
            NoteSaveStatus::Failed => ("Couldn't save", palette.danger),
        };

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .when(write_mode, |el| el.child(toolbar))
            .child(
                div()
                    .w_full()
                    .h(px(470.))
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.bg))
                    .overflow_hidden()
                    .child(self.pane_header(palette, self.mode_switch(palette, cx)))
                    .child(body),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(6.))
                                    .h(px(6.))
                                    .rounded_full()
                                    .bg(rgb(status_color)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(status_color))
                                    .child(status_label),
                            )
                            .child(div().text_xs().text_color(rgb(palette.muted)).child("·"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette.muted))
                                    .child(format!("{words} words")),
                            )
                            .child(div().text_xs().text_color(rgb(palette.muted)).child("·"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette.muted))
                                    .child("Only on this device"),
                            ),
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
                                        .hover(|style| style.opacity(0.75))
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.delete(window, cx);
                                        })),
                                )
                            })
                            .child(
                                div()
                                    .id("note-done")
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .bg(rgb(palette.accent))
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(palette.on_accent))
                                    .child("Done")
                                    .hover(|style| style.opacity(0.9))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.close(window, cx);
                                    })),
                            ),
                    ),
            )
    }
}

impl NoteDialog {
    /// Obsidian-style segmented control for the single note surface.
    fn mode_switch(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let active = self.mode;
        div()
            .flex()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.sidebar))
            .child(note_mode_tab(
                "note-mode-write",
                "Write",
                IconName::FileText,
                active == NoteMode::Write,
                NoteMode::Write,
                palette,
                cx,
            ))
            .child(note_mode_tab(
                "note-mode-preview",
                "Preview",
                IconName::Eye,
                active == NoteMode::Preview,
                NoteMode::Preview,
                palette,
                cx,
            ))
            .into_any_element()
    }

    fn pane_header(&self, palette: Palette, mode_switch: AnyElement) -> AnyElement {
        let surface = match self.mode {
            NoteMode::Write => "Markdown",
            NoteMode::Preview => "Rendered",
        };
        div()
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.card))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.muted))
                            .child("NOTE"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted))
                            .child(surface),
                    ),
            )
            .child(mode_switch)
            .into_any_element()
    }
}

fn note_mode_tab(
    id: &'static str,
    label: &'static str,
    icon: IconName,
    active: bool,
    mode: NoteMode,
    palette: Palette,
    cx: &mut Context<NoteDialog>,
) -> AnyElement {
    div()
        .id(id)
        .h(px(24.))
        .px_2()
        .flex()
        .items_center()
        .gap_1()
        .rounded_md()
        .bg(rgb(if active {
            palette.card
        } else {
            palette.sidebar
        }))
        .border_1()
        .border_color(rgb(if active {
            palette.border
        } else {
            palette.sidebar
        }))
        .text_xs()
        .text_color(rgb(if active { palette.ink } else { palette.muted }))
        .hover(move |style| style.bg(rgb(if active { palette.card } else { palette.hover })))
        .cursor_pointer()
        .child(
            Icon::new(icon)
                .w(px(12.))
                .h(px(12.))
                .text_color(rgb(if active {
                    palette.accent
                } else {
                    palette.muted
                })),
        )
        .child(div().child(label))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.set_mode(mode, window, cx);
        }))
        .into_any_element()
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
        .hover(move |style| {
            style.bg(if active {
                rgb(palette.accent_soft)
            } else {
                rgb(palette.hover)
            })
        })
        .child(label)
        .cursor_pointer()
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
            // Below this the shell has no room for the sidebar, the candidate
            // list, and the detail drawer, and dialogs start to clip.
            window_min_size: Some(size(px(780.), px(560.))),
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
