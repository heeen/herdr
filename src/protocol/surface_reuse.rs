//! Optional endpoint encoding that retains unchanged terminal cells across projections.

use super::{surface_links, CellData, FrameData, PaneSurfaceFrame, ServerMessage};
use serde::{Deserialize, Serialize};

pub(crate) const CAPABILITY: &str = "surface_reuse";
pub(crate) const MESSAGE_KIND: &str = "endpoint.surface-reuse.v1";

#[derive(Serialize, Deserialize)]
struct SurfaceReuse<S> {
    base_surface_revision: u64,
    surface: S,
}

pub(crate) fn message(
    base_surface_revision: u64,
    surface: &mut PaneSurfaceFrame,
) -> serde_json::Result<Option<ServerMessage>> {
    let cells = std::mem::take(&mut surface.frame.cells);
    let data = serde_json::to_string(&SurfaceReuse {
        base_surface_revision,
        surface: &*surface,
    });
    surface.frame.cells = cells;
    let message = ServerMessage::EndpointControl {
        kind: MESSAGE_KIND.into(),
        data: data?,
    };
    // JSON can expand non-cell data (for example escaped hyperlink URLs). A
    // failed compact encoding must fall back, not strand a newer snapshot.
    let size = bincode::serde::encode_into_std_write(
        &message,
        &mut std::io::sink(),
        bincode::config::standard(),
    );
    match size {
        Ok(size) => Ok((size <= super::MAX_FRAME_SIZE).then_some(message)),
        Err(error) => {
            tracing::warn!(%error, "failed to size surface reuse");
            Ok(None)
        }
    }
}

#[derive(Default)]
struct CellBaseline {
    boot_id: String,
    projection_revision: u64,
    surface_revision: u64,
    /// Only the geometry, cells and link table are kept in step; a reuse message brings its own
    /// cursor and graphics. The table is what lets cells handed back still mean the same links
    /// after a linked patch replaced it.
    frame: FrameData,
    popup: Option<PopupBaseline>,
}

impl CellBaseline {
    /// Whether a patch applies to exactly this baseline as its next revision.
    fn matches_patch(&self, patch: &super::PaneSurfacePatch) -> bool {
        patch.boot_id == self.boot_id
            && patch.projection_revision == self.projection_revision
            && patch.base_surface_revision == self.surface_revision
            && patch.surface_revision == self.surface_revision.saturating_add(1)
    }
}

struct PopupBaseline {
    terminal_id: String,
    width: u16,
    height: u16,
    cells: Vec<CellData>,
}

fn popup_baseline(surface: &PaneSurfaceFrame) -> Option<PopupBaseline> {
    surface.popup.as_ref().map(|popup| PopupBaseline {
        terminal_id: popup.terminal_id.clone(),
        width: popup.frame.width,
        height: popup.frame.height,
        cells: popup.frame.cells.clone(),
    })
}

/// Connection-local decoding happens before activation and presentation filtering, so
/// switching endpoints cannot discard a baseline needed by the next wire message.
#[derive(Default)]
pub(crate) struct Decoder {
    baseline: Option<CellBaseline>,
    surface_delta: bool,
    surface_scroll: bool,
}

impl Decoder {
    pub(crate) fn new(surface_delta: bool, surface_scroll: bool) -> Self {
        Self {
            baseline: None,
            surface_delta,
            surface_scroll,
        }
    }

    pub(crate) fn decode(&mut self, message: ServerMessage) -> Result<ServerMessage, String> {
        let message = match message {
            ServerMessage::EndpointControl { kind, data }
                if kind == super::surface_delta::MESSAGE_KIND =>
            {
                if !self.surface_delta {
                    return Err("surface delta was not negotiated".into());
                }
                return self.decode_delta(&data).map(ServerMessage::PaneSurface);
            }
            ServerMessage::EndpointControl { kind, data }
                if kind == super::surface_scroll::MESSAGE_KIND =>
            {
                if !self.surface_scroll {
                    return Err("surface scroll was not negotiated".into());
                }
                return self
                    .decode_scroll(&data)
                    .map(ServerMessage::PaneSurfacePatch);
            }
            ServerMessage::EndpointControl { kind, data } if kind == MESSAGE_KIND => {
                let reuse: SurfaceReuse<PaneSurfaceFrame> = serde_json::from_str(&data)
                    .map_err(|error| format!("invalid surface reuse: {error}"))?;
                let Some(base) = &mut self.baseline else {
                    return Err("surface reuse without a baseline".into());
                };
                let mut surface = reuse.surface;
                if base.boot_id != surface.boot_id
                    || base.surface_revision != reuse.base_surface_revision
                    || surface.surface_revision != base.surface_revision.saturating_add(1)
                    || base.frame.width != surface.frame.width
                    || base.frame.height != surface.frame.height
                    || base.frame.hyperlinks != surface.frame.hyperlinks
                    || !surface.frame.cells.is_empty()
                {
                    return Err("surface reuse does not match its baseline".into());
                }
                surface.frame.cells.clone_from(&base.frame.cells);
                base.projection_revision = surface.projection_revision;
                base.surface_revision = surface.surface_revision;
                if self.surface_delta {
                    base.popup = popup_baseline(&surface);
                }
                return Ok(ServerMessage::PaneSurface(surface));
            }
            ServerMessage::EndpointControl { kind, data }
                if kind == surface_links::MESSAGE_KIND =>
            {
                let (patch, hyperlinks) = surface_links::decode(&data)?;
                // A patch the baseline cannot take is not this decoder's to refuse: the shell
                // applies the same patch under the same rules and asks for a full surface when
                // it fails, at which point a fresh baseline arrives with it.
                let applied = self.baseline.as_mut().is_some_and(|base| {
                    base.matches_patch(&patch)
                        && surface_links::apply(&mut base.frame, &patch.rows, hyperlinks).is_ok()
                });
                if let Some(base) = self.baseline.as_mut().filter(|_| applied) {
                    base.surface_revision = patch.surface_revision;
                } else {
                    self.baseline = None;
                }
                return Ok(ServerMessage::EndpointControl { kind, data });
            }
            message => message,
        };
        match &message {
            ServerMessage::PaneSurface(surface) => {
                let base = self.baseline.get_or_insert_with(CellBaseline::default);
                base.boot_id.clone_from(&surface.boot_id);
                base.projection_revision = surface.projection_revision;
                base.surface_revision = surface.surface_revision;
                base.frame.width = surface.frame.width;
                base.frame.height = surface.frame.height;
                base.frame.cells.clone_from(&surface.frame.cells);
                base.frame.hyperlinks.clone_from(&surface.frame.hyperlinks);
                if self.surface_delta {
                    base.popup = popup_baseline(surface);
                }
            }
            ServerMessage::PaneSurfacePatch(patch) => {
                if let Some(base) = &mut self.baseline {
                    if !base.matches_patch(patch) {
                        self.baseline = None;
                    } else {
                        let frame = &mut base.frame;
                        for row in &patch.rows {
                            let start =
                                usize::from(row.y) * usize::from(frame.width) + usize::from(row.x);
                            let end = start.saturating_add(row.cells.len());
                            if row.y >= frame.height
                                || usize::from(row.x) + row.cells.len() > usize::from(frame.width)
                                || end > frame.cells.len()
                            {
                                return Err("surface patch exceeds the cell baseline".into());
                            }
                            frame.cells[start..end].clone_from_slice(&row.cells);
                        }
                        base.surface_revision = patch.surface_revision;
                    }
                }
            }
            _ => {}
        }
        Ok(message)
    }

    fn decode_scroll(&mut self, data: &str) -> Result<super::PaneSurfacePatch, String> {
        let Some(base) = &mut self.baseline else {
            return Err("surface scroll without a baseline".into());
        };
        let scroll = super::surface_scroll::decode(data)?;
        if !base.matches_patch(&scroll.patch) {
            return Err("surface scroll does not match its baseline".into());
        }
        let patch = super::surface_scroll::apply(
            &mut base.frame.cells,
            base.frame.width,
            base.frame.height,
            scroll,
        )?;
        base.surface_revision = patch.surface_revision;
        Ok(patch)
    }

    fn decode_delta(&mut self, data: &str) -> Result<PaneSurfaceFrame, String> {
        use super::surface_delta::{self, GridUpdate};
        let Some(base) = &mut self.baseline else {
            return Err("surface delta without a baseline".into());
        };
        let delta = surface_delta::decode_for(data, (base.frame.width, base.frame.height))?;
        let mut surface = delta.surface;
        if surface.boot_id != base.boot_id
            || delta.base_projection_revision != base.projection_revision
            || delta.base_surface_revision != base.surface_revision
            || surface.surface_revision != base.surface_revision.saturating_add(1)
            || surface.projection_revision < base.projection_revision
            || base.frame.cells.len()
                != usize::from(base.frame.width) * usize::from(base.frame.height)
        {
            return Err("surface delta does not match its baseline".into());
        }
        surface.frame.cells.clone_from(&base.frame.cells);
        surface_delta::apply_rows(&mut surface.frame.cells, base.frame.width, &delta.rows);
        match (&mut surface.popup, delta.popup_cells) {
            (None, None) => {}
            (Some(popup), Some(update)) => {
                let count = usize::from(popup.frame.width) * usize::from(popup.frame.height);
                match update {
                    GridUpdate::Replace(cells) => popup.frame.cells = cells,
                    GridUpdate::Patch(rows) => {
                        let previous = base
                            .popup
                            .as_ref()
                            .filter(|previous| {
                                previous.terminal_id == popup.terminal_id
                                    && previous.width == popup.frame.width
                                    && previous.height == popup.frame.height
                                    && previous.cells.len() == count
                            })
                            .ok_or("popup delta does not match its baseline")?;
                        popup.frame.cells.clone_from(&previous.cells);
                        surface_delta::apply_rows(&mut popup.frame.cells, previous.width, &rows);
                    }
                }
            }
            _ => return Err("popup delta is missing or unexpected".into()),
        }
        for frame in
            std::iter::once(&surface.frame).chain(surface.popup.as_ref().map(|popup| &popup.frame))
        {
            if frame.cells.iter().any(|cell| {
                cell.hyperlink
                    .is_some_and(|index| index as usize >= frame.hyperlinks.len())
            }) {
                return Err("surface delta has an invalid hyperlink index".into());
            }
        }
        // Validate the entire update before advancing either grid or revision.
        surface_delta::apply_rows(&mut base.frame.cells, base.frame.width, &delta.rows);
        base.popup = popup_baseline(&surface);
        base.frame.hyperlinks.clone_from(&surface.frame.hyperlinks);
        base.projection_revision = surface.projection_revision;
        base.surface_revision = surface.surface_revision;
        Ok(surface)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{CellData, PaneSurfacePatch, PaneSurfacePatchRow};

    fn surface(
        surface_revision: u64,
        cells: Vec<CellData>,
        hyperlinks: Vec<String>,
    ) -> PaneSurfaceFrame {
        PaneSurfaceFrame {
            boot_id: "boot".into(),
            projection_revision: 1,
            surface_revision,
            frame: FrameData {
                width: u16::try_from(cells.len()).unwrap(),
                height: 1,
                cells,
                cursor: None,
                hyperlinks,
                graphics: Vec::new(),
            },
            panes: Vec::new(),
            splits: Vec::new(),
            popup: None,
            graphics: crate::protocol::SurfaceGraphicsScene::default(),
        }
    }

    /// The message the server sends once a surface's cells are unchanged: metadata only.
    fn reuse_of(mut next: PaneSurfaceFrame, base_surface_revision: u64) -> ServerMessage {
        message(base_surface_revision, &mut next)
            .unwrap()
            .expect("within the size limit")
    }

    fn linked_patch(
        base_surface_revision: u64,
        rows: Vec<PaneSurfacePatchRow>,
        hyperlinks: &[String],
    ) -> ServerMessage {
        surface_links::message(
            &PaneSurfacePatch {
                boot_id: "boot".into(),
                projection_revision: 1,
                base_surface_revision,
                surface_revision: base_surface_revision + 1,
                rows,
                panes: Vec::new(),
                cursor: None,
            },
            hyperlinks,
        )
        .unwrap()
        .expect("within the size limit")
    }

    #[test]
    fn a_linked_patch_leaves_a_baseline_a_later_reuse_can_be_filled_from() {
        let a = "https://a.test".to_owned();
        let b = "https://b.test".to_owned();
        let mut decoder = Decoder::default();
        decoder
            .decode(ServerMessage::PaneSurface(surface(
                1,
                vec![
                    CellData::test_linked("x", Some(0)),
                    CellData::test_linked("y", None),
                ],
                vec![a.clone()],
            )))
            .unwrap();

        // The patch links the second cell, which reorders the table around the surviving cell.
        decoder
            .decode(linked_patch(
                1,
                vec![PaneSurfacePatchRow {
                    x: 1,
                    y: 0,
                    cells: vec![CellData::test_linked("y", Some(0))],
                }],
                &[b.clone(), a.clone()],
            ))
            .unwrap();

        let patched = surface(
            3,
            vec![
                CellData::test_linked("x", Some(1)),
                CellData::test_linked("y", Some(0)),
            ],
            vec![b, a],
        );
        let ServerMessage::PaneSurface(decoded) = decoder
            .decode(reuse_of(patched.clone(), 2))
            .expect("the baseline followed the patch")
        else {
            panic!("a reuse decodes into a full surface");
        };
        assert_eq!(decoded.frame, patched.frame);
    }

    #[test]
    fn a_linked_patch_that_does_not_continue_the_baseline_drops_it() {
        let mut decoder = Decoder::default();
        decoder
            .decode(ServerMessage::PaneSurface(surface(
                1,
                vec![CellData::test_linked("x", None)],
                Vec::new(),
            )))
            .unwrap();

        decoder
            .decode(linked_patch(
                7,
                vec![PaneSurfacePatchRow {
                    x: 0,
                    y: 0,
                    cells: vec![CellData::test_linked("z", None)],
                }],
                &[],
            ))
            .unwrap();

        assert!(decoder
            .decode(reuse_of(
                surface(2, vec![CellData::test_linked("x", None)], Vec::new()),
                1
            ))
            .is_err_and(|error| error.contains("without a baseline")));
    }

    #[test]
    fn a_reuse_whose_table_disagrees_with_the_baseline_is_rejected() {
        let mut decoder = Decoder::default();
        decoder
            .decode(ServerMessage::PaneSurface(surface(
                1,
                vec![CellData::test_linked("x", Some(0))],
                vec!["https://a.test".to_owned()],
            )))
            .unwrap();

        // Reusing cells against another table would change what every link means. Everything else
        // about the surface matches, so only the table can reject it.
        assert!(decoder
            .decode(reuse_of(
                surface(
                    2,
                    vec![CellData::test_linked("x", Some(0))],
                    vec!["https://b.test".to_owned()],
                ),
                1,
            ))
            .is_err_and(|error| error.contains("does not match its baseline")));
    }

    #[test]
    fn a_linked_patch_the_baseline_cannot_take_drops_the_baseline() {
        let mut decoder = Decoder::default();
        decoder
            .decode(ServerMessage::PaneSurface(surface(
                1,
                vec![
                    CellData::test_linked("x", Some(0)),
                    CellData::test_linked("y", Some(0)),
                ],
                vec!["https://a.test".to_owned()],
            )))
            .unwrap();

        // The surviving cell still needs a.test, which the patch's table drops. The message still
        // reaches the shell, which is where it is refused; the baseline is gone, so a reuse that
        // could only build on it is refused too.
        let passed = decoder
            .decode(linked_patch(
                1,
                vec![PaneSurfacePatchRow {
                    x: 0,
                    y: 0,
                    cells: vec![CellData::test_linked("x", Some(0))],
                }],
                &["https://b.test".to_owned()],
            ))
            .expect("the patch passes through for the shell to refuse");
        assert!(matches!(passed, ServerMessage::EndpointControl { .. }));
        assert!(decoder
            .decode(reuse_of(
                surface(3, vec![CellData::test_linked("x", None)], Vec::new()),
                2
            ))
            .is_err_and(|error| error.contains("without a baseline")));
    }
}
