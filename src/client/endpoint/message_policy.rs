use crate::protocol::ServerMessage;

/// Effects that are meaningful only for the endpoint currently holding the host presentation
/// lease. During a frozen handoff they are intentionally dropped and the committed target asks
/// for one bounded replay, rather than allowing target graphics or modes onto the source frame.
pub(crate) fn is_presentation_effect(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::MouseCapture { .. }
            | ServerMessage::ClientShellKeyboardReportAll { .. }
            | ServerMessage::WindowTitle { .. }
            | ServerMessage::Graphics { .. }
            | ServerMessage::GraphicsFile { .. }
            | ServerMessage::GraphicsTransmissionRetired { .. }
            | ServerMessage::TerminalBell { .. }
    )
}

pub(crate) fn accepts_endpoint_message(
    endpoint_active: bool,
    activation_message: bool,
    command_response: bool,
    message: &ServerMessage,
) -> bool {
    endpoint_active
        || matches!(
            message,
            ServerMessage::EndpointControl { kind, .. }
                // A clipboard write is a host effect like `ServerMessage::Clipboard`, and a linked
                // patch updates the surface the user is looking at: neither may arrive from an
                // endpoint in the background.
                if kind != crate::protocol::endpoint::CLIPBOARD_WRITE_KIND
                    && kind != crate::protocol::surface_links::MESSAGE_KIND
        )
        || matches!(
            message,
            ServerMessage::SemanticNotification(_)
                | ServerMessage::ServerShutdown { .. }
                | ServerMessage::ClientShellSnapshot(_)
                | ServerMessage::Welcome { .. }
        )
        || (activation_message
            && matches!(
                message,
                ServerMessage::PaneSurface(_)
                    | ServerMessage::ClientShellEndpointResponseChunk { .. }
            ))
        || (command_response
            && matches!(
                message,
                ServerMessage::ClientShellEndpointResponseChunk { .. }
            ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inactive_endpoints_deliver_metadata_but_not_presentation_effects() {
        assert!(accepts_endpoint_message(
            false,
            false,
            false,
            &ServerMessage::EndpointControl {
                kind: "snapshot".into(),
                data: "{}".into(),
            }
        ));
        assert!(!accepts_endpoint_message(
            false,
            false,
            false,
            &ServerMessage::Clipboard {
                data: "text".into()
            }
        ));
        let clipboard_write = ServerMessage::EndpointControl {
            kind: crate::protocol::endpoint::CLIPBOARD_WRITE_KIND.into(),
            data: "{}".into(),
        };
        assert!(!accepts_endpoint_message(
            false,
            false,
            false,
            &clipboard_write
        ));
        assert!(accepts_endpoint_message(
            true,
            false,
            false,
            &clipboard_write
        ));
        let linked_patch = ServerMessage::EndpointControl {
            kind: crate::protocol::surface_links::MESSAGE_KIND.into(),
            data: "{}".into(),
        };
        assert!(!accepts_endpoint_message(
            false,
            false,
            false,
            &linked_patch
        ));
        assert!(accepts_endpoint_message(true, false, false, &linked_patch));
        assert!(!accepts_endpoint_message(
            false,
            false,
            false,
            &ServerMessage::WindowTitle {
                title: Some("remote".into())
            }
        ));
    }

    #[test]
    fn frozen_handoffs_drop_target_effects_until_the_post_commit_resync() {
        assert!(is_presentation_effect(&ServerMessage::MouseCapture {
            enabled: true,
            sgr_pixels: false,
        }));
        assert!(is_presentation_effect(&ServerMessage::GraphicsFile {
            path: "asset".into(),
            expected_len: 1,
            image_id: 1,
            transfer_id: 1,
            leading: Vec::new(),
            control: "a=t".into(),
            surface_asset: None,
        }));
        assert!(!is_presentation_effect(&ServerMessage::EndpointControl {
            kind: "shell.snapshot.v1".into(),
            data: "{}".into(),
        }));
    }

    #[test]
    fn pending_activation_accepts_only_its_surface_lane() {
        assert!(accepts_endpoint_message(
            false,
            true,
            false,
            &ServerMessage::ClientShellEndpointResponseChunk {
                boot_id: "boot".into(),
                request_id: "surface".into(),
                final_chunk: true,
                data: Vec::new(),
            }
        ));
        assert!(!accepts_endpoint_message(
            false,
            true,
            false,
            &ServerMessage::MouseCapture {
                enabled: true,
                sgr_pixels: false,
            }
        ));
    }
}
