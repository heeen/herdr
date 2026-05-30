use std::collections::HashMap;
use std::time::Instant;

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::Span,
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use crate::app::state::{MenuListState, ViewLayout};
use crate::app::Mode;
use crate::client::supervisor::{AgentSidebarRow, ServerId};
use crate::detect::AgentState;
use crate::protocol::{CellData, CursorState, FrameData};
use crate::terminal::{TerminalId, TerminalRuntimeRegistry, TerminalState};

pub(crate) const DEFAULT_SIDEBAR_WIDTH: u16 = 26;

pub(crate) struct ClientCompositor {
    sidebar_width: u16,
    workspace_scroll: usize,
    agent_panel_scroll: usize,
    resizing_sidebar: bool,
    animation_tick: u32,                                  // item 5
    hover: Option<crate::app::state::SidebarHoverTarget>, // item 7
    // item 5 freshness, key = (server_id, agent_id):
    working_since: HashMap<(ServerId, String), std::time::Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarHitTarget {
    Filter,
    Workspace {
        server_id: crate::client::supervisor::ServerId,
        workspace_id: String,
    },
    Agent {
        server_id: crate::client::supervisor::ServerId,
        agent_id: String,
    },
    NewWorkspaceDestination {
        server_id: crate::client::supervisor::ServerId,
    },
    ClientGlobalMenuItem {
        index: usize,
    },
    New,
    Menu,
    // item 1: composited-modal action buttons (centered ratatui modals).
    AddRemoteSubmit,
    AddRemoteCancel,
    NewWorkspacePickerConfirm,
    NewWorkspacePickerCancel,
    // item 3 (Area 5): remote-management overlay targets.
    RemoteManageRow {
        index: usize,
    },
    RemoteManageAdd,
    RemoteManageConfirmDelete,
    RemoteManageCancelDelete,
}

#[derive(Clone)]
struct WorkspaceRoute {
    server_id: ServerId,
    workspace_id: Option<String>,
    disabled: bool,
}

#[derive(Clone)]
struct AgentRoute {
    server_id: ServerId,
    agent_id: String,
}

struct ClientSidebarSnapshot {
    app: crate::app::AppState,
    filter_label: String,
    workspace_routes: Vec<WorkspaceRoute>,
    agent_routes: Vec<AgentRoute>,
    // overlay carriers (items 1 & 3), all ui-owned/cloned — see Area 3:
    add_remote_form: Option<crate::client::supervisor::AddRemoteForm>, // item 1
    new_workspace_picker: Option<(Vec<crate::client::supervisor::ServerDestination>, usize)>, // item 1
    // item 3: the overlay state plus the snapshot of secondary rows it renders. The closure maps
    // `RemoteManageRow` -> ui-owned `RemoteManageRowView` before calling `render_*` (layering).
    remote_manage: Option<(
        crate::client::supervisor::RemoteManageOverlay,
        Vec<crate::client::supervisor::RemoteManageRow>,
    )>,
}

impl ClientCompositor {
    pub(crate) fn new(sidebar_width: u16) -> Self {
        Self {
            sidebar_width,
            workspace_scroll: 0,
            agent_panel_scroll: 0,
            resizing_sidebar: false,
            animation_tick: 0,
            hover: None,
            working_since: HashMap::new(),
        }
    }

    pub(crate) fn sidebar_width(&self) -> u16 {
        self.sidebar_width
    }

    /// Advance the single client-owned animation clock by `step`. Called ONLY from the
    /// `run_client_loop` `Timer` arm (never during render). `from_model` reads it into
    /// `AppState.spinner_tick`; items 2/7 consume the SAME tick (no second clock).
    pub(crate) fn advance_animation_tick(&mut self, step: u32) {
        self.animation_tick = self.animation_tick.wrapping_add(step);
    }

    pub(crate) fn animation_tick(&self) -> u32 {
        self.animation_tick
    }

    /// Insert/refresh the working-start instant for `(server_id, agent_id)`. Called by the
    /// event-loop upkeep helper before compose so the live duration timer survives recompose.
    pub(crate) fn seed_working_since(&mut self, key: (ServerId, String), now: Instant) {
        self.working_since.entry(key).or_insert(now);
    }

    /// Drop every working-start instant whose key is not in `keep`. Keeps the map bounded to
    /// currently-Working agents (option (a) freshness, contract Area 1 / Area 7 §6).
    pub(crate) fn retain_working_since<F>(&mut self, mut keep: F)
    where
        F: FnMut(&(ServerId, String)) -> bool,
    {
        self.working_since.retain(|key, _| keep(key));
    }

    /// item 7: update the client-truth sidebar hover target, returning whether it changed. The
    /// caller redraws only on a change so a same-row motion sweep coalesces to zero redraws.
    pub(crate) fn set_hover(
        &mut self,
        next: Option<crate::app::state::SidebarHoverTarget>,
    ) -> bool {
        let changed = self.hover != next;
        self.hover = next;
        changed
    }

    /// item 7: the current client-truth hover target. Read by the `Moved` dispatch so motion off
    /// the sidebar still clears a stale highlight, and by render mirroring in `from_model`.
    pub(crate) fn hover(&self) -> Option<crate::app::state::SidebarHoverTarget> {
        self.hover
    }

    #[cfg(test)]
    pub(crate) fn working_since_len(&self) -> usize {
        self.working_since.len()
    }

    #[cfg(test)]
    pub(crate) fn working_since_at(&self, key: &(ServerId, String)) -> Option<Instant> {
        self.working_since.get(key).copied()
    }

    pub(crate) fn handle_sidebar_resize_mouse(
        &mut self,
        mouse: &crossterm::event::MouseEvent,
        host_width: u16,
        host_height: u16,
        settings: &crate::api::schema::UiSettingsInfo,
    ) -> Option<(u16, u16)> {
        use crossterm::event::{MouseButton, MouseEventKind};

        let sidebar_width = self.effective_sidebar_width(host_width);
        let divider_col = sidebar_width.checked_sub(1)?;
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if mouse.column == divider_col => {
                self.resizing_sidebar = true;
                Some(self.content_size(host_width, host_height))
            }
            MouseEventKind::Drag(MouseButton::Left) if self.resizing_sidebar => {
                self.set_sidebar_width_from_column(
                    mouse.column,
                    host_width,
                    settings.sidebar_min_width,
                    settings.sidebar_max_width,
                );
                Some(self.content_size(host_width, host_height))
            }
            MouseEventKind::Up(MouseButton::Left) if self.resizing_sidebar => {
                self.resizing_sidebar = false;
                Some(self.content_size(host_width, host_height))
            }
            _ => None,
        }
    }

    pub(crate) fn handle_sidebar_scroll_mouse(
        &mut self,
        model: &crate::client::supervisor::ClientSupervisorModel,
        mouse: &crossterm::event::MouseEvent,
        host_width: u16,
        host_height: u16,
    ) -> Option<bool> {
        use crossterm::event::MouseEventKind;

        let delta = match mouse.kind {
            MouseEventKind::ScrollUp => -1,
            MouseEventKind::ScrollDown => 1,
            _ => return None,
        };
        let sidebar_width = self.effective_sidebar_width(host_width);
        if sidebar_width == 0
            || host_height == 0
            || mouse.column >= sidebar_width
            || mouse.row >= host_height
        {
            return None;
        }

        let snapshot = ClientSidebarSnapshot::from_model(
            model,
            self,
            sidebar_width,
            host_width,
            host_height,
            Instant::now(),
        );
        let (_, detail_area) = crate::ui::expanded_sidebar_sections(
            snapshot.app.view.sidebar_rect,
            snapshot.app.sidebar_section_split,
        );
        let over_agent_panel = detail_area != Rect::default()
            && mouse.row >= detail_area.y
            && mouse.row < detail_area.y.saturating_add(detail_area.height);

        if over_agent_panel {
            let metrics = crate::ui::agent_panel_scroll_metrics(&snapshot.app, detail_area);
            if !crate::ui::should_show_scrollbar(metrics) {
                return Some(false);
            }
            let next = scrolled_offset(snapshot.app.agent_panel_scroll, delta, metrics);
            let changed = next != snapshot.app.agent_panel_scroll;
            self.agent_panel_scroll = next;
            return Some(changed);
        }

        let area = crate::ui::workspace_list_rect(
            snapshot.app.view.sidebar_rect,
            snapshot.app.sidebar_section_split,
        );
        let metrics = crate::ui::workspace_list_scroll_metrics(&snapshot.app, area);
        if !crate::ui::should_show_scrollbar(metrics) {
            return Some(false);
        }
        let next = scrolled_offset(snapshot.app.workspace_scroll, delta, metrics);
        let changed = next != snapshot.app.workspace_scroll;
        self.workspace_scroll = next;
        Some(changed)
    }

    fn set_sidebar_width_from_column(
        &mut self,
        column: u16,
        host_width: u16,
        configured_min_width: u16,
        configured_max_width: u16,
    ) {
        if host_width <= 1 {
            self.sidebar_width = host_width;
            return;
        }
        let max_width = configured_max_width.min(host_width.saturating_sub(1));
        let min_width = configured_min_width.min(max_width);
        self.sidebar_width = column.saturating_add(1).clamp(min_width, max_width);
    }

    pub(crate) fn compose_frame(
        &self,
        model: &crate::client::supervisor::ClientSupervisorModel,
        active_frame: &FrameData,
        host_width: u16,
        host_height: u16,
        now: Instant,
    ) -> FrameData {
        let sidebar_width = self.effective_sidebar_width(host_width);
        let content_width = host_width.saturating_sub(sidebar_width);
        let snapshot = ClientSidebarSnapshot::from_model(
            model,
            self,
            sidebar_width,
            host_width,
            host_height,
            now,
        );
        // The composited client overlays (add-remote / new-workspace picker / manage-remotes) float
        // over the live content like the global launcher menu: they are footer-anchored popups (NOT
        // centered, full-screen-dimmed modals). The content copy must protect EXACTLY the open
        // popup's rect(s) — so the overlay stays visible AND the rest of the content shows around
        // it. Both render and these exclusion rects derive from the SAME `*_popup_rect(anchor_area)`
        // helpers, so what we protect lines up cell-for-cell with what gets drawn. Without an open
        // overlay we fall back to protecting just the open global-menu rect.
        let anchor_area = self.overlay_anchor_area(model, host_width, host_height);
        let mut excluded_rects: Vec<Rect> = Vec::new();
        if model.add_remote_form().is_some() {
            excluded_rects.extend(crate::ui::add_remote_popup_rect(anchor_area));
        } else if let Some((dests, _)) = snapshot.new_workspace_picker.as_ref() {
            excluded_rects.extend(crate::ui::new_workspace_picker_popup_rect(
                anchor_area,
                dests.len(),
            ));
        } else if let Some((overlay, rows)) = snapshot.remote_manage.as_ref() {
            excluded_rects.extend(crate::ui::remote_manage_popup_rect(anchor_area, rows.len()));
            if overlay.confirm_delete.is_some() {
                excluded_rects.extend(crate::ui::remote_manage_confirm_popup_rect(anchor_area));
            }
        } else {
            excluded_rects.extend(snapshot.global_menu_rect());
        }
        let mut frame = render_client_shell(&snapshot, host_width, host_height);

        copy_active_content_excluding(
            active_frame,
            &mut frame,
            sidebar_width,
            content_width,
            &excluded_rects,
        );

        // item 1/3: the add-remote / new-workspace-picker / manage modals are rendered as ratatui
        // widgets inside `render_client_shell` (composited). Here we only force the cursor hidden
        // while ANY modal is open so the real terminal cursor never leaks through the modal.
        if model.add_remote_form().is_some()
            || model.new_workspace_picker().is_some()
            || model.remote_manage_overlay().is_some()
        {
            frame.cursor = None;
        } else {
            frame.cursor =
                offset_cursor(active_frame.cursor.as_ref(), sidebar_width, content_width);
        }
        frame.hyperlinks = active_frame.hyperlinks.clone();
        if sidebar_width == 0 {
            frame.graphics = active_frame.graphics.clone();
        }

        frame
    }

    /// The footer-anchored `anchor_area` the composited client overlays (add-remote /
    /// new-workspace picker / manage-remotes) are positioned within: it spans the host top down to
    /// the sidebar footer row, so the popups open upward from the footer like the global launcher
    /// menu (instead of dead-centered). Render, content-copy exclusion, hit-test and hover-test all
    /// derive overlay geometry from this SAME rect, so they cannot drift.
    pub(crate) fn overlay_anchor_area(
        &self,
        model: &crate::client::supervisor::ClientSupervisorModel,
        host_width: u16,
        host_height: u16,
    ) -> Rect {
        let sidebar_width = self.effective_sidebar_width(host_width);
        let snapshot = ClientSidebarSnapshot::from_model(
            model,
            self,
            sidebar_width,
            host_width,
            host_height,
            Instant::now(),
        );
        Rect::new(0, 0, host_width, snapshot.app.sidebar_footer_rect().y)
    }

    pub(crate) fn content_size(&self, host_width: u16, host_height: u16) -> (u16, u16) {
        (
            host_width
                .saturating_sub(self.effective_sidebar_width(host_width))
                .max(1),
            host_height,
        )
    }

    fn effective_sidebar_width(&self, host_width: u16) -> u16 {
        if host_width <= 1 {
            return 0;
        }
        self.sidebar_width.min(host_width.saturating_sub(1))
    }

    pub(crate) fn hit_test(
        &self,
        model: &crate::client::supervisor::ClientSupervisorModel,
        x: u16,
        y: u16,
        host_width: u16,
        host_height: u16,
    ) -> Option<SidebarHitTarget> {
        let sidebar_width = self.effective_sidebar_width(host_width);
        if sidebar_width == 0 || host_height == 0 || y >= host_height {
            return None;
        }

        let snapshot = ClientSidebarSnapshot::from_model(
            model,
            self,
            sidebar_width,
            host_width,
            host_height,
            Instant::now(),
        );

        if let Some(target) = hit_test_global_menu(&snapshot.app, x, y) {
            return Some(target);
        }

        // item 1: the composited overlays are footer-anchored popups that float over the live
        // content, so their hit-test runs before the sidebar-width guard. Geometry is derived from
        // the SAME shared helpers the renderer uses (`new_workspace_picker_inner_rect`/`_row_rect`/
        // `add_remote_inner_rect` + the button-rect helpers) over the SAME `anchor_area`,
        // guaranteeing render == hit_test.
        let anchor_area = Rect::new(0, 0, host_width, snapshot.app.sidebar_footer_rect().y);
        if let Some(target) = hit_test_new_workspace_picker(&snapshot, anchor_area, x, y) {
            return Some(target);
        }
        if let Some(target) = hit_test_add_remote(&snapshot, anchor_area, x, y) {
            return Some(target);
        }
        // item 3 (Area 5): the manage overlay intercepts the whole host rect first (so a click on
        // a sidebar workspace row while the overlay is open never resolves to a `Workspace` hit).
        if snapshot.remote_manage.is_some() {
            return hit_test_remote_manage(&snapshot, anchor_area, x, y);
        }

        if x >= sidebar_width {
            return None;
        }

        if rect_contains(
            filter_label_rect(snapshot.app.view.sidebar_rect, &snapshot.filter_label),
            x,
            y,
        ) {
            return Some(SidebarHitTarget::Filter);
        }
        if rect_contains(snapshot.app.sidebar_new_button_rect(), x, y) {
            return Some(SidebarHitTarget::New);
        }
        if rect_contains(snapshot.app.global_launcher_rect(), x, y) {
            return Some(SidebarHitTarget::Menu);
        }

        for card in &snapshot.app.view.workspace_card_areas {
            if rect_contains(card.rect, x, y) {
                let route = snapshot.workspace_routes.get(card.ws_idx)?;
                if route.disabled {
                    return None;
                }
                return route.workspace_id.clone().map(|workspace_id| {
                    SidebarHitTarget::Workspace {
                        server_id: route.server_id.clone(),
                        workspace_id,
                    }
                });
            }
        }

        hit_test_agent_panel(&snapshot, x, y)
    }

    /// item 7 (Area 4): resolve a mouse-motion position to a sidebar hover target, sharing the
    /// SAME `ClientSidebarSnapshot` + rect checks as `hit_test` so render geometry and hover
    /// geometry cannot drift. Returns `None` (no highlight) for:
    /// - a collapsed/zero-width sidebar (`effective_sidebar_width == 0`),
    /// - an open add-remote form / global menu / manage overlay — those own their own hover (the
    ///   global menu moves its highlight on motion via `client_global_menu_item_at`, handled in the
    ///   client `Moved` arm before this fn), so the sidebar must not fight them,
    /// - positions outside the sidebar content,
    /// - disabled remote rows and `None`-`workspace_id` placeholders (matches `hit_test`),
    /// - non-selectable layout rows (divider/banner-skip + headers/separator — they produce no
    ///   card), and undrawn affordances (the ` new`/`menu` gate is `app.mouse_capture`).
    ///
    /// The new-workspace picker is a centered modal that DOES hover (its destination rows resolve
    /// to `NewWorkspaceDestination { row }`, before the sidebar-width guard, like `hit_test`).
    /// Never issues server traffic.
    pub(crate) fn hover_test(
        &self,
        model: &crate::client::supervisor::ClientSupervisorModel,
        x: u16,
        y: u16,
        host_width: u16,
        host_height: u16,
    ) -> Option<crate::app::state::SidebarHoverTarget> {
        use crate::app::state::SidebarHoverTarget;

        let sidebar_width = self.effective_sidebar_width(host_width);
        if sidebar_width == 0 || host_height == 0 || y >= host_height {
            return None;
        }

        // An open add-remote form / global menu / manage overlay owns input; the sidebar hover
        // must yield so the existing overlay hover is authoritative. The global menu moves its
        // highlight on motion (`client_global_menu_item_at`), handled in the client `Moved` arm.
        if model.client_global_menu_highlighted().is_some()
            || model.add_remote_form().is_some()
            || model.remote_manage_overlay().is_some()
        {
            return None;
        }

        let snapshot = ClientSidebarSnapshot::from_model(
            model,
            self,
            sidebar_width,
            host_width,
            host_height,
            Instant::now(),
        );

        // The new-workspace picker is a footer-anchored popup (item 1), so it hovers before the
        // sidebar-width guard — the SAME order/geometry `hit_test` uses for it.
        let anchor_area = Rect::new(0, 0, host_width, snapshot.app.sidebar_footer_rect().y);
        if let Some(target) = hover_test_new_workspace_picker(&snapshot, anchor_area, x, y) {
            return Some(target);
        }
        // While the picker is open the dimmed sidebar beneath is inert (matches `hit_test`).
        if snapshot.new_workspace_picker.is_some() {
            return None;
        }

        if x >= sidebar_width {
            return None;
        }

        if rect_contains(
            filter_label_rect(snapshot.app.view.sidebar_rect, &snapshot.filter_label),
            x,
            y,
        ) {
            return Some(SidebarHoverTarget::Filter);
        }
        // Affordance hover respects the SAME draw gate as the renderer (`app.mouse_capture` at
        // `sidebar.rs`): the ` new`/`menu` affordances only hover when they are actually drawn.
        if snapshot.app.mouse_capture {
            if rect_contains(snapshot.app.sidebar_new_button_rect(), x, y) {
                return Some(SidebarHoverTarget::New);
            }
            if rect_contains(snapshot.app.global_launcher_rect(), x, y) {
                return Some(SidebarHoverTarget::Menu);
            }
        }

        // host-banner rect (item 2): hoverable as `HostBanner { banner_idx }` when drawn. The
        // banner rows produce no `WorkspaceCardArea`, so they are skipped by the card loop below.
        for banner in &snapshot.app.view.host_banner_areas {
            if rect_contains(banner.rect, x, y) {
                return Some(SidebarHoverTarget::HostBanner {
                    banner_idx: banner.banner_idx,
                });
            }
        }

        // item-4 space-divider rows are non-selectable (they produce no card, so the card loop
        // below would skip them). Resolve them to the defensive `Divider` target, which render
        // treats as NO-highlight (a stable `None`-equivalent). Render never lifts a divider row —
        // the contract's "hover never highlights the divider" (Decision 4).
        if snapshot.app.view.divider_rows.contains(&y) {
            return Some(SidebarHoverTarget::Divider);
        }

        for card in &snapshot.app.view.workspace_card_areas {
            if rect_contains(card.rect, x, y) {
                let route = snapshot.workspace_routes.get(card.ws_idx)?;
                // disabled remote rows and `None`-id placeholders are not selectable → no hover
                // (matches `hit_test`'s rejection so click and hover agree).
                if route.disabled || route.workspace_id.is_none() {
                    return None;
                }
                return Some(SidebarHoverTarget::Workspace {
                    ws_idx: card.ws_idx,
                });
            }
        }

        hover_test_agent_panel(&snapshot, x, y)
    }

    /// item 7: resolve a mouse-motion position to a 0-based item index in the open client global
    /// menu, or `None` when the menu is closed / the position misses it. Shares the SAME snapshot +
    /// `global_menu_item_index_at` geometry as `hit_test`, so motion-driven highlight and click
    /// resolve identical rows. The client `Moved` arm feeds the result to
    /// `model.hover_client_global_menu_item`, mirroring the monolithic host's `global_menu.hover`.
    pub(crate) fn client_global_menu_item_at(
        &self,
        model: &crate::client::supervisor::ClientSupervisorModel,
        x: u16,
        y: u16,
        host_width: u16,
        host_height: u16,
    ) -> Option<usize> {
        let sidebar_width = self.effective_sidebar_width(host_width);
        if sidebar_width == 0
            || host_height == 0
            || model.client_global_menu_highlighted().is_none()
        {
            return None;
        }
        let snapshot = ClientSidebarSnapshot::from_model(
            model,
            self,
            sidebar_width,
            host_width,
            host_height,
            Instant::now(),
        );
        global_menu_item_index_at(&snapshot.app, x, y)
    }
}

fn scrolled_offset(current: usize, delta: i16, metrics: crate::pane::ScrollMetrics) -> usize {
    if delta.is_negative() {
        current.saturating_sub(delta.unsigned_abs() as usize)
    } else {
        current
            .saturating_add(delta as usize)
            .min(metrics.max_offset_from_bottom)
    }
}

impl Default for ClientCompositor {
    fn default() -> Self {
        Self::new(DEFAULT_SIDEBAR_WIDTH)
    }
}

impl ClientSidebarSnapshot {
    fn from_model(
        model: &crate::client::supervisor::ClientSupervisorModel,
        compositor: &ClientCompositor,
        sidebar_width: u16,
        host_width: u16,
        host_height: u16,
        now: Instant,
    ) -> Self {
        let mut app = crate::app::AppState::empty_for_client_rendering();
        let settings = model.ui_settings();
        app.sidebar_width = sidebar_width;
        app.default_sidebar_width = settings.sidebar_default_width;
        app.sidebar_min_width = settings.sidebar_min_width;
        app.sidebar_max_width = settings.sidebar_max_width;
        app.sidebar_section_split = settings.sidebar_section_split();
        app.sidebar_space = settings.sidebar_spaces.clone();
        app.sidebar_agent = settings.sidebar_agents.clone();
        // item 2 (C3): host-banner styling rides UiSettingsInfo over the wire.
        app.sidebar_host = settings.sidebar_host.clone();
        app.global_menu_extra_labels = vec!["add remote", "manage remotes"];
        app.view.layout = ViewLayout::Desktop;
        app.view.sidebar_rect = Rect::new(0, 0, sidebar_width, host_height);
        app.view.terminal_area = Rect::new(
            sidebar_width,
            0,
            host_width.saturating_sub(sidebar_width),
            host_height,
        );
        app.mode = match model.client_global_menu_highlighted() {
            Some(highlighted) => {
                app.global_menu = MenuListState::new(
                    highlighted.min(app.global_menu_labels().len().saturating_sub(1)),
                );
                Mode::GlobalMenu
            }
            None => Mode::Navigate,
        };

        let mut agents_by_workspace = HashMap::<(ServerId, String), Vec<AgentSidebarRow>>::new();
        for group in model.agent_groups() {
            agents_by_workspace
                .entry((group.server_id, group.workspace_id))
                .or_default()
                .extend(group.agents);
        }

        let mut workspace_routes = Vec::new();
        let mut agent_routes = Vec::new();
        let mut active_idx = None;
        let workspace_rows = model.workspace_rows();
        for (idx, row) in workspace_rows.into_iter().enumerate() {
            let agents = row
                .workspace_id
                .as_ref()
                .and_then(|workspace_id| {
                    agents_by_workspace.remove(&(row.server_id.clone(), workspace_id.clone()))
                })
                .unwrap_or_default();
            let focused_agent_idx = agents.iter().position(|agent| agent.focused);
            if row.focused || focused_agent_idx.is_some() {
                active_idx = Some(idx);
            }

            let mut pane_terminals = Vec::new();
            for agent in &agents {
                let terminal_id = TerminalId::alloc();
                let (state, seen) = agent_state_from_status(&agent.status);
                let mut terminal = TerminalState::new(terminal_id.clone(), "/".into());
                terminal.set_agent_name(agent.label.clone());
                if state == AgentState::Working {
                    // Working agents: seed working_since = the persisted start instant so the
                    // live duration timer survives recompose. The map is upkept in the event
                    // loop (pre-compose); here we only READ it. A first-seen agent falls back
                    // to `now`, so its first frame shows ~0s. A fresh terminal starts in
                    // `Unknown`, so the Unknown→Working transition fires (not short-circuited)
                    // and `recompute_effective_state` sets `working_since = Some(started)`.
                    let started = compositor
                        .working_since
                        .get(&(row.server_id.clone(), agent.agent_id.clone()))
                        .copied()
                        .unwrap_or(now);
                    terminal.set_detected_state_with_screen_signals_at(
                        None, // agent: no detected Agent on the client path
                        crate::detect::AgentState::Working,
                        false,   // visible_blocker
                        false,   // visible_idle
                        true,    // visible_working
                        false,   // process_exited
                        started, // now == persisted working-start instant
                    );
                } else {
                    terminal.state = state;
                }
                app.terminals.insert(terminal_id.clone(), terminal);
                pane_terminals.push((terminal_id, seen));
                agent_routes.push(AgentRoute {
                    server_id: row.server_id.clone(),
                    agent_id: agent.agent_id.clone(),
                });
            }

            let workspace_id = row
                .workspace_id
                .clone()
                .unwrap_or_else(|| format!("client-sidebar-row-{idx}"));
            let (workspace, _) = crate::workspace::Workspace::sidebar_placeholder(
                workspace_id,
                row.label.clone(),
                row.branch.clone(),
                pane_terminals,
                focused_agent_idx,
            );
            app.workspaces.push(workspace);
            // item 4: mirror the per-row local/remote signal into AppState, index-aligned with
            // app.workspaces. Empty in monolithic mode (no rows), so monolithic emits no divider.
            app.client_workspace_remote.push(row.is_remote);
            workspace_routes.push(WorkspaceRoute {
                server_id: row.server_id,
                workspace_id: row.workspace_id,
                disabled: row.disabled,
            });
        }

        if !app.workspaces.is_empty() {
            let selected = active_idx.unwrap_or(0).min(app.workspaces.len() - 1);
            app.active = Some(selected);
            app.selected = selected;
        }
        app.workspace_scroll = crate::ui::normalized_workspace_scroll(
            &app,
            app.view.sidebar_rect,
            compositor.workspace_scroll,
        );
        let (_, detail_area) =
            crate::ui::expanded_sidebar_sections(app.view.sidebar_rect, app.sidebar_section_split);
        app.agent_panel_scroll = compositor
            .agent_panel_scroll
            .min(crate::ui::agent_panel_scroll_metrics(&app, detail_area).max_offset_from_bottom);
        // item 2 (C3): populate the per-host banner specs (one per visible Secondary, in
        // visible_servers() order) and the coordination flag BEFORE computing geometry, so that
        // `workspace_list_entries` emits the HostBanner rows and flips the divider to plain. The
        // banner specs ride positionally: `HostBannerArea.banner_idx` indexes `app.host_banners`.
        let host_banner_specs = model.host_banner_specs();
        // The insertion index from `host_banner_specs` is a position in the flat
        // `workspace_rows()` stream, which is 1:1 with `app.workspaces` (each row pushed in
        // order above) — so it is a valid `ws_idx`. `host_banner_rows[i]` is the workspace the
        // i-th banner is emitted immediately before; `host_banners[i]` is its spec.
        app.host_banner_rows = host_banner_specs.iter().map(|(idx, _)| *idx).collect();
        app.host_banners = host_banner_specs
            .into_iter()
            .map(|(_, spec)| spec)
            .collect();
        app.host_banner_active = model.host_banner_active();
        // item 4: one pass produces card rects, host-banner rects (item 2), and divider rows,
        // so render and hit-test share one geometry source. `host_banner_areas` is populated
        // from the second slot of THIS single call (render == hit_test geometry), and
        // `host_banner_active` (set above) flips the divider to plain when a banner is live.
        let (cards, banners, dividers) =
            crate::ui::compute_workspace_list_areas_full(&app, app.view.sidebar_rect);
        app.view.workspace_card_areas = cards;
        app.view.host_banner_areas = banners;
        app.view.divider_rows = dividers;
        // item 5: feed the single client-owned animation tick into the rendered AppState so
        // the braille agent spinner advances (was frozen at 0 via empty_for_client_rendering).
        app.spinner_tick = compositor.animation_tick();
        // item 7 (Area 4): mirror the compositor's hover truth into the render snapshot (Copy;
        // pure read). Render reads `app.sidebar_hover` and never mutates it.
        app.set_sidebar_hover(compositor.hover);

        Self {
            app,
            filter_label: model.filter_label(),
            workspace_routes,
            agent_routes,
            // item 1: clone the overlay state out of the model into ui-owned carriers (pure read).
            // The closure maps these into ui view structs before rendering.
            add_remote_form: model.add_remote_form().cloned(),
            new_workspace_picker: model
                .new_workspace_picker()
                .map(|picker| (picker.destinations.clone(), picker.selected)),
            // item 3: clone the overlay state + the secondary rows it renders out of the model
            // (pure read). The render closure maps the rows into ui-owned views.
            remote_manage: model
                .remote_manage_overlay()
                .map(|overlay| (overlay.clone(), model.remote_manage_rows())),
        }
    }

    fn global_menu_rect(&self) -> Option<Rect> {
        matches!(self.app.mode, Mode::GlobalMenu).then(|| self.app.global_menu_rect())
    }
}

fn render_client_shell(
    snapshot: &ClientSidebarSnapshot,
    host_width: u16,
    host_height: u16,
) -> FrameData {
    if host_width == 0 || host_height == 0 {
        return blank_frame(host_width, host_height);
    }

    let backend = ratatui::backend::TestBackend::new(host_width, host_height);
    let mut terminal = ratatui::Terminal::new(backend).expect("TestBackend::new should not fail");
    let terminal_runtimes = TerminalRuntimeRegistry::new();
    terminal
        .draw(|frame| {
            crate::ui::render_sidebar(
                &snapshot.app,
                &terminal_runtimes,
                frame,
                snapshot.app.view.sidebar_rect,
            );
            render_filter_label(snapshot, frame);
            if matches!(snapshot.app.mode, Mode::GlobalMenu) {
                crate::ui::render_global_launcher_menu(&snapshot.app, frame);
            }
            // item 1: render the composited client overlays as footer-anchored popups that float
            // over the live content — the proven `render_global_launcher_menu` compositing path.
            // `anchor_area` spans the host top down to the sidebar footer row so the popups open
            // upward from the footer (matching the launcher menu), NOT dead-centered. The
            // compositor maps the ui-owned snapshot carriers into ui view structs here (no
            // supervisor types reach `ui`).
            let anchor_area = Rect::new(0, 0, host_width, snapshot.app.sidebar_footer_rect().y);
            if let Some((dests, selected)) = &snapshot.new_workspace_picker {
                let views: Vec<crate::ui::DestinationView> = dests
                    .iter()
                    .map(|d| crate::ui::DestinationView {
                        display_name: &d.display_name,
                    })
                    .collect();
                // item 7 (Area 4): pass the hovered destination row (mirrored into the snapshot)
                // so the modal lifts it; the picker's `Moved` resolves `NewWorkspaceDestination`.
                let hovered_row = match snapshot.app.sidebar_hover {
                    Some(crate::app::state::SidebarHoverTarget::NewWorkspaceDestination {
                        row,
                    }) => Some(row as usize),
                    _ => None,
                };
                crate::ui::render_new_workspace_picker_overlay(
                    &snapshot.app.palette,
                    &views,
                    *selected,
                    hovered_row,
                    frame,
                    anchor_area,
                );
            }
            if let Some(form) = &snapshot.add_remote_form {
                let view = crate::ui::AddRemoteOverlayView {
                    target: &form.target,
                    name: &form.name,
                    focused_is_target: form.focused_field
                        == crate::client::supervisor::AddRemoteField::Target,
                    error: form.error.as_deref(),
                    in_progress: form.in_progress,
                    spinner: crate::ui::spinner_frame(snapshot.app.spinner_tick),
                };
                crate::ui::render_add_remote_overlay(
                    &snapshot.app.palette,
                    &view,
                    frame,
                    anchor_area,
                );
            }
            // item 3 (Area 5): render the remote-management overlay as a footer-anchored popup. The
            // compositor maps the supervisor rows into ui-owned views here (no supervisor types
            // reach `ui`).
            if let Some((overlay, rows)) = &snapshot.remote_manage {
                let views = model_remote_manage_row_views(rows);
                crate::ui::render_remote_manage_overlay(
                    &snapshot.app.palette,
                    &views,
                    overlay.selected,
                    overlay.scroll,
                    overlay.confirm_delete.as_deref(),
                    frame,
                    anchor_area,
                );
            }
        })
        .expect("render to TestBackend should not fail");

    let buffer = terminal.backend().buffer().clone();
    FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[])
}

fn render_filter_label(snapshot: &ClientSidebarSnapshot, frame: &mut ratatui::Frame) {
    let rect = filter_label_rect(snapshot.app.view.sidebar_rect, &snapshot.filter_label);
    if rect == Rect::default() {
        return;
    }
    // item 7 (Area 4): the filter label hover lifts its fg overlay0 → subtext0.
    let fg = if snapshot.app.sidebar_hover == Some(crate::app::state::SidebarHoverTarget::Filter) {
        snapshot.app.palette.subtext0
    } else {
        snapshot.app.palette.overlay0
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            snapshot.filter_label.clone(),
            Style::default().fg(fg).add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Right),
        rect,
    );
}

fn filter_label_rect(sidebar: Rect, label: &str) -> Rect {
    if sidebar.width <= 1 || sidebar.height == 0 || label.is_empty() {
        return Rect::default();
    }
    let content_width = sidebar.width.saturating_sub(1);
    let width = (UnicodeWidthStr::width(label) as u16).min(content_width);
    Rect::new(
        sidebar.x + content_width.saturating_sub(width),
        sidebar.y,
        width,
        1,
    )
}

fn agent_state_from_status(status: &str) -> (AgentState, bool) {
    match status {
        "working" => (AgentState::Working, true),
        "blocked" => (AgentState::Blocked, true),
        "done" => (AgentState::Idle, false),
        "idle" => (AgentState::Idle, true),
        _ => (AgentState::Unknown, true),
    }
}

/// Whether anything on the client sidebar is currently animating, gating the animation
/// cadence (no idle CPU spin). Read-only over the cached model; performs NO I/O. The ONLY
/// banner-active input is `host_banner_animation_active` (contract Area 1: do not invent a
/// second clock or second flag); item 2 fills the banner hook, until then it is `false`.
pub(crate) fn sidebar_wants_animation(
    model: &crate::client::supervisor::ClientSupervisorModel,
) -> bool {
    model
        .agent_groups()
        .iter()
        .any(|g| g.agents.iter().any(|r| r.status == "working"))
        || model.host_banner_animation_active()
        || model.add_remote_in_progress()
}

/// item 3 (Area 5): map the supervisor `RemoteManageRow`s into ui-owned `RemoteManageRowView`s
/// (borrowing the row strings). This keeps `client::supervisor` types out of `ui` (the one-way
/// layering rule, contradiction 13).
fn model_remote_manage_row_views(
    rows: &[crate::client::supervisor::RemoteManageRow],
) -> Vec<crate::ui::RemoteManageRowView<'_>> {
    use crate::client::supervisor::RemoteManageState;
    rows.iter()
        .map(|row| crate::ui::RemoteManageRowView {
            glyph: match row.state {
                RemoteManageState::Connected => crate::ui::RemoteStateGlyph::Connected,
                RemoteManageState::Connecting => crate::ui::RemoteStateGlyph::Connecting,
                RemoteManageState::Disconnected => crate::ui::RemoteStateGlyph::Disconnected,
                RemoteManageState::Disabled => crate::ui::RemoteStateGlyph::Disabled,
                RemoteManageState::ProtocolMismatch => {
                    crate::ui::RemoteStateGlyph::ProtocolMismatch
                }
            },
            name: &row.name,
            target: &row.target,
            state_word: row.state.state_word(),
            disabled: !row.enabled,
        })
        .collect()
}

/// item 1: hit-test the centered new-workspace picker modal. Returns a destination row target,
/// the confirm/cancel buttons, or `None` when the picker is closed or the click misses. Geometry
/// is derived from the SAME helpers the renderer uses (`new_workspace_picker_inner_rect`/`_row_rect`
/// + `new_workspace_picker_button_rects`) over the same `full_rect`, so render == hit_test.
fn hit_test_new_workspace_picker(
    snapshot: &ClientSidebarSnapshot,
    full_rect: Rect,
    x: u16,
    y: u16,
) -> Option<SidebarHitTarget> {
    let (destinations, _) = snapshot.new_workspace_picker.as_ref()?;
    let inner = crate::ui::new_workspace_picker_inner_rect(full_rect, destinations.len())?;

    // buttons take precedence over the (overlapping) actions row.
    let (confirm_rect, cancel_rect) = crate::ui::new_workspace_picker_button_rects(inner);
    if rect_contains(confirm_rect, x, y) {
        return Some(SidebarHitTarget::NewWorkspacePickerConfirm);
    }
    if rect_contains(cancel_rect, x, y) {
        return Some(SidebarHitTarget::NewWorkspacePickerCancel);
    }

    // destination rows — same `max_rows` clamp the renderer applies.
    let max_rows = inner.height.saturating_sub(3) as usize;
    for (row_index, destination) in destinations.iter().enumerate().take(max_rows) {
        let row = crate::ui::new_workspace_picker_row_rect(inner, row_index);
        if rect_contains(row, x, y) {
            return Some(SidebarHitTarget::NewWorkspaceDestination {
                server_id: destination.server_id.clone(),
            });
        }
    }
    None
}

/// item 7 (Area 4): hover sibling of `hit_test_new_workspace_picker`. Returns
/// `NewWorkspaceDestination { row }` (keyed on the modal's logical row index, which the modal
/// render keys on) for a hovered destination row. The confirm/cancel buttons have their own
/// styling and are not hover targets. Uses the SAME centered geometry the renderer + hit-test
/// use, so render == hover_test for the modal rows.
fn hover_test_new_workspace_picker(
    snapshot: &ClientSidebarSnapshot,
    full_rect: Rect,
    x: u16,
    y: u16,
) -> Option<crate::app::state::SidebarHoverTarget> {
    let (destinations, _) = snapshot.new_workspace_picker.as_ref()?;
    let inner = crate::ui::new_workspace_picker_inner_rect(full_rect, destinations.len())?;
    let max_rows = inner.height.saturating_sub(3) as usize;
    for row_index in 0..destinations.len().min(max_rows) {
        let row = crate::ui::new_workspace_picker_row_rect(inner, row_index);
        if rect_contains(row, x, y) {
            return Some(
                crate::app::state::SidebarHoverTarget::NewWorkspaceDestination {
                    row: row_index as u16,
                },
            );
        }
    }
    None
}

/// item 1: hit-test the centered add-remote modal's submit/cancel buttons. Returns `None` when the
/// form is closed or the click misses. Uses the shared fixed `add_remote_inner_rect` geometry.
fn hit_test_add_remote(
    snapshot: &ClientSidebarSnapshot,
    full_rect: Rect,
    x: u16,
    y: u16,
) -> Option<SidebarHitTarget> {
    snapshot.add_remote_form.as_ref()?;
    let inner = crate::ui::add_remote_inner_rect(full_rect)?;
    let (submit_rect, cancel_rect) = crate::ui::add_remote_button_rects(inner);
    if rect_contains(submit_rect, x, y) {
        return Some(SidebarHitTarget::AddRemoteSubmit);
    }
    if rect_contains(cancel_rect, x, y) {
        return Some(SidebarHitTarget::AddRemoteCancel);
    }
    None
}

/// item 3 (Area 5): hit-test the centered remote-management overlay. When delete-confirm is
/// active the red popup OWNS input (its buttons are the only hit targets; list rows are inert).
/// Otherwise a click on a rendered row selects it, and the footer `add` affordance opens the
/// add-remote form. Geometry comes from the SAME shared helpers the renderer uses
/// (`remote_manage_inner_rect`/`_row_rect`/`_confirm_*`), guaranteeing render == hit_test.
fn hit_test_remote_manage(
    snapshot: &ClientSidebarSnapshot,
    full_rect: Rect,
    x: u16,
    y: u16,
) -> Option<SidebarHitTarget> {
    let (overlay, rows) = snapshot.remote_manage.as_ref()?;

    // delete-confirm sub-state: only the popup buttons are hit-testable.
    if overlay.confirm_delete.is_some() {
        let popup = crate::ui::remote_manage_confirm_popup_rect(full_rect)?;
        let inner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (delete_rect, cancel_rect) = crate::ui::remote_manage_confirm_button_rects(inner);
        if rect_contains(delete_rect, x, y) {
            return Some(SidebarHitTarget::RemoteManageConfirmDelete);
        }
        if rect_contains(cancel_rect, x, y) {
            return Some(SidebarHitTarget::RemoteManageCancelDelete);
        }
        return None;
    }

    let inner = crate::ui::remote_manage_inner_rect(full_rect, rows.len())?;

    // footer hint row hosts the `add` affordance (whole footer row).
    let footer = Rect::new(
        inner.x,
        inner.y + inner.height.saturating_sub(1),
        inner.width,
        1,
    );
    if rect_contains(footer, x, y) {
        return Some(SidebarHitTarget::RemoteManageAdd);
    }

    // rows — same `max_rows`/visible-window clamp the renderer applies.
    let max_rows = inner.height.saturating_sub(3) as usize;
    let selected = overlay.selected.min(rows.len().saturating_sub(1));
    let start = overlay
        .scroll
        .min(rows.len().saturating_sub(max_rows.max(1)))
        .min(selected)
        .max(crate::ui::open_existing_worktree_visible_start(
            selected,
            max_rows.max(1),
        ));
    for (visible_idx, (row_index, _)) in rows
        .iter()
        .enumerate()
        .skip(start)
        .take(max_rows)
        .enumerate()
    {
        let rect = crate::ui::remote_manage_row_rect(inner, visible_idx);
        if rect_contains(rect, x, y) {
            return Some(SidebarHitTarget::RemoteManageRow { index: row_index });
        }
    }
    None
}

/// Shared geometry for the open global launcher menu: resolve a position to a 0-based item index,
/// or `None` when the menu is closed or the position misses the menu's inner item rows. Both
/// `hit_test_global_menu` (click) and `client_global_menu_item_at` (motion) resolve through this so
/// click and hover geometry cannot drift from the `render_global_launcher_menu` row layout.
fn global_menu_item_index_at(app: &crate::app::AppState, x: u16, y: u16) -> Option<usize> {
    if !matches!(app.mode, Mode::GlobalMenu) {
        return None;
    }
    let rect = app.global_menu_rect();
    let inner_x = rect.x.saturating_add(1);
    let inner_y = rect.y.saturating_add(1);
    let inner_right = rect.x.saturating_add(rect.width).saturating_sub(1);
    let inner_bottom = rect.y.saturating_add(rect.height).saturating_sub(1);
    if x < inner_x || x >= inner_right || y < inner_y || y >= inner_bottom {
        return None;
    }
    let index = (y - inner_y) as usize;
    (index < app.global_menu_labels().len()).then_some(index)
}

fn hit_test_global_menu(app: &crate::app::AppState, x: u16, y: u16) -> Option<SidebarHitTarget> {
    global_menu_item_index_at(app, x, y)
        .map(|index| SidebarHitTarget::ClientGlobalMenuItem { index })
}

fn hit_test_agent_panel(
    snapshot: &ClientSidebarSnapshot,
    x: u16,
    y: u16,
) -> Option<SidebarHitTarget> {
    let (_, detail_area) = crate::ui::expanded_sidebar_sections(
        snapshot.app.view.sidebar_rect,
        snapshot.app.sidebar_section_split,
    );
    let metrics = crate::ui::agent_panel_scroll_metrics(&snapshot.app, detail_area);
    let body =
        crate::ui::agent_panel_body_rect(detail_area, crate::ui::should_show_scrollbar(metrics));
    if !rect_contains(body, x, y) {
        return None;
    }

    let entry_rows = crate::ui::agent_panel_entry_row_count(&snapshot.app);
    if entry_rows == 0 {
        return None;
    }
    let relative_row = y.saturating_sub(body.y);
    let stride = entry_rows.saturating_add(1);
    let index = (relative_row / stride) as usize;
    if relative_row % stride >= entry_rows {
        return None;
    }
    let route = snapshot
        .agent_routes
        .get(snapshot.app.agent_panel_scroll.saturating_add(index))?;
    Some(SidebarHitTarget::Agent {
        server_id: route.server_id.clone(),
        agent_id: route.agent_id.clone(),
    })
}

/// item 7 (Area 4): hover sibling of `hit_test_agent_panel`. Returns `AgentRoute { route_idx }`
/// where `route_idx = agent_panel_scroll + index` — the SAME flat `agent_routes` index
/// `hit_test_agent_panel` resolves. The index is positional in `model.agent_groups()` order, so
/// it survives recompose (a captured `pane_id` would not, contradiction 11). The client snapshot
/// is always `AgentPanelScope::AllWorkspaces`, so this flat index equals the global
/// `agent_panel_entries` index `render_agent_detail` walks (render == hover_test geometry).
fn hover_test_agent_panel(
    snapshot: &ClientSidebarSnapshot,
    x: u16,
    y: u16,
) -> Option<crate::app::state::SidebarHoverTarget> {
    let (_, detail_area) = crate::ui::expanded_sidebar_sections(
        snapshot.app.view.sidebar_rect,
        snapshot.app.sidebar_section_split,
    );
    let metrics = crate::ui::agent_panel_scroll_metrics(&snapshot.app, detail_area);
    let body =
        crate::ui::agent_panel_body_rect(detail_area, crate::ui::should_show_scrollbar(metrics));
    if !rect_contains(body, x, y) {
        return None;
    }

    let entry_rows = crate::ui::agent_panel_entry_row_count(&snapshot.app);
    if entry_rows == 0 {
        return None;
    }
    let relative_row = y.saturating_sub(body.y);
    let stride = entry_rows.saturating_add(1);
    let index = (relative_row / stride) as usize;
    if relative_row % stride >= entry_rows {
        return None;
    }
    let route_idx = snapshot.app.agent_panel_scroll.saturating_add(index);
    // only a real agent route resolves (the gap rows / over-scroll resolve to None).
    snapshot.agent_routes.get(route_idx)?;
    Some(crate::app::state::SidebarHoverTarget::AgentRoute { route_idx })
}

fn rect_contains(rect: Rect, x: u16, y: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

fn blank_frame(width: u16, height: u16) -> FrameData {
    FrameData {
        cells: vec![blank_cell(); (width as usize) * (height as usize)],
        width,
        height,
        cursor: None,
        hyperlinks: Vec::new(),
        graphics: Vec::new(),
    }
}

fn blank_cell() -> CellData {
    CellData {
        symbol: " ".into(),
        fg: 0,
        bg: 0,
        modifier: 0,
        skip: false,
        hyperlink: None,
    }
}

fn copy_active_content_excluding(
    active_frame: &FrameData,
    target: &mut FrameData,
    target_x: u16,
    target_width: u16,
    excluded_rects: &[Rect],
) {
    let copy_width = target_width.min(active_frame.width);
    let copy_height = target.height.min(active_frame.height);
    for row in 0..copy_height {
        for col in 0..copy_width {
            let source_idx = (row as usize) * (active_frame.width as usize) + (col as usize);
            let target_col = target_x + col;
            if excluded_rects
                .iter()
                .any(|rect| rect_contains(*rect, target_col, row))
            {
                continue;
            }
            let target_idx = (row as usize) * (target.width as usize) + (target_col as usize);
            if let (Some(source), Some(target_cell)) = (
                active_frame.cells.get(source_idx),
                target.cells.get_mut(target_idx),
            ) {
                *target_cell = source.clone();
            }
        }
    }
}

fn offset_cursor(
    cursor: Option<&CursorState>,
    sidebar_width: u16,
    content_width: u16,
) -> Option<CursorState> {
    let cursor = cursor?;
    if cursor.x >= content_width {
        return None;
    }
    Some(CursorState {
        x: sidebar_width + cursor.x,
        y: cursor.y,
        visible: cursor.visible,
        shape: cursor.shape,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::supervisor::{
        AgentSummary, ClientSupervisorModel, NewWorkspaceRoute, ServerId, ServerSummary,
        WorkspaceSummary,
    };
    use crate::protocol::CursorState;
    use std::time::Duration;

    fn cell(symbol: &str) -> CellData {
        CellData {
            symbol: symbol.into(),
            fg: 0,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    fn frame(width: u16, height: u16, rows: &[&str]) -> FrameData {
        let mut cells = Vec::new();
        for row in 0..height as usize {
            let line = rows.get(row).copied().unwrap_or("");
            for col in 0..width as usize {
                let symbol = line
                    .chars()
                    .nth(col)
                    .map(|ch| ch.to_string())
                    .unwrap_or_else(|| " ".into());
                cells.push(cell(&symbol));
            }
        }
        FrameData {
            cells,
            width,
            height,
            cursor: Some(CursorState {
                x: 1,
                y: 1,
                visible: true,
                shape: 2,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        }
    }

    /// The footer-anchored `anchor_area` the renderer/hit-test derive the composited client
    /// overlays from: spans the host top down to the sidebar footer row. Tests derive their
    /// expected popup coordinates from this SAME rect so render geometry == hit-test geometry.
    fn anchor_area(
        model: &ClientSupervisorModel,
        compositor: &ClientCompositor,
        host_w: u16,
        host_h: u16,
    ) -> Rect {
        compositor.overlay_anchor_area(model, host_w, host_h)
    }

    fn row_text(frame: &FrameData, row: u16) -> String {
        (0..frame.width)
            .map(|col| {
                frame.cells[(row as usize) * (frame.width as usize) + (col as usize)]
                    .symbol
                    .as_str()
            })
            .collect()
    }

    #[test]
    fn compose_frame_draws_server_sidebar_shell_and_offsets_active_content() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model
            .set_summary(
                &ServerId::main(),
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "main-herdr".into(),
                        label: "herdr".into(),
                        branch: Some("master".into()),
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-api".into(),
                        label: "api".into(),
                        branch: Some("feature/api".into()),
                        focused: false,
                    }],
                    agents: vec![AgentSummary {
                        agent_id: "remote-agent".into(),
                        workspace_id: "remote-api".into(),
                        label: "claude".into(),
                        status: "idle".into(),
                        focused: false,
                    }],
                },
            )
            .unwrap();

        let content = frame(8, 3, &["content", "frame"]);
        // item 2 (C3): the host banner adds a row to the spaces list, so render at a taller
        // sidebar to keep the remote card's branch line on screen.
        let composed = ClientCompositor::new(26).compose_frame(
            &model,
            &content,
            60,
            28,
            std::time::Instant::now(),
        );

        assert_eq!(composed.width, 60);
        assert_eq!(composed.height, 28);
        let rows: Vec<_> = (0..composed.height)
            .map(|row| row_text(&composed, row))
            .collect();
        assert!(row_text(&composed, 0).starts_with(" spaces"));
        assert!(row_text(&composed, 0)
            .chars()
            .take(25)
            .collect::<String>()
            .ends_with("all"));
        assert_eq!(composed.cells[25].symbol, "│");
        assert!(rows.iter().any(|row| row.contains("herdr")));
        assert!(rows.iter().any(|row| row.contains("master")));
        // item 2 (C3): bare space label "api" (host "x" now lives in the banner row above).
        assert!(rows.iter().any(|row| row.contains("api")));
        assert!(rows.iter().any(|row| row.contains("feature/api")));
        assert!(rows.iter().any(|row| row.starts_with(" agents")));
        assert!(rows.iter().any(|row| row.contains("claude")));
        let row0_content: String = row_text(&composed, 0).chars().skip(26).collect();
        let row1_content: String = row_text(&composed, 1).chars().skip(26).collect();
        assert!(row0_content.starts_with("content"));
        assert!(row1_content.starts_with("frame"));
        assert_eq!(
            composed.cursor,
            Some(CursorState {
                x: 27,
                y: 1,
                visible: true,
                shape: 2,
            })
        );
    }

    #[test]
    fn compose_frame_uses_main_ui_settings_for_sidebar_fields() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-api".into(),
                        label: "api".into(),
                        branch: Some("feature/api".into()),
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        let mut settings = crate::api::schema::UiSettingsInfo::default();
        crate::app::state::SidebarSpaceItem::Branch
            .set_enabled(&mut settings.sidebar_spaces, false);
        crate::app::state::SidebarSpaceItem::BranchStatus
            .set_enabled(&mut settings.sidebar_spaces, false);
        model.set_ui_settings(settings);

        let content = frame(8, 3, &["content", "frame"]);
        let composed = ClientCompositor::new(26).compose_frame(
            &model,
            &content,
            60,
            16,
            std::time::Instant::now(),
        );
        let rows: Vec<_> = (0..composed.height)
            .map(|row| row_text(&composed, row))
            .collect();

        // item 2 (C3): the workspace label is now the bare space name (the host name lives in
        // the banner above), and the branch column is disabled by the ui-settings overrides.
        assert!(rows.iter().any(|row| row.contains("api")));
        assert!(!rows.iter().any(|row| row.contains("feature/api")));
    }

    #[test]
    fn content_size_reserves_sidebar_width_and_keeps_one_column_minimum() {
        let compositor = ClientCompositor::new(12);

        assert_eq!(compositor.content_size(80, 24), (68, 24));
        assert_eq!(compositor.content_size(8, 24), (1, 24));
    }

    #[test]
    fn compose_frame_reserves_content_column_when_host_is_narrower_than_sidebar() {
        let model = ClientSupervisorModel::new("local");
        let compositor = ClientCompositor::new(12);
        let content = frame(1, 1, &["x"]);

        let composed = compositor.compose_frame(&model, &content, 8, 3, std::time::Instant::now());

        assert_eq!(composed.width, 8);
        assert_eq!(composed.cells[7].symbol, "x");
    }

    #[test]
    fn filter_label_rect_uses_display_width_for_wide_text() {
        let rect = filter_label_rect(Rect::new(0, 0, 6, 1), "전체");

        assert_eq!(rect.x, 1);
        assert_eq!(rect.width, 4);
    }

    #[test]
    fn hit_test_uses_server_sidebar_geometry() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model
            .set_summary(
                &ServerId::main(),
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "main-herdr".into(),
                        label: "herdr".into(),
                        branch: None,
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-api".into(),
                        label: "api".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: vec![AgentSummary {
                        agent_id: "remote-agent".into(),
                        workspace_id: "remote-api".into(),
                        label: "claude".into(),
                        status: "idle".into(),
                        focused: false,
                    }],
                },
            )
            .unwrap();

        let compositor = ClientCompositor::new(26);
        // Derive row geometry from the same snapshot render uses (render == hit_test): the main
        // card, the divider, item 2's host banner, and the remote card all come from one pass.
        let snapshot =
            ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 28, Instant::now());
        let main_card = snapshot
            .app
            .view
            .workspace_card_areas
            .iter()
            .find(|c| c.ws_idx == 0)
            .expect("main card");
        let remote_card = snapshot
            .app
            .view
            .workspace_card_areas
            .iter()
            .find(|c| c.ws_idx == 1)
            .expect("remote card");
        let divider_y = snapshot.app.view.divider_rows[0];
        let banner_y = snapshot.app.view.host_banner_areas[0].rect.y;

        assert_eq!(
            compositor.hit_test(&model, 23, 0, 60, 28),
            Some(SidebarHitTarget::Filter)
        );
        assert_eq!(
            compositor.hit_test(&model, 1, main_card.rect.y, 60, 28),
            Some(SidebarHitTarget::Workspace {
                server_id: ServerId::main(),
                workspace_id: "main-herdr".into(),
            })
        );
        // item 4: the local→remote divider row resolves to no workspace. item 2 (C3): the host
        // banner row (below the divider, above the remote card) also resolves to no workspace.
        assert!(!matches!(
            compositor.hit_test(&model, 1, divider_y, 60, 28),
            Some(SidebarHitTarget::Workspace { .. })
        ));
        assert!(!matches!(
            compositor.hit_test(&model, 1, banner_y, 60, 28),
            Some(SidebarHitTarget::Workspace { .. })
        ));
        assert!(divider_y < banner_y && banner_y < remote_card.rect.y);
        assert_eq!(
            compositor.hit_test(&model, 1, remote_card.rect.y, 60, 28),
            Some(SidebarHitTarget::Workspace {
                server_id: remote_id.clone(),
                workspace_id: "remote-api".into(),
            })
        );
        // The agent row + affordances still resolve to their targets at their geometry.
        let new_rect = snapshot.app.sidebar_new_button_rect();
        assert_eq!(
            compositor.hit_test(&model, new_rect.x, new_rect.y, 60, 28),
            Some(SidebarHitTarget::New)
        );
        let menu_rect = snapshot.app.global_launcher_rect();
        assert_eq!(
            compositor.hit_test(
                &model,
                menu_rect.x + menu_rect.width - 1,
                menu_rect.y,
                60,
                28
            ),
            Some(SidebarHitTarget::Menu)
        );
        assert_eq!(
            compositor.hit_test(&model, 27, main_card.rect.y, 60, 28),
            None
        );
    }

    #[test]
    fn hit_test_ignores_disabled_workspace_rows() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-api".into(),
                        label: "api".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        model
            .set_connection_state(
                &remote_id,
                crate::client::supervisor::ConnectionState::Disconnected,
            )
            .unwrap();

        let compositor = ClientCompositor::new(26);

        assert_eq!(compositor.hit_test(&model, 1, 2, 60, 16), None);
    }

    // item 4: a [Main, Secondary] model with workspaces on both sides.
    fn mixed_supervisor_model() -> (ClientSupervisorModel, ServerId) {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model
            .set_summary(
                &ServerId::main(),
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "main-herdr".into(),
                        label: "herdr".into(),
                        branch: None,
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-api".into(),
                        label: "api".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        (model, remote_id)
    }

    #[test]
    fn from_model_aligns_client_workspace_remote_with_workspaces() {
        let (model, _remote_id) = mixed_supervisor_model();
        let compositor = ClientCompositor::new(26);
        let snapshot =
            ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 16, Instant::now());

        // Index-aligned with app.workspaces, and matches each row's is_remote.
        assert_eq!(
            snapshot.app.client_workspace_remote.len(),
            snapshot.app.workspaces.len()
        );
        let rows = model.workspace_rows();
        let expected: Vec<bool> = rows.iter().map(|row| row.is_remote).collect();
        assert_eq!(snapshot.app.client_workspace_remote, expected);
        // [Main, Secondary] => exactly [false, true].
        assert_eq!(snapshot.app.client_workspace_remote, vec![false, true]);
    }

    #[test]
    fn from_model_populates_divider_rows_for_mixed_model() {
        let (model, _remote_id) = mixed_supervisor_model();
        let compositor = ClientCompositor::new(26);
        let snapshot =
            ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 16, Instant::now());
        // A mixed model yields exactly one divider row. item 2 (C3): the single visible
        // Secondary now also emits one host-banner area (from the same compute pass).
        assert_eq!(snapshot.app.view.divider_rows.len(), 1);
        assert_eq!(snapshot.app.view.host_banner_areas.len(), 1);
    }

    #[test]
    fn from_model_populates_host_banner_areas() {
        // item 2 (C3): the host-banner specs + the second slot of the single
        // compute_workspace_list_areas pass populate `app.host_banners` and
        // `app.view.host_banner_areas` (one per visible Secondary), and flip
        // `host_banner_active`. The banner_idx indexes app.host_banners.
        let (model, _remote_id) = mixed_supervisor_model();
        let compositor = ClientCompositor::new(26);
        let snapshot =
            ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 16, Instant::now());

        assert!(snapshot.app.host_banner_active);
        assert_eq!(snapshot.app.host_banners.len(), 1);
        assert_eq!(snapshot.app.host_banners[0].display_name, "x");
        assert_eq!(snapshot.app.view.host_banner_areas.len(), 1);
        let area = snapshot.app.view.host_banner_areas[0];
        assert_eq!(area.banner_idx, 0);
        // The banner area never overlaps a workspace card (render == hit_test).
        assert!(snapshot.app.view.workspace_card_areas.iter().all(|card| {
            !(area.rect.y >= card.rect.y && area.rect.y < card.rect.y + card.rect.height)
        }));
    }

    #[test]
    fn divider_banner_insertion_does_not_shift_active_idx() {
        // item 6 (Area 6) / Area 2 no-shift regression: the optimistic override flips a
        // `focused` bool on a real Workspace row; `from_model` derives `active_idx` from the FLAT
        // `workspace_rows()` stream (which contains NO divider/banner entries — those are
        // layout-only). So even though this mixed model emits a divider AND a host banner,
        // `app.active`/`app.selected` land on the optimistic remote workspace's flat index,
        // unshifted by the non-selectable rows.
        let (mut model, remote_id) = mixed_supervisor_model();

        // Sanity: the model really does emit the non-selectable rows.
        let compositor = ClientCompositor::new(26);
        let pre =
            ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 16, Instant::now());
        assert_eq!(pre.app.view.divider_rows.len(), 1);
        assert_eq!(pre.app.view.host_banner_areas.len(), 1);

        // The remote workspace's index in the flat workspace_rows() stream (no divider/banner).
        let remote_idx = model
            .workspace_rows()
            .iter()
            .position(|row| {
                row.server_id == remote_id && row.workspace_id.as_deref() == Some("remote-api")
            })
            .expect("remote workspace row should be present in the flat stream");

        model.focus_workspace_route(&remote_id, "remote-api");

        let snapshot =
            ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 16, Instant::now());
        // active/selected point at the optimistic remote row's flat index, NOT shifted by the
        // divider/banner rows that sit above it in the rendered list.
        assert_eq!(snapshot.app.active, Some(remote_idx));
        assert_eq!(snapshot.app.selected, remote_idx);
        // The flat workspace_rows() index is unchanged by the divider/banner insertion: the
        // optimistic remote row is at the same index whether or not the layout rows exist.
        assert_eq!(snapshot.app.workspaces.len(), model.workspace_rows().len());
    }

    #[test]
    fn agents_panel_follows_optimistic_group() {
        // item 6 (Area 6): with an optimistic agent focus, the agents panel follows the
        // optimistic server's group — the active workspace is the agent's workspace and that
        // workspace's pane (the agent) is focused, and `agent_groups()` reports the group focused.
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model
            .set_summary(
                &ServerId::main(),
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "main-herdr".into(),
                        label: "herdr".into(),
                        branch: None,
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-api".into(),
                        label: "api".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: vec![AgentSummary {
                        agent_id: "remote-agent".into(),
                        workspace_id: "remote-api".into(),
                        label: "claude".into(),
                        status: "idle".into(),
                        focused: false,
                    }],
                },
            )
            .unwrap();

        model.focus_agent_route(&remote_id, "remote-agent");

        // The optimistic agent's group renders focused (the panel reads agent_groups()).
        let group = model
            .agent_groups()
            .into_iter()
            .find(|group| group.workspace_id == "remote-api")
            .expect("the agent's workspace group should exist");
        assert!(group.focused);
        assert!(group
            .agents
            .iter()
            .any(|agent| agent.agent_id == "remote-agent" && agent.focused));

        let compositor = ClientCompositor::new(26);
        let snapshot =
            ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 16, Instant::now());

        // active/selected point at the agent's workspace row, and that workspace has a focused
        // pane (the agent) — so the composited agents panel renders that group as focused.
        let remote_idx = model
            .workspace_rows()
            .iter()
            .position(|row| {
                row.server_id == remote_id && row.workspace_id.as_deref() == Some("remote-api")
            })
            .expect("remote workspace row should be present");
        assert_eq!(snapshot.app.active, Some(remote_idx));
        assert!(snapshot.app.workspaces[remote_idx]
            .focused_pane_id()
            .is_some());
    }

    #[test]
    fn hit_test_none_over_banner_row() {
        // The host-banner row is not a Workspace/affordance target — hit-test yields no
        // Workspace target over it (render == hit_test; banners are non-selectable).
        let (model, _remote_id) = mixed_supervisor_model();
        let compositor = ClientCompositor::new(26);
        let snapshot =
            ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 16, Instant::now());
        let banner_y = snapshot.app.view.host_banner_areas[0].rect.y;
        let hit = compositor.hit_test(&model, 1, banner_y, 60, 16);
        assert!(
            !matches!(hit, Some(SidebarHitTarget::Workspace { .. })),
            "banner row {banner_y} hit-tested to a workspace: {hit:?}"
        );
        // No card overlaps the banner row, so the real rows still resolve to their cards.
        for card in &snapshot.app.view.workspace_card_areas {
            assert_ne!(card.rect.y, banner_y, "a card overlaps the banner row");
        }
    }

    #[test]
    fn from_model_no_divider_rows_for_all_local_model() {
        let mut model = ClientSupervisorModel::new("local");
        model
            .set_summary(
                &ServerId::main(),
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "main-herdr".into(),
                        label: "herdr".into(),
                        branch: None,
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        let compositor = ClientCompositor::new(26);
        let snapshot =
            ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 16, Instant::now());
        assert!(snapshot.app.view.divider_rows.is_empty());
    }

    #[test]
    fn client_hit_test_returns_no_workspace_for_divider_row() {
        let (model, _remote_id) = mixed_supervisor_model();
        let compositor = ClientCompositor::new(26);
        // Derive the divider y from the same snapshot geometry render uses (render == hit_test).
        let snapshot =
            ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 16, Instant::now());
        let divider_y = snapshot.app.view.divider_rows[0];

        // The divider row resolves to no Workspace target.
        let divider_hit = compositor.hit_test(&model, 1, divider_y, 60, 16);
        assert!(
            !matches!(divider_hit, Some(SidebarHitTarget::Workspace { .. })),
            "divider row {divider_y} hit-tested to a workspace: {divider_hit:?}"
        );
        // The real workspace rows still resolve to their cards (none at the divider y).
        for card in &snapshot.app.view.workspace_card_areas {
            assert_ne!(card.rect.y, divider_y, "a card overlaps the divider row");
            assert!(matches!(
                compositor.hit_test(&model, 1, card.rect.y, 60, 16),
                Some(SidebarHitTarget::Workspace { .. })
            ));
        }
    }

    #[test]
    fn hover_hit_test_skips_divider_row() {
        // Regression lock for the future hover impl (item 7): the click-path geometry used by
        // hover (workspace_card_areas) yields no workspace for the divider row.
        let (model, _remote_id) = mixed_supervisor_model();
        let compositor = ClientCompositor::new(26);
        let snapshot =
            ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 16, Instant::now());
        let divider_y = snapshot.app.view.divider_rows[0];
        assert!(snapshot
            .app
            .view
            .workspace_card_areas
            .iter()
            .all(|card| !(divider_y >= card.rect.y && divider_y < card.rect.y + card.rect.height)));
    }

    /// Build a mixed two-destination model (main `local` + remote `x`) and open the picker.
    fn two_destination_picker_model() -> (ClientSupervisorModel, ServerId) {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model
            .set_summary(
                &ServerId::main(),
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "main-herdr".into(),
                        label: "herdr".into(),
                        branch: None,
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-api".into(),
                        label: "api".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: vec![AgentSummary {
                        agent_id: "remote-agent".into(),
                        workspace_id: "remote-api".into(),
                        label: "claude".into(),
                        status: "idle".into(),
                        focused: false,
                    }],
                },
            )
            .unwrap();
        model.open_new_workspace_picker();
        (model, remote_id)
    }

    #[test]
    fn new_workspace_picker_renders_footer_anchored_selectable_list() {
        let (model, _) = two_destination_picker_model();

        let compositor = ClientCompositor::new(26);
        let content = frame(8, 3, &["content", "frame"]);
        let composed =
            compositor.compose_frame(&model, &content, 60, 20, std::time::Instant::now());
        let rows: Vec<_> = (0..composed.height)
            .map(|row| row_text(&composed, row))
            .collect();

        // footer-anchored ratatui popup: square corner, header, sub-label, both destinations,
        // buttons.
        assert!(rows.iter().any(|row| row.contains("┌")));
        assert!(rows.iter().any(|row| row.contains("new workspace")));
        assert!(rows.iter().any(|row| row.contains("create on")));
        assert!(rows.iter().any(|row| row.contains("local")));
        assert!(rows.iter().any(|row| row.contains("x")));
        // default selection (index 0) carries the `›` marker.
        assert!(rows.iter().any(|row| row.contains("›")));
        assert!(rows.iter().any(|row| row.contains("create")));
        assert!(rows.iter().any(|row| row.contains("cancel")));

        // the popup is footer-anchored (opens upward from the sidebar footer), NOT centered: its
        // top border sits below the host top, and it never reaches the bottom rows.
        let popup =
            crate::ui::new_workspace_picker_popup_rect(anchor_area(&model, &compositor, 60, 20), 2)
                .expect("popup fits");
        assert!(popup.y > 0, "popup is not flush to host top");
        assert!(rows[popup.y as usize].contains("┌"), "top border row");
        // rows below the popup show server content / blanks, not the picker.
        for row in &rows[(popup.y + popup.height) as usize..] {
            assert!(!row.contains("new workspace"));
        }
    }

    #[test]
    fn new_workspace_picker_mouse_click_hit_tests_footer_anchored_rows() {
        let (model, remote_id) = two_destination_picker_model();
        let compositor = ClientCompositor::new(26);

        // derive the FOOTER-ANCHORED row coordinates from the SAME shared geometry + anchor_area
        // the renderer/hit-test use.
        let anchor = anchor_area(&model, &compositor, 60, 20);
        let inner = crate::ui::new_workspace_picker_inner_rect(anchor, 2).expect("modal fits");
        let row0 = crate::ui::new_workspace_picker_row_rect(inner, 0);
        let row1 = crate::ui::new_workspace_picker_row_rect(inner, 1);

        // the popup is footer-anchored, so its rows sit below the host top (not centered, not the
        // old bottom-anchored geometry).
        assert!(row0.y > 0);

        assert_eq!(
            compositor.hit_test(&model, row0.x, row0.y, 60, 20),
            Some(SidebarHitTarget::NewWorkspaceDestination {
                server_id: ServerId::main(),
            })
        );
        assert_eq!(
            compositor.hit_test(&model, row1.x, row1.y, 60, 20),
            Some(SidebarHitTarget::NewWorkspaceDestination {
                server_id: remote_id,
            })
        );
    }

    #[test]
    fn new_workspace_picker_keyboard_navigates_and_confirms() {
        let (mut model, remote_id) = two_destination_picker_model();
        assert_eq!(model.new_workspace_picker().map(|p| p.selected), Some(0));

        model.move_new_workspace_picker_next();
        assert_eq!(model.new_workspace_picker().map(|p| p.selected), Some(1));

        let route = model.accept_new_workspace_picker();
        assert_eq!(route, NewWorkspaceRoute::CreateOn(remote_id));
    }

    #[test]
    fn picker_confirm_and_cancel_buttons_hit_test() {
        let (model, _) = two_destination_picker_model();
        let compositor = ClientCompositor::new(26);

        let anchor = anchor_area(&model, &compositor, 60, 20);
        let inner = crate::ui::new_workspace_picker_inner_rect(anchor, 2).expect("modal fits");
        let (confirm, cancel) = crate::ui::new_workspace_picker_button_rects(inner);

        assert_eq!(
            compositor.hit_test(&model, confirm.x, confirm.y, 60, 20),
            Some(SidebarHitTarget::NewWorkspacePickerConfirm)
        );
        assert_eq!(
            compositor.hit_test(&model, cancel.x, cancel.y, 60, 20),
            Some(SidebarHitTarget::NewWorkspacePickerCancel)
        );
    }

    #[test]
    fn client_global_menu_uses_server_launcher_menu_surface() {
        let mut model = ClientSupervisorModel::new("local");
        model.open_client_global_menu();

        let compositor = ClientCompositor::new(26);
        let content = frame(8, 3, &["content", "frame"]);
        let composed =
            compositor.compose_frame(&model, &content, 60, 16, std::time::Instant::now());

        let rows: Vec<_> = (0..composed.height)
            .map(|row| row_text(&composed, row))
            .collect();
        assert!(rows.iter().any(|row| row.contains("┌")));
        assert!(rows.iter().any(|row| row.contains("settings")));
        assert!(rows.iter().any(|row| row.contains("keybinds")));
        assert!(rows.iter().any(|row| row.contains("reload config")));
        assert!(rows.iter().any(|row| row.contains("detach")));
        assert!(rows.iter().any(|row| row.contains("add remote")));
        assert_eq!(
            compositor.hit_test(&model, 21, 1, 60, 16),
            Some(SidebarHitTarget::ClientGlobalMenuItem { index: 0 })
        );
        assert_eq!(
            compositor.hit_test(&model, 21, 5, 60, 16),
            Some(SidebarHitTarget::ClientGlobalMenuItem { index: 4 })
        );
    }

    #[test]
    fn client_global_menu_hover_moves_highlight_render() {
        // item 7: moving the highlight (as a hover `Moved` does via `hover_client_global_menu_item`)
        // repaints the accent bg onto the newly highlighted row and clears it from the old one — the
        // shared launcher-menu surface renders `highlighted` identically to the monolithic host.
        let compositor = ClientCompositor::new(26);
        let content = frame(8, 3, &["content", "frame"]);

        let mut model = ClientSupervisorModel::new("local");
        model.open_client_global_menu(); // highlighted defaults to index 0.
        let before = compositor.compose_frame(&model, &content, 60, 16, std::time::Instant::now());

        assert!(model.hover_client_global_menu_item(Some(2)));
        let after = compositor.compose_frame(&model, &content, 60, 16, std::time::Instant::now());

        let snapshot = ClientSidebarSnapshot::from_model(
            &model,
            &compositor,
            26,
            60,
            16,
            std::time::Instant::now(),
        );
        let rect = snapshot.app.global_menu_rect();
        let row0 = rect.y + 1; // item index 0 ("settings").
        let row1 = rect.y + 2; // item index 1 ("keybinds"): never highlighted in this test.
        let row2 = rect.y + 3; // item index 2 ("reload config").

        let bgs = |frame: &FrameData, row: u16| -> Vec<u32> {
            (0..frame.width)
                .map(|x| cell_at(frame, x, row).bg)
                .collect()
        };
        // before: index 0 is highlighted, so its bg differs from the unhighlighted neighbour row 1
        // (same-width label, so an unhighlighted row 0 would match row 1 exactly).
        assert_ne!(
            bgs(&before, row0),
            bgs(&before, row1),
            "row 0 starts highlighted"
        );
        // after moving the highlight to index 2: row 0 reverts to an unhighlighted row (matches the
        // unhighlighted row 1), and row 2 now carries a highlight bg row 1 lacks.
        assert_eq!(
            bgs(&after, row0),
            bgs(&after, row1),
            "row 0 reverts to unhighlighted"
        );
        assert_ne!(
            bgs(&after, row2),
            bgs(&after, row1),
            "row 2 becomes highlighted"
        );
    }

    /// Read the cell at (x, y) of a composited frame.
    fn cell_at(frame: &FrameData, x: u16, y: u16) -> &CellData {
        &frame.cells[(y as usize) * (frame.width as usize) + (x as usize)]
    }

    /// Encode an RGB color the same way `FrameData::from_ratatui_buffer_with_hyperlinks` does, so
    /// the modal tests can match against palette colors.
    fn encode_rgb(r: u8, g: u8, b: u8) -> u32 {
        0x02_00_00_00 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
    }

    /// True iff any cell on `row` carries `bg` (e.g. the focused field's `surface0` fill).
    fn row_has_bg(frame: &FrameData, row: u16, bg: u32) -> bool {
        (0..frame.width).any(|x| cell_at(frame, x, row).bg == bg)
    }

    #[test]
    fn add_remote_modal_renders_accent_border_and_action_buttons() {
        let mut model = ClientSupervisorModel::new("local");
        model.open_add_remote_form();

        let compositor = ClientCompositor::new(26);
        let content = frame(20, 8, &["content", "frame"]);
        let composed =
            compositor.compose_frame(&model, &content, 80, 24, std::time::Instant::now());
        let rows: Vec<_> = (0..composed.height)
            .map(|row| row_text(&composed, row))
            .collect();

        // border::PLAIN square corner (NOT the legacy rounded `╭`).
        assert!(rows.iter().any(|row| row.contains("┌")));
        assert!(!rows.iter().any(|row| row.contains("╭")));
        assert!(rows.iter().any(|row| row.contains("add remote")));
        assert!(rows.iter().any(|row| row.contains("target")));
        assert!(rows.iter().any(|row| row.contains("name")));
        // action buttons.
        assert!(rows.iter().any(|row| row.contains("add")));
        assert!(rows.iter().any(|row| row.contains("cancel")));
        // legacy ASCII art / raw markers / the old footer literal are ABSENT.
        assert!(!rows.iter().any(|row| row.contains("+---")));
        assert!(!rows.iter().any(|row| row.contains("enter add   esc close")));
    }

    #[test]
    fn modal_survives_full_screen_content_overwrite() {
        // Regression: a composited overlay must stay visible even when the server content frame
        // fills the ENTIRE content area (a real pane full of text). The content copy protects
        // EXACTLY the open popup's rect, so the overlay survives AND the rest of the content stays
        // visible around it (the popup is footer-anchored, NOT a full-screen-dimmed centered modal).
        let mut model = ClientSupervisorModel::new("local");
        model.open_add_remote_form();
        // content frame that fills the whole content area with a sentinel (like a busy pane).
        let filled = "#".repeat(54);
        let rows: Vec<&str> = vec![filled.as_str(); 24];
        let content = frame(54, 24, &rows);

        let compositor = ClientCompositor::new(26);
        let composed =
            compositor.compose_frame(&model, &content, 80, 24, std::time::Instant::now());
        let texts: Vec<String> = (0..composed.height)
            .map(|r| row_text(&composed, r))
            .collect();

        // the overlay (header, a field label, the cancel button) must survive — these sit in the
        // content columns and would be overwritten by the '#' fill without the popup-rect exclusion.
        assert!(
            texts.iter().any(|t| t.contains("add remote")),
            "overlay header overwritten by content; rows={texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("target")),
            "overlay field overwritten by content"
        );
        assert!(
            texts.iter().any(|t| t.contains("cancel")),
            "overlay action button overwritten by content"
        );

        // ...AND the content sentinel must STILL be visible somewhere in the content area OUTSIDE
        // the popup rect — proving the fix protects only the popup, not the whole screen (the old
        // bug blanked everything).
        let popup =
            crate::ui::add_remote_popup_rect(compositor.overlay_anchor_area(&model, 80, 24))
                .expect("popup fits");
        let sidebar_width = 26u16;
        let sentinel_outside_popup = (0..composed.height).any(|row| {
            (sidebar_width..composed.width).any(|col| {
                let inside_popup = col >= popup.x
                    && col < popup.x + popup.width
                    && row >= popup.y
                    && row < popup.y + popup.height;
                !inside_popup && cell_at(&composed, col, row).symbol == "#"
            })
        });
        assert!(
            sentinel_outside_popup,
            "content '#' fully blanked; only the popup rect should be protected; rows={texts:?}"
        );
    }

    #[test]
    fn add_remote_modal_marks_focused_field() {
        let mut model = ClientSupervisorModel::new("local");
        model.open_add_remote_form(); // focus defaults to Target.

        let compositor = ClientCompositor::new(26);
        let content = frame(20, 8, &["content", "frame"]);
        let composed =
            compositor.compose_frame(&model, &content, 80, 24, std::time::Instant::now());

        // inner rect for the footer-anchored overlay: rows[1] (target) and rows[2] (name).
        let inner = crate::ui::add_remote_inner_rect(anchor_area(&model, &compositor, 80, 24))
            .expect("modal fits");
        let target_row = inner.y.saturating_add(1);
        let name_row = inner.y.saturating_add(2);

        // catppuccin surface0 = Rgb(49, 50, 68); the focused target field carries that fill.
        let surface0 = encode_rgb(49, 50, 68);
        assert!(row_has_bg(&composed, target_row, surface0));
        // the target label cells now carry non-zero fg (regression vs. the old colorless draw).
        assert!((0..composed.width).any(|x| cell_at(&composed, x, target_row).fg != 0));
        // the unfocused name row does NOT carry the focused `surface0` bg (it has panel_bg).
        assert!(!row_has_bg(&composed, name_row, surface0));
    }

    #[test]
    fn add_remote_modal_shows_inline_error() {
        let mut model = ClientSupervisorModel::new("local");
        model.open_add_remote_form();
        // Enter with an empty target produces the `target required` inline error.
        model.handle_add_remote_key(crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ));

        let compositor = ClientCompositor::new(26);
        let content = frame(20, 8, &["content", "frame"]);
        let composed =
            compositor.compose_frame(&model, &content, 80, 24, std::time::Instant::now());
        let rows: Vec<_> = (0..composed.height)
            .map(|row| row_text(&composed, row))
            .collect();
        assert!(rows.iter().any(|row| row.contains("target required")));

        // an async-style error string renders too.
        model.set_add_remote_error("adding remote...");
        let composed =
            compositor.compose_frame(&model, &content, 80, 24, std::time::Instant::now());
        let rows: Vec<_> = (0..composed.height)
            .map(|row| row_text(&composed, row))
            .collect();
        assert!(rows.iter().any(|row| row.contains("adding remote...")));
    }

    #[test]
    fn add_remote_modal_keeps_cursor_hidden() {
        let mut model = ClientSupervisorModel::new("local");
        model.open_add_remote_form();

        let compositor = ClientCompositor::new(26);
        let content = frame(20, 8, &["content", "frame"]);
        let composed =
            compositor.compose_frame(&model, &content, 80, 24, std::time::Instant::now());

        assert!(composed.cursor.is_none());
    }

    #[test]
    fn add_remote_button_click_submits_and_cancel_closes() {
        let mut model = ClientSupervisorModel::new("local");
        model.open_add_remote_form();
        let compositor = ClientCompositor::new(26);

        let inner = crate::ui::add_remote_inner_rect(anchor_area(&model, &compositor, 80, 24))
            .expect("modal fits");
        let (submit, cancel) = crate::ui::add_remote_button_rects(inner);

        assert_eq!(
            compositor.hit_test(&model, submit.x, submit.y, 80, 24),
            Some(SidebarHitTarget::AddRemoteSubmit)
        );
        assert_eq!(
            compositor.hit_test(&model, cancel.x, cancel.y, 80, 24),
            Some(SidebarHitTarget::AddRemoteCancel)
        );
    }

    // --- item 5: client agent animation --------------------------------------------------

    /// Build a mixed model with one main workspace and one remote workspace whose single agent
    /// has `agent_status` (e.g. "working" / "idle"). Both servers connect by default.
    fn model_with_agent_status(agent_status: &str) -> (ClientSupervisorModel, ServerId) {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model
            .set_summary(
                &ServerId::main(),
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "main-herdr".into(),
                        label: "herdr".into(),
                        branch: Some("master".into()),
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-api".into(),
                        label: "api".into(),
                        branch: Some("feature/api".into()),
                        focused: false,
                    }],
                    agents: vec![AgentSummary {
                        agent_id: "remote-agent".into(),
                        workspace_id: "remote-api".into(),
                        label: "claude".into(),
                        status: agent_status.into(),
                        focused: false,
                    }],
                },
            )
            .unwrap();
        (model, remote_id)
    }

    #[test]
    fn animation_tick_feeds_spinner_tick() {
        let (model, _) = model_with_agent_status("working");
        let mut compositor = ClientCompositor::new(26);
        compositor.advance_animation_tick(8);

        let snapshot = ClientSidebarSnapshot::from_model(
            &model,
            &compositor,
            26,
            60,
            16,
            std::time::Instant::now(),
        );

        assert_eq!(snapshot.app.spinner_tick, 8);
    }

    #[test]
    fn spinner_cell_differs_between_tick_0_and_8() {
        let (model, _) = model_with_agent_status("working");
        let content = frame(8, 3, &["content", "frame"]);

        let at_zero = ClientCompositor::new(26);
        let frame_zero = at_zero.compose_frame(&model, &content, 60, 16, std::time::Instant::now());

        let mut at_eight = ClientCompositor::new(26);
        at_eight.advance_animation_tick(8);
        let frame_eight =
            at_eight.compose_frame(&model, &content, 60, 16, std::time::Instant::now());

        let symbols_zero: Vec<_> = frame_zero.cells.iter().map(|c| c.symbol.clone()).collect();
        let symbols_eight: Vec<_> = frame_eight.cells.iter().map(|c| c.symbol.clone()).collect();
        assert_ne!(
            symbols_zero, symbols_eight,
            "spinner_frame should advance the agent-status cell between tick 0 and tick 8"
        );
        // The spinner glyph at tick 0 is SPINNERS[0] = ⠋, at tick 8 it is SPINNERS[1] = ⠙.
        assert!(symbols_zero.iter().any(|s| s == "⠋"));
        assert!(symbols_eight.iter().any(|s| s == "⠙"));
    }

    #[test]
    fn advance_animation_tick_wraps_and_steps() {
        let mut compositor = ClientCompositor::new(26);
        assert_eq!(compositor.animation_tick(), 0);
        compositor.advance_animation_tick(8);
        assert_eq!(compositor.animation_tick(), 8);
        compositor.advance_animation_tick(8);
        assert_eq!(compositor.animation_tick(), 16);

        // Wrap cleanly at the u32 boundary with no discontinuity in the visible step `tick/8`.
        let mut wrapping = ClientCompositor::new(26);
        wrapping.advance_animation_tick(u32::MAX - 3);
        let before = wrapping.animation_tick();
        wrapping.advance_animation_tick(8);
        let after = wrapping.animation_tick();
        assert_eq!(after, before.wrapping_add(8));
        assert!(after < before, "tick should wrap past the u32 boundary");
    }

    #[test]
    fn sidebar_wants_animation_true_with_working_agent() {
        let (model, _) = model_with_agent_status("working");
        assert!(sidebar_wants_animation(&model));
    }

    /// Force the host-banner animation off so a test can isolate the agent-driven animation
    /// gate (item 2 (C3): a visible Secondary now animates its banner by default).
    fn with_static_host_banner(model: &mut ClientSupervisorModel) {
        let mut ui_settings = model.ui_settings().clone();
        ui_settings.sidebar_host.animation = crate::config::HostBannerAnimation::Static;
        model.set_ui_settings(ui_settings);
    }

    #[test]
    fn sidebar_wants_animation_false_when_all_idle() {
        // With the banner animation forced Static, only the agent gate remains — idle agents
        // never request animation.
        for status in ["idle", "done", "blocked", "unknown"] {
            let (mut model, _) = model_with_agent_status(status);
            with_static_host_banner(&mut model);
            assert!(
                !sidebar_wants_animation(&model),
                "status {status:?} should not request animation"
            );
        }
    }

    #[test]
    fn sidebar_wants_animation_true_with_banner() {
        // item 2 (C3): the banner hook is now the real gate. With no working agent the gate is
        // driven solely by `host_banner_animation_active` — a visible Secondary with the default
        // Animated setting makes the gate true (proving the banner hook is the single
        // banner-active input the gate reads).
        let (model, _) = model_with_agent_status("idle");
        assert!(model.host_banner_animation_active());
        assert!(sidebar_wants_animation(&model));
        assert_eq!(
            sidebar_wants_animation(&model),
            model.host_banner_animation_active(),
            "with no working agent the gate equals the banner hook"
        );
    }

    #[test]
    fn working_agent_seeds_working_since_from_map() {
        let (model, remote_id) = model_with_agent_status("working");
        let t0 = std::time::Instant::now();
        let mut compositor = ClientCompositor::new(26);
        compositor.seed_working_since((remote_id.clone(), "remote-agent".to_string()), t0);

        let snapshot = ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 16, t0);

        // Exactly one terminal (the working remote agent) was built.
        let terminal = snapshot
            .app
            .terminals
            .values()
            .find(|t| t.working_duration_at(t0).is_some())
            .expect("working agent terminal should expose a live duration");

        let at_2s = terminal
            .working_duration_at(t0 + Duration::from_secs(2))
            .expect("working duration should be live");
        let at_3s = terminal
            .working_duration_at(t0 + Duration::from_secs(3))
            .expect("working duration should be live");
        assert!(at_2s.is_live);
        assert_eq!(at_2s.elapsed.as_secs(), 2);
        assert_eq!(at_3s.elapsed.as_secs(), 3);
    }

    #[test]
    fn disabled_remote_agent_rows_do_not_gate_animation() {
        // A disabled remote's placeholder rows are not `working`, so they never request
        // animation on themselves (parity with the render==hit_test disabled-row rejection).
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model
            .set_connection_state(
                &remote_id,
                crate::client::supervisor::ConnectionState::Disconnected,
            )
            .unwrap();
        // Force the banner animation Static to isolate the agent gate: a disconnected remote's
        // placeholder rows are not `working`, so they never request animation on themselves.
        with_static_host_banner(&mut model);
        assert!(!sidebar_wants_animation(&model));
    }

    // ----- item 3 (Area 5): remote-management overlay render == hit_test --------------------

    fn manage_overlay_model() -> ClientSupervisorModel {
        let mut model = ClientSupervisorModel::new("local");
        model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "r1".into(),
            name: "alpha".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Ssh {
                target: "alpha".into(),
                args: Vec::new(),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "r2".into(),
            name: "beta".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Ssh {
                target: "beta".into(),
                args: Vec::new(),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model.open_remote_manage_overlay();
        model
    }

    #[test]
    fn remote_manage_render_equals_hit_test_geometry() {
        let model = manage_overlay_model();
        let compositor = ClientCompositor::new(26);
        let anchor = anchor_area(&model, &compositor, 80, 24);

        // the rect the renderer draws row N into is the rect hit_test checks.
        let inner = crate::ui::remote_manage_inner_rect(anchor, 2).expect("modal fits");
        let row0 = crate::ui::remote_manage_row_rect(inner, 0);
        let row1 = crate::ui::remote_manage_row_rect(inner, 1);
        assert_eq!(
            compositor.hit_test(&model, row0.x, row0.y, 80, 24),
            Some(SidebarHitTarget::RemoteManageRow { index: 0 })
        );
        assert_eq!(
            compositor.hit_test(&model, row1.x, row1.y, 80, 24),
            Some(SidebarHitTarget::RemoteManageRow { index: 1 })
        );
    }

    #[test]
    fn hit_test_returns_manage_targets() {
        let model = manage_overlay_model();
        let compositor = ClientCompositor::new(26);
        let anchor = anchor_area(&model, &compositor, 80, 24);
        let inner = crate::ui::remote_manage_inner_rect(anchor, 2).expect("modal fits");

        // row click selects.
        let row0 = crate::ui::remote_manage_row_rect(inner, 0);
        assert_eq!(
            compositor.hit_test(&model, row0.x, row0.y, 80, 24),
            Some(SidebarHitTarget::RemoteManageRow { index: 0 })
        );
        // footer `add` affordance.
        let footer_y = inner.y + inner.height.saturating_sub(1);
        assert_eq!(
            compositor.hit_test(&model, inner.x, footer_y, 80, 24),
            Some(SidebarHitTarget::RemoteManageAdd)
        );
        // click well outside the modal → None.
        assert_eq!(compositor.hit_test(&model, 0, 0, 80, 24), None);
    }

    #[test]
    fn manage_overlay_render_is_pure() {
        let model = manage_overlay_model();
        let compositor = ClientCompositor::new(26);
        let content = frame(8, 3, &["content", "frame"]);

        let a = compositor.compose_frame(&model, &content, 80, 24, std::time::Instant::now());
        let b = compositor.compose_frame(&model, &content, 80, 24, std::time::Instant::now());
        assert_eq!(a.cells, b.cells, "list render must be deterministic");

        // delete-confirm sub-state is also pure. `compose_frame` takes `&model` (shared ref), so
        // non-mutation is structural; determinism across two renders confirms purity.
        let mut model = manage_overlay_model();
        model.begin_remote_manage_delete();
        let c = compositor.compose_frame(&model, &content, 80, 24, std::time::Instant::now());
        let d = compositor.compose_frame(&model, &content, 80, 24, std::time::Instant::now());
        assert_eq!(c.cells, d.cells, "confirm render must be deterministic");
        assert!(model
            .remote_manage_overlay()
            .unwrap()
            .confirm_delete
            .is_some());
    }

    #[test]
    fn confirm_delete_renders_red_panel() {
        let mut model = manage_overlay_model();
        model.begin_remote_manage_delete();
        let compositor = ClientCompositor::new(26);
        let content = frame(8, 3, &["content", "frame"]);
        let composed =
            compositor.compose_frame(&model, &content, 80, 24, std::time::Instant::now());
        let rows: Vec<_> = (0..composed.height)
            .map(|row| row_text(&composed, row))
            .collect();
        assert!(rows.iter().any(|row| row.contains("delete remote?")));
        assert!(rows.iter().any(|row| row.contains("delete")));
        assert!(rows.iter().any(|row| row.contains("cancel")));

        // while confirm is active the list rows are NOT hit-testable; only the popup buttons are.
        let anchor = anchor_area(&model, &compositor, 80, 24);
        let inner = crate::ui::remote_manage_inner_rect(anchor, 2).expect("modal fits");
        let row0 = crate::ui::remote_manage_row_rect(inner, 0);
        assert!(!matches!(
            compositor.hit_test(&model, row0.x, row0.y, 80, 24),
            Some(SidebarHitTarget::RemoteManageRow { .. })
        ));
        let popup = crate::ui::remote_manage_confirm_popup_rect(anchor).expect("popup fits");
        let pinner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (delete_rect, cancel_rect) = crate::ui::remote_manage_confirm_button_rects(pinner);
        assert_eq!(
            compositor.hit_test(&model, delete_rect.x, delete_rect.y, 80, 24),
            Some(SidebarHitTarget::RemoteManageConfirmDelete)
        );
        assert_eq!(
            compositor.hit_test(&model, cancel_rect.x, cancel_rect.y, 80, 24),
            Some(SidebarHitTarget::RemoteManageCancelDelete)
        );
    }

    #[test]
    fn manage_overlay_hit_test_skips_non_modal() {
        // A model with a focused main workspace AND the overlay open: a click on the (sidebar)
        // workspace row resolves to a manage target or None, NEVER a Workspace hit — the overlay
        // intercepts the whole host rect first.
        let mut model = manage_overlay_model();
        model
            .set_summary(
                &ServerId::main(),
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "main-ws".into(),
                        label: "herdr".into(),
                        branch: None,
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        let compositor = ClientCompositor::new(26);
        // sweep a column of the sidebar; none may resolve to a Workspace.
        for y in 0..24u16 {
            let hit = compositor.hit_test(&model, 1, y, 80, 24);
            assert!(
                !matches!(hit, Some(SidebarHitTarget::Workspace { .. })),
                "overlay must intercept sidebar workspace hits, got {hit:?} at y={y}"
            );
        }
    }

    // ---- item 7 (Area 4): hover_test / set_hover / from_model mirror ----

    use crate::app::state::SidebarHoverTarget;

    #[test]
    fn set_hover_reports_change() {
        let mut compositor = ClientCompositor::new(26);
        assert!(compositor.set_hover(Some(SidebarHoverTarget::Workspace { ws_idx: 0 })));
        assert!(!compositor.set_hover(Some(SidebarHoverTarget::Workspace { ws_idx: 0 })));
        assert!(compositor.set_hover(Some(SidebarHoverTarget::Workspace { ws_idx: 1 })));
        assert!(compositor.set_hover(None));
        assert!(!compositor.set_hover(None));
    }

    #[test]
    fn from_model_mirrors_compositor_hover() {
        // A hover set on the compositor truth appears in the render snapshot (Copy; pure read).
        let (model, _remote_id) = mixed_supervisor_model();
        let mut compositor = ClientCompositor::new(26);
        compositor.set_hover(Some(SidebarHoverTarget::Workspace { ws_idx: 1 }));
        let snapshot =
            ClientSidebarSnapshot::from_model(&model, &compositor, 26, 60, 16, Instant::now());
        assert_eq!(
            snapshot.app.sidebar_hover,
            Some(SidebarHoverTarget::Workspace { ws_idx: 1 })
        );
    }

    #[test]
    fn hover_test_resolves_workspace_row() {
        // render == hit_test geometry: drive the row index off `hit_test` (like the click test),
        // then assert `hover_test` over the same (x,y) resolves the matching Workspace ws_idx.
        let (model, remote_id) = mixed_supervisor_model();
        let compositor = ClientCompositor::new(26);
        let host = (60u16, 28u16);
        let snapshot = ClientSidebarSnapshot::from_model(
            &model,
            &compositor,
            26,
            host.0,
            host.1,
            Instant::now(),
        );
        let remote_card = snapshot
            .app
            .view
            .workspace_card_areas
            .iter()
            .find(|c| c.ws_idx == 1)
            .expect("remote card");
        // sanity: the click path resolves the remote workspace at this row.
        assert_eq!(
            compositor.hit_test(&model, 1, remote_card.rect.y, host.0, host.1),
            Some(SidebarHitTarget::Workspace {
                server_id: remote_id.clone(),
                workspace_id: "remote-api".into(),
            })
        );
        assert_eq!(
            compositor.hover_test(&model, 1, remote_card.rect.y, host.0, host.1),
            Some(SidebarHoverTarget::Workspace { ws_idx: 1 })
        );
    }

    #[test]
    fn hover_test_skips_non_selectable_rows() {
        // the ` spaces`/` agents` header rows, the `─` separator, the right-edge resize column,
        // and the item-4 divider row never resolve to a Workspace/Agent hover target.
        let (model, _remote_id) = mixed_supervisor_model();
        let compositor = ClientCompositor::new(26);
        let host = (60u16, 28u16);
        let snapshot = ClientSidebarSnapshot::from_model(
            &model,
            &compositor,
            26,
            host.0,
            host.1,
            Instant::now(),
        );

        // ` spaces` header is the sidebar's first row.
        let header_y = snapshot.app.view.sidebar_rect.y;
        let hover = compositor.hover_test(&model, 1, header_y, host.0, host.1);
        assert!(!matches!(
            hover,
            Some(SidebarHoverTarget::Workspace { .. })
                | Some(SidebarHoverTarget::AgentRoute { .. })
        ));

        // the right-edge resize column (x == sidebar_width - 1 .. is the divider `│`): a position
        // at x >= effective_sidebar_width resolves None.
        assert_eq!(
            compositor.hover_test(&model, 27, header_y, host.0, host.1),
            None
        );

        // the item-4 divider row resolves to the defensive Divider (NEVER a Workspace/Agent) —
        // render treats Divider as no-highlight.
        let divider_y = snapshot.app.view.divider_rows[0];
        assert_eq!(
            compositor.hover_test(&model, 1, divider_y, host.0, host.1),
            Some(SidebarHoverTarget::Divider)
        );
    }

    #[test]
    fn hover_test_ignores_disabled_workspace_rows() {
        // mirror hit_test_ignores_disabled_workspace_rows: a disabled remote route hovers None.
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-api".into(),
                        label: "api".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        model
            .set_connection_state(
                &remote_id,
                crate::client::supervisor::ConnectionState::Disconnected,
            )
            .unwrap();
        let compositor = ClientCompositor::new(26);
        // sweep the sidebar column: no row resolves to a Workspace hover (disabled rows rejected).
        for y in 0..16u16 {
            assert!(!matches!(
                compositor.hover_test(&model, 1, y, 60, 16),
                Some(SidebarHoverTarget::Workspace { .. })
            ));
        }
    }

    #[test]
    fn hover_test_agent_resolves_route_index_and_survives_recompose() {
        // contradiction-11 regression: hover over an agent entry returns AgentRoute { route_idx };
        // rebuilding the snapshot (recompose) keeps the SAME route_idx mapping to the same agent
        // even though the placeholder pane_id is freshly alloc'd each recompose.
        let (model, _remote_id) = mixed_supervisor_model_with_agent();
        let compositor = ClientCompositor::new(26);
        let host = (60u16, 20u16);

        // find the agent row by sweeping (the click path resolves the agent there).
        let agent_row = (0..host.1)
            .find(|y| {
                matches!(
                    compositor.hit_test(&model, 1, *y, host.0, host.1),
                    Some(SidebarHitTarget::Agent { agent_id, .. }) if agent_id == "remote-agent"
                )
            })
            .expect("agent row should be hit-testable");

        let hover = compositor.hover_test(&model, 1, agent_row, host.0, host.1);
        let route_idx = match hover {
            Some(SidebarHoverTarget::AgentRoute { route_idx }) => route_idx,
            other => panic!("expected AgentRoute, got {other:?}"),
        };

        // recompose: the snapshot's agent_routes are rebuilt; the same route_idx still points at
        // the same agent (positional index, not the dead pane_id).
        let snap_a = ClientSidebarSnapshot::from_model(
            &model,
            &compositor,
            26,
            host.0,
            host.1,
            Instant::now(),
        );
        let snap_b = ClientSidebarSnapshot::from_model(
            &model,
            &compositor,
            26,
            host.0,
            host.1,
            Instant::now(),
        );
        assert_eq!(
            snap_a.agent_routes[route_idx].agent_id,
            snap_b.agent_routes[route_idx].agent_id
        );
        assert_eq!(snap_a.agent_routes[route_idx].agent_id, "remote-agent");
        // hover_test resolves the SAME route_idx after recompose.
        assert_eq!(
            compositor.hover_test(&model, 1, agent_row, host.0, host.1),
            Some(SidebarHoverTarget::AgentRoute { route_idx })
        );
    }

    #[test]
    fn hover_test_affordances_respect_draw_gate() {
        // over `new`/`menu`/`filter` the hover resolves to the matching affordance. The client
        // snapshot is always `mouse_capture == true` (empty_for_client_rendering), so the
        // affordances are drawn and hoverable (the gate-off branch is exercised monolithically,
        // where mouse_capture can be false — see the input/mouse tests).
        let (model, _remote_id) = mixed_supervisor_model();
        let compositor = ClientCompositor::new(26);
        let host = (60u16, 16u16);
        let snapshot = ClientSidebarSnapshot::from_model(
            &model,
            &compositor,
            26,
            host.0,
            host.1,
            Instant::now(),
        );

        let new_rect = snapshot.app.sidebar_new_button_rect();
        let menu_rect = snapshot.app.global_launcher_rect();
        // empty_for_client_rendering defaults mouse_capture = true, so affordances are drawn.
        assert!(snapshot.app.mouse_capture);
        assert_eq!(
            compositor.hover_test(&model, new_rect.x, new_rect.y, host.0, host.1),
            Some(SidebarHoverTarget::New)
        );
        assert_eq!(
            compositor.hover_test(
                &model,
                menu_rect.x + menu_rect.width - 1,
                menu_rect.y,
                host.0,
                host.1
            ),
            Some(SidebarHoverTarget::Menu)
        );
        // the filter label (top-right of the sidebar).
        assert_eq!(
            compositor.hover_test(&model, 23, snapshot.app.view.sidebar_rect.y, host.0, host.1),
            Some(SidebarHoverTarget::Filter)
        );
    }

    #[test]
    fn hover_test_suppressed_when_overlay_open() {
        // with the add-remote form open OR the client global menu highlighted, sidebar hover_test
        // returns None (the overlay owns its own hover; the global menu moves its highlight via the
        // separate `client_global_menu_item_at` path in the client `Moved` arm).
        let (mut model, _remote_id) = mixed_supervisor_model();
        model.open_add_remote_form();
        for y in 0..16u16 {
            assert_eq!(model_hover_anywhere(&model, y), None);
        }
        model.close_client_overlay();

        let (mut model, _remote_id) = mixed_supervisor_model();
        model.open_client_global_menu();
        for y in 0..16u16 {
            assert_eq!(model_hover_anywhere(&model, y), None);
        }
    }

    #[test]
    fn client_global_menu_item_at_resolves_hovered_row() {
        // item 7: motion over the open menu resolves to the row index under the cursor (same
        // geometry `hit_test` uses); a far-left column off the right-anchored menu resolves to None;
        // a closed menu resolves to None.
        let (mut model, _remote_id) = mixed_supervisor_model();
        let compositor = ClientCompositor::new(26);
        let host = (60u16, 16u16);
        // closed menu → None.
        assert_eq!(
            compositor.client_global_menu_item_at(&model, 21, 1, host.0, host.1),
            None
        );

        model.open_client_global_menu();
        let snapshot = ClientSidebarSnapshot::from_model(
            &model,
            &compositor,
            26,
            host.0,
            host.1,
            Instant::now(),
        );
        let rect = snapshot.app.global_menu_rect();
        // the first item row sits one cell inside the menu's top-left border.
        assert_eq!(
            compositor.client_global_menu_item_at(&model, rect.x + 1, rect.y + 1, host.0, host.1),
            Some(0)
        );
        // a deeper row resolves to its index.
        assert_eq!(
            compositor.client_global_menu_item_at(&model, rect.x + 1, rect.y + 3, host.0, host.1),
            Some(2)
        );
        // the far-left sidebar column misses the right-anchored menu → None.
        assert_eq!(
            compositor.client_global_menu_item_at(&model, 0, rect.y + 1, host.0, host.1),
            None
        );
    }

    fn model_hover_anywhere(model: &ClientSupervisorModel, y: u16) -> Option<SidebarHoverTarget> {
        ClientCompositor::new(26).hover_test(model, 1, y, 60, 16)
    }

    #[test]
    fn hover_test_none_when_collapsed() {
        // effective_sidebar_width == 0 (host_width <= 1) → None.
        let (model, _remote_id) = mixed_supervisor_model();
        let compositor = ClientCompositor::new(26);
        assert_eq!(compositor.hover_test(&model, 0, 0, 1, 16), None);
    }

    #[test]
    fn hover_test_resolves_new_workspace_picker_destination_row() {
        // the footer-anchored picker popup hovers its destination rows to
        // NewWorkspaceDestination { row }.
        let (model, _remote_id) = two_destination_picker_model();
        let compositor = ClientCompositor::new(26);
        let host = (60u16, 20u16);
        let anchor = anchor_area(&model, &compositor, host.0, host.1);
        let inner = crate::ui::new_workspace_picker_inner_rect(anchor, 2).expect("modal fits");
        let row1 = crate::ui::new_workspace_picker_row_rect(inner, 1);
        assert_eq!(
            compositor.hover_test(&model, row1.x, row1.y, host.0, host.1),
            Some(SidebarHoverTarget::NewWorkspaceDestination { row: 1 })
        );
    }

    // a [Main, Secondary] model with a remote agent, for agent-hover tests.
    fn mixed_supervisor_model_with_agent() -> (ClientSupervisorModel, ServerId) {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            disabled: false,
        });
        model
            .set_summary(
                &ServerId::main(),
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "main-herdr".into(),
                        label: "herdr".into(),
                        branch: None,
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-api".into(),
                        label: "api".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: vec![AgentSummary {
                        agent_id: "remote-agent".into(),
                        workspace_id: "remote-api".into(),
                        label: "claude".into(),
                        status: "idle".into(),
                        focused: false,
                    }],
                },
            )
            .unwrap();
        (model, remote_id)
    }
}
