# The `:command` bar

A vim-style colon prompt for actions that don't fit on the
keymap: jump to a screen by name, fire on-demand checks,
switch profiles, export a diagnostic bundle.

## Opening + closing

| Key | Effect |
|---|---|
| `:` | Open the command bar (focus moves to a one-line prompt at the bottom) |
| `Esc` | Close without running |
| `↵` | Run the command |
| `Backspace` | Delete left |

The screen behind the bar keeps refreshing — gauges don't
freeze while you're typing.

## Status line

After a command runs, the bottom line shows the result for
~3 seconds before fading:

- **Info** (green) — `→ Health`, `diagnostic bundle exported to /tmp/...`
- **Err** (red) — `unknown command: "..."`, `usage: :set-logger <expr> <level> ...`

If you missed the message, just re-run — the status sticks
until the next command or the next 3 s tick.

## Screen jumps

Every screen has a name; `:<name>` jumps there.

| Command | Screen |
|---|---|
| `:health` | S1 — Health gates |
| `:stamps` | S2 — Stamps + bucket drill |
| `:swap` | S3 — SWAP / cheques |
| `:lottery` | S4 — Lottery + rchash |
| `:warmup` | S5 — Warmup checklist |
| `:peers` | S6 — Peers + bin saturation |
| `:network` | S7 — Network / NAT |
| `:api` | S8 — RPC / API health |
| `:tags` | S9 — Tags / uploads |
| `:pins` | S11 — Pins |
| `:manifest <ref>` | S12 — Manifests (preloads root + jumps) |
| `:watchlist` | S13 — Watchlist |
| `:feedtimeline` | S14 — Feed Timeline |
| `:pubsub` | S15 — Pubsub watch |

These are equivalent to pressing `Tab` until you reach the
target screen, but faster on a 14-screen carousel.

## Action commands

| Command | Page | What it does |
|---|---|---|
| `:diagnose` (alias `:diag`) | [diagnose](./diagnose.md) | Dump the full snapshot + recent log buffer to a file |
| `:pins-check` (alias `:pins`) | [pins-check](./pins-check.md) | Run a full integrity check on every locally pinned reference |
| `:loggers` | [loggers](./loggers.md) | Snapshot the live logger registry to a file |
| `:set-logger <expr> <level>` | [loggers](./loggers.md) | Change one logger's verbosity at runtime |
| `:topup-preview <batch> <amount>` | [stamp-previews](./stamp-previews.md) | Predict TTL + cost of topping up an existing batch |
| `:dilute-preview <batch> <new-depth>` | [stamp-previews](./stamp-previews.md) | Predict capacity / TTL change of diluting a batch |
| `:extend-preview <batch> <duration>` | [stamp-previews](./stamp-previews.md) | Predict cost to gain N days/hours of TTL |
| `:buy-preview <depth> <amount>` | [stamp-previews](./stamp-previews.md) | Predict TTL / capacity / cost of a hypothetical fresh buy |
| `:buy-suggest <size> <duration>` | [stamp-previews](./stamp-previews.md) | Suggest the minimum (depth, amount) to cover a target |
| `:probe-upload <batch>` | [probe-upload](./probe-upload.md) | Upload one synthetic 4 KiB chunk; report end-to-end latency |
| `:upload-file <path> <batch>` | [upload-file](./upload-file.md) | Upload a single local file via `POST /bzz`, return Swarm reference |
| `:upload-collection <dir> <batch>` | [upload-collection](./upload-collection.md) | Recursive directory upload as a Swarm collection (tar `POST /bzz`); auto-detects `index.html` |
| `:feed-probe <owner> <topic>` | [feed-probe](./feed-probe.md) | Latest update for a feed (read-only lookup) |
| `:feed-timeline <owner> <topic> [N]` | [S14 — Feed Timeline](../screens/s14-feed-timeline.md) | Walk a feed's history (newest first), open S14 |
| `:watch-ref <ref> [interval]` | [watch-ref](./watch-ref.md) | Re-run `:durability-check` on `<ref>` periodically (default 60 s) |
| `:watch-ref-stop [ref]` | [watch-ref](./watch-ref.md) | Cancel one (or all) active `:watch-ref` daemons |
| `:pubsub-pss <topic>` | [S15 — Pubsub](../screens/s15-pubsub.md) | Subscribe to a PSS topic, surface frames in S15 |
| `:pubsub-gsoc <owner> <id>` | [S15 — Pubsub](../screens/s15-pubsub.md) | Subscribe to a GSOC SOC, surface frames in S15 |
| `:pubsub-stop [sub-id]` | [S15 — Pubsub](../screens/s15-pubsub.md) | Cancel one (or all) active pubsub subscriptions |
| `:pubsub-filter <substring>` | [S15 — Pubsub](../screens/s15-pubsub.md) | Show only S15 rows whose channel/preview contains substring |
| `:pubsub-filter-clear` | [S15 — Pubsub](../screens/s15-pubsub.md) | Remove the active S15 filter |
| `:pubsub-replay <path>` | [S15 — Pubsub](../screens/s15-pubsub.md) | Load a prior session's pubsub-history JSONL into S15 |
| `:manifest <ref>` | — | Open a Mantaray manifest for browsing (preloads root + jumps to S11 Manifests) |
| `:inspect <ref>` | — | Universal "what is this thing?" — auto-detects manifest / raw chunk / feed manifest |
| `:durability-check <ref>` | — | Walk every chunk of `<ref>` and report retrieved / lost / corrupt / network-seen counts |
| `:plan-batch <prefix> [usage] [ttl] [extra-depth]` | [stamp-previews](./stamp-previews.md) | Run beekeeper-stamper's `Set` algorithm read-only — outputs PlanAction (None/Topup/Dilute/Both) |
| `:check-version` | — | GitHub Releases API check; reports if a newer bee-tui is published |
| `:config-doctor` | — | Read-only audit of `bee.yaml` against swarm-desktop's migration rules |
| `:price` | [S3 — SWAP](../screens/s3-swap.md) | xBZZ → USD lookup via public token service |
| `:basefee` | [S3 — SWAP](../screens/s3-swap.md) | Gnosis Chain JSON-RPC basefee + tip lookup |
| `:grantees-list <ref>` | — | Read-only `GET /grantee/{ref}` for ACT grantee inspection |
| `:hash <path>` | — | Local Swarm-hash of a file via Mantaray (no Bee call) |
| `:cid <ref> [--type=manifest\|feed]` | — | Local Reference → CID conversion (no Bee call) |
| `:depth-table` | — | Print canonical depth → capacity table (no Bee call) |
| `:gsoc-mine <overlay> <identifier>` | — | Local CPU work — find a PrivateKey whose SOC address matches `<overlay>` |
| `:pss-target <overlay>` | — | Extract the 4-hex-char `target` prefix Bee accepts on `/pss/send` |
| `:watchlist` (jump) | — | Jump to S12 Watchlist (history of `:durability-check` results) |
| `:context <name>` (alias `:ctx`) | [context](./context.md) | Switch to a different node profile from your config |
| `:context` | [context](./context.md) | List configured profiles (no switch) |
| `:nodes` | [context](./context.md) | Open the node-picker overlay (also `Ctrl+N`) |
| `:quit` (alias `:q`) | — | Exit the cockpit |

## Why a colon prompt?

Two reasons:

1. **Discoverability without clutter.** The cockpit can have
   ten screen-jumps + half a dozen action commands without
   each one needing its own keybinding. The keymap stays
   minimal (`Tab`, `↵`, `Esc`, `?`, `:`, `q`); rare commands
   live behind the colon.
2. **Familiarity.** Anyone who's used vim, k9s, or lazygit
   has the muscle memory. The cockpit's job is to *not*
   require new muscle memory.

## What's not on the bar

These actions deliberately don't have a `:command` form:

- **Cashing out cheques.** Cashout is on-chain; it costs gas;
  you should think about whether to do it. The cockpit
  surfaces the data (S3 Pane 2) but won't trigger the
  on-chain transaction. Use `curl POST /chequebook/cashout/<peer>`
  if you really mean it.
- **Buying / topping up postage.** Same reasoning. S2 shows
  TTL and worst-bucket; the `:*-preview` verbs (see
  [stamp-previews](./stamp-previews.md)) compute predicted
  TTL/cost without writing — but `bee postage buy` and `bee
  postage topup` themselves are operator decisions with
  funding consequences and stay outside the cockpit.
- **Stake deposit / withdraw.** Same.
- **Connect / disconnect peers.** Bee's kademlia handles
  this without operator help; manual `connect` is a
  debugging escape hatch.

The cockpit is a read-mostly observer. The mutating commands it
*does* have are scoped to upload + diagnostic state, never
chain-mutation:

- `:set-logger` — changes a Bee logger level (no funds, no chain)
- `:probe-upload` — uploads one synthetic 4 KiB chunk against a
  caller-supplied stamp to verify the upload path end-to-end
- `:upload-file` / `:upload-collection` — real content uploads
  via `POST /bzz`; capped at 256 MiB (collections also at 10k
  entries). Stamp consumption is the operator's responsibility
  via the explicit `<batch>` argument.

There is intentionally **no** `:reupload`, `:tx-bump`, or
`:grantees-create` verb yet — write tier verbs that
consume stamps or mutate chain state warrant their own UX +
confirmation pass. The current write surface stops at uploads.

## See also

- [`:diagnose`](./diagnose.md)
- [`:pins-check`](./pins-check.md)
- [`:loggers` / `:set-logger`](./loggers.md)
- [Stamp dry-run previews](./stamp-previews.md)
- [`:probe-upload`](./probe-upload.md)
- [`:upload-file`](./upload-file.md)
- [`:upload-collection`](./upload-collection.md)
- [`:feed-probe`](./feed-probe.md)
- [S14 — Feed Timeline](../screens/s14-feed-timeline.md)
- [S15 — Pubsub watch](../screens/s15-pubsub.md)
- [`:context`](./context.md)
