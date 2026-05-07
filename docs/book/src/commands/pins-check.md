# `:pins-check`

Run a full integrity check on every locally pinned reference.
Bee streams one NDJSON record per pin; the cockpit dumps
each one to a file as it arrives so you can `tail -f` it.

```
:pins-check
:pins         (alias)
```

## What gets checked

Bee's `GET /pins/check` walks every locally pinned root
reference and verifies, per pin:

- **total** chunks in the manifest
- **missing** — chunks the manifest references but local
  storage doesn't have
- **invalid** — chunks present but failing hash verification

A pin is **healthy** when `missing == 0 && invalid == 0`.

## Where the file goes

`$TMPDIR/bee-tui-pins-check-<profile>-<unix-timestamp>.txt`.

Per-profile filename: switching to a different `:context`
mid-check won't conflict with another profile's parallel
run. The original check runs to completion against the
profile that started it.

The cockpit prints the path in the status line:

```
pins integrity check running → /tmp/bee-tui-pins-check-prod-1-1715056472.txt
                                (tail to watch progress)
```

## File format

A header followed by one line per pin, ending with a `# done.`
marker:

```
# bee-tui :pins-check
# profile  prod-1
# endpoint http://10.0.1.5:1633
# started  2026-05-07T08:14:32Z

abcd1234…   total=8192   missing=0     invalid=0    healthy
def56789…   total=1684   missing=12    invalid=0    UNHEALTHY
9876fedc…   total=4096   missing=0     invalid=2    UNHEALTHY
ba98cdef…   total=64     missing=0     invalid=0    healthy
# done. 4 pins checked.
```

If the check itself errors out (server 500, connection lost),
the last line is `# error: <message>` instead of `# done.`.

The `healthy` / `UNHEALTHY` literal at the end of each line
is for `grep`-ability:

```sh
grep UNHEALTHY ~/path/to/bundle.txt
```

…lists every reference that needs attention.

## Why this command exists

Locally pinned content is the only reason Bee guarantees
chunks remain accessible. If a chunk is *missing* from a
pinned manifest, your data is gone and the network may not
have it either (depending on network density). If chunks
are *invalid*, your local storage has been corrupted —
disk failure, partial write, etc.

Either case is silent until you check. `:pins-check` is the
audit trail: run it, save the file, and you have a
point-in-time integrity snapshot per pinned reference.

## How long it takes

`/pins/check` walks every chunk on disk. For a node with:

- A handful of small pins (< 10 GB each): seconds.
- Hundreds of pins or large multi-GB pins: minutes.

The cockpit doesn't block — the check runs in the
background, the file appends as Bee streams the response,
and you can keep navigating screens. A second `:pins-check`
while one is in flight just kicks off another (Bee does not
serialise; the HTTP server handles them in parallel).

## What to do with UNHEALTHY pins

For **invalid chunks** (corruption): your local storage is
broken. Best move is to re-download the reference (it's
still on the network if other nodes have it) and then re-pin.
Long-term, check disk health (`smartctl`).

For **missing chunks**: similar — re-fetch from the network
or accept the loss. Bee won't auto-heal pins; the operator
has to either re-upload or re-pin from a known good source.

If a pin shows missing > 0 *and* the cockpit's S1 Reserve
gate is also failing, your node is in a bad state — drop
to S6 Peers + S7 Network to confirm connectivity is OK
before re-fetching.

## What this command *doesn't* do

- **Doesn't try to repair anything.** Read-only check.
- **Doesn't unpin orphans.** Local pins that point to
  partially-missing references stay pinned; you decide
  whether to remove them.
- **Doesn't verify network availability.** "missing locally"
  is the only check; if a chunk is missing here but available
  on the network, Bee will lazily re-fetch it on the next
  download. The check just reports current local state.

## See also

- [`:diagnose`](./diagnose.md) — capture the live snapshot;
  `:pins-check` complements it for storage-integrity history.
- [The `:command` bar](./bar.md)
