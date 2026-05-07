# `:loggers` and `:set-logger`

Inspect and mutate Bee's runtime logger registry. Useful when
debugging a specific subsystem (push-sync, pricer, swap)
without restarting the node.

## `:loggers`

Snapshot the current logger registry to a file.

```
:loggers
```

Bee maintains a global registry of named loggers, each with
its own verbosity. The list grows as Bee initialises modules
— at steady state on a healthy node you'll see ~80–120
loggers covering pushsync, pullsync, swap, postage,
storageincentives, etc.

### Where the file goes

`$TMPDIR/bee-tui-loggers-<profile>-<unix-timestamp>.txt`.
Like `:pins-check`, the filename is per-profile so parallel
invocations across `:context` switches don't collide.

### Sort order

Output is sorted **by verbosity** descending, then by logger
name. Loud loggers float to the top so the operator
immediately sees what's currently chatty:

```
# bee-tui :loggers
# profile  prod-1
# endpoint http://10.0.1.5:1633
# started  2026-05-07T08:14:32Z
# 96 loggers registered
# VERBOSITY  LOGGER
  all        node/pushsync
  debug      node/pricer
  info       node/api
  info       node/postage
  warning    node/swap
  warning    node/topology
  error      node/p2p
  none       node/pullsync
  ...
# done.
```

If `:set-logger` set push-sync to `all` an hour ago, you can
run `:loggers` to confirm it's still there (and at what
level).

## `:set-logger <expr> <level>`

Change one logger's verbosity at runtime.

```
:set-logger node/pushsync debug
:set-logger node/swap     warning
:set-logger .             info        # all loggers
```

### Arguments

| Arg | Allowed values | Description |
|---|---|---|
| `<expr>` | logger name or `.` for all | Path-style logger identifier as Bee emits them (`node/pushsync`, `node/postage/listener`, etc.). The literal `.` matches every registered logger — Bee broadcasts the level to all of them. |
| `<level>` | `none`, `error`, `warning`, `info`, `debug`, `all` | The verbosity. `none` silences entirely; `all` is the loudest. |

bee-rs validates the level client-side before any HTTP
request goes out, so a typo errors immediately:

```
:set-logger node/swap warn
→ usage: :set-logger <expr> <level>  (level: none|error|warning|info|debug|all; expr: e.g. node/pushsync or '.' for all)
```

### What happens under the hood

The cockpit fires:

```
PUT /loggers/<base64url(expr)>/<level>
```

bee-rs URL-safe-encodes `<expr>` and constructs the path.
The result (success or error) is appended to a per-call log
file:

```
$TMPDIR/bee-tui-set-logger-<profile>-<unix-timestamp>.txt
```

Containing:

```
# bee-tui :set-logger
# profile  prod-1
# endpoint http://10.0.1.5:1633
# expr     node/pushsync
# level    debug
# started  2026-05-07T08:14:32Z
# done. node/pushsync → debug accepted by Bee.
```

### Verifying the change

After `:set-logger`, the cockpit's status line says:

```
set-logger "node/pushsync" → "debug" (PUT in-flight; check :loggers to verify)
```

Run `:loggers` to confirm the new level took effect. The PUT
is fire-and-forget; the verification is a separate GET.

## Why these commands exist

Bee's runtime logging is the way to debug specific
subsystems. Without these commands, the operator would
have to:

1. Find the bee-rs (or curl) command for `PUT /loggers/...`
2. Base64url-encode the logger expression
3. Run the curl in a separate shell
4. Run a second curl to verify

`:loggers` + `:set-logger` collapse this to one keystroke
each, with the verification dump landing in a tail-able
file.

The set-logger fix story: bee-rs `set_logger_verbosity` was
silently broken in versions before 1.6 — it emitted
`PUT /loggers/{expr}` (no verbosity in the path), which
Bee accepted with a 200 but applied nothing. bee-rs 1.6
added the correct `set_logger(expr, verbosity)` and the
cockpit uses that exclusively.

## Common scenarios

### "I want push-sync logs at debug level for 30 minutes"

```
:set-logger node/pushsync debug
```

Watch Bee's logs (journalctl / docker logs / stdout). When
done:

```
:set-logger node/pushsync info
```

There's no auto-revert.

### "What's currently at debug or louder?"

```
:loggers
```

…then `grep -E 'all|debug' /tmp/bee-tui-loggers-...`.

### "Quiet everything except errors"

```
:set-logger . error
```

The `.` expression hits every logger.

## See also

- [`:diagnose`](./diagnose.md) — pairs well with `:loggers`
  for "what was Bee logging at the time of this incident?"
- [The `:command` bar](./bar.md)
