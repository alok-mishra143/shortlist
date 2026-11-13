//! Pure Markdown block and inline editing helpers for the notes dialog.
//!
//! Every function takes UTF-8 text plus byte ranges and returns a new edit.
//! Nothing here touches GPUI, the filesystem, or the store.

use std::ops::Range;

/// The Markdown block style of a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    Bullet,
    Numbered,
    Quote,
}

/// Replacement text plus the byte range to select inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub text: String,
    /// Byte range in `text` (not the old text).
    pub selection: Range<usize>,
}

/// Rewrites every line touched by `selection` with the requested block prefix.
pub fn set_block(text: &str, selection: Range<usize>, kind: BlockKind) -> TextEdit {
    let selection = clamp_range(text, selection);
    let lines = split_lines(text);
    let first = line_index_at(&lines, selection.start);
    let mut last = line_index_at(&lines, selection.end);
    // A selection that ends exactly at a line start belongs to the previous
    // line, not the line it merely touches.
    if selection.end > selection.start && last > first && lines[last].start == selection.end {
        last -= 1;
    }

    let mut out = String::with_capacity(text.len() + 2 * (last - first + 1));
    let mut new_starts = Vec::with_capacity(lines.len());
    let mut old_prefixes = Vec::with_capacity(lines.len());
    let mut new_prefixes = Vec::with_capacity(lines.len());

    for (index, line) in lines.iter().enumerate() {
        let selected = first <= index && index <= last;
        let old_prefix_len = prefix_at(&text[line.start..line.body_end]).1;
        let new_prefix = if selected {
            render_prefix(kind, index - first + 1)
        } else {
            String::new()
        };

        new_starts.push(out.len());
        if selected {
            old_prefixes.push(old_prefix_len);
            new_prefixes.push(new_prefix.len());
            out.push_str(&new_prefix);
            out.push_str(&text[line.start + old_prefix_len..line.body_end]);
        } else {
            // Unselected lines are copied verbatim; a zero-to-zero prefix
            // mapping keeps their offsets unchanged.
            old_prefixes.push(0);
            new_prefixes.push(0);
            out.push_str(&text[line.start..line.body_end]);
        }
        out.push_str(&text[line.body_end..line.end]);
    }

    let start_line = first;
    let end_line = line_index_at(&lines, selection.end);
    let new_start = map_offset(
        selection.start,
        start_line,
        &lines,
        &new_starts,
        &old_prefixes,
        &new_prefixes,
    );
    let new_end = map_offset(
        selection.end,
        end_line,
        &lines,
        &new_starts,
        &old_prefixes,
        &new_prefixes,
    );

    let new_start = nearest_char_boundary(&out, new_start);
    let new_end = nearest_char_boundary(&out, new_end).max(new_start);
    TextEdit {
        text: out,
        selection: new_start..new_end,
    }
}

/// Wraps or unwraps `selection` with an inline `marker`.
pub fn toggle_inline(text: &str, selection: Range<usize>, marker: &str) -> TextEdit {
    let selection = clamp_range(text, selection);
    if marker.is_empty() {
        return TextEdit {
            text: text.to_string(),
            selection,
        };
    }

    let marker_len = marker.len();
    if selection.is_empty() {
        let at = selection.start;
        let mut out = String::with_capacity(text.len() + marker_len * 2);
        out.push_str(&text[..at]);
        out.push_str(marker);
        out.push_str(marker);
        out.push_str(&text[at..]);
        let caret = at + marker_len;
        return TextEdit {
            text: out,
            selection: caret..caret,
        };
    }

    let selected = &text[selection.clone()];
    if selected.len() >= marker_len * 2
        && selected.starts_with(marker)
        && selected.ends_with(marker)
    {
        // The selection already carries the markers: unwrap those instead of
        // adding another pair.
        let inner_start = selection.start + marker_len;
        let inner_end = selection.end - marker_len;
        let mut out = String::with_capacity(text.len() - marker_len * 2);
        out.push_str(&text[..selection.start]);
        out.push_str(&text[inner_start..inner_end]);
        out.push_str(&text[selection.end..]);
        return TextEdit {
            text: out,
            selection: selection.start..(selection.end - marker_len * 2),
        };
    }

    let wrapped_around = selection.start >= marker_len
        && text.len() - selection.end >= marker_len
        && text[..selection.start].ends_with(marker)
        && text[selection.end..].starts_with(marker);
    if wrapped_around {
        let before = selection.start - marker_len;
        let after = selection.end + marker_len;
        let mut out = String::with_capacity(text.len() - marker_len * 2);
        out.push_str(&text[..before]);
        out.push_str(&text[selection.start..selection.end]);
        out.push_str(&text[after..]);
        return TextEdit {
            text: out,
            selection: before..(selection.end - marker_len),
        };
    }

    let mut out = String::with_capacity(text.len() + marker_len * 2);
    out.push_str(&text[..selection.start]);
    out.push_str(marker);
    out.push_str(selected);
    out.push_str(marker);
    out.push_str(&text[selection.end..]);
    TextEdit {
        text: out,
        selection: (selection.start + marker_len)..(selection.end + marker_len),
    }
}

/// Returns the block style of the line containing `cursor`.
pub fn block_at(text: &str, cursor: usize) -> BlockKind {
    let cursor = nearest_char_boundary(text, cursor);
    let lines = split_lines(text);
    let line = &lines[line_index_at(&lines, cursor)];
    prefix_at(&text[line.start..line.body_end]).0
}

/// Reports whether `selection` is already wrapped with `marker`.
pub fn is_wrapped(text: &str, selection: Range<usize>, marker: &str) -> bool {
    if marker.is_empty() {
        return false;
    }

    let selection = clamp_range(text, selection);
    let marker_len = marker.len();
    if selection.is_empty() {
        return text[..selection.start].ends_with(marker)
            && text[selection.start..].starts_with(marker);
    }

    let selected = &text[selection.clone()];
    if selected.len() >= marker_len * 2
        && selected.starts_with(marker)
        && selected.ends_with(marker)
    {
        return true;
    }

    selection.start >= marker_len
        && text.len() - selection.end >= marker_len
        && text[..selection.start].ends_with(marker)
        && text[selection.end..].starts_with(marker)
}

#[derive(Clone, Copy)]
struct Line {
    start: usize,
    /// End of the line's content, excluding a trailing `\r\n` or `\n`.
    body_end: usize,
    /// End of the line including its terminator.
    end: usize,
}

fn split_lines(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;

    for segment in text.split_inclusive('\n') {
        let end = start + segment.len();
        let body_end = if segment.ends_with('\n') {
            let without_newline = end - 1;
            if without_newline > start && text.as_bytes()[without_newline - 1] == b'\r' {
                without_newline - 1
            } else {
                without_newline
            }
        } else {
            end
        };
        lines.push(Line {
            start,
            body_end,
            end,
        });
        start = end;
    }

    // Empty text still has one line, and a trailing newline opens one more
    // empty line that a caret can sit on.
    if lines.is_empty() || text.ends_with('\n') {
        lines.push(Line {
            start,
            body_end: start,
            end: start,
        });
    }

    lines
}

fn line_index_at(lines: &[Line], offset: usize) -> usize {
    let after = lines.partition_point(|line| line.start <= offset);
    after.saturating_sub(1)
}

fn map_offset(
    offset: usize,
    line_index: usize,
    lines: &[Line],
    new_starts: &[usize],
    old_prefixes: &[usize],
    new_prefixes: &[usize],
) -> usize {
    let line = &lines[line_index];
    let within = offset - line.start;
    let mapped_within = if within < old_prefixes[line_index] {
        new_prefixes[line_index]
    } else {
        new_prefixes[line_index] + (within - old_prefixes[line_index])
    };
    new_starts[line_index] + mapped_within
}

fn nearest_char_boundary(text: &str, index: usize) -> usize {
    let len = text.len();
    let index = index.min(len);
    if text.is_char_boundary(index) {
        return index;
    }

    let mut left = index;
    while left > 0 && !text.is_char_boundary(left) {
        left -= 1;
    }
    let mut right = index;
    while right < len && !text.is_char_boundary(right) {
        right += 1;
    }

    if index - left <= right - index {
        left
    } else {
        right
    }
}

fn clamp_range(text: &str, selection: Range<usize>) -> Range<usize> {
    let start = nearest_char_boundary(text, selection.start);
    let end = nearest_char_boundary(text, selection.end);
    if start <= end { start..end } else { end..start }
}

fn render_prefix(kind: BlockKind, number: usize) -> String {
    match kind {
        BlockKind::Paragraph => String::new(),
        BlockKind::Heading(level) => {
            let level = level.clamp(1, 6) as usize;
            let mut prefix = "#".repeat(level);
            prefix.push(' ');
            prefix
        }
        BlockKind::Bullet => "- ".to_string(),
        BlockKind::Numbered => format!("{number}. "),
        BlockKind::Quote => "> ".to_string(),
    }
}

fn prefix_at(body: &str) -> (BlockKind, usize) {
    let bytes = body.as_bytes();

    let mut hashes = 0;
    while hashes < 6 && bytes.get(hashes) == Some(&b'#') {
        hashes += 1;
    }
    if hashes > 0 && bytes.get(hashes) == Some(&b' ') {
        return (BlockKind::Heading(hashes as u8), hashes + 1);
    }

    if body.starts_with("- ") {
        return (BlockKind::Bullet, 2);
    }
    // `**bold**` must stay paragraph text; a bullet star is always "* ".
    if body.starts_with("* ") {
        return (BlockKind::Bullet, 2);
    }
    if body.starts_with("> ") {
        return (BlockKind::Quote, 2);
    }

    let mut digits = 0;
    while digits < bytes.len() && bytes[digits].is_ascii_digit() {
        digits += 1;
    }
    if digits > 0 && bytes.get(digits) == Some(&b'.') && bytes.get(digits + 1) == Some(&b' ') {
        return (BlockKind::Numbered, digits + 2);
    }

    (BlockKind::Paragraph, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_set_replace_and_remove_keeps_caret() {
        let edit = set_block("hello", 2..2, BlockKind::Heading(1));
        assert_eq!(edit.text, "# hello");
        assert_eq!(edit.selection, 4..4);

        let edit = set_block(&edit.text, edit.selection, BlockKind::Heading(2));
        assert_eq!(edit.text, "## hello");
        assert_eq!(edit.selection, 5..5);

        let edit = set_block(&edit.text, edit.selection, BlockKind::Paragraph);
        assert_eq!(edit.text, "hello");
        assert_eq!(edit.selection, 2..2);

        let edit = set_block("hello", 5..5, BlockKind::Heading(3));
        assert_eq!(edit.text, "### hello");
        assert_eq!(edit.selection, 9..9);
    }

    #[test]
    fn paragraph_removes_every_block_prefix() {
        for text in ["# one", "### one", "- one", "* one", "> one", "12. one"] {
            let edit = set_block(text, 0..text.len(), BlockKind::Paragraph);
            assert_eq!(edit.text, "one", "failed to strip prefix in {text:?}");
        }

        let edit = set_block("**bold**", 0..0, BlockKind::Paragraph);
        assert_eq!(edit.text, "**bold**");
    }

    #[test]
    fn multi_line_bullets_replace_existing_prefixes() {
        let text = "one\n* two\n3. three";
        let edit = set_block(text, 0..text.len(), BlockKind::Bullet);
        assert_eq!(edit.text, "- one\n- two\n- three");
        assert_eq!(edit.selection, 2..19);
    }

    #[test]
    fn multi_line_numbered_lists_renumber_sequentially() {
        let text = "1. one\ntwo\n3. three";
        let edit = set_block(text, 0..text.len(), BlockKind::Numbered);
        assert_eq!(edit.text, "1. one\n2. two\n3. three");
        assert_eq!(edit.selection.start, 3);

        let plain = "one\ntwo\nthree";
        let edit = set_block(plain, 4..plain.len(), BlockKind::Numbered);
        assert_eq!(edit.text, "one\n1. two\n2. three");
    }

    #[test]
    fn set_block_only_touches_selected_lines_and_preserves_content() {
        let text = "one\ntwo\nthree";
        let edit = set_block(text, 0..4, BlockKind::Bullet);
        assert_eq!(edit.text, "- one\ntwo\nthree");

        let edit = set_block(text, 4..7, BlockKind::Quote);
        assert_eq!(edit.text, "one\n> two\nthree");

        let edit = set_block("keep - and 1. and > inside", 0..0, BlockKind::Bullet);
        assert_eq!(edit.text, "- keep - and 1. and > inside");

        let edit = set_block("text   ", 0..0, BlockKind::Heading(2));
        assert_eq!(edit.text, "## text   ");
    }

    #[test]
    fn block_at_detects_each_prefix() {
        assert_eq!(block_at("# head", 0), BlockKind::Heading(1));
        assert_eq!(block_at("### head", 0), BlockKind::Heading(3));
        assert_eq!(block_at("- item", 0), BlockKind::Bullet);
        assert_eq!(block_at("* item", 0), BlockKind::Bullet);
        assert_eq!(block_at("1. item", 0), BlockKind::Numbered);
        assert_eq!(block_at("> quote", 0), BlockKind::Quote);
        assert_eq!(block_at("plain", 0), BlockKind::Paragraph);
        assert_eq!(block_at("**bold**", 0), BlockKind::Paragraph);
        assert_eq!(block_at("#nospace", 0), BlockKind::Paragraph);
        assert_eq!(block_at("", 0), BlockKind::Paragraph);
        assert_eq!(block_at("plain\n## second", 8), BlockKind::Heading(2));
        assert_eq!(block_at("plain", 99), BlockKind::Paragraph);
    }

    #[test]
    fn bold_wrap_and_unwrap_selection() {
        let edit = toggle_inline("hello world", 6..11, "**");
        assert_eq!(edit.text, "hello **world**");
        assert_eq!(edit.selection, 8..13);
        assert!(is_wrapped(&edit.text, edit.selection.clone(), "**"));

        let back = toggle_inline(&edit.text, edit.selection, "**");
        assert_eq!(back.text, "hello world");
        assert_eq!(back.selection, 6..11);
        assert!(!is_wrapped(&back.text, back.selection.clone(), "**"));
    }

    #[test]
    fn selection_including_markers_unwraps_instead_of_double_wrapping() {
        let edit = toggle_inline("a **bold** b", 2..10, "**");
        assert_eq!(edit.text, "a bold b");
        assert_eq!(edit.selection, 2..6);
    }

    #[test]
    fn supported_inline_markers_wrap_and_unwrap() {
        for (marker, wrapped) in [
            ("**", "**word**"),
            ("*", "*word*"),
            ("~~", "~~word~~"),
            ("`", "`word`"),
        ] {
            let edit = toggle_inline("word", 0..4, marker);
            assert_eq!(edit.text, wrapped);
            assert_eq!(edit.selection, marker.len()..(4 + marker.len()));

            let back = toggle_inline(&edit.text, edit.selection, marker);
            assert_eq!(back.text, "word");
            assert_eq!(back.selection, 0..4);
        }
    }

    #[test]
    fn empty_selection_wraps_with_caret_between_markers() {
        let edit = toggle_inline("hello", 5..5, "**");
        assert_eq!(edit.text, "hello****");
        assert_eq!(edit.selection, 7..7);
        assert!(is_wrapped(&edit.text, edit.selection.clone(), "**"));

        let edit = toggle_inline("hello", 2..2, "`");
        assert_eq!(edit.text, "he``llo");
        assert_eq!(edit.selection, 3..3);
    }

    #[test]
    fn bullet_star_does_not_corrupt_bold() {
        let edit = set_block("**bold**", 0..0, BlockKind::Bullet);
        assert_eq!(edit.text, "- **bold**");
        assert_eq!(edit.selection, 2..2);

        let edit = set_block("* **bold**", 0..0, BlockKind::Bullet);
        assert_eq!(edit.text, "- **bold**");

        let edit = set_block("* *italic*", 0..0, BlockKind::Bullet);
        assert_eq!(edit.text, "- *italic*");
    }

    #[test]
    fn unicode_offsets_stay_on_char_boundaries() {
        let text = "héllo 👋";
        assert_eq!(text.len(), 11);

        let edit = set_block(text, 7..11, BlockKind::Heading(2));
        assert_eq!(edit.text, "## héllo 👋");
        assert_eq!(edit.selection, 10..14);
        assert!(edit.text.is_char_boundary(edit.selection.start));
        assert!(edit.text.is_char_boundary(edit.selection.end));

        let edit = toggle_inline(text, 7..11, "**");
        assert_eq!(edit.text, "héllo **👋**");
        assert_eq!(edit.selection, 9..13);
        assert!(edit.text.is_char_boundary(edit.selection.start));
        assert!(edit.text.is_char_boundary(edit.selection.end));

        let edit = toggle_inline(text, 2..2, "~~");
        assert!(edit.text.is_char_boundary(edit.selection.start));
        assert!(edit.text.is_char_boundary(edit.selection.end));
    }

    #[test]
    fn crlf_lines_are_preserved() {
        let text = "one\r\ntwo\r\n";
        let edit = set_block(text, 0..text.len(), BlockKind::Bullet);
        assert_eq!(edit.text, "- one\r\n- two\r\n");
        assert_eq!(edit.selection, 2..14);

        let edit = set_block("one\r\ntwo", 0..5, BlockKind::Quote);
        assert_eq!(edit.text, "> one\r\ntwo");

        assert_eq!(block_at("- item\r\nplain", 9), BlockKind::Paragraph);
    }

    #[test]
    fn selection_maps_after_prefix_change() {
        let text = "alpha\nbeta\ngamma";
        let edit = set_block(text, 7..9, BlockKind::Heading(2));
        assert_eq!(edit.text, "alpha\n## beta\ngamma");
        assert_eq!(edit.selection, 10..12);

        let back = set_block(&edit.text, edit.selection, BlockKind::Paragraph);
        assert_eq!(back.text, text);
        assert_eq!(back.selection, 7..9);

        let edit = set_block(text, 0..6, BlockKind::Bullet);
        assert_eq!(edit.text, "- alpha\nbeta\ngamma");
        assert_eq!(edit.selection, 2..8);

        // Replacing a prefix keeps the selection on the line content rather
        // than on the new marker.
        let edit = set_block("- item", 0..6, BlockKind::Heading(1));
        assert_eq!(edit.text, "# item");
        assert_eq!(edit.selection, 2..6);

        let edit = set_block("# item", 0..0, BlockKind::Heading(2));
        assert_eq!(edit.text, "## item");
        assert_eq!(edit.selection, 3..3);
    }

    #[test]
    fn empty_text_and_out_of_range_offsets_do_not_panic() {
        let edit = set_block("", 0..0, BlockKind::Bullet);
        assert_eq!(edit.text, "- ");
        assert_eq!(edit.selection, 2..2);

        let edit = toggle_inline("", 0..0, "`");
        assert_eq!(edit.text, "``");
        assert_eq!(edit.selection, 1..1);
        assert!(is_wrapped(&edit.text, edit.selection.clone(), "`"));

        let edit = set_block("x", 99..99, BlockKind::Bullet);
        assert_eq!(edit.text, "- x");
        assert_eq!(edit.selection, 3..3);

        let edit = set_block("one\ntwo", 7..4, BlockKind::Bullet);
        assert_eq!(edit.text, "one\n- two");

        assert_eq!(block_at("x", 99), BlockKind::Paragraph);
    }
}
