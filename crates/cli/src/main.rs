use anyhow::{Result, anyhow};
use shortlist_core::ModelRef;
use shortlist_embed::{Embedder, MODEL_ID};
use shortlist_store::Store;

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
                let chunks = shortlist_core::chunk_text(&candidate.text, 180, 28);
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
        "ask" => {
            let question = args[1..].join(" ");
            if question.trim().is_empty() {
                return Err(anyhow!("usage: shortlist-cli ask <question>"));
            }
            if store.count()? == 0 {
                return Err(anyhow!("no candidates yet; run `shortlist-cli seed` first"));
            }
            let embedder = Embedder::load().ok();
            let model = ModelRef {
                id: "deepseek-v4.1-flash".into(),
                provider_id: "opencode-go".into(),
                name: "DeepSeek V4.1 Flash".into(),
            };
            let context =
                shortlist_pipeline::build_context(&store, embedder.as_ref(), &question, 4);
            eprintln!("--- retrieved context ---\n{context}\n-------------------------");
            let answer =
                shortlist_pipeline::ask(&store, embedder.as_ref(), Some(&model), &question)?;
            println!("{answer}");
        }
        "stream" => {
            let question = args[1..].join(" ");
            if question.trim().is_empty() {
                return Err(anyhow!("usage: shortlist-cli stream <question>"));
            }
            let model = ModelRef {
                id: "deepseek-v4.1-flash".into(),
                provider_id: "opencode-go".into(),
                name: "DeepSeek V4.1 Flash".into(),
            };
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
                "Shortlist CLI\n\n  seed   add demo candidates\n  import <file>...  parse and add resumes\n  index  build the e5-small-v2 index\n  ask <question>  run retrieval + OpenCode"
            );
        }
    }
    Ok(())
}
