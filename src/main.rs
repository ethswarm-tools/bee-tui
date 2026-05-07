use bee_tui::{app::App, cli::Cli};
use clap::Parser;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    bee_tui::errors::init()?;
    bee_tui::logging::init()?;

    let args = Cli::parse();
    let mut app = App::new(args.tick_rate, args.frame_rate)?;
    app.run().await?;
    Ok(())
}
