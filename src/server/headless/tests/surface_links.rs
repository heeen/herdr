use super::*;

use crate::protocol::surface_links;

/// The surface a capable client ends up with: its baseline with every linked patch applied.
struct ShellSurface {
    frame: FrameData,
}

impl ShellSurface {
    fn new(surface: &protocol::PaneSurfaceFrame) -> Self {
        Self {
            frame: surface.frame.clone(),
        }
    }

    /// Applies one server message the way the client shell will, and says what kind it was.
    fn apply(&mut self, message: ServerMessage) -> &'static str {
        match message {
            ServerMessage::EndpointControl { kind, data }
                if kind == surface_links::MESSAGE_KIND =>
            {
                let (patch, hyperlinks) = surface_links::decode(&data).expect("linked patch");
                surface_links::apply(&mut self.frame, &patch.rows, hyperlinks)
                    .expect("the patch fits the surface it was planned against");
                self.frame.cursor = patch.cursor;
                "linked"
            }
            ServerMessage::PaneSurfacePatch(patch) => {
                for row in &patch.rows {
                    let start =
                        usize::from(row.y) * usize::from(self.frame.width) + usize::from(row.x);
                    self.frame.cells[start..start + row.cells.len()].clone_from_slice(&row.cells);
                }
                self.frame.cursor = patch.cursor;
                "plain"
            }
            ServerMessage::PaneSurface(surface) => {
                self.frame = surface.frame;
                "full"
            }
            other => panic!("unexpected surface message: {other:?}"),
        }
    }
}

/// Every link on a surface, as uris placed at the cells that carry them.
fn frame_links(frame: &FrameData) -> Vec<((u16, u16), &str)> {
    let width = usize::from(frame.width);
    frame
        .cells
        .iter()
        .enumerate()
        .filter_map(|(index, cell)| {
            let uri = frame
                .hyperlinks
                .get(usize::try_from(cell.hyperlink?).ok()?)?;
            let (x, y) = (index % width, index / width);
            Some((
                (u16::try_from(x).ok()?, u16::try_from(y).ok()?),
                uri.as_str(),
            ))
        })
        .collect()
}

/// Points at the first difference rather than dumping two whole surfaces.
fn assert_same_frame(client: &FrameData, server: &FrameData, context: &str) {
    assert_eq!(
        (client.width, client.height),
        (server.width, server.height),
        "{context}: geometry"
    );
    let width = usize::from(client.width);
    for (index, (theirs, ours)) in client.cells.iter().zip(&server.cells).enumerate() {
        assert_eq!(
            (
                &theirs.symbol,
                theirs
                    .hyperlink
                    .and_then(|i| client.hyperlinks.get(i as usize))
            ),
            (
                &ours.symbol,
                ours.hyperlink
                    .and_then(|i| server.hyperlinks.get(i as usize))
            ),
            "{context}: cell at ({}, {})",
            index % width,
            index / width
        );
    }
    assert_eq!(
        client.hyperlinks, server.hyperlinks,
        "{context}: link tables"
    );
    assert_eq!(client, server, "{context}: frames");
}

fn server_surface(server: &HeadlessServer, client_id: u64) -> &protocol::PaneSurfaceFrame {
    server.clients[&client_id]
        .render_state
        .last_pane_surface()
        .expect("client baseline")
}

/// Drains everything the client was sent, applying it to its surface.
fn apply_pending(
    shell: &mut ShellSurface,
    render: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> Vec<&'static str> {
    render
        .try_iter()
        .map(|frame| shell.apply(read_server_message(frame)))
        .collect()
}

#[tokio::test]
async fn a_capable_shell_keeps_patching_while_a_url_scrolls() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (_control, render) = connect_linked_test_shell(&mut server, 7);
    // Fill the pane first, so every further line scrolls the url up a row instead of landing
    // under it on an empty screen.
    for filler in 0..30 {
        write_shared_test_pane(
            &mut server,
            pane_id,
            format!("filler-{filler}\r\n").as_bytes(),
        );
    }
    server.render_and_stream();
    let mut shell = ShellSurface::new(&recv_pane_surface(&render, "baseline"));

    let sources = HashSet::from([pane_id]);
    let url = "https://example.com/build/7/artifacts/output/log.txt";
    let mut kinds = Vec::new();
    for step in 0..12 {
        let bytes = if step == 0 {
            format!("see {url}\r\n")
        } else {
            format!("line-{step}\r\n")
        };
        write_shared_test_pane(&mut server, pane_id, bytes.as_bytes());
        assert!(
            server.render_retained_pane_surface_and_stream(&sources),
            "step {step} must stay on a patch"
        );
        kinds.extend(apply_pending(&mut shell, &render));
    }

    assert!(
        !kinds.contains(&"full"),
        "a url on screen must not cost a full surface: {kinds:?}"
    );
    assert_eq!(
        kinds.first(),
        Some(&"linked"),
        "the update that brings the url carries the table it needs"
    );
    assert!(
        kinds[1..].iter().all(|kind| *kind == "plain"),
        "later updates renumber nothing, so they stay on the cheaper patch: {kinds:?}"
    );
    assert_same_frame(
        &shell.frame,
        &server_surface(&server, 7).frame,
        "client vs server",
    );
    let links = frame_links(&shell.frame);
    assert!(
        links.iter().any(|(_, uri)| *uri == url),
        "the url stays clickable while output scrolls under it"
    );
    assert!(
        links.iter().all(|((_, y), _)| *y < 11),
        "the url has scrolled up the pane: {links:?}"
    );

    // A full render now has nothing to send, which is only true if every patch left the surface
    // exactly where a full render would have put it.
    server.render_and_stream();
    assert!(
        render.try_recv().is_err(),
        "the patched surface already equals a full render"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn a_capable_shell_drops_links_that_scroll_away() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (_control, render) = connect_linked_test_shell(&mut server, 7);
    server.render_and_stream();
    let mut shell = ShellSurface::new(&recv_pane_surface(&render, "baseline"));

    let sources = HashSet::from([pane_id]);
    write_shared_test_pane(
        &mut server,
        pane_id,
        b"\r\x1b]8;;https://example.com/gone\x1b\\linked\x1b]8;;\x1b\\\r\n",
    );
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    apply_pending(&mut shell, &render);
    assert!(!frame_links(&shell.frame).is_empty(), "the link arrived");

    for step in 0..40 {
        write_shared_test_pane(&mut server, pane_id, format!("line-{step}\r\n").as_bytes());
        assert!(server.render_retained_pane_surface_and_stream(&sources));
        apply_pending(&mut shell, &render);
    }

    assert!(
        frame_links(&shell.frame).is_empty(),
        "a link scrolled off the top is gone from the surface"
    );
    assert_same_frame(
        &shell.frame,
        &server_surface(&server, 7).frame,
        "client vs server",
    );
    server.render_and_stream();
    assert!(render.try_recv().is_err(), "nothing left for a full render");
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn a_wrapped_url_links_the_clean_row_it_continues_onto() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (_control, render) = connect_linked_test_shell(&mut server, 7);
    server.render_and_stream();
    let mut shell = ShellSurface::new(&recv_pane_surface(&render, "baseline"));

    let sources = HashSet::from([pane_id]);
    // Long enough to wrap the 80 column pane, written in two halves so the second half lands as a
    // patch while the first half's row stays clean.
    let head = "https://example.com/very/long/path/that/keeps/going/until/it/wraps/around/th";
    write_shared_test_pane(&mut server, pane_id, format!("\r{head}").as_bytes());
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    apply_pending(&mut shell, &render);

    write_shared_test_pane(&mut server, pane_id, b"e-edge/end.txt");
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    apply_pending(&mut shell, &render);

    let links = frame_links(&shell.frame);
    let full = format!("{head}e-edge/end.txt");
    assert!(
        links.iter().all(|(_, uri)| *uri == full),
        "every linked cell carries the whole wrapped url: {links:?}"
    );
    assert!(
        links.iter().any(|((_, y), _)| *y > links[0].0 .1),
        "the url reaches the row it wrapped onto"
    );
    assert_same_frame(
        &shell.frame,
        &server_surface(&server, 7).frame,
        "client vs server",
    );
    server.render_and_stream();
    assert!(render.try_recv().is_err(), "nothing left for a full render");
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn output_without_links_stays_on_plain_patches() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (_control, render) = connect_linked_test_shell(&mut server, 7);
    server.render_and_stream();
    let mut shell = ShellSurface::new(&recv_pane_surface(&render, "baseline"));

    let sources = HashSet::from([pane_id]);
    write_shared_test_pane(&mut server, pane_id, b"plain output\r\n");
    assert!(server.render_retained_pane_surface_and_stream(&sources));

    assert_eq!(
        apply_pending(&mut shell, &render),
        ["plain"],
        "a capable client still gets the cheaper message when nothing is linked"
    );
    assert!(shell.frame.hyperlinks.is_empty());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn a_legacy_shell_still_falls_back_for_url_output() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (_control, render) = connect_matching_test_shell(&mut server, 7);
    server.render_and_stream();
    let _ = recv_pane_surface(&render, "baseline");

    write_shared_test_pane(&mut server, pane_id, b"\rsee https://example.com/x\r\n");

    assert!(!server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn one_legacy_recipient_keeps_every_recipient_on_full_surfaces() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (_capable_control, capable_render) = connect_linked_test_shell(&mut server, 7);
    let (_legacy_control, legacy_render) = connect_matching_test_shell(&mut server, 8);
    server.render_and_stream();
    let _ = recv_pane_surface(&capable_render, "capable baseline");
    let _ = recv_pane_surface(&legacy_render, "legacy baseline");

    write_shared_test_pane(&mut server, pane_id, b"\rsee https://example.com/x\r\n");

    assert!(!server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    assert!(
        capable_render.try_recv().is_err(),
        "a refused plan sends nothing to anyone"
    );
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn a_clean_row_is_relinked_when_a_wrapped_url_grows() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (_control, render) = connect_linked_test_shell(&mut server, 7);

    // Already wrapped before the baseline, so the row holding the scheme stays clean while the
    // row past the wrap is rewritten.
    let head = "https://example.com/very/long/path/that/keeps/going/until/it/wraps/past/the/edge/x";
    write_shared_test_pane(&mut server, pane_id, format!("\r{head}").as_bytes());
    server.render_and_stream();
    let mut shell = ShellSurface::new(&recv_pane_surface(&render, "wrapped baseline"));
    let before = frame_links(&shell.frame).len();

    write_shared_test_pane(&mut server, pane_id, b"yz");
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    apply_pending(&mut shell, &render);

    let links = frame_links(&shell.frame);
    let full = format!("{head}yz");
    assert!(links.len() > before, "the url grew by two cells");
    assert!(
        links.iter().all(|(_, uri)| *uri == full),
        "the row that stayed clean now points at the longer url: {links:?}"
    );
    assert_same_frame(
        &shell.frame,
        &server_surface(&server, 7).frame,
        "client vs server",
    );
    server.render_and_stream();
    assert!(render.try_recv().is_err(), "nothing left for a full render");
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn two_urls_are_numbered_the_way_a_full_render_numbers_them() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (_control, render) = connect_linked_test_shell(&mut server, 7);
    server.render_and_stream();
    let mut shell = ShellSurface::new(&recv_pane_surface(&render, "baseline"));

    // The first url starts further right than the second, so walking columns before rows would
    // number them the other way round.
    write_shared_test_pane(
        &mut server,
        pane_id,
        b"\r          https://first.test\r\nhttps://second.test\r\n",
    );
    assert!(server.render_retained_pane_surface_and_stream(&HashSet::from([pane_id])));
    apply_pending(&mut shell, &render);

    assert_eq!(
        shell.frame.hyperlinks,
        ["https://first.test", "https://second.test"],
        "the table follows the order a full render meets the links in"
    );
    assert_same_frame(
        &shell.frame,
        &server_surface(&server, 7).frame,
        "client vs server",
    );
    server.render_and_stream();
    assert!(render.try_recv().is_err(), "nothing left for a full render");
    shutdown_test_runtimes(&mut server);
}

/// A row that changes nothing of its own while the url wrapping onto it comes and goes.
///
/// Overwriting the head of a wrapped line turns the whole line into a url and back, so the second
/// row gains and loses a link without a single cell of it being rewritten.
#[tokio::test]
async fn a_clean_row_follows_a_url_it_gains_and_loses() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (_control, render) = connect_linked_test_shell(&mut server, 7);

    let tail = "example.com/wrapped/past/the/edge";
    write_shared_test_pane(
        &mut server,
        pane_id,
        format!("\rxxxxxxxx{}{tail}", "y".repeat(72 - tail.len())).as_bytes(),
    );
    server.render_and_stream();
    let mut shell = ShellSurface::new(&recv_pane_surface(&render, "unlinked baseline"));
    assert!(frame_links(&shell.frame).is_empty(), "nothing is a url yet");

    // Drain the dirt the first write left behind, so the rows below only turn up again if their
    // links made them.
    let sources = HashSet::from([pane_id]);
    server.render_retained_pane_surface_and_stream(&sources);
    apply_pending(&mut shell, &render);

    write_shared_test_pane(&mut server, pane_id, b"\x1b[1;1Hhttps://");
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    apply_pending(&mut shell, &render);

    let gained = frame_links(&shell.frame);
    assert!(
        gained.iter().any(|((_, y), _)| *y == 1),
        "the row the url wrapped onto is linked without having changed: {gained:?}"
    );
    assert_same_frame(
        &shell.frame,
        &server_surface(&server, 7).frame,
        "after gaining",
    );

    write_shared_test_pane(&mut server, pane_id, b"\x1b[1;1Hxxxxxxxx");
    assert!(server.render_retained_pane_surface_and_stream(&sources));
    apply_pending(&mut shell, &render);

    assert!(
        frame_links(&shell.frame).is_empty(),
        "and it is unlinked again once the scheme goes away"
    );
    assert_same_frame(
        &shell.frame,
        &server_surface(&server, 7).frame,
        "after losing",
    );
    server.render_and_stream();
    assert!(render.try_recv().is_err(), "nothing left for a full render");
    shutdown_test_runtimes(&mut server);
}

/// Turning url detection off has to take the links off the screen, not leave them until the pane
/// happens to produce output again.
#[tokio::test]
async fn turning_url_detection_off_unlinks_a_pane_that_is_not_producing_output() {
    let mut server = test_headless_server();
    let pane_id = install_shared_view_test_runtime(&mut server);
    let (_control, render) = connect_linked_test_shell(&mut server, 7);
    write_shared_test_pane(&mut server, pane_id, b"\rsee https://example.com/x\r\n");
    server.render_and_stream();
    let mut shell = ShellSurface::new(&recv_pane_surface(&render, "baseline"));
    assert!(!frame_links(&shell.frame).is_empty(), "the url is linked");

    server.app.state.detect_urls = false;
    server.render_and_stream();
    apply_pending(&mut shell, &render);

    assert!(
        frame_links(&shell.frame).is_empty(),
        "a full render after the reload drops the detected links"
    );
    assert_same_frame(
        &shell.frame,
        &server_surface(&server, 7).frame,
        "client vs server",
    );
    shutdown_test_runtimes(&mut server);
}
