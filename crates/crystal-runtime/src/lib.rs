//! A server is a value: [`Server::start`] connects the databases and the shared state, binds the
//! configured services and returns; [`Server::shutdown`] stops them. No globals, so a process (the
//! binary, a test, the embedded client) can run any number of them.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, bail};
use crystal_common::{Config, Role};
use crystal_db::Databases;
use crystal_lobby::Lobby;
use crystal_net::Listener;
use crystal_state::State;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::info;

pub struct Server {
    pub db: Databases,
    pub state: State,
    addrs: HashMap<Role, SocketAddr>,
    shutdown: CancellationToken,
    tasks: TaskTracker,
}

impl Server {
    pub async fn start(config: Config) -> anyhow::Result<Self> {
        if config.roles.is_empty() {
            bail!("no roles configured (lobby, zone, chat)");
        }
        if let Some(role) = config.roles.iter().find(|r| **r != Role::Lobby) {
            bail!("the {role:?} service arrives in Phase 2; run lobby only for now");
        }

        let db = Databases::connect(&config.database).await.context("databases")?;
        if config.database.auto_migrate {
            db.migrate().await.context("migrations")?;
        }
        let state = State::connect(&config.state).await.context("shared state")?;
        state.ping().await.context("shared state")?;
        info!(backend = state.backend(), "shared state");

        let shutdown = CancellationToken::new();
        let tasks = TaskTracker::new();
        let mut addrs = HashMap::new();
        for role in &config.roles {
            match role {
                Role::Lobby => {
                    let listener = Listener::bind("lobby", config.lobby.bind).await.context("lobby")?;
                    addrs.insert(Role::Lobby, listener.local_addr()?);
                    let lobby = Arc::new(Lobby::new(db.clone(), state.clone(), config.lobby.capture_dir.clone()));
                    tasks.spawn(crystal_net::serve(listener, shutdown.clone(), move |stream, peer, stop| {
                        lobby.clone().handle(stream, peer, stop)
                    }));
                }
                Role::Zone | Role::Chat => unreachable!(),
            }
        }
        tasks.close();
        Ok(Self { db, state, addrs, shutdown, tasks })
    }

    /// Where a running service listens (useful when it was bound to port 0).
    pub fn local_addr(&self, role: Role) -> Option<SocketAddr> {
        self.addrs.get(&role).copied()
    }

    pub async fn shutdown(self) {
        self.shutdown.cancel();
        self.tasks.wait().await;
        self.db.close().await;
    }
}
