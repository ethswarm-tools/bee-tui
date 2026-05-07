use bee_tui::{app::App, cli::Cli};
use clap::Parser;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    bee_tui::errors::init()?;
    bee_tui::logging::init()?;

    let args = Cli::parse();
    let mut app = App::with_overrides(args.tick_rate, args.frame_rate, args.ascii, args.no_color)?;
    app.run().await?;
    Ok(())
}
