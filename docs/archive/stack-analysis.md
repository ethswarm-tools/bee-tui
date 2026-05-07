# Stack analysis — Rust vs Go vs Zig vs raylib vs others

Decision document for choosing the implementation stack for bee-tui.

## Verdict

**Rust + Ratatui + bee-rs + Tokio + crossterm.** Go + Bubble Tea is a close second; everything else is wrong category.

---

## Why Rust + Ratatui (lean here)

- **Built-in widgets cover the whole cockpit**: `Sparkline`, `Chart`, `Gauge`, `Table`, `Tabs`, `Block`, `Paragraph`, `BarChart` ship in the framework. Everything needed for stamp TTL bars, sync sparklines, peer tables — no third-party shopping.
- **Single static binary**: `cargo install bee-tui` and a node operator on a fresh VPS has it. No Go runtime, no Node, no Python venv.
- **Real-time fits Rust's grain**: live polling + WebSocket streams + non-blocking redraws — Tokio + crossterm + Ratatui is the canonical async TUI stack and it's fast.
- **bee-rs (v1.2.0) already exists**. A flagship TUI doubles as a marquee demo for the crate; the loop tightens — perf wins in bee-rs show up immediately in the TUI.
- **Ecosystem momentum**: in 2024–2026 Ratatui has overtaken tui-rs, picked up async-friendly state management (e.g. `tui-realm`, the official component template), and now has more active TUI prior art than Bubble Tea (gitui, atuin, oha, yazi, television, lazysql, gpg-tui).

## Why Go + Bubble Tea is fine but second

- Dashboard prior art is strongest here (**gh dash**, soft-serve, glow, lazygit, lazydocker). If you want to copy a layout, the closest analog usually exists in Charm-land.
- But you'll pull components from third-party libs (Bubbles, lipgloss, harmonica, glamour) instead of having them built-in.
- bee-go is the older, more mature client — a Bubble Tea TUI ships from the canonical ecosystem.
- Compose with `tea.Cmd` for async; it's clean but more boilerplate than Tokio.

The real tiebreaker between these two: **which client do you want to be the strategic flagship?** A TUI is a forcing function for client polish — whichever it sits on will get more attention. Pick Rust if bee-rs is the one you want to push; Go if bee-go is.

## Why not Zig

Zig has `libvaxis` (Tim Culverhouse) and a couple of real apps (comlink, ghostty's internals). The framework is technically sound. But:

- Tiny TUI ecosystem — almost no widgets, you'd be writing tables/sparklines/charts from scratch.
- No HTTP/WebSocket Bee client exists in Zig — you'd be building bee-zig before you write a single screen.
- Zig 0.14/0.15 still has stdlib churn that breaks downstream code; ratatui/bubble tea churn is feature churn, not language churn.

You'd spend 80% of the project on infrastructure that already exists in Rust/Go. Wrong tradeoff for a productivity tool.

## Why not raylib

Raylib is a **graphics/games library**: it opens an OpenGL window and draws pixels. It doesn't render to a terminal. Even the "terminal-styled" raylib apps are GUI windows that *look* like terminals — they can't run over SSH, can't run inside tmux, can't be piped, and need a display server (X11/Wayland/Cocoa).

That breaks the whole point of a TUI for Bee operators:

- Most Bee nodes run on **headless VPS / cloud boxes**. No display, no GPU. SSH is the only way in. raylib needs a window.
- **tmux/screen multiplexing** is how operators keep dashboards alive. raylib apps don't live there.
- **`scp`/`rsync` distribution** of a single binary that works anywhere — raylib pulls OpenGL/X11/Wayland deps; not a fit for `cargo install` to a server.

If raylib were on the table, so would Tauri, Iced, egui, Slint, Qt, GTK, Flutter Desktop — and at that point you're just building a **better Swarm Desktop**, not a TUI. That's a real product, just a different one.

### When raylib WOULD make sense (different product)

- A **graphical Bee dashboard** with charts and 3D topology visualization (Kademlia bins as a sphere, peer connections as arcs).
- A **kiosk-mode** display for a community node (big TV in a hackerspace showing live swarm stats).
- A **playful retro-terminal aesthetic** but in a window with custom fonts and post-FX shaders.

But that's a different product with a different audience — desktop users, not headless operators.

If you wanted graphical instead of TUI, the 2026-current options would be:
- **Rust + egui** (or `iced`) — single binary, immediate-mode, bee-rs already there. Closest thing to "raylib but the right tool."
- **Rust + Tauri** — webview-based; closer to rewriting Swarm Desktop better.
- **raylib + raygui** — fine, but you'd be writing widgets from scratch and ttf-loading fonts manually. Egui beats it for dashboard shapes.

## Why not the others

- **Python + Textual** — beautiful, async-native, CSS-styled. But operators want one static binary and you'd be shipping a venv or PyOxidizer. Wrong shape for an ops tool.
- **Node + Ink** — React mental model, fine for installers/wizards. Bad at high-frequency redraws and multi-pane dashboards. swarm-cli is already Node — no need to do it twice.
- **C/C++ + notcurses** — Nick Black's notcurses is the most powerful TUI lib that exists, with pixel/sixel graphics. But you don't need pixel graphics for Bee, and you'll fight memory management instead of building features.
- **Elixir/OCaml/Nim** — fun, niche, no Bee client.

## Bottom line

If forced to pick one: **Rust + Ratatui + bee-rs + Tokio + crossterm**. Single binary, built-in widgets, async-first, bee-rs gets a flagship. Total weekend-to-MVP path is shorter than it looks because the Ratatui examples cover ~70% of what a node cockpit needs.
