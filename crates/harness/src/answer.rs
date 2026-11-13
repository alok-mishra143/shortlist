//! Opt-in zero-token answers for questions the local tools already answered.
//!
//! Chat leaves `Config::direct` off so the model writes every answer. These
//! renderers remain for offline tooling, benchmarks, and CLI inspection, where
//! a count or a lookup should not need a model.

use crate::protocol::Call;
use crate::refs::Refs;
use crate::tool::ToolResult;

/// Render prefetched results only when every call is self-explanatory.
pub fn direct(calls: &[(Call, ToolResult)], refs: &Refs) -> Option<String> {
    let mut parts = Vec::new();
    for (call, result) in calls {
        let part = match call.tool.as_str() {
            "stats" => render_stats(&result.body),
            "find" => render_find(&result.body).or_else(|| no_matches(&result.body)),
            "shortlist" => render_shortlist(&result.body).or_else(|| no_matches(&result.body)),
            "get" => render_get(&result.body, call),
            "read" => render_read(&result.body, refs),
            "grep" => render_grep(&result.body, refs),
            "compare" => render_compare(&result.body),
            "note" => render_note(&result.body, refs),
            _ => None,
        }?;
        parts.push(part);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n\n"))
}

fn render_stats(body: &str) -> Option<String> {
    let body = body.trim();
    let rest = body.strip_prefix("stats ")?;
    if body.contains("n=0") {
        return Some("The candidate library is empty.".into());
    }
    let mut lines = rest.lines();
    let mut out = String::from("**Library**");
    if let Some(first) = lines.next() {
        for token in first.split_whitespace() {
            if let Some(value) = token.strip_prefix("n=") {
                out.push_str(&format!(" · {value} candidates"));
            } else if let Some(value) = token.strip_prefix("y=") {
                out.push_str(&format!(" · {value}y"));
            } else if let Some(value) = token.strip_prefix("avg=") {
                out.push_str(&format!(" · avg {value}"));
            }
        }
    }
    for line in lines {
        out.push('\n');
        if let Some(value) = line.strip_prefix("skills: ") {
            out.push_str("**Top skills** · ");
            out.push_str(&value.replace(',', ", ").replace('(', " ("));
        } else if let Some(value) = line.strip_prefix("loc: ") {
            out.push_str("**Locations** · ");
            out.push_str(&value.replace(';', ", ").replace('(', " ("));
        } else if let Some(value) = line.strip_prefix("files: ") {
            out.push_str("**Sources** · ");
            out.push_str(value);
        } else {
            out.push_str(line);
        }
    }
    Some(out)
}

fn render_find(body: &str) -> Option<String> {
    let mut rows = Vec::new();
    for line in body.lines() {
        if !line.starts_with('a') {
            continue;
        }
        let columns: Vec<&str> = line.split('|').collect();
        if columns.len() < 6 {
            continue;
        }
        rows.push(format!(
            "- **{}** — {} · {}y · {} · {}",
            columns[1],
            columns[2],
            columns[3],
            columns[4],
            pretty_skills(columns[5])
        ));
    }
    if rows.is_empty() {
        return None;
    }
    let matches = body
        .lines()
        .next()
        .and_then(|header| header.split("matches=").nth(1))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(rows.len());
    let header = if matches > rows.len() {
        format!("**{matches} matches** · showing {}", rows.len())
    } else {
        format!(
            "**{matches} match{}**",
            if matches == 1 { "" } else { "es" }
        )
    };
    Some(format!("{header}\n{}", rows.join("\n")))
}

fn no_matches(body: &str) -> Option<String> {
    body.contains("rows=0")
        .then(|| "**No matches** — try broader terms or drop a filter.".to_string())
}

fn render_shortlist(body: &str) -> Option<String> {
    let mut rows = Vec::new();
    for line in body.lines() {
        if !line.starts_with('a') {
            continue;
        }
        let columns: Vec<&str> = line.split('|').collect();
        if columns.len() < 9 {
            continue;
        }
        let missing = columns[3]
            .split_once('!')
            .map(|(_, missing)| format!(" · missing {missing}"))
            .unwrap_or_default();
        let why = columns[8].trim();
        let evidence = if why.is_empty() {
            String::new()
        } else {
            format!(" — “{why}”")
        };
        rows.push(format!(
            "- **{}** — pre {} · {}y · {} · {}{missing}{evidence}",
            columns[1],
            columns[2],
            columns[5],
            columns[6],
            pretty_skills(columns[7])
        ));
    }
    if rows.is_empty() {
        return None;
    }
    let header = body
        .lines()
        .next()
        .and_then(|line| line.split("evaluated=").nth(1))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(rows.len());
    let title = if header > rows.len() {
        format!("**Top {} of {header} evaluated**", rows.len())
    } else {
        format!(
            "**{} match{}**",
            rows.len(),
            if rows.len() == 1 { "" } else { "es" }
        )
    };
    Some(format!("{title}\n{}", rows.join("\n")))
}

fn pretty_skills(value: &str) -> String {
    let (base, more) = match value.rsplit_once('+') {
        Some((base, count)) if count.parse::<usize>().is_ok() => {
            (base.trim_end_matches(','), Some(count))
        }
        _ => (value, None),
    };
    let mut out = base.replace(',', ", ");
    if let Some(count) = more {
        out.push_str(&format!(" +{count} more"));
    }
    out
}

fn render_get(body: &str, call: &Call) -> Option<String> {
    let mut lines = body.lines();
    let first = lines.next()?;
    let (_handle, name) = first.split_once(' ')?;
    let requested = call.list("f");
    let mut headline = None;
    let mut parts = Vec::new();
    let mut skills = None;
    let mut summary = None;
    let mut notes = None;
    let mut email = None;
    let mut phone = None;
    let mut experience = None;
    let mut education = None;
    let mut certifications = None;
    for line in lines {
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "headline" => headline = Some(value.to_string()),
                "y" => parts.push(format!("{value}y")),
                "loc" => parts.push(value.to_string()),
                "skills" => skills = Some(pretty_skills(value)),
                "summary" => summary = Some(value.to_string()),
                "experience" => experience = Some(value.to_string()),
                "education" => education = Some(value.to_string()),
                "certs" => certifications = Some(value.to_string()),
                "notes" => notes = Some(value.to_string()),
                "email" => email = Some(value.to_string()),
                "phone" => phone = Some(value.to_string()),
                _ => {}
            }
        }
    }
    if let Some(answer) = render_contact(name, email.as_deref(), phone.as_deref(), &requested) {
        return Some(answer);
    }
    let mut out = format!("**{name}**");
    if let Some(headline) = headline {
        out.push('\n');
        out.push_str(&headline);
        if !parts.is_empty() {
            out.push_str(" · ");
            out.push_str(&parts.join(" · "));
        }
    } else if !parts.is_empty() {
        out.push('\n');
        out.push_str(&parts.join(" · "));
    }
    if let Some(skills) = skills {
        out.push_str("\n\nSkills · ");
        out.push_str(&skills);
    }
    if let Some(experience) = experience {
        out.push_str("\n\n**Experience**");
        for role in experience
            .split("; ")
            .filter(|role| !role.trim().is_empty())
        {
            out.push_str("\n- ");
            out.push_str(role.trim());
        }
    }
    if let Some(education) = education {
        out.push_str("\n\n**Education** · ");
        out.push_str(&education);
    }
    if let Some(certifications) = certifications {
        out.push_str("\n\n**Certifications** · ");
        out.push_str(&certifications);
    }
    if let Some(email) = email {
        out.push_str("\n\nEmail · ");
        out.push_str(&email);
    }
    if let Some(phone) = phone {
        out.push_str("\n\nPhone · ");
        out.push_str(&phone);
    }
    if let Some(summary) = summary {
        out.push_str("\n\n");
        out.push_str(&summary);
    }
    if let Some(notes) = notes
        && notes != "none"
    {
        out.push_str("\n\n**Recruiter note** · ");
        out.push_str(&notes);
    }
    Some(out)
}

/// A contact question should return the datum, not a profile card. The tool
/// omits empty fields, so a requested field with no value is reported here.
fn render_contact(
    name: &str,
    email: Option<&str>,
    phone: Option<&str>,
    requested: &[String],
) -> Option<String> {
    let wants_email = requested
        .iter()
        .any(|field| matches!(field.as_str(), "email" | "mail"));
    let wants_phone = requested
        .iter()
        .any(|field| matches!(field.as_str(), "phone" | "tel"));
    if !wants_email && !wants_phone {
        return None;
    }
    let mut out = format!("**{name}**");
    if wants_email {
        out.push_str("\n\n");
        match email {
            Some(value) => {
                out.push_str("Email · ");
                out.push_str(value);
            }
            None => out.push_str("No email on file."),
        }
    }
    if wants_phone {
        out.push_str("\n\n");
        match phone {
            Some(value) => {
                out.push_str("Phone · ");
                out.push_str(value);
            }
            None => out.push_str("No phone number on file."),
        }
    }
    Some(out)
}

fn render_read(body: &str, refs: &Refs) -> Option<String> {
    let mut lines = body.lines();
    let header = lines.next()?;
    let handle = header.split_whitespace().next()?;
    let name = refs.name(handle)?;
    let windows: Vec<&str> = lines.filter(|line| !line.trim().is_empty()).collect();
    if windows.is_empty() {
        return Some(format!("**{name}** — no matching text."));
    }
    let mut out = format!("**{name}** — matching text");
    for window in windows {
        out.push_str("\n> ");
        out.push_str(window.trim());
    }
    Some(out)
}

fn render_grep(body: &str, refs: &Refs) -> Option<String> {
    let mut rows = Vec::new();
    for line in body.lines() {
        let Some((handle, snippet)) = line.split_once(": ") else {
            continue;
        };
        if !handle.starts_with('a') {
            continue;
        }
        if let Some(name) = refs.name(handle) {
            rows.push(format!("- **{name}** — {snippet}"));
        }
    }
    if rows.is_empty() {
        return None;
    }
    Some(rows.join("\n"))
}

fn render_compare(body: &str) -> Option<String> {
    let mut lines = body.lines();
    let header = lines.next()?;
    let fields: Vec<&str> = header
        .strip_prefix("compare cols: id|name|")?
        .split('|')
        .collect();
    let mut rows = Vec::new();
    for line in lines {
        let columns: Vec<&str> = line.split('|').collect();
        if columns.len() < 2 + fields.len() {
            continue;
        }
        let mut cells = Vec::new();
        for (index, field) in fields.iter().enumerate() {
            let value = columns[index + 2];
            if !value.is_empty() {
                cells.push(format!("{field} · {value}"));
            }
        }
        rows.push(format!("- **{}** — {}", columns[1], cells.join(" · ")));
    }
    if rows.is_empty() {
        return None;
    }
    Some(rows.join("\n"))
}

fn render_note(body: &str, refs: &Refs) -> Option<String> {
    let (handle, rest) = body.split_once(' ')?;
    let name = refs.name(handle)?;
    Some(format!("**{name}** · {rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolResult;

    fn result(body: &str) -> ToolResult {
        ToolResult::new(body, 10_000, "test")
    }

    #[test]
    fn renders_a_shortlist_with_bold_names_and_evidence() {
        let calls = vec![(
            Call {
                tool: "shortlist".into(),
                args: Vec::new(),
            },
            result(
                "shortlist rows=1 evaluated=3\ncols: id|name|pre|req|any|yrs|loc|skills|why\na1|Sarah Chen|100|1/1|-|8|London, UK|Rust,Go|built payments",
            ),
        )];
        let answer = direct(&calls, &Refs::default()).unwrap();
        assert!(answer.contains("**Sarah Chen**"));
        assert!(answer.contains("pre 100"));
        assert!(answer.contains("built payments"));
    }

    #[test]
    fn renders_stats_as_prose() {
        let calls = vec![(
            Call {
                tool: "stats".into(),
                args: Vec::new(),
            },
            result("stats n=12 y=4-12 avg=7.3\nskills: Rust(3),Go(2)"),
        )];
        let answer = direct(&calls, &Refs::default()).unwrap();
        assert!(answer.contains("12 candidates"));
        assert!(answer.contains("Top skills"));
    }

    #[test]
    fn renders_only_the_requested_contact_fields() {
        let calls = vec![(
            Call {
                tool: "get".into(),
                args: vec![
                    ("id".into(), "a1".into()),
                    ("f".into(), "email,phone".into()),
                ],
            },
            result("a1 Sarah Chen\nemail=sarah@example.com"),
        )];
        let answer = direct(&calls, &Refs::default()).unwrap();
        assert!(answer.contains("**Sarah Chen**"));
        assert!(answer.contains("sarah@example.com"));
        assert!(answer.contains("No phone number on file"));
        assert!(!answer.contains("Skills"));
    }

    #[test]
    fn an_empty_search_is_answered_locally() {
        let answer = direct(
            &[(
                Call {
                    tool: "find".into(),
                    args: Vec::new(),
                },
                result("find rows=0 matches=0 library=12\ncols: id|name|title|yrs|loc|skills"),
            )],
            &Refs::default(),
        )
        .unwrap();
        assert!(answer.contains("No matches"));
    }
}
