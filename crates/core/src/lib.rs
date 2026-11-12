use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    pub headline: String,
    pub location: String,
    pub email: String,
    pub phone: String,
    pub years: f32,
    pub skills: Vec<String>,
    pub summary: String,
    pub text: String,
    pub file_name: String,
    pub created_at: i64,
    pub is_demo: bool,
}

impl Candidate {
    pub fn initials(&self) -> String {
        let mut out = String::new();
        for part in self.name.split_whitespace().take(2) {
            if let Some(ch) = part.chars().next() {
                out.extend(ch.to_uppercase());
            }
        }
        if out.is_empty() { "?".into() } else { out }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRef {
    pub id: String,
    pub provider_id: String,
    pub name: String,
}

impl ModelRef {
    pub fn key(&self) -> String {
        format!("{}/{}", self.provider_id, self.id)
    }
}

#[derive(Clone, Debug)]
pub struct ChatSession {
    pub id: String,
    pub opencode_id: String,
    pub title: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub text: String,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

pub fn data_dir() -> PathBuf {
    if let Some(base) = directories::BaseDirs::new() {
        base.data_dir().join("Shortlist")
    } else {
        PathBuf::from(".").join(".shortlist")
    }
}

pub fn db_path() -> PathBuf {
    data_dir().join("shortlist.db")
}

pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn human_relative(ts: i64) -> String {
    let delta = (now_ms() - ts).max(0) / 1000;
    match delta {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", delta / 60),
        3600..=86399 => format!("{}h ago", delta / 3600),
        _ => format!("{}d ago", delta / 86400),
    }
}

/// Split text into overlapping word windows, skipping tiny fragments.
pub fn chunk_text(text: &str, max_words: usize, overlap: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let max_words = max_words.max(24);
    let overlap = overlap.min(max_words / 2);
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < words.len() {
        let end = (start + max_words).min(words.len());
        let chunk = words[start..end].join(" ");
        if chunk.chars().count() > 40 {
            out.push(chunk);
        }
        if end == words.len() {
            break;
        }
        start = (end - overlap).max(start + 1);
    }
    out
}

/// Rewrite full candidate names into inline markdown links that the chat view
/// intercepts. Longest names win, so one name cannot corrupt another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateSpan {
    pub start: usize,
    pub end: usize,
    pub name: String,
    pub id: String,
}

/// Byte ranges of full candidate names inside a line, longest name first so a
/// short name can never split a longer one.
/// Byte ranges of full candidate names inside a line. Matching is
/// case-insensitive and tolerant of repeated whitespace; the longest name wins
/// so a short name can never split a longer one.
pub fn candidate_spans(candidates: &[Candidate], text: &str) -> Vec<CandidateSpan> {
    let mut names: Vec<&str> = candidates
        .iter()
        .map(|c| c.name.as_str())
        .filter(|name| !name.trim().is_empty())
        .collect();
    if names.is_empty() {
        return Vec::new();
    }
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let pattern = names
        .iter()
        .map(|name| {
            name.split_whitespace()
                .map(regex::escape)
                .collect::<Vec<_>>()
                .join(r"\s+")
        })
        .collect::<Vec<_>>()
        .join("|");
    let Ok(re) = regex::RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
    else {
        return Vec::new();
    };
    re.find_iter(text)
        .filter_map(|m| {
            let matched = m.as_str();
            candidates
                .iter()
                .find(|c| names_equal(&c.name, matched))
                .map(|candidate| CandidateSpan {
                    start: m.start(),
                    end: m.end(),
                    name: matched.to_string(),
                    id: candidate.id.clone(),
                })
        })
        .collect()
}

fn names_equal(a: &str, b: &str) -> bool {
    let normalize = |value: &str| {
        value
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    normalize(a) == normalize(b)
}

pub fn linkify_candidates(candidates: &[Candidate], text: &str) -> String {
    let spans = candidate_spans(candidates, text);
    if spans.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + spans.len() * 48);
    let mut last = 0;
    for span in spans {
        out.push_str(&text[last..span.start]);
        out.push_str(&format!(
            "[{}](https://shortlist.local/candidate/{})",
            span.name, span.id
        ));
        last = span.end;
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: &str, id: &str) -> Candidate {
        Candidate {
            id: id.into(),
            name: name.into(),
            headline: String::new(),
            location: String::new(),
            email: String::new(),
            phone: String::new(),
            years: 0.0,
            skills: Vec::new(),
            summary: String::new(),
            text: String::new(),
            file_name: String::new(),
            created_at: 0,
            is_demo: true,
        }
    }

    #[test]
    fn matches_names_case_insensitively() {
        let candidates = vec![candidate("ALOK MISHRA", "c4")];
        let spans = candidate_spans(&candidates, "Runner-up: Alok  Mishra — strong.");
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].id, "c4");
    }

    #[test]
    fn links_candidate_names_inline() {
        let candidates = vec![
            candidate("Sarah Chen", "c1"),
            candidate("Jane Doe", "c2"),
            candidate("Sarah", "c3"),
        ];
        let linked = linkify_candidates(
            &candidates,
            "**Sarah Chen** is strong. Jane Doe also fits; Sarah agrees.",
        );
        assert!(
            linked.contains("[Sarah Chen](https://shortlist.local/candidate/c1)"),
            "{linked}"
        );
        assert!(
            linked.contains("[Jane Doe](https://shortlist.local/candidate/c2)"),
            "{linked}"
        );
        assert!(
            linked.contains("[Sarah](https://shortlist.local/candidate/c3) agrees"),
            "{linked}"
        );
        assert!(!linked.contains("↗"));
    }
}
