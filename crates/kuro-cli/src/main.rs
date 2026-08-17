//! `kuro` — terminal anime streaming.

mod app;
mod cli;
mod commands;
mod flow;
mod playback;
mod ui;

use anyhow::Result;
use app::App;
use clap::{CommandFactory, Parser};
use cli::{Cli, Command};
use tracing_subscriber::EnvFilter;

fn init_tracing(verbose: u8) {
    // Silent by default: warnings and errors are printed as user-facing lines
    // (the `⚠`/`error:` messages), so emitting the structured tracing form on top
    // is just the same problem twice. `-v` opts into the diagnostic log.
    let default = match verbose {
        0 => "off",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    // RUST_LOG still wins, so -v is a convenience rather than an override.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("kuro={default},{default}")));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    if let Err(err) = run(cli).await {
        eprintln!("\x1b[31merror:\x1b[0m {err}");
        for cause in err.chain().skip(1) {
            eprintln!("  caused by: {cause}");
        }
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    // Completions need no config, and must work before anything is set up.
    if let Some(Command::Completions { shell }) = &cli.command {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        clap_complete::generate(*shell, &mut cmd, name, &mut std::io::stdout());
        return Ok(());
    }

    let mut app = App::new(
        cli.provider,
        cli.quality,
        cli.select_nth,
        cli.skip,
        cli.json,
        cli.dry_run,
        cli.no_cache,
    )?;

    match &cli.command {
        // At a terminal, searching leads somewhere: results become a browsable
        // list. Piped or `--json`, it stays a plain printable list.
        Some(Command::Search { query }) if ui::interactive() && !app.json => {
            flow::run(&mut app, query).await
        }
        Some(Command::Search { query }) => commands::search(&mut app, query).await,
        Some(Command::Play { query, ep, mirror }) => {
            commands::play_cmd(&mut app, query, *ep, mirror.clone()).await
        }
        Some(Command::Download {
            query,
            ep,
            all,
            mirror,
            out,
            jobs,
        }) => commands::download(&mut app, query, *ep, *all, mirror.clone(), out, *jobs).await,
        Some(Command::Next) => commands::next(&mut app).await,
        Some(Command::Continue) => commands::continue_watching(&mut app).await,
        Some(Command::History { limit, clear }) => commands::history(&app, *limit, *clear),
        Some(Command::Bookmark { action }) => commands::bookmark(&mut app, action).await,
        Some(Command::Provider { action }) => commands::provider_cmd(&mut app, action).await,
        Some(Command::Config { action }) => commands::config_cmd(&app, action),
        Some(Command::Cache { action }) => commands::cache_cmd(&app, action),
        Some(Command::Doctor) => commands::doctor(&mut app).await,
        Some(Command::Completions { .. }) => unreachable!("handled above"),

        None => {
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}
