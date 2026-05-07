use bee_tui::{
    app::{App, AppOverrides},
    cli::Cli,
};
use clap::Parser;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    bee_tui::errors::init()?;
    bee_tui::logging::init()?;

    let args = Cli::parse();
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
    Ok(())
}
