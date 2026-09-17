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

/// Places a pane program's clipboard write on the user's selection and confirms it in the shell.
pub(super) fn apply_pane_clipboard_write(
    state: &mut ClientState,
    target: crate::platform::SelectionTarget,
    data: &str,
) {
    if forward_clipboard(target, data) {
        let (width, height) = state.reported_size;
        let frame = state.shell.as_mut().and_then(|shell| {
            shell
                .show_copy_feedback(target, std::time::Instant::now())
                .then(|| shell.compose(width, height))
                .flatten()
        });
        if let Some(frame) = frame {
            state.present_frame(frame);
        }
    }
    let _ = io::stdout().flush();
}
