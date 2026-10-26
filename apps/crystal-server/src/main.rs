use std::io::IsTerminal;
use std::path::PathBuf;

use clap::Parser;
use crystal_common::{Config, Role};
use crystal_console::logs::Logging;
use crystal_runtime::Server;
use tracing::info;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Configuration file; missing is fine (defaults, then CRYSTAL_* environment variables).
    #[arg(long, default_value = "crystal.toml")]
    config: PathBuf,
    /// Services to run, overriding the configuration: lobby, zone, chat.
    #[arg(long, value_delimiter = ',')]
    role: Vec<Role>,
    /// Plain log lines on stdout instead of the console (always so when stdout is not a terminal,
    /// as under a service manager).
    #[arg(long)]
    plain: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut config = Config::load(&args.config)?;
    if !args.role.is_empty() {
        config.roles = args.role;
    }
    if args.plain || !std::io::stdout().is_terminal() {
        crystal_common::log::init(&config.log.filter);
        info!(version = env!("CARGO_PKG_VERSION"), config = %args.config.display(), "crystal-server");
        let server = Server::start(config).await?;
        tokio::signal::ctrl_c().await?;
        info!("shutting down");
        server.shutdown().await;
        return Ok(());
    }

    // The console: what starting logs is shown in it once it is up.
    let logging = Logging::install(&config.log.filter)?;
    if let Some(path) = &config.log.file
        && let Err(e) = logging.sink.log_to_file(path, config.log.file_max_mb * 1024 * 1024)
    {
        tracing::warn!(path = %path.display(), "log file: {e}");
    }
    info!(version = env!("CARGO_PKG_VERSION"), config = %args.config.display(), "crystal-server");
    let server = match Server::start(config).await {
        Ok(server) => server,
        Err(e) => {
            // no console to show it in: print what was logged, then the error
            logging.sink.with_lines(|lines| lines.iter().for_each(|l| println!("{}", l.text)));
            return Err(e);
        }
    };
    crystal_console::run(server, logging).await
}
