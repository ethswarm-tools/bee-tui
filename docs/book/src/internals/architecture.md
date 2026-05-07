# Architecture

Stub — high level: tokio watch/informer hub fans out per-resource pollers behind `tokio::sync::watch` channels; screens own a `Receiver<T>` plus a pure `view_for(snap) -> View` fn; render delegates to the view. The full picture is in [`docs/PLAN.md`](https://github.com/ethswarm-tools/bee-tui/blob/main/docs/PLAN.md) § 6.
