// This model is being wired into the thin client in vertical slices. Some
// routing and sidebar methods are test-covered before the real client-owned UI
// calls them, so the non-test binary temporarily sees them as unused.
#![cfg_attr(not(test), allow(dead_code))]

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ServerId(String);

impl ServerId {
    pub(crate) fn main() -> Self {
        Self("main".to_string())
    }

    pub(crate) fn secondary(id: impl Into<String>) -> Self {
        Self(format!("secondary:{}", id.into()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServerFilter {
    All,
    Server(ServerId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerRole {
    Main,
    Secondary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectionState {
    Connecting,
    Connected,
    Disconnected,
    ProtocolMismatch {
        server_protocol: Option<u32>,
        client_protocol: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServerConnectionTarget {
    Main,
    LocalSession(Option<String>),
    Ssh(String),
}

impl From<crate::remote_registry::RemoteTargetSnapshot> for ServerConnectionTarget {
    fn from(target: crate::remote_registry::RemoteTargetSnapshot) -> Self {
        match target {
            crate::remote_registry::RemoteTargetSnapshot::Local { session } => {
                Self::LocalSession(session)
            }
            crate::remote_registry::RemoteTargetSnapshot::Ssh { target } => Self::Ssh(target),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ServerSummary {
    pub(crate) workspaces: Vec<WorkspaceSummary>,
    pub(crate) agents: Vec<AgentSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceSummary {
    pub(crate) workspace_id: String,
    pub(crate) label: String,
    pub(crate) branch: Option<String>,
    pub(crate) focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentSummary {
    pub(crate) agent_id: String,
    pub(crate) workspace_id: String,
    pub(crate) label: String,
    pub(crate) status: String,
    pub(crate) focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedServer {
    pub(crate) id: ServerId,
    pub(crate) display_name: String,
    pub(crate) role: ServerRole,
    pub(crate) target: ServerConnectionTarget,
    pub(crate) keybindings: crate::remote_registry::RemoteKeybindingsSnapshot,
    pub(crate) connection_state: ConnectionState,
    pub(crate) summaries: ServerSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServerDestination {
    pub(crate) server_id: ServerId,
    pub(crate) display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecondaryConnectionPlan {
    pub(crate) server_id: ServerId,
    pub(crate) display_name: String,
    pub(crate) target: ServerConnectionTarget,
    pub(crate) keybindings: crate::remote_registry::RemoteKeybindingsSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SummarySubscriptionPlan {
    pub(crate) server_id: ServerId,
    pub(crate) target: ServerConnectionTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientGlobalMenuAction {
    Settings,
    Keybinds,
    ReloadConfig,
    Detach,
    AddRemote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddRemoteField {
    Target,
    Name,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddRemoteForm {
    pub(crate) target: String,
    pub(crate) name: String,
    pub(crate) focused_field: AddRemoteField,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddRemoteDraft {
    pub(crate) target: String,
    pub(crate) name: Option<String>,
    pub(crate) keybindings: crate::remote_registry::RemoteKeybindingsSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AddRemoteFormOutcome {
    Redraw,
    Submit(AddRemoteDraft),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClientOverlayState {
    None,
    GlobalMenu { highlighted: usize },
    AddRemote(AddRemoteForm),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NewWorkspaceRoute {
    CreateOn(ServerId),
    PickDestination(Vec<ServerDestination>),
    Unavailable { server_id: ServerId, reason: String },
}

impl NewWorkspaceRoute {
    pub(crate) fn api_request(
        &self,
        id: impl Into<String>,
    ) -> Option<(ServerId, crate::api::schema::Request)> {
        match self {
            NewWorkspaceRoute::CreateOn(server_id) => Some((
                server_id.clone(),
                crate::api::schema::Request {
                    id: id.into(),
                    method: crate::api::schema::Method::WorkspaceCreate(
                        crate::api::schema::WorkspaceCreateParams {
                            cwd: None,
                            focus: true,
                            label: None,
                        },
                    ),
                },
            )),
            NewWorkspaceRoute::PickDestination(_) | NewWorkspaceRoute::Unavailable { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FocusRoute {
    Workspace {
        server_id: ServerId,
        workspace_id: String,
    },
    Agent {
        server_id: ServerId,
        target: String,
    },
    Unavailable {
        server_id: ServerId,
        reason: String,
    },
    NotFound,
}

impl FocusRoute {
    pub(crate) fn api_request(&self, id: impl Into<String>) -> Option<crate::api::schema::Request> {
        match self {
            FocusRoute::Workspace { workspace_id, .. } => Some(crate::api::schema::Request {
                id: id.into(),
                method: crate::api::schema::Method::WorkspaceFocus(
                    crate::api::schema::WorkspaceTarget {
                        workspace_id: workspace_id.clone(),
                    },
                ),
            }),
            FocusRoute::Agent { target, .. } => Some(crate::api::schema::Request {
                id: id.into(),
                method: crate::api::schema::Method::AgentFocus(crate::api::schema::AgentTarget {
                    target: target.clone(),
                }),
            }),
            FocusRoute::Unavailable { .. } | FocusRoute::NotFound => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceSidebarRow {
    pub(crate) server_id: ServerId,
    pub(crate) workspace_id: Option<String>,
    pub(crate) label: String,
    pub(crate) branch: Option<String>,
    pub(crate) focused: bool,
    pub(crate) disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentSidebarRow {
    pub(crate) agent_id: String,
    pub(crate) label: String,
    pub(crate) status: String,
    pub(crate) focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentSidebarGroup {
    pub(crate) server_id: ServerId,
    pub(crate) workspace_id: String,
    pub(crate) label: String,
    pub(crate) focused: bool,
    pub(crate) agents: Vec<AgentSidebarRow>,
}

pub(crate) struct ClientSupervisorModel {
    servers: Vec<ManagedServer>,
    filter: ServerFilter,
    active_server_id: ServerId,
    ui_settings: crate::api::schema::UiSettingsInfo,
    new_workspace_picker_destinations: Option<Vec<ServerDestination>>,
    client_overlay: ClientOverlayState,
}

const SUPERVISOR_API_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

pub(crate) trait SupervisorApi {
    fn request(
        &mut self,
        request: crate::api::schema::Request,
    ) -> Result<crate::api::schema::SuccessResponse, String>;
}

impl SupervisorApi for crate::api::client::ApiClient {
    fn request(
        &mut self,
        request: crate::api::schema::Request,
    ) -> Result<crate::api::schema::SuccessResponse, String> {
        let value = self
            .request_value_with_timeout(&request, SUPERVISOR_API_TIMEOUT)
            .map_err(|err| err.to_string())?;
        crate::api::client::parse_response_value(value).map_err(|err| err.to_string())
    }
}

pub(crate) fn bootstrap_from_main_api(
    api: &mut impl SupervisorApi,
    main_display_name: impl Into<String>,
) -> Result<ClientSupervisorModel, String> {
    let mut model = ClientSupervisorModel::new(main_display_name);
    let remotes = request_remote_list(api)?;
    model.sync_remote_registry(remotes);
    let summary = request_server_summary(api)?;
    model
        .set_summary(&ServerId::main(), summary)
        .map_err(|()| "main server is missing from supervisor model".to_string())?;
    match request_ui_settings(api) {
        Ok(ui_settings) => model.set_ui_settings(ui_settings),
        Err(err) => tracing::warn!(
            err = %err,
            "failed to fetch main server UI settings; using defaults"
        ),
    }
    Ok(model)
}

impl ClientSupervisorModel {
    pub(crate) fn new(main_display_name: impl Into<String>) -> Self {
        Self {
            servers: vec![ManagedServer {
                id: ServerId::main(),
                display_name: main_display_name.into(),
                role: ServerRole::Main,
                target: ServerConnectionTarget::Main,
                keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Server,
                connection_state: ConnectionState::Connected,
                summaries: ServerSummary::default(),
            }],
            filter: ServerFilter::All,
            active_server_id: ServerId::main(),
            ui_settings: crate::api::schema::UiSettingsInfo::default(),
            new_workspace_picker_destinations: None,
            client_overlay: ClientOverlayState::None,
        }
    }

    pub(crate) fn ui_settings(&self) -> &crate::api::schema::UiSettingsInfo {
        &self.ui_settings
    }

    pub(crate) fn set_ui_settings(&mut self, ui_settings: crate::api::schema::UiSettingsInfo) {
        self.ui_settings = ui_settings;
    }

    pub(crate) fn refresh_main_ui_settings_from_api(
        &mut self,
        api: &mut impl SupervisorApi,
    ) -> Result<(), String> {
        let ui_settings = request_ui_settings(api)?;
        self.set_ui_settings(ui_settings);
        Ok(())
    }

    pub(crate) fn refresh_remote_registry_from_api(
        &mut self,
        api: &mut impl SupervisorApi,
    ) -> Result<(), String> {
        let remotes = request_remote_list(api)?;
        self.sync_remote_registry(remotes);
        Ok(())
    }

    pub(crate) fn activate_main_server(&mut self) {
        self.close_new_workspace_picker();
        self.active_server_id = ServerId::main();
    }

    pub(crate) fn add_secondary(
        &mut self,
        definition: crate::remote_registry::RemoteDefinitionSnapshot,
    ) -> ServerId {
        let id = ServerId::secondary(definition.id.clone());
        self.servers
            .push(managed_secondary(definition, ConnectionState::Connected));
        id
    }

    pub(crate) fn sync_remote_registry(
        &mut self,
        remotes: Vec<crate::remote_registry::RemoteDefinitionSnapshot>,
    ) {
        let mut next_servers: Vec<ManagedServer> = self
            .servers
            .iter()
            .filter(|server| server.role == ServerRole::Main)
            .cloned()
            .collect();

        for definition in remotes {
            let id = ServerId::secondary(definition.id.clone());
            let existing = self
                .servers
                .iter()
                .find(|server| server.id == id && server.role == ServerRole::Secondary)
                .cloned();
            let mut server = existing.unwrap_or_else(|| {
                managed_secondary(definition.clone(), ConnectionState::Connecting)
            });
            server.display_name = definition.name;
            server.target = definition.target.into();
            server.keybindings = definition.keybindings;
            next_servers.push(server);
        }

        self.servers = next_servers;
        self.reconcile_selected_servers();
        self.reconcile_new_workspace_picker();
    }

    pub(crate) fn filter(&self) -> &ServerFilter {
        &self.filter
    }

    pub(crate) fn set_filter(&mut self, filter: ServerFilter) {
        self.close_new_workspace_picker();
        self.filter = match filter {
            ServerFilter::All => ServerFilter::All,
            ServerFilter::Server(id) if self.server(&id).is_some() => ServerFilter::Server(id),
            ServerFilter::Server(_) => ServerFilter::All,
        };
    }

    pub(crate) fn filter_label(&self) -> String {
        match &self.filter {
            ServerFilter::All => "all".to_string(),
            ServerFilter::Server(id) => self
                .server(id)
                .map(|server| server.display_name.clone())
                .unwrap_or_else(|| "all".to_string()),
        }
    }

    pub(crate) fn cycle_filter(&mut self) {
        self.close_new_workspace_picker();
        let order: Vec<ServerId> = self
            .servers
            .iter()
            .map(|server| server.id.clone())
            .collect();
        self.filter = match &self.filter {
            ServerFilter::All => order
                .first()
                .cloned()
                .map(ServerFilter::Server)
                .unwrap_or(ServerFilter::All),
            ServerFilter::Server(current) => order
                .iter()
                .position(|id| id == current)
                .and_then(|idx| order.get(idx + 1))
                .cloned()
                .map(ServerFilter::Server)
                .unwrap_or(ServerFilter::All),
        };
    }

    pub(crate) fn active_server_id(&self) -> &ServerId {
        &self.active_server_id
    }

    pub(crate) fn set_active_server(&mut self, id: ServerId) -> Result<(), ()> {
        if self.server(&id).is_none() {
            return Err(());
        }
        self.active_server_id = id;
        Ok(())
    }

    pub(crate) fn remove_secondary(&mut self, id: &ServerId) -> bool {
        let Some(index) = self
            .servers
            .iter()
            .position(|server| server.id == *id && server.role == ServerRole::Secondary)
        else {
            return false;
        };
        self.servers.remove(index);
        if matches!(&self.filter, ServerFilter::Server(selected) if selected == id) {
            self.filter = ServerFilter::All;
        }
        if &self.active_server_id == id {
            self.active_server_id = ServerId::main();
        }
        self.reconcile_new_workspace_picker();
        true
    }

    pub(crate) fn set_connection_state(
        &mut self,
        id: &ServerId,
        connection_state: ConnectionState,
    ) -> Result<(), ()> {
        let is_connected = connection_state == ConnectionState::Connected;
        let Some(server) = self.server_mut(id) else {
            return Err(());
        };
        server.connection_state = connection_state;
        if &self.active_server_id == id && !is_connected {
            self.active_server_id = ServerId::main();
        }
        self.reconcile_new_workspace_picker();
        Ok(())
    }

    pub(crate) fn set_summary(&mut self, id: &ServerId, summary: ServerSummary) -> Result<(), ()> {
        let Some(server) = self.server_mut(id) else {
            return Err(());
        };
        server.summaries = summary;
        Ok(())
    }

    pub(crate) fn secondary_connection_plans(&self) -> Vec<SecondaryConnectionPlan> {
        let mut plans: Vec<SecondaryConnectionPlan> = self
            .servers
            .iter()
            .filter(|server| server.role == ServerRole::Secondary)
            .map(|server| SecondaryConnectionPlan {
                server_id: server.id.clone(),
                display_name: server.display_name.clone(),
                target: server.target.clone(),
                keybindings: server.keybindings,
            })
            .collect();
        plans.sort_by_key(|plan| connection_target_rank(&plan.target));
        plans
    }

    pub(crate) fn summary_subscription_plans(&self) -> Vec<SummarySubscriptionPlan> {
        self.servers
            .iter()
            .filter(|server| server.connection_state == ConnectionState::Connected)
            .map(|server| SummarySubscriptionPlan {
                server_id: server.id.clone(),
                target: server.target.clone(),
            })
            .collect()
    }

    pub(crate) fn server_connection_target(&self, id: &ServerId) -> Option<ServerConnectionTarget> {
        self.server(id).map(|server| server.target.clone())
    }

    pub(crate) fn unconnected_secondary_server_ids(&self) -> Vec<ServerId> {
        self.servers
            .iter()
            .filter(|server| server.role == ServerRole::Secondary)
            .filter(|server| {
                matches!(
                    server.connection_state,
                    ConnectionState::Connecting | ConnectionState::Disconnected
                )
            })
            .map(|server| server.id.clone())
            .collect()
    }

    pub(crate) fn secondary_server_ids_missing_client_stream(
        &self,
        connected_streams: &std::collections::HashSet<ServerId>,
    ) -> Vec<ServerId> {
        self.servers
            .iter()
            .filter(|server| server.role == ServerRole::Secondary)
            .filter(|server| !connected_streams.contains(&server.id))
            .filter(|server| {
                !matches!(
                    server.connection_state,
                    ConnectionState::ProtocolMismatch { .. }
                )
            })
            .map(|server| server.id.clone())
            .collect()
    }

    pub(crate) fn refresh_secondary_summaries(
        &mut self,
        mut fetch: impl FnMut(&SecondaryConnectionPlan) -> Result<ServerSummary, ConnectionState>,
    ) {
        let results: Vec<_> = self
            .secondary_connection_plans()
            .into_iter()
            .map(|plan| {
                let result = fetch(&plan);
                (plan.server_id, result)
            })
            .collect();
        self.apply_secondary_summary_results(results);
    }

    pub(crate) fn apply_secondary_summary_results(
        &mut self,
        results: impl IntoIterator<Item = (ServerId, Result<ServerSummary, ConnectionState>)>,
    ) {
        for (server_id, result) in results {
            match result {
                Ok(summary) => {
                    if let Some(server) = self.server_mut(&server_id) {
                        server.connection_state = ConnectionState::Connected;
                        server.summaries = summary;
                    }
                }
                Err(connection_state) => {
                    if let Some(server) = self.server_mut(&server_id) {
                        server.connection_state = connection_state;
                    }
                }
            }
        }
        self.reconcile_new_workspace_picker();
    }

    pub(crate) fn refresh_main_summary_from_api(
        &mut self,
        api: &mut impl SupervisorApi,
    ) -> Result<(), String> {
        let summary = request_server_summary(api)?;
        self.set_summary(&ServerId::main(), summary)
            .map_err(|()| "main server is missing from supervisor model".to_string())
    }

    pub(crate) fn refresh_local_secondary_summaries_from_api(&mut self) {
        self.refresh_secondary_summaries(|plan| match &plan.target {
            ServerConnectionTarget::LocalSession(session) => {
                fetch_local_secondary_summary(session.clone())
            }
            ServerConnectionTarget::Ssh(_) => Err(ConnectionState::Connecting),
            ServerConnectionTarget::Main => unreachable!("secondary plans never include main"),
        });
    }

    pub(crate) fn new_workspace_route(&self) -> NewWorkspaceRoute {
        match &self.filter {
            ServerFilter::Server(id) => self.route_for_specific_server(id),
            ServerFilter::All => {
                let destinations = self.connected_destinations();
                if destinations.len() > 1 {
                    NewWorkspaceRoute::PickDestination(destinations)
                } else if let Some(destination) = destinations.into_iter().next() {
                    NewWorkspaceRoute::CreateOn(destination.server_id)
                } else {
                    NewWorkspaceRoute::Unavailable {
                        server_id: ServerId::main(),
                        reason: "server disconnected".to_string(),
                    }
                }
            }
        }
    }

    pub(crate) fn new_workspace_picker_destinations(&self) -> Option<&[ServerDestination]> {
        self.new_workspace_picker_destinations.as_deref()
    }

    pub(crate) fn open_new_workspace_picker(&mut self) -> NewWorkspaceRoute {
        self.close_client_overlay();
        let route = self.new_workspace_route();
        match &route {
            NewWorkspaceRoute::CreateOn(server_id) => {
                self.new_workspace_picker_destinations = None;
                self.active_server_id = server_id.clone();
            }
            NewWorkspaceRoute::PickDestination(destinations) => {
                self.new_workspace_picker_destinations = Some(destinations.clone());
            }
            NewWorkspaceRoute::Unavailable { .. } => {
                self.new_workspace_picker_destinations = None;
            }
        }
        route
    }

    pub(crate) fn client_global_menu_items(&self) -> Vec<&'static str> {
        vec![
            "settings",
            "keybinds",
            "reload config",
            "detach",
            "add remote",
        ]
    }

    pub(crate) fn client_global_menu_highlighted(&self) -> Option<usize> {
        match self.client_overlay {
            ClientOverlayState::GlobalMenu { highlighted } => Some(highlighted),
            ClientOverlayState::None | ClientOverlayState::AddRemote(_) => None,
        }
    }

    pub(crate) fn open_client_global_menu(&mut self) {
        self.new_workspace_picker_destinations = None;
        self.client_overlay = ClientOverlayState::GlobalMenu { highlighted: 0 };
    }

    pub(crate) fn move_client_global_menu_next(&mut self) {
        let item_count = self.client_global_menu_items().len();
        if let ClientOverlayState::GlobalMenu { highlighted } = &mut self.client_overlay {
            *highlighted = (*highlighted + 1).min(item_count.saturating_sub(1));
        }
    }

    pub(crate) fn move_client_global_menu_prev(&mut self) {
        if let ClientOverlayState::GlobalMenu { highlighted } = &mut self.client_overlay {
            *highlighted = highlighted.saturating_sub(1);
        }
    }

    pub(crate) fn accept_client_global_menu_item(&mut self) -> Option<ClientGlobalMenuAction> {
        let highlighted = self.client_global_menu_highlighted()?;
        self.select_client_global_menu_item(highlighted)
    }

    pub(crate) fn select_client_global_menu_item(
        &mut self,
        index: usize,
    ) -> Option<ClientGlobalMenuAction> {
        match index {
            0 => {
                self.close_client_overlay();
                Some(ClientGlobalMenuAction::Settings)
            }
            1 => {
                self.close_client_overlay();
                Some(ClientGlobalMenuAction::Keybinds)
            }
            2 => {
                self.close_client_overlay();
                Some(ClientGlobalMenuAction::ReloadConfig)
            }
            3 => {
                self.close_client_overlay();
                Some(ClientGlobalMenuAction::Detach)
            }
            4 => {
                self.open_add_remote_form();
                Some(ClientGlobalMenuAction::AddRemote)
            }
            _ => None,
        }
    }

    pub(crate) fn open_add_remote_form(&mut self) {
        self.new_workspace_picker_destinations = None;
        self.client_overlay = ClientOverlayState::AddRemote(AddRemoteForm {
            target: String::new(),
            name: String::new(),
            focused_field: AddRemoteField::Target,
            error: None,
        });
    }

    pub(crate) fn add_remote_form(&self) -> Option<&AddRemoteForm> {
        match &self.client_overlay {
            ClientOverlayState::AddRemote(form) => Some(form),
            ClientOverlayState::None | ClientOverlayState::GlobalMenu { .. } => None,
        }
    }

    pub(crate) fn handle_add_remote_key(
        &mut self,
        key: crate::input::TerminalKey,
    ) -> AddRemoteFormOutcome {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return AddRemoteFormOutcome::Redraw;
        }

        match key.code {
            KeyCode::Esc => {
                self.close_client_overlay();
                AddRemoteFormOutcome::Redraw
            }
            KeyCode::Tab | KeyCode::Down | KeyCode::Up => {
                if let Some(form) = self.add_remote_form_mut() {
                    form.focused_field = match form.focused_field {
                        AddRemoteField::Target => AddRemoteField::Name,
                        AddRemoteField::Name => AddRemoteField::Target,
                    };
                    form.error = None;
                }
                AddRemoteFormOutcome::Redraw
            }
            KeyCode::Enter => {
                let Some(form) = self.add_remote_form_mut() else {
                    return AddRemoteFormOutcome::Redraw;
                };
                let target = form.target.trim().to_string();
                if target.is_empty() {
                    form.error = Some("target required".to_string());
                    return AddRemoteFormOutcome::Redraw;
                }
                let name = trimmed_optional(&form.name);
                AddRemoteFormOutcome::Submit(AddRemoteDraft {
                    target,
                    name,
                    keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
                })
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(input) = self.add_remote_current_input_mut() {
                    input.clear();
                }
                if let Some(form) = self.add_remote_form_mut() {
                    form.error = None;
                }
                AddRemoteFormOutcome::Redraw
            }
            KeyCode::Backspace => {
                if let Some(input) = self.add_remote_current_input_mut() {
                    input.pop();
                }
                if let Some(form) = self.add_remote_form_mut() {
                    form.error = None;
                }
                AddRemoteFormOutcome::Redraw
            }
            KeyCode::Char(ch) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                if let Some(input) = self.add_remote_current_input_mut() {
                    input.push(ch);
                }
                if let Some(form) = self.add_remote_form_mut() {
                    form.error = None;
                }
                AddRemoteFormOutcome::Redraw
            }
            _ => AddRemoteFormOutcome::Redraw,
        }
    }

    pub(crate) fn append_add_remote_paste(&mut self, text: &str) -> AddRemoteFormOutcome {
        if let Some(input) = self.add_remote_current_input_mut() {
            input.push_str(text);
        }
        if let Some(form) = self.add_remote_form_mut() {
            form.error = None;
        }
        AddRemoteFormOutcome::Redraw
    }

    pub(crate) fn set_add_remote_error(&mut self, error: impl Into<String>) {
        if let Some(form) = self.add_remote_form_mut() {
            form.error = Some(error.into());
        }
    }

    pub(crate) fn finish_add_remote(&mut self) {
        self.close_client_overlay();
    }

    pub(crate) fn choose_new_workspace_destination(
        &mut self,
        server_id: &ServerId,
    ) -> NewWorkspaceRoute {
        let destination_is_visible =
            self.new_workspace_picker_destinations
                .as_ref()
                .is_some_and(|destinations| {
                    destinations
                        .iter()
                        .any(|destination| &destination.server_id == server_id)
                });
        self.new_workspace_picker_destinations = None;
        if !destination_is_visible {
            return NewWorkspaceRoute::Unavailable {
                server_id: server_id.clone(),
                reason: "server unavailable".to_string(),
            };
        }

        let route = self.route_for_specific_server(server_id);
        if matches!(route, NewWorkspaceRoute::CreateOn(_)) {
            self.active_server_id = server_id.clone();
        }
        route
    }

    pub(crate) fn focus_workspace_route(
        &mut self,
        server_id: &ServerId,
        workspace_id: &str,
    ) -> FocusRoute {
        let Some(server) = self.server(server_id) else {
            return FocusRoute::NotFound;
        };

        if server.connection_state != ConnectionState::Connected {
            return FocusRoute::Unavailable {
                server_id: server_id.clone(),
                reason: unavailable_reason(&server.connection_state).to_string(),
            };
        }

        if !server
            .summaries
            .workspaces
            .iter()
            .any(|workspace| workspace.workspace_id == workspace_id)
        {
            return FocusRoute::NotFound;
        }

        self.close_new_workspace_picker();
        self.active_server_id = server_id.clone();
        FocusRoute::Workspace {
            server_id: server_id.clone(),
            workspace_id: workspace_id.to_string(),
        }
    }

    pub(crate) fn focus_agent_route(&mut self, server_id: &ServerId, agent_id: &str) -> FocusRoute {
        let Some(server) = self.server(server_id) else {
            return FocusRoute::NotFound;
        };

        if server.connection_state != ConnectionState::Connected {
            return FocusRoute::Unavailable {
                server_id: server_id.clone(),
                reason: unavailable_reason(&server.connection_state).to_string(),
            };
        }

        if !server
            .summaries
            .agents
            .iter()
            .any(|agent| agent.agent_id == agent_id)
        {
            return FocusRoute::NotFound;
        }

        self.close_new_workspace_picker();
        self.active_server_id = server_id.clone();
        FocusRoute::Agent {
            server_id: server_id.clone(),
            target: agent_id.to_string(),
        }
    }

    fn route_for_specific_server(&self, id: &ServerId) -> NewWorkspaceRoute {
        let Some(server) = self.server(id) else {
            return NewWorkspaceRoute::Unavailable {
                server_id: id.clone(),
                reason: "server unavailable".to_string(),
            };
        };

        if server.connection_state == ConnectionState::Connected {
            return NewWorkspaceRoute::CreateOn(id.clone());
        }

        NewWorkspaceRoute::Unavailable {
            server_id: id.clone(),
            reason: unavailable_reason(&server.connection_state).to_string(),
        }
    }

    fn connected_destinations(&self) -> Vec<ServerDestination> {
        self.servers
            .iter()
            .filter(|server| server.connection_state == ConnectionState::Connected)
            .map(|server| ServerDestination {
                server_id: server.id.clone(),
                display_name: server.display_name.clone(),
            })
            .collect()
    }

    pub(crate) fn workspace_rows(&self) -> Vec<WorkspaceSidebarRow> {
        let all_filter = self.filter == ServerFilter::All;
        self.visible_servers()
            .into_iter()
            .flat_map(|server| workspace_rows_for_server(server, all_filter))
            .collect()
    }

    pub(crate) fn agent_groups(&self) -> Vec<AgentSidebarGroup> {
        let all_filter = self.filter == ServerFilter::All;
        self.visible_servers()
            .into_iter()
            .filter(|server| server.connection_state == ConnectionState::Connected)
            .flat_map(|server| agent_groups_for_server(server, all_filter))
            .collect()
    }

    fn visible_servers(&self) -> Vec<&ManagedServer> {
        match &self.filter {
            ServerFilter::All => self.servers.iter().collect(),
            ServerFilter::Server(id) => self.server(id).into_iter().collect(),
        }
    }

    fn server(&self, id: &ServerId) -> Option<&ManagedServer> {
        self.servers.iter().find(|server| &server.id == id)
    }

    fn server_mut(&mut self, id: &ServerId) -> Option<&mut ManagedServer> {
        self.servers.iter_mut().find(|server| &server.id == id)
    }

    fn reconcile_selected_servers(&mut self) {
        if matches!(&self.filter, ServerFilter::Server(selected) if self.server(selected).is_none())
        {
            self.filter = ServerFilter::All;
        }
        if self.server(&self.active_server_id).is_none() {
            self.active_server_id = ServerId::main();
        }
    }

    fn close_new_workspace_picker(&mut self) {
        self.new_workspace_picker_destinations = None;
    }

    pub(crate) fn close_client_overlay(&mut self) {
        self.client_overlay = ClientOverlayState::None;
    }

    fn add_remote_form_mut(&mut self) -> Option<&mut AddRemoteForm> {
        match &mut self.client_overlay {
            ClientOverlayState::AddRemote(form) => Some(form),
            ClientOverlayState::None | ClientOverlayState::GlobalMenu { .. } => None,
        }
    }

    fn add_remote_current_input_mut(&mut self) -> Option<&mut String> {
        let form = self.add_remote_form_mut()?;
        match form.focused_field {
            AddRemoteField::Target => Some(&mut form.target),
            AddRemoteField::Name => Some(&mut form.name),
        }
    }

    fn reconcile_new_workspace_picker(&mut self) {
        let connected_destinations = self.connected_destinations();
        let Some(destinations) = self.new_workspace_picker_destinations.take() else {
            return;
        };

        let next_destinations: Vec<ServerDestination> = destinations
            .into_iter()
            .filter_map(|existing| {
                connected_destinations
                    .iter()
                    .find(|current| current.server_id == existing.server_id)
                    .cloned()
            })
            .collect();
        if !next_destinations.is_empty() {
            self.new_workspace_picker_destinations = Some(next_destinations);
        }
    }
}

fn workspace_rows_for_server(server: &ManagedServer, all_filter: bool) -> Vec<WorkspaceSidebarRow> {
    if server.connection_state != ConnectionState::Connected
        && server.summaries.workspaces.is_empty()
    {
        return vec![WorkspaceSidebarRow {
            server_id: server.id.clone(),
            workspace_id: None,
            label: unavailable_row_label(server),
            branch: None,
            focused: false,
            disabled: true,
        }];
    }

    if server.role == ServerRole::Secondary && server.summaries.workspaces.is_empty() {
        return vec![WorkspaceSidebarRow {
            server_id: server.id.clone(),
            workspace_id: None,
            label: format!("{} no workspaces", server.display_name),
            branch: None,
            focused: false,
            disabled: true,
        }];
    }

    server
        .summaries
        .workspaces
        .iter()
        .map(|workspace| WorkspaceSidebarRow {
            server_id: server.id.clone(),
            workspace_id: Some(workspace.workspace_id.clone()),
            label: workspace_label(server, &workspace.label, all_filter),
            branch: workspace.branch.clone(),
            focused: workspace.focused,
            disabled: server.connection_state != ConnectionState::Connected,
        })
        .collect()
}

fn agent_groups_for_server(server: &ManagedServer, all_filter: bool) -> Vec<AgentSidebarGroup> {
    server
        .summaries
        .workspaces
        .iter()
        .filter_map(|workspace| {
            let agents: Vec<AgentSidebarRow> = server
                .summaries
                .agents
                .iter()
                .filter(|agent| agent.workspace_id == workspace.workspace_id)
                .map(|agent| AgentSidebarRow {
                    agent_id: agent.agent_id.clone(),
                    label: agent.label.clone(),
                    status: agent.status.clone(),
                    focused: agent.focused,
                })
                .collect();

            (!agents.is_empty()).then(|| AgentSidebarGroup {
                server_id: server.id.clone(),
                workspace_id: workspace.workspace_id.clone(),
                label: workspace_label(server, &workspace.label, all_filter),
                focused: workspace.focused || agents.iter().any(|agent| agent.focused),
                agents,
            })
        })
        .collect()
}

fn workspace_label(server: &ManagedServer, label: &str, all_filter: bool) -> String {
    if all_filter && server.role == ServerRole::Secondary {
        format!("{} {label}", server.display_name)
    } else {
        label.to_string()
    }
}

fn unavailable_row_label(server: &ManagedServer) -> String {
    match server.connection_state {
        ConnectionState::Connecting => format!("{} connecting", server.display_name),
        ConnectionState::Disconnected => format!("{} offline", server.display_name),
        ConnectionState::ProtocolMismatch { .. } => {
            format!("{} protocol mismatch", server.display_name)
        }
        ConnectionState::Connected => server.display_name.clone(),
    }
}

fn unavailable_reason(connection_state: &ConnectionState) -> &'static str {
    match connection_state {
        ConnectionState::Connecting => "server connecting",
        ConnectionState::Connected => "server connected",
        ConnectionState::Disconnected => "server disconnected",
        ConnectionState::ProtocolMismatch { .. } => "protocol mismatch",
    }
}

fn trimmed_optional(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn managed_secondary(
    definition: crate::remote_registry::RemoteDefinitionSnapshot,
    connection_state: ConnectionState,
) -> ManagedServer {
    ManagedServer {
        id: ServerId::secondary(definition.id),
        display_name: definition.name,
        role: ServerRole::Secondary,
        target: definition.target.into(),
        keybindings: definition.keybindings,
        connection_state,
        summaries: ServerSummary::default(),
    }
}

fn connection_target_rank(target: &ServerConnectionTarget) -> u8 {
    match target {
        ServerConnectionTarget::LocalSession(_) => 0,
        ServerConnectionTarget::Ssh(_) => 1,
        ServerConnectionTarget::Main => 2,
    }
}

fn request_remote_list(
    api: &mut impl SupervisorApi,
) -> Result<Vec<crate::remote_registry::RemoteDefinitionSnapshot>, String> {
    let response = api.request(crate::api::schema::Request {
        id: "client-supervisor:remote-list".into(),
        method: crate::api::schema::Method::RemoteList(crate::api::schema::EmptyParams::default()),
    })?;
    match response.result {
        crate::api::schema::ResponseResult::RemoteList { remotes } => Ok(remotes),
        other => Err(format!("remote.list returned unexpected result: {other:?}")),
    }
}

fn request_server_summary(api: &mut impl SupervisorApi) -> Result<ServerSummary, String> {
    let workspaces_response = api.request(crate::api::schema::Request {
        id: "client-supervisor:workspace-list".into(),
        method: crate::api::schema::Method::WorkspaceList(
            crate::api::schema::EmptyParams::default(),
        ),
    })?;
    let workspaces = match workspaces_response.result {
        crate::api::schema::ResponseResult::WorkspaceList { workspaces } => workspaces,
        other => {
            return Err(format!(
                "workspace.list returned unexpected result: {other:?}"
            ))
        }
    };

    let agents_response = api.request(crate::api::schema::Request {
        id: "client-supervisor:agent-list".into(),
        method: crate::api::schema::Method::AgentList(crate::api::schema::EmptyParams::default()),
    })?;
    let agents = match agents_response.result {
        crate::api::schema::ResponseResult::AgentList { agents } => agents,
        other => return Err(format!("agent.list returned unexpected result: {other:?}")),
    };

    Ok(ServerSummary::from_api(workspaces, agents))
}

fn fetch_local_secondary_summary(
    session: Option<String>,
) -> Result<ServerSummary, ConnectionState> {
    fetch_server_summary_from_api_target(crate::api::client::ConnectionTarget::LocalSession(
        session,
    ))
}

pub(crate) fn fetch_server_summary_from_api_target(
    target: crate::api::client::ConnectionTarget,
) -> Result<ServerSummary, ConnectionState> {
    let mut api = crate::api::client::ApiClient::for_target(target);
    let status = request_runtime_status(&mut api).map_err(|_| ConnectionState::Disconnected)?;
    if status.protocol != Some(crate::protocol::PROTOCOL_VERSION) {
        return Err(ConnectionState::ProtocolMismatch {
            server_protocol: status.protocol,
            client_protocol: crate::protocol::PROTOCOL_VERSION,
        });
    }

    request_server_summary(&mut api).map_err(|_| ConnectionState::Disconnected)
}

pub(crate) fn request_runtime_status(
    api: &mut impl SupervisorApi,
) -> Result<crate::api::RuntimeStatus, String> {
    let response = api.request(crate::api::schema::Request {
        id: "client-supervisor:status".into(),
        method: crate::api::schema::Method::Ping(crate::api::schema::PingParams::default()),
    })?;
    match response.result {
        crate::api::schema::ResponseResult::Pong {
            version,
            protocol,
            capabilities,
        } => Ok(crate::api::RuntimeStatus {
            version: Some(version),
            protocol: Some(protocol),
            capabilities,
        }),
        other => Err(format!("ping returned unexpected result: {other:?}")),
    }
}

pub(crate) fn request_ui_settings(
    api: &mut impl SupervisorApi,
) -> Result<crate::api::schema::UiSettingsInfo, String> {
    let response = api.request(crate::api::schema::Request {
        id: "client-supervisor:ui-settings".into(),
        method: crate::api::schema::Method::ServerUiSettings(
            crate::api::schema::EmptyParams::default(),
        ),
    })?;
    match response.result {
        crate::api::schema::ResponseResult::UiSettings { settings } => Ok(settings),
        other => Err(format!(
            "server.ui_settings returned unexpected result: {other:?}"
        )),
    }
}

impl ServerSummary {
    fn from_api(
        workspaces: Vec<crate::api::schema::WorkspaceInfo>,
        agents: Vec<crate::api::schema::AgentInfo>,
    ) -> Self {
        Self {
            workspaces: workspaces
                .into_iter()
                .map(|workspace| WorkspaceSummary {
                    workspace_id: workspace.workspace_id,
                    label: workspace.label,
                    branch: workspace.branch,
                    focused: workspace.focused,
                })
                .collect(),
            agents: agents
                .into_iter()
                .map(|agent| {
                    let label = agent_label(&agent);
                    let status = agent_status_label(agent.agent_status);
                    AgentSummary {
                        agent_id: agent.terminal_id,
                        workspace_id: agent.workspace_id,
                        label,
                        status,
                        focused: agent.focused,
                    }
                })
                .collect(),
        }
    }
}

fn agent_label(agent: &crate::api::schema::AgentInfo) -> String {
    agent
        .name
        .as_ref()
        .or(agent.display_agent.as_ref())
        .or(agent.agent.as_ref())
        .or(agent.title.as_ref())
        .cloned()
        .unwrap_or_else(|| agent.terminal_id.clone())
}

fn agent_status_label(status: crate::api::schema::AgentStatus) -> String {
    match status {
        crate::api::schema::AgentStatus::Idle => "idle",
        crate::api::schema::AgentStatus::Working => "working",
        crate::api::schema::AgentStatus::Blocked => "blocked",
        crate::api::schema::AgentStatus::Done => "done",
        crate::api::schema::AgentStatus::Unknown => "unknown",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh_remote(
        id: &str,
        name: &str,
        target: &str,
    ) -> crate::remote_registry::RemoteDefinitionSnapshot {
        crate::remote_registry::RemoteDefinitionSnapshot {
            id: id.into(),
            name: name.into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Ssh {
                target: target.into(),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
        }
    }

    fn local_remote(
        id: &str,
        name: &str,
        session: Option<&str>,
    ) -> crate::remote_registry::RemoteDefinitionSnapshot {
        crate::remote_registry::RemoteDefinitionSnapshot {
            id: id.into(),
            name: name.into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: session.map(str::to_string),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Server,
        }
    }

    fn workspace_info(
        workspace_id: &str,
        label: &str,
        focused: bool,
    ) -> crate::api::schema::WorkspaceInfo {
        crate::api::schema::WorkspaceInfo {
            workspace_id: workspace_id.into(),
            number: 1,
            label: label.into(),
            branch: None,
            focused,
            pane_count: 1,
            tab_count: 1,
            active_tab_id: "tab-1".into(),
            agent_status: crate::api::schema::AgentStatus::Idle,
            worktree: None,
        }
    }

    fn agent_info(
        terminal_id: &str,
        workspace_id: &str,
        label: &str,
        status: crate::api::schema::AgentStatus,
        focused: bool,
    ) -> crate::api::schema::AgentInfo {
        crate::api::schema::AgentInfo {
            terminal_id: terminal_id.into(),
            name: Some(label.into()),
            agent: None,
            title: None,
            display_agent: None,
            agent_status: status,
            custom_status: None,
            state_labels: std::collections::HashMap::new(),
            agent_session: None,
            workspace_id: workspace_id.into(),
            tab_id: "tab-1".into(),
            pane_id: "pane-1".into(),
            focused,
            cwd: None,
            revision: 1,
        }
    }

    #[derive(Default)]
    struct FakeSupervisorApi {
        requests: Vec<&'static str>,
        remotes: Vec<crate::remote_registry::RemoteDefinitionSnapshot>,
        workspaces: Vec<crate::api::schema::WorkspaceInfo>,
        agents: Vec<crate::api::schema::AgentInfo>,
        ui_settings: crate::api::schema::UiSettingsInfo,
        fail_ui_settings: bool,
    }

    impl SupervisorApi for FakeSupervisorApi {
        fn request(
            &mut self,
            request: crate::api::schema::Request,
        ) -> Result<crate::api::schema::SuccessResponse, String> {
            let result = match request.method {
                crate::api::schema::Method::RemoteList(_) => {
                    self.requests.push("remote.list");
                    crate::api::schema::ResponseResult::RemoteList {
                        remotes: self.remotes.clone(),
                    }
                }
                crate::api::schema::Method::WorkspaceList(_) => {
                    self.requests.push("workspace.list");
                    crate::api::schema::ResponseResult::WorkspaceList {
                        workspaces: self.workspaces.clone(),
                    }
                }
                crate::api::schema::Method::AgentList(_) => {
                    self.requests.push("agent.list");
                    crate::api::schema::ResponseResult::AgentList {
                        agents: self.agents.clone(),
                    }
                }
                crate::api::schema::Method::ServerUiSettings(_) => {
                    self.requests.push("server.ui_settings");
                    if self.fail_ui_settings {
                        return Err("settings unavailable".into());
                    }
                    crate::api::schema::ResponseResult::UiSettings {
                        settings: self.ui_settings.clone(),
                    }
                }
                other => return Err(format!("unexpected method: {other:?}")),
            };

            Ok(crate::api::schema::SuccessResponse {
                id: request.id,
                result,
            })
        }
    }

    #[test]
    fn bootstrap_from_main_api_fetches_registry_summary_and_connection_plans() {
        let mut api = FakeSupervisorApi {
            remotes: vec![
                ssh_remote("remote-ssh", "prod", "prod.example.com"),
                local_remote("remote-dev", "dev", Some("dev")),
            ],
            workspaces: vec![workspace_info("main-workspace", "herdr", true)],
            agents: vec![agent_info(
                "terminal-1",
                "main-workspace",
                "claude",
                crate::api::schema::AgentStatus::Working,
                true,
            )],
            ..FakeSupervisorApi::default()
        };

        let model = bootstrap_from_main_api(&mut api, "local").unwrap();

        assert_eq!(
            api.requests,
            vec![
                "remote.list",
                "workspace.list",
                "agent.list",
                "server.ui_settings"
            ]
        );
        assert_eq!(
            model.workspace_rows(),
            vec![
                WorkspaceSidebarRow {
                    server_id: ServerId::main(),
                    workspace_id: Some("main-workspace".into()),
                    label: "herdr".into(),
                    branch: None,
                    focused: true,
                    disabled: false,
                },
                WorkspaceSidebarRow {
                    server_id: ServerId::secondary("remote-ssh"),
                    workspace_id: None,
                    label: "prod connecting".into(),
                    branch: None,
                    focused: false,
                    disabled: true,
                },
                WorkspaceSidebarRow {
                    server_id: ServerId::secondary("remote-dev"),
                    workspace_id: None,
                    label: "dev connecting".into(),
                    branch: None,
                    focused: false,
                    disabled: true,
                },
            ]
        );
        assert_eq!(
            model.agent_groups(),
            vec![AgentSidebarGroup {
                server_id: ServerId::main(),
                workspace_id: "main-workspace".into(),
                label: "herdr".into(),
                focused: true,
                agents: vec![AgentSidebarRow {
                    agent_id: "terminal-1".into(),
                    label: "claude".into(),
                    status: "working".into(),
                    focused: true,
                }],
            }]
        );
        assert_eq!(
            model.secondary_connection_plans(),
            vec![
                SecondaryConnectionPlan {
                    server_id: ServerId::secondary("remote-dev"),
                    display_name: "dev".into(),
                    target: ServerConnectionTarget::LocalSession(Some("dev".into())),
                    keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Server,
                },
                SecondaryConnectionPlan {
                    server_id: ServerId::secondary("remote-ssh"),
                    display_name: "prod".into(),
                    target: ServerConnectionTarget::Ssh("prod.example.com".into()),
                    keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
                },
            ]
        );
    }

    #[test]
    fn bootstrap_from_main_api_stores_main_ui_settings_snapshot() {
        let mut ui_settings = crate::api::schema::UiSettingsInfo {
            sidebar_width: 33,
            ..crate::api::schema::UiSettingsInfo::default()
        };
        crate::app::state::SidebarSpaceItem::Branch
            .set_enabled(&mut ui_settings.sidebar_spaces, false);
        let mut api = FakeSupervisorApi {
            workspaces: vec![workspace_info("main-workspace", "herdr", true)],
            ui_settings: ui_settings.clone(),
            ..FakeSupervisorApi::default()
        };

        let model = bootstrap_from_main_api(&mut api, "local").unwrap();

        assert_eq!(model.ui_settings(), &ui_settings);
        assert_eq!(
            api.requests,
            vec![
                "remote.list",
                "workspace.list",
                "agent.list",
                "server.ui_settings"
            ]
        );
    }

    #[test]
    fn bootstrap_from_main_api_keeps_default_ui_settings_when_snapshot_fails() {
        let mut api = FakeSupervisorApi {
            workspaces: vec![workspace_info("main-workspace", "herdr", true)],
            fail_ui_settings: true,
            ..FakeSupervisorApi::default()
        };

        let model = bootstrap_from_main_api(&mut api, "local").unwrap();

        assert_eq!(
            model.ui_settings(),
            &crate::api::schema::UiSettingsInfo::default()
        );
        assert_eq!(
            api.requests,
            vec![
                "remote.list",
                "workspace.list",
                "agent.list",
                "server.ui_settings"
            ]
        );
    }

    #[test]
    fn refresh_main_summary_from_api_replaces_main_summary_only() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(ssh_remote("remote-x", "x", "x"));
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
        let mut api = FakeSupervisorApi {
            workspaces: vec![workspace_info("main-updated", "herdr", true)],
            agents: vec![agent_info(
                "main-agent",
                "main-updated",
                "claude",
                crate::api::schema::AgentStatus::Idle,
                true,
            )],
            ..FakeSupervisorApi::default()
        };

        model.refresh_main_summary_from_api(&mut api).unwrap();

        assert_eq!(api.requests, vec!["workspace.list", "agent.list"]);
        assert_eq!(
            model.workspace_rows(),
            vec![
                WorkspaceSidebarRow {
                    server_id: ServerId::main(),
                    workspace_id: Some("main-updated".into()),
                    label: "herdr".into(),
                    branch: None,
                    focused: true,
                    disabled: false,
                },
                WorkspaceSidebarRow {
                    server_id: remote_id,
                    workspace_id: Some("remote-api".into()),
                    label: "x api".into(),
                    branch: None,
                    focused: false,
                    disabled: false,
                },
            ]
        );
    }

    #[test]
    fn refresh_secondary_summaries_visits_local_first_and_marks_per_server_state() {
        let mut model = ClientSupervisorModel::new("local");
        model.sync_remote_registry(vec![
            ssh_remote("remote-ssh", "prod", "prod.example.com"),
            local_remote("remote-dev", "dev", Some("dev")),
        ]);
        let mut visited = Vec::new();

        model.refresh_secondary_summaries(|plan| {
            visited.push(plan.target.clone());
            match &plan.target {
                ServerConnectionTarget::LocalSession(_) => Ok(ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "dev-workspace".into(),
                        label: "api".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: Vec::new(),
                }),
                ServerConnectionTarget::Ssh(_) => Err(ConnectionState::ProtocolMismatch {
                    server_protocol: Some(10),
                    client_protocol: crate::protocol::PROTOCOL_VERSION,
                }),
                ServerConnectionTarget::Main => unreachable!("secondary plans never include main"),
            }
        });

        assert_eq!(
            visited,
            vec![
                ServerConnectionTarget::LocalSession(Some("dev".into())),
                ServerConnectionTarget::Ssh("prod.example.com".into()),
            ]
        );
        assert_eq!(
            model.workspace_rows(),
            vec![
                WorkspaceSidebarRow {
                    server_id: ServerId::secondary("remote-ssh"),
                    workspace_id: None,
                    label: "prod protocol mismatch".into(),
                    branch: None,
                    focused: false,
                    disabled: true,
                },
                WorkspaceSidebarRow {
                    server_id: ServerId::secondary("remote-dev"),
                    workspace_id: Some("dev-workspace".into()),
                    label: "dev api".into(),
                    branch: None,
                    focused: false,
                    disabled: false,
                },
            ]
        );
    }

    #[test]
    fn server_filter_cycles_all_main_then_secondary_registry_order() {
        let mut model = ClientSupervisorModel::new("local");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model.add_secondary(ssh_remote("remote-y", "y", "y"));

        assert_eq!(model.filter(), &ServerFilter::All);
        assert_eq!(model.filter_label(), "all");

        model.cycle_filter();
        assert_eq!(model.filter(), &ServerFilter::Server(ServerId::main()));
        assert_eq!(model.filter_label(), "local");

        model.cycle_filter();
        assert_eq!(
            model.filter(),
            &ServerFilter::Server(ServerId::secondary("remote-x"))
        );
        assert_eq!(model.filter_label(), "x");

        model.cycle_filter();
        assert_eq!(
            model.filter(),
            &ServerFilter::Server(ServerId::secondary("remote-y"))
        );
        assert_eq!(model.filter_label(), "y");

        model.cycle_filter();
        assert_eq!(model.filter(), &ServerFilter::All);
        assert_eq!(model.filter_label(), "all");
    }

    #[test]
    fn removing_selected_remote_falls_back_to_all_and_main_active_server() {
        let mut model = ClientSupervisorModel::new("local");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        let remote_id = ServerId::secondary("remote-x");
        model.set_filter(ServerFilter::Server(remote_id.clone()));
        model.set_active_server(remote_id.clone()).unwrap();

        let removed = model.remove_secondary(&remote_id);

        assert!(removed);
        assert_eq!(model.filter(), &ServerFilter::All);
        assert_eq!(model.active_server_id(), &ServerId::main());
    }

    #[test]
    fn new_workspace_route_uses_filter_or_picker() {
        let mut model = ClientSupervisorModel::new("local");

        assert_eq!(
            model.new_workspace_route(),
            NewWorkspaceRoute::CreateOn(ServerId::main())
        );

        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        assert_eq!(
            model.new_workspace_route(),
            NewWorkspaceRoute::PickDestination(vec![
                ServerDestination {
                    server_id: ServerId::main(),
                    display_name: "local".into(),
                },
                ServerDestination {
                    server_id: ServerId::secondary("remote-x"),
                    display_name: "x".into(),
                },
            ])
        );

        model.set_filter(ServerFilter::Server(ServerId::secondary("remote-x")));
        assert_eq!(
            model.new_workspace_route(),
            NewWorkspaceRoute::CreateOn(ServerId::secondary("remote-x"))
        );
    }

    #[test]
    fn new_workspace_route_builds_focused_create_request_for_single_destination() {
        let mut model = ClientSupervisorModel::new("local");
        model.set_filter(ServerFilter::Server(ServerId::main()));
        let route = model.new_workspace_route();

        assert_eq!(
            route.api_request("client:workspace-create"),
            Some((
                ServerId::main(),
                crate::api::schema::Request {
                    id: "client:workspace-create".into(),
                    method: crate::api::schema::Method::WorkspaceCreate(
                        crate::api::schema::WorkspaceCreateParams {
                            cwd: None,
                            focus: true,
                            label: None,
                        },
                    ),
                }
            ))
        );
    }

    #[test]
    fn new_workspace_route_waits_for_picker_when_multiple_destinations_exist() {
        let mut model = ClientSupervisorModel::new("local");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        let route = model.new_workspace_route();

        assert_eq!(route.api_request("client:workspace-create"), None);
    }

    #[test]
    fn opening_new_workspace_picker_tracks_connected_destinations() {
        let mut model = ClientSupervisorModel::new("local");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        let expected = vec![
            ServerDestination {
                server_id: ServerId::main(),
                display_name: "local".into(),
            },
            ServerDestination {
                server_id: ServerId::secondary("remote-x"),
                display_name: "x".into(),
            },
        ];

        let route = model.open_new_workspace_picker();

        assert_eq!(route, NewWorkspaceRoute::PickDestination(expected.clone()));
        assert_eq!(
            model.new_workspace_picker_destinations(),
            Some(expected.as_slice())
        );
    }

    #[test]
    fn open_new_workspace_picker_keeps_single_remaining_destination() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model.open_new_workspace_picker();

        model
            .set_connection_state(&remote_id, ConnectionState::Disconnected)
            .unwrap();

        let expected = vec![ServerDestination {
            server_id: ServerId::main(),
            display_name: "local".into(),
        }];
        assert_eq!(
            model.new_workspace_picker_destinations(),
            Some(expected.as_slice())
        );
    }

    #[test]
    fn secondary_server_id_cannot_collide_with_main_server_id() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(local_remote("main", "remote main", Some("main")));

        assert_ne!(remote_id, ServerId::main());
        assert_eq!(
            model
                .servers
                .iter()
                .filter(|server| server.id == ServerId::main())
                .count(),
            1
        );
        assert_eq!(
            model.server(&remote_id).map(|server| server.role),
            Some(ServerRole::Secondary)
        );
    }

    #[test]
    fn choosing_new_workspace_destination_routes_create_and_switches_active_server() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model.open_new_workspace_picker();

        let route = model.choose_new_workspace_destination(&remote_id);

        assert_eq!(route, NewWorkspaceRoute::CreateOn(remote_id.clone()));
        assert_eq!(model.active_server_id(), &remote_id);
        assert_eq!(model.new_workspace_picker_destinations(), None);
        assert_eq!(
            route.api_request("client:workspace-create"),
            Some((
                remote_id,
                crate::api::schema::Request {
                    id: "client:workspace-create".into(),
                    method: crate::api::schema::Method::WorkspaceCreate(
                        crate::api::schema::WorkspaceCreateParams {
                            cwd: None,
                            focus: true,
                            label: None,
                        },
                    ),
                },
            ))
        );
    }

    #[test]
    fn disconnected_filtered_server_does_not_route_new_workspace_elsewhere() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model
            .set_connection_state(&remote_id, ConnectionState::Disconnected)
            .unwrap();
        model.set_filter(ServerFilter::Server(remote_id));

        assert_eq!(
            model.new_workspace_route(),
            NewWorkspaceRoute::Unavailable {
                server_id: ServerId::secondary("remote-x"),
                reason: "server disconnected".into(),
            }
        );
    }

    #[test]
    fn unavailable_filtered_server_reports_specific_connection_state() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model.set_filter(ServerFilter::Server(remote_id.clone()));

        model
            .set_connection_state(&remote_id, ConnectionState::Connecting)
            .unwrap();
        assert_eq!(
            model.new_workspace_route(),
            NewWorkspaceRoute::Unavailable {
                server_id: remote_id.clone(),
                reason: "server connecting".into(),
            }
        );

        model
            .set_connection_state(
                &remote_id,
                ConnectionState::ProtocolMismatch {
                    server_protocol: Some(10),
                    client_protocol: 11,
                },
            )
            .unwrap();
        assert_eq!(
            model.new_workspace_route(),
            NewWorkspaceRoute::Unavailable {
                server_id: remote_id,
                reason: "protocol mismatch".into(),
            }
        );
    }

    #[test]
    fn active_remote_falls_back_to_main_when_connection_becomes_unavailable() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model.set_active_server(remote_id.clone()).unwrap();

        model
            .set_connection_state(&remote_id, ConnectionState::Disconnected)
            .unwrap();

        assert_eq!(model.active_server_id(), &ServerId::main());
    }

    #[test]
    fn workspace_rows_prefix_secondary_labels_only_in_all_filter() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
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
                        workspace_id: "remote-herdr".into(),
                        label: "herdr".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();

        assert_eq!(
            model.workspace_rows(),
            vec![
                WorkspaceSidebarRow {
                    server_id: ServerId::main(),
                    workspace_id: Some("main-herdr".into()),
                    label: "herdr".into(),
                    branch: None,
                    focused: true,
                    disabled: false,
                },
                WorkspaceSidebarRow {
                    server_id: remote_id.clone(),
                    workspace_id: Some("remote-herdr".into()),
                    label: "x herdr".into(),
                    branch: None,
                    focused: false,
                    disabled: false,
                },
            ]
        );

        model.set_filter(ServerFilter::Server(remote_id.clone()));
        assert_eq!(
            model.workspace_rows(),
            vec![WorkspaceSidebarRow {
                server_id: remote_id,
                workspace_id: Some("remote-herdr".into()),
                label: "herdr".into(),
                branch: None,
                focused: false,
                disabled: false,
            }]
        );
    }

    #[test]
    fn offline_remote_without_summary_renders_disabled_workspace_row() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model
            .set_connection_state(&remote_id, ConnectionState::Disconnected)
            .unwrap();

        assert_eq!(
            model.workspace_rows(),
            vec![WorkspaceSidebarRow {
                server_id: remote_id.clone(),
                workspace_id: None,
                label: "x offline".into(),
                branch: None,
                focused: false,
                disabled: true,
            }]
        );

        model.set_filter(ServerFilter::Server(remote_id.clone()));
        assert_eq!(
            model.workspace_rows(),
            vec![WorkspaceSidebarRow {
                server_id: remote_id,
                workspace_id: None,
                label: "x offline".into(),
                branch: None,
                focused: false,
                disabled: true,
            }]
        );
    }

    #[test]
    fn connected_empty_remote_renders_empty_workspace_row() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model
            .set_summary(&remote_id, ServerSummary::default())
            .unwrap();

        assert_eq!(
            model.workspace_rows(),
            vec![WorkspaceSidebarRow {
                server_id: remote_id,
                workspace_id: None,
                label: "x no workspaces".into(),
                branch: None,
                focused: false,
                disabled: true,
            }]
        );
    }

    #[test]
    fn agent_groups_prefix_secondary_workspace_labels_in_all_filter() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model
            .set_summary(
                &ServerId::main(),
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "main-herdr".into(),
                        label: "herdr".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: vec![AgentSummary {
                        agent_id: "main-agent".into(),
                        workspace_id: "main-herdr".into(),
                        label: "claude".into(),
                        status: "idle".into(),
                        focused: false,
                    }],
                },
            )
            .unwrap();
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-herdr".into(),
                        label: "herdr".into(),
                        branch: None,
                        focused: true,
                    }],
                    agents: vec![AgentSummary {
                        agent_id: "remote-agent".into(),
                        workspace_id: "remote-herdr".into(),
                        label: "claude".into(),
                        status: "idle".into(),
                        focused: true,
                    }],
                },
            )
            .unwrap();

        assert_eq!(
            model.agent_groups(),
            vec![
                AgentSidebarGroup {
                    server_id: ServerId::main(),
                    workspace_id: "main-herdr".into(),
                    label: "herdr".into(),
                    focused: false,
                    agents: vec![AgentSidebarRow {
                        agent_id: "main-agent".into(),
                        label: "claude".into(),
                        status: "idle".into(),
                        focused: false,
                    }],
                },
                AgentSidebarGroup {
                    server_id: remote_id.clone(),
                    workspace_id: "remote-herdr".into(),
                    label: "x herdr".into(),
                    focused: true,
                    agents: vec![AgentSidebarRow {
                        agent_id: "remote-agent".into(),
                        label: "claude".into(),
                        status: "idle".into(),
                        focused: true,
                    }],
                },
            ]
        );

        model.set_filter(ServerFilter::Server(remote_id.clone()));
        assert_eq!(
            model.agent_groups(),
            vec![AgentSidebarGroup {
                server_id: remote_id,
                workspace_id: "remote-herdr".into(),
                label: "herdr".into(),
                focused: true,
                agents: vec![AgentSidebarRow {
                    agent_id: "remote-agent".into(),
                    label: "claude".into(),
                    status: "idle".into(),
                    focused: true,
                }],
            }]
        );
    }

    #[test]
    fn focus_workspace_route_switches_active_server_for_connected_owner() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
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

        let route = model.focus_workspace_route(&remote_id, "remote-api");

        assert_eq!(
            route,
            FocusRoute::Workspace {
                server_id: remote_id.clone(),
                workspace_id: "remote-api".into(),
            }
        );
        assert_eq!(model.active_server_id(), &remote_id);
        assert_eq!(
            route.api_request("client:workspace-focus"),
            Some(crate::api::schema::Request {
                id: "client:workspace-focus".into(),
                method: crate::api::schema::Method::WorkspaceFocus(
                    crate::api::schema::WorkspaceTarget {
                        workspace_id: "remote-api".into(),
                    },
                ),
            })
        );
    }

    #[test]
    fn focus_agent_route_switches_active_server_for_connected_owner() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
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

        let route = model.focus_agent_route(&remote_id, "remote-agent");

        assert_eq!(
            route,
            FocusRoute::Agent {
                server_id: remote_id.clone(),
                target: "remote-agent".into(),
            }
        );
        assert_eq!(model.active_server_id(), &remote_id);
        assert_eq!(
            route.api_request("client:agent-focus"),
            Some(crate::api::schema::Request {
                id: "client:agent-focus".into(),
                method: crate::api::schema::Method::AgentFocus(crate::api::schema::AgentTarget {
                    target: "remote-agent".into(),
                },),
            })
        );
    }

    #[test]
    fn focus_route_rejects_disconnected_owner_without_fallback() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model
            .set_connection_state(&remote_id, ConnectionState::Disconnected)
            .unwrap();

        let route = model.focus_workspace_route(&remote_id, "remote-api");

        assert_eq!(
            route,
            FocusRoute::Unavailable {
                server_id: remote_id,
                reason: "server disconnected".into(),
            }
        );
        assert_eq!(model.active_server_id(), &ServerId::main());
        assert_eq!(route.api_request("client:workspace-focus"), None);
    }

    #[test]
    fn focus_route_does_not_send_unknown_rows() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));

        let route = model.focus_agent_route(&remote_id, "missing-agent");

        assert_eq!(route, FocusRoute::NotFound);
        assert_eq!(model.active_server_id(), &ServerId::main());
        assert_eq!(route.api_request("client:agent-focus"), None);
    }

    #[test]
    fn client_global_menu_uses_server_launcher_items() {
        let mut model = ClientSupervisorModel::new("local");

        model.open_client_global_menu();

        assert_eq!(model.client_global_menu_highlighted(), Some(0));
        assert_eq!(
            model.client_global_menu_items(),
            [
                "settings",
                "keybinds",
                "reload config",
                "detach",
                "add remote"
            ]
        );
        for _ in 0..4 {
            model.move_client_global_menu_next();
        }
        assert_eq!(model.client_global_menu_highlighted(), Some(4));
        assert_eq!(
            model.accept_client_global_menu_item(),
            Some(ClientGlobalMenuAction::AddRemote)
        );
        assert_eq!(
            model.add_remote_form(),
            Some(&AddRemoteForm {
                target: String::new(),
                name: String::new(),
                focused_field: AddRemoteField::Target,
                error: None,
            })
        );
    }

    #[test]
    fn add_remote_form_edits_fields_and_builds_draft() {
        let mut model = ClientSupervisorModel::new("local");
        model.open_add_remote_form();

        for ch in "local:dev".chars() {
            assert_eq!(
                model.handle_add_remote_key(crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Char(ch),
                    crossterm::event::KeyModifiers::empty(),
                )),
                AddRemoteFormOutcome::Redraw
            );
        }
        assert_eq!(
            model.handle_add_remote_key(crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Tab,
                crossterm::event::KeyModifiers::empty(),
            )),
            AddRemoteFormOutcome::Redraw
        );
        for ch in "dev".chars() {
            assert_eq!(
                model.handle_add_remote_key(crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Char(ch),
                    crossterm::event::KeyModifiers::empty(),
                )),
                AddRemoteFormOutcome::Redraw
            );
        }

        assert_eq!(
            model.handle_add_remote_key(crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            )),
            AddRemoteFormOutcome::Submit(AddRemoteDraft {
                target: "local:dev".into(),
                name: Some("dev".into()),
                keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            })
        );
    }

    #[test]
    fn summary_subscription_plans_include_connected_main_and_local_secondaries_only() {
        let mut model = ClientSupervisorModel::new("local");
        let dev_id = model.add_secondary(local_remote("remote-dev", "dev", Some("dev")));
        let ssh_id = model.add_secondary(ssh_remote("remote-ssh", "prod", "prod.example.com"));
        model
            .set_connection_state(&ssh_id, ConnectionState::Connecting)
            .unwrap();

        assert_eq!(
            model.summary_subscription_plans(),
            vec![
                SummarySubscriptionPlan {
                    server_id: ServerId::main(),
                    target: ServerConnectionTarget::Main,
                },
                SummarySubscriptionPlan {
                    server_id: dev_id,
                    target: ServerConnectionTarget::LocalSession(Some("dev".into())),
                },
            ]
        );
    }
}
