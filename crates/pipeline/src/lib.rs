use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use shortlist_ai::OpenCode;
use shortlist_core::{Candidate, ModelRef, now_ms};
use shortlist_embed::{Embedder, cosine};
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
