//! Local retrieval and scoring.
//!
//! The harness never sends a resume body to the model to rank it. It loads
//! metadata once, fuses BM25 with the local vectors, applies structured
//! criteria in Rust, and emits only the compact winners.

use anyhow::Result;
use shortlist_core::CandidateLite;
use shortlist_embed::{Embedder, cosine};
use shortlist_store::Store;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use crate::protocol::Call;

pub struct Library {
    pub rows: Vec<CandidateLite>,
    by_id: HashMap<String, usize>,
    vectors: OnceCell<Vec<VecEntry>>,
}

struct VecEntry {
    row: usize,
    text: String,
    vector: Vec<f32>,
}

/// A fused retrieval hit, with the best chunk as its evidence.
#[derive(Clone, Debug)]
pub struct Ranked {
    pub row: usize,
    pub relevance: f32,
    pub snippet: String,
}

impl Library {
    pub fn load(store: &Store) -> Result<Self> {
        let rows = store.list_lite(100_000, 0)?;
        let by_id: HashMap<String, usize> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id.clone(), index))
            .collect();
        Ok(Self {
            rows,
            by_id,
            vectors: OnceCell::new(),
        })
    }

    /// Vector chunks load on the first semantic query, not for every profile
    /// or stats lookup.
    fn vectors(&self, store: &Store) -> &[VecEntry] {
        self.vectors.get_or_init(|| {
            store
                .embedded_chunks()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(candidate_id, text, _model, vector)| {
                    let row = *self.by_id.get(&candidate_id)?;
                    Some(VecEntry { row, text, vector })
                })
                .collect()
        })
    }

    pub fn get(&self, id: &str) -> Option<&CandidateLite> {
        self.by_id.get(id).map(|index| &self.rows[*index])
    }

    /// Hybrid retrieval: BM25 + local vectors, fused with reciprocal ranks.
    pub fn ranked(
        &self,
        store: &Store,
        embedder: Option<&Embedder>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Ranked>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(self
                .rows
                .iter()
                .enumerate()
                .take(limit)
                .map(|(index, candidate)| Ranked {
                    row: *self.by_id.get(&candidate.id).unwrap_or(&index),
                    relevance: 0.0,
                    snippet: candidate.summary.clone(),
                })
                .collect());
        }

        let lexical = store.search_lite(query, 60)?;
        let vector = self.vector_hits(store, embedder, query, 60);

        let mut fused: HashMap<usize, f32> = HashMap::new();
        for (rank, candidate) in lexical.iter().enumerate() {
            if let Some(row) = self.by_id.get(&candidate.id) {
                *fused.entry(*row).or_default() += rrf(rank);
            }
        }
        for (rank, (row, _score, _text)) in vector.iter().enumerate() {
            *fused.entry(*row).or_default() += rrf(rank);
        }
        let peak = fused
            .values()
            .copied()
            .fold(0.0f32, f32::max)
            .max(f32::EPSILON);

        let snippets: HashMap<usize, String> = vector
            .iter()
            .map(|(row, _score, text)| (*row, text.clone()))
            .collect();

        let mut hits: Vec<Ranked> = fused
            .into_iter()
            .map(|(row, score)| Ranked {
                row,
                relevance: (score / peak).clamp(0.0, 1.0),
                snippet: snippets
                    .get(&row)
                    .cloned()
                    .unwrap_or_else(|| self.rows[row].summary.clone()),
            })
            .collect();
        hits.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hits.truncate(limit);
        Ok(hits)
    }

    fn vector_hits(
        &self,
        store: &Store,
        embedder: Option<&Embedder>,
        query: &str,
        limit: usize,
    ) -> Vec<(usize, f32, String)> {
        let Some(embedder) = embedder else {
            return Vec::new();
        };
        let vectors = self.vectors(store);
        if vectors.is_empty() {
            return Vec::new();
        }
        let Ok(query_vector) = embedder.embed_query(query) else {
            return Vec::new();
        };
        let mut best: HashMap<usize, (f32, String)> = HashMap::new();
        for entry in vectors {
            let score = cosine(&query_vector, &entry.vector);
            let slot = best.entry(entry.row).or_insert((f32::MIN, String::new()));
            if score > slot.0 {
                *slot = (score, entry.text.clone());
            }
        }
        let mut hits: Vec<(usize, f32, String)> = best
            .into_iter()
            .map(|(row, (score, text))| (row, score, text))
            .collect();
        hits.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit);
        hits
    }
}

fn rrf(rank: usize) -> f32 {
    1.0 / (60.0 + rank as f32 + 1.0)
}

/// Structured criteria shared by `find` and `shortlist`.
#[derive(Clone, Debug, Default)]
pub struct Criteria {
    pub query: String,
    pub require: Vec<String>,
    pub any: Vec<String>,
    pub min_years: Option<f32>,
    pub max_years: Option<f32>,
    pub location: Option<String>,
}

impl Criteria {
    pub fn from_call(call: &Call) -> Self {
        Self {
            query: call.arg("q").unwrap_or("").trim().to_string(),
            require: call.list("skills"),
            any: call.list("any"),
            min_years: call.number("miny").filter(|v| *v > 0.0),
            max_years: call.number("maxy").filter(|v| *v > 0.0),
            location: call
                .arg("loc")
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
            && self.require.is_empty()
            && self.any.is_empty()
            && self.min_years.is_none()
            && self.max_years.is_none()
            && self.location.is_none()
    }

    fn passes_hard_filters(&self, row: &CandidateLite) -> bool {
        if let Some(min) = self.min_years
            && row.years + 0.05 < min
        {
            return false;
        }
        if let Some(max) = self.max_years
            && row.years - 0.05 > max
        {
            return false;
        }
        if let Some(location) = &self.location
            && !location_matches(&row.location, location)
        {
            return false;
        }
        true
    }
}

/// How one candidate fits a set of criteria.
#[derive(Clone, Debug)]
pub struct Fit {
    pub require_hits: usize,
    pub require_total: usize,
    pub any_hits: usize,
    pub any_total: usize,
    pub matched: Vec<String>,
    pub missing: Vec<String>,
    pub score: f32,
    /// Role-based signals, all 0–1 and independent of identity fields.
    pub seniority: f32,
    pub recency: f32,
    pub career: f32,
}

pub fn evaluate(row: &CandidateLite, criteria: &Criteria, relevance: f32) -> Option<Fit> {
    if !criteria.passes_hard_filters(row) {
        return None;
    }
    let mut matched = Vec::new();
    let mut missing = Vec::new();
    let mut require_hits = 0;
    for wanted in &criteria.require {
        if row.skills.iter().any(|skill| skill_matches(skill, wanted)) {
            require_hits += 1;
            matched.push(wanted.clone());
        } else {
            missing.push(wanted.clone());
        }
    }
    let any_hits = criteria
        .any
        .iter()
        .filter(|wanted| row.skills.iter().any(|skill| skill_matches(skill, wanted)))
        .count();
    for wanted in &criteria.any {
        if row.skills.iter().any(|skill| skill_matches(skill, wanted))
            && !matched.iter().any(|m| m.eq_ignore_ascii_case(wanted))
        {
            matched.push(wanted.clone());
        }
    }

    let require_ratio = ratio(require_hits, criteria.require.len());
    let any_ratio = ratio(any_hits, criteria.any.len());
    let seniority = row.seniority();
    let recency = row.recency();
    let career = shortlist_core::career_relevance(&row.experience, &career_terms(criteria));

    let mut score = require_ratio * 50.0 + any_ratio * 15.0;
    if let Some(min) = criteria.min_years {
        score += if row.years >= min {
            12.0
        } else {
            12.0 * (row.years / min.max(0.1)).clamp(0.0, 1.0)
        };
    } else if row.years > 0.0 {
        score += 6.0;
    }
    score += relevance.clamp(0.0, 1.0) * 8.0;
    if let Some(location) = &criteria.location
        && location_matches(&row.location, location)
    {
        score += 3.0;
    }
    score += seniority * 6.0;
    score += recency * 3.0;
    score += career * 3.0;

    Some(Fit {
        require_hits,
        require_total: criteria.require.len(),
        any_hits,
        any_total: criteria.any.len(),
        matched,
        missing,
        score: score.clamp(0.0, 100.0),
        seniority,
        recency,
        career,
    })
}

/// Terms from the criteria and the free-text query that a role description
/// can match. This is what lets "payments" credit someone whose resume says
/// "led the ledger team" even when the skill list is generic.
fn career_terms(criteria: &Criteria) -> Vec<String> {
    const SKIP: &[&str] = &[
        "about",
        "after",
        "also",
        "and",
        "are",
        "background",
        "been",
        "best",
        "candidate",
        "could",
        "does",
        "experience",
        "find",
        "for",
        "from",
        "has",
        "have",
        "how",
        "into",
        "most",
        "please",
        "profile",
        "profile",
        "relevant",
        "role",
        "roles",
        "show",
        "strong",
        "strongest",
        "that",
        "the",
        "their",
        "them",
        "they",
        "this",
        "top",
        "was",
        "what",
        "when",
        "where",
        "which",
        "who",
        "with",
        "work",
        "worked",
        "would",
        "years",
        "you",
    ];
    let mut terms = criteria.require.clone();
    terms.extend(criteria.any.clone());
    terms.extend(
        criteria
            .query
            .split(|ch: char| !ch.is_alphanumeric())
            .map(str::to_lowercase)
            .filter(|word| word.len() >= 4 && !SKIP.contains(&word.as_str())),
    );
    terms.sort();
    terms.dedup();
    terms.truncate(10);
    terms
}

fn ratio(hits: usize, total: usize) -> f32 {
    if total == 0 {
        1.0
    } else {
        hits as f32 / total as f32
    }
}

fn location_matches(candidate: &str, wanted: &str) -> bool {
    let candidate = candidate.to_lowercase();
    let wanted = wanted.to_lowercase();
    if candidate.contains(&wanted) {
        return true;
    }
    // "Remote" in the query should match "Remote, Nigeria"; the reverse does too.
    let wanted_tokens: HashSet<&str> = wanted.split_whitespace().collect();
    let candidate_tokens: HashSet<&str> = candidate.split_whitespace().collect();
    wanted_tokens.is_subset(&candidate_tokens)
}

/// Canonical skill form: lowercase alphanumerics plus a small alias table.
pub fn canon_skill(skill: &str) -> String {
    match skill.trim().to_lowercase().as_str() {
        "c++" | "cpp" => return "cplusplus".into(),
        "c#" | "csharp" => return "csharp".into(),
        "c" => return "clang".into(),
        ".net" | "dotnet" => return "dotnet".into(),
        _ => {}
    }
    let base: String = skill
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    match base.as_str() {
        "k8s" => "kubernetes".into(),
        "postgres" | "psql" | "postgresdb" => "postgresql".into(),
        "js" => "javascript".into(),
        "ts" => "typescript".into(),
        "node" | "nodejs" => "nodejs".into(),
        "reactjs" => "react".into(),
        "nextjs" => "nextjs".into(),
        "golang" => "go".into(),
        "ml" => "machinelearning".into(),
        "googlecloud" | "googlecloudplatform" => "gcp".into(),
        "amazonwebservices" => "aws".into(),
        other => other.to_string(),
    }
}

pub fn skill_matches(candidate_skill: &str, wanted: &str) -> bool {
    let candidate = canon_skill(candidate_skill);
    let wanted = canon_skill(wanted);
    !candidate.is_empty() && candidate == wanted
}

/// Join skills in a stable, compact form.
pub fn compact_list(items: &[String], max: usize) -> String {
    let mut out = items
        .iter()
        .take(max)
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    if items.len() > max {
        out.push_str(&format!("+{}", items.len() - max));
    }
    out
}

/// Collapse whitespace and clip on a word boundary.
pub fn snippet(text: &str, max_chars: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let clipped: String = collapsed.chars().take(max_chars).collect();
    let cut = clipped.rfind(' ').unwrap_or(clipped.len());
    format!("{}…", clipped[..cut].trim_end())
}

pub fn years(value: f32) -> String {
    if (value - value.round()).abs() < 0.05 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalises_common_aliases() {
        assert!(skill_matches("PostgreSQL", "postgres"));
        assert!(skill_matches("Kubernetes", "k8s"));
        assert!(skill_matches("Node.js", "node"));
        assert!(skill_matches("Go", "golang"));
        assert!(skill_matches("AWS", "Amazon Web Services"));
    }

    #[test]
    fn does_not_confuse_java_and_javascript() {
        assert!(!skill_matches("JavaScript", "java"));
        assert!(skill_matches("Java", "java"));
    }

    #[test]
    fn snippet_stays_short() {
        let text = "built   the ledger service\n\nand cut latency";
        assert_eq!(
            snippet(text, 100),
            "built the ledger service and cut latency"
        );
        assert!(snippet(&"word ".repeat(100), 20).chars().count() <= 21);
    }
}
