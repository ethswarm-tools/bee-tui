# Does a TUI exist for Bee? — Verdict

Initial research question: is there an existing terminal user interface (curses / k9s / lazygit-style full-screen interactive terminal app) for Ethereum Swarm Bee?

## Verdict

**No TUI exists for Bee.** The niche is open.

## What was searched

- **awesome-swarm README** — grepped for `tui|terminal|curses|dashboard|monitor|interactive`. Only Bee Dashboard, swarm-cli, and Beest match — none are TUIs.
- **ethersphere org** — enumerated all ~190 repos via `gh repo list ethersphere --limit 200`. No repo is described as a TUI/curses/ncurses app.
- **GitHub search** — `bee swarm tui`, `ethersphere tui`, `bee node terminal` — zero TUI hits.
- **Web search** — `"Swarm Bee" TUI ncurses bubbletea`, `bee node k9s OR lazygit` — zero relevant matches.
- **Local bee-apis/** — `bee, bee-bench, bee-go, bee-js, bee-rs, swarm-cli` + plan files. Confirmed no TUI.

## Closest things (and why they're not TUIs)

| Project | What it is | Why not a TUI |
|---|---|---|
| [bee-dashboard](https://github.com/ethersphere/bee-dashboard) | React/Vite web app on :8080, optionally Electron-wrapped | Browser-based |
| [swarm-desktop](https://github.com/ethersphere/swarm-desktop) | Electron installer bundling Bee Dashboard | Native GUI |
| [swarm-cli](https://github.com/ethersphere/swarm-cli) | One-shot CLI commands | No persistent screen |
| [Beest](https://github.com/w3rkspacelabs/beest) | "Interactive CLI toolkit" — uses `@clack/prompts` + `cli-tableau` | Sequential prompts, not full-screen — no `blessed`/`ink`/`bubbletea` |
| [beepulse](https://github.com/ethersphere/beepulse) | Metrics scraper → Prometheus Pushgateway | Headless |
| [grafana-dashboards](https://github.com/ethersphere/grafana-dashboards) | Prometheus dashboards | Web |

## TUI-adjacent worth knowing

- **Beest** is the closest spiritual precedent (interactive multi-node menus). A real TUI would supersede it.
- **Grafana dashboards** define the metric set a Bee TUI would naturally surface: BZZ balance, cheques, peers, chunk counts, syncing, postage stamps.

## Sources

- [awesome-swarm README](https://raw.githubusercontent.com/ethersphere/awesome-swarm/master/README.md)
- [ethersphere org](https://github.com/ethersphere)
- [bee-dashboard repo](https://github.com/ethersphere/bee-dashboard)
- [swarm-desktop repo](https://github.com/ethersphere/swarm-desktop)
- [swarm-cli repo](https://github.com/ethersphere/swarm-cli)
- [Beest (w3rkspacelabs)](https://github.com/w3rkspacelabs/beest)
- [Bee Dashboard blog post](https://blog.ethswarm.org/foundation/2021/bee-dashboard/)
- [Swarm tools overview 2023](https://blog.ethswarm.org/foundation/2023/swarm-ecosystem-tools-update/)
