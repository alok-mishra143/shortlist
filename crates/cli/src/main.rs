use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use shortlist_core::ModelRef;
use shortlist_embed::{Embedder, MODEL_ID};
use shortlist_harness::{Budget, Registry, ToolContext, estimate_tokens};
use shortlist_store::Store;
use std::path::PathBuf;
use std::process::Command;

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

fn absolute_data_dir() -> PathBuf {
    let dir = shortlist_core::data_dir();
    if dir.is_absolute() {
        dir
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&dir))
            .unwrap_or(dir)
    }
}

fn mcp_list() -> Result<String> {
    let output = Command::new("opencode")
        .args(["mcp", "list"])
        .output()
        .map_err(|error| anyhow!("could not run opencode: {error}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn install_mcp() -> Result<String> {
    if mcp_list()?.to_lowercase().contains("shortlist") {
        return Ok(
            "Shortlist MCP is already registered. Remove the `shortlist` entry from OpenCode config to change it."
                .into(),
        );
    }
    let binary = mcp_binary().ok_or_else(|| {
        anyhow!(
            "shortlist-mcp was not found next to this executable. Build it with `cargo build -p shortlist-mcp`."
        )
    })?;
    let data_dir = absolute_data_dir();
    std::fs::create_dir_all(&data_dir).ok();
    let env = format!("SHORTLIST_DATA_DIR={}", data_dir.display());
    let output = Command::new("opencode")
        .args(["mcp", "add", "shortlist", "--global", "--env", &env, "--"])
        .arg(&binary)
        .output()
        .map_err(|error| anyhow!("could not run opencode: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        return Err(anyhow!(
            "{}",
            if stderr.is_empty() { &stdout } else { &stderr }
        ));
    }
    let native = configure_native_mcp().is_ok();
    Ok(format!(
        "Registered shortlist MCP at {} (data: {}).\n{}\n{}",
        binary.display(),
        data_dir.display(),
        if native {
            "Exposed native tools (codemode disabled)."
        } else {
            "Set \"codemode\": false on the shortlist MCP entry to expose native tools."
        },
        if stdout.is_empty() {
            "Run `shortlist-cli mcp status` to check."
        } else {
            &stdout
        }
    ))
}

/// Turn off Code Mode for this server so OpenCode shows the eight tools as
/// native tool definitions. Best-effort: JSONC configs are left untouched.
fn configure_native_mcp() -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_default();
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(home).join(".config"))
        .join("opencode");
    let path = config_dir.join("opencode.json");
    if !path.exists() {
        return Err(anyhow!("OpenCode config not found"));
    }
    let text = std::fs::read_to_string(&path)?;
    let mut config: Value = serde_json::from_str(&text)?;
    let Some(server) = config
        .get_mut("mcp")
        .and_then(|mcp| mcp.get_mut("servers"))
        .and_then(|servers| servers.get_mut("shortlist"))
    else {
        return Err(anyhow!("shortlist MCP entry not found"));
    };
    server["codemode"] = json!(false);
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&config)?),
    )?;
    Ok(())
}

fn mcp_status() -> Result<String> {
    let list = mcp_list()?;
    let binary = mcp_binary()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "not built".into());
    Ok(format!(
        "MCP helper: {binary}\nData dir: {}\n\n{list}",
        absolute_data_dir().display()
    ))
}

fn default_model() -> ModelRef {
    ModelRef {
        id: "deepseek-v4.1-flash".into(),
        provider_id: "opencode-go".into(),
        name: "DeepSeek V4.1 Flash".into(),
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");
    let store = Store::open(&shortlist_core::db_path())?;

    match command {
        "seed" => {
            let n = store.seed_demo()?;
            println!("added {n} demo candidates");
        }
        "import" => {
            if args.len() < 2 {
                return Err(anyhow!("usage: shortlist-cli import <file>..."));
            }
            for path in &args[1..] {
                let candidate = shortlist_parse::parse_resume(std::path::Path::new(path))?;
                let chunks = shortlist_core::chunk_text(&candidate.index_text(), 180, 28);
                store.insert_candidate(&candidate, &chunks)?;
                println!(
                    "{} — {} · {} skills · {} chunks",
                    candidate.name,
                    candidate.headline,
                    candidate.skills.len(),
                    chunks.len()
                );
            }
        }
        "index" => {
            let embedder = Embedder::load()?;
            let chunks = store.chunks_missing_vectors()?;
            if chunks.is_empty() {
                println!("index already up to date");
                return Ok(());
            }
            let total = chunks.len();
            let mut done = 0;
            for batch in chunks.chunks(24) {
                let texts: Vec<String> = batch.iter().map(|(_, _, t)| t.clone()).collect();
                let vectors = embedder.embed_passages(&texts)?;
                for ((id, _, _), vector) in batch.iter().zip(vectors.iter()) {
                    store.set_chunk_vector(id, vector, MODEL_ID)?;
                }
                done += batch.len();
            }
            println!("embedded {done}/{total} chunks");
        }
        "reindex" => {
            let embedder = Embedder::load()?;
            let candidates = store.list_candidates("", 100_000)?;
            let total = candidates.len();
            let mut done = 0;
            for candidate in candidates {
                let note = store.note(&candidate.id).ok().flatten();
                // Refresh the profile index body and FTS at the same time as the
                // vectors, so an upgraded library has no stale search fields.
                store.update_candidate(&candidate)?;
                let chunks = shortlist_core::chunk_text(
                    &candidate.index_text_with_note(note.as_deref()),
                    180,
                    28,
                );
                let vectors = embedder.embed_passages(&chunks)?;
                store.save_chunks_with_vectors(&candidate.id, &chunks, &vectors, MODEL_ID)?;
                done += 1;
                if done % 25 == 0 {
                    eprintln!("reindexed {done}/{total}");
                }
            }
            println!("reindexed {done}/{total} candidates");
        }
        "tools" => {
            let registry = Registry::builtin();
            println!("{}", registry.catalog());
            println!("\nCall one directly, without a model:\n  shortlist-cli tool find q=rust n=5");
        }
        "mcp" => match args.get(1).map(String::as_str) {
            Some("install") => println!("{}", install_mcp()?),
            Some("status") | None => println!("{}", mcp_status()?),
            Some(other) => {
                return Err(anyhow!(
                    "unknown mcp action '{other}'. Use `shortlist-cli mcp install` or `shortlist-cli mcp status`."
                ));
            }
        },
        "db" => {
            let table = args.get(1).map(String::as_str).unwrap_or("");
            if table.is_empty() {
                println!("SQLite tables");
                for (name, count) in store.table_counts()? {
                    println!("  {name:<20} {count} rows");
                }
                println!("\nInspect one:\n  shortlist-cli db chunks");
                return Ok(());
            }
            let data = store.inspect_table(table, 5)?;
            println!(
                "{} · {} rows · showing {}\n",
                data.name,
                data.total,
                data.rows.len()
            );
            println!(
                "schema: {}\n",
                data.columns
                    .iter()
                    .map(|column| format!(
                        "{} {}{}",
                        column.name,
                        if column.kind.is_empty() {
                            "ANY"
                        } else {
                            column.kind.as_str()
                        },
                        if column.primary { " PK" } else { "" }
                    ))
                    .collect::<Vec<_>>()
                    .join(" · ")
            );
            for (index, row) in data.rows.iter().enumerate() {
                println!("--- row {} ---", index + 1);
                for (column, value) in data.columns.iter().zip(row.iter()) {
                    println!("{} = {}", column.name, value);
                }
            }
        }
        "tool" => {
            if args.len() < 2 {
                return Err(anyhow!("usage: shortlist-cli tool <name> [k=v]..."));
            }
            let registry = Registry::builtin();
            let known = registry.names();
            let rendered = args[1..]
                .iter()
                .map(|arg| match arg.split_once('=') {
                    Some((key, value)) => {
                        format!("{key}=\"{}\"", value.replace('"', "\\\""))
                    }
                    None => arg.clone(),
                })
                .collect::<Vec<_>>()
                .join(" ");
            let block = format!("```shortlist\n{rendered}\n```");
            let call = shortlist_harness::parse_calls(&block, &known)
                .and_then(|calls| calls.into_iter().next())
                .ok_or_else(|| anyhow!("unknown tool or arguments. Try `shortlist-cli tools`."))?;
            let embedder = Embedder::load().ok();
            let mut ctx =
                ToolContext::new(&store, embedder.as_ref(), Budget::new(20_000)).with_writes();
            let result = registry.run(&mut ctx, &call)?;
            println!("{}", result.body);
            eprintln!(
                "\n~{} tokens · {} chars shown · {} chars raw ({:.1}% less)",
                estimate_tokens(&result.body),
                result.body.len(),
                result.raw_chars,
                if result.raw_chars == 0 {
                    0.0
                } else {
                    100.0 * (result.raw_chars.saturating_sub(result.body.len())) as f32
                        / result.raw_chars as f32
                }
            );
        }
        "tokens" => {
            let question = args[1..].join(" ");
            if question.trim().is_empty() {
                return Err(anyhow!("usage: shortlist-cli tokens <question>"));
            }
            let embedder = Embedder::load().ok();
            let report = shortlist_pipeline::token_report(&store, embedder.as_ref(), &question)?;
            println!("question: {question}\n");
            println!(
                "old excerpt prompt : ~{} tokens ({} chars, {} chars of excerpts)",
                report.old_tokens, report.old_chars, report.context_chars
            );
            if report.direct {
                println!(
                    "local answer       : 0 AI tokens ({} chars of tool output)",
                    report.tool_chars
                );
                println!("harness total      : 0 tokens");
            } else {
                println!(
                    "harness prompt     : ~{} tokens (+ ~{} for tool results)",
                    report.prompt_tokens, report.tool_tokens
                );
                println!(
                    "harness total      : ~{} tokens",
                    report.prompt_tokens + report.tool_tokens
                );
            }
            println!(
                "whole library body : {} chars ≈ {} tokens",
                report.raw_chars,
                report.raw_chars.div_ceil(4)
            );
            let new_total = report.prompt_tokens + report.tool_tokens;
            if report.old_tokens > 0 {
                println!(
                    "\nthis question      : {:.0}% of the old prompt",
                    100.0 * new_total as f32 / report.old_tokens as f32
                );
            }
        }
        "ask" => {
            let plain = args.iter().any(|arg| arg == "--plain");
            let question = args[1..]
                .iter()
                .filter(|arg| arg.as_str() != "--plain")
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            if question.trim().is_empty() {
                return Err(anyhow!("usage: shortlist-cli ask [--plain] <question>"));
            }
            if store.count()? == 0 {
                return Err(anyhow!("no candidates yet; run `shortlist-cli seed` first"));
            }
            let embedder = Embedder::load().ok();
            let model = default_model();
            if plain {
                let context =
                    shortlist_pipeline::build_context(&store, embedder.as_ref(), &question, 4);
                eprintln!("--- retrieved context ---\n{context}\n-------------------------");
                let answer =
                    shortlist_pipeline::ask(&store, embedder.as_ref(), Some(&model), &question)?;
                println!("{answer}");
            } else {
                let report = shortlist_pipeline::ask_with_tools(
                    &store,
                    embedder.as_ref(),
                    Some(&model),
                    &question,
                    &[],
                )?;
                println!("{}", report.answer);
                eprintln!(
                    "\n{} tool call(s) · {} round(s) · ~{} prompt + {} reply tokens",
                    report.calls, report.rounds, report.prompt_tokens, report.reply_tokens
                );
                eprintln!(
                    "tool payload: {} chars vs {} raw ({:.1}% less, ~{} tokens saved)",
                    report.tool_chars,
                    report.raw_chars,
                    report.savings_pct(),
                    report.saved_tokens()
                );
                if report.clips > 0 {
                    eprintln!("{} result(s) hit the budget cap", report.clips);
                }
            }
        }
        "stream" => {
            let question = args[1..].join(" ");
            if question.trim().is_empty() {
                return Err(anyhow!("usage: shortlist-cli stream <question>"));
            }
            let model = default_model();
            let opencode = shortlist_ai::OpenCode;
            let session = opencode.create_session("Shortlist stream", Some(&model))?;
            let state =
                std::sync::Arc::new(std::sync::Mutex::new(shortlist_ai::StreamState::default()));
            let watcher = state.clone();
            let printer = std::thread::spawn(move || {
                use std::io::Write;
                let mut printed = 0usize;
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(80));
                    let snapshot = watcher.lock().unwrap().clone();
                    if snapshot.text.len() > printed {
                        print!("{}", &snapshot.text[printed..]);
                        let _ = std::io::stdout().flush();
                        printed = snapshot.text.len();
                    }
                    if snapshot.done {
                        break;
                    }
                }
            });
            let answer = opencode.stream_prompt(&session, &question, state)?;
            let _ = printer.join();
            println!("\n---\n{answer}");
            let _ = opencode.delete_session(&session);
        }
        _ => {
            println!(
                "Shortlist CLI\n\n  seed   add demo candidates\n  import <file>...  parse and add resumes\n  index  build the e5-small-v2 index\n  reindex  rebuild chunks and embeddings with profile fields and notes\n  tools  list the model's built-in tools\n  mcp install|status  register or inspect the local MCP server\n  tool <name> [k=v]...  call one tool without a model\n  db [table]  inspect SQLite tables, UUIDs, and embedding blobs\n  tokens <question>  compare old vs harness prompt sizes\n  ask [--plain] <question>  answer with tools (default) or excerpts\n  stream <question>  raw streamed answer from the model"
            );
        }
    }
    Ok(())
}
