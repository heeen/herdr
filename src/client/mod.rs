//! Thin client mode — connects to the server's client socket.
//!
//! The client:
//! - Connects to `herdr-client.sock`, sends Hello with terminal size and protocol version
//! - Sets up the real terminal (raw mode, mouse capture, keyboard enhancements)
//! - Receives Frame messages and blits them to the terminal (diff against last frame)
//! - Reads stdin events (keystrokes, mouse, paste) and sends them as ClientMessage::Input
//! - Detects terminal resize and sends ClientMessage::Resize
//! - Restores terminal on exit (normal or error)
//! - Handles ServerShutdown gracefully (clean exit, informative message to stderr)
//! - Handles server unreachable (clear error screen, not blank/hang)
//! - Forwards OSC 52 clipboard writes from server to its own stdout
//! - Displays sound/toast notifications forwarded from server

mod compositor;
mod input;
mod supervisor;

use std::collections::{HashMap, HashSet};
use std::io::{self, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, KeyCode, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use tracing::{debug, info, warn};

use crate::protocol::render_ansi;
use crate::protocol::{
    self, AttachScrollDirection, AttachScrollSource, ClientKeybindings, ClientMessage,
    ClientSurfaceMode, NotifyKind, RenderEncoding, ServerMessage, MAX_CLIPBOARD_IMAGE_PAYLOAD,
    MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION,
};
use crate::server::socket_paths::client_socket_path;

static RECEIVED_KITTY_GRAPHICS_IDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
const CLIENT_SUPERVISOR_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const ADD_REMOTE_TARGET_VALIDATE_TIMEOUT: Duration = Duration::from_secs(5);
const ADD_REMOTE_TARGET_VALIDATE_RETRY_DELAY: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------
// Client state
// ---------------------------------------------------------------------------

/// State tracking for the thin client.
struct ClientState {
    /// Stateful semantic-frame encoder used when the server sends FrameData.
    blit_encoder: render_ansi::BlitEncoder,
    /// Whether host mouse capture is currently active.
    mouse_capture_active: bool,
    /// The terminal size we reported to the server in our last Hello/Resize.
    reported_size: (u16, u16),
    /// The outer terminal size owned by the client compositor.
    host_size: (u16, u16),
    /// Last known host cell size in pixels, used for secondary handshakes.
    cell_size_px: (u32, u32),
    /// Client-local sound playback config, refreshed on server request.
    sound_config: crate::config::SoundConfig,
    /// Whether this client may write Kitty graphics bytes to its host terminal.
    kitty_graphics_enabled: bool,
    /// Direct attach prefix escape state. None for full-app clients.
    attach_escape: Option<AttachEscapeState>,
    /// Rows scrolled for one direct-attach wheel notch.
    mouse_scroll_lines: usize,
    /// Whether outer focus gain should force a full host-terminal redraw.
    redraw_on_focus_gained: bool,
    /// Client-owned sidebar/frame compositor used for mixed-server sessions.
    compositor: Option<compositor::ClientCompositor>,
    /// Runtime multi-server summary state used by the client-owned sidebar.
    supervisor_model: Option<supervisor::ClientSupervisorModel>,
    /// Last time the client refreshed sidebar summaries through API polling.
    last_supervisor_summary_refresh: Instant,
    /// Last semantic frame received from each connected server stream.
    frame_cache: HashMap<supervisor::ServerId, protocol::FrameData>,
    /// Servers with active summary-event subscription workers.
    summary_subscription_server_ids: HashSet<supervisor::ServerId>,
    /// SSH bridges owned by this client for secondary servers.
    ssh_bridges: HashMap<supervisor::ServerId, crate::remote::RemoteBridge>,
    /// Backoff state for secondary servers that should be reconnected.
    secondary_retries: HashMap<supervisor::ServerId, SecondaryRetryState>,
}

#[derive(Debug, Clone, Copy)]
struct SecondaryRetryState {
    attempt: usize,
    next_retry_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClientRenderPlan {
    surface_mode: ClientSurfaceMode,
    requested_encoding: RenderEncoding,
    server_size: (u16, u16),
    use_client_compositor: bool,
}

#[derive(Debug, Default)]
struct AttachEscapeState {
    pending_prefix: bool,
}

#[derive(Debug)]
enum AttachInputAction {
    Forward(Vec<u8>),
    Scroll {
        source: AttachScrollSource,
        direction: AttachScrollDirection,
        lines: u16,
        column: Option<u16>,
        row: Option<u16>,
        modifiers: u8,
    },
    Detach,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClientApiRefreshPolicy {
    Immediate,
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClientInputDispatch {
    Forward(Vec<u8>),
    ServerControl {
        server_id: supervisor::ServerId,
        message: ClientMessage,
    },
    ApiRequest {
        server_id: supervisor::ServerId,
        refresh: ClientApiRefreshPolicy,
        request: Box<crate::api::schema::Request>,
    },
    AddRemote(supervisor::AddRemoteDraft),
    Resize {
        cols: u16,
        rows: u16,
    },
    DetachAll,
    Redraw,
    Consumed,
}

impl AttachEscapeState {
    fn filter_input(
        &mut self,
        data: Vec<u8>,
        viewport_rows: u16,
        mouse_scroll_lines: usize,
    ) -> AttachInputAction {
        const PREFIX: u8 = 0x02; // Ctrl+B

        let mut output = Vec::with_capacity(data.len());
        for byte in data {
            if self.pending_prefix {
                self.pending_prefix = false;
                match byte {
                    b'q' => return AttachInputAction::Detach,
                    PREFIX => output.push(PREFIX),
                    other => {
                        output.push(PREFIX);
                        output.push(other);
                    }
                }
                continue;
            }

            if byte == PREFIX {
                self.pending_prefix = true;
            } else {
                output.push(byte);
            }
        }

        if output.is_empty() {
            AttachInputAction::None
        } else if let Some(action) =
            attach_scroll_action(&output, viewport_rows, mouse_scroll_lines)
        {
            action
        } else {
            AttachInputAction::Forward(output)
        }
    }
}

fn attach_scroll_action(
    data: &[u8],
    viewport_rows: u16,
    mouse_scroll_lines: usize,
) -> Option<AttachInputAction> {
    let mut events = crate::raw_input::parse_raw_input_bytes_sync(data);
    if events.len() != 1 {
        return None;
    }

    match events.pop()? {
        crate::raw_input::RawInputEvent::Mouse(mouse) => {
            let direction = match mouse.kind {
                MouseEventKind::ScrollUp => AttachScrollDirection::Up,
                MouseEventKind::ScrollDown => AttachScrollDirection::Down,
                _ => return Some(AttachInputAction::None),
            };
            Some(AttachInputAction::Scroll {
                source: AttachScrollSource::Wheel,
                direction,
                lines: mouse_scroll_lines.max(1).min(u16::MAX as usize) as u16,
                column: Some(mouse.column),
                row: Some(mouse.row),
                modifiers: mouse.modifiers.bits(),
            })
        }
        crate::raw_input::RawInputEvent::Key(key)
            if key.modifiers.is_empty()
                && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
        {
            let direction = match key.code {
                KeyCode::PageUp => AttachScrollDirection::Up,
                KeyCode::PageDown => AttachScrollDirection::Down,
                _ => return None,
            };
            Some(AttachInputAction::Scroll {
                source: AttachScrollSource::PageKey {
                    input: data.to_vec(),
                },
                direction,
                lines: viewport_rows.saturating_sub(1).max(1),
                column: None,
                row: None,
                modifiers: KeyModifiers::empty().bits(),
            })
        }
        crate::raw_input::RawInputEvent::Key(key)
            if key.modifiers.is_empty()
                && key.kind == KeyEventKind::Release
                && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) =>
        {
            Some(AttachInputAction::None)
        }
        _ => None,
    }
}

fn dispatch_composited_input(
    data: Vec<u8>,
    compositor: &mut compositor::ClientCompositor,
    model: &mut supervisor::ClientSupervisorModel,
    host_size: (u16, u16),
) -> ClientInputDispatch {
    if model.add_remote_form().is_some() || model.client_global_menu_highlighted().is_some() {
        return dispatch_client_overlay_input(data, compositor, model, host_size);
    }

    let events = crate::raw_input::parse_raw_input_bytes_sync(&data);
    let [crate::raw_input::RawInputEvent::Mouse(mouse)] = events.as_slice() else {
        return ClientInputDispatch::Forward(data);
    };

    dispatch_composited_mouse_input(data, compositor, model, host_size, mouse)
}

fn dispatch_client_overlay_input(
    data: Vec<u8>,
    compositor: &mut compositor::ClientCompositor,
    model: &mut supervisor::ClientSupervisorModel,
    host_size: (u16, u16),
) -> ClientInputDispatch {
    let events = crate::raw_input::parse_raw_input_bytes_sync(&data);
    if events.is_empty() {
        return ClientInputDispatch::Consumed;
    }

    let mut dispatch = ClientInputDispatch::Consumed;
    for event in events {
        let next = match event {
            crate::raw_input::RawInputEvent::Key(key) if model.add_remote_form().is_some() => {
                match model.handle_add_remote_key(key) {
                    supervisor::AddRemoteFormOutcome::Redraw => ClientInputDispatch::Redraw,
                    supervisor::AddRemoteFormOutcome::Submit(draft) => {
                        ClientInputDispatch::AddRemote(draft)
                    }
                }
            }
            crate::raw_input::RawInputEvent::Paste(text) if model.add_remote_form().is_some() => {
                match model.append_add_remote_paste(&text) {
                    supervisor::AddRemoteFormOutcome::Redraw => ClientInputDispatch::Redraw,
                    supervisor::AddRemoteFormOutcome::Submit(draft) => {
                        ClientInputDispatch::AddRemote(draft)
                    }
                }
            }
            crate::raw_input::RawInputEvent::Key(key)
                if model.client_global_menu_highlighted().is_some() =>
            {
                dispatch_client_global_menu_key(model, key)
            }
            crate::raw_input::RawInputEvent::Mouse(mouse) => {
                dispatch_composited_mouse_input(data.clone(), compositor, model, host_size, &mouse)
            }
            _ => ClientInputDispatch::Consumed,
        };

        if matches!(
            next,
            ClientInputDispatch::AddRemote(_)
                | ClientInputDispatch::ApiRequest { .. }
                | ClientInputDispatch::ServerControl { .. }
                | ClientInputDispatch::Resize { .. }
                | ClientInputDispatch::DetachAll
        ) {
            dispatch = next;
            break;
        }
        if matches!(next, ClientInputDispatch::Redraw) {
            dispatch = ClientInputDispatch::Redraw;
        }
    }
    dispatch
}

fn dispatch_client_global_menu_key(
    model: &mut supervisor::ClientSupervisorModel,
    key: crate::input::TerminalKey,
) -> ClientInputDispatch {
    if !matches!(
        key.kind,
        crossterm::event::KeyEventKind::Press | crossterm::event::KeyEventKind::Repeat
    ) {
        return ClientInputDispatch::Consumed;
    }

    match key.code {
        KeyCode::Esc => {
            model.close_client_overlay();
            ClientInputDispatch::Redraw
        }
        KeyCode::Up | KeyCode::Char('k') => {
            model.move_client_global_menu_prev();
            ClientInputDispatch::Redraw
        }
        KeyCode::Down | KeyCode::Char('j') => {
            model.move_client_global_menu_next();
            ClientInputDispatch::Redraw
        }
        KeyCode::Enter => {
            let action = model.accept_client_global_menu_item();
            dispatch_client_global_menu_action(model, action)
        }
        _ => ClientInputDispatch::Consumed,
    }
}

fn dispatch_client_global_menu_action(
    model: &mut supervisor::ClientSupervisorModel,
    action: Option<supervisor::ClientGlobalMenuAction>,
) -> ClientInputDispatch {
    match action {
        Some(supervisor::ClientGlobalMenuAction::Settings) => {
            model.activate_main_server();
            ClientInputDispatch::ServerControl {
                server_id: supervisor::ServerId::main(),
                message: ClientMessage::OpenSettings,
            }
        }
        Some(supervisor::ClientGlobalMenuAction::Keybinds) => {
            model.activate_main_server();
            ClientInputDispatch::ServerControl {
                server_id: supervisor::ServerId::main(),
                message: ClientMessage::OpenKeybindHelp,
            }
        }
        Some(supervisor::ClientGlobalMenuAction::ReloadConfig) => ClientInputDispatch::ApiRequest {
            server_id: supervisor::ServerId::main(),
            refresh: ClientApiRefreshPolicy::Immediate,
            request: Box::new(crate::api::schema::Request {
                id: "client:reload-config".into(),
                method: crate::api::schema::Method::ServerReloadConfig(
                    crate::api::schema::EmptyParams::default(),
                ),
            }),
        },
        Some(supervisor::ClientGlobalMenuAction::Detach) => ClientInputDispatch::DetachAll,
        Some(supervisor::ClientGlobalMenuAction::AddRemote) => ClientInputDispatch::Redraw,
        None => ClientInputDispatch::Consumed,
    }
}

fn dispatch_composited_mouse_input(
    data: Vec<u8>,
    compositor: &mut compositor::ClientCompositor,
    model: &mut supervisor::ClientSupervisorModel,
    host_size: (u16, u16),
    mouse: &MouseEvent,
) -> ClientInputDispatch {
    if let Some((cols, rows)) =
        compositor.handle_sidebar_resize_mouse(mouse, host_size.0, host_size.1, model.ui_settings())
    {
        return if matches!(mouse.kind, MouseEventKind::Drag(MouseButton::Left)) {
            ClientInputDispatch::Resize { cols, rows }
        } else {
            ClientInputDispatch::Redraw
        };
    }

    if let Some(target) =
        compositor.hit_test(model, mouse.column, mouse.row, host_size.0, host_size.1)
    {
        return dispatch_sidebar_hit_target(target, model, mouse);
    }

    let sidebar_width = compositor.sidebar_width().min(host_size.0);
    if mouse.column < sidebar_width {
        return ClientInputDispatch::Consumed;
    }

    translate_content_mouse_input(data, mouse, sidebar_width)
}

fn dispatch_sidebar_hit_target(
    target: compositor::SidebarHitTarget,
    model: &mut supervisor::ClientSupervisorModel,
    mouse: &MouseEvent,
) -> ClientInputDispatch {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return ClientInputDispatch::Consumed;
    }

    match target {
        compositor::SidebarHitTarget::Filter => {
            model.cycle_filter();
            ClientInputDispatch::Redraw
        }
        compositor::SidebarHitTarget::Workspace {
            server_id,
            workspace_id,
        } => model
            .focus_workspace_route(&server_id, &workspace_id)
            .api_request("client:workspace-focus")
            .map(|request| ClientInputDispatch::ApiRequest {
                server_id,
                refresh: ClientApiRefreshPolicy::Deferred,
                request: Box::new(request),
            })
            .unwrap_or(ClientInputDispatch::Consumed),
        compositor::SidebarHitTarget::Agent {
            server_id,
            agent_id,
        } => model
            .focus_agent_route(&server_id, &agent_id)
            .api_request("client:agent-focus")
            .map(|request| ClientInputDispatch::ApiRequest {
                server_id,
                refresh: ClientApiRefreshPolicy::Deferred,
                request: Box::new(request),
            })
            .unwrap_or(ClientInputDispatch::Consumed),
        compositor::SidebarHitTarget::New => match model.open_new_workspace_picker() {
            route @ supervisor::NewWorkspaceRoute::CreateOn(_) => route
                .api_request("client:workspace-create")
                .map(|(server_id, request)| ClientInputDispatch::ApiRequest {
                    server_id,
                    refresh: ClientApiRefreshPolicy::Immediate,
                    request: Box::new(request),
                })
                .unwrap_or(ClientInputDispatch::Consumed),
            supervisor::NewWorkspaceRoute::PickDestination(_) => ClientInputDispatch::Redraw,
            supervisor::NewWorkspaceRoute::Unavailable { .. } => ClientInputDispatch::Consumed,
        },
        compositor::SidebarHitTarget::NewWorkspaceDestination { server_id } => model
            .choose_new_workspace_destination(&server_id)
            .api_request("client:workspace-create")
            .map(|(server_id, request)| ClientInputDispatch::ApiRequest {
                server_id,
                refresh: ClientApiRefreshPolicy::Immediate,
                request: Box::new(request),
            })
            .unwrap_or(ClientInputDispatch::Consumed),
        compositor::SidebarHitTarget::ClientGlobalMenuItem { index } => {
            let action = model.select_client_global_menu_item(index);
            dispatch_client_global_menu_action(model, action)
        }
        compositor::SidebarHitTarget::Menu => {
            model.open_client_global_menu();
            ClientInputDispatch::Redraw
        }
    }
}

fn translate_content_mouse_input(
    original: Vec<u8>,
    mouse: &MouseEvent,
    sidebar_width: u16,
) -> ClientInputDispatch {
    let Some(column) = mouse.column.checked_sub(sidebar_width) else {
        return ClientInputDispatch::Consumed;
    };

    let encoded = match mouse.kind {
        MouseEventKind::ScrollUp
        | MouseEventKind::ScrollDown
        | MouseEventKind::ScrollLeft
        | MouseEventKind::ScrollRight => crate::input::encode_mouse_scroll(
            mouse.kind,
            column,
            mouse.row,
            mouse.modifiers,
            crate::input::MouseProtocolEncoding::Sgr,
        ),
        MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_) => {
            crate::input::encode_mouse_button(
                mouse.kind,
                column,
                mouse.row,
                mouse.modifiers,
                crate::input::MouseProtocolEncoding::Sgr,
            )
        }
        MouseEventKind::Moved => None,
    };

    ClientInputDispatch::Forward(encoded.unwrap_or(original))
}

impl ClientState {
    fn request_full_redraw(&mut self) {
        self.blit_encoder = render_ansi::BlitEncoder::new();
    }
}

fn client_render_plan(
    supervisor_model: Option<&supervisor::ClientSupervisorModel>,
    requested_encoding: RenderEncoding,
    host_size: (u16, u16),
) -> ClientRenderPlan {
    let use_client_compositor = supervisor_model.is_some();
    if use_client_compositor {
        let compositor = compositor::ClientCompositor::default();
        return ClientRenderPlan {
            surface_mode: ClientSurfaceMode::EmbeddedContent,
            requested_encoding: RenderEncoding::SemanticFrame,
            server_size: compositor.content_size(host_size.0, host_size.1),
            use_client_compositor,
        };
    }

    ClientRenderPlan {
        surface_mode: ClientSurfaceMode::FullApp,
        requested_encoding,
        server_size: host_size,
        use_client_compositor: false,
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Errors that can occur during client operation.
#[derive(Debug)]
pub enum ClientError {
    /// Could not connect to the server's client socket.
    ConnectionFailed(io::Error),
    /// Server rejected our handshake.
    HandshakeRejected { version: u32, error: String },
    /// Server shut down.
    ServerShutdown { reason: Option<String> },
    /// Lost connection to the server.
    ConnectionLost(io::Error),
    /// Protocol error (framing, deserialization).
    Protocol(protocol::FramingError),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::ConnectionFailed(err) => {
                write!(f, "failed to connect to server: {err}")?;
                let path = client_socket_path();
                write!(
                    f,
                    "\nIs herdr server running? Start it with `herdr server`."
                )?;
                write!(f, "\nSocket path: {}", path.display())
            }
            ClientError::HandshakeRejected { version, error } => {
                write!(f, "server rejected handshake (version {version}): {error}")
            }
            ClientError::ServerShutdown { reason } => {
                match reason.as_deref() {
                    Some("detached") => {
                        if let Ok(reattach_command) =
                            std::env::var(crate::remote::REATTACH_COMMAND_ENV_VAR)
                        {
                            write!(f, "detached from remote server")?;
                            write!(f, "\nRun `{reattach_command}` to reattach")?;
                        } else {
                            write!(f, "detached from server")?;
                            write!(
                                f,
                                "\nRun `{}` to reattach",
                                crate::session::local_attach_command()
                            )?;
                        }
                    }
                    _ => {
                        write!(f, "server shut down")?;
                        if let Some(reason) = reason {
                            write!(f, ": {reason}")?;
                        }
                    }
                }
                Ok(())
            }
            ClientError::ConnectionLost(err) => {
                write!(f, "lost connection to server: {err}")
            }
            ClientError::Protocol(err) => {
                write!(f, "protocol error: {err}")
            }
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::ConnectionFailed(err) => Some(err),
            ClientError::ConnectionLost(err) => Some(err),
            ClientError::Protocol(err) => Some(err),
            _ => None,
        }
    }
}

impl From<protocol::FramingError> for ClientError {
    fn from(err: protocol::FramingError) -> Self {
        ClientError::Protocol(err)
    }
}

// ---------------------------------------------------------------------------
// Terminal setup / restore
// ---------------------------------------------------------------------------

/// Sets up the terminal for client mode (raw mode, optional mouse, keyboard enhancements).
///
/// Returns a guard that restores the terminal when dropped.
fn setup_terminal(mouse_capture: bool) -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(true, mouse_capture)
}

/// Sets up a direct attach terminal.
///
/// Direct attach forwards stdin to the attached PTY. It enables mouse capture
/// so wheel events can drive the attached viewport or be forwarded to child
/// programs that requested mouse input.
fn setup_direct_attach_terminal() -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(false, true)
}

fn setup_terminal_with_capabilities(
    enable_client_protocols: bool,
    mouse_capture: bool,
) -> io::Result<TerminalGuard> {
    ratatui::init();

    if enable_client_protocols {
        if mouse_capture {
            execute!(io::stdout(), EnableMouseCapture)?;
        } else {
            execute!(io::stdout(), DisableMouseCapture)?;
        }
        execute!(
            io::stdout(),
            EnableBracketedPaste,
            EnableFocusChange,
            PushKeyboardEnhancementFlags(crate::input::ime_compatible_keyboard_enhancement_flags())
        )?;
    } else if mouse_capture {
        execute!(io::stdout(), EnableMouseCapture)?;
    } else {
        execute!(io::stdout(), DisableMouseCapture)?;
    }

    let modify_other_keys_mode = enable_client_protocols
        .then(|| {
            crate::input::host_modify_other_keys_mode(
                std::env::var("TMUX").is_ok(),
                std::env::var("TERM_PROGRAM").ok().as_deref(),
                std::env::var_os("WEZTERM_PANE").is_some(),
            )
        })
        .flatten();
    if let Some(mode) = modify_other_keys_mode {
        io::stdout().write_all(mode.set_sequence())?;
        io::stdout().flush()?;
    }

    Ok(TerminalGuard {
        reset_modify_other_keys: modify_other_keys_mode.is_some(),
    })
}

/// Guard that restores the terminal when dropped.
struct TerminalGuard {
    reset_modify_other_keys: bool,
}

fn write_terminal_restore_postlude(writer: &mut impl io::Write) -> io::Result<()> {
    // Restore a visible cursor and reset DECSCUSR back to the terminal default.
    writer.write_all(b"\x1b[?25h\x1b[0 q")?;
    writer.flush()
}

fn set_mouse_capture(enabled: bool) -> io::Result<()> {
    if enabled {
        execute!(io::stdout(), EnableMouseCapture)
    } else {
        execute!(io::stdout(), DisableMouseCapture)
    }
}

fn desired_mouse_capture(server_enabled: bool, client_compositor_enabled: bool) -> bool {
    server_enabled || client_compositor_enabled
}

fn restore_terminal_state(reset_modify_other_keys: bool) {
    let _ = clear_received_kitty_graphics(&mut io::stdout());

    // Reset modifyOtherKeys if we enabled it.
    if reset_modify_other_keys {
        let _ = io::stdout().write_all(b"\x1b[>4;0m");
        let _ = io::stdout().flush();
    }

    let _ = execute!(
        io::stdout(),
        PopKeyboardEnhancementFlags,
        DisableFocusChange,
        DisableBracketedPaste,
        DisableMouseCapture
    );
    ratatui::restore();
    let _ = write_terminal_restore_postlude(&mut io::stdout());
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal_state(self.reset_modify_other_keys);
    }
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

fn requested_render_encoding() -> RenderEncoding {
    match std::env::var("HERDR_RENDER_ENCODING").ok().as_deref() {
        Some("terminal-ansi" | "terminal_ansi" | "ansi") => RenderEncoding::TerminalAnsi,
        _ => RenderEncoding::SemanticFrame,
    }
}

fn requested_keybindings() -> ClientKeybindings {
    match std::env::var(crate::remote::REMOTE_KEYBINDINGS_ENV_VAR)
        .ok()
        .as_deref()
    {
        Some("local") => crate::config::Config::load()
            .config
            .local_keybindings_profile_toml()
            .map(|keys_toml| ClientKeybindings::Local { keys_toml })
            .unwrap_or(ClientKeybindings::Server),
        _ => ClientKeybindings::Server,
    }
}

/// Performs the client→server handshake.
///
/// Sends Hello with the terminal size and protocol version, reads the Welcome
/// response. Returns Ok(()) on success, or an error if the server rejects us.
fn do_handshake(
    stream: &mut UnixStream,
    cols: u16,
    rows: u16,
    cell_width_px: u32,
    cell_height_px: u32,
    requested_encoding: RenderEncoding,
    surface_mode: ClientSurfaceMode,
    keybindings: ClientKeybindings,
) -> Result<RenderEncoding, ClientError> {
    stream
        .set_nonblocking(false)
        .map_err(ClientError::ConnectionFailed)?;

    // Send Hello.
    let hello = build_hello_message(
        cols,
        rows,
        cell_width_px,
        cell_height_px,
        requested_encoding,
        surface_mode,
        keybindings,
    );
    protocol::write_message(stream, &hello)
        .map_err(|e| ClientError::ConnectionFailed(io::Error::other(e.to_string())))?;

    // Read Welcome.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(ClientError::ConnectionFailed)?;
    let welcome: ServerMessage = protocol::read_message(stream, MAX_FRAME_SIZE)?;
    stream
        .set_read_timeout(None)
        .map_err(ClientError::ConnectionFailed)?;

    match welcome {
        ServerMessage::Welcome {
            version,
            encoding,
            error,
        } => {
            if let Some(error) = error {
                return Err(ClientError::HandshakeRejected { version, error });
            }
            info!(version, ?encoding, "handshake succeeded");
            Ok(encoding)
        }
        _ => Err(ClientError::Protocol(protocol::FramingError::Io(
            io::Error::new(io::ErrorKind::InvalidData, "expected Welcome message"),
        ))),
    }
}

fn build_hello_message(
    cols: u16,
    rows: u16,
    cell_width_px: u32,
    cell_height_px: u32,
    requested_encoding: RenderEncoding,
    surface_mode: ClientSurfaceMode,
    keybindings: ClientKeybindings,
) -> ClientMessage {
    ClientMessage::Hello {
        version: PROTOCOL_VERSION,
        cols,
        rows,
        cell_width_px,
        cell_height_px,
        requested_encoding,
        surface_mode,
        keybindings,
    }
}

// ---------------------------------------------------------------------------
// Client event loop
// ---------------------------------------------------------------------------

/// Internal events for the client event loop.
enum ClientLoopEvent {
    /// Raw input bytes from stdin.
    StdinInput(Vec<u8>),
    /// Terminal resize detected.
    Resize(u16, u16, u32, u32),
    /// Server message received.
    ServerMessage {
        server_id: supervisor::ServerId,
        message: ServerMessage,
    },
    /// A subscribed sidebar-summary event arrived from one managed server.
    SupervisorSummaryChanged(supervisor::ServerId),
    /// A sidebar-summary subscription worker ended and should be eligible to restart.
    SupervisorSummarySubscriptionEnded(supervisor::ServerId),
    /// Server reader thread exited (connection lost).
    ServerDisconnected(supervisor::ServerId),
    /// Timer tick.
    Timer,
}

struct SummarySubscriptionEndGuard {
    server_id: supervisor::ServerId,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
}

impl Drop for SummarySubscriptionEndGuard {
    fn drop(&mut self) {
        let _ = self
            .event_tx
            .blocking_send(ClientLoopEvent::SupervisorSummarySubscriptionEnded(
                self.server_id.clone(),
            ));
    }
}

struct ClientLoopOptions {
    host_size: (u16, u16),
    reported_size: (u16, u16),
    cell_size_px: (u32, u32),
    sound_config: crate::config::SoundConfig,
    mouse_scroll_lines: usize,
    redraw_on_focus_gained: bool,
    kitty_graphics_enabled: bool,
    mouse_capture_active: bool,
    negotiated_encoding: RenderEncoding,
    attach_escape: Option<AttachEscapeState>,
    compositor: Option<compositor::ClientCompositor>,
    supervisor_model: Option<supervisor::ClientSupervisorModel>,
    secondary_streams: Vec<(supervisor::ServerId, UnixStream)>,
    ssh_bridges: HashMap<supervisor::ServerId, crate::remote::RemoteBridge>,
}

/// Runs the thin client: connects to the server, performs the handshake,
/// and enters the main event loop.
///
/// This is the entry point called from `main.rs` when running in client mode.
pub fn run_client() -> io::Result<()> {
    run_client_with_mode(
        requested_render_encoding(),
        None,
        None,
        "connecting to server",
    )
}

/// Runs a direct terminal attach client.
pub fn run_terminal_attach(terminal_id: String, takeover: bool) -> io::Result<()> {
    run_client_with_mode(
        RenderEncoding::TerminalAnsi,
        Some((terminal_id, takeover)),
        Some(AttachEscapeState::default()),
        "attaching to terminal",
    )
}

fn run_client_with_mode(
    requested_encoding: RenderEncoding,
    attach_request: Option<(String, bool)>,
    attach_escape: Option<AttachEscapeState>,
    log_message: &'static str,
) -> io::Result<()> {
    init_logging();

    let loaded_config = crate::config::Config::load();
    let mouse_scroll_lines = loaded_config.config.ui.mouse_scroll_lines();
    let redraw_on_focus_gained = loaded_config.config.ui.redraw_on_focus_gained;
    let sound_config = loaded_config.config.ui.sound;
    let direct_attach_requested = attach_request.is_some();
    let kitty_graphics_enabled =
        loaded_config.config.experimental.kitty_graphics && !direct_attach_requested;

    let socket_path = client_socket_path();
    crate::logging::startup("client");
    info!(path = %socket_path.display(), "{log_message}");

    // Get the terminal geometry before handshake (before raw mode).
    let (cols, rows, cell_width_px, cell_height_px) =
        current_terminal_geometry(kitty_graphics_enabled);

    let mut supervisor_model = if direct_attach_requested {
        None
    } else {
        let mut api = crate::api::client::ApiClient::local();
        match bootstrap_supervisor_for_client(false, &mut api) {
            Ok(Some(mut model)) => {
                model.refresh_local_secondary_summaries_from_api();
                Some(model)
            }
            Ok(None) => None,
            Err(err) => {
                warn!(err = %err, "failed to bootstrap client supervisor from main API");
                None
            }
        }
    };
    if let Some(model) = &supervisor_model {
        debug!(
            secondary_servers = model.secondary_connection_plans().len(),
            workspace_rows = model.workspace_rows().len(),
            "client supervisor bootstrapped"
        );
    }

    let render_plan =
        client_render_plan(supervisor_model.as_ref(), requested_encoding, (cols, rows));

    // Try to connect to the server.
    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(err) => {
            // Server unreachable — show clear error and exit.
            let client_err = ClientError::ConnectionFailed(err);
            eprintln!("herdr: {client_err}");
            std::process::exit(1);
        }
    };

    // Perform handshake while the stream is still in blocking mode.
    let negotiated_encoding = match do_handshake(
        &mut stream,
        render_plan.server_size.0,
        render_plan.server_size.1,
        cell_width_px,
        cell_height_px,
        render_plan.requested_encoding,
        render_plan.surface_mode,
        requested_keybindings(),
    ) {
        Ok(encoding) => encoding,
        Err(err) => {
            eprintln!("herdr: {err}");
            std::process::exit(1);
        }
    };

    let mut ssh_bridges = HashMap::new();
    let secondary_streams = if render_plan.use_client_compositor {
        supervisor_model
            .as_mut()
            .map(|model| {
                connect_secondary_client_streams(
                    model,
                    render_plan.server_size,
                    cell_width_px,
                    cell_height_px,
                    &mut ssh_bridges,
                )
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    if let Some((terminal_id, takeover)) = attach_request {
        let attach = ClientMessage::AttachTerminal {
            terminal_id,
            takeover,
        };
        if let Err(err) = write_to_server(&mut stream, &attach) {
            eprintln!("herdr: failed to request terminal attach: {err}");
            std::process::exit(1);
        }
    }

    // Now set up the terminal. This must happen AFTER the handshake succeeds,
    // so we don't leave the terminal in raw mode if the server rejects us.
    let direct_attach = attach_escape.is_some();
    let client_compositor_enabled = render_plan.use_client_compositor;
    let _guard = if direct_attach {
        setup_direct_attach_terminal()
    } else {
        setup_terminal(client_compositor_enabled)
    }
    .map_err(|err| {
        eprintln!("herdr: failed to set up terminal: {err}");
        err
    })?;

    // Install a panic hook to restore the terminal on panic (same as monolithic).
    let in_tmux = std::env::var("TMUX").is_ok();
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal_state(in_tmux);
        original_hook(info);
    }));

    // Create the tokio runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;

    let should_quit = Arc::new(AtomicBool::new(false));

    // Install Ctrl+C handler.
    let quit_flag = should_quit.clone();
    let _ = ctrlc::set_handler(move || {
        quit_flag.store(true, Ordering::Release);
    });

    let result = rt.block_on(async {
        let client_compositor = render_plan
            .use_client_compositor
            .then(compositor::ClientCompositor::default);
        run_client_loop(
            stream,
            should_quit,
            ClientLoopOptions {
                host_size: (cols, rows),
                reported_size: render_plan.server_size,
                cell_size_px: (cell_width_px, cell_height_px),
                sound_config,
                mouse_scroll_lines,
                redraw_on_focus_gained,
                kitty_graphics_enabled,
                mouse_capture_active: client_compositor_enabled,
                negotiated_encoding,
                attach_escape,
                compositor: client_compositor,
                supervisor_model: supervisor_model.take(),
                secondary_streams,
                ssh_bridges,
            },
        )
        .await
    });

    // Restore the terminal before printing any final status message.
    drop(_guard);

    if let Err(err) = result {
        eprintln!("herdr: {err}");
        rt.shutdown_timeout(Duration::from_millis(100));
        crate::logging::shutdown("client");

        if matches!(
            err,
            ClientError::ServerShutdown {
                reason: Some(reason)
            } if reason == "detached"
        ) {
            return Ok(());
        }

        std::process::exit(1);
    }

    rt.shutdown_timeout(Duration::from_millis(100));
    crate::logging::shutdown("client");
    Ok(())
}

fn bootstrap_supervisor_for_client(
    direct_attach_requested: bool,
    api: &mut impl supervisor::SupervisorApi,
) -> Result<Option<supervisor::ClientSupervisorModel>, String> {
    if direct_attach_requested {
        return Ok(None);
    }

    supervisor::bootstrap_from_main_api(api, main_display_name_for_client()).map(Some)
}

fn main_display_name_for_client() -> String {
    std::env::var(crate::remote::MAIN_DISPLAY_NAME_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "local".to_string())
}

fn api_target_for_supervisor_server(
    model: &supervisor::ClientSupervisorModel,
    server_id: &supervisor::ServerId,
    ssh_bridges: &HashMap<supervisor::ServerId, crate::remote::RemoteBridge>,
) -> Option<crate::api::client::ConnectionTarget> {
    let target = model.server_connection_target(server_id)?;
    api_target_for_supervisor_target(server_id, &target, ssh_bridges)
}

fn api_target_for_supervisor_target(
    server_id: &supervisor::ServerId,
    target: &supervisor::ServerConnectionTarget,
    ssh_bridges: &HashMap<supervisor::ServerId, crate::remote::RemoteBridge>,
) -> Option<crate::api::client::ConnectionTarget> {
    match target {
        supervisor::ServerConnectionTarget::Ssh(_) => ssh_bridges.get(server_id).map(|bridge| {
            crate::api::client::ConnectionTarget::SocketPath(bridge.api_socket_path().to_path_buf())
        }),
        _ => api_target_for_connection_target(target),
    }
}

fn api_target_for_connection_target(
    target: &supervisor::ServerConnectionTarget,
) -> Option<crate::api::client::ConnectionTarget> {
    match target {
        supervisor::ServerConnectionTarget::Main => {
            Some(crate::api::client::ConnectionTarget::LocalSession(None))
        }
        supervisor::ServerConnectionTarget::LocalSession(session) => Some(
            crate::api::client::ConnectionTarget::LocalSession(session.clone()),
        ),
        supervisor::ServerConnectionTarget::Ssh(_) => None,
    }
}

fn client_socket_path_for_connection_target(
    target: &supervisor::ServerConnectionTarget,
) -> Option<std::path::PathBuf> {
    match target {
        supervisor::ServerConnectionTarget::Main => Some(client_socket_path()),
        supervisor::ServerConnectionTarget::LocalSession(session) => {
            Some(crate::session::client_socket_path_for(session.as_deref()))
        }
        supervisor::ServerConnectionTarget::Ssh(_) => None,
    }
}

#[cfg(test)]
fn client_socket_path_for_supervisor_server(
    model: &supervisor::ClientSupervisorModel,
    server_id: &supervisor::ServerId,
    ssh_bridges: &HashMap<supervisor::ServerId, crate::remote::RemoteBridge>,
) -> Option<std::path::PathBuf> {
    let target = model.server_connection_target(server_id)?;
    match target {
        supervisor::ServerConnectionTarget::Ssh(_) => ssh_bridges
            .get(server_id)
            .map(|bridge| bridge.client_socket_path().to_path_buf()),
        _ => client_socket_path_for_connection_target(&target),
    }
}

fn connect_secondary_client_streams(
    model: &mut supervisor::ClientSupervisorModel,
    server_size: (u16, u16),
    cell_width_px: u32,
    cell_height_px: u32,
    ssh_bridges: &mut HashMap<supervisor::ServerId, crate::remote::RemoteBridge>,
) -> Vec<(supervisor::ServerId, UnixStream)> {
    let mut streams = Vec::new();
    for plan in model.secondary_connection_plans() {
        match connect_secondary_client_stream_for_plan(
            &plan,
            server_size,
            cell_width_px,
            cell_height_px,
            ssh_bridges,
        ) {
            Ok(stream) => {
                let _ = model
                    .set_connection_state(&plan.server_id, supervisor::ConnectionState::Connected);
                streams.push((plan.server_id, stream));
            }
            Err(err) => {
                let state = connection_state_from_client_error(&err);
                let _ = model.set_connection_state(&plan.server_id, state);
                ssh_bridges.remove(&plan.server_id);
                warn!(
                    server_id = ?plan.server_id,
                    err = %err,
                    "failed to connect secondary client stream"
                );
            }
        }
    }
    streams
}

fn connect_secondary_client_stream_for_plan(
    plan: &supervisor::SecondaryConnectionPlan,
    server_size: (u16, u16),
    cell_width_px: u32,
    cell_height_px: u32,
    ssh_bridges: &mut HashMap<supervisor::ServerId, crate::remote::RemoteBridge>,
) -> Result<UnixStream, ClientError> {
    let socket_path = match &plan.target {
        supervisor::ServerConnectionTarget::Ssh(target) => {
            if !ssh_bridges.contains_key(&plan.server_id) {
                let bridge = crate::remote::start_ssh_remote_bridge(target, None)
                    .map_err(ClientError::ConnectionFailed)?;
                ssh_bridges.insert(plan.server_id.clone(), bridge);
            }
            ssh_bridges
                .get(&plan.server_id)
                .map(|bridge| bridge.client_socket_path().to_path_buf())
                .ok_or_else(|| {
                    ClientError::ConnectionFailed(io::Error::new(
                        io::ErrorKind::NotFound,
                        "missing ssh bridge for secondary server",
                    ))
                })?
        }
        _ => client_socket_path_for_connection_target(&plan.target).ok_or_else(|| {
            ClientError::ConnectionFailed(io::Error::new(
                io::ErrorKind::InvalidInput,
                "secondary server has no client socket target",
            ))
        })?,
    };

    connect_secondary_client_stream(
        &socket_path,
        server_size,
        cell_width_px,
        cell_height_px,
        plan.keybindings,
    )
}

fn connect_secondary_client_stream(
    socket_path: &std::path::Path,
    server_size: (u16, u16),
    cell_width_px: u32,
    cell_height_px: u32,
    keybindings: crate::remote_registry::RemoteKeybindingsSnapshot,
) -> Result<UnixStream, ClientError> {
    let mut stream = UnixStream::connect(socket_path).map_err(ClientError::ConnectionFailed)?;
    do_handshake(
        &mut stream,
        server_size.0,
        server_size.1,
        cell_width_px,
        cell_height_px,
        RenderEncoding::SemanticFrame,
        ClientSurfaceMode::EmbeddedContent,
        client_keybindings_from_snapshot(keybindings),
    )?;
    Ok(stream)
}

fn attach_secondary_client_stream(
    server_id: supervisor::ServerId,
    stream: UnixStream,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    server_writes: &mut HashMap<supervisor::ServerId, UnixStream>,
) -> Result<(), ClientError> {
    let read_stream = stream.try_clone().map_err(ClientError::ConnectionFailed)?;
    let read_tx = event_tx.clone();
    let read_quit = should_quit.clone();
    let reader_server_id = server_id.clone();
    std::thread::spawn(move || {
        server_reader_thread(
            reader_server_id,
            read_stream,
            read_tx,
            &read_quit,
            MAX_FRAME_SIZE,
        );
    });
    stream
        .set_nonblocking(false)
        .map_err(ClientError::ConnectionFailed)?;
    server_writes.insert(server_id, stream);
    Ok(())
}

fn connection_state_from_client_error(err: &ClientError) -> supervisor::ConnectionState {
    match err {
        ClientError::HandshakeRejected { version, .. } => {
            supervisor::ConnectionState::ProtocolMismatch {
                server_protocol: Some(*version),
                client_protocol: PROTOCOL_VERSION,
            }
        }
        _ => supervisor::ConnectionState::Disconnected,
    }
}

fn client_keybindings_from_snapshot(
    keybindings: crate::remote_registry::RemoteKeybindingsSnapshot,
) -> ClientKeybindings {
    match keybindings {
        crate::remote_registry::RemoteKeybindingsSnapshot::Server => ClientKeybindings::Server,
        crate::remote_registry::RemoteKeybindingsSnapshot::Local => crate::config::Config::load()
            .config
            .local_keybindings_profile_toml()
            .map(|keys_toml| ClientKeybindings::Local { keys_toml })
            .unwrap_or(ClientKeybindings::Server),
    }
}

fn send_client_supervisor_request(
    model: &supervisor::ClientSupervisorModel,
    server_id: &supervisor::ServerId,
    request: crate::api::schema::Request,
    ssh_bridges: &HashMap<supervisor::ServerId, crate::remote::RemoteBridge>,
) -> Result<(), String> {
    let target = api_target_for_supervisor_server(model, server_id, ssh_bridges)
        .ok_or_else(|| format!("no API target for server {server_id:?}"))?;
    let api = crate::api::client::ApiClient::for_target(target);
    let value = api
        .request_value_with_timeout(&request, Duration::from_millis(500))
        .map_err(|err| err.to_string())?;
    crate::api::client::parse_response_value(value)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

fn submit_remote_add_to_main_api(
    api: &mut impl supervisor::SupervisorApi,
    draft: supervisor::AddRemoteDraft,
) -> Result<crate::remote_registry::RemoteDefinitionSnapshot, String> {
    let response = api
        .request(crate::api::schema::Request {
            id: "client:remote-add".into(),
            method: crate::api::schema::Method::RemoteAdd(crate::api::schema::RemoteAddParams {
                name: draft.name,
                target: draft.target,
                keybindings: draft.keybindings,
            }),
        })
        .map_err(|err| add_remote_error_message(&err))?;
    match response.result {
        crate::api::schema::ResponseResult::RemoteAdded { remote } => Ok(remote),
        other => Err(format!("remote.add returned unexpected result: {other:?}")),
    }
}

fn add_remote_error_message(error: &str) -> String {
    match error {
        "remote target already exists" => "remote already added".to_string(),
        "remote name already exists" => "name already used".to_string(),
        other => other.to_string(),
    }
}

fn summary_refresh_subscription_request(id: impl Into<String>) -> crate::api::schema::Request {
    use crate::api::schema::Subscription;

    crate::api::schema::Request {
        id: id.into(),
        method: crate::api::schema::Method::EventsSubscribe(
            crate::api::schema::EventsSubscribeParams {
                subscriptions: vec![
                    Subscription::WorkspaceCreated {},
                    Subscription::WorkspaceUpdated {},
                    Subscription::WorkspaceRenamed {},
                    Subscription::WorkspaceClosed {},
                    Subscription::WorkspaceFocused {},
                    Subscription::TabCreated {},
                    Subscription::TabClosed {},
                    Subscription::TabFocused {},
                    Subscription::TabRenamed {},
                    Subscription::PaneCreated {},
                    Subscription::PaneClosed {},
                    Subscription::PaneFocused {},
                    Subscription::PaneExited {},
                    Subscription::PaneAgentDetected {},
                    Subscription::PaneAgentStatusChanged {
                        pane_id: None,
                        agent_status: None,
                    },
                ],
            },
        ),
    }
}

fn start_missing_supervisor_summary_subscriptions(
    model: &supervisor::ClientSupervisorModel,
    subscribed_server_ids: &mut HashSet<supervisor::ServerId>,
    ssh_bridges: &HashMap<supervisor::ServerId, crate::remote::RemoteBridge>,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
) {
    for plan in model.summary_subscription_plans() {
        let Some(target) =
            api_target_for_supervisor_target(&plan.server_id, &plan.target, ssh_bridges)
        else {
            continue;
        };
        if !subscribed_server_ids.insert(plan.server_id.clone()) {
            continue;
        }
        spawn_supervisor_summary_subscription(plan.server_id, target, event_tx, should_quit);
    }
}

fn spawn_supervisor_summary_subscription(
    server_id: supervisor::ServerId,
    target: crate::api::client::ConnectionTarget,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
) {
    let event_tx = event_tx.clone();
    let should_quit = should_quit.clone();
    std::thread::spawn(move || {
        let changed_event_tx = event_tx.clone();
        let _end_guard = SummarySubscriptionEndGuard {
            server_id: server_id.clone(),
            event_tx,
        };
        let client = crate::api::client::ApiClient::for_target(target);
        let request = summary_refresh_subscription_request(format!("client:summary:{server_id:?}"));
        let (ack, mut stream) =
            match client.subscribe_value(&request, Some(Duration::from_millis(500))) {
                Ok(value) => value,
                Err(err) => {
                    warn!(
                        server_id = ?server_id,
                        err = %err,
                        "failed to subscribe to supervisor summary events"
                    );
                    return;
                }
            };
        if let Err(err) = crate::api::client::parse_response_value(ack) {
            warn!(
                server_id = ?server_id,
                err = %err,
                "supervisor summary subscription was rejected"
            );
            return;
        }

        while !should_quit.load(Ordering::Acquire) {
            match stream.next_value() {
                Ok(Some(_event)) => {
                    if changed_event_tx
                        .blocking_send(ClientLoopEvent::SupervisorSummaryChanged(server_id.clone()))
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(None) => return,
                Err(err) if api_client_error_is_timeout(&err) => continue,
                Err(err) => {
                    warn!(
                        server_id = ?server_id,
                        err = %err,
                        "supervisor summary subscription ended"
                    );
                    return;
                }
            }
        }
    });
}

fn api_client_error_is_timeout(err: &crate::api::client::ApiClientError) -> bool {
    matches!(
        err,
        crate::api::client::ApiClientError::Io(io_err)
            if matches!(
                io_err.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            )
    )
}

fn handle_client_add_remote_submission(
    draft: supervisor::AddRemoteDraft,
    model: &mut supervisor::ClientSupervisorModel,
    server_size: (u16, u16),
    cell_size_px: (u32, u32),
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    server_writes: &mut HashMap<supervisor::ServerId, UnixStream>,
    ssh_bridges: &mut HashMap<supervisor::ServerId, crate::remote::RemoteBridge>,
) -> Result<(), String> {
    let target = crate::remote_registry::RemoteTargetSnapshot::parse(&draft.target)
        .map_err(|err| err.message().to_string())?;
    reject_duplicate_main_target(&target)?;

    let keybindings = draft.keybindings;
    let (pending_stream, pending_bridge) = match &target {
        crate::remote_registry::RemoteTargetSnapshot::Local { session } => {
            validate_add_remote_target(
                crate::api::client::ConnectionTarget::LocalSession(session.clone()),
                |connection_target| {
                    let mut api = crate::api::client::ApiClient::for_target(connection_target);
                    supervisor::request_runtime_status(&mut api)
                },
            )?;
            let socket_path = crate::session::client_socket_path_for(session.as_deref());
            let stream = connect_secondary_client_stream(
                &socket_path,
                server_size,
                cell_size_px.0,
                cell_size_px.1,
                keybindings,
            )
            .map_err(|err| err.to_string())?;
            (Some(stream), None)
        }
        crate::remote_registry::RemoteTargetSnapshot::Ssh { target } => {
            let bridge = crate::remote::start_ssh_remote_bridge(target, None)
                .map_err(|err| format!("failed to start ssh remote bridge: {err}"))?;
            validate_add_remote_target(
                crate::api::client::ConnectionTarget::SocketPath(
                    bridge.api_socket_path().to_path_buf(),
                ),
                |connection_target| {
                    let mut api = crate::api::client::ApiClient::for_target(connection_target);
                    supervisor::request_runtime_status(&mut api)
                },
            )?;
            let stream = connect_secondary_client_stream(
                bridge.client_socket_path(),
                server_size,
                cell_size_px.0,
                cell_size_px.1,
                keybindings,
            )
            .map_err(|err| err.to_string())?;
            (Some(stream), Some(bridge))
        }
    };

    let mut main_api = crate::api::client::ApiClient::local();
    let remote = submit_remote_add_to_main_api(&mut main_api, draft)?;
    let server_id = model.add_secondary(remote);

    if let Some(bridge) = pending_bridge {
        ssh_bridges.insert(server_id.clone(), bridge);
    }

    if let Some(stream) = pending_stream {
        attach_secondary_client_stream(
            server_id.clone(),
            stream,
            event_tx,
            should_quit,
            server_writes,
        )
        .map_err(|err| err.to_string())?;
        let _ = model.set_connection_state(&server_id, supervisor::ConnectionState::Connected);
    }

    model.finish_add_remote();
    Ok(())
}

fn reject_duplicate_main_target(
    target: &crate::remote_registry::RemoteTargetSnapshot,
) -> Result<(), String> {
    let Some(main_target) = main_server_target_snapshot() else {
        return Ok(());
    };
    if main_target.canonical_key() == target.canonical_key() {
        return Err("remote already added".to_string());
    }
    Ok(())
}

fn main_server_target_snapshot() -> Option<crate::remote_registry::RemoteTargetSnapshot> {
    if let Ok(target) = std::env::var(crate::remote::MAIN_REMOTE_TARGET_ENV_VAR) {
        return crate::remote_registry::RemoteTargetSnapshot::parse(&target).ok();
    }

    Some(crate::remote_registry::RemoteTargetSnapshot::Local {
        session: crate::session::active_name(),
    })
}

fn validate_add_remote_target(
    target: crate::api::client::ConnectionTarget,
    mut status_for_target: impl FnMut(
        crate::api::client::ConnectionTarget,
    ) -> Result<crate::api::RuntimeStatus, String>,
) -> Result<(), String> {
    let deadline = Instant::now() + ADD_REMOTE_TARGET_VALIDATE_TIMEOUT;
    loop {
        match status_for_target(target.clone()) {
            Ok(status) => {
                if status.protocol != Some(PROTOCOL_VERSION) {
                    return Err(format!(
                        "protocol mismatch: server protocol {:?}, client protocol {}",
                        status.protocol, PROTOCOL_VERSION
                    ));
                }
                return Ok(());
            }
            Err(err)
                if add_remote_target_status_error_is_transient(&err)
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(ADD_REMOTE_TARGET_VALIDATE_RETRY_DELAY);
            }
            Err(err) => return Err(err),
        }
    }
}

fn add_remote_target_status_error_is_transient(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("resource temporarily unavailable")
        || error.contains("operation would block")
        || error.contains("timed out")
        || error.contains("connection refused")
        || error.contains("no such file or directory")
}

fn refresh_client_supervisor_summaries(
    model: &mut supervisor::ClientSupervisorModel,
    ssh_bridges: &HashMap<supervisor::ServerId, crate::remote::RemoteBridge>,
) {
    let mut api = crate::api::client::ApiClient::local();
    if let Err(err) = model.refresh_remote_registry_from_api(&mut api) {
        warn!(err = %err, "failed to refresh main server remote registry");
    }
    if let Err(err) = model.refresh_main_ui_settings_from_api(&mut api) {
        warn!(err = %err, "failed to refresh main server UI settings");
    }
    if let Err(err) = model.refresh_main_summary_from_api(&mut api) {
        warn!(err = %err, "failed to refresh main server summary");
    }
    let mut immediate_results = Vec::new();
    let (tx, rx) = std::sync::mpsc::channel();
    for plan in model.secondary_connection_plans() {
        let Some(target) =
            api_target_for_supervisor_target(&plan.server_id, &plan.target, ssh_bridges)
        else {
            immediate_results.push((plan.server_id, Err(supervisor::ConnectionState::Connecting)));
            continue;
        };
        let server_id = plan.server_id;
        let tx = tx.clone();
        std::thread::spawn(move || {
            let result = supervisor::fetch_server_summary_from_api_target(target);
            let _ = tx.send((server_id, result));
        });
    }
    drop(tx);
    immediate_results.extend(rx);
    model.apply_secondary_summary_results(immediate_results);
}

fn supervisor_summary_refresh_due(now: Instant, last_refresh: Instant) -> bool {
    now.duration_since(last_refresh) >= CLIENT_SUPERVISOR_REFRESH_INTERVAL
}

fn secondary_retry_delay(attempt: usize) -> Duration {
    match attempt {
        0 => Duration::from_secs(1),
        1 => Duration::from_secs(2),
        2 => Duration::from_secs(5),
        _ => Duration::from_secs(15),
    }
}

fn schedule_secondary_retry(
    state: &mut ClientState,
    server_id: supervisor::ServerId,
    attempt: usize,
    now: Instant,
) {
    state.secondary_retries.insert(
        server_id,
        SecondaryRetryState {
            attempt,
            next_retry_at: now + secondary_retry_delay(attempt),
        },
    );
}

fn schedule_missing_secondary_stream_retries(
    state: &mut ClientState,
    server_writes: &HashMap<supervisor::ServerId, UnixStream>,
    now: Instant,
) {
    let Some(model) = &state.supervisor_model else {
        return;
    };
    let connected_streams: HashSet<_> = server_writes.keys().cloned().collect();
    let retry_server_ids = model
        .secondary_server_ids_missing_client_stream(&connected_streams)
        .into_iter()
        .chain(model.unconnected_secondary_server_ids());
    for server_id in retry_server_ids {
        state
            .secondary_retries
            .entry(server_id.clone())
            .or_insert_with(|| SecondaryRetryState {
                attempt: 0,
                next_retry_at: now + secondary_retry_delay(0),
            });
    }
}

fn handle_server_write_failure(
    state: &mut ClientState,
    server_writes: &mut HashMap<supervisor::ServerId, UnixStream>,
    server_id: supervisor::ServerId,
    error: io::Error,
    now: Instant,
) -> Result<(), ClientError> {
    if server_id == supervisor::ServerId::main() {
        return Err(ClientError::ConnectionLost(error));
    }

    warn!(
        server_id = ?server_id,
        err = %error,
        "secondary server write failed; marking it disconnected"
    );
    server_writes.remove(&server_id);
    state.frame_cache.remove(&server_id);
    state.summary_subscription_server_ids.remove(&server_id);
    state.ssh_bridges.remove(&server_id);
    if let Some(model) = &mut state.supervisor_model {
        let _ = model.set_connection_state(&server_id, supervisor::ConnectionState::Disconnected);
    }
    schedule_secondary_retry(state, server_id, 0, now);
    state.request_full_redraw();
    Ok(())
}

fn retry_due_secondary_connections(
    state: &mut ClientState,
    now: Instant,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    server_writes: &mut HashMap<supervisor::ServerId, UnixStream>,
) {
    let due: Vec<(supervisor::ServerId, usize)> = state
        .secondary_retries
        .iter()
        .filter(|(_, retry)| retry.next_retry_at <= now)
        .map(|(server_id, retry)| (server_id.clone(), retry.attempt))
        .collect();

    for (server_id, attempt) in due {
        if server_writes.contains_key(&server_id) {
            state.secondary_retries.remove(&server_id);
            continue;
        }

        let plan = state.supervisor_model.as_ref().and_then(|model| {
            model
                .secondary_connection_plans()
                .into_iter()
                .find(|plan| plan.server_id == server_id)
        });
        let Some(plan) = plan else {
            state.secondary_retries.remove(&server_id);
            continue;
        };

        match connect_secondary_client_stream_for_plan(
            &plan,
            state.reported_size,
            state.cell_size_px.0,
            state.cell_size_px.1,
            &mut state.ssh_bridges,
        ) {
            Ok(stream) => {
                if let Err(err) = attach_secondary_client_stream(
                    server_id.clone(),
                    stream,
                    event_tx,
                    should_quit,
                    server_writes,
                ) {
                    let next_attempt = attempt.saturating_add(1);
                    schedule_secondary_retry(state, server_id.clone(), next_attempt, now);
                    if let Some(model) = &mut state.supervisor_model {
                        let _ = model.set_connection_state(
                            &server_id,
                            connection_state_from_client_error(&err),
                        );
                    }
                    warn!(
                        server_id = ?server_id,
                        err = %err,
                        "failed to attach retried secondary client stream"
                    );
                    state.request_full_redraw();
                    continue;
                }

                state.secondary_retries.remove(&server_id);
                if let Some(model) = &mut state.supervisor_model {
                    let _ = model
                        .set_connection_state(&server_id, supervisor::ConnectionState::Connected);
                    refresh_client_supervisor_summaries(model, &state.ssh_bridges);
                    start_missing_supervisor_summary_subscriptions(
                        model,
                        &mut state.summary_subscription_server_ids,
                        &state.ssh_bridges,
                        event_tx,
                        should_quit,
                    );
                    state.last_supervisor_summary_refresh = now;
                }
                state.request_full_redraw();
            }
            Err(err) => {
                let connection_state = connection_state_from_client_error(&err);
                if matches!(
                    connection_state,
                    supervisor::ConnectionState::ProtocolMismatch { .. }
                ) {
                    state.secondary_retries.remove(&server_id);
                } else {
                    let next_attempt = attempt.saturating_add(1);
                    schedule_secondary_retry(state, server_id.clone(), next_attempt, now);
                }
                state.ssh_bridges.remove(&server_id);
                if let Some(model) = &mut state.supervisor_model {
                    let _ = model.set_connection_state(&server_id, connection_state);
                }
                warn!(
                    server_id = ?server_id,
                    err = %err,
                    "failed to retry secondary client connection"
                );
                state.request_full_redraw();
            }
        }
    }
}

fn select_composited_render_frame<'a>(
    frames: &'a HashMap<supervisor::ServerId, protocol::FrameData>,
    active_server_id: &supervisor::ServerId,
    _incoming_server_id: &supervisor::ServerId,
) -> Option<&'a protocol::FrameData> {
    frames.get(active_server_id)
}

fn render_cached_composited_frame(state: &mut ClientState) {
    let (Some(compositor), Some(model)) = (&state.compositor, &state.supervisor_model) else {
        return;
    };

    let active_server_id = model.active_server_id().clone();
    let Some(active_frame) = state.frame_cache.get(&active_server_id).cloned() else {
        return;
    };

    let frame_data =
        compositor.compose_frame(model, &active_frame, state.host_size.0, state.host_size.1);
    let encoded = state.blit_encoder.encode(&frame_data, false);
    let graphics = if state.kitty_graphics_enabled {
        frame_data.graphics.as_slice()
    } else {
        &[]
    };
    let mut stdout = io::stdout();
    let _ = write_encoded_frame_with_graphics(&mut stdout, &encoded.bytes, graphics);
    let _ = stdout.flush();
    state.blit_encoder.commit(frame_data, encoded);
}

/// The main client event loop.
///
/// Uses a threaded architecture:
/// - stdin reader thread → sends raw input bytes to main loop
/// - resize poller thread → sends resize events to main loop
/// - server reader thread → reads ServerMessages and sends to main loop
/// - main loop: coordinates input, output, and server communication
async fn run_client_loop(
    stream: UnixStream,
    should_quit: Arc<AtomicBool>,
    options: ClientLoopOptions,
) -> Result<(), ClientError> {
    let ClientLoopOptions {
        host_size,
        reported_size,
        cell_size_px,
        sound_config,
        mouse_scroll_lines,
        redraw_on_focus_gained,
        kitty_graphics_enabled,
        mouse_capture_active,
        negotiated_encoding,
        attach_escape,
        compositor,
        supervisor_model,
        secondary_streams,
        ssh_bridges,
    } = options;
    let mut state = ClientState {
        blit_encoder: render_ansi::BlitEncoder::new(),
        mouse_capture_active,
        reported_size,
        host_size,
        cell_size_px,
        sound_config,
        kitty_graphics_enabled,
        attach_escape,
        mouse_scroll_lines,
        redraw_on_focus_gained,
        compositor,
        supervisor_model,
        last_supervisor_summary_refresh: Instant::now(),
        frame_cache: HashMap::new(),
        summary_subscription_server_ids: HashSet::new(),
        ssh_bridges,
        secondary_retries: HashMap::new(),
    };
    debug!(?negotiated_encoding, "client render encoding active");

    // Channel for events from the stdin, resize, and server reader threads.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<ClientLoopEvent>(256);

    // Spawn the stdin reader thread.
    let stdin_quit = should_quit.clone();
    let stdin_tx = event_tx.clone();
    std::thread::spawn(move || {
        input::stdin_reader_loop(stdin_tx, &stdin_quit);
    });

    if state.attach_escape.is_none() {
        query_host_terminal_theme();
    }

    // Spawn the resize poller thread.
    let resize_quit = should_quit.clone();
    let resize_tx = event_tx.clone();
    std::thread::spawn(move || {
        resize_poll_loop(
            resize_tx,
            host_size.0,
            host_size.1,
            kitty_graphics_enabled,
            &resize_quit,
        );
    });

    // Spawn the server reader thread (blocking reads from the socket).
    // Clone the stream's file descriptor so we can read from a blocking stream.
    let server_read_quit = should_quit.clone();
    let server_read_tx = event_tx.clone();
    let main_server_id = supervisor::ServerId::main();
    let read_stream = stream.try_clone().map_err(ClientError::ConnectionFailed)?;
    std::thread::spawn(move || {
        let max_frame_size = if kitty_graphics_enabled {
            MAX_GRAPHICS_FRAME_SIZE
        } else {
            MAX_FRAME_SIZE
        };
        server_reader_thread(
            main_server_id,
            read_stream,
            server_read_tx,
            &server_read_quit,
            max_frame_size,
        );
    });

    // Use the original stream for writing (blocking is fine since we write
    // from the async loop).
    let mut server_writes = HashMap::new();
    stream
        .set_nonblocking(false)
        .map_err(ClientError::ConnectionFailed)?;
    server_writes.insert(supervisor::ServerId::main(), stream);

    for (server_id, stream) in secondary_streams {
        let read_stream = stream.try_clone().map_err(ClientError::ConnectionFailed)?;
        let secondary_read_tx = event_tx.clone();
        let secondary_read_quit = should_quit.clone();
        let reader_server_id = server_id.clone();
        std::thread::spawn(move || {
            server_reader_thread(
                reader_server_id,
                read_stream,
                secondary_read_tx,
                &secondary_read_quit,
                MAX_FRAME_SIZE,
            );
        });
        stream
            .set_nonblocking(false)
            .map_err(ClientError::ConnectionFailed)?;
        server_writes.insert(server_id, stream);
    }

    schedule_missing_secondary_stream_retries(&mut state, &server_writes, Instant::now());

    if let Some(model) = &state.supervisor_model {
        start_missing_supervisor_summary_subscriptions(
            model,
            &mut state.summary_subscription_server_ids,
            &state.ssh_bridges,
            &event_tx,
            &should_quit,
        );
    }

    // Main event loop.
    while !should_quit.load(Ordering::Acquire) {
        let event = tokio::select! {
            ev = event_rx.recv() => ev.unwrap_or(ClientLoopEvent::Timer),
            _ = tokio::time::sleep(Duration::from_millis(100)) => ClientLoopEvent::Timer,
        };

        match event {
            ClientLoopEvent::StdinInput(data) => {
                let data = if let Some(attach_escape) = &mut state.attach_escape {
                    match attach_escape.filter_input(
                        data,
                        state.reported_size.1,
                        state.mouse_scroll_lines,
                    ) {
                        AttachInputAction::Forward(data) => data,
                        AttachInputAction::Scroll {
                            source,
                            direction,
                            lines,
                            column,
                            row,
                            modifiers,
                        } => {
                            let msg = ClientMessage::AttachScroll {
                                source,
                                direction,
                                lines,
                                column,
                                row,
                                modifiers,
                            };
                            if let Err(e) = write_to_server_id(
                                &mut server_writes,
                                &supervisor::ServerId::main(),
                                &msg,
                            ) {
                                return Err(ClientError::ConnectionLost(e));
                            }
                            continue;
                        }
                        AttachInputAction::Detach => {
                            let _ = write_to_server_id(
                                &mut server_writes,
                                &supervisor::ServerId::main(),
                                &ClientMessage::Detach,
                            );
                            return Ok(());
                        }
                        AttachInputAction::None => continue,
                    }
                } else {
                    let events = crate::raw_input::parse_raw_input_bytes_sync(&data);
                    if crate::raw_input::events_require_host_surface_redraw(
                        &events,
                        state.redraw_on_focus_gained,
                    ) {
                        state.request_full_redraw();
                    }
                    if let (Some(compositor), Some(model)) =
                        (&mut state.compositor, &mut state.supervisor_model)
                    {
                        match dispatch_composited_input(data, compositor, model, state.host_size) {
                            ClientInputDispatch::Forward(data) => data,
                            ClientInputDispatch::ServerControl { server_id, message } => {
                                if let Err(e) =
                                    write_to_server_id(&mut server_writes, &server_id, &message)
                                {
                                    handle_server_write_failure(
                                        &mut state,
                                        &mut server_writes,
                                        server_id,
                                        e,
                                        Instant::now(),
                                    )?;
                                }
                                state.request_full_redraw();
                                render_cached_composited_frame(&mut state);
                                continue;
                            }
                            ClientInputDispatch::ApiRequest {
                                server_id,
                                refresh,
                                request,
                            } => {
                                match send_client_supervisor_request(
                                    model,
                                    &server_id,
                                    *request,
                                    &state.ssh_bridges,
                                ) {
                                    Ok(()) => {
                                        if refresh == ClientApiRefreshPolicy::Immediate {
                                            refresh_client_supervisor_summaries(
                                                model,
                                                &state.ssh_bridges,
                                            );
                                            start_missing_supervisor_summary_subscriptions(
                                                model,
                                                &mut state.summary_subscription_server_ids,
                                                &state.ssh_bridges,
                                                &event_tx,
                                                &should_quit,
                                            );
                                            state.last_supervisor_summary_refresh = Instant::now();
                                        }
                                    }
                                    Err(err) => {
                                        warn!(
                                            server_id = ?server_id,
                                            err = %err,
                                            "failed to route client sidebar request"
                                        );
                                    }
                                }
                                state.request_full_redraw();
                                render_cached_composited_frame(&mut state);
                                continue;
                            }
                            ClientInputDispatch::AddRemote(draft) => {
                                match handle_client_add_remote_submission(
                                    draft,
                                    model,
                                    state.reported_size,
                                    state.cell_size_px,
                                    &event_tx,
                                    &should_quit,
                                    &mut server_writes,
                                    &mut state.ssh_bridges,
                                ) {
                                    Ok(()) => {
                                        refresh_client_supervisor_summaries(
                                            model,
                                            &state.ssh_bridges,
                                        );
                                        start_missing_supervisor_summary_subscriptions(
                                            model,
                                            &mut state.summary_subscription_server_ids,
                                            &state.ssh_bridges,
                                            &event_tx,
                                            &should_quit,
                                        );
                                        state.last_supervisor_summary_refresh = Instant::now();
                                    }
                                    Err(err) => {
                                        warn!(err = %err, "failed to add client remote");
                                        model.set_add_remote_error(err);
                                    }
                                }
                                state.request_full_redraw();
                                render_cached_composited_frame(&mut state);
                                continue;
                            }
                            ClientInputDispatch::Resize { cols, rows } => {
                                state.reported_size = (cols, rows);
                                let msg = ClientMessage::Resize {
                                    cols,
                                    rows,
                                    cell_width_px: state.cell_size_px.0,
                                    cell_height_px: state.cell_size_px.1,
                                };
                                let mut write_failures = Vec::new();
                                for (server_id, stream) in server_writes.iter_mut() {
                                    if let Err(e) = write_to_server(stream, &msg) {
                                        write_failures.push((server_id.clone(), e));
                                    }
                                }
                                for (server_id, error) in write_failures {
                                    handle_server_write_failure(
                                        &mut state,
                                        &mut server_writes,
                                        server_id,
                                        error,
                                        Instant::now(),
                                    )?;
                                }
                                state.request_full_redraw();
                                render_cached_composited_frame(&mut state);
                                continue;
                            }
                            ClientInputDispatch::DetachAll => {
                                let detach = ClientMessage::Detach;
                                for stream in server_writes.values_mut() {
                                    let _ = write_to_server(stream, &detach);
                                }
                                return Ok(());
                            }
                            ClientInputDispatch::Redraw => {
                                state.request_full_redraw();
                                render_cached_composited_frame(&mut state);
                                continue;
                            }
                            ClientInputDispatch::Consumed => continue,
                        }
                    } else {
                        data
                    }
                };
                if should_bridge_clipboard_image_paste(&data) {
                    if let Some(image) = crate::platform::read_clipboard_image() {
                        if image.bytes.len() > MAX_CLIPBOARD_IMAGE_PAYLOAD {
                            warn!(
                                bytes = image.bytes.len(),
                                max = MAX_CLIPBOARD_IMAGE_PAYLOAD,
                                "local clipboard image is too large to bridge"
                            );
                            continue;
                        }
                        info!(
                            bytes = image.bytes.len(),
                            extension = image.extension,
                            "bridging local clipboard image paste to remote server"
                        );
                        let msg = ClientMessage::ClipboardImage {
                            extension: image.extension.to_owned(),
                            data: image.bytes,
                        };
                        let server_id = active_server_id(&state);
                        if let Err(e) = write_to_server_id(&mut server_writes, &server_id, &msg) {
                            handle_server_write_failure(
                                &mut state,
                                &mut server_writes,
                                server_id,
                                e,
                                Instant::now(),
                            )?;
                            render_cached_composited_frame(&mut state);
                        }
                        continue;
                    }
                    info!(
                        "clipboard image paste trigger received, but local clipboard has no image"
                    );
                }
                let msg = ClientMessage::Input { data };
                let server_id = active_server_id(&state);
                if let Err(e) = write_to_server_id(&mut server_writes, &server_id, &msg) {
                    handle_server_write_failure(
                        &mut state,
                        &mut server_writes,
                        server_id,
                        e,
                        Instant::now(),
                    )?;
                    render_cached_composited_frame(&mut state);
                }
            }
            ClientLoopEvent::Resize(new_cols, new_rows, cell_width_px, cell_height_px) => {
                state.host_size = (new_cols, new_rows);
                state.cell_size_px = (cell_width_px, cell_height_px);
                state.reported_size = state
                    .compositor
                    .as_ref()
                    .map(|compositor| compositor.content_size(new_cols, new_rows))
                    .unwrap_or((new_cols, new_rows));
                let msg = ClientMessage::Resize {
                    cols: state.reported_size.0,
                    rows: state.reported_size.1,
                    cell_width_px,
                    cell_height_px,
                };
                if state.compositor.is_some() {
                    let mut write_failures = Vec::new();
                    for (server_id, stream) in server_writes.iter_mut() {
                        if let Err(e) = write_to_server(stream, &msg) {
                            write_failures.push((server_id.clone(), e));
                        }
                    }
                    for (server_id, error) in write_failures {
                        handle_server_write_failure(
                            &mut state,
                            &mut server_writes,
                            server_id,
                            error,
                            Instant::now(),
                        )?;
                    }
                } else {
                    let server_id = active_server_id(&state);
                    if let Err(e) = write_to_server_id(&mut server_writes, &server_id, &msg) {
                        handle_server_write_failure(
                            &mut state,
                            &mut server_writes,
                            server_id,
                            e,
                            Instant::now(),
                        )?;
                    }
                }
            }
            ClientLoopEvent::ServerMessage { server_id, message } => match message {
                ServerMessage::Frame(mut frame_data) => {
                    if let (Some(compositor), Some(model)) =
                        (&state.compositor, &state.supervisor_model)
                    {
                        state.frame_cache.insert(server_id.clone(), frame_data);
                        let active_server_id = model.active_server_id().clone();
                        let Some(active_frame) = select_composited_render_frame(
                            &state.frame_cache,
                            &active_server_id,
                            &server_id,
                        ) else {
                            continue;
                        };
                        frame_data = compositor.compose_frame(
                            model,
                            active_frame,
                            state.host_size.0,
                            state.host_size.1,
                        );
                    }
                    let encoded = state.blit_encoder.encode(&frame_data, false);
                    let mut stdout = io::stdout();
                    let graphics = if state.kitty_graphics_enabled {
                        frame_data.graphics.as_slice()
                    } else {
                        &[]
                    };
                    let _ =
                        write_encoded_frame_with_graphics(&mut stdout, &encoded.bytes, graphics);
                    let _ = stdout.flush();
                    state.blit_encoder.commit(frame_data, encoded);
                }
                ServerMessage::Terminal(frame) => {
                    if server_id != active_server_id(&state) {
                        continue;
                    }
                    if state.kitty_graphics_enabled && contains_kitty_graphics_bytes(&frame.bytes) {
                        record_received_kitty_graphics(&frame.bytes);
                    }
                    let mut stdout = io::stdout();
                    let _ = stdout.write_all(&frame.bytes);
                    let _ = stdout.flush();
                }
                ServerMessage::Graphics { bytes } => {
                    if server_id != active_server_id(&state) {
                        continue;
                    }
                    if state.kitty_graphics_enabled {
                        record_received_kitty_graphics(&bytes);
                        let mut stdout = io::stdout();
                        let _ = stdout.write_all(&bytes);
                        let _ = stdout.flush();
                    }
                }
                ServerMessage::ServerShutdown { reason } => {
                    if server_id != supervisor::ServerId::main() {
                        server_writes.remove(&server_id);
                        state.frame_cache.remove(&server_id);
                        state.summary_subscription_server_ids.remove(&server_id);
                        state.ssh_bridges.remove(&server_id);
                        if let Some(model) = &mut state.supervisor_model {
                            let _ = model.set_connection_state(
                                &server_id,
                                supervisor::ConnectionState::Disconnected,
                            );
                            state.request_full_redraw();
                        }
                        schedule_secondary_retry(&mut state, server_id, 0, Instant::now());
                        render_cached_composited_frame(&mut state);
                        continue;
                    }
                    return Err(ClientError::ServerShutdown { reason });
                }
                ServerMessage::Notify { kind, message } => {
                    handle_notify(kind, &message, &state.sound_config);
                }
                ServerMessage::Clipboard { data } => {
                    forward_clipboard(&data);
                    let _ = io::stdout().flush();
                }
                ServerMessage::ReloadSoundConfig => {
                    reload_local_client_config(
                        &mut state.sound_config,
                        &mut state.redraw_on_focus_gained,
                    );
                }
                ServerMessage::MouseCapture { enabled } => {
                    if server_id != active_server_id(&state) {
                        continue;
                    }
                    let desired = desired_mouse_capture(enabled, state.compositor.is_some());
                    if desired != state.mouse_capture_active {
                        set_mouse_capture(desired).map_err(ClientError::ConnectionFailed)?;
                        state.mouse_capture_active = desired;
                    }
                }
                ServerMessage::Welcome { .. } => {
                    debug!("received unexpected Welcome in main loop");
                }
            },
            ClientLoopEvent::SupervisorSummaryChanged(server_id) => {
                debug!(
                    server_id = ?server_id,
                    "supervisor summary event requested refresh"
                );
                if let Some(model) = &mut state.supervisor_model {
                    refresh_client_supervisor_summaries(model, &state.ssh_bridges);
                    state.last_supervisor_summary_refresh = Instant::now();
                    state.request_full_redraw();
                }
                schedule_missing_secondary_stream_retries(
                    &mut state,
                    &server_writes,
                    Instant::now(),
                );
                if let Some(model) = &state.supervisor_model {
                    start_missing_supervisor_summary_subscriptions(
                        model,
                        &mut state.summary_subscription_server_ids,
                        &state.ssh_bridges,
                        &event_tx,
                        &should_quit,
                    );
                }
                render_cached_composited_frame(&mut state);
            }
            ClientLoopEvent::SupervisorSummarySubscriptionEnded(server_id) => {
                state.summary_subscription_server_ids.remove(&server_id);
            }
            ClientLoopEvent::ServerDisconnected(server_id) => {
                if server_id == supervisor::ServerId::main() {
                    return Err(ClientError::ConnectionLost(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "server closed connection",
                    )));
                }
                server_writes.remove(&server_id);
                state.frame_cache.remove(&server_id);
                state.summary_subscription_server_ids.remove(&server_id);
                state.ssh_bridges.remove(&server_id);
                if let Some(model) = &mut state.supervisor_model {
                    let _ = model.set_connection_state(
                        &server_id,
                        supervisor::ConnectionState::Disconnected,
                    );
                    state.request_full_redraw();
                }
                schedule_secondary_retry(&mut state, server_id, 0, Instant::now());
                render_cached_composited_frame(&mut state);
            }
            ClientLoopEvent::Timer => {
                // Check if we should quit.
                let now = Instant::now();
                retry_due_secondary_connections(
                    &mut state,
                    now,
                    &event_tx,
                    &should_quit,
                    &mut server_writes,
                );
                if supervisor_summary_refresh_due(now, state.last_supervisor_summary_refresh) {
                    if let Some(model) = &mut state.supervisor_model {
                        refresh_client_supervisor_summaries(model, &state.ssh_bridges);
                        state.last_supervisor_summary_refresh = now;
                        state.request_full_redraw();
                    }
                    schedule_missing_secondary_stream_retries(&mut state, &server_writes, now);
                    if let Some(model) = &state.supervisor_model {
                        start_missing_supervisor_summary_subscriptions(
                            model,
                            &mut state.summary_subscription_server_ids,
                            &state.ssh_bridges,
                            &event_tx,
                            &should_quit,
                        );
                    }
                    render_cached_composited_frame(&mut state);
                }
            }
        }
    }

    // Clean exit (Ctrl+C). Send Detach before closing.
    let detach = ClientMessage::Detach;
    for stream in server_writes.values_mut() {
        let _ = write_to_server(stream, &detach);
    }
    let _ = io::stdout().flush();

    Ok(())
}

// ---------------------------------------------------------------------------
// Server reader thread
// ---------------------------------------------------------------------------

/// Blocking thread that reads ServerMessages from the server and sends them
/// to the main event loop.
fn server_reader_thread(
    server_id: supervisor::ServerId,
    mut stream: UnixStream,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    max_frame_size: usize,
) {
    // Ensure the read stream is in blocking mode to avoid WouldBlock errors
    // from read_exact inside read_message. The stream should already be
    // blocking after handshake, but we enforce it here as a safety measure.
    if stream.set_nonblocking(false).is_err() {
        // If we can't set blocking mode, the stream is likely broken.
        let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected(server_id));
        return;
    }

    loop {
        if should_quit.load(Ordering::Acquire) {
            break;
        }

        match protocol::read_message(&mut stream, max_frame_size) {
            Ok(msg) => {
                if event_tx
                    .blocking_send(ClientLoopEvent::ServerMessage {
                        server_id: server_id.clone(),
                        message: msg,
                    })
                    .is_err()
                {
                    break; // Main loop gone.
                }
            }
            Err(protocol::FramingError::UnexpectedEof) => {
                // Server closed connection.
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected(server_id));
                break;
            }
            Err(protocol::FramingError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {
                // Should not happen with blocking mode, but handle gracefully
                // in case the stream was set nonblocking by another clone.
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(err) => {
                warn!(err = %err, "server read error");
                let _ = event_tx.blocking_send(ClientLoopEvent::ServerDisconnected(server_id));
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Write helper
// ---------------------------------------------------------------------------

/// Writes a message to the server stream (blocking).
fn write_to_server(stream: &mut UnixStream, msg: &ClientMessage) -> io::Result<()> {
    protocol::write_message(stream, msg).map_err(|e| io::Error::other(e.to_string()))
}

fn active_server_id(state: &ClientState) -> supervisor::ServerId {
    state
        .supervisor_model
        .as_ref()
        .map(|model| model.active_server_id().clone())
        .unwrap_or_else(supervisor::ServerId::main)
}

fn write_to_server_id(
    server_writes: &mut HashMap<supervisor::ServerId, UnixStream>,
    server_id: &supervisor::ServerId,
    msg: &ClientMessage,
) -> io::Result<()> {
    let Some(stream) = server_writes.get_mut(server_id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            format!("server stream {server_id:?} is not connected"),
        ));
    };
    write_to_server(stream, msg)
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

fn reload_local_client_config(
    sound_config: &mut crate::config::SoundConfig,
    redraw_on_focus_gained: &mut bool,
) {
    match crate::config::load_live_config() {
        Ok(loaded) => {
            for diagnostic in loaded.config.ui.sound.diagnostics() {
                warn!(diagnostic = %diagnostic, "local sound config diagnostic");
            }
            *sound_config = loaded.config.ui.sound;
            *redraw_on_focus_gained = loaded.config.ui.redraw_on_focus_gained;
            debug!("reloaded local client config");
        }
        Err(diagnostics) => {
            warn!(diagnostics = ?diagnostics, "failed to reload local client config; keeping current client config");
        }
    }
}

fn handle_notify(kind: NotifyKind, message: &str, sound_config: &crate::config::SoundConfig) {
    handle_notify_with_notifiers(
        kind,
        message,
        sound_config,
        crate::terminal_notify::show_notification,
        crate::platform::show_desktop_notification,
    );
}

fn handle_notify_with_notifiers(
    kind: NotifyKind,
    message: &str,
    sound_config: &crate::config::SoundConfig,
    mut show_terminal_notification: impl FnMut(&str, Option<&str>) -> io::Result<bool>,
    mut show_system_notification: impl FnMut(&str, Option<&str>) -> io::Result<bool>,
) {
    match kind {
        NotifyKind::Sound => {
            let Some(sound) = sound_from_notify_message(message) else {
                warn!(
                    message = message,
                    "received unknown sound notification from server"
                );
                return;
            };
            if sound_config.enabled {
                crate::sound::play(sound, sound_config);
            }
        }
        NotifyKind::Toast => {
            debug!(
                message = message,
                "received terminal toast notification from server"
            );
            let (title, body) = crate::terminal_notify::split_message(message);
            if let Err(err) = show_terminal_notification(title, body) {
                warn!(err = %err, "failed to emit terminal notification");
            }
        }
        NotifyKind::SystemToast => {
            debug!(
                message = message,
                "received system toast notification from server"
            );
            let (title, body) = crate::terminal_notify::split_message(message);
            if let Err(err) = show_system_notification(title, body) {
                warn!(err = %err, "failed to emit system notification");
            }
        }
    }
}

fn sound_from_notify_message(message: &str) -> Option<crate::sound::Sound> {
    match message {
        "agent done" => Some(crate::sound::Sound::Done),
        "agent attention" => Some(crate::sound::Sound::Request),
        _ => None,
    }
}

fn should_bridge_clipboard_image_paste(data: &[u8]) -> bool {
    if data == b"\x1b[200~\x1b[201~" {
        return true;
    }

    let events = crate::raw_input::parse_raw_input_bytes_sync(data);
    matches!(
        events.as_slice(),
        [crate::raw_input::RawInputEvent::Key(key)]
            if key.kind == crossterm::event::KeyEventKind::Press
                && key.modifiers == crossterm::event::KeyModifiers::CONTROL
                && matches!(key.code, crossterm::event::KeyCode::Char('v' | 'V'))
    )
}

// ---------------------------------------------------------------------------
// Clipboard forwarding
// ---------------------------------------------------------------------------

/// Decode a clipboard payload forwarded by the server.
fn decode_clipboard_payload(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

/// Forwards a clipboard write from the server to the local client clipboard.
fn forward_clipboard(data: &str) {
    let Some(bytes) = decode_clipboard_payload(data) else {
        warn!("received invalid clipboard payload from server");
        return;
    };

    crate::selection::write_osc52_bytes(&bytes);
}

// ---------------------------------------------------------------------------
// Frame output
// ---------------------------------------------------------------------------

fn write_encoded_frame_with_graphics(
    mut writer: impl io::Write,
    encoded: &[u8],
    graphics: &[u8],
) -> io::Result<()> {
    writer.write_all(encoded)?;
    if graphics.is_empty() {
        return Ok(());
    }

    record_received_kitty_graphics(graphics);
    writer.write_all(b"\x1b7")?;
    writer.write_all(graphics)?;
    writer.write_all(b"\x1b8")
}

fn contains_kitty_graphics_bytes(bytes: &[u8]) -> bool {
    bytes.windows(3).any(|window| window == b"\x1b_G")
}

fn record_received_kitty_graphics(bytes: &[u8]) {
    let ids = kitty_graphics_image_ids(bytes);
    if ids.is_empty() {
        return;
    }
    let set = RECEIVED_KITTY_GRAPHICS_IDS.get_or_init(|| Mutex::new(HashSet::new()));
    if let Ok(mut set) = set.lock() {
        set.extend(ids);
    }
}

fn clear_received_kitty_graphics(mut writer: impl io::Write) -> io::Result<()> {
    let Some(set) = RECEIVED_KITTY_GRAPHICS_IDS.get() else {
        return Ok(());
    };
    let Ok(mut set) = set.lock() else {
        return Ok(());
    };
    for id in set.drain() {
        write!(writer, "\x1b_Ga=d,d=I,i={id},q=2;\x1b\\")?;
    }
    writer.flush()
}

fn kitty_graphics_image_ids(bytes: &[u8]) -> Vec<u32> {
    let mut ids = Vec::new();
    let mut index = 0usize;
    while let Some(start) = find_subslice(&bytes[index..], b"\x1b_G") {
        let command_start = index + start + 3;
        let Some(end) = find_subslice(&bytes[command_start..], b"\x1b\\") else {
            break;
        };
        let command = &bytes[command_start..command_start + end];
        if let Some(id) = kitty_graphics_command_image_id(command) {
            ids.push(id);
        }
        index = command_start + end + 2;
    }
    ids
}

fn kitty_graphics_command_image_id(command: &[u8]) -> Option<u32> {
    let header_end = command
        .iter()
        .position(|byte| *byte == b';')
        .unwrap_or(command.len());
    for part in command[..header_end].split(|byte| *byte == b',') {
        let Some(value) = part.strip_prefix(b"i=") else {
            continue;
        };
        let text = std::str::from_utf8(value).ok()?;
        if let Ok(id) = text.parse::<u32>() {
            return Some(id);
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ---------------------------------------------------------------------------
// Resize polling
// ---------------------------------------------------------------------------

fn current_terminal_geometry(kitty_graphics_enabled: bool) -> (u16, u16, u32, u32) {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    if !kitty_graphics_enabled {
        return (cols, rows, 0, 0);
    }
    let Ok(size) = crossterm::terminal::window_size() else {
        return (cols, rows, 8, 16);
    };
    if size.columns == 0 || size.rows == 0 || size.width == 0 || size.height == 0 {
        return (cols, rows, 8, 16);
    }
    (
        cols,
        rows,
        (size.width as u32 / size.columns as u32).max(1),
        (size.height as u32 / size.rows as u32).max(1),
    )
}

/// Polls the terminal size and sends resize events when it changes.
fn resize_poll_loop(
    resize_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    initial_cols: u16,
    initial_rows: u16,
    kitty_graphics_enabled: bool,
    should_quit: &Arc<AtomicBool>,
) {
    let (_, _, initial_cell_width, initial_cell_height) =
        current_terminal_geometry(kitty_graphics_enabled);
    let mut last_size = (
        initial_cols,
        initial_rows,
        initial_cell_width,
        initial_cell_height,
    );
    while !should_quit.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
        let new_size = current_terminal_geometry(kitty_graphics_enabled);
        if new_size != last_size {
            last_size = new_size;
            if resize_tx
                .blocking_send(ClientLoopEvent::Resize(
                    new_size.0, new_size.1, new_size.2, new_size.3,
                ))
                .is_err()
            {
                break; // Main loop gone.
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// Initialize logging for the client process.
fn query_host_terminal_theme() {
    let _ = write_host_terminal_theme_query(io::stdout());
}

fn write_host_terminal_theme_query(mut writer: impl io::Write) -> io::Result<()> {
    writer.write_all(crate::terminal_theme::HOST_COLOR_QUERY_SEQUENCE.as_bytes())?;
    writer.flush()
}

fn init_logging() {
    crate::logging::init_file_logging("herdr-client.log");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn restore_env_var(key: &str, value: Option<OsString>) {
        if let Some(value) = value {
            std::env::set_var(key, value);
        } else {
            std::env::remove_var(key);
        }
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            restore_env_var(self.key, self.previous.clone());
        }
    }

    fn test_remote_definition(
        id: &str,
        name: &str,
    ) -> crate::remote_registry::RemoteDefinitionSnapshot {
        crate::remote_registry::RemoteDefinitionSnapshot {
            id: id.into(),
            name: name.into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some(id.into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
        }
    }

    fn test_client_state_with_model(model: supervisor::ClientSupervisorModel) -> ClientState {
        ClientState {
            blit_encoder: render_ansi::BlitEncoder::new(),
            mouse_capture_active: false,
            reported_size: (80, 24),
            host_size: (80, 24),
            cell_size_px: (0, 0),
            sound_config: crate::config::SoundConfig::default(),
            kitty_graphics_enabled: false,
            attach_escape: None,
            mouse_scroll_lines: 3,
            redraw_on_focus_gained: false,
            compositor: None,
            supervisor_model: Some(model),
            last_supervisor_summary_refresh: Instant::now(),
            frame_cache: HashMap::new(),
            summary_subscription_server_ids: HashSet::new(),
            ssh_bridges: HashMap::new(),
            secondary_retries: HashMap::new(),
        }
    }

    struct EnvVarsRemovedGuard {
        previous: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvVarsRemovedGuard {
        fn new(keys: &[&'static str]) -> Self {
            let previous: Vec<_> = keys
                .iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect();
            for key in keys {
                std::env::remove_var(key);
            }
            Self { previous }
        }
    }

    impl Drop for EnvVarsRemovedGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.clone() {
                restore_env_var(key, value);
            }
        }
    }

    #[test]
    fn clipboard_image_paste_bridge_triggers_on_ctrl_v_and_empty_paste() {
        assert!(should_bridge_clipboard_image_paste(&[0x16]));
        assert!(should_bridge_clipboard_image_paste(b"\x1b[118;5u"));
        assert!(should_bridge_clipboard_image_paste(b"\x1b[200~\x1b[201~"));
        assert!(!should_bridge_clipboard_image_paste(
            b"\x1b[200~text\x1b[201~"
        ));
        assert!(!should_bridge_clipboard_image_paste(b"v"));
    }

    #[test]
    fn graphics_bytes_are_written_after_blit_with_saved_cursor() {
        let mut output = Vec::new();
        write_encoded_frame_with_graphics(
            &mut output,
            b"\x1b[?2026htext\x1b[?2026lcursor",
            b"graphics",
        )
        .unwrap();

        assert_eq!(
            output,
            b"\x1b[?2026htext\x1b[?2026lcursor\x1b7graphics\x1b8"
        );
    }

    #[test]
    fn empty_graphics_writes_only_blit_frame() {
        let mut output = Vec::new();
        write_encoded_frame_with_graphics(&mut output, b"text", b"").unwrap();

        assert_eq!(output, b"text");
    }

    #[test]
    fn terminal_frame_kitty_detection_matches_apc_prefix() {
        assert!(contains_kitty_graphics_bytes(b"text\x1b_Ga=p;\x1b\\"));
        assert!(!contains_kitty_graphics_bytes(b"text\x1b[?2026h"));
    }

    #[test]
    fn kitty_graphics_image_id_parser_tracks_herdr_ids_only() {
        let ids = kitty_graphics_image_ids(
            b"text\x1b_Ga=t,t=d,f=32,s=1,v=1,i=10023,q=2;AAAA\x1b\\\x1b_Ga=p,i=10023,p=7;\x1b\\",
        );
        assert_eq!(ids, vec![10023, 10023]);
    }

    #[test]
    fn kitty_graphics_cleanup_deletes_tracked_images_not_all_images() {
        record_received_kitty_graphics(b"\x1b_Ga=t,i=123,q=2;AAAA\x1b\\");
        let mut output = Vec::new();
        clear_received_kitty_graphics(&mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("a=d,d=I,i=123"));
        assert!(!text.contains("d=A"));
    }

    #[test]
    fn write_host_terminal_theme_query_emits_osc_queries() {
        let mut output = Vec::new();
        write_host_terminal_theme_query(&mut output).unwrap();
        assert_eq!(
            output,
            crate::terminal_theme::HOST_COLOR_QUERY_SEQUENCE.as_bytes()
        );
    }

    #[test]
    fn terminal_restore_postlude_restores_visible_default_cursor() {
        let mut output = Vec::new();
        write_terminal_restore_postlude(&mut output).unwrap();
        assert_eq!(output, b"\x1b[?25h\x1b[0 q");
    }

    #[test]
    fn attach_escape_detaches_on_prefix_q() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(vec![0x02], 24, 3),
            AttachInputAction::None
        ));
        assert!(matches!(
            escape.filter_input(vec![b'q'], 24, 3),
            AttachInputAction::Detach
        ));
    }

    #[test]
    fn attach_escape_sends_literal_prefix_on_double_prefix() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(vec![0x02], 24, 3),
            AttachInputAction::None
        ));
        match escape.filter_input(vec![0x02], 24, 3) {
            AttachInputAction::Forward(bytes) => assert_eq!(bytes, vec![0x02]),
            other => panic!("expected forwarded prefix, got {other:?}"),
        }
    }

    #[test]
    fn attach_escape_forwards_prefix_before_non_escape_key() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(vec![b'a', 0x02], 24, 3),
            AttachInputAction::Forward(bytes) if bytes == b"a"
        ));
        match escape.filter_input(vec![b'x'], 24, 3) {
            AttachInputAction::Forward(bytes) => assert_eq!(bytes, vec![0x02, b'x']),
            other => panic!("expected forwarded bytes, got {other:?}"),
        }
    }

    #[test]
    fn attach_escape_turns_wheel_into_scroll_action() {
        let mut escape = AttachEscapeState::default();
        match escape.filter_input(b"\x1b[<64;11;6M".to_vec(), 24, 7) {
            AttachInputAction::Scroll {
                source,
                direction,
                lines,
                column,
                row,
                ..
            } => {
                assert_eq!(source, AttachScrollSource::Wheel);
                assert_eq!(direction, AttachScrollDirection::Up);
                assert_eq!(lines, 7);
                assert_eq!(column, Some(10));
                assert_eq!(row, Some(5));
            }
            other => panic!("expected scroll action, got {other:?}"),
        }
    }

    #[test]
    fn attach_escape_swallows_non_wheel_mouse_reports() {
        let mut escape = AttachEscapeState::default();
        assert!(matches!(
            escape.filter_input(b"\x1b[<0;11;6M".to_vec(), 24, 7),
            AttachInputAction::None
        ));
    }

    #[test]
    fn attach_escape_turns_plain_page_keys_into_scroll_actions() {
        let mut escape = AttachEscapeState::default();
        match escape.filter_input(b"\x1b[5~".to_vec(), 12, 3) {
            AttachInputAction::Scroll {
                source,
                direction,
                lines,
                ..
            } => {
                assert_eq!(
                    source,
                    AttachScrollSource::PageKey {
                        input: b"\x1b[5~".to_vec()
                    }
                );
                assert_eq!(direction, AttachScrollDirection::Up);
                assert_eq!(lines, 11);
            }
            other => panic!("expected page-up scroll action, got {other:?}"),
        }

        match escape.filter_input(b"\x1b[6~".to_vec(), 12, 3) {
            AttachInputAction::Scroll {
                source,
                direction,
                lines,
                ..
            } => {
                assert_eq!(
                    source,
                    AttachScrollSource::PageKey {
                        input: b"\x1b[6~".to_vec()
                    }
                );
                assert_eq!(direction, AttachScrollDirection::Down);
                assert_eq!(lines, 11);
            }
            other => panic!("expected page-down scroll action, got {other:?}"),
        }
    }

    #[test]
    fn attach_escape_forwards_modified_page_key() {
        let mut escape = AttachEscapeState::default();
        match escape.filter_input(b"\x1b[5;5~".to_vec(), 12, 3) {
            AttachInputAction::Forward(bytes) => assert_eq!(bytes, b"\x1b[5;5~"),
            other => panic!("expected modified page key to forward, got {other:?}"),
        }
    }

    #[test]
    fn client_error_display_connection_failed() {
        let err = ClientError::ConnectionFailed(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("failed to connect to server"),
            "should mention connection failure: {msg}"
        );
        assert!(
            msg.contains("herdr server"),
            "should suggest starting server: {msg}"
        );
    }

    #[test]
    fn client_error_display_handshake_rejected() {
        let err = ClientError::HandshakeRejected {
            version: 1,
            error: "incompatible".into(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("rejected handshake"),
            "should mention rejection: {msg}"
        );
        assert!(msg.contains("incompatible"), "should include error: {msg}");
    }

    #[test]
    fn client_error_display_server_shutdown() {
        let err = ClientError::ServerShutdown {
            reason: Some("maintenance".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("server shut down"),
            "should mention shutdown: {msg}"
        );
        assert!(msg.contains("maintenance"), "should include reason: {msg}");
    }

    #[test]
    fn client_error_display_server_shutdown_no_reason() {
        let err = ClientError::ServerShutdown { reason: None };
        let msg = err.to_string();
        assert!(
            msg.contains("server shut down"),
            "should mention shutdown: {msg}"
        );
    }

    #[test]
    fn client_error_display_detached_default_session_reattach_hint() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvVarsRemovedGuard::new(&[
            crate::remote::REATTACH_COMMAND_ENV_VAR,
            crate::session::SESSION_ENV_VAR,
        ]);
        let err = ClientError::ServerShutdown {
            reason: Some("detached".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Run `herdr` to reattach"),
            "should suggest default reattach command: {msg}"
        );
    }

    #[test]
    fn client_error_display_detached_named_session_reattach_hint() {
        let _guard = env_lock().lock().unwrap();
        let _remote_env = EnvVarsRemovedGuard::new(&[crate::remote::REATTACH_COMMAND_ENV_VAR]);
        let _session_env = EnvVarGuard::set(crate::session::SESSION_ENV_VAR, "work");
        let err = ClientError::ServerShutdown {
            reason: Some("detached".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Run `herdr session attach work` to reattach"),
            "should suggest named session reattach command: {msg}"
        );
    }

    #[test]
    fn client_error_display_detached_remote_reattach_hint_takes_precedence() {
        let _guard = env_lock().lock().unwrap();
        let _remote_env = EnvVarGuard::set(
            crate::remote::REATTACH_COMMAND_ENV_VAR,
            "herdr --remote host --session work",
        );
        let _session_env = EnvVarGuard::set(crate::session::SESSION_ENV_VAR, "work");
        let err = ClientError::ServerShutdown {
            reason: Some("detached".into()),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Run `herdr --remote host --session work` to reattach"),
            "should prefer remote reattach command: {msg}"
        );
    }

    #[test]
    fn client_error_display_connection_lost() {
        let err =
            ClientError::ConnectionLost(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"));
        let msg = err.to_string();
        assert!(
            msg.contains("lost connection to server"),
            "should mention lost connection: {msg}"
        );
    }

    #[test]
    fn hello_message_uses_requested_surface_mode() {
        let hello = build_hello_message(
            80,
            24,
            0,
            0,
            RenderEncoding::SemanticFrame,
            ClientSurfaceMode::EmbeddedContent,
            ClientKeybindings::Server,
        );

        match hello {
            ClientMessage::Hello { surface_mode, .. } => {
                assert_eq!(surface_mode, ClientSurfaceMode::EmbeddedContent);
            }
            other => panic!("expected hello, got {other:?}"),
        }
    }

    #[test]
    fn sound_from_notify_message_maps_done() {
        assert_eq!(
            sound_from_notify_message("agent done"),
            Some(crate::sound::Sound::Done)
        );
    }

    #[test]
    fn sound_from_notify_message_maps_attention() {
        assert_eq!(
            sound_from_notify_message("agent attention"),
            Some(crate::sound::Sound::Request)
        );
    }

    #[test]
    fn sound_from_notify_message_rejects_unknown_payloads() {
        assert_eq!(sound_from_notify_message("toast"), None);
    }

    #[test]
    fn reload_local_client_config_refreshes_redraw_on_focus_gained() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "herdr-client-config-reload-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, "[ui]\nredraw_on_focus_gained = false\n").unwrap();
        let path_string = path.to_string_lossy().to_string();
        let _env = EnvVarGuard::set(crate::config::CONFIG_PATH_ENV_VAR, &path_string);
        let mut sound_config = crate::config::SoundConfig::default();
        let mut redraw_on_focus_gained = true;

        reload_local_client_config(&mut sound_config, &mut redraw_on_focus_gained);

        assert!(!redraw_on_focus_gained);
        let _ = std::fs::remove_file(path);
    }

    #[derive(Default)]
    struct BootstrapApi {
        requests: Vec<&'static str>,
    }

    impl supervisor::SupervisorApi for BootstrapApi {
        fn request(
            &mut self,
            request: crate::api::schema::Request,
        ) -> Result<crate::api::schema::SuccessResponse, String> {
            let result = match request.method {
                crate::api::schema::Method::RemoteList(_) => {
                    self.requests.push("remote.list");
                    crate::api::schema::ResponseResult::RemoteList {
                        remotes: Vec::new(),
                    }
                }
                crate::api::schema::Method::WorkspaceList(_) => {
                    self.requests.push("workspace.list");
                    crate::api::schema::ResponseResult::WorkspaceList {
                        workspaces: Vec::new(),
                    }
                }
                crate::api::schema::Method::AgentList(_) => {
                    self.requests.push("agent.list");
                    crate::api::schema::ResponseResult::AgentList { agents: Vec::new() }
                }
                crate::api::schema::Method::ServerUiSettings(_) => {
                    self.requests.push("server.ui_settings");
                    crate::api::schema::ResponseResult::UiSettings {
                        settings: crate::api::schema::UiSettingsInfo::default(),
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

    #[derive(Default)]
    struct RemoteAddApi {
        captured: Option<crate::api::schema::RemoteAddParams>,
    }

    impl supervisor::SupervisorApi for RemoteAddApi {
        fn request(
            &mut self,
            request: crate::api::schema::Request,
        ) -> Result<crate::api::schema::SuccessResponse, String> {
            match request.method {
                crate::api::schema::Method::RemoteAdd(params) => {
                    self.captured = Some(params);
                    Ok(crate::api::schema::SuccessResponse {
                        id: request.id,
                        result: crate::api::schema::ResponseResult::RemoteAdded {
                            remote: crate::remote_registry::RemoteDefinitionSnapshot {
                                id: "remote-1".into(),
                                name: "dev".into(),
                                target: crate::remote_registry::RemoteTargetSnapshot::Local {
                                    session: Some("dev".into()),
                                },
                                session: None,
                                keybindings:
                                    crate::remote_registry::RemoteKeybindingsSnapshot::Local,
                            },
                        },
                    })
                }
                other => Err(format!("unexpected method: {other:?}")),
            }
        }
    }

    #[test]
    fn submit_remote_add_to_main_api_builds_remote_add_request() {
        let mut api = RemoteAddApi::default();

        let remote = submit_remote_add_to_main_api(
            &mut api,
            supervisor::AddRemoteDraft {
                target: "local:dev".into(),
                name: Some("dev".into()),
                keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            },
        )
        .unwrap();

        assert_eq!(remote.id, "remote-1");
        assert_eq!(
            api.captured,
            Some(crate::api::schema::RemoteAddParams {
                name: Some("dev".into()),
                target: "local:dev".into(),
                keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            })
        );
    }

    #[test]
    fn add_remote_error_message_maps_registry_duplicates_for_modal() {
        assert_eq!(
            add_remote_error_message("remote target already exists"),
            "remote already added"
        );
        assert_eq!(
            add_remote_error_message("remote name already exists"),
            "name already used"
        );
        assert_eq!(
            add_remote_error_message("connection refused"),
            "connection refused"
        );
    }

    #[test]
    fn validate_add_remote_target_rejects_local_protocol_mismatch() {
        let err = validate_add_remote_target(
            crate::api::client::ConnectionTarget::LocalSession(Some("dev".into())),
            |_| {
                Ok(crate::api::RuntimeStatus {
                    version: Some("0.6.0".into()),
                    protocol: Some(crate::protocol::PROTOCOL_VERSION - 1),
                    capabilities: None,
                })
            },
        )
        .unwrap_err();

        assert!(err.contains("protocol mismatch"));
        assert!(err.contains(&crate::protocol::PROTOCOL_VERSION.to_string()));
    }

    #[test]
    fn validate_add_remote_target_accepts_ssh_bridge_api_socket() {
        let err = validate_add_remote_target(
            crate::api::client::ConnectionTarget::SocketPath(std::path::PathBuf::from(
                "/tmp/herdr-prod-api.sock",
            )),
            |_| {
                Ok(crate::api::RuntimeStatus {
                    version: Some("0.6.0".into()),
                    protocol: Some(crate::protocol::PROTOCOL_VERSION),
                    capabilities: None,
                })
            },
        );

        assert_eq!(err, Ok(()));
    }

    #[test]
    fn validate_add_remote_target_retries_transient_bridge_timeout() {
        let mut attempts = 0;

        let err = validate_add_remote_target(
            crate::api::client::ConnectionTarget::SocketPath(std::path::PathBuf::from(
                "/tmp/herdr-prod-api.sock",
            )),
            |_| {
                attempts += 1;
                if attempts == 1 {
                    return Err("Resource temporarily unavailable (os error 35)".to_string());
                }
                Ok(crate::api::RuntimeStatus {
                    version: Some("0.6.0".into()),
                    protocol: Some(crate::protocol::PROTOCOL_VERSION),
                    capabilities: None,
                })
            },
        );

        assert_eq!(err, Ok(()));
        assert_eq!(attempts, 2);
    }

    #[test]
    fn add_remote_target_rejects_active_local_session_as_duplicate_main() {
        let _guard = env_lock().lock().unwrap();
        let _session_env = EnvVarGuard::set(crate::session::SESSION_ENV_VAR, "dev");
        let _remote_env = EnvVarsRemovedGuard::new(&[crate::remote::MAIN_REMOTE_TARGET_ENV_VAR]);

        let target = crate::remote_registry::RemoteTargetSnapshot::Local {
            session: Some("dev".into()),
        };

        assert_eq!(
            reject_duplicate_main_target(&target),
            Err("remote already added".to_string())
        );
    }

    #[test]
    fn add_remote_target_rejects_main_remote_target_from_launch_env() {
        let _guard = env_lock().lock().unwrap();
        let _remote_env = EnvVarGuard::set(crate::remote::MAIN_REMOTE_TARGET_ENV_VAR, "iq-64");

        let target = crate::remote_registry::RemoteTargetSnapshot::Ssh {
            target: "iq-64".into(),
        };

        assert_eq!(
            reject_duplicate_main_target(&target),
            Err("remote already added".to_string())
        );
    }

    #[test]
    fn summary_refresh_subscription_request_covers_sidebar_summary_events() {
        let request = summary_refresh_subscription_request("client:summary-events");

        assert_eq!(request.id, "client:summary-events");
        let crate::api::schema::Method::EventsSubscribe(params) = request.method else {
            panic!("expected events.subscribe request");
        };
        assert_eq!(
            params.subscriptions,
            vec![
                crate::api::schema::Subscription::WorkspaceCreated {},
                crate::api::schema::Subscription::WorkspaceUpdated {},
                crate::api::schema::Subscription::WorkspaceRenamed {},
                crate::api::schema::Subscription::WorkspaceClosed {},
                crate::api::schema::Subscription::WorkspaceFocused {},
                crate::api::schema::Subscription::TabCreated {},
                crate::api::schema::Subscription::TabClosed {},
                crate::api::schema::Subscription::TabFocused {},
                crate::api::schema::Subscription::TabRenamed {},
                crate::api::schema::Subscription::PaneCreated {},
                crate::api::schema::Subscription::PaneClosed {},
                crate::api::schema::Subscription::PaneFocused {},
                crate::api::schema::Subscription::PaneExited {},
                crate::api::schema::Subscription::PaneAgentDetected {},
                crate::api::schema::Subscription::PaneAgentStatusChanged {
                    pane_id: None,
                    agent_status: None,
                },
            ]
        );
    }

    #[test]
    fn full_app_client_bootstraps_supervisor_from_main_api() {
        let mut api = BootstrapApi::default();

        let model = bootstrap_supervisor_for_client(false, &mut api)
            .unwrap()
            .expect("full app client should bootstrap supervisor");

        assert_eq!(
            api.requests,
            vec![
                "remote.list",
                "workspace.list",
                "agent.list",
                "server.ui_settings"
            ]
        );
        assert!(model.secondary_connection_plans().is_empty());
    }

    #[test]
    fn remote_launch_display_name_labels_main_filter() {
        let _guard = env_lock().lock().unwrap();
        let _display_env = EnvVarGuard::set(crate::remote::MAIN_DISPLAY_NAME_ENV_VAR, "iq-64");
        let mut api = BootstrapApi::default();

        let mut model = bootstrap_supervisor_for_client(false, &mut api)
            .unwrap()
            .expect("remote client should bootstrap supervisor");
        model.cycle_filter();

        assert_eq!(model.filter_label(), "iq-64");
    }

    #[test]
    fn direct_attach_client_skips_supervisor_bootstrap() {
        let mut api = BootstrapApi::default();

        let model = bootstrap_supervisor_for_client(true, &mut api).unwrap();

        assert!(model.is_none());
        assert!(api.requests.is_empty());
    }

    #[test]
    fn client_render_plan_uses_embedded_content_when_supervisor_is_available() {
        let model = supervisor::ClientSupervisorModel::new("local");

        let plan = client_render_plan(Some(&model), RenderEncoding::TerminalAnsi, (80, 24));

        assert_eq!(plan.surface_mode, ClientSurfaceMode::EmbeddedContent);
        assert_eq!(plan.requested_encoding, RenderEncoding::SemanticFrame);
        assert_eq!(
            plan.server_size,
            (80 - compositor::DEFAULT_SIDEBAR_WIDTH, 24)
        );
        assert!(plan.use_client_compositor);
    }

    #[test]
    fn client_render_plan_uses_full_app_when_supervisor_is_unavailable() {
        let plan = client_render_plan(None, RenderEncoding::TerminalAnsi, (80, 24));

        assert_eq!(plan.surface_mode, ClientSurfaceMode::FullApp);
        assert_eq!(plan.requested_encoding, RenderEncoding::TerminalAnsi);
        assert_eq!(plan.server_size, (80, 24));
        assert!(!plan.use_client_compositor);
    }

    #[test]
    fn client_render_plan_uses_embedded_content_with_secondary_servers() {
        let mut model = supervisor::ClientSupervisorModel::new("local");
        model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-x".into(),
            name: "x".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Local {
                session: Some("x".into()),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
        });

        let plan = client_render_plan(Some(&model), RenderEncoding::TerminalAnsi, (80, 24));

        assert_eq!(plan.surface_mode, ClientSurfaceMode::EmbeddedContent);
        assert_eq!(plan.requested_encoding, RenderEncoding::SemanticFrame);
        assert_eq!(
            plan.server_size,
            (80 - compositor::DEFAULT_SIDEBAR_WIDTH, 24)
        );
        assert!(plan.use_client_compositor);
    }

    fn mixed_remote_model() -> (supervisor::ClientSupervisorModel, supervisor::ServerId) {
        let mut model = supervisor::ClientSupervisorModel::new("local");
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
                &supervisor::ServerId::main(),
                supervisor::ServerSummary {
                    workspaces: vec![supervisor::WorkspaceSummary {
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
                supervisor::ServerSummary {
                    workspaces: vec![supervisor::WorkspaceSummary {
                        workspace_id: "remote-api".into(),
                        label: "api".into(),
                        branch: Some("feature/api".into()),
                        focused: false,
                    }],
                    agents: vec![supervisor::AgentSummary {
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
    fn composited_input_clicking_filter_cycles_sidebar_filter_without_forwarding() {
        let (mut model, _) = mixed_remote_model();
        let mut compositor = compositor::ClientCompositor::new(26);

        let dispatch = dispatch_composited_input(
            b"\x1b[<0;24;1M".to_vec(),
            &mut compositor,
            &mut model,
            (60, 16),
        );

        assert_eq!(dispatch, ClientInputDispatch::Redraw);
        assert_eq!(
            model.filter(),
            &supervisor::ServerFilter::Server(supervisor::ServerId::main())
        );
    }

    #[test]
    fn composited_input_clicking_workspace_returns_owner_api_request() {
        let (mut model, remote_id) = mixed_remote_model();
        let mut compositor = compositor::ClientCompositor::new(26);
        let row = (0..16)
            .find(|row| {
                matches!(
                    compositor.hit_test(&model, 1, *row, 60, 20),
                    Some(compositor::SidebarHitTarget::Workspace {
                        server_id,
                        workspace_id,
                    }) if server_id == remote_id && workspace_id == "remote-api"
                )
            })
            .expect("remote workspace row should be hit-testable");

        let dispatch = dispatch_composited_input(
            format!("\x1b[<0;2;{}M", row + 1).into_bytes(),
            &mut compositor,
            &mut model,
            (60, 20),
        );

        assert_eq!(
            dispatch,
            ClientInputDispatch::ApiRequest {
                server_id: remote_id.clone(),
                refresh: ClientApiRefreshPolicy::Deferred,
                request: Box::new(crate::api::schema::Request {
                    id: "client:workspace-focus".into(),
                    method: crate::api::schema::Method::WorkspaceFocus(
                        crate::api::schema::WorkspaceTarget {
                            workspace_id: "remote-api".into(),
                        },
                    ),
                }),
            }
        );
        assert_eq!(model.active_server_id(), &remote_id);
    }

    #[test]
    fn composited_input_clicking_agent_returns_owner_api_request() {
        let (mut model, remote_id) = mixed_remote_model();
        let mut compositor = compositor::ClientCompositor::new(26);

        let dispatch = dispatch_composited_input(
            b"\x1b[<0;2;12M".to_vec(),
            &mut compositor,
            &mut model,
            (60, 16),
        );

        assert_eq!(
            dispatch,
            ClientInputDispatch::ApiRequest {
                server_id: remote_id.clone(),
                refresh: ClientApiRefreshPolicy::Deferred,
                request: Box::new(crate::api::schema::Request {
                    id: "client:agent-focus".into(),
                    method: crate::api::schema::Method::AgentFocus(
                        crate::api::schema::AgentTarget {
                            target: "remote-agent".into(),
                        },
                    ),
                }),
            }
        );
        assert_eq!(model.active_server_id(), &remote_id);
    }

    #[test]
    fn composited_input_translates_content_mouse_to_embedded_viewport() {
        let (mut model, _) = mixed_remote_model();
        let mut compositor = compositor::ClientCompositor::new(26);

        let dispatch = dispatch_composited_input(
            b"\x1b[<0;28;3M".to_vec(),
            &mut compositor,
            &mut model,
            (60, 16),
        );

        assert_eq!(
            dispatch,
            ClientInputDispatch::Forward(b"\x1b[<0;2;3M".to_vec())
        );
    }

    #[test]
    fn composited_input_clicking_new_with_single_destination_returns_create_request() {
        let (mut model, _) = mixed_remote_model();
        model.set_filter(supervisor::ServerFilter::Server(
            supervisor::ServerId::main(),
        ));
        let mut compositor = compositor::ClientCompositor::new(26);

        let dispatch = dispatch_composited_input(
            b"\x1b[<0;2;8M".to_vec(),
            &mut compositor,
            &mut model,
            (60, 16),
        );

        assert_eq!(
            dispatch,
            ClientInputDispatch::ApiRequest {
                server_id: supervisor::ServerId::main(),
                refresh: ClientApiRefreshPolicy::Immediate,
                request: Box::new(crate::api::schema::Request {
                    id: "client:workspace-create".into(),
                    method: crate::api::schema::Method::WorkspaceCreate(
                        crate::api::schema::WorkspaceCreateParams {
                            cwd: None,
                            focus: true,
                            label: None,
                        },
                    ),
                }),
            }
        );
    }

    #[test]
    fn composited_input_clicking_new_with_multiple_destinations_opens_picker() {
        let (mut model, _) = mixed_remote_model();
        let mut compositor = compositor::ClientCompositor::new(26);

        let dispatch = dispatch_composited_input(
            b"\x1b[<0;2;8M".to_vec(),
            &mut compositor,
            &mut model,
            (60, 16),
        );

        assert_eq!(dispatch, ClientInputDispatch::Redraw);
        assert_eq!(
            model
                .new_workspace_picker_destinations()
                .map(|items| items.len()),
            Some(2)
        );
    }

    #[test]
    fn composited_input_clicking_picker_destination_returns_create_request() {
        let (mut model, remote_id) = mixed_remote_model();
        model.open_new_workspace_picker();
        let mut compositor = compositor::ClientCompositor::new(26);

        let dispatch = dispatch_composited_input(
            b"\x1b[<0;2;15M".to_vec(),
            &mut compositor,
            &mut model,
            (60, 16),
        );

        assert_eq!(
            dispatch,
            ClientInputDispatch::ApiRequest {
                server_id: remote_id.clone(),
                refresh: ClientApiRefreshPolicy::Immediate,
                request: Box::new(crate::api::schema::Request {
                    id: "client:workspace-create".into(),
                    method: crate::api::schema::Method::WorkspaceCreate(
                        crate::api::schema::WorkspaceCreateParams {
                            cwd: None,
                            focus: true,
                            label: None,
                        },
                    ),
                }),
            }
        );
        assert_eq!(model.active_server_id(), &remote_id);
        assert_eq!(model.new_workspace_picker_destinations(), None);
    }

    #[test]
    fn composited_input_clicking_menu_opens_client_global_menu() {
        let (mut model, _) = mixed_remote_model();
        let mut compositor = compositor::ClientCompositor::new(26);

        let dispatch = dispatch_composited_input(
            b"\x1b[<0;24;8M".to_vec(),
            &mut compositor,
            &mut model,
            (60, 16),
        );

        assert_eq!(dispatch, ClientInputDispatch::Redraw);
        assert_eq!(model.client_global_menu_highlighted(), Some(0));
    }

    #[test]
    fn composited_input_clicking_client_global_menu_dispatches_server_actions() {
        let (mut model, _) = mixed_remote_model();
        model.open_client_global_menu();
        let mut compositor = compositor::ClientCompositor::new(26);

        let settings = dispatch_composited_input(
            b"\x1b[<0;22;2M".to_vec(),
            &mut compositor,
            &mut model,
            (60, 16),
        );

        assert_eq!(
            settings,
            ClientInputDispatch::ServerControl {
                server_id: supervisor::ServerId::main(),
                message: ClientMessage::OpenSettings,
            }
        );

        model.open_client_global_menu();
        let keybinds = dispatch_composited_input(
            b"\x1b[<0;22;3M".to_vec(),
            &mut compositor,
            &mut model,
            (60, 16),
        );

        assert_eq!(
            keybinds,
            ClientInputDispatch::ServerControl {
                server_id: supervisor::ServerId::main(),
                message: ClientMessage::OpenKeybindHelp,
            }
        );

        model.open_client_global_menu();
        let reload = dispatch_composited_input(
            b"\x1b[<0;22;4M".to_vec(),
            &mut compositor,
            &mut model,
            (60, 16),
        );

        assert_eq!(
            reload,
            ClientInputDispatch::ApiRequest {
                server_id: supervisor::ServerId::main(),
                refresh: ClientApiRefreshPolicy::Immediate,
                request: Box::new(crate::api::schema::Request {
                    id: "client:reload-config".into(),
                    method: crate::api::schema::Method::ServerReloadConfig(
                        crate::api::schema::EmptyParams::default(),
                    ),
                }),
            }
        );

        model.open_client_global_menu();
        let detach = dispatch_composited_input(
            b"\x1b[<0;22;5M".to_vec(),
            &mut compositor,
            &mut model,
            (60, 16),
        );

        assert_eq!(detach, ClientInputDispatch::DetachAll);
    }

    #[test]
    fn composited_global_menu_settings_targets_and_activates_main_when_remote_is_active() {
        let (mut model, remote_id) = mixed_remote_model();
        model
            .focus_workspace_route(&remote_id, "remote-api")
            .api_request("client:workspace-focus")
            .unwrap();
        assert_eq!(model.active_server_id(), &remote_id);

        model.open_client_global_menu();
        let mut compositor = compositor::ClientCompositor::new(26);
        let dispatch = dispatch_composited_input(
            b"\x1b[<0;22;2M".to_vec(),
            &mut compositor,
            &mut model,
            (60, 16),
        );

        assert_eq!(
            dispatch,
            ClientInputDispatch::ServerControl {
                server_id: supervisor::ServerId::main(),
                message: ClientMessage::OpenSettings,
            }
        );
        assert_eq!(model.active_server_id(), &supervisor::ServerId::main());
    }

    #[test]
    fn composited_input_dragging_sidebar_divider_resizes_content() {
        let (mut model, _) = mixed_remote_model();
        let mut compositor = compositor::ClientCompositor::new(26);

        assert_eq!(
            dispatch_composited_input(
                b"\x1b[<0;26;5M".to_vec(),
                &mut compositor,
                &mut model,
                (80, 24),
            ),
            ClientInputDispatch::Redraw
        );
        assert_eq!(
            dispatch_composited_input(
                b"\x1b[<32;31;5M".to_vec(),
                &mut compositor,
                &mut model,
                (80, 24),
            ),
            ClientInputDispatch::Resize { cols: 49, rows: 24 }
        );
        assert_eq!(compositor.sidebar_width(), 31);
    }

    #[test]
    fn composited_client_keeps_mouse_capture_enabled_for_sidebar() {
        assert!(desired_mouse_capture(false, true));
        assert!(desired_mouse_capture(true, true));
        assert!(desired_mouse_capture(true, false));
        assert!(!desired_mouse_capture(false, false));
    }

    #[test]
    fn composited_input_add_remote_form_submits_draft() {
        let (mut model, _) = mixed_remote_model();
        model.open_add_remote_form();
        let mut compositor = compositor::ClientCompositor::new(12);

        assert_eq!(
            dispatch_composited_input(b"local:dev".to_vec(), &mut compositor, &mut model, (24, 8)),
            ClientInputDispatch::Redraw
        );
        assert_eq!(
            dispatch_composited_input(b"\tdev".to_vec(), &mut compositor, &mut model, (24, 8)),
            ClientInputDispatch::Redraw
        );

        let dispatch =
            dispatch_composited_input(b"\r".to_vec(), &mut compositor, &mut model, (24, 8));

        assert_eq!(
            dispatch,
            ClientInputDispatch::AddRemote(supervisor::AddRemoteDraft {
                target: "local:dev".into(),
                name: Some("dev".into()),
                keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
            })
        );
    }

    #[test]
    fn api_target_for_supervisor_server_maps_main_and_local_secondary() {
        let (model, remote_id) = mixed_remote_model();

        assert_eq!(
            api_target_for_supervisor_server(
                &model,
                &supervisor::ServerId::main(),
                &HashMap::new()
            ),
            Some(crate::api::client::ConnectionTarget::LocalSession(None))
        );
        assert_eq!(
            api_target_for_supervisor_server(&model, &remote_id, &HashMap::new()),
            Some(crate::api::client::ConnectionTarget::LocalSession(Some(
                "x".into()
            )))
        );
    }

    #[test]
    fn supervisor_targets_map_ssh_secondary_through_bridge_sockets() {
        let mut model = supervisor::ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(crate::remote_registry::RemoteDefinitionSnapshot {
            id: "remote-prod".into(),
            name: "prod".into(),
            target: crate::remote_registry::RemoteTargetSnapshot::Ssh {
                target: "prod.example.com".into(),
            },
            session: None,
            keybindings: crate::remote_registry::RemoteKeybindingsSnapshot::Local,
        });
        let api_socket = std::path::PathBuf::from("/tmp/herdr-prod-api.sock");
        let client_socket = std::path::PathBuf::from("/tmp/herdr-prod-client.sock");
        let bridge = crate::remote::RemoteBridge::from_socket_paths_for_test(
            client_socket.clone(),
            api_socket.clone(),
        );
        let ssh_bridges = HashMap::from([(remote_id.clone(), bridge)]);

        assert_eq!(
            api_target_for_supervisor_server(&model, &remote_id, &ssh_bridges),
            Some(crate::api::client::ConnectionTarget::SocketPath(api_socket))
        );
        assert_eq!(
            client_socket_path_for_supervisor_server(&model, &remote_id, &ssh_bridges),
            Some(client_socket)
        );
    }

    #[test]
    fn supervisor_summary_refresh_due_uses_two_second_interval() {
        let start = Instant::now();

        assert!(!supervisor_summary_refresh_due(
            start + Duration::from_millis(1999),
            start
        ));
        assert!(supervisor_summary_refresh_due(
            start + Duration::from_secs(2),
            start
        ));
    }

    #[test]
    fn secondary_retry_delay_uses_conservative_backoff_schedule() {
        assert_eq!(secondary_retry_delay(0), Duration::from_secs(1));
        assert_eq!(secondary_retry_delay(1), Duration::from_secs(2));
        assert_eq!(secondary_retry_delay(2), Duration::from_secs(5));
        assert_eq!(secondary_retry_delay(3), Duration::from_secs(15));
        assert_eq!(secondary_retry_delay(8), Duration::from_secs(15));
    }

    #[test]
    fn client_socket_path_for_connection_target_maps_local_sessions_only() {
        let named = client_socket_path_for_connection_target(
            &supervisor::ServerConnectionTarget::LocalSession(Some("work".into())),
        )
        .unwrap();
        assert!(named.ends_with("sessions/work/herdr-client.sock"));

        let default =
            client_socket_path_for_connection_target(&supervisor::ServerConnectionTarget::Main)
                .unwrap();
        assert!(default.ends_with("herdr-client.sock"));

        assert_eq!(
            client_socket_path_for_connection_target(&supervisor::ServerConnectionTarget::Ssh(
                "host".into()
            )),
            None
        );
    }

    fn test_frame(width: u16) -> protocol::FrameData {
        protocol::FrameData {
            cells: Vec::new(),
            width,
            height: 1,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        }
    }

    #[test]
    fn select_composited_render_frame_requires_active_server_cache() {
        let main = supervisor::ServerId::main();
        let remote = supervisor::ServerId::secondary("remote-x");
        let mut frames = std::collections::HashMap::new();
        frames.insert(main.clone(), test_frame(10));
        frames.insert(remote.clone(), test_frame(20));

        assert_eq!(
            select_composited_render_frame(&frames, &remote, &main)
                .unwrap()
                .width,
            20
        );

        let missing = supervisor::ServerId::secondary("missing");
        assert_eq!(
            select_composited_render_frame(&frames, &missing, &main),
            None
        );
    }

    #[test]
    fn secondary_write_failure_disconnects_server_without_failing_client() {
        let now = Instant::now();
        let mut model = supervisor::ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(test_remote_definition("remote-x", "x"));
        model.set_active_server(remote_id.clone()).unwrap();
        let mut state = test_client_state_with_model(model);
        state.frame_cache.insert(remote_id.clone(), test_frame(8));
        state
            .summary_subscription_server_ids
            .insert(remote_id.clone());
        let mut server_writes = HashMap::new();

        let result = handle_server_write_failure(
            &mut state,
            &mut server_writes,
            remote_id.clone(),
            io::Error::new(io::ErrorKind::BrokenPipe, "secondary closed"),
            now,
        );

        assert!(result.is_ok());
        assert!(!state.frame_cache.contains_key(&remote_id));
        assert!(!state.summary_subscription_server_ids.contains(&remote_id));
        assert_eq!(
            state.supervisor_model.as_ref().unwrap().active_server_id(),
            &supervisor::ServerId::main()
        );
        assert_eq!(
            state
                .secondary_retries
                .get(&remote_id)
                .map(|retry| retry.next_retry_at),
            Some(now + secondary_retry_delay(0))
        );
    }

    #[test]
    fn main_write_failure_still_fails_client() {
        let mut state =
            test_client_state_with_model(supervisor::ClientSupervisorModel::new("local"));
        let mut server_writes = HashMap::new();

        let result = handle_server_write_failure(
            &mut state,
            &mut server_writes,
            supervisor::ServerId::main(),
            io::Error::new(io::ErrorKind::BrokenPipe, "main closed"),
            Instant::now(),
        );

        assert!(matches!(result, Err(ClientError::ConnectionLost(_))));
        assert!(state.secondary_retries.is_empty());
    }

    #[test]
    fn schedule_missing_secondary_stream_retries_includes_new_connecting_servers() {
        let now = Instant::now();
        let mut model = supervisor::ClientSupervisorModel::new("local");
        model.sync_remote_registry(vec![test_remote_definition("remote-x", "x")]);
        let remote_id = supervisor::ServerId::secondary("remote-x");
        let mut state = test_client_state_with_model(model);
        let server_writes = HashMap::new();

        schedule_missing_secondary_stream_retries(&mut state, &server_writes, now);

        assert_eq!(
            state
                .secondary_retries
                .get(&remote_id)
                .map(|retry| retry.next_retry_at),
            Some(now + secondary_retry_delay(0))
        );
    }

    #[test]
    fn schedule_missing_secondary_stream_retries_includes_connected_server_without_stream() {
        let now = Instant::now();
        let mut model = supervisor::ClientSupervisorModel::new("local");
        let remote_id = model.add_secondary(test_remote_definition("remote-x", "x"));
        let mut state = test_client_state_with_model(model);
        let server_writes = HashMap::new();

        schedule_missing_secondary_stream_retries(&mut state, &server_writes, now);

        assert_eq!(
            state
                .secondary_retries
                .get(&remote_id)
                .map(|retry| retry.next_retry_at),
            Some(now + secondary_retry_delay(0))
        );
    }

    #[test]
    fn summary_subscription_end_guard_sends_ended_event_on_drop() {
        let server_id = supervisor::ServerId::secondary("remote-x");
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);

        drop(SummarySubscriptionEndGuard {
            server_id: server_id.clone(),
            event_tx: tx,
        });

        let event = rx.blocking_recv().expect("subscription ended event");
        assert!(matches!(
            event,
            ClientLoopEvent::SupervisorSummarySubscriptionEnded(id) if id == server_id
        ));
    }

    #[test]
    fn toast_notify_from_server_is_emitted_even_when_attach_config_was_off() {
        let sound_config = crate::config::SoundConfig::default();
        let mut emitted = None;

        handle_notify_with_notifiers(
            NotifyKind::Toast,
            "pi finished: workspace 1",
            &sound_config,
            |title, body| {
                emitted = Some((title.to_string(), body.map(str::to_string)));
                Ok(true)
            },
            |_, _| Ok(false),
        );

        assert_eq!(
            emitted,
            Some(("pi finished".to_string(), Some("workspace 1".to_string())))
        );
    }

    #[test]
    fn system_toast_notify_from_server_uses_system_notifier() {
        let sound_config = crate::config::SoundConfig::default();
        let mut emitted = None;

        handle_notify_with_notifiers(
            NotifyKind::SystemToast,
            "pi finished: workspace 1",
            &sound_config,
            |_, _| Ok(false),
            |title, body| {
                emitted = Some((title.to_string(), body.map(str::to_string)));
                Ok(true)
            },
        );

        assert_eq!(
            emitted,
            Some(("pi finished".to_string(), Some("workspace 1".to_string())))
        );
    }

    #[test]
    fn decode_clipboard_payload_decodes_base64() {
        assert_eq!(decode_clipboard_payload("dGVzdA=="), Some(b"test".to_vec()));
    }

    #[test]
    fn decode_clipboard_payload_rejects_invalid_base64() {
        assert_eq!(decode_clipboard_payload("not-base64!!!"), None);
    }

    #[test]
    fn forward_clipboard_uses_local_clipboard_path() {
        unsafe {
            std::env::set_var("SSH_CONNECTION", "1 2 3 4");
        }
        forward_clipboard("dGVzdA==");
        unsafe {
            std::env::remove_var("SSH_CONNECTION");
        }
    }
}
