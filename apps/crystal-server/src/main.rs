use std::path::PathBuf;

use clap::Parser;
use crystal_common::{Config, Role};
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut config = Config::load(&args.config)?;
    if !args.role.is_empty() {
        config.roles = args.role;
    }
    crystal_common::log::init(&config.log.filter);
    info!(version = env!("CARGO_PKG_VERSION"), config = %args.config.display(), "crystal-server");

    let server = Server::start(config).await?;
    tokio::signal::ctrl_c().await?;
    info!("shutting down");
    server.shutdown().await;
    Ok(())
}
