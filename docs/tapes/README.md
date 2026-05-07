# VHS demo tapes

Each `.tape` file in this directory is a [VHS](https://github.com/charmbracelet/vhs)
script that renders an animated `.gif` (or `.svg` / `.webm`) of bee-tui in
operation. The output goes into the README + the v1.0 blog post.

## Render

```sh
# install vhs once
brew install vhs           # macOS
go install github.com/charmbracelet/vhs@latest   # cross-platform

# render every tape in this directory
for t in *.tape; do vhs "$t"; done
```

VHS spawns a real terminal under the hood, so you'll need a Bee node
reachable on `localhost:1633` (or whatever the tape's `BEE_TUI_CONFIG`
points at) for the screens to populate. For deterministic recordings,
point at a fixture node with stable peer / batch / tag state.

## Tapes

- `cold-start.tape` — first 30 seconds of a fresh launch: tab cycle,
  syncing-warn header, gates settling.
- `s2-stamp-drill.tape` — open S2, navigate to a batch, `↵` drill,
  read the bucket histogram, `Esc` back.
- `s6-peer-drill.tape` — open S6, drill a peer, watch the four-way
  fan-out land.
- `pins-check.tape` — `:pins-check`, watch the file appear in
  `$TMPDIR`, `tail -f` it in another pane.

## Conventions

- 80×24 terminal — same constraint as the screens are designed for.
- Tape names are kebab-case + match a feature.
- Output files (`.gif`) go alongside the `.tape` and are committed
  (small, ~200 KiB each); regenerated only on intentional UX changes.
- Don't include real bearer tokens in tape config; use the `local`
  default profile or a dummy fixture.
