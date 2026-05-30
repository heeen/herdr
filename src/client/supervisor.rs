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

    /// item 3 (Area 5): the registry `remote_id` for a secondary server (the part after the
    /// `secondary:` prefix); for the main server it is the raw inner string. Used to address the
    /// off-thread `remote.set_enabled`/`remote.remove` request against the registry.
    pub(crate) fn registry_id(&self) -> &str {
        self.0.strip_prefix("secondary:").unwrap_or(&self.0)
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

impl ServerConnectionTarget {
    /// item 3 (Area 5): the short target string shown in the management overlay row.
    pub(crate) fn display_label(&self) -> String {
        match self {
            ServerConnectionTarget::Main => "local".to_string(),
            ServerConnectionTarget::LocalSession(session) => {
                format!("local:{}", session.as_deref().unwrap_or("default"))
            }
            ServerConnectionTarget::Ssh(target) => target.clone(),
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
    pub(crate) disabled: bool, // item 3 (serde-driven via registry; default false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServerDestination {
    pub(crate) server_id: ServerId,
    pub(crate) display_name: String,
}

/// item 1 (Area 3 / Decision 4): the new-workspace destination picker state. Carries the
/// connected destinations AND the current keyboard/mouse selection so the composited modal
/// can render a highlighted row and arrow-key navigate. Replaces the bare
/// `Option<Vec<ServerDestination>>` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewWorkspacePickerState {
    pub(crate) destinations: Vec<ServerDestination>,
    pub(crate) selected: usize,
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
    ManageRemotes, // item 3 (Area 5)
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
    ManageRemotes(RemoteManageOverlay), // item 3 (Area 5)
}

/// item 3 (Area 5): remote-management overlay state. Inert in C0; item 3 fills behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteManageOverlay {
    pub selected: usize,
    pub scroll: usize,
    pub confirm_delete: Option<String>, // remote_id
    pub pending: Option<String>,        // in-flight toggle/delete remote_id
}

/// item 6 (Area 6): optimistic focus target. Inert in C0; item 6 sets/clears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OptimisticFocusTarget {
    Workspace(String),
    Agent(String),
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
    pub(crate) is_remote: bool, // item 4: server.role == ServerRole::Secondary
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentSidebarRow {
    pub(crate) agent_id: String,
    pub(crate) label: String,
    pub(crate) status: String,
    pub(crate) focused: bool,
}

/// item 3 (Area 5): the overlay-state of a remote in the management overlay, analogous to
/// `ConnectionState` but flattened for display. `Disabled` (the gate) wins over the connection
/// state, mirroring `host_banner_state`. The compositor maps this to the ui-owned
/// `RemoteStateGlyph` (the layering rule keeps `client::supervisor` types out of `ui`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteManageState {
    Connected,
    Connecting,
    Disconnected,
    Disabled,
    ProtocolMismatch,
}

impl RemoteManageState {
    /// The short state word rendered in the overlay row.
    pub(crate) fn state_word(self) -> &'static str {
        match self {
            RemoteManageState::Connected => "connected",
            RemoteManageState::Connecting => "connecting",
            RemoteManageState::Disconnected => "offline",
            RemoteManageState::Disabled => "disabled",
            RemoteManageState::ProtocolMismatch => "protocol mismatch",
        }
    }
}

/// item 3 (Area 5): a pure view row for the remote-management overlay, one per `Secondary` host.
/// `remote_id` is the registry id (NOT the supervisor `ServerId`) so the off-thread
/// `remote.set_enabled`/`remote.remove` request targets the registry directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteManageRow {
    pub(crate) remote_id: String,
    pub(crate) name: String,
    pub(crate) target: String,
    pub(crate) enabled: bool,
    pub(crate) state: RemoteManageState,
}

/// item 3 (Area 5): the typed outcome of a key press in the management overlay. The client loop
/// consumes it and spawns the off-thread main-server API request (toggle / delete).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteManageOutcome {
    Redraw,
    OpenAddRemote,
    SetEnabled { remote_id: String, enabled: bool },
    Delete { remote_id: String },
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
    new_workspace_picker: Option<NewWorkspacePickerState>, // item 1 (was new_workspace_picker_destinations)
    client_overlay: ClientOverlayState,
    optimistic_focus: Option<(ServerId, OptimisticFocusTarget)>, // item 6 (always None in C0)
}

const SUPERVISOR_API_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

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
                disabled: false,
            }],
            filter: ServerFilter::All,
            active_server_id: ServerId::main(),
            ui_settings: crate::api::schema::UiSettingsInfo::default(),
            new_workspace_picker: None,
            client_overlay: ClientOverlayState::None,
            optimistic_focus: None,
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
            // item 3 (Area 5): re-apply the gate input on every sync (like display_name/target/
            // keybindings). NOTE: connection_state is intentionally NOT re-applied here, so a
            // re-enabled remote keeps its prior state — the toggle-success handler explicitly
            // sets it to `Connecting` so the now-ungated plans pick it back up.
            server.disabled = definition.disabled;
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
        // item 6 (Area 6): authoritative summary wins — clear the optimistic focus override
        // for this server so the highlight reconciles to the freshly applied truth.
        self.clear_optimistic_focus_for_server(id);
        Ok(())
    }

    pub(crate) fn secondary_connection_plans(&self) -> Vec<SecondaryConnectionPlan> {
        let mut plans: Vec<SecondaryConnectionPlan> = self
            .servers
            .iter()
            .filter(|server| server.role == ServerRole::Secondary)
            // item 3 (Area 5): a disabled remote is inert — emit no connection plan.
            .filter(|server| !server.disabled)
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
            // item 3 (Area 5): even a stale-connected disabled remote stops being polled.
            .filter(|server| !server.disabled)
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
            // item 3 (Area 5): a disabled remote is not a reconnect candidate.
            .filter(|server| !server.disabled)
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
            // item 3 (Area 5): a disabled remote needs no client stream.
            .filter(|server| !server.disabled)
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
                    // item 6 (Area 6): authoritative summary arrived for this server — drop the
                    // optimistic focus override. `Err(_)` results carry no fresh truth and do
                    // NOT clear (the highlight stays optimistic until truth or failure arrives).
                    self.clear_optimistic_focus_for_server(&server_id);
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

    /// item 6 (Area 6): clear the optimistic focus override on a focus-request failure so the
    /// highlight reconciles back to summary truth on the next refresh.
    pub(crate) fn clear_optimistic_focus_on_failure(&mut self, server_id: &ServerId) {
        self.clear_optimistic_focus_for_server(server_id);
    }

    fn clear_optimistic_focus_for_server(&mut self, server_id: &ServerId) {
        if matches!(&self.optimistic_focus, Some((id, _)) if id == server_id) {
            self.optimistic_focus = None;
        }
    }

    pub(crate) fn refresh_main_summary_from_api(
        &mut self,
        api: &mut impl SupervisorApi,
    ) -> Result<(), String> {
        let summary = request_server_summary(api)?;
        self.set_summary(&ServerId::main(), summary)
            .map_err(|()| "main server is missing from supervisor model".to_string())
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

    pub(crate) fn new_workspace_picker(&self) -> Option<&NewWorkspacePickerState> {
        self.new_workspace_picker.as_ref()
    }

    pub(crate) fn new_workspace_picker_destinations(&self) -> Option<&[ServerDestination]> {
        self.new_workspace_picker
            .as_ref()
            .map(|picker| picker.destinations.as_slice())
    }

    pub(crate) fn open_new_workspace_picker(&mut self) -> NewWorkspaceRoute {
        self.close_client_overlay();
        let route = self.new_workspace_route();
        match &route {
            NewWorkspaceRoute::CreateOn(server_id) => {
                self.new_workspace_picker = None;
                self.active_server_id = server_id.clone();
            }
            NewWorkspaceRoute::PickDestination(destinations) => {
                self.new_workspace_picker = Some(NewWorkspacePickerState {
                    destinations: destinations.clone(),
                    selected: 0,
                });
            }
            NewWorkspaceRoute::Unavailable { .. } => {
                self.new_workspace_picker = None;
            }
        }
        route
    }

    /// item 1: advance the picker highlight, saturating at the last destination.
    pub(crate) fn move_new_workspace_picker_next(&mut self) {
        if let Some(picker) = self.new_workspace_picker.as_mut() {
            let last = picker.destinations.len().saturating_sub(1);
            picker.selected = (picker.selected + 1).min(last);
        }
    }

    /// item 1: retreat the picker highlight, saturating at the first destination.
    pub(crate) fn move_new_workspace_picker_prev(&mut self) {
        if let Some(picker) = self.new_workspace_picker.as_mut() {
            picker.selected = picker.selected.saturating_sub(1);
        }
    }

    /// item 1: resolve the highlighted destination into a create route (clamping the selection
    /// defensively), routing through the same `choose_new_workspace_destination` the mouse path
    /// uses. Returns `Unavailable` when the picker is not open.
    pub(crate) fn accept_new_workspace_picker(&mut self) -> NewWorkspaceRoute {
        let Some(picker) = self.new_workspace_picker.as_ref() else {
            return NewWorkspaceRoute::Unavailable {
                server_id: self.active_server_id.clone(),
                reason: "no destination picker open".to_string(),
            };
        };
        let index = picker
            .selected
            .min(picker.destinations.len().saturating_sub(1));
        let Some(destination) = picker.destinations.get(index) else {
            return NewWorkspaceRoute::Unavailable {
                server_id: self.active_server_id.clone(),
                reason: "no destination available".to_string(),
            };
        };
        let server_id = destination.server_id.clone();
        self.choose_new_workspace_destination(&server_id)
    }

    pub(crate) fn client_global_menu_items(&self) -> Vec<&'static str> {
        vec![
            "settings",
            "keybinds",
            "reload config",
            "detach",
            "add remote",
            "manage remotes",
        ]
    }

    pub(crate) fn client_global_menu_highlighted(&self) -> Option<usize> {
        match self.client_overlay {
            ClientOverlayState::GlobalMenu { highlighted } => Some(highlighted),
            ClientOverlayState::None
            | ClientOverlayState::AddRemote(_)
            | ClientOverlayState::ManageRemotes(_) => None,
        }
    }

    pub(crate) fn open_client_global_menu(&mut self) {
        self.new_workspace_picker = None;
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

    /// item 7: motion over an open global menu moves the highlight to the hovered row (mirrors the
    /// monolithic host's `MenuListState::hover`): `Some(idx)` snaps the highlight there, `None`
    /// (off the menu) leaves it put. Returns whether the highlight changed, so the client `Moved`
    /// arm can repaint only on a real move. A no-op when the menu is closed.
    pub(crate) fn hover_client_global_menu_item(&mut self, idx: Option<usize>) -> bool {
        let Some(idx) = idx else {
            return false;
        };
        let item_count = self.client_global_menu_items().len();
        if let ClientOverlayState::GlobalMenu { highlighted } = &mut self.client_overlay {
            let next = idx.min(item_count.saturating_sub(1));
            if *highlighted != next {
                *highlighted = next;
                return true;
            }
        }
        false
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
            5 => {
                self.open_remote_manage_overlay();
                Some(ClientGlobalMenuAction::ManageRemotes)
            }
            _ => None,
        }
    }

    pub(crate) fn open_add_remote_form(&mut self) {
        self.new_workspace_picker = None;
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
            ClientOverlayState::None
            | ClientOverlayState::GlobalMenu { .. }
            | ClientOverlayState::ManageRemotes(_) => None,
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

    // ----- item 3 (Area 5): remote-management overlay -----------------------------------------

    /// Open the management overlay with a fresh selection clamped to the current secondary rows.
    pub(crate) fn open_remote_manage_overlay(&mut self) {
        self.new_workspace_picker = None;
        self.client_overlay = ClientOverlayState::ManageRemotes(RemoteManageOverlay {
            selected: 0,
            scroll: 0,
            confirm_delete: None,
            pending: None,
        });
    }

    pub(crate) fn remote_manage_overlay(&self) -> Option<&RemoteManageOverlay> {
        match &self.client_overlay {
            ClientOverlayState::ManageRemotes(overlay) => Some(overlay),
            ClientOverlayState::None
            | ClientOverlayState::GlobalMenu { .. }
            | ClientOverlayState::AddRemote(_) => None,
        }
    }

    fn remote_manage_overlay_mut(&mut self) -> Option<&mut RemoteManageOverlay> {
        match &mut self.client_overlay {
            ClientOverlayState::ManageRemotes(overlay) => Some(overlay),
            ClientOverlayState::None
            | ClientOverlayState::GlobalMenu { .. }
            | ClientOverlayState::AddRemote(_) => None,
        }
    }

    /// Pure view builder: one `RemoteManageRow` per `Secondary` host, in registry order. `Main`
    /// is never listed. The `Disabled` overlay-state wins over the connection state (the gate).
    pub(crate) fn remote_manage_rows(&self) -> Vec<RemoteManageRow> {
        self.servers
            .iter()
            .filter(|server| server.role == ServerRole::Secondary)
            .map(|server| {
                let state = if server.disabled {
                    RemoteManageState::Disabled
                } else {
                    match server.connection_state {
                        ConnectionState::Connected => RemoteManageState::Connected,
                        ConnectionState::Connecting => RemoteManageState::Connecting,
                        ConnectionState::Disconnected => RemoteManageState::Disconnected,
                        ConnectionState::ProtocolMismatch { .. } => {
                            RemoteManageState::ProtocolMismatch
                        }
                    }
                };
                RemoteManageRow {
                    remote_id: server.id.registry_id().to_string(),
                    name: server.display_name.clone(),
                    target: server.target.display_label(),
                    enabled: !server.disabled,
                    state,
                }
            })
            .collect()
    }

    pub(crate) fn move_remote_manage_next(&mut self) {
        let count = self.remote_manage_rows().len();
        if let Some(overlay) = self.remote_manage_overlay_mut() {
            if overlay.confirm_delete.is_some() {
                return;
            }
            overlay.selected = (overlay.selected + 1).min(count.saturating_sub(1));
        }
    }

    pub(crate) fn move_remote_manage_prev(&mut self) {
        if let Some(overlay) = self.remote_manage_overlay_mut() {
            if overlay.confirm_delete.is_some() {
                return;
            }
            overlay.selected = overlay.selected.saturating_sub(1);
        }
    }

    /// Enter the two-step delete confirmation for the currently-selected remote (if any).
    pub(crate) fn begin_remote_manage_delete(&mut self) {
        let selected_id = self
            .remote_manage_overlay()
            .map(|overlay| overlay.selected)
            .and_then(|selected| self.remote_manage_rows().into_iter().nth(selected))
            .map(|row| row.remote_id);
        if let (Some(remote_id), Some(overlay)) = (selected_id, self.remote_manage_overlay_mut()) {
            overlay.confirm_delete = Some(remote_id);
        }
    }

    pub(crate) fn cancel_remote_manage_delete(&mut self) {
        if let Some(overlay) = self.remote_manage_overlay_mut() {
            overlay.confirm_delete = None;
        }
    }

    /// item 3 (Area 5): mouse-driven selection — clamp and set the selected row index.
    pub(crate) fn set_remote_manage_selected(&mut self, index: usize) {
        let count = self.remote_manage_rows().len();
        if let Some(overlay) = self.remote_manage_overlay_mut() {
            if overlay.confirm_delete.is_some() {
                return;
            }
            overlay.selected = index.min(count.saturating_sub(1));
        }
    }

    /// item 3 (Area 5): clear the in-flight `pending` marker for a finished request, and drop the
    /// delete-confirm sub-state if it was targeting the same remote (a completed delete closes the
    /// popup). No-op when the overlay is closed (e.g. it was navigated away while in flight).
    pub(crate) fn clear_remote_manage_pending(&mut self, remote_id: &str) {
        if let Some(overlay) = self.remote_manage_overlay_mut() {
            if overlay.pending.as_deref() == Some(remote_id) {
                overlay.pending = None;
            }
            if overlay.confirm_delete.as_deref() == Some(remote_id) {
                overlay.confirm_delete = None;
            }
        }
    }

    /// item 3 (Area 5): mouse-driven confirm of the active delete (the red popup's delete button).
    /// Sets `pending` and emits the `Delete` outcome, mirroring the `Enter` key in confirm state.
    pub(crate) fn confirm_remote_manage_delete(&mut self) -> RemoteManageOutcome {
        let Some(remote_id) = self
            .remote_manage_overlay()
            .and_then(|overlay| overlay.confirm_delete.clone())
        else {
            return RemoteManageOutcome::Redraw;
        };
        if self
            .remote_manage_overlay()
            .and_then(|overlay| overlay.pending.as_deref())
            == Some(remote_id.as_str())
        {
            return RemoteManageOutcome::Redraw;
        }
        if let Some(overlay) = self.remote_manage_overlay_mut() {
            overlay.pending = Some(remote_id.clone());
        }
        RemoteManageOutcome::Delete { remote_id }
    }

    /// Translate a key press in the management overlay into a typed outcome. `Space` toggles the
    /// selected remote's enabled flag, `d` enters delete-confirm, `a` jumps to the add-remote
    /// form, `Esc` closes. In delete-confirm sub-state the popup owns input (`Enter`/`d` confirm,
    /// `Esc` cancels; nav keys are inert). A `pending` request for a remote blocks re-issuing a
    /// toggle/delete for THAT remote while it is in flight.
    pub(crate) fn handle_remote_manage_key(
        &mut self,
        key: crate::input::TerminalKey,
    ) -> RemoteManageOutcome {
        use crossterm::event::{KeyCode, KeyEventKind};

        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return RemoteManageOutcome::Redraw;
        }

        let rows = self.remote_manage_rows();
        let Some(overlay) = self.remote_manage_overlay() else {
            return RemoteManageOutcome::Redraw;
        };

        // delete-confirm sub-state: the popup owns input.
        if let Some(remote_id) = overlay.confirm_delete.clone() {
            return match key.code {
                KeyCode::Enter | KeyCode::Char('d') => {
                    if overlay.pending.as_deref() == Some(remote_id.as_str()) {
                        return RemoteManageOutcome::Redraw;
                    }
                    if let Some(overlay) = self.remote_manage_overlay_mut() {
                        overlay.pending = Some(remote_id.clone());
                    }
                    RemoteManageOutcome::Delete { remote_id }
                }
                KeyCode::Esc => {
                    self.cancel_remote_manage_delete();
                    RemoteManageOutcome::Redraw
                }
                _ => RemoteManageOutcome::Redraw,
            };
        }

        match key.code {
            KeyCode::Esc => {
                self.close_client_overlay();
                RemoteManageOutcome::Redraw
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_remote_manage_prev();
                RemoteManageOutcome::Redraw
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_remote_manage_next();
                RemoteManageOutcome::Redraw
            }
            KeyCode::Char(' ') => {
                let Some(row) = rows.get(overlay.selected) else {
                    return RemoteManageOutcome::Redraw;
                };
                if overlay.pending.as_deref() == Some(row.remote_id.as_str()) {
                    return RemoteManageOutcome::Redraw;
                }
                let remote_id = row.remote_id.clone();
                let enabled = !row.enabled;
                if let Some(overlay) = self.remote_manage_overlay_mut() {
                    overlay.pending = Some(remote_id.clone());
                }
                RemoteManageOutcome::SetEnabled { remote_id, enabled }
            }
            KeyCode::Char('d') => {
                if rows.get(overlay.selected).is_some() {
                    self.begin_remote_manage_delete();
                }
                RemoteManageOutcome::Redraw
            }
            KeyCode::Char('a') => {
                self.open_add_remote_form();
                RemoteManageOutcome::OpenAddRemote
            }
            _ => RemoteManageOutcome::Redraw,
        }
    }

    pub(crate) fn choose_new_workspace_destination(
        &mut self,
        server_id: &ServerId,
    ) -> NewWorkspaceRoute {
        let destination_is_visible = self.new_workspace_picker.as_ref().is_some_and(|picker| {
            picker
                .destinations
                .iter()
                .any(|destination| &destination.server_id == server_id)
        });
        self.new_workspace_picker = None;
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
        // item 6 (Area 6): optimistic focus override. This arm is only reached after the
        // existence check above, so the target is a real workspace with a `Some(_)` id when
        // rendered. `Unavailable`/`NotFound` return before this point and set NO optimistic
        // focus (the "only set on resolved `Some(_)` targets" rule).
        self.optimistic_focus = Some((
            server_id.clone(),
            OptimisticFocusTarget::Workspace(workspace_id.to_string()),
        ));
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
        // item 6 (Area 6): optimistic agent focus. Only reached after the existence check
        // above, so the agent (and its workspace) is real. `Unavailable`/`NotFound` set none.
        self.optimistic_focus = Some((
            server_id.clone(),
            OptimisticFocusTarget::Agent(agent_id.to_string()),
        ));
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
        // item 6 (Area 6): the optimistic-focus override. For the optimistically-focused server
        // only, clear summary-derived `focused` on all of its rows and set it on the optimistic
        // target's row. Placeholder rows have `workspace_id == None`, so they never match and
        // never become focused. Every other server's rows are returned untouched. The override
        // only flips a `focused` bool on a real `Workspace` row; it never reorders rows, so
        // `from_model`'s `active_idx` (computed from this flat stream) cannot be shifted.
        self.visible_servers()
            .into_iter()
            .flat_map(|server| {
                let mut rows = workspace_rows_for_server(server, all_filter);
                if let Some(focused_workspace) = self.optimistic_focused_workspace_id(&server.id) {
                    for row in &mut rows {
                        row.focused =
                            row.workspace_id.as_deref() == Some(focused_workspace.as_str());
                    }
                }
                rows
            })
            .collect()
    }

    /// item 6 (Area 6): resolve the workspace id that the optimistic focus targets on `server_id`
    /// (if any). For a `Workspace` target it is the target id directly; for an `Agent` target it
    /// is the agent's owning workspace, joined through the server's summary (the same
    /// `agent.workspace_id == workspace.workspace_id` join `agent_groups_for_server` uses). Returns
    /// `None` when the optimistic focus is unset, targets another server, or the agent is unknown.
    fn optimistic_focused_workspace_id(&self, server_id: &ServerId) -> Option<String> {
        let (focus_server, target) = self.optimistic_focus.as_ref()?;
        if focus_server != server_id {
            return None;
        }
        match target {
            OptimisticFocusTarget::Workspace(workspace_id) => Some(workspace_id.clone()),
            OptimisticFocusTarget::Agent(agent_id) => self
                .server(server_id)?
                .summaries
                .agents
                .iter()
                .find(|agent| &agent.agent_id == agent_id)
                .map(|agent| agent.workspace_id.clone()),
        }
    }

    /// item 2 (C3): one banner spec per visible `Secondary` host, in `visible_servers()`
    /// order. Each tuple is `(insertion_index, spec)` where `insertion_index` is the position
    /// in the flat `workspace_rows()` stream at which the banner precedes that host's first
    /// row. `Main` yields no entry; monolithic mode (no secondaries) yields an empty Vec.
    pub(crate) fn host_banner_specs(&self) -> Vec<(usize, crate::app::state::HostBannerSpec)> {
        let all_filter = self.filter == ServerFilter::All;
        let mut specs = Vec::new();
        let mut row_offset = 0usize;
        for server in self.visible_servers() {
            let rows = workspace_rows_for_server(server, all_filter);
            if server.role == ServerRole::Secondary {
                let space_count = server
                    .summaries
                    .workspaces
                    .iter()
                    .filter(|workspace| !workspace.workspace_id.is_empty())
                    .count();
                specs.push((
                    row_offset,
                    crate::app::state::HostBannerSpec {
                        display_name: server.display_name.clone(),
                        connection_state: host_banner_state(server),
                        space_count,
                    },
                ));
            }
            row_offset += rows.len();
        }
        specs
    }

    /// item 2 (C3) coordination flag — `true` whenever the host-banner feature is live, i.e.
    /// at least one visible `Secondary` host group will render a banner. Read once in
    /// `from_model` and used by `workspace_list_entries` to flip the divider to plain. `false`
    /// in monolithic mode. This is read-only render state; never mutated during render.
    pub(crate) fn host_banner_active(&self) -> bool {
        !self.host_banner_specs().is_empty()
    }

    /// Whether a host banner is currently animating. The SINGLE banner-active input read by
    /// `compositor::sidebar_wants_animation` (contract Area 1: do not invent a second clock or
    /// second flag). `true` iff the animation is set to `Animated` AND at least one banner is
    /// visible.
    pub(crate) fn host_banner_animation_active(&self) -> bool {
        self.ui_settings().sidebar_host.animation == crate::config::HostBannerAnimation::Animated
            && self.host_banner_active()
    }

    pub(crate) fn agent_groups(&self) -> Vec<AgentSidebarGroup> {
        let all_filter = self.filter == ServerFilter::All;
        self.visible_servers()
            .into_iter()
            .filter(|server| server.connection_state == ConnectionState::Connected)
            .flat_map(|server| {
                let mut groups = agent_groups_for_server(server, all_filter);
                // item 6 (Area 6): apply the optimistic focus override to the optimistically-
                // focused server's groups only. Other servers are untouched.
                if let Some((focus_server, target)) = self.optimistic_focus.as_ref() {
                    if focus_server == &server.id {
                        Self::apply_optimistic_focus_to_groups(&mut groups, target);
                    }
                }
                groups
            })
            .collect()
    }

    /// item 6 (Area 6): rewrite the per-group `focused` (spaces highlight) and per-agent
    /// `focused` flags so the optimistic target is the only focused entry on its server.
    fn apply_optimistic_focus_to_groups(
        groups: &mut [AgentSidebarGroup],
        target: &OptimisticFocusTarget,
    ) {
        match target {
            OptimisticFocusTarget::Workspace(workspace_id) => {
                for group in groups.iter_mut() {
                    group.focused = &group.workspace_id == workspace_id;
                    for agent in &mut group.agents {
                        agent.focused = false;
                    }
                }
            }
            OptimisticFocusTarget::Agent(agent_id) => {
                for group in groups.iter_mut() {
                    let mut group_has_focus = false;
                    for agent in &mut group.agents {
                        agent.focused = &agent.agent_id == agent_id;
                        group_has_focus |= agent.focused;
                    }
                    // Mark the agent's group focused so the spaces highlight follows the agent
                    // (matches the `focused: workspace.focused || agents.any(..)` invariant).
                    group.focused = group_has_focus;
                }
            }
        }
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

    #[cfg(test)]
    pub(crate) fn server_for_test(&self, id: &ServerId) -> Option<&ManagedServer> {
        self.server(id)
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

    pub(crate) fn close_new_workspace_picker(&mut self) {
        self.new_workspace_picker = None;
    }

    pub(crate) fn close_client_overlay(&mut self) {
        self.client_overlay = ClientOverlayState::None;
    }

    fn add_remote_form_mut(&mut self) -> Option<&mut AddRemoteForm> {
        match &mut self.client_overlay {
            ClientOverlayState::AddRemote(form) => Some(form),
            ClientOverlayState::None
            | ClientOverlayState::GlobalMenu { .. }
            | ClientOverlayState::ManageRemotes(_) => None,
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
        let Some(picker) = self.new_workspace_picker.take() else {
            return;
        };

        let next_destinations: Vec<ServerDestination> = picker
            .destinations
            .into_iter()
            .filter_map(|existing| {
                connected_destinations
                    .iter()
                    .find(|current| current.server_id == existing.server_id)
                    .cloned()
            })
            .collect();
        if !next_destinations.is_empty() {
            // item 1: clamp the carried selection to the filtered list so a disconnect of the
            // highlighted (e.g. last) destination cannot leave `selected` out of range.
            let selected = picker.selected.min(next_destinations.len() - 1);
            self.new_workspace_picker = Some(NewWorkspacePickerState {
                destinations: next_destinations,
                selected,
            });
        }
    }
}

fn workspace_rows_for_server(server: &ManagedServer, all_filter: bool) -> Vec<WorkspaceSidebarRow> {
    // item 4: a row is remote iff its server is a secondary host.
    let is_remote = server.role == ServerRole::Secondary;

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
            is_remote,
        }];
    }

    if server.role == ServerRole::Secondary && server.summaries.workspaces.is_empty() {
        return vec![WorkspaceSidebarRow {
            server_id: server.id.clone(),
            workspace_id: None,
            label: "no workspaces".to_string(),
            branch: None,
            focused: false,
            disabled: true,
            is_remote,
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
            is_remote,
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

fn workspace_label(_server: &ManagedServer, label: &str, _all_filter: bool) -> String {
    // item 2 (C3): always the bare space label. Host identity now lives in the per-host
    // banner row above each remote group (drops the jammed "{display_name} {label}"). This
    // applies to both `workspace_rows_for_server` and `agent_groups_for_server` call sites.
    label.to_string()
}

fn unavailable_row_label(server: &ManagedServer) -> String {
    // item 3 (Area 5): a disabled remote reads "<name> disabled" first — the gate, not the
    // connection state, governs the placeholder. Checked BEFORE the connection-state match.
    if server.disabled {
        return format!("{} disabled", server.display_name);
    }
    // item 2 (C3): drop the `display_name` prefix — the banner above carries the host name.
    // The placeholder row keeps the state word for layout; the banner suffix is the primary
    // state signal.
    match server.connection_state {
        ConnectionState::Connecting => "connecting".to_string(),
        ConnectionState::Disconnected => "offline".to_string(),
        ConnectionState::ProtocolMismatch { .. } => "protocol mismatch".to_string(),
        ConnectionState::Connected => "connected".to_string(),
    }
}

/// item 2 (C3): map a managed host's connection/disabled state to the banner state used for
/// the per-host banner suffix + glyph. `Disabled` (item 3 `server.disabled`) wins over the
/// connection state; until item 3 wires the flag it is always `false`.
fn host_banner_state(server: &ManagedServer) -> crate::app::state::HostBannerState {
    use crate::app::state::HostBannerState;
    if server.disabled {
        return HostBannerState::Disabled;
    }
    match server.connection_state {
        ConnectionState::Connected => HostBannerState::Connected,
        ConnectionState::Connecting => HostBannerState::Connecting,
        ConnectionState::Disconnected => HostBannerState::Disconnected,
        ConnectionState::ProtocolMismatch { .. } => HostBannerState::ProtocolMismatch,
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
        disabled: definition.disabled, // item 3 (Area 5): gate input from the registry.
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

    #[test]
    fn workspace_sidebar_row_is_remote_set_in_all_paths() {
        // Path 1: secondary offline placeholder (disconnected, no summary).
        let offline = ServerId::secondary("offline");
        // Path 2: secondary empty-remote placeholder (connected, empty summary).
        let empty = ServerId::secondary("empty");
        // Path 3: secondary normal row (connected, with summary).
        let normal = ServerId::secondary("normal");

        let mut model = ClientSupervisorModel::new("local");
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
        model.add_secondary(ssh_remote("offline", "off", "off"));
        model.add_secondary(ssh_remote("empty", "emp", "emp"));
        model.add_secondary(ssh_remote("normal", "nrm", "nrm"));

        model
            .set_connection_state(&offline, ConnectionState::Disconnected)
            .unwrap();
        model
            .set_connection_state(&empty, ConnectionState::Connected)
            .unwrap();
        model.set_summary(&empty, ServerSummary::default()).unwrap();
        model
            .set_connection_state(&normal, ConnectionState::Connected)
            .unwrap();
        model
            .set_summary(
                &normal,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-ws".into(),
                        label: "api".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();

        let rows = model.workspace_rows();
        let row_for = |id: &ServerId| {
            rows.iter()
                .find(|row| &row.server_id == id)
                .expect("row for server")
        };

        // Main server row is local.
        assert!(!row_for(&ServerId::main()).is_remote);
        // All three secondary paths report remote.
        let offline_row = row_for(&offline);
        assert!(offline_row.is_remote);
        assert!(offline_row.workspace_id.is_none());
        let empty_row = row_for(&empty);
        assert!(empty_row.is_remote);
        assert!(empty_row.workspace_id.is_none());
        let normal_row = row_for(&normal);
        assert!(normal_row.is_remote);
        assert!(normal_row.workspace_id.is_some());
    }

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
            disabled: false,
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
            disabled: false,
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
                    is_remote: false,
                },
                WorkspaceSidebarRow {
                    server_id: ServerId::secondary("remote-ssh"),
                    workspace_id: None,
                    label: "connecting".into(),
                    branch: None,
                    focused: false,
                    disabled: true,
                    is_remote: true,
                },
                WorkspaceSidebarRow {
                    server_id: ServerId::secondary("remote-dev"),
                    workspace_id: None,
                    label: "connecting".into(),
                    branch: None,
                    focused: false,
                    disabled: true,
                    is_remote: true,
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
                    is_remote: false,
                },
                WorkspaceSidebarRow {
                    server_id: remote_id,
                    workspace_id: Some("remote-api".into()),
                    label: "api".into(),
                    branch: None,
                    focused: false,
                    disabled: false,
                    is_remote: true,
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
                    label: "protocol mismatch".into(),
                    branch: None,
                    focused: false,
                    disabled: true,
                    is_remote: true,
                },
                WorkspaceSidebarRow {
                    server_id: ServerId::secondary("remote-dev"),
                    workspace_id: Some("dev-workspace".into()),
                    label: "api".into(),
                    branch: None,
                    focused: false,
                    disabled: false,
                    is_remote: true,
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
    fn new_workspace_picker_selection_defaults_to_zero_and_moves() {
        let mut model = ClientSupervisorModel::new("local");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model.add_secondary(ssh_remote("remote-y", "y", "y"));

        model.open_new_workspace_picker();
        assert_eq!(model.new_workspace_picker().map(|p| p.selected), Some(0));

        model.move_new_workspace_picker_next();
        assert_eq!(model.new_workspace_picker().map(|p| p.selected), Some(1));
        model.move_new_workspace_picker_next();
        assert_eq!(model.new_workspace_picker().map(|p| p.selected), Some(2));
        // saturates at the last destination (main + 2 remotes == 3).
        model.move_new_workspace_picker_next();
        assert_eq!(model.new_workspace_picker().map(|p| p.selected), Some(2));

        model.move_new_workspace_picker_prev();
        assert_eq!(model.new_workspace_picker().map(|p| p.selected), Some(1));
        model.move_new_workspace_picker_prev();
        model.move_new_workspace_picker_prev();
        // saturates at the first destination.
        assert_eq!(model.new_workspace_picker().map(|p| p.selected), Some(0));
    }

    #[test]
    fn new_workspace_picker_clamps_selection_on_reconcile() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model.open_new_workspace_picker();
        // highlight the last destination (the remote).
        model.move_new_workspace_picker_next();
        assert_eq!(model.new_workspace_picker().map(|p| p.selected), Some(1));

        // disconnecting the highlighted destination triggers reconcile.
        model
            .set_connection_state(&remote_id, ConnectionState::Disconnected)
            .unwrap();

        let picker = model.new_workspace_picker().expect("picker remains open");
        assert!(picker.selected < picker.destinations.len());
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn closing_picker_clears_selection() {
        let mut model = ClientSupervisorModel::new("local");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model.open_new_workspace_picker();
        assert!(model.new_workspace_picker().is_some());

        model.close_new_workspace_picker();
        assert_eq!(model.new_workspace_picker(), None);
    }

    #[test]
    fn new_workspace_picker_single_destination_skips_modal() {
        let mut model = ClientSupervisorModel::new("local");

        let route = model.open_new_workspace_picker();

        assert_eq!(route, NewWorkspaceRoute::CreateOn(ServerId::main()));
        assert_eq!(model.new_workspace_picker(), None);
    }

    #[test]
    fn accept_new_workspace_picker_returns_highlighted_route() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model.open_new_workspace_picker();
        // move the highlight onto the remote destination.
        model.move_new_workspace_picker_next();

        let route = model.accept_new_workspace_picker();

        assert_eq!(route, NewWorkspaceRoute::CreateOn(remote_id.clone()));
        assert_eq!(model.active_server_id(), &remote_id);
        assert_eq!(model.new_workspace_picker(), None);
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
    fn workspace_label_drops_host_prefix() {
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
                    is_remote: false,
                },
                WorkspaceSidebarRow {
                    server_id: remote_id.clone(),
                    workspace_id: Some("remote-herdr".into()),
                    // item 2 (C3): bare label even in the `All` filter — host identity is the
                    // banner above, not a jammed prefix.
                    label: "herdr".into(),
                    branch: None,
                    focused: false,
                    disabled: false,
                    is_remote: true,
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
                is_remote: true,
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
                label: "offline".into(),
                branch: None,
                focused: false,
                disabled: true,
                is_remote: true,
            }]
        );

        model.set_filter(ServerFilter::Server(remote_id.clone()));
        assert_eq!(
            model.workspace_rows(),
            vec![WorkspaceSidebarRow {
                server_id: remote_id,
                workspace_id: None,
                label: "offline".into(),
                branch: None,
                focused: false,
                disabled: true,
                is_remote: true,
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
                label: "no workspaces".into(),
                branch: None,
                focused: false,
                disabled: true,
                is_remote: true,
            }]
        );
    }

    #[test]
    fn agent_group_label_has_no_host_prefix() {
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
                    label: "herdr".into(),
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

    // item 6 (Area 6): optimistic focus override tests. A connected remote with two workspaces
    // and one agent so the override can be observed across `workspace_rows()`/`agent_groups()`.
    fn optimistic_focus_model() -> (ClientSupervisorModel, ServerId) {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![
                        WorkspaceSummary {
                            workspace_id: "remote-api".into(),
                            label: "api".into(),
                            branch: None,
                            focused: false,
                        },
                        WorkspaceSummary {
                            workspace_id: "remote-web".into(),
                            label: "web".into(),
                            branch: None,
                            focused: true,
                        },
                    ],
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

    #[test]
    fn focus_workspace_route_sets_optimistic_focus() {
        let (mut model, remote_id) = optimistic_focus_model();

        // Pre-focus: the summary's `remote-web` row is the focused remote row.
        assert!(model
            .workspace_rows()
            .iter()
            .any(|row| row.workspace_id.as_deref() == Some("remote-web") && row.focused));

        model.focus_workspace_route(&remote_id, "remote-api");

        let rows = model.workspace_rows();
        let remote_rows: Vec<_> = rows
            .iter()
            .filter(|row| row.server_id == remote_id)
            .collect();
        assert!(
            remote_rows
                .iter()
                .filter(|row| row.focused)
                .all(|row| row.workspace_id.as_deref() == Some("remote-api")),
            "only the optimistic target should be focused"
        );
        assert!(remote_rows
            .iter()
            .any(|row| row.workspace_id.as_deref() == Some("remote-api") && row.focused));
        // Every OTHER remote row is now unfocused (summary `focused` cleared on this server).
        assert!(remote_rows
            .iter()
            .filter(|row| row.workspace_id.as_deref() != Some("remote-api"))
            .all(|row| !row.focused));
    }

    #[test]
    fn focus_agent_route_sets_optimistic_focus() {
        let (mut model, remote_id) = optimistic_focus_model();

        model.focus_agent_route(&remote_id, "remote-agent");

        let groups = model.agent_groups();
        let group = groups
            .iter()
            .find(|group| group.workspace_id == "remote-api")
            .expect("agent's workspace group should exist");
        assert!(
            group.focused,
            "the agent's group (spaces highlight) follows"
        );
        assert!(group
            .agents
            .iter()
            .any(|agent| agent.agent_id == "remote-agent" && agent.focused));

        // The agent's workspace row also follows in `workspace_rows()`.
        let rows = model.workspace_rows();
        assert!(rows
            .iter()
            .any(|row| row.workspace_id.as_deref() == Some("remote-api") && row.focused));
        assert!(rows
            .iter()
            .filter(|row| row.server_id == remote_id)
            .filter(|row| row.workspace_id.as_deref() != Some("remote-api"))
            .all(|row| !row.focused));
    }

    #[test]
    fn optimistic_focus_cleared_when_summary_applied() {
        let (mut model, remote_id) = optimistic_focus_model();

        // Apply via apply_secondary_summary_results (authoritative wins).
        model.focus_workspace_route(&remote_id, "remote-api");
        model.apply_secondary_summary_results([(
            remote_id.clone(),
            Ok(ServerSummary {
                workspaces: vec![WorkspaceSummary {
                    workspace_id: "remote-api".into(),
                    label: "api".into(),
                    branch: None,
                    focused: false,
                }],
                agents: Vec::new(),
            }),
        )]);
        assert!(
            model
                .workspace_rows()
                .iter()
                .filter(|row| row.server_id == remote_id)
                .all(|row| !row.focused),
            "summary truth (focused == false) wins after apply"
        );

        // Apply via set_summary (also clears).
        model.focus_workspace_route(&remote_id, "remote-api");
        model
            .set_summary(
                &remote_id,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "remote-web".into(),
                        label: "web".into(),
                        branch: None,
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        assert!(model
            .workspace_rows()
            .iter()
            .any(|row| row.workspace_id.as_deref() == Some("remote-web") && row.focused));
        assert!(model
            .workspace_rows()
            .iter()
            .filter(|row| row.workspace_id.as_deref() != Some("remote-web"))
            .all(|row| !row.focused));
    }

    #[test]
    fn optimistic_focus_only_affects_its_server() {
        let mut model = ClientSupervisorModel::new("local");
        let server_a = model.add_secondary(ssh_remote("a", "a", "a"));
        let server_b = model.add_secondary(ssh_remote("b", "b", "b"));
        model
            .set_summary(
                &server_a,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "a-ws".into(),
                        label: "a".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        model
            .set_summary(
                &server_b,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "b-ws".into(),
                        label: "b".into(),
                        branch: None,
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();

        model.focus_workspace_route(&server_a, "a-ws");

        let rows = model.workspace_rows();
        // B's summary-derived focused flag is untouched.
        assert!(rows
            .iter()
            .any(|row| row.workspace_id.as_deref() == Some("b-ws") && row.focused));
        assert!(rows
            .iter()
            .any(|row| row.workspace_id.as_deref() == Some("a-ws") && row.focused));
    }

    #[test]
    fn optimistic_focus_overrides_stale_focused_on_other_server() {
        let mut model = ClientSupervisorModel::new("local");
        let server_a = model.add_secondary(ssh_remote("a", "a", "a"));
        let server_b = model.add_secondary(ssh_remote("b", "b", "b"));
        model
            .set_summary(
                &server_a,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "a-ws".into(),
                        label: "a".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        // B carries a stale focused row, and main is focused too.
        model
            .set_summary(
                &server_b,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "b-ws".into(),
                        label: "b".into(),
                        branch: None,
                        focused: true,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();

        model.focus_workspace_route(&server_a, "a-ws");

        let rows = model.workspace_rows();
        let focused: Vec<_> = rows
            .iter()
            .filter(|row| row.focused)
            .filter_map(|row| row.workspace_id.clone())
            .collect();
        // Focus is single across the fleet on server A's optimistic row. B's stale focus is
        // left intact (single-server override), but since focus on A overrides A's summary, the
        // optimistic row is the only focused row server A contributes — and `from_model` lands
        // `active_idx` on it. Here we assert A's optimistic row is focused and is the only
        // focused row on A.
        assert!(focused.contains(&"a-ws".to_string()));
        assert!(rows
            .iter()
            .filter(|row| row.server_id == server_a)
            .filter(|row| row.focused)
            .all(|row| row.workspace_id.as_deref() == Some("a-ws")));
    }

    #[test]
    fn optimistic_focus_skips_disabled_or_unavailable_targets() {
        let mut model = ClientSupervisorModel::new("local");
        let remote_id = ServerId::secondary("remote-x");
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        model
            .set_connection_state(&remote_id, ConnectionState::Disconnected)
            .unwrap();

        // Unavailable owner: no optimistic focus is set.
        let route = model.focus_workspace_route(&remote_id, "remote-api");
        assert!(matches!(route, FocusRoute::Unavailable { .. }));
        // The disconnected remote renders a `None`-id placeholder row; it stays unfocused.
        assert!(model
            .workspace_rows()
            .iter()
            .filter(|row| row.server_id == remote_id)
            .all(|row| !row.focused && row.workspace_id.is_none()));

        // Reconnect, but focus an unknown id (NotFound) — still no optimistic focus.
        model
            .set_connection_state(&remote_id, ConnectionState::Connected)
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
        let route = model.focus_workspace_route(&remote_id, "does-not-exist");
        assert_eq!(route, FocusRoute::NotFound);
        assert!(model
            .workspace_rows()
            .iter()
            .filter(|row| row.server_id == remote_id)
            .all(|row| !row.focused));
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
                "add remote",
                "manage remotes"
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
    fn hover_client_global_menu_item_moves_highlight() {
        let mut model = ClientSupervisorModel::new("local");
        // a no-op when the menu is closed (and reports no change).
        assert!(!model.hover_client_global_menu_item(Some(2)));
        assert_eq!(model.client_global_menu_highlighted(), None);

        model.open_client_global_menu();
        assert_eq!(model.client_global_menu_highlighted(), Some(0));

        // Some(idx) snaps the highlight and reports the change; re-hovering the same row is a no-op.
        assert!(model.hover_client_global_menu_item(Some(2)));
        assert_eq!(model.client_global_menu_highlighted(), Some(2));
        assert!(!model.hover_client_global_menu_item(Some(2)));

        // None (off the menu) leaves the highlight put.
        assert!(!model.hover_client_global_menu_item(None));
        assert_eq!(model.client_global_menu_highlighted(), Some(2));

        // an out-of-range index clamps to the last item (and reports the change from row 2).
        assert!(model.hover_client_global_menu_item(Some(99)));
        assert_eq!(
            model.client_global_menu_highlighted(),
            Some(model.client_global_menu_items().len() - 1)
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

    fn set_host_animation(
        model: &mut ClientSupervisorModel,
        animation: crate::config::HostBannerAnimation,
    ) {
        let mut ui_settings = model.ui_settings().clone();
        ui_settings.sidebar_host.animation = animation;
        model.set_ui_settings(ui_settings);
    }

    #[test]
    fn host_banner_specs_one_per_visible_remote() {
        use crate::app::state::HostBannerState;
        let mut model = ClientSupervisorModel::new("local");
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
        let dev = model.add_secondary(local_remote("dev", "dev", Some("dev")));
        let prod = model.add_secondary(ssh_remote("prod", "prod", "prod.example.com"));
        model
            .set_summary(
                &dev,
                ServerSummary {
                    workspaces: vec![
                        WorkspaceSummary {
                            workspace_id: "dev-a".into(),
                            label: "a".into(),
                            branch: None,
                            focused: false,
                        },
                        WorkspaceSummary {
                            workspace_id: "dev-b".into(),
                            label: "b".into(),
                            branch: None,
                            focused: false,
                        },
                    ],
                    agents: Vec::new(),
                },
            )
            .unwrap();
        model
            .set_summary(
                &prod,
                ServerSummary {
                    workspaces: vec![WorkspaceSummary {
                        workspace_id: "prod-a".into(),
                        label: "p".into(),
                        branch: None,
                        focused: false,
                    }],
                    agents: Vec::new(),
                },
            )
            .unwrap();

        let specs = model.host_banner_specs();
        // Exactly one spec per visible Secondary, none for Main, in visible_servers() order.
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].1.display_name, "dev");
        assert_eq!(specs[0].1.connection_state, HostBannerState::Connected);
        assert_eq!(specs[0].1.space_count, 2);
        assert_eq!(specs[1].1.display_name, "prod");
        assert_eq!(specs[1].1.space_count, 1);

        // The insertion index precedes that host's first row in the flat workspace_rows() stream.
        let rows = model.workspace_rows();
        assert_eq!(
            rows[specs[0].0].server_id,
            ServerId::secondary("dev"),
            "dev banner index precedes dev's first row"
        );
        assert_eq!(
            rows[specs[1].0].server_id,
            ServerId::secondary("prod"),
            "prod banner index precedes prod's first row"
        );
    }

    #[test]
    fn host_banner_state_mapping() {
        use crate::app::state::HostBannerState;
        let mut model = ClientSupervisorModel::new("local");
        let connecting = model.add_secondary(ssh_remote("c", "c", "c"));
        let disconnected = model.add_secondary(ssh_remote("d", "d", "d"));
        let mismatch = model.add_secondary(ssh_remote("m", "m", "m"));
        let empty = model.add_secondary(ssh_remote("e", "e", "e"));

        model
            .set_connection_state(&connecting, ConnectionState::Connecting)
            .unwrap();
        model
            .set_connection_state(&disconnected, ConnectionState::Disconnected)
            .unwrap();
        model
            .set_connection_state(
                &mismatch,
                ConnectionState::ProtocolMismatch {
                    server_protocol: Some(1),
                    client_protocol: 2,
                },
            )
            .unwrap();
        // Empty connected host: connected with zero spaces.
        model.set_summary(&empty, ServerSummary::default()).unwrap();

        let by_name = |name: &str| {
            model
                .host_banner_specs()
                .into_iter()
                .find(|(_, spec)| spec.display_name == name)
                .map(|(_, spec)| spec)
                .expect("spec exists")
        };
        assert_eq!(by_name("c").connection_state, HostBannerState::Connecting);
        assert_eq!(by_name("d").connection_state, HostBannerState::Disconnected);
        assert_eq!(
            by_name("m").connection_state,
            HostBannerState::ProtocolMismatch
        );
        let empty_spec = by_name("e");
        assert_eq!(empty_spec.connection_state, HostBannerState::Connected);
        assert_eq!(empty_spec.space_count, 0);
    }

    #[test]
    fn host_banner_active_true_with_remote() {
        // Monolithic / all-main: no banner.
        let mut model = ClientSupervisorModel::new("local");
        assert!(!model.host_banner_active());
        // One visible secondary → active.
        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        assert!(model.host_banner_active());
    }

    #[test]
    fn host_banner_animation_active_gated() {
        let mut model = ClientSupervisorModel::new("local");
        // No remote → never active even when Animated.
        set_host_animation(&mut model, crate::config::HostBannerAnimation::Animated);
        assert!(!model.host_banner_animation_active());

        model.add_secondary(ssh_remote("remote-x", "x", "x"));
        // Animated + a banner → active (this feeds sidebar_wants_animation).
        assert!(model.host_banner_animation_active());
        // Static → not active even with a banner.
        set_host_animation(&mut model, crate::config::HostBannerAnimation::Static);
        assert!(!model.host_banner_animation_active());
    }

    // ----- item 3 (Area 5): disabled-gate + management overlay -------------------------------

    fn disabled_ssh_remote(
        id: &str,
        name: &str,
        target: &str,
    ) -> crate::remote_registry::RemoteDefinitionSnapshot {
        let mut remote = ssh_remote(id, name, target);
        remote.disabled = true;
        remote
    }

    #[test]
    fn disabled_remote_excluded_from_all_four_plan_producers() {
        let mut model = ClientSupervisorModel::new("local");
        // Sync with one disabled secondary.
        model.sync_remote_registry(vec![disabled_ssh_remote("r1", "alpha", "alpha")]);
        let id = ServerId::secondary("r1");

        let connected: std::collections::HashSet<ServerId> = std::collections::HashSet::new();
        assert!(model
            .secondary_connection_plans()
            .iter()
            .all(|plan| plan.server_id != id));
        assert!(model
            .summary_subscription_plans()
            .iter()
            .all(|plan| plan.server_id != id));
        assert!(!model.unconnected_secondary_server_ids().contains(&id));
        assert!(!model
            .secondary_server_ids_missing_client_stream(&connected)
            .contains(&id));

        // Re-enable (sync with disabled = false) — now included again.
        model.sync_remote_registry(vec![ssh_remote("r1", "alpha", "alpha")]);
        assert!(model
            .secondary_connection_plans()
            .iter()
            .any(|plan| plan.server_id == id));
        assert!(model.unconnected_secondary_server_ids().contains(&id));
        assert!(model
            .secondary_server_ids_missing_client_stream(&connected)
            .contains(&id));
    }

    #[test]
    fn disabled_remote_row_label_is_name_disabled() {
        let mut model = ClientSupervisorModel::new("local");
        model.sync_remote_registry(vec![disabled_ssh_remote("r1", "alpha", "alpha")]);

        let rows = model.workspace_rows();
        let row = rows
            .iter()
            .find(|row| row.server_id == ServerId::secondary("r1"))
            .expect("disabled secondary row");
        assert!(row.disabled);
        assert_eq!(row.label, "alpha disabled");
    }

    #[test]
    fn summary_subscription_excludes_stale_connected_disabled() {
        let mut model = ClientSupervisorModel::new("local");
        let id = ServerId::secondary("r1");
        // Bring it up connected, then disable while leaving connection_state == Connected.
        model.add_secondary(ssh_remote("r1", "alpha", "alpha"));
        model
            .set_connection_state(&id, ConnectionState::Connected)
            .unwrap();
        // sync to a disabled definition flips ManagedServer.disabled WITHOUT touching state.
        model.sync_remote_registry(vec![disabled_ssh_remote("r1", "alpha", "alpha")]);

        let server = model.server(&id).expect("server");
        assert_eq!(server.connection_state, ConnectionState::Connected);
        assert!(server.disabled);
        // The gate (not just connection state) keeps it out of summary subscriptions.
        assert!(model
            .summary_subscription_plans()
            .iter()
            .all(|plan| plan.server_id != id));
    }

    #[test]
    fn open_remote_manage_overlay_sets_state() {
        let mut model = ClientSupervisorModel::new("local");
        assert!(model.remote_manage_overlay().is_none());
        model.open_remote_manage_overlay();
        assert!(model.remote_manage_overlay().is_some());
    }

    #[test]
    fn remote_manage_rows_lists_all_secondaries() {
        let mut model = ClientSupervisorModel::new("local");
        model.add_secondary(ssh_remote("r1", "alpha", "alpha"));
        model.add_secondary(disabled_ssh_remote("r2", "beta", "beta"));

        let rows = model.remote_manage_rows();
        assert_eq!(rows.len(), 2, "Main is never listed");
        assert_eq!(rows[0].remote_id, "r1");
        assert_eq!(rows[0].name, "alpha");
        assert!(rows[0].enabled);
        assert_eq!(rows[0].state, RemoteManageState::Connected);
        assert_eq!(rows[1].remote_id, "r2");
        assert!(!rows[1].enabled);
        assert_eq!(rows[1].state, RemoteManageState::Disabled);
    }

    #[test]
    fn remote_manage_nav_clamps() {
        let mut model = ClientSupervisorModel::new("local");
        model.add_secondary(ssh_remote("r1", "alpha", "alpha"));
        model.add_secondary(ssh_remote("r2", "beta", "beta"));
        model.open_remote_manage_overlay();

        model.move_remote_manage_prev(); // already at 0
        assert_eq!(model.remote_manage_overlay().unwrap().selected, 0);
        model.move_remote_manage_next();
        assert_eq!(model.remote_manage_overlay().unwrap().selected, 1);
        model.move_remote_manage_next(); // clamp to last (index 1)
        assert_eq!(model.remote_manage_overlay().unwrap().selected, 1);
    }

    #[test]
    fn begin_cancel_delete_scopes_to_selected() {
        let mut model = ClientSupervisorModel::new("local");
        model.add_secondary(ssh_remote("r1", "alpha", "alpha"));
        model.add_secondary(ssh_remote("r2", "beta", "beta"));
        model.open_remote_manage_overlay();
        model.move_remote_manage_next(); // select r2

        model.begin_remote_manage_delete();
        assert_eq!(
            model
                .remote_manage_overlay()
                .unwrap()
                .confirm_delete
                .as_deref(),
            Some("r2")
        );
        model.cancel_remote_manage_delete();
        assert!(model
            .remote_manage_overlay()
            .unwrap()
            .confirm_delete
            .is_none());
    }

    fn press(code: crossterm::event::KeyCode) -> crate::input::TerminalKey {
        crate::input::TerminalKey::new(code, crossterm::event::KeyModifiers::empty())
    }

    #[test]
    fn manage_key_emits_expected_outcomes() {
        use crossterm::event::KeyCode;
        let mut model = ClientSupervisorModel::new("local");
        model.add_secondary(ssh_remote("r1", "alpha", "alpha"));
        model.open_remote_manage_overlay();

        // Space toggles enable -> disable for the selected (currently-enabled) remote.
        let outcome = model.handle_remote_manage_key(press(KeyCode::Char(' ')));
        assert_eq!(
            outcome,
            RemoteManageOutcome::SetEnabled {
                remote_id: "r1".into(),
                enabled: false
            }
        );
        // pending now blocks a re-issue for r1.
        assert_eq!(
            model.handle_remote_manage_key(press(KeyCode::Char(' '))),
            RemoteManageOutcome::Redraw
        );
        // clear pending and enter delete-confirm.
        model.clear_remote_manage_pending("r1");
        let confirm = model.handle_remote_manage_key(press(KeyCode::Char('d')));
        assert_eq!(confirm, RemoteManageOutcome::Redraw);
        assert_eq!(
            model
                .remote_manage_overlay()
                .unwrap()
                .confirm_delete
                .as_deref(),
            Some("r1")
        );
        // nav keys are inert while confirm is active.
        model.handle_remote_manage_key(press(KeyCode::Down));
        assert_eq!(model.remote_manage_overlay().unwrap().selected, 0);
        // Enter in confirm emits Delete.
        let deleted = model.handle_remote_manage_key(press(KeyCode::Enter));
        assert_eq!(
            deleted,
            RemoteManageOutcome::Delete {
                remote_id: "r1".into()
            }
        );

        // `a` opens the add-remote form (fresh overlay first).
        let mut model = ClientSupervisorModel::new("local");
        model.add_secondary(ssh_remote("r1", "alpha", "alpha"));
        model.open_remote_manage_overlay();
        assert_eq!(
            model.handle_remote_manage_key(press(KeyCode::Char('a'))),
            RemoteManageOutcome::OpenAddRemote
        );
        assert!(model.add_remote_form().is_some());
    }

    #[test]
    fn global_menu_includes_manage_remotes() {
        let mut model = ClientSupervisorModel::new("local");
        assert!(model.client_global_menu_items().contains(&"manage remotes"));
        let action = model.select_client_global_menu_item(5);
        assert_eq!(action, Some(ClientGlobalMenuAction::ManageRemotes));
        assert!(model.remote_manage_overlay().is_some());
    }
}
