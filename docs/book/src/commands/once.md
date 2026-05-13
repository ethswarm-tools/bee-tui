# `--once` CI mode

Run a single verb without launching the TUI. Designed for CI
pipelines, cron jobs, monitoring scripts, and any situation where
you want a one-shot answer with a clean exit code instead of a
full-screen cockpit.

```sh
bee-tui --once <verb> [args…] [--json]
```

The whole TUI runtime — App, screens, ratatui, supervisor, watch
hub — is bypassed. Only what the verb actually needs is built:
pure-local verbs touch nothing; Bee-API verbs build a one-shot
[`ApiClient`](../internals/architecture.md) from your active
node profile and call Bee directly.

## Output

By default, one human-readable line on stdout:

```sh
$ bee-tui --once readiness
readiness OK · status=ok · radius=8 · in [1,30]
```

With `--json`, a single JSON object on stdout:

```sh
$ bee-tui --once readiness --json
{"verb":"readiness","status":"ok","message":"readiness OK · status=ok · radius=8 · in [1,30]","data":{"status":"ok","radius":8}}
```

JSON shape (stable since v1.3 — see "Stability contract" below):

| Field | Meaning |
|---|---|
| `verb` | Echo of the requested verb |
| `status` | One of `"ok"`, `"unhealthy"`, `"error"`, `"usage_error"` |
| `message` | Same one-liner the non-JSON form would have printed |
| `data` | Verb-specific structured fields (object). Omitted for verbs that have nothing structured to add. |

## Exit codes

| Code | When |
|---|---|
| `0` | Verb succeeded and the answer was healthy / OK |
| `1` | Verb completed but the answer is unhealthy, the gate failed, or the network said no |
| `2` | Usage error — unknown verb, bad args, missing config |

The split between `1` and `2` matters in CI: code `1` is "the
node says no" (alert your on-call); code `2` is "the script is
wrong" (fix your YAML).

## Tracing

Tracing / logging is **not** initialised in `--once` mode. Stdout
is reserved for the human line or JSON object — nothing else is
written there. Stderr stays clean unless the verb itself prints
to it. This keeps `bee-tui --once` safe to use inside `$()`,
pipes, and structured-output parsers.

## The 24 verbs

### Pure-local (5 — no Bee call required)

| Verb | What it does |
|---|---|
| `hash <path>` | Local Swarm-hash of a file via Mantaray (`bee::manifest::MerkleTree::root`). Same answer as `swarm-cli hash`. |
| `cid <ref> [manifest\|feed]` | Local Reference → CID conversion. Type defaults to manifest when omitted. |
| `depth-table` | Print the canonical depth → capacity table. Reference data, no inputs. |
| `pss-target <overlay>` | Extract the 4-hex-char `target` prefix Bee accepts on `/pss/send` from a full overlay address. |
| `gsoc-mine <overlay> <identifier>` | Local CPU work — find a `PrivateKey` whose SOC address has the target prefix. |

These do not touch the Bee API; you can run them on a build
agent that has no Bee node.

### Bee API (9 — connects to your active node)

| Verb | What it does | Failure code |
|---|---|---|
| `readiness` | Gateway-proxy-style smoke test: `status == ok && radius in [1,30]`. The canonical "ready for traffic?" check. | `1` if unhealthy |
| `version-check` | Reports Bee's `/health` API version vs. the bee-rs client's compiled-against version. | `1` on mismatch |
| `inspect <ref>` | Universal "what is this thing?" — fetches one chunk and detects manifest / raw / feed. | `1` if not retrievable |
| `durability-check <ref>` | Walks the chunk graph, reports `total/lost/errors/corrupt` with optional BMT verify + swarmscan cross-check. See [S12 Watchlist](../screens/s13-watchlist.md) for the full model. | `1` if any chunk is lost / errored / corrupt |
| `upload-file <path> <batch>` | Single-file `POST /bzz`; 256 MiB cap; ext-based content-type guess. Emits `{"reference":"...","tag":N}`. | `1` on upload failure |
| `upload-collection <dir> <batch>` | Recursive directory upload as a Swarm collection (tar `POST /bzz`); auto-detects `index.html`. Caps: 256 MiB / 10 000 entries. | `1` on upload failure |
| `feed-probe <owner> <topic>` | Read-only `/feeds/{owner}/{topic}` lookup of the latest update. | `1` on lookup failure |
| `feed-timeline <owner> <topic> [N]` | Walks a feed's history (newest first). Default 50 entries; hard cap 1000. | `1` on walk failure |
| `grantees-list <ref>` | Read-only `GET /grantee/{ref}` for ACT grantee inspection. Emits `{"count":N,"grantees":[…]}`. | `1` if the grantee list is empty (treat as missing) |

### Stamp economics (10 — fetches chain state + stamps then runs pure math)

| Verb | What it does |
|---|---|
| `buy-preview <depth> <amount>` | Predict TTL / capacity / cost of a hypothetical fresh batch buy. |
| `buy-suggest <size> <duration>` | Suggest the minimum `(depth, amount)` to cover a target size + TTL. |
| `topup-preview <batch> <amount>` | Predict TTL + cost of topping up an existing batch. |
| `dilute-preview <batch> <new-depth>` | Predict the capacity / TTL change of diluting a batch. |
| `extend-preview <batch> <duration>` | Predict the cost to gain N days/hours of TTL on an existing batch. |
| `plan-batch <prefix> [usage] [ttl] [extra-depth]` | Run beekeeper-stamper's `Set` algorithm read-only. Outputs a `PlanAction`: `None`, `Topup`, `Dilute`, or `TopupThenDilute`. Defaults: usage `0.85`, TTL `24h`, extra-depth `+2`. **Exits `1` when an action is recommended** — making this a CI gate signal: "is this batch about to need attention?" |
| `check-version` | GitHub releases API call; reports if a newer bee-tui is published. |
| `config-doctor [path]` | Read-only audit of `bee.yaml` against swarm-desktop's migration rule set. |
| `price` | xBZZ → USD lookup via a public token service. |
| `basefee` | Gnosis Chain JSON-RPC basefee + tip. |

These all hit the Bee API once to fetch the inputs, then do
their work in pure Rust — no chain mutation, no stamp purchase,
no upload.

## Stability contract

The `--once` surface is part of bee-tui's
[semver-stable surface](../intro.md#versioning) since v1.3:

- The four exit codes (`0` ok, `1` unhealthy/failed, `2` usage)
  are pinned. New failure modes get one of the existing codes,
  never a new one.
- The JSON shape `{verb, status, message, data}` is pinned.
  Future minor versions may grow `data` with new keys, but
  existing keys won't be renamed or removed without a v2.0.0
  bump.
- Existing verbs won't be removed. New verbs may appear in
  minor versions.

If you script against `--once` in CI, you can pin a minor
version and trust the surface won't break under you.

## Examples

### Smoke-test a Bee node from CI

```yaml
- name: Bee readiness gate
  run: bee-tui --once readiness
```

Exit `0` → all good. Exit `1` → fail the build.

### Watch a stamp in CI

```yaml
- name: Stamp plan-batch gate
  run: bee-tui --once plan-batch ee7f3a20
```

Exits `1` when the batch needs `Topup`, `Dilute`, or both.
Hook a Slack notification onto the failure to nudge the
operator before the batch hits the cliff.

### Periodic durability check from cron

```sh
*/30 * * * * /usr/local/bin/bee-tui --once durability-check $REF --json \
  | jq -c '. + {timestamp:now}' >> /var/log/bee-durability.jsonl
```

The JSONL is append-only, parseable by anything that speaks
JSON, and re-runnable without parsing TUI output.

### Upload from a build pipeline

```yaml
- name: Publish site to Swarm
  run: |
    REF=$(bee-tui --once upload-collection ./public $BATCH --json | jq -r .data.reference)
    echo "site_ref=$REF" >> $GITHUB_OUTPUT
```

The `data.reference` field is part of the v1.5 stable surface.

## See also

- [The `:command` bar](./bar.md) — the cockpit-mode counterpart to most of these verbs
- [S12 Watchlist](../screens/s13-watchlist.md) — `:durability-check` model in depth
- [Stamp dry-run previews](./stamp-previews.md) — the stamp-economics verbs in cockpit form
