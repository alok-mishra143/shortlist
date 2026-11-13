//! Candidate-level tools: read one profile, search inside a resume, take notes,
//! and compare a handful of people side by side.

use anyhow::{Result, anyhow};

use super::{count, resolve, resolve_id, text_of};
use crate::protocol::Call;
use crate::search::{compact_list, snippet, years};
use crate::tool::{Tool, ToolContext, ToolResult};

pub struct Get;

impl Tool for Get {
    fn name(&self) -> &'static str {
        "get"
    }

    fn signature(&self) -> &'static str {
        "get id [f chars] — profile fields"
    }

    fn run(&self, ctx: &mut ToolContext<'_>, call: &Call) -> Result<ToolResult> {
        let resolved = resolve(ctx, call, "id")?;
        let mut fields = call.list("f");
        if fields.is_empty() {
            fields = [
                "headline",
                "yrs",
                "loc",
                "skills",
                "summary",
                "experience",
                "education",
                "certifications",
                "email",
                "notes",
            ]
            .iter()
            .map(|field| field.to_string())
            .collect();
        }
        let all = fields.iter().any(|field| field == "all");
        let want = |names: &[&str]| -> bool {
            all || fields.iter().any(|field| names.contains(&field.as_str()))
        };
        let chars = call.uint("chars").unwrap_or(400).clamp(80, 4_000);
        let row = &resolved.row;

        let mut body = format!("{} {}", resolved.handle, row.name);
        if want(&["headline", "title", "t"]) && !row.headline.is_empty() {
            body.push_str(&format!("\nheadline={}", snippet(&row.headline, 80)));
        }
        if want(&["yrs", "years", "y"]) {
            body.push_str(&format!("\ny={}", years(row.years)));
        }
        if want(&["loc", "location"]) && !row.location.is_empty() {
            body.push_str(&format!("\nloc={}", snippet(&row.location, 60)));
        }
        if want(&["skills", "s"]) && !row.skills.is_empty() {
            body.push_str(&format!("\nskills={}", compact_list(&row.skills, 20)));
        }
        if want(&["summary", "sum"]) && !row.summary.is_empty() {
            body.push_str(&format!(
                "\nsummary={}",
                snippet(&row.summary, chars.min(600))
            ));
        }
        if want(&["experience", "exp", "roles"]) && !row.experience.is_empty() {
            body.push_str(&format!(
                "\nexperience={}",
                shortlist_core::experience_summary(&row.experience, 5, 160).replace('\n', " ")
            ));
        }
        if want(&["education", "edu"]) && !row.education.is_empty() {
            let education = row
                .education
                .iter()
                .take(3)
                .map(|item| item.label())
                .filter(|label| !label.is_empty())
                .collect::<Vec<_>>()
                .join("; ");
            if !education.is_empty() {
                body.push_str(&format!("\neducation={}", snippet(&education, 240)));
            }
        }
        if want(&["certifications", "certs"]) && !row.certifications.is_empty() {
            body.push_str(&format!("\ncerts={}", compact_list(&row.certifications, 8)));
        }
        if want(&["email", "mail"]) && !row.email.is_empty() {
            body.push_str(&format!("\nemail={}", row.email));
        }
        if want(&["phone", "tel"]) && !row.phone.is_empty() {
            body.push_str(&format!("\nphone={}", row.phone));
        }
        if want(&["file", "source"]) && !row.file_name.is_empty() {
            body.push_str(&format!("\nfile={}", row.file_name));
        }
        if want(&["notes", "note"]) {
            match ctx.store.note(&resolved.id)? {
                Some(note) if !note.trim().is_empty() => {
                    body.push_str(&format!("\nnotes={}", snippet(&note, 240)));
                }
                _ if fields.iter().any(|f| f == "notes") => body.push_str("\nnotes=none"),
                _ => {}
            }
        }
        if want(&["text", "body"]) {
            let text = text_of(ctx, &resolved.id)?;
            body.push_str(&format!("\ntext={}", snippet(&text, chars)));
        }

        let raw = ctx.store.candidate_text_chars(&resolved.id)?;
        Ok(ToolResult::new(
            body,
            raw,
            format!("{} profile", resolved.row.name),
        ))
    }
}

pub struct Read;

impl Tool for Read {
    fn name(&self) -> &'static str {
        "read"
    }

    fn signature(&self) -> &'static str {
        "read id q [w n] — resume windows"
    }

    fn run(&self, ctx: &mut ToolContext<'_>, call: &Call) -> Result<ToolResult> {
        let resolved = resolve(ctx, call, "id")?;
        let query = call
            .arg("q")
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .ok_or_else(|| anyhow!("read needs q= (the phrase to find)"))?;
        let window = call.uint("w").unwrap_or(120).clamp(40, 400);
        let limit = count(call.uint("n"), 3, 6);
        let text = text_of(ctx, &resolved.id)?;
        let found = windows(&text, query, window, limit);
        let body = if found.is_empty() {
            format!(
                "{} read \"{query}\" hits=0 — no match (try a shorter term)",
                resolved.handle
            )
        } else {
            format!(
                "{} read \"{query}\" hits={}\n{}",
                resolved.handle,
                found.len(),
                found.join("\n")
            )
        };
        let raw = text.chars().count();
        Ok(ToolResult::new(
            body,
            raw,
            format!("{} matches", found.len()),
        ))
    }
}

pub struct Grep;

impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn signature(&self) -> &'static str {
        "grep q [ids w n] — snippets"
    }

    fn run(&self, ctx: &mut ToolContext<'_>, call: &Call) -> Result<ToolResult> {
        let query = call
            .arg("q")
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .ok_or_else(|| anyhow!("grep needs q="))?;
        let window = call.uint("w").unwrap_or(90).clamp(40, 300);
        let limit = count(call.uint("n"), 6, 10);
        let requested = call.list("ids");

        let mut targets = Vec::new();
        if requested.is_empty() {
            let hits = ctx.ranked(query, 12)?;
            let library = ctx.library()?;
            for hit in hits {
                if let Some(row) = library.rows.get(hit.row) {
                    targets.push(row.clone());
                }
            }
        } else {
            for raw in &requested {
                let resolved = resolve_id(ctx, raw)?;
                targets.push(resolved.row);
            }
        }

        let mut lines = Vec::new();
        for row in &targets {
            if lines.len() >= limit {
                break;
            }
            let Ok(text) = text_of(ctx, &row.id) else {
                continue;
            };
            if let Some(window_text) = windows(&text, query, window, 1).into_iter().next() {
                lines.push(format!("{}: {window_text}", ctx.handle(row)));
            }
        }
        let body = if lines.is_empty() {
            format!("grep \"{query}\" hits=0")
        } else {
            format!(
                "grep \"{query}\" hits={}\n{}",
                lines.len(),
                lines.join("\n")
            )
        };
        Ok(ToolResult::new(
            body,
            ctx.store.total_text_chars()?,
            format!("{} snippets", lines.len()),
        ))
    }
}

pub struct Note;

impl Tool for Note {
    fn name(&self) -> &'static str {
        "note"
    }

    fn signature(&self) -> &'static str {
        "note id — recruiter note (read-only for the model)"
    }

    fn run(&self, ctx: &mut ToolContext<'_>, call: &Call) -> Result<ToolResult> {
        let resolved = resolve(ctx, call, "id")?;
        if call.flag("del") || call.flag("delete") {
            if !ctx.allow_writes {
                return Err(anyhow!(
                    "note writes are disabled for model calls; edit the note in the candidate drawer"
                ));
            }
            ctx.store.delete_note(&resolved.id)?;
            return Ok(ToolResult::new(
                format!("{} note deleted", resolved.handle),
                0,
                "note deleted",
            ));
        }
        if let Some(value) = call.arg("set") {
            if !ctx.allow_writes {
                return Err(anyhow!(
                    "note writes are disabled for model calls; edit the note in the candidate drawer"
                ));
            }
            let value = value.trim();
            ctx.store.save_note(&resolved.id, value)?;
            return Ok(ToolResult::new(
                format!("{} note saved ({} chars)", resolved.handle, value.len()),
                0,
                "note saved",
            ));
        }
        let raw = ctx.store.note(&resolved.id)?;
        let body = match raw.as_deref().map(str::trim) {
            Some(note) if !note.is_empty() => {
                format!("{} note: {}", resolved.handle, snippet(note, 300))
            }
            _ => format!("{} note: none", resolved.handle),
        };
        let raw_chars = raw.map(|note| note.len()).unwrap_or(0);
        Ok(ToolResult::new(body, raw_chars, "note"))
    }
}

pub struct Compare;

impl Tool for Compare {
    fn name(&self) -> &'static str {
        "compare"
    }

    fn signature(&self) -> &'static str {
        "compare ids [f len] → id|name|fields — aligned rows"
    }

    fn run(&self, ctx: &mut ToolContext<'_>, call: &Call) -> Result<ToolResult> {
        let ids = call.list("ids");
        if ids.is_empty() {
            return Err(anyhow!("compare needs ids=a1,a2"));
        }
        let fields = if call.list("f").is_empty() {
            vec![
                "yrs".to_string(),
                "skills".to_string(),
                "summary".to_string(),
            ]
        } else {
            call.list("f")
        };
        let width = call.uint("len").unwrap_or(90).clamp(40, 300);
        let mut rows = Vec::new();
        let mut raw_chars = 0usize;
        for raw in ids.iter().take(6) {
            let resolved = resolve_id(ctx, raw)?;
            let mut cells = vec![resolved.handle.clone(), resolved.row.name.replace('|', "/")];
            for field in &fields {
                let value = match field.as_str() {
                    "yrs" | "years" => years(resolved.row.years),
                    "skills" => compact_list(&resolved.row.skills, 8),
                    "summary" => snippet(&resolved.row.summary, width),
                    "experience" | "exp" => {
                        shortlist_core::experience_summary(&resolved.row.experience, 4, width)
                    }
                    "education" | "edu" => resolved
                        .row
                        .education
                        .iter()
                        .map(|item| item.label())
                        .filter(|label| !label.is_empty())
                        .collect::<Vec<_>>()
                        .join("; "),
                    "certifications" | "certs" => compact_list(&resolved.row.certifications, 8),
                    "loc" | "location" => snippet(&resolved.row.location, width),
                    "headline" | "title" => snippet(&resolved.row.headline, width),
                    "email" => resolved.row.email.clone(),
                    "phone" => resolved.row.phone.clone(),
                    "file" => resolved.row.file_name.clone(),
                    "notes" => ctx
                        .store
                        .note(&resolved.id)?
                        .map(|note| snippet(&note, width))
                        .unwrap_or_default(),
                    other => return Err(anyhow!("compare: unknown field '{other}'")),
                };
                cells.push(value.replace('|', "/"));
            }
            raw_chars += ctx.store.candidate_text_chars(&resolved.id)?;
            rows.push(cells.join("|"));
        }
        let header = format!("compare cols: id|name|{}", fields.join("|"));
        Ok(ToolResult::new(
            format!("{header}\n{}", rows.join("\n")),
            raw_chars,
            format!("{} candidates", rows.len()),
        ))
    }
}

/// Case-insensitive matching windows, collapsed and clipped. A plural query
/// also tries its singular so "payments" finds "payment infrastructure".
fn windows(text: &str, query: &str, width: usize, limit: usize) -> Vec<String> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let haystack = text.to_ascii_lowercase();
    let Some(needle) = variants(query)
        .into_iter()
        .find(|candidate| haystack.contains(candidate.as_str()))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut from = 0usize;
    while out.len() < limit {
        let Some(relative) = haystack[from..].find(&needle) else {
            break;
        };
        let position = from + relative;
        let mut start = position.saturating_sub(width / 2);
        while start > 0 && !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (position + needle.len() + width / 2).min(text.len());
        while end < text.len() && !text.is_char_boundary(end) {
            end += 1;
        }
        // Do not start or end mid-word. The match sits in the middle of the
        // window, so trimming to a word boundary cannot cut it out.
        if start > 0
            && let Some(offset) = text[start..].find(char::is_whitespace)
            && start + offset < position
        {
            start += offset + 1;
        }
        if end < text.len()
            && let Some(offset) = text[..end].rfind(char::is_whitespace)
            && offset >= position + needle.len()
        {
            end = offset;
        }
        let mut piece = snippet(&text[start..end], width * 2);
        if start > 0 {
            piece.insert(0, '…');
        }
        if end < text.len() {
            piece.push('…');
        }
        out.push(piece);
        from = end.max(position + needle.len());
    }
    out
}

fn variants(query: &str) -> Vec<String> {
    let lower = query.to_ascii_lowercase();
    let mut out = vec![lower.clone()];
    if lower.len() > 4 {
        if let Some(stem) = lower.strip_suffix("ies") {
            out.push(format!("{stem}y"));
        } else if lower.ends_with("es") {
            out.push(lower[..lower.len() - 2].to_string());
            out.push(lower[..lower.len() - 1].to_string());
        } else if lower.ends_with('s') {
            out.push(lower[..lower.len() - 1].to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_find_each_mention_once() {
        let text = "Built the ledger. Later the ledger scaled. Finally a third ledger.";
        let found = windows(text, "ledger", 30, 3);
        assert_eq!(found.len(), 3);
        assert!(found.iter().all(|s| s.contains("ledger")));
        assert_ne!(found[0], found[1]);
    }

    #[test]
    fn windows_return_nothing_for_a_miss() {
        assert!(windows("Rust and Go", "cobol", 40, 2).is_empty());
    }

    #[test]
    fn a_plural_query_finds_the_singular() {
        let found = windows(
            "Built payment infrastructure at a fintech.",
            "payments",
            40,
            1,
        );
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("payment infrastructure"));
    }
}
