//! `crystal.toml`, with environment overrides: `CRYSTAL_<SECTION>__<KEY>`, for example
//! `CRYSTAL_DATABASE__LOGIN__URL` or `CRYSTAL_STATE__REDIS_URL`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
#[error("configuration: {0}")]
pub struct ConfigError(#[from] Box<figment::Error>);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub log: LogConfig,
    /// Services this process runs.
    pub roles: Vec<Role>,
    pub database: DatabaseConfig,
    pub state: StateConfig,
    pub lobby: LobbyConfig,
    pub zone: ServiceConfig,
    pub chat: ServiceConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Lobby,
    Zone,
    Chat,
}

impl std::str::FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lobby" => Ok(Role::Lobby),
            "zone" => Ok(Role::Zone),
            "chat" => Ok(Role::Chat),
            other => Err(format!("unknown role `{other}` (lobby, zone, chat)")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    /// `tracing` filter directives, as in `RUST_LOG`.
    pub filter: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    /// Apply pending migrations on startup.
    pub auto_migrate: bool,
    pub login: DatabaseEndpoint,
    pub characters: DatabaseEndpoint,
    pub world: DatabaseEndpoint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseEndpoint {
    /// `postgres://...` or `sqlite:path?mode=rwc`.
    pub url: String,
    pub max_connections: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StateConfig {
    /// Redis (or Valkey) URL. Unset: in-process state, for a single process or the embedded build.
    pub redis_url: Option<String>,
    /// Prefix for every key and channel, so several realms can share one Redis.
    pub prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceConfig {
    pub bind: SocketAddr,
    /// When set, every connection's received frames are written here, raw, for replay tests.
    pub capture_dir: Option<PathBuf>,
}

/// The lobby. The client opens one connection to each of its two lobby hosts (`DEV.LobbyHost01`/
/// `02`, `DEV.LobbyPort01`/`02`), and each feeds its own region of the world list (JP, NA/EU).
/// Both connections look alike on the wire, so to tell them apart give the second host its own
/// port.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LobbyConfig {
    /// Lobby host 01 (or both, when `bind_2` is unset).
    pub bind: SocketAddr,
    /// Lobby host 02.
    pub bind_2: Option<SocketAddr>,
    /// When the hosts are apart: the host (1 or 2) that shows the Lobby world, where characters
    /// are made (and not played). The other shows Crystal, to play on. The client makes characters
    /// through host 01 (its JP region), so this is 1. With one port for both, every region shows
    /// Crystal, open for both.
    pub create_host: u8,
    /// When set, every connection's received frames are written here, raw, for replay tests.
    pub capture_dir: Option<PathBuf>,
}

impl Default for LobbyConfig {
    fn default() -> Self {
        Self { bind: SocketAddr::from(([127, 0, 0, 1], 54994)), bind_2: None, create_host: 1, capture_dir: None }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            log: LogConfig::default(),
            roles: vec![Role::Lobby],
            database: DatabaseConfig::default(),
            state: StateConfig { redis_url: None, prefix: "crystal".into() },
            lobby: LobbyConfig::default(),
            zone: ServiceConfig::at(54992),
            chat: ServiceConfig::at(54993),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self { filter: "info".into() }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            auto_migrate: true,
            login: DatabaseEndpoint::sqlite("var/login.db"),
            characters: DatabaseEndpoint::sqlite("var/characters.db"),
            world: DatabaseEndpoint::sqlite("var/world.db"),
        }
    }
}

impl Default for DatabaseEndpoint {
    fn default() -> Self {
        Self::sqlite("var/crystal.db")
    }
}

impl DatabaseEndpoint {
    /// `sqlite:<path>`, not `sqlite://<path>`: a URL authority would swallow a Windows drive letter.
    pub fn sqlite(path: &str) -> Self {
        Self { url: format!("sqlite:{path}?mode=rwc"), max_connections: 8 }
    }
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self::at(0)
    }
}

impl ServiceConfig {
    fn at(port: u16) -> Self {
        Self { bind: SocketAddr::from(([127, 0, 0, 1], port)), capture_dir: None }
    }
}

impl Config {
    /// Defaults, then `path` if it exists, then `CRYSTAL_*` environment variables.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Figment::from(Serialized::defaults(Config::default()))
            .merge(Toml::file(path))
            .merge(Env::prefixed("CRYSTAL_").split("__"))
            .extract()
            .map_err(|e| ConfigError(Box::new(e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_without_a_file() {
        let config = Config::load(Path::new("does-not-exist.toml")).unwrap();
        assert_eq!(config.roles, vec![Role::Lobby]);
        assert_eq!(config.lobby.bind.port(), 54994);
        assert!(config.state.redis_url.is_none());
    }

    #[test]
    fn file_overrides_defaults() {
        let config: Config = Figment::from(Serialized::defaults(Config::default()))
            .merge(Toml::string(
                r#"
                roles = ["lobby", "chat"]
                [state]
                redis_url = "redis://127.0.0.1:6379"
                [database.login]
                url = "postgres://crystal@127.0.0.1/crystal_login"
                "#,
            ))
            .extract()
            .unwrap();
        assert_eq!(config.roles, vec![Role::Lobby, Role::Chat]);
        assert_eq!(config.state.redis_url.as_deref(), Some("redis://127.0.0.1:6379"));
        assert_eq!(config.database.login.url, "postgres://crystal@127.0.0.1/crystal_login");
        assert_eq!(config.database.login.max_connections, 8);
        assert!(config.database.world.url.starts_with("sqlite:"));
    }
}
