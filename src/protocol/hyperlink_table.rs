//! The rules that turn a pane's visible hyperlinks into a frame's link table.
//!
//! A full render and an incremental patch must agree on which cell carries which link and on the
//! order of the table, or a patched frame would differ from the one a full render sends. Both
//! build through these two pieces.

use std::collections::HashMap;

use crate::pane::VisibleHyperlink;

/// Where each link lands: the last entry for a position wins, and it only applies if its symbol
/// matches the cell the renderer drew there.
pub(crate) fn resolve_placements(links: &[VisibleHyperlink]) -> HashMap<(u16, u16), (&str, &str)> {
    links
        .iter()
        .map(|link| (link.position, (link.symbol.as_str(), link.uri.as_ref())))
        .collect()
}

/// A frame's link table, numbered in the order links are first met while walking cells row by row.
#[derive(Debug, Default)]
pub(crate) struct HyperlinkTable {
    uris: Vec<String>,
    index: HashMap<String, u32>,
}

impl HyperlinkTable {
    /// The index for `uri`, adding it at the end when it is new.
    pub(crate) fn intern(&mut self, uri: &str) -> u32 {
        if let Some(index) = self.index.get(uri) {
            return *index;
        }
        let index = u32::try_from(self.uris.len()).unwrap_or(u32::MAX);
        self.uris.push(uri.to_owned());
        self.index.insert(uri.to_owned(), index);
        index
    }

    pub(crate) fn into_uris(self) -> Vec<String> {
        self.uris
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(x: u16, symbol: &str, uri: &str) -> VisibleHyperlink {
        VisibleHyperlink {
            position: (x, 0),
            symbol: symbol.to_owned(),
            uri: uri.into(),
        }
    }

    #[test]
    fn the_last_link_for_a_position_wins() {
        let links = [
            link(0, "a", "https://first.test"),
            link(0, "a", "https://second.test"),
        ];
        assert_eq!(
            resolve_placements(&links).get(&(0, 0)),
            Some(&("a", "https://second.test"))
        );
    }

    #[test]
    fn a_table_numbers_uris_by_first_appearance_without_duplicates() {
        let mut table = HyperlinkTable::default();
        assert_eq!(table.intern("https://b.test"), 0);
        assert_eq!(table.intern("https://a.test"), 1);
        assert_eq!(table.intern("https://b.test"), 0);
        assert_eq!(table.into_uris(), ["https://b.test", "https://a.test"]);
    }
}
