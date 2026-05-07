# Changelog

All notable changes to bee-tui will be documented in this file. The
format follows [Keep a Changelog]; the project adheres to
[Semantic Versioning].

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html

## [Unreleased]

## [0.0.1] - 2026-05-07

### Added

- **Initial crates.io reservation publish.** Scaffolds the bee-tui
  binary from the upstream
  [ratatui/templates component](https://github.com/ratatui/templates)
  template. Builds, runs, and prints a placeholder home screen — no
  bee-rs integration or operator screens yet. The reservation
  protects the `bee-tui` crate name while the implementation work
  outlined in [`docs/PLAN.md`](docs/PLAN.md) lands.

### Notes

- This release is **not functional** for Bee operators. Watch the
  repository for `0.1.0`, which lands the first three screens
  (S1 Health gates, S2 Stamps, S10 Command log).
