use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use shortlist_ai::OpenCode;
use shortlist_core::{Candidate, ModelRef, now_ms};
use shortlist_embed::{Embedder, cosine};
use shortlist_harness::{Config, Harness, OpenCodeModel, RunOptions, RunReport};
use shortlist_store::Store;

/// Reciprocal-rank fusion of BM25 and vector retrieval. At this scale this is
/// both faster and more reliable than tuning a weighted sum.
fn rrf(rank: usize) -> f32 {
    1.0 / (60.0 + rank as f32 + 1.0)
}

/// Append the recruiter's note for a candidate, if one exists.
fn push_note(store: &Store, candidate_id: &str, out: &mut String) {
    if let Ok(Some(note)) = store.note(candidate_id) {
        if !note.trim().is_empty() {
            out.push_str(&format!(
                "\nRecruiter notes:\n{}\n",
                note.chars().take(1200).collect::<String>()
            ));
        }
    }
}

/// Build the grounding block sent to the model. Uses hybrid retrieval when
/// embeddings are available and BM25 alone when they are not.
pub fn build_context(store: &Store, embedder: Option<&Embedder>, query: &str, k: usize) -> String {
    let mut vector: Vec<(f32, String, String)> = Vec::new();
    if let Some(emb) = embedder {
        if let Ok(qv) = emb.embed_query(query) {
            if let Ok(chunks) = store.embedded_chunks() {
                let mut scored: Vec<(f32, String, String)> = chunks
                    .into_iter()
                    .map(|(cid, text, _model, vec)| (cosine(&qv, &vec), cid, text))
                    .collect();
                scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                let mut seen = HashSet::new();
                for (score, cid, text) in scored {
                    if seen.insert(cid.clone()) {
                        vector.push((score, cid, text));
                    }
                    if vector.len() >= 40 {
                        break;
                    }
                }
            }
        }
    }

    let lexical: Vec<Candidate> = store.fts_candidates(query, 40).unwrap_or_default();

    let mut fused: HashMap<String, f32> = HashMap::new();
    for (rank, (_, cid, _)) in vector.iter().enumerate() {
        *fused.entry(cid.clone()).or_default() += rrf(rank);
    }
    for (rank, candidate) in lexical.iter().enumerate() {
        *fused.entry(candidate.id.clone()).or_default() += rrf(rank);
    }

    let mut ranked: Vec<(String, f32)> = fused.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(k);

    let vectors: HashMap<String, (f32, String)> = vector
        .into_iter()
        .map(|(score, cid, text)| (cid, (score, text)))
        .collect();
    let lexical: HashMap<String, Candidate> =
        lexical.into_iter().map(|c| (c.id.clone(), c)).collect();

    let mut out = String::new();
    for (cid, fused_score) in ranked {
        if let Some((cosine, text)) = vectors.get(&cid) {
            let name = store
                .candidate(&cid)
                .ok()
                .flatten()
                .map(|c| format!("{} — {}", c.name, c.headline))
                .unwrap_or_else(|| cid.clone());
            out.push_str(&format!(
                "\n[{name}] (fusion {:.3}, cosine {:.2})\n{}\n",
                fused_score,
                cosine,
                text.chars().take(700).collect::<String>()
            ));
            push_note(store, &cid, &mut out);
        } else if let Some(candidate) = lexical.get(&cid) {
            out.push_str(&format!(
                "\n[{} — {}] (fusion {:.3})\n{}\n",
                candidate.name,
                candidate.headline,
                fused_score,
                candidate.summary.chars().take(700).collect::<String>()
            ));
            push_note(store, &cid, &mut out);
        }
    }
    out
}

pub fn build_prompt(context: &str, question: &str) -> String {
    if context.trim().is_empty() {
        return format!(
            "You are Shortlist, a recruiting assistant. The local candidate index has no matching candidates for this question. \
             Say that plainly, then answer generally if you can. Do not invent candidates.\n\nQuestion: {question}"
        );
    }
    format!(
        "You are Shortlist, a recruiting assistant working with a private candidate database. \
         Answer the recruiter's question using only the candidate excerpts below. Cite candidate names. \
         The excerpts may include the recruiter's own notes: treat them as first-hand context and cite \
         the candidate by name when you use them. If the excerpts do not contain the answer, say so \
         instead of guessing. Be concise and practical.\n\n\
         --- CANDIDATE EXCERPTS ---{context}\n--- END EXCERPTS ---\n\nQuestion: {question}"
    )
}

/// Full headless path: retrieve grounding, ask OpenCode, return the answer.
pub fn ask(
    store: &Store,
    embedder: Option<&Embedder>,
    model: Option<&ModelRef>,
    question: &str,
) -> Result<String> {
    let context = build_context(store, embedder, question, 4);
    let prompt = build_prompt(&context, question);
    let opencode = OpenCode;
    let session = opencode.create_session("Shortlist CLI", model)?;
    let after = now_ms();
    opencode.prompt(&session, &prompt)?;
    let result = opencode.wait_for_reply(&session, after, Duration::from_secs(180));
    let _ = opencode.delete_session(&session);
    result
}

/// The token-lean path: the model calls local tools instead of receiving
/// resume excerpts. One fresh OpenCode session per model call keeps the context
/// bounded no matter how long the chat gets.
pub fn ask_with_tools(
    store: &Store,
    embedder: Option<&Embedder>,
    model: Option<&ModelRef>,
    question: &str,
    history: &[(String, String)],
) -> Result<RunReport> {
    ask_with_tools_report(store, embedder, model, question, history, None)
}

/// As [`ask_with_tools`], with an optional progress feed for the UI.
pub fn ask_with_tools_report(
    store: &Store,
    embedder: Option<&Embedder>,
    model: Option<&ModelRef>,
    question: &str,
    history: &[(String, String)],
    progress: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<RunReport> {
    let harness = Harness::new(store, embedder, Config::default());
    harness.run(
        &OpenCodeModel::new(model),
        question,
        RunOptions { history, progress },
    )
}

/// A compact, model-free summary of what the harness would save on this
/// question. Used by `shortlist-cli tokens`.
pub fn token_report(
    store: &Store,
    embedder: Option<&Embedder>,
    question: &str,
) -> Result<TokenReport> {
    use shortlist_harness::{Budget, Call, Registry, ToolContext, estimate_tokens};

    let harness = Harness::new(store, embedder, Config::default());
    let mut ctx = ToolContext::new(store, embedder, Budget::new(20_000));
    let registry = Registry::builtin();
    let calls = harness.prefetch(question)?;
    let mut tool_text = String::new();
    let mut has_shortlist = false;
    for call in &calls {
        has_shortlist |= call.tool == "shortlist";
        if let Ok(result) = registry.run(&mut ctx, call) {
            tool_text.push_str(&result.body);
            tool_text.push('\n');
        }
    }
    // Ranking questions also carry a research pack; report it honestly.
    if !harness.prefers_local_answer(question) && has_shortlist {
        let handles: Vec<String> = ctx
            .refs
            .entries()
            .iter()
            .take(3)
            .map(|entry| entry.handle.clone())
            .collect();
        for handle in handles {
            let call = Call {
                tool: "get".into(),
                args: vec![
                    ("id".into(), handle),
                    (
                        "f".into(),
                        "headline,yrs,loc,skills,summary,experience,education".into(),
                    ),
                ],
            };
            if let Ok(result) = registry.run(&mut ctx, &call) {
                tool_text.push_str(&result.body);
                tool_text.push('\n');
            }
        }
    }
    let old_context = build_context(store, embedder, question, 4);
    let old_prompt = build_prompt(&old_context, question);
    let direct = harness.local_answer(question)?.is_some();
    let prompt_tokens = if direct {
        0
    } else {
        let mut tokens = estimate_tokens(harness.system()) + estimate_tokens(question);
        if tool_text.is_empty() {
            tokens += estimate_tokens(harness.digest());
        }
        tokens
    };
    Ok(TokenReport {
        old_tokens: estimate_tokens(&old_prompt),
        old_chars: old_prompt.len(),
        context_chars: old_context.len(),
        prompt_tokens,
        tool_tokens: if direct {
            0
        } else {
            estimate_tokens(&tool_text)
        },
        tool_chars: tool_text.len(),
        raw_chars: store.total_text_chars()?,
        direct,
    })
}

#[derive(Clone, Debug)]
pub struct TokenReport {
    /// Tokens the old excerpt-stuffing prompt would send.
    pub old_tokens: usize,
    pub old_chars: usize,
    pub context_chars: usize,
    /// Tokens the harness system prompt and digest cost.
    pub prompt_tokens: usize,
    /// Tokens representative tool results cost.
    pub tool_tokens: usize,
    pub tool_chars: usize,
    /// Characters of resume text the library holds.
    pub raw_chars: usize,
    /// Direct local rendering (opt-in) needs no model tokens at all.
    pub direct: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_includes_recruiter_notes() {
        let store = Store::open_in_memory().unwrap();
        store.seed_demo().unwrap();
        store
            .save_note("demo-01", "zebracorn wants a principal role")
            .unwrap();
        let context = build_context(&store, None, "zebracorn", 5);
        assert!(context.contains("Sarah Chen"));
        assert!(context.contains("zebracorn wants a principal role"));
    }
}
