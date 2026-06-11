# what's different

herdr-mx = upstream [herdr](https://github.com/ogulcancelik/herdr) + the changes on this page. nothing else.

currently tracking: **upstream v0.6.10** (released 2026-06-11). policy: every upstream release is merged within days, mx releases are tagged `v<upstream>-mx.<n>`, and anything on this page is offered upstream when it fits — when a feature lands upstream it leaves this page. when *everything* lands upstream, herdr-mx retires.

## the big one: multi-remote client

upstream herdr attaches to one server at a time (`herdr --remote <host>` per terminal). herdr-mx makes the client a **fleet console**:

- **one sidebar, every machine** — register secondary local or SSH-backed herdr servers; their workspaces and agents appear in the same sidebar as your main session, with combined status summaries (blocked / working / done across hosts at a glance).
- **add-remote that provisions itself** — point it at any ssh host; herdr-mx installs the matching server binary non-interactively, with animated progress and truthful failure diagnosis. parsed ssh options (ports, identities, jump hosts) are honored.
- **offline cross-OS seeding** — the opt-in "fat" build (`just bundle`, `herdr bundle list|pack`) embeds macOS + Linux (x86_64/aarch64, static musl) binaries in one file, so a fresh host of a *different* OS is seeded at exact version parity with no release download on the remote.
- **per-remote lifecycle** — host context menu (add space / disable / disconnect / rename), per-remote auto-update-to-this-client toggle, live host version+protocol readout, one-click force-reinstall update.
- **fast over distance** — semantic-frame delta streaming with compression for ~30fps remote panes, last-frame-first switching, accurate per-host ping/throughput in the banner.
- **isolation** — a secondary host going down never touches your main session.

## quality-of-life on top

- **sidebar settings TUI** — configurable agent segments and sidebar lines, applied instantly, with tab names shown in the multi-remote sidebar.
- **fleet-scale rendering** — model updates coalesced to one render per frame, cached sidebar shell, hover painted as a per-frame overlay; the client stays smooth with many remotes attached (upstream's single-remote client never hits these paths).
- **client overlay framework** — unified drag-to-move client menus, keyboard navigation, drag-reorder with preview, collapsed status-only sidebar mode.
- **clipboard image hardening (macOS)** — sandboxed/unreadable screenshot locations warn instead of silently failing; staged image paths paste un-bracketed so they attach; interactive panes spawn as login shells so `~/.zprofile` applies.

## intentionally changed from upstream

| area | upstream | herdr-mx | why |
|---|---|---|---|
| `herdr update` / update channels | herdr.dev manifests | disabled; brew/mise/releases | a stock-herdr download would silently remove multi-remote |
| version string | `0.6.10` | `0.6.10-mx.1` | so bug reports route to the right tracker |
| settings popup | 76×22 base | 96×32 base | room for the sidebar settings TUI |
| windows build | preview beta | unavailable | the multi-remote client doesn't compile on windows yet ([#63](https://github.com/2lab-ai/herdr-mx/issues/63)) |

## not yet re-applied after the v0.6.10 merge

- cline phantom-working guard (#37): upstream's new manifest-based detection defaults cline to "working" again; the mx fix needs a `manifests/cline.toml` override.
- deferred remote refinements from the #60→#62 merge: upstream keepalive (#355), fish-shell remote bootstrap (#396), mise+preview remote seeding.
