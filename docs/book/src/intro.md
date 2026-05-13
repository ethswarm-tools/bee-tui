# Introduction

`bee-tui` is a terminal cockpit for [Ethereum Swarm](https://www.ethswarm.org/)
Bee node operators. It surfaces the state Bee's API hides — bucket
collisions, redistribution skip reasons, bin starvation, NAT reality — in
fourteen live screens, with an always-on HTTP request tail so operators
trust what they see.

![bee-tui cold-start tour](https://raw.githubusercontent.com/ethswarm-tools/bee-tui/main/docs/tapes/cold-start.gif)

This handbook is the **per-screen reference**. It explains what each
screen shows, why it matters, and how to use the keymap. The
high-level project plan lives in
[`docs/PLAN.md`](https://github.com/ethswarm-tools/bee-tui/blob/main/docs/PLAN.md);
the README is the install + quickstart entry point.

## Who this is for

You run a Bee node (mainnet or testnet) and want to know what's wrong
without reading 50 endpoints worth of JSON. The pain points this tool
exists to address:

- **"Why is my node unhealthy?"** — answered by S1 with WHY tooltips
  encoding tribal knowledge from the bee-go source.
- **"Which batch is about to fail uploads?"** — S2's worst-bucket fill
  bar + Enter-to-drill bucket histogram.
- **"Why am I unreachable?"** — S7 distinguishes public-vs-private
  underlay and tracks AutoNAT reachability stability over a window.
- **"Why am I not earning rewards?"** — S4's redistribution skip
  reasons reconstruct the truth `LastPlayedRound` doesn't tell you.
- **"Where is my upload stuck?"** — S9's TagStatus ladder lights up
  the exact phase a stuck upload is in.
- **"Which of my nodes am I driving?"** — `Ctrl+N` opens the v1.10
  node picker over a list of every `[[nodes]]` entry; the top-bar
  metadata line always names the active profile + endpoint.
- **"Is anything red across my whole fleet?"** — S15 Fleet view
  (v1.11+) polls every configured node every 10 s and surfaces the
  aggregate status / peers / worst stamp TTL / ping per row. Enter
  on a row switches context to that node's per-screen detail.
- **"5 nodes flap and I get 5 Slack pings."** — v1.12's
  `[fleet].aggregate_webhook_url` consolidates per-node status
  transitions into a single rolled-up POST per coalesce window.
- **"Bee keeps crashing; can the cockpit restart it?"** — v1.12's
  `[bee.supervisor].auto_restart` adds an exponential-backoff
  watchdog with a per-hour budget; the top-bar chip shows uptime
  and historical restart count.
- **"Run a topup-preview without remembering the arg order."** —
  v1.12's `Shift+E` opens a guided modal that builds the verb line
  field-by-field and shows the result inline.
- **"Is anything running in the background?"** — top-bar awareness
  chips (`subs N`, `watch N`, `alerts ●`, v1.10+) appear whenever
  a pubsub subscription, a `:watch-ref` daemon, or webhook alerting
  is active, and disappear when nothing is.

## What this handbook is *not*

It's not a Bee operations manual. The deep model of how Swarm works —
postage, neighborhoods, kademlia, redistribution — is best absorbed
from [the Bee book](https://docs.ethswarm.org/) and the bee-go source.
This handbook assumes you know that domain and just need to know how
the cockpit surfaces it.

## Versioning

bee-tui follows [Semantic Versioning](https://semver.org/). The handbook
on this site reflects whatever version is on `main`; the README's
**Status** table tells you what's shipped and what's coming.
