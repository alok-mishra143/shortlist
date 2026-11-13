//! Markdown tables from assistant replies.
//!
//! The chat view renders answers block by block instead of using a full
//! Markdown parser. This module supplies the one missing block: it detects a
//! table, splits its cells, and reports where the table ends so the renderer
//! can continue with normal paragraphs.
//!
//! Parsing is deliberately lenient about spacing and width because model
//! output often drifts from CommonMark. A separator row is still required, so
//! a stray pipe in prose never turns into a table.

/// Column alignment from the separator row (`:---`, `:---:`, `---:`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Alignment {
    #[default]
    Left,
    Center,
    Right,
}

/// A parsed Markdown table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub alignments: Vec<Alignment>,
}

impl Table {
    /// Longest visible cell in each column, used to size columns.
    /// Inline Markdown markers do not take up visual space.
    pub fn column_widths(&self) -> Vec<usize> {
        (0..self.header.len())
            .map(|column| {
                std::iter::once(self.header.get(column))
                    .chain(self.rows.iter().map(|row| row.get(column)))
                    .flatten()
                    .map(|cell| visible_len(cell))
                    .max()
                    .unwrap_or(0)
            })
            .collect()
    }
}

/// Parses a table beginning at `lines[start]`, returning it and the index of
/// the first line after it.
///
/// Blank lines are tolerated between the header, separator, and rows because
/// models frequently add them. A blank line only stays inside the table when
/// the next non-empty line is a pipe-wrapped row; otherwise it ends the table
/// and is left for the surrounding renderer.
pub fn parse(lines: &[&str], start: usize) -> Option<(Table, usize)> {
    let header = split_row(lines.get(start)?)?;
    if header.len() < 2 {
        return None;
    }

    let separator_index = next_non_empty(lines, start + 1)?;
    let alignments = parse_separator(lines[separator_index])?;
    if alignments.len() != header.len() {
        return None;
    }
    let columns = header.len();

    let mut rows = Vec::new();
    let mut index = separator_index + 1;

    while index < lines.len() {
        if lines[index].trim().is_empty() {
            let Some(next) = next_non_empty(lines, index + 1) else {
                break;
            };
            if !is_pipe_wrapped(lines[next]) {
                break;
            }
            index = next;
        }

        if !is_body_row(lines[index], columns) {
            break;
        }
        let mut cells = split_row(lines[index]).unwrap_or_default();
        cells.resize(columns, String::new());
        cells.truncate(columns);
        rows.push(cells);
        index += 1;
    }

    Some((
        Table {
            header,
            rows,
            alignments,
        },
        index,
    ))
}

fn next_non_empty(lines: &[&str], start: usize) -> Option<usize> {
    (start..lines.len()).find(|&index| !lines[index].trim().is_empty())
}

fn is_pipe_wrapped(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && !trimmed.ends_with("\\|")
}

fn is_body_row(line: &str, columns: usize) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || parse_separator(trimmed).is_some() {
        return false;
    }
    match split_row(trimmed) {
        Some(cells) => cells.len() == columns || (is_pipe_wrapped(trimmed) && !cells.is_empty()),
        None => false,
    }
}

/// Splits one pipe-delimited row. Pipes escaped with a backslash stay inside
/// the cell, and leading or trailing delimiters do not create empty cells.
fn split_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }

    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut escaped = false;
    let mut last_split = false;

    for ch in trimmed.chars() {
        if escaped {
            if ch != '|' {
                cell.push('\\');
            }
            cell.push(ch);
            escaped = false;
            last_split = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '|' => {
                cells.push(cell.trim().to_string());
                cell.clear();
                last_split = true;
            }
            _ => {
                cell.push(ch);
                last_split = false;
            }
        }
    }
    if escaped {
        cell.push('\\');
    }
    if !last_split {
        cells.push(cell.trim().to_string());
    }

    if trimmed.starts_with('|') && !cells.is_empty() {
        cells.remove(0);
    }

    (!cells.is_empty()).then_some(cells)
}

fn parse_separator(line: &str) -> Option<Vec<Alignment>> {
    let cells = split_row(line)?;
    let mut alignments = Vec::with_capacity(cells.len());
    for cell in cells {
        let cell = cell.trim();
        let left = cell.starts_with(':');
        let right = cell.ends_with(':');
        let dashes = cell.trim_start_matches(':').trim_end_matches(':').trim();
        if dashes.is_empty() || !dashes.chars().all(|ch| ch == '-') {
            return None;
        }
        alignments.push(match (left, right) {
            (true, true) => Alignment::Center,
            (false, true) => Alignment::Right,
            _ => Alignment::Left,
        });
    }
    (!alignments.is_empty()).then_some(alignments)
}

fn visible_len(cell: &str) -> usize {
    let mut length = 0;
    let mut chars = cell.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' | '`' | '~' => {}
            '[' => {
                // Count a link's label, not its target.
                for inner in chars.by_ref() {
                    if inner == ']' {
                        break;
                    }
                    length += 1;
                }
                if chars.peek() == Some(&'(') {
                    chars.next();
                    for inner in chars.by_ref() {
                        if inner == ')' {
                            break;
                        }
                    }
                }
            }
            _ => length += 1,
        }
    }
    length
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    #[test]
    fn parses_a_candidate_table() {
        let text = "\
| Rank | Candidate | Why | Gap |
|---|---|---|---|
| 1 | Sarah Chen | Rust among core skills. | Tenure not stated. |
| 2 | Fatima Zahra | Security engineer. | Narrower backend depth. |";
        let lines = lines(text);
        let (table, next) = parse(&lines, 0).expect("table");
        assert_eq!(next, 4);
        assert_eq!(table.header.join(" | "), "Rank | Candidate | Why | Gap");
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0][1], "Sarah Chen");
        assert_eq!(table.alignments.len(), 4);
    }

    #[test]
    fn tolerates_blank_lines_between_rows() {
        let text = "\
| A | B |
| --- | --- |

| 1 | two |

| 3 | four |";
        let lines = lines(text);
        let (table, next) = parse(&lines, 0).expect("table");
        assert_eq!(next, 6);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[1][0], "3");
    }

    #[test]
    fn stops_at_the_first_non_table_line() {
        let text = "\
| A | B |
| --- | --- |
| 1 | two |

Verdict: ship it.";
        let lines = lines(text);
        let (table, next) = parse(&lines, 0).expect("table");
        assert_eq!(next, 3);
        assert_eq!(table.rows.len(), 1);
    }

    #[test]
    fn reads_alignment_markers() {
        let text = "\
| L | C | R |
| :--- | :---: | ---: |
| 1 | 2 | 3 |";
        let lines = lines(text);
        let (table, _) = parse(&lines, 0).expect("table");
        assert_eq!(
            table.alignments,
            vec![Alignment::Left, Alignment::Center, Alignment::Right]
        );
    }

    #[test]
    fn escaped_pipes_stay_in_the_cell() {
        let text = "\
| A | B |
| --- | --- |
| x \\| y | z |";
        let lines = lines(text);
        let (table, _) = parse(&lines, 0).expect("table");
        assert_eq!(table.rows[0][0], "x | y");
    }

    #[test]
    fn requires_a_separator_with_matching_columns() {
        assert!(parse(&lines("| A | B |\nplain text"), 0).is_none());
        assert!(parse(&lines("| A | B |\n|---|"), 0).is_none());
        assert!(parse(&lines("Name | Role\n---"), 0).is_none());
    }

    #[test]
    fn pads_short_rows_to_the_header() {
        let text = "\
| A | B | C |
| --- | --- | --- |
| 1 | two |";
        let lines = lines(text);
        let (table, _) = parse(&lines, 0).expect("table");
        assert_eq!(table.rows[0], vec!["1", "two", ""]);
    }

    #[test]
    fn parses_rows_without_outer_pipes() {
        let text = "\
Rank | Candidate | Why
--- | --- | ---
1 | Sarah Chen | Rust";
        let lines = lines(text);
        let (table, next) = parse(&lines, 0).expect("table");
        assert_eq!(next, 3);
        assert_eq!(table.header.len(), 3);
        assert_eq!(table.rows[0][2], "Rust");
    }

    #[test]
    fn column_widths_ignore_inline_markup() {
        let text = "\
| **Rank** | `Name` |
| --- | --- |
| 1 | Sarah Chen |";
        let lines = lines(text);
        let (table, _) = parse(&lines, 0).expect("table");
        assert_eq!(table.column_widths(), vec![4, 10]);
    }
}
