use std::collections::HashMap;

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::Span,
    widgets::Paragraph,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
}

type NewWorkspacePickerLayout = (
    Option<u16>,
    Vec<(u16, crate::client::supervisor::ServerDestination)>,
);

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
}

impl ClientCompositor {
    pub(crate) fn new(sidebar_width: u16) -> Self {
        Self {
            sidebar_width,
            workspace_scroll: 0,
            agent_panel_scroll: 0,
            resizing_sidebar: false,
        }
    }

    pub(crate) fn sidebar_width(&self) -> u16 {
        self.sidebar_width
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

        let snapshot =
            ClientSidebarSnapshot::from_model(model, self, sidebar_width, host_width, host_height);
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
    ) -> FrameData {
        let sidebar_width = self.effective_sidebar_width(host_width);
        let content_width = host_width.saturating_sub(sidebar_width);
        let snapshot =
            ClientSidebarSnapshot::from_model(model, self, sidebar_width, host_width, host_height);
        let global_menu_rect = snapshot.global_menu_rect();
        let mut frame = render_client_shell(&snapshot, host_width, host_height);

        copy_active_content_excluding(
            active_frame,
            &mut frame,
            sidebar_width,
            content_width,
            global_menu_rect,
        );
        self.draw_new_workspace_picker(model, &mut frame, sidebar_width);

        if model.add_remote_form().is_some() {
            draw_add_remote_form(model, &mut frame);
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

        let snapshot =
            ClientSidebarSnapshot::from_model(model, self, sidebar_width, host_width, host_height);

        if let Some(target) = hit_test_global_menu(&snapshot.app, x, y) {
            return Some(target);
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

        if let Some((_, destination_rows)) = new_workspace_picker_layout(model, host_height) {
            for (row, destination) in destination_rows {
                if y == row {
                    return Some(SidebarHitTarget::NewWorkspaceDestination {
                        server_id: destination.server_id,
                    });
                }
            }
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

    fn draw_new_workspace_picker(
        &self,
        model: &crate::client::supervisor::ClientSupervisorModel,
        frame: &mut FrameData,
        sidebar_width: u16,
    ) {
        let Some((label_row, destination_rows)) = new_workspace_picker_layout(model, frame.height)
        else {
            return;
        };

        if let Some(label_row) = label_row {
            draw_text_cleared(frame, 0, label_row, sidebar_width, "create on");
        }
        for (row, destination) in destination_rows {
            draw_text_cleared(
                frame,
                0,
                row,
                sidebar_width,
                &format!("+ {}", destination.display_name),
            );
        }
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
        app.global_menu_extra_labels = vec!["add remote"];
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
                terminal.state = state;
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
        app.view.workspace_card_areas =
            crate::ui::compute_workspace_card_areas(&app, app.view.sidebar_rect);

        Self {
            app,
            filter_label: model.filter_label(),
            workspace_routes,
            agent_routes,
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
    frame.render_widget(
        Paragraph::new(Span::styled(
            snapshot.filter_label.clone(),
            Style::default()
                .fg(snapshot.app.palette.overlay0)
                .add_modifier(Modifier::BOLD),
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

fn hit_test_global_menu(app: &crate::app::AppState, x: u16, y: u16) -> Option<SidebarHitTarget> {
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
    (index < app.global_menu_labels().len())
        .then_some(SidebarHitTarget::ClientGlobalMenuItem { index })
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

fn new_workspace_picker_layout(
    model: &crate::client::supervisor::ClientSupervisorModel,
    host_height: u16,
) -> Option<NewWorkspacePickerLayout> {
    let destinations = model.new_workspace_picker_destinations()?;
    if destinations.is_empty() || host_height < 3 {
        return None;
    }

    let footer_row = host_height.saturating_sub(1);
    let available_destination_rows = footer_row.saturating_sub(1);
    if available_destination_rows == 0 {
        return None;
    }

    let visible_destination_count = destinations.len().min(available_destination_rows as usize);
    if visible_destination_count == 0 {
        return None;
    }

    let destination_start = footer_row.saturating_sub(visible_destination_count as u16);
    let label_row = (destination_start > 1).then_some(destination_start - 1);
    let destination_rows = destinations
        .iter()
        .take(visible_destination_count)
        .enumerate()
        .map(|(offset, destination)| (destination_start + offset as u16, destination.clone()))
        .collect();

    Some((label_row, destination_rows))
}

fn draw_add_remote_form(
    model: &crate::client::supervisor::ClientSupervisorModel,
    frame: &mut FrameData,
) {
    let Some(form) = model.add_remote_form() else {
        return;
    };
    if frame.width < 12 || frame.height < 7 {
        return;
    }

    let popup_width = frame
        .width
        .saturating_sub(4)
        .min(54)
        .max(28.min(frame.width));
    let popup_height = 9.min(frame.height);
    let x = (frame.width.saturating_sub(popup_width)) / 2;
    let y = (frame.height.saturating_sub(popup_height)) / 2;
    draw_box(frame, x, y, popup_width, popup_height);

    let inner_x = x + 1;
    let inner_width = popup_width.saturating_sub(2);
    draw_text_cleared(frame, inner_x, y + 1, inner_width, "add remote");

    let target_marker = if form.focused_field == crate::client::supervisor::AddRemoteField::Target {
        ">"
    } else {
        " "
    };
    let name_marker = if form.focused_field == crate::client::supervisor::AddRemoteField::Name {
        ">"
    } else {
        " "
    };
    draw_text_cleared(
        frame,
        inner_x,
        y + 3,
        inner_width,
        &format!("{target_marker} target  {}", form.target),
    );
    draw_text_cleared(
        frame,
        inner_x,
        y + 4,
        inner_width,
        &format!("{name_marker} name    {}", form.name),
    );
    if let Some(error) = &form.error {
        draw_text_cleared(frame, inner_x, y + 6, inner_width, error);
    }
    draw_text_cleared(frame, inner_x, y + 7, inner_width, "enter add   esc close");
}

fn draw_text(frame: &mut FrameData, x: u16, y: u16, max_width: u16, text: &str) {
    if y >= frame.height {
        return;
    }
    let mut offset: u16 = 0;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0).max(1) as u16;
        if offset.saturating_add(width) > max_width {
            break;
        }
        let col = x.saturating_add(offset);
        if col >= frame.width {
            break;
        }
        let idx = (y as usize) * (frame.width as usize) + (col as usize);
        if let Some(cell) = frame.cells.get_mut(idx) {
            cell.symbol = ch.to_string();
            cell.skip = false;
            cell.hyperlink = None;
        }
        offset = offset.saturating_add(width);
    }
}

fn draw_box(frame: &mut FrameData, x: u16, y: u16, width: u16, height: u16) {
    if width < 2 || height < 2 {
        return;
    }
    for row in 0..height {
        for col in 0..width {
            let symbol = if (row == 0 || row == height - 1) && (col == 0 || col == width - 1) {
                match (row == 0, col == 0) {
                    (true, true) => "╭",
                    (true, false) => "╮",
                    (false, true) => "╰",
                    (false, false) => "╯",
                }
            } else if row == 0 || row == height - 1 {
                "─"
            } else if col == 0 || col == width - 1 {
                "│"
            } else {
                " "
            };
            put_symbol(frame, x + col, y + row, symbol);
        }
    }
}

fn put_symbol(frame: &mut FrameData, x: u16, y: u16, symbol: &str) {
    if x >= frame.width || y >= frame.height {
        return;
    }
    let idx = (y as usize) * (frame.width as usize) + (x as usize);
    if let Some(cell) = frame.cells.get_mut(idx) {
        cell.symbol = symbol.into();
        cell.skip = false;
        cell.hyperlink = None;
    }
}

fn draw_text_cleared(frame: &mut FrameData, x: u16, y: u16, max_width: u16, text: &str) {
    if y >= frame.height {
        return;
    }
    for offset in 0..max_width {
        let col = x.saturating_add(offset);
        if col >= frame.width {
            break;
        }
        let idx = (y as usize) * (frame.width as usize) + (col as usize);
        if let Some(cell) = frame.cells.get_mut(idx) {
            cell.symbol = " ".into();
            cell.skip = false;
            cell.hyperlink = None;
        }
    }
    draw_text(frame, x, y, max_width, text);
}

fn copy_active_content_excluding(
    active_frame: &FrameData,
    target: &mut FrameData,
    target_x: u16,
    target_width: u16,
    excluded_rect: Option<Rect>,
) {
    let copy_width = target_width.min(active_frame.width);
    let copy_height = target.height.min(active_frame.height);
    for row in 0..copy_height {
        for col in 0..copy_width {
            let source_idx = (row as usize) * (active_frame.width as usize) + (col as usize);
            let target_col = target_x + col;
            if excluded_rect.is_some_and(|rect| rect_contains(rect, target_col, row)) {
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
        AgentSummary, ClientSupervisorModel, ServerId, ServerSummary, WorkspaceSummary,
    };
    use crate::protocol::CursorState;

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
        let composed = ClientCompositor::new(26).compose_frame(&model, &content, 60, 20);

        assert_eq!(composed.width, 60);
        assert_eq!(composed.height, 20);
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
        assert!(rows.iter().any(|row| row.contains("x api")));
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
        let composed = ClientCompositor::new(26).compose_frame(&model, &content, 60, 16);
        let rows: Vec<_> = (0..composed.height)
            .map(|row| row_text(&composed, row))
            .collect();

        assert!(rows.iter().any(|row| row.contains("x api")));
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

        let composed = compositor.compose_frame(&model, &content, 8, 3);

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
    fn draw_text_cleared_truncates_by_display_width() {
        let mut output = blank_frame(3, 1);

        draw_text_cleared(&mut output, 0, 0, 2, "가x");

        assert_eq!(output.cells[0].symbol, "가");
        assert_eq!(output.cells[1].symbol, " ");
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

        assert_eq!(
            compositor.hit_test(&model, 23, 0, 60, 16),
            Some(SidebarHitTarget::Filter)
        );
        assert_eq!(
            compositor.hit_test(&model, 1, 2, 60, 16),
            Some(SidebarHitTarget::Workspace {
                server_id: ServerId::main(),
                workspace_id: "main-herdr".into(),
            })
        );
        assert_eq!(
            compositor.hit_test(&model, 1, 4, 60, 16),
            Some(SidebarHitTarget::Workspace {
                server_id: remote_id.clone(),
                workspace_id: "remote-api".into(),
            })
        );
        assert_eq!(
            compositor.hit_test(&model, 1, 11, 60, 16),
            Some(SidebarHitTarget::Agent {
                server_id: remote_id,
                agent_id: "remote-agent".into(),
            })
        );
        assert_eq!(
            compositor.hit_test(&model, 1, 7, 60, 16),
            Some(SidebarHitTarget::New)
        );
        assert_eq!(
            compositor.hit_test(&model, 23, 7, 60, 16),
            Some(SidebarHitTarget::Menu)
        );
        assert_eq!(compositor.hit_test(&model, 27, 2, 60, 16), None);
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

    #[test]
    fn destination_picker_draws_and_hit_tests_destination_rows() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
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

        let compositor = ClientCompositor::new(26);
        let content = frame(8, 3, &["content", "frame"]);
        let composed = compositor.compose_frame(&model, &content, 60, 16);

        assert!(row_text(&composed, 12).starts_with("create on"));
        assert!(row_text(&composed, 13).starts_with("+ local"));
        assert!(row_text(&composed, 14).starts_with("+ x"));
        assert_eq!(
            compositor.hit_test(&model, 1, 13, 60, 16),
            Some(SidebarHitTarget::NewWorkspaceDestination {
                server_id: ServerId::main(),
            })
        );
        assert_eq!(
            compositor.hit_test(&model, 1, 14, 60, 16),
            Some(SidebarHitTarget::NewWorkspaceDestination {
                server_id: remote_id,
            })
        );
    }

    #[test]
    fn client_global_menu_uses_server_launcher_menu_surface() {
        let mut model = ClientSupervisorModel::new("local");
        model.open_client_global_menu();

        let compositor = ClientCompositor::new(26);
        let content = frame(8, 3, &["content", "frame"]);
        let composed = compositor.compose_frame(&model, &content, 60, 16);

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
    fn add_remote_form_uses_modal_surface_instead_of_ascii_placeholder() {
        let mut model = ClientSupervisorModel::new("local");
        model.open_add_remote_form();

        let compositor = ClientCompositor::new(26);
        let content = frame(20, 8, &["content", "frame"]);
        let composed = compositor.compose_frame(&model, &content, 80, 24);
        let rows: Vec<_> = (0..composed.height)
            .map(|row| row_text(&composed, row))
            .collect();

        assert!(rows.iter().any(|row| row.contains("╭")));
        assert!(rows.iter().any(|row| row.contains("add remote")));
        assert!(rows.iter().any(|row| row.contains("target")));
        assert!(rows.iter().any(|row| row.contains("name")));
        assert!(!rows.iter().any(|row| row.contains("+---")));
    }
}
