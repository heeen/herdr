//! Frame blitting — renders FrameData to the terminal using diff-based updates.
//!
//! The blitting strategy:
//! 1. On the first frame, write the entire buffer (full redraw).
//! 2. On subsequent frames, diff against the last frame and only write
//!    the cells that changed.
//! 3. Wrap each frame in synchronized output so terminals that support it do
//!    not expose intermediate cursor positions while the frame is painted.
//! 4. Before writing any cells, hide the cursor to avoid stray cursor
//!    artifacts on terminals that render the hardware cursor at intermediate
//!    `CUP` positions during the frame stream.
//! 5. After writing all changed cells, restore the final cursor visibility
//!    and position from `frame.cursor`.
//! 6. On platforms that need it, repeat the final cursor anchor after ending
//!    synchronized output so external IMEs can place candidate windows at the
//!    real input position. Windows Terminal exposes that repeat as visible
//!    cursor movement during active TUI repaints, so Windows skips it.
//!
//! Escape sequences used:
//! - `CSI H` (CUP) — move cursor to (row, col)
//! - `CSI m` (SGR) — set graphic rendition (colors, bold, etc.)
//! - `CSI ? 2026 h/l` — begin/end synchronized output
//! - `CSI Ps SP q` — DECSCUSR cursor shape
//! - `ESC ] 52 ; c ; <base64> BEL` — OSC 52 clipboard write
//! - `ESC ] 8 ; id=<id> ; <uri> ST` — OSC 8 hyperlink
//!
//! The goal is minimal output: skip unchanged cells, batch adjacent changes,
//! and minimize cursor movement.

use std::cmp;
use std::io::Write;

use unicode_width::UnicodeWidthStr;

use crate::protocol::surface_links::uri_at;
use crate::protocol::{
    underline_style_from_modifier, CellData, CursorState, FrameData, PaneSurfacePatchRow,
};

const REVERSED_MODIFIER: u16 = 1 << 6;
const SYNC_OUTPUT_END: &[u8] = b"\x1b[?2026l";

pub(crate) fn final_sync_output_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(SYNC_OUTPUT_END.len())
        .rposition(|window| window == SYNC_OUTPUT_END)
}

/// Bytes produced by a [`BlitEncoder`] for one terminal frame.
pub(crate) struct EncodedBlit {
    /// Terminal escape bytes ready to write to the host terminal.
    pub(crate) bytes: Vec<u8>,
    /// Whether this frame was encoded as a full redraw.
    pub(crate) full: bool,
    next_last_visible_cursor: Option<(u16, u16)>,
    next_last_cursor_shape: u8,
}

/// Stateful encoder that diffs semantic frames into terminal ANSI bytes.
#[derive(Default)]
pub(crate) struct BlitEncoder {
    last_frame: Option<FrameData>,
    last_visible_cursor: Option<(u16, u16)>,
    last_cursor_shape: u8,
}

impl BlitEncoder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn encode(&self, frame: &FrameData, repaint: bool) -> EncodedBlit {
        self.encode_inner(frame, repaint, false)
    }

    pub(crate) fn encode_with_suppressed_visible_cursor(
        &self,
        frame: &FrameData,
        repaint: bool,
    ) -> EncodedBlit {
        self.encode_inner(frame, repaint, true)
    }

    fn encode_inner(
        &self,
        frame: &FrameData,
        repaint: bool,
        suppress_visible_cursor: bool,
    ) -> EncodedBlit {
        let previous_frame = self.last_frame.as_ref();
        let prev = if repaint { None } else { previous_frame };
        let full = repaint
            || prev.is_none()
            || prev.is_some_and(|p| p.width != frame.width || p.height != frame.height);
        let clear_before_full_redraw = previous_frame.is_none();
        let prof_stats =
            crate::render_prof::enabled().then(|| compute_prof_blit_stats(frame, prev, full));
        let prof_started = crate::render_prof::timer();
        let mut bytes = Vec::new();
        let mut next_last_visible_cursor = self.last_visible_cursor;
        let mut next_last_cursor_shape = self.last_cursor_shape;
        blit_frame_to_with_cursor_memory_and_clear_policy(
            &mut bytes,
            frame,
            prev,
            &mut next_last_visible_cursor,
            &mut next_last_cursor_shape,
            repeat_ime_anchor_after_sync(),
            clear_before_full_redraw,
            suppress_visible_cursor,
        );
        if let Some(stats) = prof_stats {
            crate::render_prof::duration_since("ansi_encode.total", prof_started);
            crate::render_prof::counter("ansi_encode.bytes", bytes.len() as u64);
            crate::render_prof::counter("ansi_encode.scanned_cells", stats.scanned_cells);
            crate::render_prof::counter("ansi_encode.changed_cells", stats.changed_cells);
            crate::render_prof::counter("ansi_encode.changed_runs", stats.changed_runs);
            if full {
                crate::render_prof::event("ansi_encode.full");
            } else {
                crate::render_prof::event("ansi_encode.partial");
            }
        }
        EncodedBlit {
            bytes,
            full,
            next_last_visible_cursor,
            next_last_cursor_shape,
        }
    }

    pub(crate) fn commit(&mut self, frame: FrameData, encoded: EncodedBlit) {
        self.last_visible_cursor = encoded.next_last_visible_cursor;
        self.last_cursor_shape = encoded.next_last_cursor_shape;
        self.last_frame = Some(frame);
    }

    pub(crate) fn is_current(&self, frame: &FrameData) -> bool {
        self.last_frame.as_ref() == Some(frame)
    }

    /// Encodes a patch, resolving its cells against `hyperlinks` when it brings its own table.
    pub(crate) fn encode_patch(
        &self,
        rows: &[PaneSurfacePatchRow],
        cursor: Option<CursorState>,
        suppress_visible_cursor: bool,
        hyperlinks: Option<&[String]>,
    ) -> Option<EncodedBlit> {
        let frame = self.last_frame.as_ref()?;
        if rows.iter().any(|row| !patch_row_fits(frame, row)) || patch_rows_overlap(rows) {
            return None;
        }
        // Metadata revisions need no terminal output. Keep visible cursors on
        // the normal path because their suppression policy can change.
        if rows.is_empty()
            && cursor == frame.cursor
            && cursor.as_ref().is_none_or(|cursor| !cursor.visible)
        {
            return Some(EncodedBlit {
                bytes: Vec::new(),
                full: false,
                next_last_visible_cursor: self.last_visible_cursor,
                next_last_cursor_shape: self.last_cursor_shape,
            });
        }
        let mut bytes = Vec::new();
        let mut next_last_visible_cursor = self.last_visible_cursor;
        let mut next_last_cursor_shape = self.last_cursor_shape;
        blit_patch_to(
            &mut bytes,
            frame,
            hyperlinks.unwrap_or(&frame.hyperlinks),
            rows,
            cursor,
            &mut next_last_visible_cursor,
            &mut next_last_cursor_shape,
            repeat_ime_anchor_after_sync(),
            suppress_visible_cursor,
        );
        Some(EncodedBlit {
            bytes,
            full: false,
            next_last_visible_cursor,
            next_last_cursor_shape,
        })
    }

    pub(crate) fn patch_rows_with_drawn_cursor(
        &self,
        rows: &[PaneSurfacePatchRow],
        cursor: Option<&CursorState>,
        hyperlinks: Option<&[String]>,
    ) -> Option<Vec<PaneSurfacePatchRow>> {
        let frame = self.last_frame.as_ref()?;
        let mut rows = rows.to_vec();
        // A cell taken from the old frame carries an old index, which the patch's table renumbers.
        let borrowed = |cell: &CellData| {
            let mut cell = cell.clone();
            if let Some(hyperlinks) = hyperlinks {
                cell.hyperlink = uri_at(&frame.hyperlinks, cell.hyperlink)
                    .and_then(|uri| hyperlinks.iter().position(|candidate| candidate == uri))
                    .and_then(|index| u32::try_from(index).ok());
            }
            cell.modifier ^= REVERSED_MODIFIER;
            cell
        };

        let previous = frame
            .cursor
            .as_ref()
            .filter(|cursor| cursor.visible)
            .map(|cursor| clamp_cursor_position(frame, cursor.x, cursor.y));
        let next = cursor
            .filter(|cursor| cursor.visible)
            .map(|cursor| clamp_cursor_position(frame, cursor.x, cursor.y));

        if let Some((x, y)) = previous.filter(|position| Some(*position) != next) {
            if patch_cell_mut(&mut rows, x, y).is_none() {
                let cell = borrowed(frame.cells.get(frame_cell_index(frame, x, y)?)?);
                rows.push(PaneSurfacePatchRow {
                    x,
                    y,
                    cells: vec![cell],
                });
            }
        }
        if let Some((x, y)) = next {
            if let Some(cell) = patch_cell_mut(&mut rows, x, y) {
                cell.modifier ^= REVERSED_MODIFIER;
            } else if previous != next {
                let cell = borrowed(frame.cells.get(frame_cell_index(frame, x, y)?)?);
                rows.push(PaneSurfacePatchRow {
                    x,
                    y,
                    cells: vec![cell],
                });
            }
        }
        Some(rows)
    }

    /// Advances the baseline to the patch that was just written.
    ///
    /// A patch bringing its own table is applied through the shared rules, so the baseline ends up
    /// where a full compose would have put it. Refusing here leaves the baseline untouched and the
    /// caller composes a full frame instead.
    pub(crate) fn commit_patch(
        &mut self,
        rows: &[PaneSurfacePatchRow],
        cursor: Option<CursorState>,
        encoded: EncodedBlit,
        hyperlinks: Option<Vec<String>>,
    ) -> bool {
        let Some(frame) = self.last_frame.as_mut() else {
            return false;
        };
        if let Some(hyperlinks) = hyperlinks {
            if let Err(error) = crate::protocol::surface_links::apply(frame, rows, hyperlinks) {
                crate::render_prof::event("client_surface_patch.fallback.link_table");
                tracing::debug!(%error, "composed patch does not fit the presented frame");
                return false;
            }
        } else {
            for row in rows {
                let start = usize::from(row.y) * usize::from(frame.width) + usize::from(row.x);
                let end = start + row.cells.len();
                let Some(target) = frame.cells.get_mut(start..end) else {
                    return false;
                };
                target.clone_from_slice(&row.cells);
            }
        }
        frame.cursor = cursor;
        self.last_visible_cursor = encoded.next_last_visible_cursor;
        self.last_cursor_shape = encoded.next_last_cursor_shape;
        true
    }
}

pub(crate) fn frame_with_drawn_cursor(mut frame: FrameData) -> FrameData {
    if let Some(cursor) = frame.cursor.as_ref().filter(|cursor| cursor.visible) {
        let (x, y) = clamp_cursor_position(&frame, cursor.x, cursor.y);
        let idx = (y as usize)
            .saturating_mul(frame.width as usize)
            .saturating_add(x as usize);
        if let Some(cell) = frame.cells.get_mut(idx) {
            cell.modifier ^= REVERSED_MODIFIER;
        }
    }
    frame
}

#[derive(Clone, Copy, Default)]
struct ProfBlitStats {
    scanned_cells: u64,
    changed_cells: u64,
    changed_runs: u64,
}

fn compute_prof_blit_stats(
    frame: &FrameData,
    prev: Option<&FrameData>,
    full: bool,
) -> ProfBlitStats {
    let Some(prev) = prev.filter(|_| !full) else {
        let changed_cells = frame.cells.iter().filter(|cell| !cell.skip).count() as u64;
        return ProfBlitStats {
            scanned_cells: frame.cells.len() as u64,
            changed_cells,
            changed_runs: changed_cells,
        };
    };
    if prev.width != frame.width || prev.height != frame.height {
        let changed_cells = frame.cells.iter().filter(|cell| !cell.skip).count() as u64;
        return ProfBlitStats {
            scanned_cells: frame.cells.len() as u64,
            changed_cells,
            changed_runs: changed_cells,
        };
    }

    let sanitized = sanitized_hyperlinks(&frame.hyperlinks);
    let prev_sanitized = sanitized_hyperlinks(&prev.hyperlinks);
    let mut stats = ProfBlitStats {
        scanned_cells: frame.cells.len() as u64,
        changed_cells: 0,
        changed_runs: 0,
    };
    for row in 0..frame.height {
        let mut in_run = false;
        let mut invalidated = 0usize;
        let mut to_skip = 0usize;
        for col in 0..frame.width {
            let idx = (row as usize) * (frame.width as usize) + (col as usize);
            let cell = &frame.cells[idx];
            let prev_cell = &prev.cells[idx];
            let changed = !cell.skip
                && (!cells_visually_equal(&sanitized, cell, &prev_sanitized, prev_cell)
                    || invalidated > 0)
                && to_skip == 0;
            if changed {
                stats.changed_cells += 1;
                if !in_run {
                    stats.changed_runs += 1;
                    in_run = true;
                }
            } else {
                in_run = false;
            }
            to_skip = cell_width(cell).saturating_sub(1);
            let affected_width = cmp::max(cell_width(cell), cell_width(prev_cell));
            invalidated = cmp::max(affected_width, invalidated).saturating_sub(1);
        }
    }
    stats
}

// ---------------------------------------------------------------------------
// Color → escape sequence
// ---------------------------------------------------------------------------

/// Converts a packed u32 color to an SGR escape sequence fragment.
///
/// Returns a string like `38;5;123` (indexed) or `38;2;255;128;64` (RGB)
/// or `39` (reset), without the leading `\x1b[` or trailing `m`.
fn color_to_sgr_fg(val: u32) -> String {
    match val >> 24 {
        0x00 => match val & 0xFF {
            0x00 => "39".to_owned(), // Reset
            0x01 => "30".to_owned(), // Black
            0x02 => "31".to_owned(), // Red
            0x03 => "32".to_owned(), // Green
            0x04 => "33".to_owned(), // Yellow
            0x05 => "34".to_owned(), // Blue
            0x06 => "35".to_owned(), // Magenta
            0x07 => "36".to_owned(), // Cyan
            0x08 => "37".to_owned(), // Gray (light gray)
            0x09 => "90".to_owned(), // DarkGray
            0x0A => "91".to_owned(), // LightRed
            0x0B => "92".to_owned(), // LightGreen
            0x0C => "93".to_owned(), // LightYellow
            0x0D => "94".to_owned(), // LightBlue
            0x0E => "95".to_owned(), // LightMagenta
            0x0F => "96".to_owned(), // LightCyan
            0x10 => "97".to_owned(), // White
            _ => "39".to_owned(),    // Unknown → Reset
        },
        0x01 => format!("38;5;{}", val & 0xFF), // Indexed
        0x02 => {
            // RGB
            let r = (val >> 16) & 0xFF;
            let g = (val >> 8) & 0xFF;
            let b = val & 0xFF;
            format!("38;2;{r};{g};{b}")
        }
        _ => "39".to_owned(), // Unknown → Reset
    }
}

/// Converts a packed u32 color to a background SGR fragment.
fn color_to_sgr_bg(val: u32) -> String {
    match val >> 24 {
        0x00 => match val & 0xFF {
            0x00 => "49".to_owned(),  // Reset
            0x01 => "40".to_owned(),  // Black
            0x02 => "41".to_owned(),  // Red
            0x03 => "42".to_owned(),  // Green
            0x04 => "43".to_owned(),  // Yellow
            0x05 => "44".to_owned(),  // Blue
            0x06 => "45".to_owned(),  // Magenta
            0x07 => "46".to_owned(),  // Cyan
            0x08 => "47".to_owned(),  // Gray (light gray)
            0x09 => "100".to_owned(), // DarkGray
            0x0A => "101".to_owned(), // LightRed
            0x0B => "102".to_owned(), // LightGreen
            0x0C => "103".to_owned(), // LightYellow
            0x0D => "104".to_owned(), // LightBlue
            0x0E => "105".to_owned(), // LightMagenta
            0x0F => "106".to_owned(), // LightCyan
            0x10 => "107".to_owned(), // White
            _ => "49".to_owned(),     // Unknown → Reset
        },
        0x01 => format!("48;5;{}", val & 0xFF), // Indexed
        0x02 => {
            let r = (val >> 16) & 0xFF;
            let g = (val >> 8) & 0xFF;
            let b = val & 0xFF;
            format!("48;2;{r};{g};{b}")
        }
        _ => "49".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Modifier → SGR
// ---------------------------------------------------------------------------

/// Converts a u16 modifier bitmask to SGR escape sequence fragments.
///
/// Returns a Vec of SGR parameter strings (e.g., "1" for bold, "3" for italic).
fn modifier_to_sgr_parts(val: u16) -> Vec<&'static str> {
    let mut parts = Vec::new();

    // ratatui::Modifier bits (from bitflags)
    const BOLD: u16 = 1 << 0; // 0x01
    const DIM: u16 = 1 << 1; // 0x02
    const ITALIC: u16 = 1 << 2; // 0x04
    const UNDERLINED: u16 = 1 << 3; // 0x08
    const SLOW_BLINK: u16 = 1 << 4; // 0x10
    const RAPID_BLINK: u16 = 1 << 5; // 0x20
    const HIDDEN: u16 = 1 << 7; // 0x80
    const CROSSED_OUT: u16 = 1 << 8; // 0x100

    if val & BOLD != 0 {
        parts.push("1");
    }
    if val & DIM != 0 {
        parts.push("2");
    }
    if val & ITALIC != 0 {
        parts.push("3");
    }
    if val & UNDERLINED != 0 {
        parts.push(match underline_style_from_modifier(val) {
            2 => "4:2",
            3 => "4:3",
            4 => "4:4",
            5 => "4:5",
            _ => "4",
        });
    }
    if val & SLOW_BLINK != 0 {
        parts.push("5");
    }
    if val & RAPID_BLINK != 0 {
        parts.push("6");
    }
    if val & REVERSED_MODIFIER != 0 {
        parts.push("7");
    }
    if val & HIDDEN != 0 {
        parts.push("8");
    }
    if val & CROSSED_OUT != 0 {
        parts.push("9");
    }

    parts
}

/// Builds a complete SGR escape sequence for a cell's style.
fn build_sgr(fg: u32, bg: u32, modifier: u16) -> String {
    let mut parts = vec!["0".to_owned()];
    parts.extend(
        modifier_to_sgr_parts(modifier)
            .into_iter()
            .map(str::to_owned),
    );
    parts.push(color_to_sgr_fg(fg));
    parts.push(color_to_sgr_bg(bg));
    format!("\x1b[{}m", parts.join(";"))
}

// ---------------------------------------------------------------------------
// Blitting
// ---------------------------------------------------------------------------

/// Blits a frame to a writer, diffing against the previous frame.
#[cfg(test)]
fn blit_frame_to(writer: impl Write, frame: &FrameData, prev: Option<&FrameData>) {
    let mut last_visible_cursor = None;
    let mut last_cursor_shape = 0;
    blit_frame_to_with_cursor_memory(
        writer,
        frame,
        prev,
        &mut last_visible_cursor,
        &mut last_cursor_shape,
        false,
    );
}

#[cfg(test)]
fn blit_frame_to_with_cursor_memory(
    writer: impl Write,
    frame: &FrameData,
    prev: Option<&FrameData>,
    last_visible_cursor: &mut Option<(u16, u16)>,
    last_cursor_shape: &mut u8,
    suppress_visible_cursor: bool,
) {
    blit_frame_to_with_cursor_memory_and_policy(
        writer,
        frame,
        prev,
        last_visible_cursor,
        last_cursor_shape,
        repeat_ime_anchor_after_sync(),
        suppress_visible_cursor,
    );
}

#[cfg(test)]
fn blit_frame_to_with_cursor_memory_and_policy(
    writer: impl Write,
    frame: &FrameData,
    prev: Option<&FrameData>,
    last_visible_cursor: &mut Option<(u16, u16)>,
    last_cursor_shape: &mut u8,
    repeat_ime_anchor: bool,
    suppress_visible_cursor: bool,
) {
    blit_frame_to_with_cursor_memory_and_clear_policy(
        writer,
        frame,
        prev,
        last_visible_cursor,
        last_cursor_shape,
        repeat_ime_anchor,
        true,
        suppress_visible_cursor,
    );
}

fn frame_cell_index(frame: &FrameData, x: u16, y: u16) -> Option<usize> {
    (x < frame.width && y < frame.height)
        .then(|| usize::from(y) * usize::from(frame.width) + usize::from(x))
}

fn patch_cell_mut(rows: &mut [PaneSurfacePatchRow], x: u16, y: u16) -> Option<&mut CellData> {
    rows.iter_mut().rev().find_map(|row| {
        if row.y != y || x < row.x {
            return None;
        }
        row.cells.get_mut(usize::from(x - row.x))
    })
}

fn patch_rows_overlap(rows: &[PaneSurfacePatchRow]) -> bool {
    if rows.len() < 2 {
        return false;
    }
    let mut spans = rows
        .iter()
        .map(|row| (row.y, row.x, row.x.saturating_add(row.cells.len() as u16)))
        .collect::<Vec<_>>();
    spans.sort_unstable();
    spans
        .windows(2)
        .any(|pair| pair[0].0 == pair[1].0 && pair[1].1 < pair[0].2)
}

fn patch_row_fits(frame: &FrameData, row: &PaneSurfacePatchRow) -> bool {
    let Ok(len) = u16::try_from(row.cells.len()) else {
        return false;
    };
    if row.y >= frame.height || row.x.saturating_add(len) > frame.width {
        return false;
    }
    let start = usize::from(row.y) * usize::from(frame.width) + usize::from(row.x);
    start
        .checked_add(row.cells.len())
        .is_some_and(|end| end <= frame.cells.len())
}

fn blit_patch_to(
    mut writer: impl Write,
    frame: &FrameData,
    hyperlinks: &[String],
    rows: &[PaneSurfacePatchRow],
    cursor: Option<CursorState>,
    last_visible_cursor: &mut Option<(u16, u16)>,
    last_cursor_shape: &mut u8,
    repeat_ime_anchor: bool,
    suppress_visible_cursor: bool,
) {
    let _ = writer.write_all(b"\x1b[?2026h\x1b[?25l\x1b]8;;\x1b\\");
    let mut last_sgr = String::new();
    let mut last_style = None;
    let mut active_hyperlink = None;
    // A patch may renumber the table, so cells are compared by uri exactly as a full diff
    // compares them.
    let sanitized = sanitized_hyperlinks(hyperlinks);
    let previous_sanitized = sanitized_hyperlinks(&frame.hyperlinks);
    for row in rows {
        let mut invalidated = 0usize;
        let mut to_skip = 0usize;
        let mut next_inline_col = None;
        for (offset, cell) in row.cells.iter().enumerate() {
            let col = row.x + offset as u16;
            let idx = usize::from(row.y) * usize::from(frame.width) + usize::from(col);
            let prev_cell = &frame.cells[idx];
            if !cell.skip
                && (!cells_visually_equal(&sanitized, cell, &previous_sanitized, prev_cell)
                    || invalidated > 0)
                && to_skip == 0
            {
                let cursor_position =
                    (next_inline_col != Some(col) || invalidated > 0).then_some((col, row.y));
                write_cell(
                    &mut writer,
                    cursor_position,
                    cell,
                    &mut last_sgr,
                    &mut last_style,
                    &mut active_hyperlink,
                    hyperlinks,
                );
                next_inline_col = (cell.symbol.is_ascii() && cell_width(cell) == 1)
                    .then_some(col.saturating_add(1));
            }
            to_skip = cell_width(cell).saturating_sub(1);
            let affected_width = cmp::max(cell_width(cell), cell_width(prev_cell));
            invalidated = cmp::max(affected_width, invalidated).saturating_sub(1);
        }
    }
    close_hyperlink(&mut writer, &mut active_hyperlink);
    if !last_sgr.is_empty() {
        let _ = writer.write_all(b"\x1b[0m");
    }

    let cursor_frame = FrameData {
        cells: Vec::new(),
        width: frame.width,
        height: frame.height,
        cursor,
        hyperlinks: Vec::new(),
        graphics: Vec::new(),
    };
    let mut host_cursor = resolve_host_cursor_state(&cursor_frame, last_visible_cursor);
    if suppress_visible_cursor && host_cursor.visible {
        host_cursor.visible = false;
    }
    write_host_cursor_state(&mut writer, host_cursor, last_cursor_shape);
    let _ = writer.write_all(b"\x1b[?2026l");
    if repeat_ime_anchor {
        write_ime_anchor_cursor_state(&mut writer, host_cursor);
    }
    let _ = writer.flush();
}

fn blit_frame_to_with_cursor_memory_and_clear_policy(
    mut writer: impl Write,
    frame: &FrameData,
    prev: Option<&FrameData>,
    last_visible_cursor: &mut Option<(u16, u16)>,
    last_cursor_shape: &mut u8,
    repeat_ime_anchor: bool,
    clear_before_full_redraw: bool,
    suppress_visible_cursor: bool,
) {
    // On first frame or size change, do a full redraw.
    let full_redraw =
        prev.is_none() || prev.is_some_and(|p| p.width != frame.width || p.height != frame.height);

    // Ask terminals that support synchronized output to apply the whole frame
    // atomically. This keeps IMEs and cursor trackers from observing the
    // intermediate CUP positions used while painting changed cells.
    let _ = writer.write_all(b"\x1b[?2026h");

    // Hide cursor before any cell writes to avoid stray cursor artifacts
    // on terminals that render the hardware cursor at intermediate CUP positions.
    let _ = writer.write_all(b"\x1b[?25l");

    // Start each frame from a known OSC 8 state. If a previous write was
    // interrupted or the outer terminal had an active hyperlink, unlinked cells
    // must not inherit it.
    let _ = writer.write_all(b"\x1b]8;;\x1b\\");

    if full_redraw {
        if clear_before_full_redraw {
            let _ = writer.write_all(b"\x1b[2J");
        }
        write_all_cells(&mut writer, frame);
    } else {
        // Diff-based update: only write changed cells.
        let prev = prev.unwrap();
        write_changed_cells(&mut writer, frame, prev);
    }

    // Position the cursor while it is still hidden, then restore visibility.
    // Showing before moving makes slow terminals and IMEs briefly observe the
    // cursor at the last painted cell, which can be an animated sidebar/status
    // cell rather than the focused pane's input position. When the focused pane
    // hides its cursor, still park the host cursor intentionally so IMEs do not
    // anchor to whichever cell happened to be painted last.
    let mut host_cursor = resolve_host_cursor_state(frame, last_visible_cursor);
    if suppress_visible_cursor && host_cursor.visible {
        host_cursor.visible = false;
    }
    write_host_cursor_state(&mut writer, host_cursor, last_cursor_shape);

    // End the synchronized output block immediately after the final cursor
    // state is emitted so supporting terminals can present the frame atomically.
    let _ = writer.write_all(b"\x1b[?2026l");

    // Some native IMEs track candidate-window placement from normal terminal
    // cursor updates and may not observe cursor moves emitted inside synchronized
    // output. Re-emit only the resolved final cursor anchor after the sync block
    // on targets that need it; Windows Terminal exposes that repeat as cursor
    // movement during active TUI repaints.
    if repeat_ime_anchor {
        write_ime_anchor_cursor_state(&mut writer, host_cursor);
    }
    let _ = writer.flush();
}

#[cfg(windows)]
fn repeat_ime_anchor_after_sync() -> bool {
    false
}

#[cfg(not(windows))]
fn repeat_ime_anchor_after_sync() -> bool {
    true
}

/// Writes all cells in the frame (full redraw).
fn cell_width(cell: &CellData) -> usize {
    if is_halfwidth_katakana_voiced_grapheme(&cell.symbol) {
        return 2;
    }
    cell.symbol.width()
}

fn is_halfwidth_katakana_voiced_grapheme(symbol: &str) -> bool {
    let mut chars = symbol.chars();
    let Some(base) = chars.next() else {
        return false;
    };
    let Some(mark) = chars.next() else {
        return false;
    };
    chars.next().is_none()
        && ('\u{ff66}'..='\u{ff9d}').contains(&base)
        && matches!(mark, '\u{ff9e}' | '\u{ff9f}')
}

#[derive(Clone, Copy)]
struct HostCursorState {
    position: (u16, u16),
    visible: bool,
    /// DECSCUSR parameter (0–6). 0 means terminal default.
    shape: u8,
}

fn resolve_host_cursor_state(
    frame: &FrameData,
    last_visible_cursor: &mut Option<(u16, u16)>,
) -> HostCursorState {
    if let Some(cursor) = &frame.cursor {
        if cursor.visible {
            let position = clamp_cursor_position(frame, cursor.x, cursor.y);
            *last_visible_cursor = Some(position);
            return HostCursorState {
                position,
                visible: true,
                shape: normalize_cursor_shape(cursor.shape),
            };
        }

        let position = clamp_cursor_position(frame, cursor.x, cursor.y);
        return HostCursorState {
            position,
            visible: false,
            shape: normalize_cursor_shape(cursor.shape),
        };
    }

    let position = (*last_visible_cursor)
        .map(|(x, y)| clamp_cursor_position(frame, x, y))
        .unwrap_or_else(|| default_hidden_cursor_position(frame));
    HostCursorState {
        position,
        visible: false,
        shape: 0,
    }
}

fn normalize_cursor_shape(shape: u8) -> u8 {
    if shape <= 6 {
        shape
    } else {
        0
    }
}

fn default_hidden_cursor_position(frame: &FrameData) -> (u16, u16) {
    (
        frame.width.saturating_sub(1),
        frame.height.saturating_sub(1),
    )
}

fn clamp_cursor_position(frame: &FrameData, x: u16, y: u16) -> (u16, u16) {
    (
        x.min(frame.width.saturating_sub(1)),
        y.min(frame.height.saturating_sub(1)),
    )
}

fn write_cursor_position(writer: &mut impl Write, (x, y): (u16, u16)) {
    // CUP: move cursor to (row+1, col+1) — 1-based.
    let _ = write!(writer, "\x1b[{};{}H", y + 1, x + 1);
}

fn write_host_cursor_state(writer: &mut impl Write, cursor: HostCursorState, last_shape: &mut u8) {
    write_cursor_position(writer, cursor.position);
    if cursor.shape != *last_shape {
        let _ = write!(writer, "\x1b[{} q", cursor.shape);
        *last_shape = cursor.shape;
    }
    if cursor.visible {
        // Show cursor only after it is already at the final position.
        let _ = writer.write_all(b"\x1b[?25h");
    } else {
        let _ = writer.write_all(b"\x1b[?25l");
    }
}

fn write_ime_anchor_cursor_state(writer: &mut impl Write, cursor: HostCursorState) {
    write_cursor_position(writer, cursor.position);
    if cursor.visible {
        let _ = writer.write_all(b"\x1b[?25h");
    } else {
        let _ = writer.write_all(b"\x1b[?25l");
    }
}

fn write_all_cells(writer: &mut impl Write, frame: &FrameData) {
    let mut last_sgr = String::new();
    let mut last_style = None;
    let mut active_hyperlink = None;
    for row in 0..frame.height {
        let mut to_skip = 0usize;
        let mut next_inline_col = None;
        for col in 0..frame.width {
            if to_skip > 0 {
                to_skip -= 1;
                continue;
            }

            let idx = (row as usize) * (frame.width as usize) + (col as usize);
            let cell = &frame.cells[idx];

            if cell.skip {
                next_inline_col = None;
                continue;
            }

            let cursor_position = (next_inline_col != Some(col)).then_some((col, row));
            write_cell(
                writer,
                cursor_position,
                cell,
                &mut last_sgr,
                &mut last_style,
                &mut active_hyperlink,
                &frame.hyperlinks,
            );
            let width = cell_width(cell);
            next_inline_col =
                (cell.symbol.is_ascii() && width == 1).then_some(col.saturating_add(1));
            to_skip = width.saturating_sub(1);
        }
    }

    close_hyperlink(writer, &mut active_hyperlink);

    // Reset style at the end.
    let _ = writer.write_all(b"\x1b[0m");
}

fn sanitized_hyperlink_uri(uri: &str) -> Option<String> {
    let sanitized: String = uri
        .chars()
        .filter(|ch| *ch != '\x1b' && *ch != '\x07' && !ch.is_control())
        .collect();
    (!sanitized.is_empty()).then_some(sanitized)
}

fn sanitized_hyperlinks(hyperlinks: &[String]) -> Vec<Option<String>> {
    hyperlinks
        .iter()
        .map(|uri| sanitized_hyperlink_uri(uri))
        .collect()
}

fn sanitized_cell_hyperlink_uri<'a>(
    sanitized_hyperlinks: &'a [Option<String>],
    cell: &CellData,
) -> Option<&'a str> {
    let index = cell.hyperlink? as usize;
    sanitized_hyperlinks.get(index)?.as_deref()
}

/// Groups the pieces of one hyperlink for the outer terminal.
///
/// A link is closed and reopened whenever an unlinked cell interrupts it, which pane borders and
/// the sidebar do on every row of a wrapped URL. Without an `id`, OSC 8 only joins cells that are
/// contiguous, so each row would be its own link. The id is derived from the URI so it stays the
/// same across frames and across panes: a frame's hyperlink index is rebuilt per frame and per
/// composited surface, and an id that changed under a repaint would split the link again.
///
/// Two separate occurrences of one URL therefore share an id and highlight together on hover.
fn hyperlink_id(uri: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    uri.hash(&mut hasher);
    hasher.finish()
}

fn write_hyperlink_if_changed(
    writer: &mut impl Write,
    active: &mut Option<String>,
    requested: Option<&str>,
) {
    let requested = requested.and_then(sanitized_hyperlink_uri);
    if active.as_deref() == requested.as_deref() {
        return;
    }

    if active.is_some() {
        let _ = writer.write_all(b"\x1b]8;;\x1b\\");
    }
    *active = requested;
    if let Some(uri) = active.as_deref() {
        let _ = write!(
            writer,
            "\x1b]8;id=herdr-{:016x};{uri}\x1b\\",
            hyperlink_id(uri)
        );
    }
}

fn close_hyperlink(writer: &mut impl Write, active: &mut Option<String>) {
    if active.take().is_some() {
        let _ = writer.write_all(b"\x1b]8;;\x1b\\");
    }
}

fn write_cell(
    writer: &mut impl Write,
    cursor_position: Option<(u16, u16)>,
    cell: &CellData,
    last_sgr: &mut String,
    last_style: &mut Option<(u32, u32, u16)>,
    active_hyperlink: &mut Option<String>,
    hyperlinks: &[String],
) {
    if cell.skip {
        return;
    }

    if let Some(position) = cursor_position {
        write_cursor_position(writer, position);
    }

    let style = (cell.fg, cell.bg, cell.modifier);
    if *last_style != Some(style) {
        let sgr = build_sgr(cell.fg, cell.bg, cell.modifier);
        if sgr != *last_sgr {
            let _ = writer.write_all(sgr.as_bytes());
            *last_sgr = sgr;
        }
        *last_style = Some(style);
    }

    write_hyperlink_if_changed(writer, active_hyperlink, uri_at(hyperlinks, cell.hyperlink));
    let _ = writer.write_all(cell.symbol.as_bytes());
}

/// Writes only the cells that changed between the previous and current frame.
fn cells_visually_equal(
    sanitized_hyperlinks: &[Option<String>],
    cell: &CellData,
    prev_sanitized_hyperlinks: &[Option<String>],
    prev_cell: &CellData,
) -> bool {
    cell.symbol == prev_cell.symbol
        && cell.fg == prev_cell.fg
        && cell.bg == prev_cell.bg
        && cell.modifier == prev_cell.modifier
        && sanitized_cell_hyperlink_uri(sanitized_hyperlinks, cell)
            == sanitized_cell_hyperlink_uri(prev_sanitized_hyperlinks, prev_cell)
    // Skip flag is only for ratatui internal use, not visual.
}

fn write_changed_cells(writer: &mut impl Write, frame: &FrameData, prev: &FrameData) {
    let mut last_sgr = String::new(); // Track last SGR to avoid redundant style changes.
    let mut last_style = None;
    let mut active_hyperlink = None;
    let sanitized = sanitized_hyperlinks(&frame.hyperlinks);
    let prev_sanitized = sanitized_hyperlinks(&prev.hyperlinks);

    for row in 0..frame.height {
        let mut invalidated = 0usize;
        let mut to_skip = 0usize;
        // Herdr clients disable host autowrap, so safe cells can advance inline
        // without spilling into adjacent rows during a resize race.
        let mut next_inline_col = None;

        for col in 0..frame.width {
            let idx = (row as usize) * (frame.width as usize) + (col as usize);
            let cell = &frame.cells[idx];
            let prev_cell = &prev.cells[idx];

            if !cell.skip
                && (!cells_visually_equal(&sanitized, cell, &prev_sanitized, prev_cell)
                    || invalidated > 0)
                && to_skip == 0
            {
                let cursor_position =
                    (next_inline_col != Some(col) || invalidated > 0).then_some((col, row));
                write_cell(
                    writer,
                    cursor_position,
                    cell,
                    &mut last_sgr,
                    &mut last_style,
                    &mut active_hyperlink,
                    &frame.hyperlinks,
                );
                next_inline_col = (cell.symbol.is_ascii() && cell_width(cell) == 1)
                    .then_some(col.saturating_add(1));
            }

            to_skip = cell_width(cell).saturating_sub(1);
            let affected_width = cmp::max(cell_width(cell), cell_width(prev_cell));
            invalidated = cmp::max(affected_width, invalidated).saturating_sub(1);
        }
    }

    close_hyperlink(writer, &mut active_hyperlink);

    // Reset style if we wrote anything.
    if !last_sgr.is_empty() {
        let _ = writer.write_all(b"\x1b[0m");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{CellData, CursorState};

    const WIDE_GRAPHEME: &str = "💡";
    const HALFWIDTH_VOICED_KANA: &str = "ｶ\u{ff9e}";

    fn make_cell(symbol: &str, fg: u32, bg: u32, modifier: u16) -> CellData {
        CellData {
            symbol: symbol.to_owned(),
            fg,
            bg,
            modifier,
            skip: false,
            hyperlink: None,
        }
    }

    fn make_skip_cell(symbol: &str, fg: u32, bg: u32, modifier: u16) -> CellData {
        let mut cell = make_cell(symbol, fg, bg, modifier);
        cell.skip = true;
        cell
    }

    fn make_frame(width: u16, height: u16, cells: Vec<CellData>) -> FrameData {
        FrameData {
            cells,
            width,
            height,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        }
    }

    fn linked_cell(symbol: &str, index: u32) -> CellData {
        let mut cell = make_cell(symbol, 0, 0, 0);
        cell.hyperlink = Some(index);
        cell
    }

    #[test]
    fn color_to_sgr_fg_named_colors() {
        assert_eq!(color_to_sgr_fg(0x00_00_00_00), "39"); // Reset
        assert_eq!(color_to_sgr_fg(0x00_00_00_01), "30"); // Black
        assert_eq!(color_to_sgr_fg(0x00_00_00_02), "31"); // Red
        assert_eq!(color_to_sgr_fg(0x00_00_00_10), "97"); // White
    }

    #[test]
    fn color_to_sgr_fg_indexed() {
        assert_eq!(color_to_sgr_fg(0x01_00_00_AB), "38;5;171");
    }

    #[test]
    fn color_to_sgr_fg_rgb() {
        assert_eq!(color_to_sgr_fg(0x02_FF_80_40), "38;2;255;128;64");
    }

    #[test]
    fn color_to_sgr_bg_named_colors() {
        assert_eq!(color_to_sgr_bg(0x00_00_00_00), "49"); // Reset
        assert_eq!(color_to_sgr_bg(0x00_00_00_01), "40"); // Black
        assert_eq!(color_to_sgr_bg(0x00_00_00_10), "107"); // White
    }

    #[test]
    fn color_to_sgr_bg_rgb() {
        assert_eq!(color_to_sgr_bg(0x02_FF_80_40), "48;2;255;128;64");
    }

    #[test]
    fn modifier_to_sgr_parts_bold() {
        let parts = modifier_to_sgr_parts(1); // BOLD
        assert!(parts.contains(&"1"));
    }

    #[test]
    fn modifier_to_sgr_parts_italic() {
        let parts = modifier_to_sgr_parts(4); // ITALIC
        assert!(parts.contains(&"3"));
    }

    #[test]
    fn modifier_to_sgr_parts_empty() {
        let parts = modifier_to_sgr_parts(0);
        assert!(parts.is_empty());
    }

    #[test]
    fn build_sgr_produces_valid_sequence() {
        let sgr = build_sgr(0x00_00_00_02, 0x00_00_00_01, 1); // fg=Red, bg=Black, bold
        assert!(sgr.starts_with("\x1b["));
        assert!(sgr.ends_with("m"));
        assert!(sgr.contains("0")); // reset existing style first
        assert!(sgr.contains("1")); // bold
        assert!(sgr.contains("31")); // fg red
        assert!(sgr.contains("40")); // bg black
    }

    #[test]
    fn build_sgr_resets_previous_modifiers_when_cell_is_plain() {
        assert_eq!(build_sgr(0x00_00_00_00, 0x00_00_00_00, 0), "\x1b[0;39;49m");
    }

    #[test]
    fn repeated_and_equivalent_styles_keep_text_and_link_changes() {
        let mut cells = vec![
            make_cell("a", 0, 0, 0),
            make_cell("b", 0, 0, 0),
            make_cell("c", 0xff_00_00_00, 0, 0), // Unknown color also encodes as reset.
            make_cell("d", 2, 0, 0),
            make_cell("e", 0, 0, 0),
        ];
        cells[1].hyperlink = Some(0);
        let mut frame = make_frame(5, 1, cells);
        frame.hyperlinks.push("https://example.com".into());
        let mut output = Vec::new();
        write_all_cells(&mut output, &frame);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!(
                "\x1b[1;1H\x1b[0;39;49ma\x1b]8;id=herdr-{:016x};https://example.com\x1b\\b\x1b]8;;\x1b\\c\x1b[0;31;49md\x1b[0;39;49me\x1b[0m",
                hyperlink_id("https://example.com")
            )
        );
    }

    #[test]
    fn build_sgr_preserves_curly_underline_style() {
        let modifier = crate::protocol::modifier_to_u16(
            crate::protocol::modifier_with_underline_style(ratatui::style::Modifier::UNDERLINED, 3),
        );

        assert_eq!(
            build_sgr(0x00_00_00_00, 0x00_00_00_00, modifier),
            "\x1b[0;4:3;39;49m"
        );
    }

    #[test]
    fn cells_are_visually_equal_only_when_symbol_style_and_uri_agree() {
        let links = [Some("https://a.test".to_owned())];
        let other = [Some("https://b.test".to_owned())];
        let a = make_cell("A", 2, 1, 0);
        assert!(cells_visually_equal(&links, &a, &links, &a.clone()));
        assert!(!cells_visually_equal(
            &links,
            &a,
            &links,
            &make_cell("B", 2, 1, 0)
        ));
        assert!(!cells_visually_equal(
            &links,
            &a,
            &links,
            &make_cell("A", 3, 1, 0)
        ));
        assert!(
            !cells_visually_equal(&links, &linked_cell("A", 0), &other, &linked_cell("A", 0)),
            "the same index means a different uri in another table"
        );
    }

    #[test]
    fn blit_frame_hides_cursor_before_full_redraw_writes() {
        let frame = make_frame(
            2,
            2,
            vec![
                make_cell("H", 0, 0, 0),
                make_cell("i", 0, 0, 0),
                make_cell("!", 0, 0, 0),
                make_cell(" ", 0, 0, 0),
            ],
        );

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.starts_with("\x1b[?2026h\x1b[?25l"),
            "should hide cursor inside synchronized frame painting during full redraw"
        );
    }

    #[test]
    fn blit_frame_hides_cursor_before_diff_writes() {
        let prev = make_frame(
            2,
            2,
            vec![
                make_cell("H", 0, 0, 0),
                make_cell("i", 0, 0, 0),
                make_cell("!", 0, 0, 0),
                make_cell(" ", 0, 0, 0),
            ],
        );

        let curr = make_frame(
            2,
            2,
            vec![
                make_cell("X", 0, 0, 0), // Changed
                make_cell("i", 0, 0, 0), // Same
                make_cell("!", 0, 0, 0), // Same
                make_cell(" ", 0, 0, 0), // Same
            ],
        );

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.starts_with("\x1b[?2026h\x1b[?25l"),
            "should hide cursor inside synchronized frame painting during diff"
        );
    }

    #[test]
    fn blit_frame_wraps_frame_in_synchronized_output() {
        let frame = make_frame(1, 1, vec![make_cell("A", 0, 0, 0)]);

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.starts_with("\x1b[?2026h\x1b[?25l"),
            "should begin synchronized output before frame writes"
        );
        let sync_end = output_str
            .find("\x1b[?2026l")
            .expect("should end synchronized output after frame writes");
        assert!(
            sync_end > 0,
            "should end synchronized output after frame writes"
        );
    }

    #[test]
    fn blit_frame_begins_sync_before_hiding_cursor_after_visible_cursor_repeat() {
        let visible = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 2,
                y: 1,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let mut changed = visible.clone();
        changed.cells[0] = make_cell("B", 0, 0, 0);

        let mut last_visible_cursor = None;
        let mut last_cursor_shape = 0;
        let mut first_output = Vec::new();
        blit_frame_to_with_cursor_memory_and_policy(
            &mut first_output,
            &visible,
            None,
            &mut last_visible_cursor,
            &mut last_cursor_shape,
            true,
            false,
        );

        let mut second_output = Vec::new();
        blit_frame_to_with_cursor_memory_and_policy(
            &mut second_output,
            &changed,
            Some(&visible),
            &mut last_visible_cursor,
            &mut last_cursor_shape,
            true,
            false,
        );

        let second_output_str = std::str::from_utf8(&second_output).unwrap();
        assert!(
            second_output_str.starts_with("\x1b[?2026h\x1b[?25l"),
            "next frame should enter synchronized output before hiding the cursor"
        );

        let hide = second_output_str
            .find("\x1b[?25l")
            .expect("second frame should hide cursor before painting");
        let first_paint = second_output_str
            .find("\x1b[1;1H")
            .expect("second frame should paint changed cell");
        assert!(
            hide < first_paint,
            "cursor should still hide before painting"
        );

        first_output.extend_from_slice(&second_output);
        let combined = String::from_utf8(first_output).unwrap();
        assert!(
            combined.contains("\x1b[?2026l\x1b[2;3H\x1b[?25h\x1b[?2026h\x1b[?25l"),
            "post-sync cursor repeat should be followed by a synchronized cursor hide"
        );
    }

    #[test]
    fn blit_frame_can_repeat_final_cursor_state_after_synchronized_output() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 2,
                y: 1,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut last_visible_cursor = None;
        let mut last_cursor_shape = 0;
        let mut output = Vec::new();
        blit_frame_to_with_cursor_memory_and_policy(
            &mut output,
            &frame,
            None,
            &mut last_visible_cursor,
            &mut last_cursor_shape,
            true,
            false,
        );

        let output_str = String::from_utf8(output).unwrap();
        let sync_end = output_str
            .find("\x1b[?2026l")
            .expect("should end synchronized output");
        let trailing_cursor = &output_str[sync_end + "\x1b[?2026l".len()..];
        assert_eq!(
            trailing_cursor, "\x1b[2;3H\x1b[?25h",
            "should expose only the final cursor state after synchronized output"
        );
    }

    #[test]
    fn blit_frame_can_skip_final_cursor_state_after_synchronized_output() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 2,
                y: 1,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut last_visible_cursor = None;
        let mut last_cursor_shape = 0;
        let mut output = Vec::new();
        blit_frame_to_with_cursor_memory_and_policy(
            &mut output,
            &frame,
            None,
            &mut last_visible_cursor,
            &mut last_cursor_shape,
            false,
            false,
        );

        let output_str = String::from_utf8(output).unwrap();
        let sync_end = output_str
            .find("\x1b[?2026l")
            .expect("should end synchronized output");
        let trailing_cursor = &output_str[sync_end + "\x1b[?2026l".len()..];
        assert_eq!(
            trailing_cursor, "",
            "should not expose a post-sync cursor repeat when the target terminal flickers on it"
        );
    }

    #[test]
    fn drawn_cursor_reverses_visible_cursor_cell() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 2,
                y: 1,
                visible: true,
                shape: 6,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let drawn = frame_with_drawn_cursor(frame.clone());

        assert_eq!(drawn.cells[5].modifier, REVERSED_MODIFIER);
        assert_eq!(frame.cells[5].modifier, 0);

        let encoded = BlitEncoder::new().encode_with_suppressed_visible_cursor(&drawn, false);
        let output_str = String::from_utf8(encoded.bytes).unwrap();

        assert!(
            output_str.contains("\x1b[2;3H\x1b[6 q\x1b[?25l"),
            "drawn cursor mode should park the host cursor hidden at the focused cursor position"
        );
        assert!(
            !output_str.contains("\x1b[?25h"),
            "drawn cursor mode should not show the host cursor"
        );
        assert!(
            output_str.contains("\x1b[0;7;39;49mA"),
            "drawn cursor should be emitted as reverse-video cell content"
        );
    }

    #[test]
    fn drawn_cursor_ignores_hidden_cursor() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: false,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        assert_eq!(frame_with_drawn_cursor(frame.clone()), frame);
    }

    #[test]
    fn blit_frame_emits_cursor_shape_before_visibility_without_touching_ime_anchor() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 6,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut last_visible_cursor = None;
        let mut last_cursor_shape = 0;
        let mut output = Vec::new();
        blit_frame_to_with_cursor_memory_and_policy(
            &mut output,
            &frame,
            None,
            &mut last_visible_cursor,
            &mut last_cursor_shape,
            true,
            false,
        );

        let output_str = String::from_utf8(output).unwrap();
        let final_cursor = output_str
            .find("\x1b[1;1H\x1b[6 q\x1b[?25h")
            .expect("should set cursor shape before showing cursor");
        let sync_end = output_str
            .find("\x1b[?2026l")
            .expect("should end synchronized output");
        assert!(
            final_cursor < sync_end,
            "shape should be part of the synchronized final cursor state"
        );
        let trailing_cursor = &output_str[sync_end + "\x1b[?2026l".len()..];
        assert_eq!(
            trailing_cursor, "\x1b[1;1H\x1b[?25h",
            "IME anchor update should preserve the existing position/visibility-only contract"
        );
    }

    #[test]
    fn blit_frame_repeats_explicit_hidden_cursor_anchor_after_synchronized_output() {
        let visible = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let hidden = FrameData {
            cells: vec![make_cell("B", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 2,
                y: 1,
                visible: false,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let mut last_visible_cursor = None;
        let mut last_cursor_shape = 0;
        let mut output = Vec::new();

        blit_frame_to_with_cursor_memory_and_policy(
            &mut output,
            &visible,
            None,
            &mut last_visible_cursor,
            &mut last_cursor_shape,
            true,
            false,
        );
        output.clear();
        blit_frame_to_with_cursor_memory_and_policy(
            &mut output,
            &hidden,
            Some(&visible),
            &mut last_visible_cursor,
            &mut last_cursor_shape,
            true,
            false,
        );

        let output_str = String::from_utf8(output).unwrap();
        let sync_end = output_str
            .find("\x1b[?2026l")
            .expect("should end synchronized output");
        let trailing_cursor = &output_str[sync_end + "\x1b[?2026l".len()..];
        assert_eq!(
            trailing_cursor, "\x1b[2;3H\x1b[?25l",
            "should repeat the explicit hidden cursor position while preserving visibility"
        );
    }

    #[test]
    fn blit_frame_emits_osc8_for_linked_cells() {
        let mut frame = make_frame(
            3,
            1,
            vec![
                linked_cell("L", 0),
                linked_cell("i", 0),
                make_cell("!", 0, 0, 0),
            ],
        );
        frame.hyperlinks.push("https://example.com".to_owned());

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(output_str.contains(&format!(
            "\x1b]8;id=herdr-{:016x};https://example.com\x1b\\L",
            hyperlink_id("https://example.com")
        )));
        assert!(output_str.contains('i'));
        assert!(output_str.contains("\x1b]8;;\x1b\\"));
    }

    #[test]
    fn hyperlink_segments_split_by_unlinked_cells_share_one_id() {
        // A wrapped URL reaches the outer terminal as one linked run per row, with the pane border
        // and whatever sits beside the pane in between. The id is what joins them back together.
        let mut frame = make_frame(
            3,
            2,
            vec![
                linked_cell("h", 0),
                make_cell("|", 0, 0, 0),
                linked_cell("t", 0),
                linked_cell("t", 1),
                make_cell("|", 0, 0, 0),
                linked_cell("p", 1),
            ],
        );
        frame.hyperlinks.push("https://example.com/one".to_owned());
        frame.hyperlinks.push("https://example.com/two".to_owned());

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);
        let output_str = String::from_utf8(output).unwrap();

        let opens = output_str.match_indices("\x1b]8;id=").count();
        assert_eq!(opens, 4, "each interrupted segment reopens its link");
        let first = format!("id=herdr-{:016x};", hyperlink_id("https://example.com/one"));
        let second = format!("id=herdr-{:016x};", hyperlink_id("https://example.com/two"));
        assert_eq!(
            output_str.matches(first.as_str()).count(),
            2,
            "both segments of one url carry the same id"
        );
        assert_eq!(output_str.matches(second.as_str()).count(), 2);
        assert_ne!(first, second, "different urls must not share an id");
    }

    #[test]
    fn blit_frame_sanitizes_hyperlink_uris() {
        let mut frame = make_frame(1, 1, vec![linked_cell("L", 0)]);
        frame
            .hyperlinks
            .push("https://exa\x1b\x07mple.com".to_owned());

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(output_str.contains(&format!(
            "\x1b]8;id=herdr-{:016x};https://example.com\x1b\\L",
            hyperlink_id("https://example.com")
        )));
        assert!(
            !output_str.contains('\x07'),
            "the sanitized uri is what the id is derived from"
        );
    }

    #[test]
    fn blit_frame_first_frame_produces_output() {
        let frame = make_frame(
            2,
            2,
            vec![
                make_cell("H", 0, 0, 0),
                make_cell("i", 0, 0, 0),
                make_cell("!", 0, 0, 0),
                make_cell(" ", 0, 0, 0),
            ],
        );

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        // Full redraw should start with clear screen.
        assert!(
            output_str.contains("\x1b[2J"),
            "full redraw should clear screen"
        );
        assert!(
            output_str.contains('H') || output_str.contains('i'),
            "should contain cell content"
        );
    }

    #[test]
    fn blit_frame_diff_only_writes_changed_cells() {
        let prev = make_frame(
            2,
            2,
            vec![
                make_cell("H", 0, 0, 0),
                make_cell("i", 0, 0, 0),
                make_cell("!", 0, 0, 0),
                make_cell(" ", 0, 0, 0),
            ],
        );

        // Only the first cell changed.
        let curr = make_frame(
            2,
            2,
            vec![
                make_cell("X", 0, 0, 0), // Changed
                make_cell("i", 0, 0, 0), // Same
                make_cell("!", 0, 0, 0), // Same
                make_cell(" ", 0, 0, 0), // Same
            ],
        );

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));

        let output_str = String::from_utf8(output).unwrap();
        // Diff should NOT clear the screen.
        assert!(
            !output_str.contains("\x1b[2J"),
            "diff should not clear screen"
        );
        // Should contain the changed cell content.
        assert!(output_str.contains('X'), "should contain changed cell 'X'");
    }

    #[test]
    fn scroll_sized_ascii_shift_batches_changed_cells_by_row() {
        const WIDTH: u16 = 140;
        const HEIGHT: u16 = 50;
        let prev = make_frame(
            WIDTH,
            HEIGHT,
            vec![make_cell("A", 0, 0, 0); usize::from(WIDTH) * usize::from(HEIGHT)],
        );
        let curr = make_frame(
            WIDTH,
            HEIGHT,
            vec![make_cell("B", 0, 0, 0); usize::from(WIDTH) * usize::from(HEIGHT)],
        );

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));

        let cup_count = output.iter().filter(|&&byte| byte == b'H').count();
        assert!(
            cup_count <= usize::from(HEIGHT) + 2,
            "one dense scroll frame should need at most one CUP per row plus cursor anchors, got {cup_count}"
        );
        assert!(
            output.len() <= 16_290,
            "one dense scroll frame should stay below 25% of the 65,161-byte live baseline, got {} bytes",
            output.len()
        );
    }

    #[test]
    fn batched_ascii_diff_replays_to_current_frame() {
        let prev = make_frame(4, 3, vec![make_cell("A", 0, 0, 0); 12]);
        let curr = make_frame(4, 3, vec![make_cell("B", 0, 0, 0); 12]);
        let mut terminal = crate::ghostty::Terminal::new(4, 3, 0).unwrap();

        let mut initial = Vec::new();
        blit_frame_to(&mut initial, &prev, None);
        terminal.write(&initial);

        let mut diff = Vec::new();
        blit_frame_to(&mut diff, &curr, Some(&prev));
        terminal.write(&diff);

        for row in 0..3 {
            for col in 0..4 {
                let (_, graphemes) = terminal.screen_cell(col, row).unwrap();
                assert_eq!(graphemes, vec![u32::from('B')]);
            }
        }
    }

    #[test]
    fn encoder_size_change_repaints_without_clearing() {
        let prev = make_frame(2, 2, vec![make_cell("A", 0, 0, 0); 4]);
        let curr = make_frame(3, 2, vec![make_cell("B", 0, 0, 0); 6]);
        let mut encoder = BlitEncoder::new();
        let initial = encoder.encode(&prev, false);
        encoder.commit(prev, initial);

        let encoded = encoder.encode(&curr, false);
        assert!(encoded.full);
        let output = String::from_utf8(encoded.bytes).unwrap();

        assert!(!output.contains("\x1b[2J"));
        assert!(output.bytes().filter(|byte| *byte == b'B').count() >= 6);
    }

    #[test]
    fn encoder_forced_repaint_writes_all_cells_without_clearing() {
        let frame = make_frame(3, 2, vec![make_cell("A", 0, 0, 0); 6]);
        let mut encoder = BlitEncoder::new();
        let initial = encoder.encode(&frame, false);
        encoder.commit(frame.clone(), initial);

        let encoded = encoder.encode(&frame, true);
        assert!(encoded.full);
        let output = String::from_utf8(encoded.bytes).unwrap();

        assert!(!output.contains("\x1b[2J"));
        assert!(output.bytes().filter(|byte| *byte == b'A').count() >= 6);
    }

    #[test]
    fn retained_patch_matches_full_diff_and_updates_the_encoder_baseline() {
        let previous = make_frame(
            4,
            2,
            vec![
                make_cell("a", 0, 0, 0),
                make_cell("b", 0, 0, 0),
                make_cell("c", 0, 0, 0),
                make_cell("d", 0, 0, 0),
                make_cell("e", 0, 0, 0),
                make_cell("f", 0, 0, 0),
                make_cell("g", 0, 0, 0),
                make_cell("h", 0, 0, 0),
            ],
        );
        let mut encoder = BlitEncoder::new();
        let initial = encoder.encode(&previous, false);
        encoder.commit(previous.clone(), initial);

        let rows = vec![PaneSurfacePatchRow {
            x: 0,
            y: 1,
            cells: vec![
                make_cell("E", 0, 0, 0),
                make_cell("f", 0, 0, 0),
                make_cell("G", 0, 0, 0),
                make_cell("h", 0, 0, 0),
            ],
        }];
        let cursor = Some(CursorState {
            x: 3,
            y: 1,
            visible: true,
            shape: 2,
        });
        let mut expected = previous;
        expected.cells[4..8].clone_from_slice(&rows[0].cells);
        expected.cursor = cursor.clone();

        let full_diff = encoder.encode(&expected, false);
        let patch = encoder
            .encode_patch(&rows, cursor.clone(), false, None)
            .expect("valid retained patch");
        assert_eq!(patch.bytes, full_diff.bytes);
        assert!(encoder.commit_patch(&rows, cursor, patch, None));
        assert!(encoder.is_current(&expected));
    }

    /// A linked patch has to write exactly the bytes a full diff would, for every way a table can
    /// move under it, or the outer terminal and the baseline drift apart.
    #[test]
    fn retained_linked_patch_matches_full_diff() {
        let a = "https://a.test".to_owned();
        let b = "https://b.test".to_owned();
        let wide = "https://wide.test".to_owned();
        struct Case {
            name: &'static str,
            cells: Vec<CellData>,
            hyperlinks: Vec<String>,
            rows: Vec<PaneSurfacePatchRow>,
            table: Vec<String>,
        }
        let cases = vec![
            Case {
                name: "a link appears",
                cells: vec![make_cell("x", 0, 0, 0), make_cell("y", 0, 0, 0)],
                hyperlinks: Vec::new(),
                rows: vec![PaneSurfacePatchRow {
                    x: 0,
                    y: 0,
                    cells: vec![linked_cell("x", 0)],
                }],
                table: vec![a.clone()],
            },
            Case {
                name: "a link goes away",
                cells: vec![linked_cell("x", 0), make_cell("y", 0, 0, 0)],
                hyperlinks: vec![a.clone()],
                rows: vec![PaneSurfacePatchRow {
                    x: 0,
                    y: 0,
                    cells: vec![make_cell("x", 0, 0, 0)],
                }],
                table: Vec::new(),
            },
            Case {
                name: "the table is renumbered around a cell the patch does not cover",
                cells: vec![linked_cell("x", 0), linked_cell("y", 1)],
                hyperlinks: vec![b.clone(), a.clone()],
                rows: vec![PaneSurfacePatchRow {
                    x: 0,
                    y: 0,
                    cells: vec![linked_cell("x", 1)],
                }],
                table: vec![a.clone(), b.clone()],
            },
            Case {
                name: "the same symbol points somewhere else",
                cells: vec![linked_cell("x", 0), make_cell("y", 0, 0, 0)],
                hyperlinks: vec![a.clone()],
                rows: vec![PaneSurfacePatchRow {
                    x: 0,
                    y: 0,
                    cells: vec![linked_cell("x", 0)],
                }],
                table: vec![b.clone()],
            },
            Case {
                name: "a wide character inside a link",
                cells: vec![make_cell("a", 0, 0, 0), make_skip_cell("", 0, 0, 0)],
                hyperlinks: Vec::new(),
                rows: vec![PaneSurfacePatchRow {
                    x: 0,
                    y: 0,
                    cells: vec![linked_cell("路", 0), {
                        let mut spacer = linked_cell("", 0);
                        spacer.skip = true;
                        spacer
                    }],
                }],
                table: vec![wide.clone()],
            },
        ];

        for Case {
            name,
            cells,
            hyperlinks,
            rows,
            table,
        } in cases
        {
            let mut previous = make_frame(2, 1, cells);
            previous.hyperlinks = hyperlinks;
            let mut encoder = BlitEncoder::new();
            let initial = encoder.encode(&previous, false);
            encoder.commit(previous.clone(), initial);

            let mut expected = previous.clone();
            crate::protocol::surface_links::apply(&mut expected, &rows, table.clone())
                .unwrap_or_else(|error| panic!("{name}: {error}"));

            let full_diff = encoder.encode(&expected, false);
            let patch = encoder
                .encode_patch(&rows, None, false, Some(&table))
                .unwrap_or_else(|| panic!("{name}: patch refused"));
            assert_eq!(
                String::from_utf8_lossy(&patch.bytes),
                String::from_utf8_lossy(&full_diff.bytes),
                "{name}"
            );
            assert!(
                encoder.commit_patch(&rows, None, patch, Some(table)),
                "{name}"
            );
            assert!(encoder.is_current(&expected), "{name}: baseline");
        }
    }

    /// The cell the drawn cursor borrows from the old frame keeps its uri, not its old index.
    #[test]
    fn a_borrowed_cursor_cell_follows_its_uri_into_the_new_table() {
        let a = "https://a.test".to_owned();
        let b = "https://b.test".to_owned();
        let mut previous = make_frame(2, 1, vec![linked_cell("x", 1), make_cell("y", 0, 0, 0)]);
        previous.hyperlinks = vec![b.clone(), a.clone()];
        previous.cursor = Some(CursorState {
            x: 0,
            y: 0,
            visible: true,
            shape: 2,
        });
        let mut encoder = BlitEncoder::new();
        let initial = encoder.encode(&previous, false);
        encoder.commit(previous, initial);

        // The patch drops b.test, so a.test moves to index 0 and the cursor cell has to follow it.
        let rows = vec![PaneSurfacePatchRow {
            x: 1,
            y: 0,
            cells: vec![make_cell("z", 0, 0, 0)],
        }];
        let table = vec![a];
        let cursor = Some(CursorState {
            x: 1,
            y: 0,
            visible: true,
            shape: 2,
        });

        let drawn = encoder
            .patch_rows_with_drawn_cursor(&rows, cursor.as_ref(), Some(&table))
            .expect("drawn cursor rows");

        let borrowed = drawn
            .iter()
            .find(|row| row.x == 0)
            .expect("the cell under the old cursor is repainted");
        assert_eq!(borrowed.cells[0].hyperlink, Some(0), "renumbered by uri");
    }

    #[test]
    fn retained_patch_width_transition_matches_full_diff_with_following_cell() {
        let previous = make_frame(
            3,
            1,
            vec![
                make_cell("界", 0, 0, 0),
                make_cell("z", 0, 0, 0),
                make_cell("q", 0, 0, 0),
            ],
        );
        let mut encoder = BlitEncoder::new();
        let initial = encoder.encode(&previous, false);
        encoder.commit(previous.clone(), initial);

        let rows = vec![PaneSurfacePatchRow {
            x: 0,
            y: 0,
            cells: vec![make_cell("x", 0, 0, 0), make_cell("z", 0, 0, 0)],
        }];
        let mut expected = previous;
        expected.cells[0..2].clone_from_slice(&rows[0].cells);

        let full_diff = encoder.encode(&expected, false);
        let patch = encoder
            .encode_patch(&rows, None, false, None)
            .expect("valid retained patch");
        assert_eq!(patch.bytes, full_diff.bytes);
    }

    #[test]
    fn retained_patch_rejects_overlapping_rows() {
        let frame = make_frame(
            3,
            1,
            vec![
                make_cell("a", 0, 0, 0),
                make_cell("b", 0, 0, 0),
                make_cell("c", 0, 0, 0),
            ],
        );
        let mut encoder = BlitEncoder::new();
        let initial = encoder.encode(&frame, false);
        encoder.commit(frame, initial);
        let rows = vec![
            PaneSurfacePatchRow {
                x: 0,
                y: 0,
                cells: vec![make_cell("A", 0, 0, 0), make_cell("B", 0, 0, 0)],
            },
            PaneSurfacePatchRow {
                x: 1,
                y: 0,
                cells: vec![make_cell("C", 0, 0, 0)],
            },
        ];

        assert!(encoder.encode_patch(&rows, None, false, None).is_none());
        let mut reversed = rows.clone();
        reversed.reverse();
        assert!(encoder.encode_patch(&reversed, None, false, None).is_none());

        // Input order need not match screen order; touching runs are disjoint.
        let disjoint = vec![
            PaneSurfacePatchRow {
                x: 2,
                y: 0,
                cells: vec![make_cell("Z", 0, 0, 0)],
            },
            rows[0].clone(),
        ];
        assert!(encoder.encode_patch(&disjoint, None, false, None).is_some());
    }

    #[test]
    fn metadata_only_patches_do_not_write_but_cursor_changes_do() {
        let mut frame = make_frame(3, 1, vec![make_cell("a", 0, 0, 0); 3]);
        let mut encoder = BlitEncoder::new();
        for cursor in [
            None,
            Some(CursorState {
                x: 0,
                y: 0,
                visible: false,
                shape: 2,
            }),
        ] {
            frame.cursor = cursor.clone();
            let initial = encoder.encode(&frame, false);
            encoder.commit(frame.clone(), initial);
            let encoded = encoder
                .encode_patch(&[], cursor.clone(), false, None)
                .unwrap();
            assert!(encoded.bytes.is_empty());
            assert!(encoder.commit_patch(&[], cursor, encoded, None));
            assert!(encoder.is_current(&frame));
        }
        for cursor in [
            CursorState {
                x: 2,
                y: 0,
                visible: false,
                shape: 2,
            },
            CursorState {
                x: 2,
                y: 0,
                visible: true,
                shape: 2,
            },
        ] {
            let encoded = encoder
                .encode_patch(&[], Some(cursor.clone()), false, None)
                .unwrap();
            assert!(String::from_utf8_lossy(&encoded.bytes).contains("\x1b[1;3H"));
            assert!(encoder.commit_patch(&[], Some(cursor), encoded, None));
        }
        // Switching to a client-drawn cursor must still hide the visible host cursor.
        let encoded = encoder
            .encode_patch(
                &[],
                encoder.last_frame.as_ref().unwrap().cursor.clone(),
                true,
                None,
            )
            .unwrap();
        assert!(String::from_utf8_lossy(&encoded.bytes).contains("\x1b[?25l"));
    }

    #[test]
    fn retained_patch_preserves_the_client_drawn_cursor_overlay() {
        let mut previous = make_frame(
            3,
            1,
            vec![
                make_cell("a", 0, 0, 0),
                make_cell("b", 0, 0, 0),
                make_cell("c", 0, 0, 0),
            ],
        );
        previous.cursor = Some(CursorState {
            x: 0,
            y: 0,
            visible: true,
            shape: 0,
        });
        let previous_drawn = frame_with_drawn_cursor(previous.clone());
        let mut encoder = BlitEncoder::new();
        let initial = encoder.encode_with_suppressed_visible_cursor(&previous_drawn, false);
        encoder.commit(previous_drawn, initial);

        let rows = vec![PaneSurfacePatchRow {
            x: 0,
            y: 0,
            cells: vec![
                make_cell("A", 0, 0, 0),
                make_cell("b", 0, 0, 0),
                make_cell("c", 0, 0, 0),
            ],
        }];
        let cursor = Some(CursorState {
            x: 1,
            y: 0,
            visible: true,
            shape: 0,
        });
        let drawn_rows = encoder
            .patch_rows_with_drawn_cursor(&rows, cursor.as_ref(), None)
            .expect("drawn cursor patch rows");
        let mut expected = previous;
        expected.cells[0..3].clone_from_slice(&rows[0].cells);
        expected.cursor = cursor.clone();
        let expected = frame_with_drawn_cursor(expected);

        let full_diff = encoder.encode_with_suppressed_visible_cursor(&expected, false);
        let patch = encoder
            .encode_patch(&drawn_rows, cursor.clone(), true, None)
            .expect("valid drawn cursor patch");
        assert_eq!(patch.bytes, full_diff.bytes);
        assert!(encoder.commit_patch(&drawn_rows, cursor, patch, None));
        assert!(encoder.is_current(&expected));
    }

    #[test]
    fn blit_frame_positions_cursor() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.contains("\x1b[1;1H"),
            "should position cursor at (1,1)"
        );
    }

    #[test]
    fn blit_frame_hides_cursor_when_invisible() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: false,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.contains("\x1b[?25l"),
            "should hide cursor when invisible"
        );
    }

    #[test]
    fn blit_frame_no_cursor_hides_cursor() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.contains("\x1b[?25l"),
            "should hide cursor when no cursor state"
        );
    }

    #[test]
    fn blit_frame_restores_cursor_visibility() {
        // First frame: cursor hidden.
        let prev = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: false,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &prev, None);
        assert!(
            String::from_utf8(output).unwrap().contains("\x1b[?25l"),
            "first frame should hide cursor"
        );

        // Second frame: cursor visible — should restore visibility.
        let curr = FrameData {
            cells: vec![make_cell("B", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));
        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.contains("\x1b[?25h"),
            "second frame should restore cursor visibility with ?25h"
        );
        assert!(
            output_str.contains("\x1b[1;1H"),
            "should position cursor before showing it"
        );
    }

    #[test]
    fn blit_frame_positions_cursor_before_showing_it() {
        let prev = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let mut curr = prev.clone();
        curr.cells[0] = make_cell("B", 0, 0, 0);
        curr.cursor = Some(CursorState {
            x: 2,
            y: 2,
            visible: true,
            shape: 0,
        });

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));
        let output_str = String::from_utf8(output).unwrap();
        let final_move = output_str
            .rfind("\x1b[3;3H")
            .expect("should move cursor to final position");
        let show = output_str
            .rfind("\x1b[?25h")
            .expect("should show cursor after positioning it");

        assert!(
            final_move < show,
            "should move cursor to final position before showing it"
        );
    }

    #[test]
    fn blit_frame_parks_hidden_cursor_at_last_visible_position() {
        let visible = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: Some(CursorState {
                x: 1,
                y: 1,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let hidden = FrameData {
            cells: vec![make_cell("B", 0, 0, 0); 9],
            width: 3,
            height: 3,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let mut last_visible_cursor = None;
        let mut last_cursor_shape = 0;
        let mut output = Vec::new();

        blit_frame_to_with_cursor_memory(
            &mut output,
            &visible,
            None,
            &mut last_visible_cursor,
            &mut last_cursor_shape,
            false,
        );
        output.clear();
        blit_frame_to_with_cursor_memory(
            &mut output,
            &hidden,
            Some(&visible),
            &mut last_visible_cursor,
            &mut last_cursor_shape,
            false,
        );

        let output_str = String::from_utf8(output).unwrap();
        let park = output_str
            .rfind("\x1b[2;2H")
            .expect("should park hidden cursor at last visible position");
        let hide = output_str
            .rfind("\x1b[?25l")
            .expect("should keep hidden cursor hidden");
        assert!(park < hide, "should park cursor before hiding it");
    }

    #[test]
    fn blit_frame_parks_hidden_cursor_at_bottom_right_without_history() {
        let frame = FrameData {
            cells: vec![make_cell("A", 0, 0, 0); 6],
            width: 3,
            height: 2,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let mut last_visible_cursor = None;
        let mut last_cursor_shape = 0;
        let mut output = Vec::new();

        blit_frame_to_with_cursor_memory(
            &mut output,
            &frame,
            None,
            &mut last_visible_cursor,
            &mut last_cursor_shape,
            false,
        );

        let output_str = String::from_utf8(output).unwrap();
        assert!(
            output_str.contains("\x1b[2;3H\x1b[?25l"),
            "should park hidden cursor at bottom-right before ending the frame"
        );
    }

    #[test]
    fn blit_frame_hides_previous_visible_cursor_when_next_frame_has_none() {
        let prev = FrameData {
            cells: vec![make_cell("A", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: Some(CursorState {
                x: 0,
                y: 0,
                visible: true,
                shape: 0,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let curr = FrameData {
            cells: vec![make_cell("B", 0, 0, 0)],
            width: 1,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));

        assert!(
            String::from_utf8(output).unwrap().contains("\x1b[?25l"),
            "diff redraw should hide a previously visible cursor when the next frame has none"
        );
    }

    #[test]
    fn full_redraw_skips_trailing_cells_covered_by_wide_graphemes() {
        let frame = FrameData {
            cells: vec![
                make_cell(WIDE_GRAPHEME, 0, 0, 0),
                make_cell(" ", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("\x1b[1;1H"));
        assert!(!output_str.contains("\x1b[1;2H"));
        assert!(output_str.contains("\x1b[1;3H"));
    }

    #[test]
    fn full_redraw_skips_trailing_cells_covered_by_halfwidth_voiced_kana() {
        let frame = FrameData {
            cells: vec![
                make_cell(HALFWIDTH_VOICED_KANA, 0, 0, 0),
                make_skip_cell(" ", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &frame, None);
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("\x1b[1;1H"));
        assert!(!output_str.contains("\x1b[1;2H"));
        assert!(output_str.contains("\x1b[1;3H"));
    }

    #[test]
    fn diff_redraw_reveals_cells_hidden_by_previous_wide_graphemes() {
        let prev = FrameData {
            cells: vec![
                make_cell(WIDE_GRAPHEME, 0, 0, 0),
                make_cell(" ", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let curr = FrameData {
            cells: vec![
                make_cell("A", 0, 0, 0),
                make_cell(" ", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("\x1b[1;1H"));
        assert!(
            output_str.contains("\x1b[1;2H"),
            "cells hidden by a previous wide grapheme must be redrawn when they become visible"
        );
    }

    #[test]
    fn diff_redraw_skips_new_trailing_cells_covered_by_wide_graphemes() {
        let prev = FrameData {
            cells: vec![
                make_cell("A", 0, 0, 0),
                make_cell("B", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let curr = FrameData {
            cells: vec![
                make_cell(WIDE_GRAPHEME, 0, 0, 0),
                make_cell(" ", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("\x1b[1;1H"));
        assert!(!output_str.contains("\x1b[1;2H"));
    }

    #[test]
    fn diff_redraw_reveals_cells_hidden_by_previous_halfwidth_voiced_kana() {
        let prev = FrameData {
            cells: vec![
                make_cell(HALFWIDTH_VOICED_KANA, 0, 0, 0),
                make_skip_cell(" ", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let curr = FrameData {
            cells: vec![
                make_cell("A", 0, 0, 0),
                make_cell(" ", 0, 0, 0),
                make_cell("Z", 0, 0, 0),
            ],
            width: 3,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        let mut output = Vec::new();
        blit_frame_to(&mut output, &curr, Some(&prev));
        let output_str = String::from_utf8(output).unwrap();

        assert!(output_str.contains("\x1b[1;1H"));
        assert!(
            output_str.contains("\x1b[1;2H"),
            "cells hidden by a previous halfwidth voiced kana must be redrawn when visible"
        );
    }
}
