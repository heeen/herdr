//! Finding http and https URLs in terminal text.
//!
//! One rule serves every path that acts on a URL: Ctrl+click activation, hover highlighting, and
//! the hyperlinks herdr hands to the outer terminal. They must agree, or herdr would underline
//! text that clicking will not open.
//!
//! A URL runs from its scheme to the next whitespace, minus trailing punctuation that reads as
//! sentence punctuation rather than part of the address.

/// One character of terminal text with the display columns it occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextCell {
    pub(crate) ch: char,
    pub(crate) start_col: u16,
    pub(crate) end_col: u16,
}

/// An inclusive range of [`TextCell`] indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl CellSpan {
    pub(crate) fn contains(self, idx: usize) -> bool {
        idx >= self.start && idx <= self.end
    }

    pub(crate) fn columns(self, cells: &[TextCell]) -> (u16, u16) {
        (cells[self.start].start_col, cells[self.end].end_col)
    }
}

pub(crate) fn safe_web_url(url: &str) -> Option<&str> {
    (url.starts_with("http://") || url.starts_with("https://")).then_some(url)
}

pub(crate) fn text_cells(row: &str) -> Vec<TextCell> {
    let mut cells = Vec::new();
    text_cells_into(row, &mut cells);
    cells
}

/// Fills a caller-owned buffer so a repeated scan does not reallocate per row.
pub(crate) fn text_cells_into(row: &str, cells: &mut Vec<TextCell>) {
    cells.clear();
    let mut next_col = 0u16;
    cells.extend(row.chars().map(|ch| {
        let width = u16::from(crate::ghostty::unicode_codepoint_width(ch as u32));
        let start_col = if width == 0 {
            next_col.saturating_sub(1)
        } else {
            next_col
        };
        if width > 0 {
            next_col = next_col.saturating_add(width);
        }
        TextCell {
            ch,
            start_col,
            end_col: next_col.saturating_sub(1),
        }
    }));
}

/// Every URL in `cells`, in order and non-overlapping.
///
/// A candidate that trims away to nothing is skipped rather than ending the scan.
pub(crate) fn url_cell_spans(cells: &[TextCell]) -> impl Iterator<Item = CellSpan> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        while start < cells.len() {
            if !starts_with_chars(&cells[start..], "http://")
                && !starts_with_chars(&cells[start..], "https://")
            {
                start += 1;
                continue;
            }
            let mut end = start;
            while end + 1 < cells.len() && !cells[end + 1].ch.is_whitespace() {
                end += 1;
            }
            let trimmed = trim_url_edges(cells, CellSpan { start, end });
            start = end + 1;
            if trimmed.is_some() {
                return trimmed;
            }
        }
        None
    })
}

/// The URL covering `clicked_idx`, if any.
pub(crate) fn url_span_at_column(cells: &[TextCell], clicked_idx: usize) -> Option<CellSpan> {
    url_cell_spans(cells).find(|span| span.contains(clicked_idx))
}

/// Byte ranges of every URL in `text`, whose cells are `cells`.
pub(crate) fn url_byte_spans<'a>(
    text: &'a str,
    cells: &'a [TextCell],
) -> impl Iterator<Item = std::ops::Range<usize>> + 'a {
    // Spans arrive in order, so one forward-only cursor over the text serves them all instead of
    // counting characters from the start for each span.
    let mut chars = text.char_indices().peekable();
    let mut cursor = 0usize;
    let mut byte_of_char = move |target: usize| -> usize {
        while cursor < target {
            if chars.next().is_none() {
                return text.len();
            }
            cursor += 1;
        }
        chars.peek().map_or(text.len(), |(byte, _)| *byte)
    };
    url_cell_spans(cells).filter_map(move |span| {
        let start = byte_of_char(span.start);
        let end = byte_of_char(span.end + 1);
        safe_web_url(text.get(start..end)?)?;
        Some(start..end)
    })
}

fn trim_url_edges(cells: &[TextCell], span: CellSpan) -> Option<CellSpan> {
    let start = span.start;
    let mut end = span.end;
    while start <= end && should_trim_trailing_url_cell(cells, start, end) {
        if end == 0 {
            return None;
        }
        end -= 1;
    }
    (start <= end).then_some(CellSpan { start, end })
}

fn should_trim_trailing_url_cell(cells: &[TextCell], start: usize, end: usize) -> bool {
    match cells[end].ch {
        '"' | '\'' | '`' | '.' | ',' | ';' | ':' | '!' | '?' => true,
        ')' => !trailing_url_closer_is_balanced(cells, start, end, '(', ')'),
        ']' => !trailing_url_closer_is_balanced(cells, start, end, '[', ']'),
        '}' => !trailing_url_closer_is_balanced(cells, start, end, '{', '}'),
        _ => false,
    }
}

fn trailing_url_closer_is_balanced(
    cells: &[TextCell],
    start: usize,
    end: usize,
    open: char,
    close: char,
) -> bool {
    let mut balance = 0i32;
    for cell in &cells[start..end] {
        if cell.ch == open {
            balance += 1;
        } else if cell.ch == close {
            balance -= 1;
        }
    }
    balance > 0
}

pub(crate) fn starts_with_chars(cells: &[TextCell], prefix: &str) -> bool {
    prefix
        .chars()
        .enumerate()
        .all(|(idx, expected)| cells.get(idx).is_some_and(|cell| cell.ch == expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str) -> Vec<&str> {
        let cells = text_cells(text);
        url_byte_spans(text, &cells)
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn urls_enumerate_in_order_without_overlapping() {
        assert_eq!(
            spans("see https://example.com/a and http://b.test/c now"),
            ["https://example.com/a", "http://b.test/c"]
        );
        assert_eq!(spans("no links here"), Vec::<&str>::new());
    }

    #[test]
    fn trailing_punctuation_and_unbalanced_brackets_are_trimmed() {
        assert_eq!(
            spans("https://example.com/a(b)c. and https://x.test/y!"),
            ["https://example.com/a(b)c", "https://x.test/y"]
        );
        // A closer that balances an opener inside the url stays part of it.
        assert_eq!(spans("(https://example.com/a)"), ["https://example.com/a"]);
        assert_eq!(
            spans("https://example.com/wiki/Foo_(bar)"),
            ["https://example.com/wiki/Foo_(bar)"]
        );
    }

    #[test]
    fn a_trimmed_candidate_does_not_end_the_scan() {
        // Pinned as-is: trimming the trailing "." leaves a bare scheme, which still counts as a
        // url. Ctrl+click has always resolved it that way; what matters here is that a later url
        // is still found after it.
        assert_eq!(
            spans("https://. https://example.com"),
            ["https://", "https://example.com"]
        );
    }

    #[test]
    fn only_http_and_https_are_urls() {
        assert_eq!(
            spans("ftp://a ssh://b mailto:c@d file:///e"),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn a_scheme_is_matched_wherever_it_starts() {
        // Pinned deliberately: the scan looks for the scheme at any offset, so text glued to a url
        // still yields the url. Ctrl+click has always behaved this way.
        assert_eq!(spans("xhttps://example.com"), ["https://example.com"]);
    }

    #[test]
    fn enumeration_agrees_with_the_span_at_a_clicked_index() {
        // `url_span_at_column` is the enumeration plus a `find`, and this proves the reduction over
        // every index of every sample, including indices inside trimmed-away tails.
        for text in [
            "see https://example.com/a and http://b.test/c now",
            "https://example.com/a(b)c. trailing",
            "https://. https://example.com",
            "xhttps://example.com",
            "https://example.com/wiki/Foo_(bar))",
            "no links here",
            "   ",
        ] {
            let cells = text_cells(text);
            for idx in 0..cells.len() {
                assert_eq!(
                    url_span_at_column(&cells, idx),
                    url_cell_spans(&cells).find(|span| span.contains(idx)),
                    "{text:?} at {idx}"
                );
            }
        }
    }

    #[test]
    fn byte_spans_survive_multibyte_and_wide_characters() {
        let text = "über https://example.com/路径/x ende";
        assert_eq!(spans(text), ["https://example.com/路径/x"]);
        let cells = text_cells(text);
        let range = url_byte_spans(text, &cells).next().unwrap();
        assert_eq!(&text[range.clone()], "https://example.com/路径/x");
        assert!(text.is_char_boundary(range.start) && text.is_char_boundary(range.end));
    }
}
