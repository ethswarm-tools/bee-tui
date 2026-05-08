# S12 — Manifests

A Mantaray-tree browser. The first screen in bee-tui that gives
operators X-ray vision into their *data* — not just their node.
Type a Swarm reference into `:manifest <ref>` (or `:inspect <ref>`)
and the cockpit fetches the chunk, parses it as a Mantaray
manifest, and renders the tree here as a flat indented list.

## How to load a manifest

```text
:manifest <ref>      # always tries to render as a manifest
:inspect  <ref>      # auto-detects: manifest, raw chunk, or feed manifest
```

`<ref>` is a 64-hex-char Swarm reference (with or without `0x`).

- `:manifest` jumps to S12 immediately and starts an async
  `GET /chunks/{ref}` against the active node. If the chunk
  parses as a Mantaray manifest, the tree renders. If it doesn't,
  the screen shows `error: <reason>` so the operator can re-try
  with `:inspect` to learn what it is.
- `:inspect` is the universal "what is this thing?" verb. It
  fetches the same chunk, then auto-detects:
  - Mantaray manifest → routes to S12 (same as `:manifest`)
  - Raw chunk → prints `raw chunk, N bytes` on the command-status
    line; doesn't switch screens.
  - Feed manifest → prints the feed-manifest fingerprint hint.

`:inspect` is non-destructive — at most one chunk fetch per
invocation.

## Layout

```
┌ MANIFEST  · 32-byte chunk · 12 forks · 4 leaves ─────────────────────────────┐
│   f8aa0f76…3e4d1abf                                                          │
│ ▼ (root)                                                                     │
│   ▶ images/                                                                  │
│   ▼ articles/                                                                │
│       · post-1.html         text/html        ee7f3a20…                       │
│       · post-2.html         text/html        9c4d9a80…                       │
│       ⌛ assets/                              loading…                        │
│   · index.html              text/html        a02ee188…                       │
│                                                                              │
│   selected: target ee7f3a201810c5e9…                                         │
│  Tab switch screen   ↑↓/jk select   ↵ expand/collapse   ? help   q quit      │
└──────────────────────────────────────────────────────────────────────────────┘
```

The header line shows the chunk size + fork/leaf summary. The
second header line shows the full root reference for click-drag
copy.

Tree glyphs:

| Glyph | Meaning |
|---|---|
| `▼` | Expanded fork (children visible) |
| `▶` | Collapsed fork with children |
| `·` | Leaf (TYPE_VALUE — points at a file target, no further forks) |
| `⌛` | Fork is loading (async fetch in flight) |
| `✗` | Fetch / parse failed |

Each row carries: indent depth, glyph, path-segment label, content-type
(when present in metadata), and the truncated target reference (for
leaves) or fork self-address.

## Lazy-load semantics

Pressing `↵` on a collapsed fork that has children either:

- **Toggles expansion** (cheap) when the child node is already
  loaded.
- **Starts an async `GET /chunks/{self_address}`** when it isn't.
  The row glyph flips to `⌛` until the response arrives; on
  failure it becomes `✗ error: <reason>` (and you can retry with
  another `↵`).

The walker only fetches forks the operator opens — large manifests
(e.g. a 10k-page wiki) don't pre-load every chunk. The cost of
exploring the tree scales with how much you actually look at.

## The `selected:` line

The detail row above the footer renders the cursored row's
identifier in plain text, so you can drag-select it in your
terminal and copy without bee-tui needing a copy key:

- For leaf rows: `selected: target <target-ref-hex>`
- For fork rows: `selected: chunk <self-address-hex>`
- For the root summary: `(no copyable id on this row)`

Copy that hex into a `bee` CLI invocation, a browser URL
(`http://<gateway>/bzz/<ref>/<path>`), or another bee-tui verb.

## Keymap

| Key | Action |
|---|---|
| `↑` / `k` | Move cursor up |
| `↓` / `j` | Move cursor down |
| `↵` | Toggle expand / load the cursored fork |
| `Tab` | Cycle to the next screen |
| `:` | Open the command bar |

The `?` overlay shows these alongside the global keys.

## What it doesn't do

- **No encrypted-ref support** (yet). 64-byte references with an
  obfuscation key suffix render as `error: not a manifest` —
  bee-rs's recursive walker would need to thread the key through
  the chunk decoder. Tracked for a v2.x follow-up.
- **No path-based addressing.** Operators type a chunk reference,
  not `<ref>/path/in/manifest`. bee-rs's `resolve_path` lives in
  the runtime; surfacing it as `:manifest <ref> <path>` is a
  candidate enhancement.
- **No write side.** S12 is strictly a read-only browser. Editing
  a manifest, re-uploading after a fix, or rewiring a fork lives
  in the deferred [write tier](./../commands/bar.md#whats-not-on-the-bar).
- **No file-content preview.** Leaf rows show the target reference
  but not the bytes the leaf points at. Use a separate
  `bee` / `swarm-cli` invocation, or the Bee gateway URL above,
  to fetch the file itself.

## Trust anchor: where do these counts come from?

The fork count and leaf count in the header are derived purely
from the loaded `MantarayNode`s — no Bee API call is needed once
the root + currently-expanded children are in memory. If a
`▶` fork has never been opened, its sub-tree is *not* counted.
This is intentional: the walker only commits to fetching forks
the operator actually navigates into, so the cost of opening S12
on a 10⁵-chunk manifest is one chunk fetch, not 10⁵.

When all forks in a sub-tree are expanded, the leaf count for
that branch reflects every child fork's loaded `MantarayNode`.
