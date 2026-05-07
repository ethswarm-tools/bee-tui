# TUI Design Research for Bee Node Operator Cockpit

## 1. The Canon — takeaways

- **k9s**: The gold standard. Distinctive: `:resource` command bar, vim-style navigation, contextual keymap footer per resource view, breadcrumb trail. **Steal**: command-bar resource switching and the always-visible keymap legend.
- **lazygit / lazydocker**: Numbered panels (1-5) with focus-driven contextual keys; the *keys change based on focused panel*. **Steal**: numbered panel focus and dynamic keymap footer — perfect for peers vs stamps vs chunks panels.
- **btop / htop**: Truecolor gradients, braille-character sparklines, per-core meters, mouse-aware but keyboard-first. **Steal**: braille sparklines for chunk-rate and the meter+history pairing.
- **gh dash**: Tab strip across the top, card-based row layout, async data refresh per tab, configurable via YAML. **Steal**: YAML-configurable views so operators can define their own dashboards.
- **bottom (btm)**: Multi-pane resizable grid, time-windowed graphs with zoom, expand-pane (`e`) to fullscreen. **Steal**: the expand-to-fullscreen-then-restore pattern for a peer list or sync queue.
- **dive**: Split-pane tree on left, detail on right, filter-as-you-type. **Steal**: directly for Mantaray collection drill-down — same shape (hash → manifest → entries → chunks).
- **gping / oha**: Live rolling line charts, percentile overlays, terminating summary. **Steal**: oha's percentile breakdown for upload/download latency histograms.
- **slumber / posting**: Posting (Textual-based) is the modern leader — saved request collections, env vars, response inspector. **Steal**: the "request builder" pattern for ad-hoc Bee API calls inside the TUI.
- **harlequin**: Schema tree + query editor + result grid. **Steal**: result-grid pattern for chunk inspection (address, type, size, stamp).
- **yazi**: Async I/O, three-pane miller-columns, image previews. **Steal**: miller columns for Mantaray (manifest → forks → leaves), and async-everything as a hard rule.

## 2. Framework recommendation: **Bubble Tea (Go)** vs **Ratatui (Rust)**

Honest comparison:

- **Bubble Tea**: Best ecosystem in 2026 — Bubbles (tables, viewport, textinput), Lipgloss (styling), Harmonica (animation), Wish (SSH serving). Mature, widely adopted (gh dash, soft-serve, glow). **Pick this** because bee-go is your reference client and the rendering primitives are excellent.
- **Ratatui (Rust)**: Technically the most performant; widget set is rich (Sparkline, Chart, Gauge built-in). Best fit *if* you're writing the canonical client in Rust. bee-rs makes this viable. Tradeoff: smaller "dashboard-shaped" prior art than Bubble Tea.
- **Ink (Node)**: Comfortable for JS devs; React mental model. Weak for high-frequency redraws and multi-pane dashboards — better for installers/wizards. Skip for a node cockpit.
- **Textual (Python)**: Beautiful, CSS-styled, async-native, but the runtime weight and packaging story are heavier than Go/Rust binaries; node operators want a single static binary.

**Recommendation**: Bubble Tea if shipping one TUI; Ratatui if you want it tied to bee-rs and a static cargo-installable binary. Both will look great. Don't pick Ink or Textual.

## 3. Patterns to steal for Bee specifically

1. **k9s `:resource` command bar** → `:peers`, `:stamps`, `:chunks`, `:topology`, `:cheques`, `:queues`.
2. **lazygit numbered panels + dynamic keymap** → 1=Peers, 2=Stamps, 3=Sync, 4=Accounting; keymap updates per focus.
3. **btop braille sparklines** → live chunk-rate, sync-queue depth, peer count, per-bin storage.
4. **gh dash tabs** → "Operator" (health/stamps/cheques) vs "Developer" (uploads/feeds/Mantaray).
5. **dive tree drill-down** → Mantaray manifest browser: ref → fork bytes → entries → chunk addresses.
6. **harlequin result grid** → `:chunks` table with address, type, stamp, bin, sortable columns.
7. **bottom expand-pane** → `e` to fullscreen the topology view (Kademlia bins) without losing context.
8. **oha percentile chart** → upload/download p50/p95/p99 over the session, overlaid on live chart.

## 4. Anti-patterns to avoid

- **Mouse-required navigation** — operators are over SSH; keyboard-first or it dies.
- **Modal dialog soup** (looking at you, mc) — prefer inline command bar + status line over stacked popups.
- **Unicode box-drawing without ASCII fallback** — Windows Terminal, tmux on old hosts, and dumb pipes choke; provide `--ascii`.
- **Color-only signaling** — pair every red/green with a glyph (`!`, `OK`, `↑↓`) for color-blind operators.

## 5. Color & rendering

- **Default to 256-color**, opt into truecolor via `COLORTERM=truecolor` detection. Most server terminals still report 256.
- **No Nerd Fonts dependency.** Optional yes, required no — node operators SSH from random boxes. Use Unicode 9.0 only (braille `⠀-⣿` for sparklines, block elements `▁▂▃▄▅▆▇█` for bars — both are universally supported).
- **Truecolor gradients are tempting; don't.** A 4-color severity palette (ok/warn/err/info) plus dim/bold reads better and survives palette swaps.
- **Respect `NO_COLOR`** env var. Operators set this on purpose.

**Bottom line**: Build it like k9s drank lazygit and looked at btop's graphs. Bubble Tea, keyboard-first, ASCII-fallback, 256-color, no Nerd Fonts.

> **Plan v2 update:** Subsequent 2026 Ratatui SOTA research swung the recommendation to **Ratatui + bee-rs** because the official `ratatui/templates` `component` template has become the de-facto starting point and the built-in widget set (Sparkline, Chart, Gauge, Table, Tabs) covers the cockpit natively. See [`08-ratatui-2026-sota.md`](08-ratatui-2026-sota.md).
