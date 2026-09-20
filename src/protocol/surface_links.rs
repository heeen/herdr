//! Optional endpoint encoding that lets an incremental pane patch carry hyperlinks.
//!
//! `PaneSurfacePatch` has no link table, and the meaning of a patch cell's link index is not part
//! of generation 1, so a patch that touches a linked row costs a whole surface today. A client that
//! negotiates [`CAPABILITY`] instead receives the patch inside this message, together with the
//! complete link table the surface has once the patch is applied.
//!
//! Cells the patch does not cover keep the link they already had: their index is translated into
//! the new table by URI. That keeps the result identical to what a full render would have sent,
//! which is what lets the server, the client and the surface-reuse baseline stay in step.

use base64::Engine;
use serde::{Deserialize, Serialize};

use super::{FrameData, PaneSurfacePatch, PaneSurfacePatchRow, ServerMessage};

pub(crate) const CAPABILITY: &str = "surface_links";
pub(crate) const MESSAGE_KIND: &str = "endpoint.surface-links.patch.v1";

#[derive(Serialize, Deserialize)]
struct LinkedSurfacePatch {
    /// The generation-1 patch, bincode-encoded and base64'd rather than re-described in JSON:
    /// one wire shape, one set of validations, and a fraction of the size of JSON cells.
    patch: String,
    /// The surface's complete link table after the patch, in frame-build order.
    hyperlinks: Vec<String>,
}

pub(crate) fn message(
    patch: &PaneSurfacePatch,
    hyperlinks: &[String],
) -> serde_json::Result<Option<ServerMessage>> {
    let encoded = match bincode::serde::encode_to_vec(patch, bincode::config::standard()) {
        Ok(encoded) => encoded,
        Err(error) => {
            tracing::warn!(%error, "failed to encode linked surface patch");
            return Ok(None);
        }
    };
    let message = ServerMessage::EndpointControl {
        kind: MESSAGE_KIND.into(),
        data: serde_json::to_string(&LinkedSurfacePatch {
            patch: base64::engine::general_purpose::STANDARD.encode(encoded),
            hyperlinks: hyperlinks.to_vec(),
        })?,
    };
    let size = bincode::serde::encode_into_std_write(
        &message,
        &mut std::io::sink(),
        bincode::config::standard(),
    );
    match size {
        Ok(size) => Ok((size <= super::MAX_FRAME_SIZE).then_some(message)),
        Err(error) => {
            tracing::warn!(%error, "failed to size linked surface patch");
            Ok(None)
        }
    }
}

pub(crate) fn decode(data: &str) -> Result<(PaneSurfacePatch, Vec<String>), String> {
    let linked: LinkedSurfacePatch =
        serde_json::from_str(data).map_err(|error| format!("invalid linked patch: {error}"))?;
    let encoded = base64::engine::general_purpose::STANDARD
        .decode(&linked.patch)
        .map_err(|error| format!("invalid linked patch payload: {error}"))?;
    let (patch, read) = bincode::serde::decode_from_slice::<PaneSurfacePatch, _>(
        &encoded,
        bincode::config::standard(),
    )
    .map_err(|error| format!("invalid linked patch body: {error}"))?;
    if read != encoded.len() {
        return Err("linked patch body has trailing bytes".into());
    }
    Ok((patch, linked.hyperlinks))
}

/// Applies a linked patch to `frame`, leaving it exactly as a full render would have built it.
///
/// Nothing is written unless the whole patch checks out, so a rejected patch leaves the frame
/// usable and the caller can ask for a full surface instead.
pub(crate) fn apply(
    frame: &mut FrameData,
    rows: &[PaneSurfacePatchRow],
    hyperlinks: Vec<String>,
) -> Result<(), &'static str> {
    let FrameData {
        width,
        height,
        cells: frame_cells,
        hyperlinks: frame_hyperlinks,
        ..
    } = frame;
    let (width, height) = (usize::from(*width), *height);
    let mut covered = vec![false; frame_cells.len()];
    for row in rows {
        if row.y >= height || usize::from(row.x) + row.cells.len() > width {
            return Err("linked patch row leaves the frame");
        }
        if row
            .cells
            .iter()
            .any(|cell| cell.hyperlink.is_some() && uri_at(&hyperlinks, cell.hyperlink).is_none())
        {
            return Err("linked patch cell points outside its table");
        }
        let start = usize::from(row.y) * width + usize::from(row.x);
        covered[start..start + row.cells.len()].fill(true);
    }

    // Cells the patch does not rewrite keep their uri, so their index has to be translated.
    let remap = frame_hyperlinks
        .iter()
        .map(|uri| {
            hyperlinks
                .iter()
                .position(|candidate| candidate == uri)
                .and_then(|index| u32::try_from(index).ok())
        })
        .collect::<Vec<_>>();
    let translate = |link: Option<u32>| remap.get(usize::try_from(link?).ok()?).copied().flatten();
    let stranded = frame_cells.iter().zip(&covered).any(|(cell, covered)| {
        !covered && cell.hyperlink.is_some() && translate(cell.hyperlink).is_none()
    });
    if stranded {
        return Err("linked patch drops a uri that surviving cells still use");
    }

    for (cell, _) in frame_cells
        .iter_mut()
        .zip(&covered)
        .filter(|(_, covered)| !**covered)
    {
        cell.hyperlink = translate(cell.hyperlink);
    }
    for row in rows {
        let start = usize::from(row.y) * width + usize::from(row.x);
        frame_cells[start..start + row.cells.len()].clone_from_slice(&row.cells);
    }
    *frame_hyperlinks = hyperlinks;
    Ok(())
}

/// The uri a link index stands for in `table`, if it points inside it.
pub(crate) fn uri_at(table: &[String], index: Option<u32>) -> Option<&str> {
    table.get(usize::try_from(index?).ok()?).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CellData;

    fn frame(cells: Vec<CellData>, hyperlinks: Vec<String>) -> FrameData {
        FrameData {
            width: u16::try_from(cells.len()).unwrap(),
            height: 1,
            cells,
            cursor: None,
            hyperlinks,
            graphics: Vec::new(),
        }
    }

    fn patch(rows: Vec<PaneSurfacePatchRow>) -> PaneSurfacePatch {
        PaneSurfacePatch {
            boot_id: "boot".into(),
            projection_revision: 1,
            base_surface_revision: 4,
            surface_revision: 5,
            rows,
            panes: Vec::new(),
            cursor: None,
        }
    }

    #[test]
    fn a_linked_patch_round_trips_through_its_control_message() {
        let sent = patch(vec![PaneSurfacePatchRow {
            x: 1,
            y: 0,
            cells: vec![CellData::test_linked("a", Some(1))],
        }]);
        let table = vec!["https://a.test".to_owned(), "https://b.test".to_owned()];

        let ServerMessage::EndpointControl { kind, data } = message(&sent, &table)
            .unwrap()
            .expect("within the size limit")
        else {
            panic!("linked patches travel as endpoint controls");
        };

        assert_eq!(kind, MESSAGE_KIND);
        assert_eq!(decode(&data).unwrap(), (sent, table));
    }

    #[test]
    fn a_linked_patch_tolerates_future_fields_and_rejects_damaged_payloads() {
        let encoded = bincode::serde::encode_to_vec(patch(Vec::new()), bincode::config::standard())
            .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
            .unwrap();
        let future = format!(
            r#"{{"patch":"{encoded}","hyperlinks":["https://a.test"],"compression":"none"}}"#
        );
        assert_eq!(
            decode(&future).unwrap().1,
            ["https://a.test".to_owned()],
            "an unknown field must not fail the decode"
        );

        let trailing = format!(
            r#"{{"patch":"{}","hyperlinks":[]}}"#,
            base64::engine::general_purpose::STANDARD.encode(
                bincode::serde::encode_to_vec(patch(Vec::new()), bincode::config::standard())
                    .map(|mut bytes| {
                        bytes.push(0);
                        bytes
                    })
                    .unwrap()
            )
        );
        assert!(decode(&trailing).is_err(), "trailing bytes are rejected");
        assert!(decode(r#"{"patch":"not base64!","hyperlinks":[]}"#).is_err());
        assert!(decode("not json").is_err());
    }

    #[test]
    fn applying_a_patch_reindexes_the_cells_it_does_not_cover() {
        // "a" is being overwritten, so the surviving "b" cell has to follow its uri to index 0.
        let mut target = frame(
            vec![
                CellData::test_linked("x", Some(0)),
                CellData::test_linked("y", Some(0)),
                CellData::test_linked("z", Some(1)),
            ],
            vec!["https://a.test".to_owned(), "https://b.test".to_owned()],
        );

        apply(
            &mut target,
            &[PaneSurfacePatchRow {
                x: 0,
                y: 0,
                cells: vec![
                    CellData::test_linked("p", None),
                    CellData::test_linked("q", None),
                ],
            }],
            vec!["https://b.test".to_owned()],
        )
        .unwrap();

        assert_eq!(
            target
                .cells
                .iter()
                .map(|cell| cell.hyperlink)
                .collect::<Vec<_>>(),
            [None, None, Some(0)]
        );
        assert_eq!(
            target
                .cells
                .iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<Vec<_>>(),
            ["p", "q", "z"]
        );
        assert_eq!(target.hyperlinks, ["https://b.test"]);
    }

    #[test]
    fn a_patch_that_would_strand_a_surviving_link_is_rejected_whole() {
        let original = frame(
            vec![
                CellData::test_linked("x", Some(0)),
                CellData::test_linked("z", Some(1)),
            ],
            vec!["https://a.test".to_owned(), "https://b.test".to_owned()],
        );
        let mut target = original.clone();

        // The surviving "z" cell still needs b.test, which the new table drops.
        let error = apply(
            &mut target,
            &[PaneSurfacePatchRow {
                x: 0,
                y: 0,
                cells: vec![CellData::test_linked("p", Some(0))],
            }],
            vec!["https://a.test".to_owned()],
        )
        .unwrap_err();

        assert!(error.contains("drops a uri"));
        assert_eq!(target, original, "a rejected patch must not half-apply");
    }

    #[test]
    fn a_patch_cell_outside_its_table_or_frame_is_rejected() {
        let original = frame(vec![CellData::test_linked("x", None)], Vec::new());

        let mut target = original.clone();
        assert!(apply(
            &mut target,
            &[PaneSurfacePatchRow {
                x: 0,
                y: 0,
                cells: vec![CellData::test_linked("p", Some(0))],
            }],
            Vec::new(),
        )
        .is_err());
        assert_eq!(target, original);

        let mut target = original.clone();
        assert!(apply(
            &mut target,
            &[PaneSurfacePatchRow {
                x: 0,
                y: 3,
                cells: vec![CellData::test_linked("p", None)],
            }],
            Vec::new(),
        )
        .is_err());
        assert_eq!(target, original);
    }
}
