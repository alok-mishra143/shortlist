//! The wire format between the model and the tools.
//!
//! JSON function calling is correct but expensive: every call repeats schema
//! keys, braces and quotes. A line-oriented form costs a fraction of that and
//! parses without a schema:
//!
//! ```text
//! find q="rust fintech" n=8
//! get id=a1 f=summary,skills
//! ```
//!
//! Tools are executed locally, so the model never needs to see internal ids or
//! validation metadata.

/// One parsed tool call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub tool: String,
    pub args: Vec<(String, String)>,
}

impl Call {
    pub fn arg(&self, key: &str) -> Option<&str> {
        self.args
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// A comma-separated argument, trimmed and with empty entries removed.
    pub fn list(&self, key: &str) -> Vec<String> {
        self.arg(key)
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn number(&self, key: &str) -> Option<f32> {
        self.arg(key).and_then(|v| v.trim().parse::<f32>().ok())
    }

    pub fn uint(&self, key: &str) -> Option<usize> {
        self.arg(key).and_then(|v| v.trim().parse::<usize>().ok())
    }

    pub fn flag(&self, key: &str) -> bool {
        self.arg(key)
            .map(|v| !matches!(v.trim(), "" | "0" | "false" | "no"))
            .unwrap_or(false)
    }

    /// Stable key for the duplicate-call cache.
    pub fn cache_key(&self) -> String {
        let mut parts: Vec<String> = self.args.iter().map(|(k, v)| format!("{k}={v}")).collect();
        parts.sort();
        format!("{}|{}", self.tool, parts.join("|"))
    }
}

/// Extract tool calls from a reply, or `None` when the reply is an answer.
///
/// Accepted forms, in order of preference:
/// 1. A fenced block tagged `shortlist`, `tool` or `tools`.
/// 2. Lines beginning with `@`.
/// 3. Call lines mixed with prose, as long as they look like real calls.
pub fn parse_calls(reply: &str, known: &[&str]) -> Option<Vec<Call>> {
    let mut fence_tag: Option<String> = None;
    let mut body = String::new();

    for line in reply.lines() {
        let trimmed = line.trim();
        if let Some(tag) = trimmed.strip_prefix("```") {
            match fence_tag.take() {
                None => {
                    fence_tag = Some(tag.trim().to_ascii_lowercase());
                    body.clear();
                }
                Some(tag) => {
                    let tag = tag.to_ascii_lowercase();
                    if tag.is_empty()
                        || matches!(tag.as_str(), "shortlist" | "tool" | "tools" | "calls")
                    {
                        let calls = collect_calls(&body, known);
                        if !calls.is_empty() {
                            return Some(calls);
                        }
                    }
                }
            }
            continue;
        }
        if fence_tag.is_some() {
            body.push_str(line);
            body.push('\n');
        }
    }

    // The model often narrates first, then emits bare call lines. Execute the
    // calls instead of rendering them as the answer.
    let calls = collect_calls(reply, known);
    (!calls.is_empty()).then_some(calls)
}

fn collect_calls(body: &str, known: &[&str]) -> Vec<Call> {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let call = parse_line(line, known)?;
            (looks_like_call(line, &call)).then_some(call)
        })
        .collect()
}

/// A line is a call when it has a key=value argument, is an `@` call, is a
/// bare handle argument (`get a1`), or is a no-argument tool (`stats`).
fn looks_like_call(line: &str, call: &Call) -> bool {
    if line.trim_start().starts_with('@') {
        return true;
    }
    let rest = line
        .splitn(2, char::is_whitespace)
        .nth(1)
        .unwrap_or("")
        .trim();
    if rest.contains('=') {
        return true;
    }
    if rest.split_whitespace().count() == 1 && is_handle(rest) {
        return true;
    }
    rest.is_empty() && matches!(call.tool.as_str(), "stats")
}

fn is_handle(value: &str) -> bool {
    let value = value
        .trim()
        .trim_start_matches(['@', '['])
        .trim_end_matches(']');
    value
        .strip_prefix(['a', 'A'])
        .map(|digits| !digits.is_empty() && digits.chars().all(|ch| ch.is_ascii_digit()))
        .unwrap_or(false)
}

fn parse_line(line: &str, known: &[&str]) -> Option<Call> {
    let line = line
        .trim()
        .trim_start_matches('@')
        .trim_start_matches("- ")
        .trim();
    if line.is_empty() {
        return None;
    }
    let mut split = line.splitn(2, char::is_whitespace);
    let tool = split.next()?.to_ascii_lowercase();
    if !known.contains(&tool.as_str()) {
        return None;
    }
    let rest = split.next().unwrap_or("").trim();
    let args = parse_args(rest)?;
    let mut call = Call { tool, args };
    normalize_positional(&mut call);
    Some(call)
}

/// Accept the shorthand models actually write: `get a1`, `read a2 q=...`,
/// `compare a1,a2`. Bare handles become `id`/`ids`; explicit keys win.
fn normalize_positional(call: &mut Call) {
    let needs_id = matches!(call.tool.as_str(), "get" | "read" | "note");
    let needs_ids = call.tool == "compare";
    if needs_id {
        if call.args.iter().any(|(key, _)| key == "id") {
            return;
        }
        if let Some(index) = call
            .args
            .iter()
            .position(|(key, value)| value == "1" && is_handle(key))
        {
            let handle = call.args.remove(index).0;
            call.args.insert(0, ("id".into(), handle));
        }
    } else if needs_ids && !call.args.iter().any(|(key, _)| key == "ids") {
        if let Some(index) = call
            .args
            .iter()
            .position(|(key, value)| value == "1" && is_handle_list(key))
        {
            let handles = call.args.remove(index).0;
            call.args.insert(0, ("ids".into(), handles));
        }
    }
}

fn is_handle_list(value: &str) -> bool {
    let parts: Vec<&str> = value.split(',').map(str::trim).collect();
    !parts.is_empty() && parts.iter().all(|part| is_handle(part))
}

fn parse_args(input: &str) -> Option<Vec<(String, String)>> {
    let mut chars = input.chars().peekable();
    let mut args = Vec::new();
    loop {
        while matches!(chars.peek(), Some(ch) if ch.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut key = String::new();
        while let Some(&ch) = chars.peek() {
            if ch == '=' || ch.is_whitespace() {
                break;
            }
            key.push(ch);
            chars.next();
        }
        if key.is_empty() {
            return None;
        }
        let mut value = String::from("1");
        if chars.peek() == Some(&'=') {
            chars.next();
            value.clear();
            match chars.peek() {
                Some('"') | Some('\'') => {
                    let quote = chars.next()?;
                    let mut closed = false;
                    while let Some(ch) = chars.next() {
                        match ch {
                            '\\' => {
                                if let Some(escaped) = chars.next() {
                                    value.push(escaped);
                                }
                            }
                            ch if ch == quote => {
                                closed = true;
                                break;
                            }
                            ch => value.push(ch),
                        }
                    }
                    if !closed {
                        return None;
                    }
                }
                _ => {
                    while let Some(&ch) = chars.peek() {
                        if ch.is_whitespace() {
                            break;
                        }
                        value.push(ch);
                        chars.next();
                    }
                }
            }
        }
        args.push((key.to_ascii_lowercase(), value));
    }
    Some(args)
}

/// True when a line is a valid call to one of the known tools.
pub fn is_call_line(line: &str, known: &[&str]) -> bool {
    parse_line(line, known).is_some()
}

/// Remove a wrapping markdown fence that some models add around plain answers.
pub fn clean_answer(reply: &str) -> String {
    let trimmed = reply.trim();
    if trimmed.starts_with("```") && trimmed.ends_with("```") {
        let inner = &trimmed[trimmed.find('\n').unwrap_or(3)..];
        let inner = inner.trim_end_matches('`').trim();
        if !inner.contains("```") {
            return inner.to_string();
        }
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KNOWN: &[&str] = &[
        "stats",
        "find",
        "get",
        "read",
        "grep",
        "note",
        "shortlist",
        "compare",
    ];

    #[test]
    fn parses_a_fenced_block_with_quoted_values() {
        let reply = "Sure.\n```shortlist\nfind q=\"rust fintech\" n=8\nstats n=5\n```";
        let calls = parse_calls(reply, KNOWN).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tool, "find");
        assert_eq!(calls[0].arg("q"), Some("rust fintech"));
        assert_eq!(calls[0].uint("n"), Some(8));
        assert_eq!(calls[1].uint("n"), Some(5));
    }

    #[test]
    fn parses_at_lines() {
        let calls = parse_calls("@stats\n@get id=a3 f=summary,skills", KNOWN).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].list("f"), vec!["summary", "skills"]);
    }

    #[test]
    fn parses_calls_mixed_with_prose() {
        let reply = "Only 3 of 13 have Terraform.\nPulling depth evidence.\n\
                     find q=\"terraform\"\nread a2 q=\"Terraform\" w=10\nread a3 q=\"Terraform\" w=10";
        let calls = parse_calls(reply, KNOWN).unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].tool, "find");
        assert_eq!(calls[1].arg("id"), Some("a2"));
        assert_eq!(calls[1].arg("q"), Some("Terraform"));
        assert_eq!(calls[1].arg("w"), Some("10"));
    }

    #[test]
    fn prose_that_mentions_a_tool_is_not_a_call() {
        assert!(parse_calls("Find the best candidate in the library.", KNOWN).is_none());
        assert!(parse_calls("get the data from the resume", KNOWN).is_none());
    }

    #[test]
    fn parses_bare_handles() {
        let calls = parse_calls("get a1", KNOWN).unwrap();
        assert_eq!(calls[0].tool, "get");
        assert_eq!(calls[0].arg("id"), Some("a1"));
    }

    #[test]
    fn treats_prose_as_an_answer() {
        assert!(parse_calls("Sarah Chen is the strongest Rust engineer.", KNOWN).is_none());
    }

    #[test]
    fn rejects_unknown_tools() {
        assert!(parse_calls("```shortlist\nlaunch_rockets\n```", KNOWN).is_none());
    }

    #[test]
    fn cache_key_is_order_insensitive() {
        let a = parse_calls("find n=8 q=rust", KNOWN).unwrap().remove(0);
        let b = parse_calls("find q=rust n=8", KNOWN).unwrap().remove(0);
        assert_eq!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn unquoted_values_keep_their_case() {
        let call = parse_calls("find q=Rust loc=London", KNOWN)
            .unwrap()
            .remove(0);
        assert_eq!(call.arg("q"), Some("Rust"));
        assert_eq!(call.arg("loc"), Some("London"));
    }
}
