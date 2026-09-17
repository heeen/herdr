use super::*;

pub(super) fn decode_clipboard_payload(data: &str) -> Option<Vec<u8>> {
    use base64::Engine;

    base64::engine::general_purpose::STANDARD.decode(data).ok()
}

pub(super) fn forward_clipboard(target: crate::platform::SelectionTarget, data: &str) -> bool {
    let Some(bytes) = decode_clipboard_payload(data) else {
        warn!("received invalid clipboard payload from server");
        return false;
    };
    crate::selection::write_osc52_bytes(target, &bytes);
    true
}

/// Writes every selection the route asks for, reporting whether any write succeeded. `any` would
/// stop at the first success and leave the rest of a multi-selection route unwritten.
fn write_selections(
    targets: &[crate::platform::SelectionTarget],
    mut write: impl FnMut(crate::platform::SelectionTarget) -> bool,
) -> bool {
    let mut written = false;
    for target in targets {
        written |= write(*target);
    }
    written
}

/// Places a pane program's clipboard write on the user's selections and confirms it in the shell.
/// `ui.clipboard.agents` may route an agent's writes somewhere other than the selection its
/// program addressed.
pub(super) fn apply_pane_clipboard_write(
    state: &mut ClientState,
    requested: crate::platform::SelectionTarget,
    agent: Option<&str>,
    data: &str,
) {
    let targets = state.clipboard_config.targets_for(requested, agent);
    if write_selections(targets, |target| forward_clipboard(target, data)) {
        let (width, height) = state.reported_size;
        let frame = state.shell.as_mut().and_then(|shell| {
            shell
                .show_copy_feedback(targets, std::time::Instant::now())
                .then(|| shell.compose(width, height))
                .flatten()
        });
        if let Some(frame) = frame {
            state.present_frame(frame);
        }
    }
    let _ = io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::SelectionTarget::{Clipboard, Primary};

    #[test]
    fn every_selection_in_a_route_is_written() {
        let mut seen = Vec::new();
        assert!(write_selections(&[Clipboard, Primary], |target| {
            seen.push(target);
            true
        }));
        assert_eq!(seen, [Clipboard, Primary]);

        let mut seen = Vec::new();
        assert!(write_selections(&[Clipboard, Primary], |target| {
            seen.push(target);
            // A failing clipboard tool must not stop the remaining selections.
            target == Primary
        }));
        assert_eq!(seen, [Clipboard, Primary]);

        assert!(!write_selections(&[Clipboard], |_| false));
    }
}
