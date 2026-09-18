//! Finding the URLs a pane is showing, so they can be handed to the outer terminal as hyperlinks.
//!
//! A URL is one logical line's worth of text, which the terminal may have wrapped over several
//! rows, and which may start above the viewport or continue below it. The scan therefore collects
//! the text of the whole logical line while remembering which cell each byte came from; rows
//! outside the viewport contribute text but no cells.

use std::sync::Arc;

use crate::url_scan::{text_cells_into, url_byte_spans, TextCell};

/// The cell a run of scan text came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScanCell {
    pub(super) x: u16,
    pub(super) y: u16,
    /// A wide cell also covers the column to its right.
    pub(super) wide: bool,
    /// The program already marked this cell with its own OSC 8 link.
    pub(super) program_linked: bool,
}

#[derive(Debug)]
struct ScanSpan {
    byte_start: usize,
    byte_end: usize,
    cell: Option<ScanCell>,
}

/// One cell of a detected URL, ready to become a [`super::VisibleHyperlink`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DetectedLinkCell {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) symbol: String,
    pub(super) uri: Arc<str>,
}

#[derive(Debug, Default)]
pub(super) struct ViewportScan {
    text: String,
    spans: Vec<ScanSpan>,
}

impl ViewportScan {
    pub(super) fn with_capacity(cells: usize) -> Self {
        Self {
            text: String::with_capacity(cells),
            spans: Vec::with_capacity(cells),
        }
    }

    /// Appends one visible cell's text exactly as the renderer draws it.
    pub(super) fn push_cell(&mut self, symbol: &str, cell: ScanCell) {
        self.push_span(symbol, Some(cell));
    }

    /// Appends text from a row outside the viewport, which completes a URL without being clickable.
    pub(super) fn push_offscreen(&mut self, text: &str) {
        self.push_span(text, None);
    }

    /// Ends a logical line. Soft-wrapped rows are not separated, so a URL survives the wrap.
    pub(super) fn push_line_break(&mut self) {
        self.push_span("\n", None);
    }

    fn push_span(&mut self, text: &str, cell: Option<ScanCell>) {
        if text.is_empty() {
            return;
        }
        let byte_start = self.text.len();
        self.text.push_str(text);
        self.spans.push(ScanSpan {
            byte_start,
            byte_end: self.text.len(),
            cell,
        });
    }

    /// Moves text collected for rows above the viewport in front of everything scanned so far.
    pub(super) fn prepend_offscreen(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let shift = text.len();
        self.text.insert_str(0, text);
        for span in &mut self.spans {
            span.byte_start += shift;
            span.byte_end += shift;
        }
        self.spans.insert(
            0,
            ScanSpan {
                byte_start: 0,
                byte_end: shift,
                cell: None,
            },
        );
    }

    /// Whether the scan can contain a URL at all. Cheap enough to run before building cells.
    pub(super) fn may_contain_url(&self) -> bool {
        self.text.contains("http")
    }

    /// Every cell of every URL on screen.
    ///
    /// A URL touching a cell the program already linked is skipped: the frame table keeps the last
    /// entry for a cell, so a detected link would otherwise replace the program's own.
    pub(super) fn detected_links(&self, cells: &mut Vec<TextCell>) -> Vec<DetectedLinkCell> {
        if !self.may_contain_url() {
            return Vec::new();
        }
        text_cells_into(&self.text, cells);
        let mut detected = Vec::new();
        for range in url_byte_spans(&self.text, cells) {
            let covered = self
                .spans
                .iter()
                .filter(|span| span.byte_start < range.end && span.byte_end > range.start);
            let uri: Arc<str> = Arc::from(&self.text[range.clone()]);
            let mut url_cells = Vec::new();
            let mut program_linked = false;
            for span in covered {
                let Some(cell) = span.cell else {
                    continue;
                };
                if cell.program_linked {
                    program_linked = true;
                    break;
                }
                url_cells.push(DetectedLinkCell {
                    x: cell.x,
                    y: cell.y,
                    symbol: self.text[span.byte_start..span.byte_end].to_owned(),
                    uri: Arc::clone(&uri),
                });
                if cell.wide {
                    // The spacer the renderer leaves beside a wide character carries the link too,
                    // so the outer terminal does not see a gap mid-URL.
                    url_cells.push(DetectedLinkCell {
                        x: cell.x + 1,
                        y: cell.y,
                        symbol: String::new(),
                        uri: Arc::clone(&uri),
                    });
                }
            }
            if !program_linked {
                detected.append(&mut url_cells);
            }
        }
        detected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(x: u16, y: u16) -> ScanCell {
        ScanCell {
            x,
            y,
            wide: false,
            program_linked: false,
        }
    }

    /// Lays `text` out from `(x, y)`, one cell per character.
    fn push_row(scan: &mut ViewportScan, text: &str, x: u16, y: u16) {
        for (offset, ch) in text.chars().enumerate() {
            let x = x + u16::try_from(offset).unwrap();
            scan.push_cell(&ch.to_string(), cell(x, y));
        }
    }

    fn positions(detected: &[DetectedLinkCell]) -> Vec<(u16, u16)> {
        detected.iter().map(|link| (link.x, link.y)).collect()
    }

    #[test]
    fn a_wrapped_url_links_every_row_it_covers() {
        let mut scan = ViewportScan::default();
        push_row(&mut scan, "see https://ex", 0, 0);
        push_row(&mut scan, "ample.com/a b", 0, 1);

        let detected = scan.detected_links(&mut Vec::new());

        assert!(detected
            .iter()
            .all(|link| link.uri.as_ref() == "https://example.com/a"));
        assert_eq!(
            positions(&detected),
            [
                (4, 0),
                (5, 0),
                (6, 0),
                (7, 0),
                (8, 0),
                (9, 0),
                (10, 0),
                (11, 0),
                (12, 0),
                (13, 0),
                (0, 1),
                (1, 1),
                (2, 1),
                (3, 1),
                (4, 1),
                (5, 1),
                (6, 1),
                (7, 1),
                (8, 1),
                (9, 1),
                (10, 1),
            ],
            "the link covers the tail of the first row and the head of the second"
        );
    }

    #[test]
    fn a_line_break_separates_urls() {
        let mut scan = ViewportScan::default();
        push_row(&mut scan, "https://a.test", 0, 0);
        scan.push_line_break();
        push_row(&mut scan, "https://b.test", 0, 1);

        let detected = scan.detected_links(&mut Vec::new());

        let uris: Vec<&str> = detected
            .iter()
            .map(|link| link.uri.as_ref())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        assert_eq!(uris, ["https://a.test", "https://b.test"]);
    }

    #[test]
    fn text_above_the_viewport_completes_the_url_without_linking() {
        let mut scan = ViewportScan::default();
        push_row(&mut scan, "ample.com/deep", 0, 0);
        scan.prepend_offscreen("https://ex");

        let detected = scan.detected_links(&mut Vec::new());

        assert!(detected
            .iter()
            .all(|link| link.uri.as_ref() == "https://example.com/deep"));
        assert_eq!(
            positions(&detected).first(),
            Some(&(0, 0)),
            "only visible cells are linked, and the uri is still the whole url"
        );
        assert_eq!(detected.len(), 14);
    }

    #[test]
    fn text_below_the_viewport_completes_the_url_without_linking() {
        let mut scan = ViewportScan::default();
        push_row(&mut scan, "https://exampl", 0, 0);
        scan.push_offscreen("e.com/deep");

        let detected = scan.detected_links(&mut Vec::new());

        assert!(detected
            .iter()
            .all(|link| link.uri.as_ref() == "https://example.com/deep"));
        assert_eq!(detected.len(), 14);
    }

    #[test]
    fn a_wide_character_also_links_its_spacer_column() {
        let mut scan = ViewportScan::default();
        push_row(&mut scan, "https://a.test/", 0, 0);
        scan.push_cell(
            "路",
            ScanCell {
                x: 15,
                y: 0,
                wide: true,
                program_linked: false,
            },
        );

        let detected = scan.detected_links(&mut Vec::new());

        assert_eq!(
            detected.last(),
            Some(&DetectedLinkCell {
                x: 16,
                y: 0,
                symbol: String::new(),
                uri: "https://a.test/路".into(),
            })
        );
    }

    #[test]
    fn a_url_the_program_already_linked_is_left_alone() {
        let mut scan = ViewportScan::default();
        scan.push_cell(
            "h",
            ScanCell {
                x: 0,
                y: 0,
                wide: false,
                program_linked: true,
            },
        );
        push_row(&mut scan, "ttps://a.test", 1, 0);

        assert!(scan.detected_links(&mut Vec::new()).is_empty());
    }

    #[test]
    fn text_without_a_scheme_is_skipped_before_any_work() {
        let mut scan = ViewportScan::default();
        push_row(&mut scan, "nothing to see here", 0, 0);

        assert!(!scan.may_contain_url());
        assert!(scan.detected_links(&mut Vec::new()).is_empty());
    }
}
