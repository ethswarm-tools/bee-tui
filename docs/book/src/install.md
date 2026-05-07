# Install

bee-tui ships as a single static binary — no Rust toolchain
required, no Python runtime, no Docker. Pick the option that
matches your platform.

## Prebuilt installer (recommended)

Every GitHub release publishes prebuilt binaries for five
targets via [cargo-dist](https://github.com/axodotdev/cargo-dist).
The matching one-line installer detects your CPU + OS and
downloads the right tarball.

### Linux / macOS

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/ethswarm-tools/bee-tui/releases/latest/download/bee-tui-installer.sh \
  | sh
```

The installer writes the binary into `$XDG_BIN_HOME`
(`~/.local/bin` if unset). Make sure that directory is on
your `$PATH` — the script reminds you if it isn't.

### Windows

```powershell
powershell -c "irm https://github.com/ethswarm-tools/bee-tui/releases/latest/download/bee-tui-installer.ps1 | iex"
```

### What's in the installer

- Linux x86_64 / arm64 → `.tar.xz` with a stripped ELF binary
- macOS x86_64 / arm64 → `.tar.xz` with a stripped Mach-O binary
- Windows x86_64 → `.zip` with `bee-tui.exe`
- README, CHANGELOG, LICENSE-MIT, LICENSE-APACHE bundled
  alongside the binary
- Per-tarball `.sha256` checksum + a top-level `sha256.sum`
  manifest covering every artifact

If you'd rather verify by hand, fetch the artifact directly
from the [releases page](https://github.com/ethswarm-tools/bee-tui/releases)
and check it against `sha256.sum`.

## From source (cargo)

If you have a Rust toolchain (≥ 1.85, the project's MSRV):

```sh
cargo install bee-tui
```

The binary lands in `~/.cargo/bin/bee-tui`. This route compiles
from source against the latest crates.io release; expect a 30-60
second compile on a modern laptop, longer on a Raspberry Pi.

## From source (git)

If you want to track `main` or hack on the cockpit:

```sh
git clone https://github.com/ethswarm-tools/bee-tui
cd bee-tui
cargo build --release
./target/release/bee-tui --version
```

The `dist` profile in `Cargo.toml` matches what cargo-dist uses
for releases (LTO thin, optimised); `--release` uses the
standard release profile and takes a hair longer at runtime.

## Verifying the install

```sh
bee-tui --version
# bee-tui 1.0.0
```

If the version prints and Bee is running on `localhost:1633`,
the cockpit will launch with no further configuration:

```sh
bee-tui
```

## Platform-specific notes

### macOS Gatekeeper

The released binary is unsigned (no Apple Developer signature).
The first time you run it, macOS may show a "developer cannot
be verified" dialog. To approve it:

```sh
xattr -d com.apple.quarantine "$(which bee-tui)"
```

Or right-click → Open in Finder once. Apple notarisation is on
the v1.x roadmap.

### Windows Defender SmartScreen

Same story — the binary is unsigned, so SmartScreen may flag
it on first launch. Click "More info" → "Run anyway", or run
from a PowerShell prompt where the `irm | iex` install step
already implicitly accepted execution.

### Corporate / restricted environments

If `curl` / `iwr` to `github.com` is blocked, download the
tarball from another machine, transfer it manually, and verify
the sha256 by hand:

```sh
sha256sum bee-tui-x86_64-unknown-linux-gnu.tar.xz
# compare against the value in sha256.sum from the release page
```

## Uninstall

The shell installer drops a marker file at
`$XDG_DATA_HOME/bee-tui/installed-files.txt` (or
`~/.local/share/bee-tui/installed-files.txt`) listing every
file it placed. Removing those files cleanly uninstalls. Or
simply delete the binary:

```sh
rm "$(which bee-tui)"
```

Configuration at `~/.config/bee-tui/config.toml` is not touched
by the installer and stays put across upgrades.
