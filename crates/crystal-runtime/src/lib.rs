//! A server is a value: [`Server::start`] connects the databases and the shared state, binds the
//! configured services and returns; [`Server::shutdown`] stops them. No globals, so a process (the
//! binary, a test, the embedded client) can run any number of them.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, bail};
use crystal_common::{Config, Role};
use crystal_data::{Overrides, WorldData};
use crystal_db::{BETA_PATCH, Databases};
use crystal_lobby::{HostRole, Lobby};
use crystal_net::Listener;
use crystal_state::State;
use crystal_world::{BnpcPlacement, NpcPlacement, TerritoryContent, World};
use crystal_zone::Zone;
use crystal_zone::commands::Commands;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{info, warn};

/// The client build whose folder under `[data] seed` the world is seeded from.
pub const SEED_BUILD: &str = "672261";

/// Which of the client's lobby hosts a lobby port serves.
enum Host {
    Both,
    One(u8),
}

pub struct Server {
    pub db: Databases,
    pub state: State,
    addrs: HashMap<Role, SocketAddr>,
    /// Where lobby hosts 01 and 02 listen (one address when both share a port).
    lobby_hosts: Vec<SocketAddr>,
    /// The client install's `game` folder, when configured.
    pub game: Option<std::path::PathBuf>,
    /// The zone service's territories.
    worlds: Vec<Arc<World>>,
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
        let mut lobby_hosts = Vec::new();
        let mut worlds = Vec::new();
        // the client's game data, read once for every service that needs it
        let game_data = match &config.data.game {
            Some(game) => {
                let mut overrides = Overrides::default();
                for o in db.world.exd_overrides(BETA_PATCH).await.context("exd overrides")? {
                    overrides.add(&o.sheet, o.row_id, o.col as usize, o.value);
                }
                let count = overrides.len();
                let world = WorldData::open_with(game, overrides)
                    .with_context(|| format!("game data in {}", game.display()))?;
                info!(game = %game.display(), overrides = count, "game data");
                Some(Arc::new(world))
            }
            None => {
                warn!("no game data configured ([data] game): zone exits are off, characters cannot be made");
                None
            }
        };
        for role in &config.roles {
            match role {
                Role::Lobby => {
                    let c = &config.lobby;
                    let lobby = Arc::new(Lobby::new(
                        db.clone(),
                        state.clone(),
                        config.zone.bind,
                        c.capture_dir.clone(),
                        game_data.clone(),
                    ));
                    let hosts = match c.bind_2 {
                        None => vec![(c.bind, Host::Both)],
                        Some(bind_2) => vec![(c.bind, Host::One(1)), (bind_2, Host::One(2))],
                    };
                    for (i, (bind, host)) in hosts.into_iter().enumerate() {
                        let listener = Listener::bind("lobby", bind).await.context("lobby")?;
                        if i == 0 {
                            addrs.insert(Role::Lobby, listener.local_addr()?);
                        }
                        lobby_hosts.push(listener.local_addr()?);
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
                    let world = game_data.clone();
                    let listener = Listener::bind("zone", config.zone.bind).await.context("zone")?;
                    addrs.insert(Role::Zone, listener.local_addr()?);
                    // a new server's world gets the repository's starting content
                    if let Some(seed) = &config.data.seed {
                        crystal_db::seed::seed_world(&db.world, &seed.join(SEED_BUILD), BETA_PATCH)
                            .await
                            .context("seed data")?;
                    }
                    // battle NPCs from the world database, by territory (read once; a few MB at most)
                    let mut bnpcs: HashMap<u16, Vec<BnpcPlacement>> = HashMap::new();
                    for s in db.world.bnpc_spawns(BETA_PATCH, None).await.context("battle NPC spawns")? {
                        bnpcs.entry(s.territory).or_default().push(BnpcPlacement {
                            base_id: s.base_id,
                            name_id: s.name_id,
                            level: s.level,
                            pos: s.pos,
                            rotation: s.rotation,
                            aggressive: s.aggressive,
                        });
                    }
                    info!(
                        spawns = bnpcs.values().map(Vec::len).sum::<usize>(),
                        territories = bnpcs.len(),
                        "battle NPCs"
                    );
                    let npcs_from = world.clone();
                    let npcs = Box::new(move |territory: u16| -> TerritoryContent {
                        let bnpcs = bnpcs.get(&territory).cloned().unwrap_or_default();
                        let Some(data) = &npcs_from else { return TerritoryContent { enpcs: vec![], bnpcs } };
                        let enpcs = match data.npcs(territory) {
                            Ok(n) => n
                                .iter()
                                .map(|n| NpcPlacement { enpc: n.enpc, pos: n.pos, rotation: n.rotation })
                                .collect(),
                            Err(e) => {
                                warn!(territory, "event NPCs: {e}");
                                vec![]
                            }
                        };
                        TerritoryContent { enpcs, bnpcs }
                    });
                    let territories = Arc::new(World::new(npcs, shutdown.child_token()));
                    let mut commands = Commands::default();
                    mod_gm::register(&mut commands);
                    let zone = Arc::new(Zone::new(
                        db.clone(),
                        state.clone(),
                        world,
                        territories.clone(),
                        commands,
                        config.zone.capture_dir.clone(),
                    ));
                    worlds.push(territories);
                    tasks.spawn(crystal_net::serve(listener, shutdown.clone(), move |stream, peer, stop| {
                        zone.clone().handle(stream, peer, stop)
                    }));
                }
                Role::Chat => unreachable!(),
            }
        }
        tasks.close();
        Ok(Self { db, state, addrs, lobby_hosts, game: config.data.game.clone(), worlds, shutdown, tasks })
    }

    /// Where a running service listens (useful when it was bound to port 0).
    /// The zone service's territories, when it runs here.
    pub fn world(&self) -> Option<&Arc<World>> {
        self.worlds.first()
    }

    /// Where lobby hosts 01 and 02 listen: the client's `DEV.LobbyPort01`/`02`.
    pub fn lobby_hosts(&self) -> &[SocketAddr] {
        &self.lobby_hosts
    }

    pub fn local_addr(&self, role: Role) -> Option<SocketAddr> {
        self.addrs.get(&role).copied()
    }

    pub async fn shutdown(self) {
        self.shutdown.cancel();
        self.tasks.wait().await;
        for world in &self.worlds {
            world.shutdown().await;
        }
        self.db.close().await;
    }
}
