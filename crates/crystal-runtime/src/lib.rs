//! A server is a value: [`Server::start`] connects the databases and the shared state, binds the
//! configured services and returns; [`Server::shutdown`] stops them. No globals, so a process (the
//! binary, a test, the embedded client) can run any number of them.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, bail};
use crystal_common::{Config, Role};
use crystal_data::WorldData;
use crystal_db::Databases;
use crystal_lobby::{HostRole, Lobby};
use crystal_net::Listener;
use crystal_state::State;
use crystal_zone::Zone;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{info, warn};

/// Which of the client's lobby hosts a lobby port serves.
enum Host {
    Both,
    One(u8),
}

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
        if config.roles.contains(&Role::Chat) {
            bail!("the chat service is not there yet; run lobby and zone");
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
                    let c = &config.lobby;
                    let lobby =
                        Arc::new(Lobby::new(db.clone(), state.clone(), config.zone.bind, c.capture_dir.clone()));
                    let hosts = match c.bind_2 {
                        None => vec![(c.bind, Host::Both)],
                        Some(bind_2) => vec![(c.bind, Host::One(1)), (bind_2, Host::One(2))],
                    };
                    for (i, (bind, host)) in hosts.into_iter().enumerate() {
                        let listener = Listener::bind("lobby", bind).await.context("lobby")?;
                        if i == 0 {
                            addrs.insert(Role::Lobby, listener.local_addr()?);
                        }
                        let role = match host {
                            Host::Both => HostRole::Both,
                            Host::One(n) if n == c.create_host => HostRole::Create,
                            Host::One(_) => HostRole::Play,
                        };
                        let lobby = lobby.clone();
                        tasks.spawn(crystal_net::serve(listener, shutdown.clone(), move |stream, peer, stop| {
                            lobby.clone().handle(stream, peer, role, stop)
                        }));
                    }
                }
                Role::Zone => {
                    let world = match &config.data.game {
                        Some(game) => {
                            let world =
                                WorldData::open(game).with_context(|| format!("game data in {}", game.display()))?;
                            info!(game = %game.display(), "game data");
                            Some(Arc::new(world))
                        }
                        None => {
                            warn!("no game data configured ([data] game): zone exits are off");
                            None
                        }
                    };
                    let listener = Listener::bind("zone", config.zone.bind).await.context("zone")?;
                    addrs.insert(Role::Zone, listener.local_addr()?);
                    let zone = Arc::new(Zone::new(db.clone(), state.clone(), world, config.zone.capture_dir.clone()));
                    tasks.spawn(crystal_net::serve(listener, shutdown.clone(), move |stream, peer, stop| {
                        zone.clone().handle(stream, peer, stop)
                    }));
                }
                Role::Chat => unreachable!(),
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
