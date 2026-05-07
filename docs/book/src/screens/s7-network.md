# S7 — Network / NAT

Reachability + advertised addresses, in one screen. Answers
the "I have peers but I'm unreachable" question (bee#4194)
that operators hit when AutoNAT silently flips them to Private
mid-session.

## Why this screen exists

Bee tells you whether a peer is connected. It doesn't tell
you whether *you* are reachable from outside. A node behind
NAT can have 100 connected peers (all outbound) and still be
useless to the network — chunks won't be pushed to it because
no one can dial it.

The data to answer this is in `/addresses` (advertised
multiaddrs) and the AutoNAT `reachability` /
`networkAvailability` fields on `/topology`. S7 surfaces both
with a stability window so transient flap (common on
symmetric NAT) doesn't trigger false alarms.

## Header — overlay + ethereum

```
NETWORK   overlay aaaa…aaaa  ethereum 0xCE3…fd615
```

Just identifiers. The overlay is the kademlia address; the
ethereum address is the operator wallet. Both shortened to
first 4 + last 4 hex.

## Pane 1 — Underlays (advertised addresses)

```
  /ip4/198.51.100.42/tcp/1634/p2p/16Uiu2…       ← Public, IPv4
  /ip4/192.168.1.5/tcp/1634/p2p/16Uiu2…         ← Private (dimmed)
  /ip4/127.0.0.1/tcp/1634/p2p/16Uiu2…           ← Private (loopback, dimmed)
  /ip6/2a01::1/tcp/1634/p2p/16Uiu2…             ← Public, IPv6
```

Every multiaddr Bee returns from `/addresses` is shown.
Classification:

| Kind | Style | Examples |
|---|---|---|
| Public | normal text | Routable IPv4 / IPv6 |
| Private | dimmed | RFC 1918 (`10.*`, `172.16.*`, `192.168.*`), link-local, loopback |
| Unknown | normal | DNS multiaddrs, exotic transports the cockpit doesn't classify |

The dim treatment makes it obvious which underlays are
actually advertised to the network (vs. the laundry list of
LAN addresses every Bee node spits out).

If your underlay list shows *only* private addresses, you're
NAT-trapped — Bee literally has no public address to give to
peers, and they can't dial you back.

## Pane 2 — Inbound vs outbound

```
  Inbound  47        ← peers dialing in to you
  Outbound 95        ← peers you've dialed out to
```

Counted from each peer's `session_connection_direction`
metric. The headline check: *can peers dial me?* If Inbound
is 0 (or near zero) and Outbound is healthy, you're reachable
in name only — chunks pushed *to you* won't arrive.

Inbound 0 on a public node usually means firewall (port
1634/tcp blocked) or the node restarted recently and hasn't
been re-dialed yet. Wait 5–10 min; if Inbound stays at 0,
debug the firewall.

## Pane 3 — Reachability + availability

```
  Reachability         Public         (stable for 9m)
  Network availability Available
```

Two strings from AutoNAT:

| Field | Source | Meaning |
|---|---|---|
| Reachability | `topology.reachability` | `Public` / `Private` / `(unknown)` |
| Network availability | `topology.networkAvailability` | `Available` / `Unavailable` / `(unknown)` |

### The stability window

Reachability *flickers*. Under symmetric NAT (carrier-grade
NAT, double NAT, weird home routers), AutoNAT can flip
Public → Private → Public on a per-minute basis. If the
cockpit just showed the latest value, you'd see it bouncing
and chase a phantom problem.

The fix: track the timestamp of the last *change*. The pane
shows "stable for Xm" — if the value just changed, this is
"a few seconds"; if it's been stable for 9 minutes, that's
the value you should trust.

### Reachability ladder

| Status | Glyph | What it means |
|---|---|---|
| Public ✓ | green | AutoNAT confirmed inbound dials work |
| Private ⚠ | yellow | AutoNAT failed inbound dials. Operator action needed. |
| (unknown) · | dim | AutoNAT hasn't reported yet (cold start or older Bee build) |
| Other (verbatim) | dim | Bee surfaced a string we don't classify; shown raw |

## Common scenarios

### "Reachability says Public but Inbound is 0"

AutoNAT thinks you're reachable but in practice no one is
dialing you. Could be:

- Firewall blocks `1634/tcp` for *external* traffic but not
  for AutoNAT's own dialback flow (rare but seen on
  cloud-VPS security groups).
- Recent restart — wait 5–10 min for the network to re-discover
  you.
- Your Inbound count is just delayed (the metric updates
  per-session); refresh after a minute.

### "Reachability bouncing every minute"

Symmetric NAT. The stability window will show "stable for a
few seconds" repeatedly. Options:

- Set up explicit port forwarding on your router (UPnP +
  manual rule on TCP 1634)
- Run Bee on a public VPS instead
- Accept it — your node will still work, just only as an
  outbound participant; reserve fills will be slower

### "Network availability says Unavailable"

Bee's libp2p layer can't reach the network at all. Check
the underlays — if they're empty or all loopback, the node
isn't binding to anything routable. Restart Bee with the
correct `--api-addr` and `--p2p-addr` flags.

### "Underlay list is empty"

`/addresses` returned nothing. Either the API wasn't ready
yet (cold start, wait 30 s) or Bee's listening sockets failed
to bind. Check `bee` process logs.

### "Public IPv4 underlay shows but a different IPv4 is what people see"

Common on multi-homed hosts. Bee advertises whatever it can
detect locally; if your real public IP comes via a NAT
gateway, AutoNAT will figure it out and add a second
underlay once the dialback succeeds. Until then, ignore the
local-detected one.

## What this screen *doesn't* show

- **External port-check** — the cockpit doesn't dial you
  back from a 3rd-party endpoint. AutoNAT does this for free
  via dialback peers; the result feeds the Reachability
  field. If you want a manual check, use a port-checker
  service against your public IP + 1634.
- **Relay candidates** — Bee doesn't expose its relay-pool
  state via API. There's no way for the cockpit to show
  "5 relay candidates available". Future Bee builds may
  expose this; the pane will grow accordingly.

## Snapshot cadence

S7 piggy-backs on:

- `/topology` (5 s) — reachability strings, peer directions
- `/addresses` (60 s — slow data, only changes on bind change)

The reachability stability tracker is internal; it reads each
topology snapshot and maintains a "last changed" timestamp.

## Keys

S7 has no screen-specific keys. The global keymap (`Tab`,
`?`, `:`, `q`) covers everything.
