//! trace-diff — interactive terminal-native network & API regression diagnostics.

use clap::Parser;
use trace_diff::cli::{Cli, Commands};
use trace_diff::error::Result;
use trace_diff::tui;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> miette::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    color_eyre::install().ok();

    let cli = Cli::parse();
    init_tracing(cli.verbose, cli.debug);

    run(cli).await.map_err(|e| miette::miette!("{e}"))
}

fn init_tracing(verbose: u8, debug: bool) {
    // Default to warn so INFO probe logs do not corrupt the interactive TUI
    // (stderr shares the terminal with the alternate screen).
    let level = if debug || verbose >= 2 {
        "trace"
    } else if verbose == 1 {
        "debug"
    } else {
        "warn"
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("trace_diff={level},warn")));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(verbose > 0 || debug)
        .with_writer(std::io::stderr)
        .init();
}

async fn run(cli: Cli) -> Result<()> {
    let theme = tui::theme_from_flags(cli.theme, cli.no_color, cli.force_color);
    let ui = trace_diff::cli::run::UiOpts { theme };
    match cli.command {
        Commands::Run(args) => trace_diff::cli::run::execute(args, ui).await,
        Commands::Features(args) => trace_diff::cli::features_cmd::execute(args, ui).await,
        Commands::Baseline(args) => trace_diff::cli::baseline::execute(args).await,
        Commands::Diff(args) => trace_diff::cli::diff_cmd::execute(args, ui).await,
        Commands::List(args) => trace_diff::cli::list::execute(args).await,
    }
}
