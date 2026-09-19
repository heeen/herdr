use super::*;

/// What one scenario writes into every pane before each update.
#[derive(Clone, Copy, Debug)]
enum LinkScenario {
    /// Plain output scrolling, with no urls anywhere.
    Plain,
    /// Output scrolling with a wrapped url every tenth line, so one is almost always on screen.
    UrlScrolling,
    /// A url printed once, then typing on another row, so nothing scrolls.
    UrlStaticTyping,
}

impl LinkScenario {
    fn setup(self) -> &'static [u8] {
        match self {
            Self::Plain | Self::UrlScrolling => b"",
            Self::UrlStaticTyping => {
                b"see https://example.com/build/123/artifacts/output/very/long/log.txt\r\n"
            }
        }
    }

    fn update(self, sample: usize) -> Vec<u8> {
        match self {
            Self::Plain => format!("line-{sample}\r\n").into_bytes(),
            Self::UrlScrolling if sample.is_multiple_of(10) => format!(
                "see https://example.com/build/{sample}/artifacts/output/very/long/log.txt\r\n"
            )
            .into_bytes(),
            Self::UrlScrolling => format!("line-{sample}\r\n").into_bytes(),
            Self::UrlStaticTyping => format!("\x1b[20;1Htyped-{sample}").into_bytes(),
        }
    }
}

#[tokio::test]
#[ignore = "manual retained patch profile with urls on screen"]
async fn render_scale_profile_retained_links() {
    use ratatui::layout::Direction;

    for scenario in [
        LinkScenario::Plain,
        LinkScenario::UrlScrolling,
        LinkScenario::UrlStaticTyping,
    ] {
        for count in [1, 4, 15] {
            let (mut server, client_rx, root) = retained_test_server(b"");
            let mut pane_ids = vec![root];
            for index in 1..count {
                let workspace = &mut server.app.state.workspaces[0];
                workspace.tabs[0]
                    .layout
                    .focus_pane(pane_ids[(index - 1) / 2]);
                let id = workspace.test_split(if index % 2 == 0 {
                    Direction::Vertical
                } else {
                    Direction::Horizontal
                });
                workspace.insert_test_runtime(
                    id,
                    crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
                );
                pane_ids.push(id);
            }
            let client = server.clients.get_mut(&1).unwrap();
            client.mode = ClientConnectionMode::ClientShell;
            for id in &pane_ids {
                write_shared_test_pane(&mut server, *id, scenario.setup());
            }
            server.render_and_stream();
            let _ = client_rx.try_iter().count();

            let sources = pane_ids.iter().copied().collect();
            let mut samples = Vec::new();
            let mut retained = 0usize;
            let mut bytes = 0usize;
            for sample in 0..110 {
                for id in &pane_ids {
                    write_shared_test_pane(&mut server, *id, &scenario.update(sample));
                }
                // The same sequence the server runs: try the retained patch, fall back to a full
                // render when it refuses.
                let started = Instant::now();
                let patched = server.render_retained_pane_surface_and_stream(&sources);
                if !patched {
                    server.render_and_stream();
                }
                let elapsed = started.elapsed();
                let sent: usize = client_rx.try_iter().map(|frame| frame.len()).sum();
                if sample >= 10 {
                    samples.push(elapsed);
                    retained += usize::from(patched);
                    bytes += sent;
                }
            }
            samples.sort_unstable();
            println!(
                "retained_links scenario={scenario:?} panes={count} retained={retained}/100 median_us={} p95_us={} bytes_per_update={}",
                samples[50].as_micros(),
                samples[94].as_micros(),
                bytes / 100,
            );
        }
    }
}
