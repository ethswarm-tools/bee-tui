use std::path::PathBuf;

use clap::Parser;

use crate::config::{get_config_dir, get_data_dir};

#[derive(Parser, Debug)]
#[command(author, version = version(), about)]
pub struct Cli {
    /// Tick rate, i.e. number of ticks per second
    #[arg(short, long, value_name = "FLOAT", default_value_t = 4.0)]
    pub tick_rate: f64,

    /// Frame rate, i.e. number of frames per second
    #[arg(short, long, value_name = "FLOAT", default_value_t = 60.0)]
    pub frame_rate: f64,

    /// Render with ASCII glyphs only — no Unicode (✓ ⚠ ✗ ▶ ▇ …).
    /// Use on terminals with poor Unicode support: Windows Terminal
    /// pre-Win11, screen readers, some SSH chains. Equivalent to
    /// setting `[ui].ascii_fallback = true` in `config.toml`.
    #[arg(long)]
    pub ascii: bool,

    /// Suppress colour output regardless of the configured theme.
    /// Equivalent to setting `[ui].theme = "mono"` in `config.toml`,
    /// or to `NO_COLOR=1` in the environment (which is also honoured
    /// automatically per <https://no-color.org>).
    #[arg(long)]
    pub no_color: bool,

    /// Path to a `bee` binary to spawn before opening the cockpit.
    /// When set together with `--bee-config`, bee-tui starts Bee as
    /// a child process, captures its log into a temp file, waits for
    /// the API to come up, then opens the cockpit. Overrides
    /// `[bee].bin` from `config.toml`.
    #[arg(long, value_name = "PATH")]
    pub bee_bin: Option<PathBuf>,

    /// Path to the Bee YAML config the spawned binary should use.
    /// Required when `--bee-bin` is set unless `[bee].config` is
    /// already in `config.toml`. Overrides `[bee].config`.
    #[arg(long, value_name = "PATH")]
    pub bee_config: Option<PathBuf>,
}

const VERSION_MESSAGE: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "-",
    env!("VERGEN_GIT_DESCRIBE"),
    " (",
    env!("VERGEN_BUILD_DATE"),
    ")"
);

pub fn version() -> String {
    let author = clap::crate_authors!();

    // let current_exe_path = PathBuf::from(clap::crate_name!()).display().to_string();
    let config_dir_path = get_config_dir().display().to_string();
    let data_dir_path = get_data_dir().display().to_string();

    format!(
        "\
{VERSION_MESSAGE}

Authors: {author}

Config directory: {config_dir_path}
Data directory: {data_dir_path}"
    )
}
