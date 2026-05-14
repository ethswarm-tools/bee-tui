use std::path::PathBuf;

use clap::Parser;

use crate::config::{config_search_dirs, get_data_dir, resolved_config_dir};

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

    /// Path to an already-running Bee's log file to tail. Use this
    /// when bee-tui connects to an external Bee (not spawned via
    /// `--bee-bin`) but you still want its log lines in the bottom
    /// pane's Bee-side tabs. Tailing starts at end-of-file — existing
    /// history is not replayed. Overrides `log_file` on the active
    /// `[[nodes]]` entry. Ignored when `--bee-bin` is set.
    #[arg(long, value_name = "PATH")]
    pub bee_log: Option<PathBuf>,

    /// Shell command whose stdout streams an already-running Bee's
    /// log. Run via `sh -c`, so pipes / quoting / redirects work:
    /// `journalctl -u bee -f`, `docker logs -f bee 2>&1`,
    /// `kubectl logs -f bee-0`, `ssh host 'tail -f /var/log/bee.log'`.
    /// The cockpit tails the command's stdout into the bottom pane's
    /// Bee-side tabs and kills the child on quit. Overrides
    /// `log_command` on the active `[[nodes]]` entry. Takes precedence
    /// over `--bee-log`; ignored when `--bee-bin` is set.
    #[arg(long, value_name = "CMD")]
    pub bee_log_cmd: Option<String>,

    /// Run a single verb without launching the TUI. Prints the
    /// result on stdout and exits with `0` (ok), `1` (unhealthy /
    /// failed), or `2` (usage error). Designed for CI / shell
    /// pipelines: `bee-tui --once readiness` is the canonical
    /// "is my Bee node ready for traffic?" smoke test.
    ///
    /// Supported verbs: `hash`, `cid`, `depth-table`, `pss-target`,
    /// `gsoc-mine`, `readiness`, `version-check`, `inspect`,
    /// `durability-check`. Trailing positional args go to the verb.
    #[arg(long, value_name = "VERB")]
    pub once: Option<String>,

    /// When `--once` is set, emit a single JSON object on stdout
    /// instead of the human-readable line. Field shape: `{ verb,
    /// status, message, data }`. Lets CI parse the result without
    /// regex on the human line.
    #[arg(long)]
    pub json: bool,

    /// Trailing positional args consumed by `--once <verb>`. Not
    /// used in TUI mode.
    #[arg(trailing_var_arg = true)]
    pub once_args: Vec<String>,
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
    let config_dir_path = match resolved_config_dir() {
        Some(dir) => dir.display().to_string(),
        None => format!(
            "(no config file found — searched: {})",
            config_search_dirs()
                .iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let data_dir_path = get_data_dir().display().to_string();

    format!(
        "\
{VERSION_MESSAGE}

Authors: {author}

Config directory: {config_dir_path}
Data directory: {data_dir_path}"
    )
}
