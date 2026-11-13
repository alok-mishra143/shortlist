//! Short, stable handles for candidates.
//!
//! Candidate ids are UUIDs. Repeating them burns context on every row, so a
//! run replaces each id with `a1`, `a2`, … and resolves the handles itself
//! before touching the store. The model only ever sees the short form.

use shortlist_core::CandidateLite;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefEntry {
    pub handle: String,
    pub candidate_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct Refs {
    entries: Vec<RefEntry>,
    by_id: HashMap<String, usize>,
}

impl Refs {
    /// Return the handle for this candidate, allocating one on first sight.
    pub fn handle(&mut self, candidate: &CandidateLite) -> String {
        if let Some(index) = self.by_id.get(&candidate.id) {
            return self.entries[*index].handle.clone();
        }
        let handle = format!("a{}", self.entries.len() + 1);
        self.by_id.insert(candidate.id.clone(), self.entries.len());
        self.entries.push(RefEntry {
            handle: handle.clone(),
            candidate_id: candidate.id.clone(),
            name: candidate.name.clone(),
        });
        handle
    }

    /// Accept `a1`, `@a1`, `[a1]` or `A1` and return the candidate id.
    pub fn resolve(&self, raw: &str) -> Option<&str> {
        let cleaned = raw
            .trim()
            .trim_start_matches(['@', '['])
            .trim_end_matches(']');
        let index = cleaned
            .strip_prefix(['a', 'A'])
            .and_then(|n| n.parse::<usize>().ok())?;
        self.entries
            .get(index.checked_sub(1)?)
            .map(|entry| entry.candidate_id.as_str())
    }

    pub fn name(&self, handle: &str) -> Option<&str> {
        let cleaned = handle
            .trim()
            .trim_start_matches(['@', '['])
            .trim_end_matches(']');
        self.entries
            .iter()
            .find(|entry| entry.handle == cleaned)
            .map(|entry| entry.name.as_str())
    }

    pub fn entries(&self) -> &[RefEntry] {
        &self.entries
    }

    /// Expand `[a1]` and `[a1, a2]` citations into candidate names, so the
    /// stored answer reads naturally and the UI can still attach chips.
    pub fn render(&self, answer: &str) -> String {
        let mut out = String::with_capacity(answer.len());
        let mut rest = answer;
        while let Some(start) = rest.find('[') {
            let Some(relative_end) = rest[start..].find(']') else {
                break;
            };
            let end = start + relative_end;
            let inside = &rest[start + 1..end];
            let handles: Vec<&str> = inside.split(',').map(str::trim).collect();
            let names: Vec<&str> = handles.iter().filter_map(|h| self.name(h)).collect();
            if names.is_empty() || names.len() != handles.len() {
                out.push_str(&rest[..=end]);
            } else {
                out.push_str(&rest[..start]);
                // If the model already wrote the name earlier on this line,
                // the citation would duplicate it. Keep only names that have
                // not appeared yet; if all have, drop the citation.
                let line_start = out.rfind('\n').map(|index| index + 1).unwrap_or(0);
                let line = out[line_start..].to_lowercase();
                let mut missing: Vec<&str> = Vec::new();
                for name in names {
                    if !line.contains(&name.to_lowercase())
                        && !missing
                            .iter()
                            .any(|existing| existing.eq_ignore_ascii_case(name))
                    {
                        missing.push(name);
                    }
                }
                if missing.is_empty() {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                } else {
                    out.push_str(&missing.join(", "));
                }
            }
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        // Models sometimes write the name and cite it too. Collapse the
        // resulting "Name Name" without touching legitimate repeats.
        for entry in &self.entries {
            let doubled = format!("{} {}", entry.name, entry.name);
            while out.contains(&doubled) {
                out = out.replace(&doubled, &entry.name);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, name: &str) -> CandidateLite {
        CandidateLite {
            id: id.into(),
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn handles_are_short_stable_and_reused() {
        let mut refs = Refs::default();
        assert_eq!(refs.handle(&candidate("uuid-1", "Sarah Chen")), "a1");
        assert_eq!(refs.handle(&candidate("uuid-1", "Sarah Chen")), "a1");
        assert_eq!(refs.handle(&candidate("uuid-2", "Marcus Webb")), "a2");
        assert_eq!(refs.resolve("[a2]"), Some("uuid-2"));
    }

    #[test]
    fn expands_citations_to_names() {
        let mut refs = Refs::default();
        refs.handle(&candidate("uuid-1", "Sarah Chen"));
        refs.handle(&candidate("uuid-2", "Marcus Webb"));
        assert_eq!(
            refs.render("Top: [a1, a2].\nRunner-up: [a1]."),
            "Top: Sarah Chen, Marcus Webb.\nRunner-up: Sarah Chen."
        );
    }

    #[test]
    fn leaves_unknown_brackets_alone() {
        let refs = Refs::default();
        assert_eq!(refs.render("keep [this] and [a9]"), "keep [this] and [a9]");
    }

    #[test]
    fn collapses_a_name_the_model_wrote_and_also_cited() {
        let mut refs = Refs::default();
        refs.handle(&candidate("uuid-1", "Sarah Chen"));
        assert_eq!(
            refs.render("**Sarah Chen [a1]** is strong."),
            "**Sarah Chen** is strong."
        );
    }

    #[test]
    fn drops_a_citation_when_the_name_is_already_on_the_line() {
        let mut refs = Refs::default();
        refs.handle(&candidate("uuid-1", "Sarah Chen"));
        assert_eq!(
            refs.render("**Sarah Chen** built the ledger [a1]."),
            "**Sarah Chen** built the ledger."
        );
    }
}
