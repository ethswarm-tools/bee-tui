use std::process::ExitCode;

use bee_tui::{
    app::{App, AppOverrides},
    cli::Cli,
};
use clap::Parser;

#[tokio::main]
async fn main() -> color_eyre::Result<ExitCode> {
    bee_tui::errors::init()?;

    let args = Cli::parse();

    // CI single-shot mode: skip the TUI entirely, run one verb,
    // print to stdout, exit with a meaningful status code. We do
    // NOT initialise tracing/logging in this path — `--once` should
    // emit only its single line of stdout so shell pipelines can
    // grep cleanly without filtering log noise.
    if let Some(verb) = args.once.as_deref() {
        let code = bee_tui::once::run(verb, &args.once_args, args.json).await;
        return Ok(code);
    }

    bee_tui::logging::init()?;
    let mut app = App::with_overrides(
        args.tick_rate,
        args.frame_rate,
        AppOverrides {
            ascii: args.ascii,
            no_color: args.no_color,
            bee_bin: args.bee_bin,
            bee_config: args.bee_config,
        },
    )
    .await?;
    app.run().await?;
    Ok(ExitCode::SUCCESS)
}
