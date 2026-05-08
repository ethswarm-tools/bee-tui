# VHS demo tapes

Each `.tape` file in this directory is a [VHS](https://github.com/charmbracelet/vhs)
script that renders an animated `.gif` (or `.svg` / `.webm`) of bee-tui in
operation. The output goes into the README + the v1.0 blog post.

## Render

Prerequisites:

```sh
# install vhs once
brew install vhs                                          # macOS
go install github.com/charmbracelet/vhs@latest            # cross-platform

# vhs also needs ttyd + ffmpeg on PATH:
#   apt-get install ttyd ffmpeg                           # Debian/Ubuntu
#   brew install ttyd ffmpeg                              # macOS

# install the bee-tui binary you want to record
cargo install --path . --locked

# verify the Bee binary + testnet config are at the relative paths
# the tapes assume: ../bee/dist/bee and ../testnet.yaml from the
# bee-tui repo root.
```

Each tape invokes `bee-tui --bee-bin ../bee/dist/bee --bee-config
../testnet.yaml`, so bee-tui supervises a fresh Bee for the duration
of the recording. **Render from the bee-tui repo root**, not from
inside `docs/tapes/` — the `..`-relative paths resolve against the
cwd vhs was invoked from:

```sh
# from bee-tui repo root
vhs docs/tapes/cold-start.tape

# render every tape (sequential — supervisor mode means each tape
# spins up a fresh Bee, so two renders cannot overlap)
for t in docs/tapes/*.tape; do vhs "$t"; done

# (optional) shrink the GIFs without visible quality loss — palette
# extraction at 10 fps + lanczos downscale to 900 wide gets ~40% off
for g in docs/tapes/*.gif; do
  ffmpeg -y -i "$g" -vf "fps=10,scale=900:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=128[p];[s1][p]paletteuse" "${g%.gif}-opt.gif"
  mv "${g%.gif}-opt.gif" "$g"
done
```

The `once-ci.tape` is the only one that doesn't use supervisor mode —
`--once` skips the App entirely, so the `readiness` line records
"connection refused" unless a Bee is already running externally on
`localhost:1633`. Pure-local verbs (`depth-table`, `pss-target`)
record cleanly regardless.

## Tapes

| Tape | Output GIF | What it shows |
|---|---|---|
| `cold-start.tape` | `cold-start.gif` | Launch + Tab through all 14 screens + `?` overlay. Headline image referenced in README + intro. |
| `pins-check.tape` | `pins-check.gif` | `:pins-check` from the command bar; status line shows the destination file path. |
| `s2-stamp-drill.tape` | `s2-stamp-drill.gif` | S2 cursor + `↵` on a batch row → bucket histogram drill. |
| `s6-peer-drill.tape` | `s6-peer-drill.gif` | S6 cursor + `↵` on a peer → 4-endpoint fan-out drill (balance / cheques / settlement / ping). |
| `s12-manifest.tape` | `s12-manifest.gif` | `:manifest <ref>` opens the Mantaray tree; `↵` expands forks lazily. |
| `s13-durability.tape` | `s13-durability.gif` | `:durability-check <ref>` walks the chunk graph; results land as S13 rows. |
| `s14-feed-timeline.tape` | `s14-feed-timeline.gif` | `:feed-timeline <owner> <topic>` walks a feed's history (newest first, bounded-parallel). |
| `s15-pubsub.tape` | `s15-pubsub.gif` | `:pubsub-pss` opens PSS subscriptions; `:pubsub-filter` narrows the timeline; `:pubsub-stop` cleans up. |
| `once-ci.tape` | `once-ci.gif` | `--once <verb>` single-shot CI mode — pure-local verbs, Bee-API verbs, `--json` shape, exit codes. |

## Substituting demo data

Three tapes use placeholder hex values that should be swapped for
real ones when rendering against a specific node:

- `s12-manifest.tape` and `s13-durability.tape` — replace the
  64-hex reference (`f8aa…3e4d`) with one your test node has.
- `s14-feed-timeline.tape` — replace
  `0x1234…5678 my-feed` with a real `(owner, topic)` from your
  test node.

The recordings still produce a valid GIF if you don't substitute
(the cockpit shows clear "not found" / "not a manifest" / "no
updates" errors in those cases), but the visuals are less
illustrative than a populated tree / timeline.

## Conventions

- 80×24 terminal — same constraint as the screens are designed for.
- Tape names are kebab-case + match a feature.
- Output files (`.gif`) go alongside the `.tape` and are committed
  (small, ~200 KiB each); regenerated only on intentional UX changes.
- Don't include real bearer tokens in tape config; use the `local`
  default profile or a dummy fixture.
