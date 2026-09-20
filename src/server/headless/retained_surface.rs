use super::*;

use crate::protocol::surface_links::uri_at;

fn rect_fits_frame(rect: protocol::SurfaceRect, frame: &FrameData) -> bool {
    rect.x.saturating_add(rect.width) <= frame.width
        && rect.y.saturating_add(rect.height) <= frame.height
}

fn patch_intersects_hyperlinks(
    frame: &FrameData,
    area: protocol::SurfaceRect,
    patch: &crate::pane::TerminalDirtyPatch,
) -> bool {
    if frame.hyperlinks.is_empty() || !rect_fits_frame(area, frame) {
        return false;
    }
    let width = usize::from(frame.width);
    patch
        .rows
        .iter()
        .filter(|(local_y, _)| *local_y < area.height)
        .any(|(local_y, _)| {
            let start = usize::from(area.y + *local_y) * width + usize::from(area.x);
            let end = start + usize::from(area.width);
            end > frame.cells.len()
                || frame.cells[start..end]
                    .iter()
                    .any(|cell| cell.hyperlink.is_some())
        })
}

/// Cells agree apart from their link index, which only means a uri through a table. Destructured
/// so a new cell field has to be classified here rather than silently ignored.
fn equal_ignoring_link(a: &protocol::CellData, b: &protocol::CellData) -> bool {
    let protocol::CellData {
        symbol,
        fg,
        bg,
        modifier,
        skip,
        hyperlink: _,
    } = a;
    *symbol == b.symbol && *fg == b.fg && *bg == b.bg && *modifier == b.modifier && *skip == b.skip
}

/// Emits the changed spans of one row, taking each sent cell from `cell`.
///
/// A span is extended by the following cell so a wide-to-narrow (or narrow-to-wide) transition
/// repaints content covered by the old grapheme width even when that logical neighbor is unchanged.
fn changed_spans<C, F>(
    width: usize,
    changed: C,
    x: u16,
    y: u16,
    mut cell: F,
    rows: &mut Vec<protocol::PaneSurfacePatchRow>,
) -> Option<()>
where
    C: Fn(usize) -> bool,
    F: FnMut(usize) -> protocol::CellData,
{
    let mut offset = 0;
    while offset < width {
        if !changed(offset) {
            offset += 1;
            continue;
        }
        let start = offset;
        offset += 1;
        while offset < width && changed(offset) {
            offset += 1;
        }
        let end = offset.saturating_add(1).min(width);
        rows.push(protocol::PaneSurfacePatchRow {
            x: x.checked_add(u16::try_from(start).ok()?)?,
            y,
            cells: (start..end).map(&mut cell).collect(),
        });
        offset = end;
    }
    Some(())
}

fn patch_row_changed(frame: &FrameData, row: &protocol::PaneSurfacePatchRow) -> Option<bool> {
    if row.y >= frame.height
        || row.x.saturating_add(u16::try_from(row.cells.len()).ok()?) > frame.width
    {
        return None;
    }
    let start = usize::from(row.y) * usize::from(frame.width) + usize::from(row.x);
    let end = start + row.cells.len();
    if end > frame.cells.len() {
        return None;
    }
    Some(frame.cells[start..end] != row.cells)
}

fn changed_rows(
    frame: &FrameData,
    area: protocol::SurfaceRect,
    patch: &crate::pane::TerminalDirtyPatch,
) -> Option<Vec<protocol::PaneSurfacePatchRow>> {
    if !rect_fits_frame(area, frame) {
        return None;
    }
    let mut rows = Vec::new();
    for (local_y, cells) in &patch.rows {
        if *local_y >= area.height {
            continue;
        }
        let width = usize::from(area.width);
        if cells.len() < width {
            return None;
        }
        let y = area.y + *local_y;
        let frame_start = usize::from(y) * usize::from(frame.width) + usize::from(area.x);
        let frame_end = frame_start.checked_add(width)?;
        let existing = frame.cells.get(frame_start..frame_end)?;
        let desired = &cells[..width];
        changed_spans(
            width,
            |offset| existing[offset] != desired[offset],
            area.x,
            y,
            |offset| desired[offset].clone(),
            &mut rows,
        )?;
    }
    Some(rows)
}

/// The pane rows already carrying a link, as rows within the pane.
///
/// A patch that rewrites one of these may move or remove the link, which the patch itself gives no
/// sign of. Rows away from them keep whatever the last render placed there.
fn pane_baseline_link_rows(frame: &FrameData, rect: protocol::SurfaceRect) -> Vec<u16> {
    if frame.hyperlinks.is_empty() || !rect_fits_frame(rect, frame) {
        return Vec::new();
    }
    let width = usize::from(frame.width);
    (0..rect.height)
        .filter(|local_y| {
            let start = usize::from(rect.y + local_y) * width + usize::from(rect.x);
            frame
                .cells
                .get(start..start + usize::from(rect.width))
                .is_some_and(|row| row.iter().any(|cell| cell.hyperlink.is_some()))
        })
        .collect()
}

/// One rescanned pane, resolved against the rows its dirty walk rewrote.
///
/// Links are collected for the pane's whole viewport, so they describe the screen the pane will be
/// showing once those rows are applied, not just the rows themselves.
struct PaneLinkPlan<'a> {
    rect: protocol::SurfaceRect,
    /// Rows the dirty walk rewrote, by row within the pane.
    dirty: HashMap<u16, &'a [protocol::CellData]>,
    /// Where each uri lands, and the symbol it only applies to.
    placements: HashMap<(u16, u16), (&'a str, &'a str)>,
    /// Surface rows holding at least one placement.
    placement_rows: HashSet<u16>,
}

impl<'a> PaneLinkPlan<'a> {
    fn new(
        rect: protocol::SurfaceRect,
        patch: &'a crate::pane::TerminalDirtyPatch,
        links: &'a [crate::pane::VisibleHyperlink],
    ) -> Self {
        let placements = crate::protocol::hyperlink_table::resolve_placements(links);
        Self {
            rect,
            dirty: patch
                .rows
                .iter()
                .map(|(local_y, cells)| (*local_y, cells.as_slice()))
                .collect(),
            placement_rows: placements.keys().map(|(_, y)| *y).collect(),
            placements,
        }
    }

    fn contains(&self, x: u16, y: u16) -> bool {
        (self.rect.x..self.rect.x.saturating_add(self.rect.width)).contains(&x)
            && (self.rect.y..self.rect.y.saturating_add(self.rect.height)).contains(&y)
    }

    /// The symbol the pane will be showing at this position once its rows are applied.
    fn resulting_symbol<'f: 'a>(&self, frame: &'f FrameData, x: u16, y: u16) -> Option<&'a str> {
        let local_x = x.checked_sub(self.rect.x)?;
        let local_y = y.checked_sub(self.rect.y)?;
        match self.dirty.get(&local_y) {
            Some(cells) => cells.get(usize::from(local_x)),
            None => frame
                .cells
                .get(usize::from(y) * usize::from(frame.width) + usize::from(x)),
        }
        .map(|cell| cell.symbol.as_str())
    }
}

/// The link table the surface has once every pane patch is applied, plus the index each linked
/// position takes in it.
///
/// The table is numbered by walking linked cells in row-major order, exactly the order a full
/// render numbers them, which is what lets a patched surface equal a rendered one.
fn plan_link_table(
    frame: &FrameData,
    panes: &[PaneLinkPlan<'_>],
) -> (Vec<String>, HashMap<(u16, u16), u32>) {
    let width = usize::from(frame.width);
    let mut linked: Vec<((u16, u16), &str)> = Vec::new();
    if width > 0 && !frame.hyperlinks.is_empty() {
        // Links the baseline keeps: everything outside the panes that were rescanned.
        for (index, cell) in frame.cells.iter().enumerate() {
            let Some(link) = cell.hyperlink else {
                continue;
            };
            let (Ok(x), Ok(y)) = (u16::try_from(index % width), u16::try_from(index / width))
            else {
                continue;
            };
            if panes.iter().any(|pane| pane.contains(x, y)) {
                continue;
            }
            if let Some(uri) = frame
                .hyperlinks
                .get(usize::try_from(link).unwrap_or(usize::MAX))
            {
                linked.push(((x, y), uri.as_str()));
            }
        }
    }
    for pane in panes {
        for (position, (symbol, uri)) in &pane.placements {
            if pane.resulting_symbol(frame, position.0, position.1) == Some(*symbol) {
                linked.push((*position, uri));
            }
        }
    }

    linked.sort_unstable_by_key(|((x, y), _)| (*y, *x));
    let mut table = crate::protocol::hyperlink_table::HyperlinkTable::default();
    let index_at = linked
        .into_iter()
        .map(|(position, uri)| (position, table.intern(uri)))
        .collect();
    (table.into_uris(), index_at)
}

/// The rows a rescanned pane must send: everything its dirty walk changed, plus the rows whose
/// links changed even though their cells did not, which is how a wrapped url reaches a clean row.
///
/// Cells whose uri is unchanged are left out even when the table renumbered them, because the
/// client re-indexes everything the patch does not cover.
fn linked_pane_rows(
    frame: &FrameData,
    pane: &PaneLinkPlan<'_>,
    table: &[String],
    index_at: &HashMap<(u16, u16), u32>,
    rows: &mut Vec<protocol::PaneSurfacePatchRow>,
) -> Option<()> {
    if !rect_fits_frame(pane.rect, frame) {
        return None;
    }
    let width = usize::from(pane.rect.width);
    for local_y in 0..pane.rect.height {
        let y = pane.rect.y + local_y;
        let dirty = pane.dirty.get(&local_y).copied();
        if dirty.is_some_and(|cells| cells.len() < width) {
            return None;
        }
        let start = usize::from(y) * usize::from(frame.width) + usize::from(pane.rect.x);
        let existing = frame.cells.get(start..start.checked_add(width)?)?;
        if dirty.is_none()
            && !pane.placement_rows.contains(&y)
            && !existing.iter().any(|cell| cell.hyperlink.is_some())
        {
            continue;
        }
        let desired = |offset: usize| dirty.map_or(&existing[offset], |cells| &cells[offset]);
        let desired_link = |offset: usize| {
            index_at
                .get(&(pane.rect.x + u16::try_from(offset).unwrap_or(u16::MAX), y))
                .copied()
        };
        changed_spans(
            width,
            |offset| {
                !equal_ignoring_link(&existing[offset], desired(offset))
                    || uri_at(&frame.hyperlinks, existing[offset].hyperlink)
                        != uri_at(table, desired_link(offset))
            },
            pane.rect.x,
            y,
            |offset| protocol::CellData {
                hyperlink: desired_link(offset),
                ..desired(offset).clone()
            },
            rows,
        )?;
    }
    Some(())
}

fn retained_scrollbar_patch(
    app: &app::App,
    frame: &FrameData,
    pane: &mut protocol::PaneSurfacePane,
    alternate_screen_active: bool,
    metrics: Option<crate::pane::ScrollMetrics>,
) -> Option<Vec<protocol::PaneSurfacePatchRow>> {
    let next_rect = metrics
        .filter(|metrics| metrics.max_offset_from_bottom > 0)
        .filter(|_| app.state.pane_scrollbars && !alternate_screen_active)
        .and_then(|_| {
            let rect = protocol::SurfaceRect {
                x: pane.inner_rect.x.checked_add(pane.inner_rect.width)?,
                y: pane.inner_rect.y,
                width: 1,
                height: pane.inner_rect.height,
            };
            (rect_fits_frame(rect, frame)
                && rect.x >= pane.rect.x
                && rect.x < pane.rect.x.saturating_add(pane.rect.width))
            .then_some(rect)
        });
    let patch_rect = next_rect.or(pane.scrollbar_rect);
    pane.scrollbar_rect = next_rect;
    let Some(rect) = patch_rect else {
        return Some(Vec::new());
    };

    let track = Rect::new(0, 0, 1, rect.height);
    let mut buffer = ratatui::buffer::Buffer::empty(track);
    if let (Some(metrics), Some(_)) = (metrics, next_rect) {
        crate::ui::render_pane_scrollbar_buffer(
            &mut buffer,
            metrics,
            track,
            &app.state.palette,
            pane.focused,
        );
    }
    let cells = buffer
        .content
        .iter()
        .map(protocol::CellData::from_ratatui_cell)
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for (offset, cell) in cells.into_iter().enumerate() {
        let row = protocol::PaneSurfacePatchRow {
            x: rect.x,
            y: rect.y.checked_add(u16::try_from(offset).ok()?)?,
            cells: vec![cell],
        };
        if patch_row_changed(frame, &row)? {
            rows.push(row);
        }
    }
    Some(rows)
}

fn retained_cursor(
    app: &app::App,
    panes: &[protocol::PaneSurfacePane],
) -> Option<protocol::CursorState> {
    let pane = panes.iter().find(|pane| pane.focused)?;
    let (workspace_index, pane_id) = app.parse_pane_id(&pane.pane_id)?;
    if !app.state.pane_exposes_host_cursor(workspace_index, pane_id) {
        return None;
    }
    let runtime = app.state.runtime_for_pane_in_workspace(
        &app.terminal_runtimes,
        workspace_index,
        pane_id,
    )?;
    if runtime.synchronized_output_active() {
        return None;
    }
    let area = Rect::new(
        pane.inner_rect.x,
        pane.inner_rect.y,
        pane.inner_rect.width,
        pane.inner_rect.height,
    );
    runtime
        .cursor_state(area, true)
        .map(|cursor| protocol::CursorState {
            x: cursor.x,
            y: cursor.y,
            visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
            shape: cursor.shape,
        })
}

struct RetainedRecipient<'a> {
    client_id: u64,
    surface: &'a protocol::PaneSurfaceFrame,
    surface_links: bool,
}

struct CollectedPanePatch {
    pane_id: String,
    patch: crate::pane::TerminalDirtyPatch,
    /// Every link the pane is showing, collected with the patch when they can have changed. When
    /// this is set the pane is planned with a link table instead of refusing the patch.
    links: Option<crate::pane::VisibleHyperlinks>,
    content_revision: u64,
    scroll_metrics: Option<crate::pane::ScrollMetrics>,
    mouse_reporting: bool,
    sgr_pixel_mouse: bool,
    alternate_screen_active: bool,
    graphics_may_have_placements: bool,
}

struct RetainedRecipientUpdate {
    client_id: u64,
    patch: protocol::PaneSurfacePatch,
    hyperlinks: Option<Vec<String>>,
    graphics: Option<(
        protocol::PaneSurfaceFrame,
        crate::kitty_graphics::surface::DeliveryCache,
        crate::kitty_graphics::surface::SourceFiles,
    )>,
}

fn has_synchronized_pane(app: &app::App, surface: &protocol::PaneSurfaceFrame) -> bool {
    surface.panes.iter().any(|pane| {
        app.parse_pane_id(&pane.pane_id)
            .and_then(|(workspace_index, pane_id)| {
                app.state.runtime_for_pane_in_workspace(
                    &app.terminal_runtimes,
                    workspace_index,
                    pane_id,
                )
            })
            .is_some_and(|runtime| runtime.synchronized_output_active())
    })
}

impl HeadlessServer {
    /// Applies terminal dirty rows to the committed origin-relative pane surface.
    /// Any presentation or geometry uncertainty falls back to the complete renderer.
    pub(super) fn render_retained_pane_surface_and_stream(
        &mut self,
        pty_sources: &HashSet<crate::layout::PaneId>,
    ) -> bool {
        crate::render_prof::event("retained_surface.attempt");
        let started = crate::render_prof::timer();
        macro_rules! fallback {
            ($reason:literal) => {{
                crate::render_prof::event(concat!("retained_surface.fallback.", $reason));
                crate::render_prof::duration_since("retained_surface.total", started);
                return false;
            }};
        }
        macro_rules! success {
            ($reason:literal) => {{
                crate::render_prof::event("retained_surface.success");
                crate::render_prof::event(concat!("retained_surface.success.", $reason));
                crate::render_prof::duration_since("retained_surface.total", started);
                return true;
            }};
        }

        if pty_sources.is_empty()
            || self.app.full_redraw_pending
            || self.app.state.popup_pane.is_some()
            || self.app.state.reveal_hidden_cursor_for_cjk_ime
        {
            fallback!("unsafe_state");
        }
        let mut targets = render_targets(&self.clients, self.foreground_client_id);
        targets.retain(|(client_id, _, _, _, mode)| {
            !matches!(mode, ClientConnectionMode::ClientShell)
                || self
                    .clients
                    .get(client_id)
                    .is_some_and(|client| client.shell_surface_active)
        });
        if targets.is_empty() {
            success!("no_active_surface");
        }
        if targets
            .iter()
            .any(|target| !matches!(target.4, ClientConnectionMode::ClientShell))
        {
            fallback!("non_shell_target");
        }

        let mut recipients = Vec::with_capacity(targets.len());
        for (client_id, (cols, rows), _, _, _) in &targets {
            let Some(client) = self.clients.get(client_id) else {
                fallback!("client_missing");
            };
            if client.deferred_render() != DeferredRender::None {
                crate::render_prof::event("retained_surface.recipient_deferred");
                continue;
            }
            if client.render_state.requires_recompute() {
                fallback!("recompute_pending");
            }
            let Some(surface) = client.render_state.last_pane_surface() else {
                fallback!("no_baseline");
            };
            if surface.boot_id != self.client_shell_boot_id
                || surface.projection_revision != client.shell_projection_revision
                || surface.frame.width != *cols
                || surface.frame.height != *rows
                || surface.popup.is_some()
                || !surface.graphics.assets.is_empty()
                || !surface.frame.graphics.is_empty()
            {
                fallback!("baseline_mismatch");
            }
            if has_synchronized_pane(&self.app, surface) {
                fallback!("synchronized_visible");
            }
            recipients.push(RetainedRecipient {
                client_id: *client_id,
                surface,
                surface_links: client.render_state.surface_links(),
            });
        }
        if recipients.is_empty() {
            success!("all_recipients_deferred");
        }

        // Links are planned for the whole pass or not at all: one legacy recipient and every
        // recipient keeps the refusals it had before the capability existed.
        let link_patches = recipients.iter().all(|recipient| recipient.surface_links);
        let mut collected = Vec::with_capacity(pty_sources.len());
        for source in pty_sources {
            let mut public_pane_id = None;
            let mut width = 0u16;
            let mut height = 0u16;
            let mut link_rect = None;
            let mut rects_agree = true;
            let mut linked_rows: Vec<u16> = Vec::new();
            for recipient in &recipients {
                let Some(pane) = recipient.surface.panes.iter().find(|pane| {
                    self.app
                        .parse_pane_id(&pane.pane_id)
                        .is_some_and(|(_, pane_id)| pane_id == *source)
                }) else {
                    continue;
                };
                public_pane_id.get_or_insert_with(|| pane.pane_id.clone());
                width = width.max(pane.inner_rect.width);
                height = height.max(pane.inner_rect.height);
                if link_patches {
                    rects_agree &= *link_rect.get_or_insert(pane.inner_rect) == pane.inner_rect;
                    for local_y in
                        pane_baseline_link_rows(&recipient.surface.frame, pane.inner_rect)
                    {
                        if !linked_rows.contains(&local_y) {
                            linked_rows.push(local_y);
                        }
                    }
                }
            }
            let Some(public_pane_id) = public_pane_id else {
                continue;
            };
            let Some((workspace_index, pane_id)) = self.app.parse_pane_id(&public_pane_id) else {
                fallback!("pane_missing");
            };
            let Some(runtime) = self.app.state.runtime_for_pane_in_workspace(
                &self.app.terminal_runtimes,
                workspace_index,
                pane_id,
            ) else {
                fallback!("runtime_missing");
            };
            // Links are collected under the same lock and revision as the patch, and only when
            // the patch can have changed them. Recipients that lay the pane out differently cannot
            // share one scan, so they keep the legacy refusals.
            let link_request =
                link_rect
                    .filter(|_| rects_agree)
                    .map(|rect| crate::pane::LinkRequest {
                        area: Rect::new(rect.x, rect.y, rect.width, rect.height),
                        options: crate::pane::LinkScanOptions {
                            detect_plain_urls: self.app.state.detect_urls,
                        },
                        linked_rows,
                    });
            let Some(snapshot) = runtime.collect_dirty_patch_snapshot(width, height, link_request)
            else {
                fallback!("terminal_snapshot");
            };
            let patch = match snapshot.patch {
                crate::pane::TerminalDirtyPatchOutcome::Clean => {
                    crate::render_prof::event("retained_surface.pane_clean");
                    crate::pane::TerminalDirtyPatch::default()
                }
                crate::pane::TerminalDirtyPatchOutcome::Patch(patch) => patch,
                crate::pane::TerminalDirtyPatchOutcome::Fallback => {
                    fallback!("terminal_patch");
                }
            };
            collected.push(CollectedPanePatch {
                pane_id: public_pane_id,
                patch,
                links: snapshot.links,
                content_revision: snapshot.content_revision,
                scroll_metrics: snapshot.scroll_metrics,
                mouse_reporting: snapshot.mouse_reporting,
                sgr_pixel_mouse: snapshot.sgr_pixel_mouse,
                alternate_screen_active: snapshot.alternate_screen_active,
                graphics_may_have_placements: snapshot.graphics_may_have_placements,
            });
        }

        let mut updates = Vec::with_capacity(recipients.len());
        for recipient in &recipients {
            let client_id = recipient.client_id;
            let surface = recipient.surface;
            let mut panes = surface.panes.clone();
            let projection_revision = surface.projection_revision;
            let base_surface_revision = surface.surface_revision;
            let mut changed_panes = Vec::with_capacity(collected.len());
            let mut patch_rows = Vec::new();
            let link_plans = collected
                .iter()
                .filter_map(|collected_pane| {
                    let links = collected_pane.links.as_deref()?;
                    let pane = surface
                        .panes
                        .iter()
                        .find(|pane| pane.pane_id == collected_pane.pane_id)?;
                    Some(PaneLinkPlan::new(
                        pane.inner_rect,
                        &collected_pane.patch,
                        links,
                    ))
                })
                .collect::<Vec<_>>();
            let mut metadata_changed = false;
            let mut refresh_graphics = !surface.graphics.placements.is_empty()
                || !surface.graphics.retained_assets.is_empty();
            for collected_pane in &collected {
                let Some(pane) = panes
                    .iter_mut()
                    .find(|pane| pane.pane_id == collected_pane.pane_id)
                else {
                    continue;
                };
                // Alternate-screen transitions change whether the pane reserves
                // a scrollbar gutter. Recompute layout and resize the runtime
                // through the complete renderer before retaining further rows.
                if pane.alternate_screen_active != collected_pane.alternate_screen_active {
                    fallback!("alternate_screen_geometry");
                }
                // A pane whose links were collected is planned with the table below. Without them
                // the pane cannot have links to plan, so these refusals stand exactly as they did
                // before the capability existed.
                if collected_pane.links.is_none() {
                    if collected_pane.patch.program_links
                        || patch_intersects_hyperlinks(
                            &surface.frame,
                            pane.inner_rect,
                            &collected_pane.patch,
                        )
                    {
                        fallback!("hyperlink");
                    }
                    if self.app.state.detect_urls && collected_pane.patch.may_contain_url() {
                        fallback!("detected_link");
                    }
                    let Some(rows) =
                        changed_rows(&surface.frame, pane.inner_rect, &collected_pane.patch)
                    else {
                        fallback!("invalid_patch");
                    };
                    patch_rows.extend(rows);
                }
                refresh_graphics |= collected_pane.graphics_may_have_placements;
                let previous_pane = pane.clone();
                let Some(scrollbar_rows) = retained_scrollbar_patch(
                    &self.app,
                    &surface.frame,
                    pane,
                    collected_pane.alternate_screen_active,
                    collected_pane.scroll_metrics,
                ) else {
                    fallback!("scrollbar_patch");
                };
                patch_rows.extend(scrollbar_rows);
                pane.content_revision = collected_pane.content_revision;
                pane.mouse_reporting = collected_pane.mouse_reporting;
                pane.sgr_pixel_mouse = collected_pane.sgr_pixel_mouse;
                pane.alternate_screen_active = collected_pane.alternate_screen_active;
                pane.scroll = collected_pane.scroll_metrics.map(|metrics| {
                    protocol::PaneSurfaceScrollMetrics {
                        offset_from_bottom: metrics.offset_from_bottom as u64,
                        max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
                        viewport_rows: metrics.viewport_rows as u64,
                    }
                });
                metadata_changed |= *pane != previous_pane;
                changed_panes.push(pane.clone());
            }

            // The table is numbered over the whole surface, so it is planned once all panes have
            // been collected, and the rows it indexes are emitted against it.
            let mut hyperlinks = None;
            if !link_plans.is_empty() {
                let (table, index_at) = plan_link_table(&surface.frame, &link_plans);
                for plan in &link_plans {
                    if linked_pane_rows(&surface.frame, plan, &table, &index_at, &mut patch_rows)
                        .is_none()
                    {
                        fallback!("invalid_link_patch");
                    }
                }
                // An unchanged table renumbers nothing, so the rows already carry indices the
                // client can read and the cheaper patch says exactly the same thing.
                if table != surface.frame.hyperlinks {
                    crate::render_prof::event("retained_surface.link_patch");
                    hyperlinks = Some(table);
                }
            }

            let cursor = retained_cursor(&self.app, &panes);
            let cursor_changed = cursor != surface.frame.cursor;
            let patch = protocol::PaneSurfacePatch {
                boot_id: self.client_shell_boot_id.clone(),
                projection_revision,
                base_surface_revision,
                surface_revision: 0,
                rows: patch_rows,
                panes: changed_panes,
                cursor,
            };
            let mut graphics_changed = false;
            let graphics = if refresh_graphics {
                let Some(target) = self.shell_target_for_client(client_id) else {
                    fallback!("graphics_target");
                };
                let client = &self.clients[&client_id];
                let Some((graphics, delivery, sources)) =
                    crate::server::client_shell_graphics::collect_retained(
                        &self.app,
                        &panes,
                        target,
                        client.cell_size,
                        &client.shell_graphics_delivery,
                        client_id,
                    )
                else {
                    fallback!("graphics_geometry");
                };
                graphics_changed = graphics != surface.graphics;
                Some((graphics, delivery, sources))
            } else {
                None
            };
            if patch.rows.is_empty() && !cursor_changed && !metadata_changed && !graphics_changed {
                continue;
            }
            // A graphics refresh sends the patched surface whole, so the table goes into that
            // surface rather than travelling with the patch.
            let (graphics, hyperlinks) = match graphics {
                Some((graphics, delivery, sources)) => {
                    let mut next_surface = surface.clone();
                    if crate::server::render_stream::apply_pane_surface_patch(
                        &mut next_surface,
                        &patch,
                        hyperlinks,
                    )
                    .is_err()
                    {
                        fallback!("link_patch_baseline");
                    }
                    next_surface.graphics = graphics;
                    (Some((next_surface, delivery, sources)), None)
                }
                None => (None, hyperlinks),
            };
            updates.push(RetainedRecipientUpdate {
                client_id,
                patch,
                hyperlinks,
                graphics,
            });
        }
        if updates.is_empty() {
            success!("unchanged");
        }
        if recipients
            .iter()
            .any(|recipient| has_synchronized_pane(&self.app, recipient.surface))
        {
            fallback!("synchronized_during_patch");
        }

        let mut sent = 0u64;
        let mut deferred = 0u64;
        let mut disconnected = Vec::new();
        for update in updates {
            let RetainedRecipientUpdate {
                client_id,
                patch,
                hyperlinks,
                mut graphics,
            } = update;
            if graphics.as_ref().is_some_and(|(surface, _, _)| {
                self.defer_changed_native_geometry(client_id, &surface.graphics)
            }) {
                deferred += 1;
                continue;
            }
            let native_upload = graphics.as_mut().and_then(|(surface, delivery, sources)| {
                self.prepare_native_scene(client_id, &mut surface.graphics, delivery, sources)
            });
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            let Some(writer) = client.writer.as_ref().cloned() else {
                client.defer_full_render();
                deferred += 1;
                continue;
            };
            // The published row patch cannot carry images. Reuse the retained text/layout
            // in a graphics-capable surface message rather than invoking the full renderer.
            let (prepared, graphics_delivery) = match graphics {
                Some((surface, delivery, _)) => (
                    client
                        .render_state
                        .prepare_pane_surface_with_file(surface, native_upload.is_some()),
                    Some(delivery),
                ),
                None => (
                    client
                        .render_state
                        .prepare_pane_surface_patch(patch, hyperlinks),
                    None,
                ),
            };
            let Some(prepared) = prepared else {
                client.defer_full_render();
                deferred += 1;
                continue;
            };
            let max_frame_size = if graphics_delivery.is_some() {
                MAX_GRAPHICS_FRAME_SIZE
            } else {
                protocol::MAX_FRAME_SIZE
            };
            let mut serialized =
                match Self::frame_server_message_with_max(prepared.message(), max_frame_size) {
                    Ok(serialized) => serialized,
                    Err(error) => {
                        warn!(
                            client_id,
                            %error,
                            "failed to serialize retained pane surface patch"
                        );
                        // A delta may own an encoded graphics payload that cannot be
                        // trimmed in place. Force the bounded full-surface recovery path.
                        client.render_state.request_repaint();
                        client.defer_full_render();
                        deferred += 1;
                        continue;
                    }
                };
            if let Some((_, message)) = &native_upload {
                let Ok(file_frame) =
                    Self::frame_server_message_with_max(message, MAX_GRAPHICS_FRAME_SIZE)
                else {
                    client.defer_full_render();
                    deferred += 1;
                    continue;
                };
                serialized.extend_from_slice(&file_frame);
            }
            crate::render_prof::counter("retained_surface.bytes", serialized.len() as u64);
            let send = if native_upload.is_some() || self.native_graphics.is_pending(client_id) {
                writer.render.send_ordered(serialized)
            } else {
                writer.render.try_send(serialized)
            };
            match send {
                Ok(()) => {
                    if let Some((graphics, inline_assets)) = prepared.queued_surface_graphics() {
                        self.native_graphics
                            .commit_scene(client_id, graphics, inline_assets);
                    }
                    if let Some((pending, _)) = native_upload {
                        self.native_graphics.commit(client_id, pending);
                    }
                    let graphics_pending = graphics_delivery
                        .as_ref()
                        .is_some_and(crate::kitty_graphics::surface::DeliveryCache::has_pending);
                    if let Some(delivery) = graphics_delivery {
                        client.shell_graphics_delivery = delivery;
                    }
                    if graphics_pending {
                        client.defer_full_render();
                    } else {
                        client.clear_deferred_render();
                    }
                    client.render_state.commit_sent_frame(prepared);
                    // A commit that could not take the patch dropped the baseline. The client
                    // holds the same patch and refuses it the same way, so it needs a whole
                    // surface without waiting for the pane to produce output.
                    if client.render_state.last_pane_surface().is_none() {
                        client.defer_full_render();
                    }
                    sent += 1;
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    client.defer_full_render();
                    deferred += 1;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    disconnected.push(client_id);
                }
            }
        }
        for client_id in disconnected {
            self.remove_client_and_resize_if_needed(client_id);
        }
        crate::render_prof::counter("retained_surface.recipients.sent", sent);
        crate::render_prof::counter("retained_surface.recipients.deferred", deferred);
        if sent > 0 {
            success!("sent");
        }
        success!("recovery_queued");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(symbol: &str) -> protocol::CellData {
        protocol::CellData {
            symbol: symbol.to_owned(),
            fg: 0,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    fn link(x: u16, y: u16, symbol: &str, uri: &str) -> crate::pane::VisibleHyperlink {
        crate::pane::VisibleHyperlink {
            position: (x, y),
            symbol: symbol.to_owned(),
            uri: uri.into(),
        }
    }

    #[test]
    fn a_planned_table_keeps_links_the_rescanned_panes_do_not_own() {
        // Row 0 belongs to another pane and keeps its link; row 1 is the pane being rescanned.
        let frame = FrameData {
            width: 2,
            height: 2,
            cells: vec![
                protocol::CellData::test_linked("a", Some(0)),
                cell(" "),
                cell("b"),
                cell(" "),
            ],
            cursor: None,
            hyperlinks: vec!["https://kept.test".to_owned()],
            graphics: Vec::new(),
        };
        let patch = crate::pane::TerminalDirtyPatch::default();
        let links = [link(0, 1, "b", "https://new.test")];
        let rect = protocol::SurfaceRect {
            x: 0,
            y: 1,
            width: 2,
            height: 1,
        };

        let (table, index_at) = plan_link_table(&frame, &[PaneLinkPlan::new(rect, &patch, &links)]);

        assert_eq!(table, ["https://kept.test", "https://new.test"]);
        assert_eq!(
            index_at.get(&(0, 0)),
            Some(&0),
            "the other pane keeps its link"
        );
        assert_eq!(index_at.get(&(0, 1)), Some(&1));
    }

    #[test]
    fn a_planned_table_skips_a_link_whose_symbol_moved_on() {
        // The scan saw "b", but the surface is showing something else there now, exactly the case
        // the full render's builder drops.
        let frame = FrameData {
            width: 1,
            height: 1,
            cells: vec![cell("c")],
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let patch = crate::pane::TerminalDirtyPatch::default();
        let links = [link(0, 0, "b", "https://stale.test")];
        let rect = protocol::SurfaceRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };

        let (table, index_at) = plan_link_table(&frame, &[PaneLinkPlan::new(rect, &patch, &links)]);

        assert!(table.is_empty());
        assert!(index_at.is_empty());
    }

    #[test]
    fn retained_rows_send_only_changed_cell_spans() {
        let frame = FrameData {
            width: 6,
            height: 2,
            cells: vec![cell(" "); 12],
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let patch = crate::pane::TerminalDirtyPatch {
            rows: vec![(0, vec![cell(" "), cell("x"), cell("y"), cell(" ")])],
            ..Default::default()
        };

        let rows = changed_rows(
            &frame,
            protocol::SurfaceRect {
                x: 1,
                y: 1,
                width: 4,
                height: 1,
            },
            &patch,
        )
        .expect("valid patch");

        assert_eq!(
            rows,
            vec![protocol::PaneSurfacePatchRow {
                x: 2,
                y: 1,
                cells: vec![cell("x"), cell("y"), cell(" ")],
            }]
        );
        assert_eq!(frame.cells, vec![cell(" "); 12], "planning must not commit");
    }

    #[test]
    fn retained_rows_include_the_cell_after_a_width_transition() {
        let frame = FrameData {
            width: 3,
            height: 1,
            cells: vec![cell("界"), cell("z"), cell("q")],
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let patch = crate::pane::TerminalDirtyPatch {
            rows: vec![(0, vec![cell("x"), cell("z"), cell("q")])],
            ..Default::default()
        };

        let rows = changed_rows(
            &frame,
            protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 3,
                height: 1,
            },
            &patch,
        )
        .expect("valid patch");

        assert_eq!(
            rows,
            vec![protocol::PaneSurfacePatchRow {
                x: 0,
                y: 0,
                cells: vec![cell("x"), cell("z")],
            }]
        );
    }

    #[test]
    fn retained_rows_omit_unchanged_full_dirty_rows() {
        let frame = FrameData {
            width: 4,
            height: 2,
            cells: vec![cell(" "); 8],
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let patch = crate::pane::TerminalDirtyPatch {
            rows: vec![(0, vec![cell(" "); 4]), (1, vec![cell(" "); 4])],
            ..Default::default()
        };

        let rows = changed_rows(
            &frame,
            protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 4,
                height: 2,
            },
            &patch,
        )
        .expect("valid patch");

        assert!(rows.is_empty());
    }
}
