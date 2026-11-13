use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Experience {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub company: String,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub end: String,
    #[serde(default, deserialize_with = "deserialize_f32")]
    pub years: f32,
    #[serde(default)]
    pub description: String,
}

impl Experience {
    pub fn is_current(&self) -> bool {
        let end = self.end.trim().to_lowercase();
        end.is_empty()
            || end.contains("present")
            || end.contains("current")
            || end.contains("now")
            || end.contains("ongoing")
    }

    /// "Senior Backend Engineer at Stripe · 2021 – Present"
    pub fn label(&self) -> String {
        let mut out = String::new();
        if !self.title.is_empty() {
            out.push_str(&self.title);
        }
        if !self.company.is_empty() {
            if !out.is_empty() {
                out.push_str(" at ");
            }
            out.push_str(&self.company);
        }
        let dates = match (self.start.trim(), self.end.trim()) {
            ("", "") => String::new(),
            (start, "") => start.to_string(),
            ("", end) => end.to_string(),
            (start, end) => format!("{start} – {end}"),
        };
        if !dates.is_empty() {
            if !out.is_empty() {
                out.push_str(" · ");
            }
            out.push_str(&dates);
        }
        out
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Education {
    #[serde(default)]
    pub degree: String,
    #[serde(default)]
    pub institution: String,
    #[serde(default)]
    pub year: String,
}

impl Education {
    pub fn label(&self) -> String {
        match (self.degree.trim(), self.institution.trim()) {
            ("", "") => String::new(),
            (degree, "") => degree.to_string(),
            ("", institution) => institution.to_string(),
            (degree, institution) => format!("{degree} · {institution}"),
        }
    }
}

fn deserialize_f32<'de, D>(deserializer: D) -> std::result::Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::Number(number)) => number.as_f64().unwrap_or(0.0) as f32,
        Some(serde_json::Value::String(value)) => value.trim().parse::<f32>().unwrap_or(0.0),
        _ => 0.0,
    })
}

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
    #[serde(default)]
    pub experience: Vec<Experience>,
    #[serde(default)]
    pub education: Vec<Education>,
    #[serde(default)]
    pub certifications: Vec<String>,
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

    pub fn current_role(&self) -> Option<&Experience> {
        self.experience.iter().find(|role| role.is_current())
    }

    /// Everything in the profile that should be searchable, without the notes.
    pub fn search_body(&self) -> String {
        let mut out = String::new();
        for value in [
            self.headline.as_str(),
            self.location.as_str(),
            &self.skills.join(", "),
            self.summary.as_str(),
        ] {
            if !value.trim().is_empty() {
                out.push_str(value);
                out.push('\n');
            }
        }
        for role in &self.experience {
            let label = role.label();
            if !label.is_empty() {
                out.push_str(&label);
                out.push('\n');
            }
            if !role.description.trim().is_empty() {
                out.push_str(&role.description);
                out.push('\n');
            }
        }
        for education in &self.education {
            let label = education.label();
            if !label.is_empty() {
                out.push_str(&label);
                out.push('\n');
            }
        }
        if !self.certifications.is_empty() {
            out.push_str(&self.certifications.join(", "));
            out.push('\n');
        }
        out.push_str(&self.text);
        out
    }

    /// The text that is chunked and embedded. Profile fields first, so a
    /// re-read changes the vectors even when the resume body is unchanged.
    pub fn index_text(&self) -> String {
        self.search_body()
    }

    /// Embeddings should follow the recruiter note too; it is first-hand
    /// context and the chat can cite it.
    pub fn index_text_with_note(&self, note: Option<&str>) -> String {
        let mut text = self.index_text();
        if let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) {
            text.push_str("\nRecruiter notes:\n");
            text.push_str(note);
        }
        text
    }

    pub fn seniority(&self) -> f32 {
        seniority_score(&self.headline).max(
            self.experience
                .iter()
                .map(|role| seniority_score(&role.title))
                .fold(0.0, f32::max),
        )
    }

    pub fn recency(&self) -> f32 {
        recency_score(&self.experience)
    }

    pub fn is_leader(&self) -> bool {
        leadership_title(&self.headline)
            || self
                .experience
                .iter()
                .any(|role| leadership_title(&role.title))
    }
}

/// 0–1 seniority from a job title. Deliberately conservative and role-based;
/// it never looks at identity fields.
pub fn seniority_score(title: &str) -> f32 {
    let title = title.to_lowercase();
    let markers: &[(f32, &[&str])] = &[
        (
            1.0,
            &[
                "principal",
                "staff",
                "distinguished",
                "architect",
                "chief",
                "vp",
                "vice president",
                "head of",
                "director",
            ],
        ),
        (0.85, &["manager", "lead", "supervisor"]),
        (0.7, &["senior", "sr."]),
        (
            0.5,
            &[
                "engineer",
                "developer",
                "designer",
                "analyst",
                "scientist",
                "consultant",
                "specialist",
            ],
        ),
        (0.2, &["junior", "associate", "intern", "trainee", "entry"]),
    ];
    markers
        .iter()
        .find(|(_, words)| words.iter().any(|word| title.contains(word)))
        .map(|(score, _)| *score)
        .unwrap_or(0.45)
}

pub fn leadership_title(title: &str) -> bool {
    let title = title.to_lowercase();
    [
        "manager",
        "lead",
        "director",
        "head of",
        "vp",
        "chief",
        "supervisor",
    ]
    .iter()
    .any(|word| title.contains(word))
}

/// How current the candidate's career is, 0–1. A current role scores 1.0.
pub fn recency_score(experience: &[Experience]) -> f32 {
    if experience.is_empty() {
        return 0.5;
    }
    if experience.iter().any(Experience::is_current) {
        return 1.0;
    }
    let now = current_year();
    let latest = experience
        .iter()
        .filter_map(|role| last_year(&role.end).or_else(|| last_year(&role.start)))
        .max();
    match latest {
        Some(year) => (1.0 - (now - year).max(0) as f32 * 0.12).clamp(0.1, 1.0),
        None => 0.5,
    }
}

/// Compact role history for prompts and the detail drawer.
pub fn experience_summary(experience: &[Experience], max_roles: usize, max_chars: usize) -> String {
    experience
        .iter()
        .take(max_roles)
        .map(|role| {
            let mut text = role.label();
            if !role.description.trim().is_empty() {
                if !text.is_empty() {
                    text.push_str(": ");
                }
                text.push_str(role.description.trim());
            }
            if text.chars().count() > max_chars {
                text = text.chars().take(max_chars).collect::<String>();
                text.push('…');
            }
            text
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Share of the requested terms that appear in the candidate's roles.
pub fn career_relevance(experience: &[Experience], terms: &[String]) -> f32 {
    let terms: Vec<String> = terms
        .iter()
        .map(|term| term.trim().to_lowercase())
        .filter(|term| term.len() >= 3)
        .collect();
    if terms.is_empty() || experience.is_empty() {
        return 0.0;
    }
    let corpus = experience
        .iter()
        .map(|role| {
            format!(
                "{} {} {} {}",
                role.title, role.company, role.description, role.start
            )
            .to_lowercase()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let hits = terms
        .iter()
        .filter(|term| {
            corpus.contains(term.as_str())
                || (term.ends_with('s')
                    && term.len() > 4
                    && corpus.contains(&term[..term.len() - 1]))
        })
        .count();
    hits as f32 / terms.len() as f32
}

fn last_year(value: &str) -> Option<i32> {
    value
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| part.len() == 4)
        .filter_map(|part| part.parse::<i32>().ok())
        .max()
}

fn current_year() -> i32 {
    1970 + (now_ms() / 31_556_952_000) as i32
}

/// The metadata half of a candidate. Everything except the resume text, so a
/// whole library can be ranked and summarised without ever paying for the
/// bodies. The tool harness works exclusively in this shape.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CandidateLite {
    pub id: String,
    pub name: String,
    pub headline: String,
    pub location: String,
    pub email: String,
    pub phone: String,
    pub years: f32,
    pub skills: Vec<String>,
    pub summary: String,
    #[serde(default)]
    pub experience: Vec<Experience>,
    #[serde(default)]
    pub education: Vec<Education>,
    #[serde(default)]
    pub certifications: Vec<String>,
    pub file_name: String,
    pub created_at: i64,
    pub is_demo: bool,
}

impl CandidateLite {
    pub fn current_role(&self) -> Option<&Experience> {
        self.experience.iter().find(|role| role.is_current())
    }

    pub fn seniority(&self) -> f32 {
        seniority_score(&self.headline).max(
            self.experience
                .iter()
                .map(|role| seniority_score(&role.title))
                .fold(0.0, f32::max),
        )
    }

    pub fn recency(&self) -> f32 {
        recency_score(&self.experience)
    }

    pub fn is_leader(&self) -> bool {
        leadership_title(&self.headline)
            || self
                .experience
                .iter()
                .any(|role| leadership_title(&role.title))
    }
}

impl From<&Candidate> for CandidateLite {
    fn from(c: &Candidate) -> Self {
        Self {
            id: c.id.clone(),
            name: c.name.clone(),
            headline: c.headline.clone(),
            location: c.location.clone(),
            email: c.email.clone(),
            phone: c.phone.clone(),
            years: c.years,
            skills: c.skills.clone(),
            summary: c.summary.clone(),
            experience: c.experience.clone(),
            education: c.education.clone(),
            certifications: c.certifications.clone(),
            file_name: c.file_name.clone(),
            created_at: c.created_at,
            is_demo: c.is_demo,
        }
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
    if let Ok(dir) = std::env::var("SHORTLIST_DATA_DIR")
        && !dir.trim().is_empty()
    {
        return PathBuf::from(dir);
    }
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
            experience: Vec::new(),
            education: Vec::new(),
            certifications: Vec::new(),
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

    #[test]
    fn seniority_and_recency_are_bounded() {
        assert!(seniority_score("Principal Engineer") > seniority_score("Software Engineer"));
        assert!(seniority_score("Engineering Manager") > seniority_score("Intern"));
        assert_eq!(seniority_score("Something Else"), 0.45);

        let current = vec![Experience {
            title: "Staff Engineer".into(),
            start: "2021".into(),
            end: "Present".into(),
            ..Default::default()
        }];
        assert_eq!(recency_score(&current), 1.0);
        let old = vec![Experience {
            title: "Engineer".into(),
            start: "2014".into(),
            end: "2018".into(),
            ..Default::default()
        }];
        assert!(recency_score(&old) < 0.5);
    }

    #[test]
    fn experience_summary_and_index_include_roles() {
        let mut person = candidate("Sarah Chen", "c1");
        person.experience = vec![Experience {
            title: "Staff Engineer".into(),
            company: "Ledger Co".into(),
            start: "2021".into(),
            end: "Present".into(),
            years: 5.0,
            description: "Built payment rails".into(),
        }];
        person.education = vec![Education {
            degree: "B.Tech".into(),
            institution: "University of London".into(),
            year: "2014".into(),
        }];
        person.certifications = vec!["AWS Solutions Architect".into()];
        assert!(person.index_text().contains("Ledger Co"));
        assert!(person.index_text().contains("University of London"));
        assert!(person.index_text().contains("AWS Solutions Architect"));
        assert_eq!(
            experience_summary(&person.experience, 3, 120),
            "Staff Engineer at Ledger Co · 2021 – Present: Built payment rails"
        );
        assert_eq!(
            career_relevance(&person.experience, &["payments".into()]),
            1.0
        );
    }
}
