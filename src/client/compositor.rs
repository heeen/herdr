use crate::protocol::{CellData, CursorState, FrameData};

pub(crate) const DEFAULT_SIDEBAR_WIDTH: u16 = 26;

pub(crate) struct ClientCompositor {
    sidebar_width: u16,
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

impl ClientCompositor {
    pub(crate) fn new(sidebar_width: u16) -> Self {
        Self { sidebar_width }
    }

    pub(crate) fn sidebar_width(&self) -> u16 {
        self.sidebar_width
    }

    pub(crate) fn compose_frame(
        &self,
        model: &crate::client::supervisor::ClientSupervisorModel,
        active_frame: &FrameData,
        host_width: u16,
        host_height: u16,
    ) -> FrameData {
        let sidebar_width = self.sidebar_width.min(host_width);
        let content_width = host_width.saturating_sub(sidebar_width);
        let mut frame = blank_frame(host_width, host_height);

        self.draw_sidebar(model, &mut frame, sidebar_width);
        copy_active_content(active_frame, &mut frame, sidebar_width, content_width);

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
            host_width.saturating_sub(self.sidebar_width).max(1),
            host_height,
        )
    }

    pub(crate) fn hit_test(
        &self,
        model: &crate::client::supervisor::ClientSupervisorModel,
        x: u16,
        y: u16,
        host_width: u16,
        host_height: u16,
    ) -> Option<SidebarHitTarget> {
        let sidebar_width = self.sidebar_width.min(host_width);
        if sidebar_width == 0 || host_height == 0 || x >= sidebar_width || y >= host_height {
            return None;
        }

        if y == 0 {
            return right_label_hit(x, "spaces", &model.filter_label(), sidebar_width)
                .then_some(SidebarHitTarget::Filter);
        }

        let footer_row = host_height.saturating_sub(1);
        if y == footer_row {
            if x < "new".chars().count() as u16 {
                return Some(SidebarHitTarget::New);
            }
            return right_label_hit(x, "new", "menu", sidebar_width)
                .then_some(SidebarHitTarget::Menu);
        }

        if let Some((_, item_rows)) = client_global_menu_layout(model, host_height) {
            for (row, index) in item_rows {
                if y == row {
                    return Some(SidebarHitTarget::ClientGlobalMenuItem { index });
                }
            }
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

        let mut row = 1;
        for workspace in model.workspace_rows() {
            if row >= footer_row {
                return None;
            }
            if y == row {
                if workspace.disabled {
                    return None;
                }
                return workspace
                    .workspace_id
                    .map(|workspace_id| SidebarHitTarget::Workspace {
                        server_id: workspace.server_id,
                        workspace_id,
                    });
            }
            row += 1;
        }

        if row < footer_row {
            row += 1;
        }
        if row < footer_row {
            if y == row {
                return None;
            }
            row += 1;
        }

        for group in model.agent_groups() {
            if row >= footer_row {
                return None;
            }
            if y == row {
                return Some(SidebarHitTarget::Workspace {
                    server_id: group.server_id,
                    workspace_id: group.workspace_id,
                });
            }
            row += 1;

            for agent in group.agents {
                if row >= footer_row {
                    return None;
                }
                if y == row {
                    return Some(SidebarHitTarget::Agent {
                        server_id: group.server_id.clone(),
                        agent_id: agent.agent_id,
                    });
                }
                row += 1;
            }
        }

        None
    }

    fn draw_sidebar(
        &self,
        model: &crate::client::supervisor::ClientSupervisorModel,
        frame: &mut FrameData,
        sidebar_width: u16,
    ) {
        if sidebar_width == 0 || frame.height == 0 {
            return;
        }

        let mut row = 0;
        draw_text(
            frame,
            0,
            row,
            sidebar_width,
            &left_right_label("spaces", &model.filter_label(), sidebar_width),
        );
        row += 1;

        for workspace in model.workspace_rows() {
            if row >= frame.height.saturating_sub(1) {
                return;
            }
            let marker = if workspace.focused { "O" } else { "-" };
            draw_text(
                frame,
                0,
                row,
                sidebar_width,
                &format!("{marker} {}", workspace.label),
            );
            row += 1;
        }

        if row < frame.height.saturating_sub(1) {
            row += 1;
        }
        if row < frame.height.saturating_sub(1) {
            draw_text(frame, 0, row, sidebar_width, "agents");
            row += 1;
        }

        for group in model.agent_groups() {
            if row >= frame.height.saturating_sub(1) {
                return;
            }
            let marker = if group.focused { "O" } else { "v" };
            draw_text(
                frame,
                0,
                row,
                sidebar_width,
                &format!("{marker} {}", group.label),
            );
            row += 1;

            for agent in group.agents {
                if row >= frame.height.saturating_sub(1) {
                    return;
                }
                draw_text(
                    frame,
                    0,
                    row,
                    sidebar_width,
                    &format!("  {} {}", agent.status, agent.label),
                );
                row += 1;
            }
        }

        self.draw_new_workspace_picker(model, frame, sidebar_width);
        self.draw_client_global_menu(model, frame, sidebar_width);
        draw_text(
            frame,
            0,
            frame.height.saturating_sub(1),
            sidebar_width,
            &left_right_label("new", "menu", sidebar_width),
        );
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

    fn draw_client_global_menu(
        &self,
        model: &crate::client::supervisor::ClientSupervisorModel,
        frame: &mut FrameData,
        sidebar_width: u16,
    ) {
        let Some((title_row, item_rows)) = client_global_menu_layout(model, frame.height) else {
            return;
        };
        draw_text_cleared(frame, 0, title_row, sidebar_width, "menu");
        let highlighted = model.client_global_menu_highlighted().unwrap_or(0);
        let items = model.client_global_menu_items();
        for (row, index) in item_rows {
            let marker = if index == highlighted { ">" } else { " " };
            if let Some(item) = items.get(index) {
                draw_text_cleared(frame, 0, row, sidebar_width, &format!("{marker} {item}"));
            }
        }
    }
}

impl Default for ClientCompositor {
    fn default() -> Self {
        Self::new(DEFAULT_SIDEBAR_WIDTH)
    }
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

fn left_right_label(left: &str, right: &str, width: u16) -> String {
    let width = width as usize;
    if width == 0 {
        return String::new();
    }
    let left_width = left.chars().count();
    let right_width = right.chars().count();
    if left_width + 1 + right_width > width {
        return left.chars().take(width).collect();
    }
    format!(
        "{left}{:gap$}{right}",
        "",
        gap = width - left_width - right_width
    )
}

fn right_label_hit(x: u16, left: &str, right: &str, width: u16) -> bool {
    let width = width as usize;
    if width == 0 {
        return false;
    }
    let left_width = left.chars().count();
    let right_width = right.chars().count();
    if right_width == 0 || left_width + 1 + right_width > width {
        return false;
    }
    x as usize >= width - right_width
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

fn client_global_menu_layout(
    model: &crate::client::supervisor::ClientSupervisorModel,
    host_height: u16,
) -> Option<(u16, Vec<(u16, usize)>)> {
    model.client_global_menu_highlighted()?;
    let items = model.client_global_menu_items();
    if items.is_empty() || host_height < 4 {
        return None;
    }

    let footer_row = host_height.saturating_sub(1);
    let visible_item_count = items.len().min(footer_row.saturating_sub(1) as usize);
    if visible_item_count == 0 {
        return None;
    }

    let title_row = footer_row.saturating_sub(visible_item_count as u16 + 1);
    let item_rows = (0..visible_item_count)
        .map(|index| (title_row + 1 + index as u16, index))
        .collect();
    Some((title_row, item_rows))
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

    let popup_width = frame.width.saturating_sub(2).min(52);
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
        &format!("{target_marker} target: {}", form.target),
    );
    draw_text_cleared(
        frame,
        inner_x,
        y + 4,
        inner_width,
        &format!("{name_marker} name: {}", form.name),
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
    for (offset, ch) in text.chars().take(max_width as usize).enumerate() {
        let col = x.saturating_add(offset as u16);
        if col >= frame.width {
            break;
        }
        let idx = (y as usize) * (frame.width as usize) + (col as usize);
        if let Some(cell) = frame.cells.get_mut(idx) {
            cell.symbol = ch.to_string();
            cell.skip = false;
            cell.hyperlink = None;
        }
    }
}

fn draw_box(frame: &mut FrameData, x: u16, y: u16, width: u16, height: u16) {
    if width < 2 || height < 2 {
        return;
    }
    for row in 0..height {
        for col in 0..width {
            let symbol = if (row == 0 || row == height - 1) && (col == 0 || col == width - 1) {
                "+"
            } else if row == 0 || row == height - 1 {
                "-"
            } else if col == 0 || col == width - 1 {
                "|"
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

fn copy_active_content(
    active_frame: &FrameData,
    target: &mut FrameData,
    target_x: u16,
    target_width: u16,
) {
    let copy_width = target_width.min(active_frame.width);
    let copy_height = target.height.min(active_frame.height);
    for row in 0..copy_height {
        for col in 0..copy_width {
            let source_idx = (row as usize) * (active_frame.width as usize) + (col as usize);
            let target_col = target_x + col;
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
    fn compose_frame_draws_unified_sidebar_and_offsets_active_content() {
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
        let composed = ClientCompositor::new(12).compose_frame(&model, &content, 24, 6);

        assert_eq!(composed.width, 24);
        assert_eq!(composed.height, 6);
        assert!(row_text(&composed, 0).starts_with("spaces   all"));
        assert!(row_text(&composed, 1).starts_with("O herdr"));
        assert!(row_text(&composed, 2).starts_with("- x api"));
        assert!(row_text(&composed, 0)[12..].starts_with("content"));
        assert!(row_text(&composed, 1)[12..].starts_with("frame"));
        assert_eq!(
            composed.cursor,
            Some(CursorState {
                x: 13,
                y: 1,
                visible: true,
                shape: 2,
            })
        );
    }

    #[test]
    fn content_size_reserves_sidebar_width_and_keeps_one_column_minimum() {
        let compositor = ClientCompositor::new(12);

        assert_eq!(compositor.content_size(80, 24), (68, 24));
        assert_eq!(compositor.content_size(8, 24), (1, 24));
    }

    #[test]
    fn hit_test_identifies_sidebar_filter_rows_and_footer_actions() {
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

        let compositor = ClientCompositor::new(12);

        assert_eq!(
            compositor.hit_test(&model, 10, 0, 24, 8),
            Some(SidebarHitTarget::Filter)
        );
        assert_eq!(
            compositor.hit_test(&model, 1, 1, 24, 8),
            Some(SidebarHitTarget::Workspace {
                server_id: ServerId::main(),
                workspace_id: "main-herdr".into(),
            })
        );
        assert_eq!(
            compositor.hit_test(&model, 1, 2, 24, 8),
            Some(SidebarHitTarget::Workspace {
                server_id: remote_id.clone(),
                workspace_id: "remote-api".into(),
            })
        );
        assert_eq!(
            compositor.hit_test(&model, 1, 5, 24, 8),
            Some(SidebarHitTarget::Workspace {
                server_id: remote_id.clone(),
                workspace_id: "remote-api".into(),
            })
        );
        assert_eq!(
            compositor.hit_test(&model, 1, 6, 24, 8),
            Some(SidebarHitTarget::Agent {
                server_id: remote_id,
                agent_id: "remote-agent".into(),
            })
        );
        assert_eq!(
            compositor.hit_test(&model, 1, 7, 24, 8),
            Some(SidebarHitTarget::New)
        );
        assert_eq!(
            compositor.hit_test(&model, 9, 7, 24, 8),
            Some(SidebarHitTarget::Menu)
        );
        assert_eq!(compositor.hit_test(&model, 13, 1, 24, 8), None);
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

        let compositor = ClientCompositor::new(12);

        assert_eq!(compositor.hit_test(&model, 1, 1, 24, 4), None);
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

        let compositor = ClientCompositor::new(12);
        let content = frame(8, 3, &["content", "frame"]);
        let composed = compositor.compose_frame(&model, &content, 24, 8);

        assert!(row_text(&composed, 4).starts_with("create on"));
        assert!(row_text(&composed, 5).starts_with("+ local"));
        assert!(row_text(&composed, 6).starts_with("+ x"));
        assert_eq!(
            compositor.hit_test(&model, 1, 5, 24, 8),
            Some(SidebarHitTarget::NewWorkspaceDestination {
                server_id: ServerId::main(),
            })
        );
        assert_eq!(
            compositor.hit_test(&model, 1, 6, 24, 8),
            Some(SidebarHitTarget::NewWorkspaceDestination {
                server_id: remote_id,
            })
        );
    }

    #[test]
    fn client_global_menu_draws_and_hit_tests_add_remote_item() {
        let mut model = ClientSupervisorModel::new("local");
        model.open_client_global_menu();

        let compositor = ClientCompositor::new(12);
        let content = frame(8, 3, &["content", "frame"]);
        let composed = compositor.compose_frame(&model, &content, 24, 8);

        assert!(row_text(&composed, 5).starts_with("menu"));
        assert!(row_text(&composed, 6).starts_with("> add remote"));
        assert_eq!(
            compositor.hit_test(&model, 1, 6, 24, 8),
            Some(SidebarHitTarget::ClientGlobalMenuItem { index: 0 })
        );
    }
}
