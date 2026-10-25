//! State shared between services and processes: keys with expiry (sessions, presence, handoff
//! tokens) and pub/sub (chat, cross-zone messages, world-wide events).
//!
//! Hosted servers use Redis, so lobby, zone and chat processes can run on different machines.
//! A single process, and the embedded build, use the in-process backend. Both have the same
//! semantics; nothing on a tick path waits on either.
//!
//! [`State`] is an enum rather than a trait object: calls are static dispatch, and the `redis`
//! feature removes the Redis variant (and the dependency) from the embedded build.

mod memory;
#[cfg(feature = "redis")]
mod redis_backend;

use std::time::Duration;

use bytes::Bytes;
use crystal_common::config::StateConfig;
use tokio::sync::broadcast;
use tracing::warn;

pub use memory::MemoryState;
#[cfg(feature = "redis")]
pub use redis_backend::RedisState;

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[cfg(feature = "redis")]
    #[error("redis: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("redis_url is set but this build has no Redis support")]
    RedisUnavailable,
}

pub type Result<T> = std::result::Result<T, StateError>;

/// Per-channel buffer. A subscriber this far behind loses the oldest messages.
pub(crate) const CHANNEL_CAPACITY: usize = 4096;

#[derive(Clone)]
pub enum State {
    Memory(MemoryState),
    #[cfg(feature = "redis")]
    Redis(RedisState),
}

macro_rules! dispatch {
    ($self:ident, $s:ident => $e:expr) => {
        match $self {
            State::Memory($s) => $e,
            #[cfg(feature = "redis")]
            State::Redis($s) => $e,
        }
    };
}

impl State {
    /// Redis when `redis_url` is set, in-process otherwise.
    pub async fn connect(config: &StateConfig) -> Result<Self> {
        match config.redis_url.as_deref() {
            None | Some("") => Ok(Self::memory(&config.prefix)),
            #[cfg(feature = "redis")]
            Some(url) => Ok(Self::Redis(RedisState::connect(url, &config.prefix).await?)),
            #[cfg(not(feature = "redis"))]
            Some(_) => Err(StateError::RedisUnavailable),
        }
    }

    pub fn memory(prefix: &str) -> Self {
        Self::Memory(MemoryState::new(prefix))
    }

    pub fn backend(&self) -> &'static str {
        dispatch!(self, _s => _s.backend())
    }

    pub async fn ping(&self) -> Result<()> {
        dispatch!(self, s => s.ping().await)
    }

    pub async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        dispatch!(self, s => s.get(key).await)
    }

    /// Sets `key`, replacing any value; `ttl` of `None` keeps it until deleted.
    pub async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<()> {
        dispatch!(self, s => s.set(key, value, ttl).await)
    }

    /// Sets `key` only if it does not exist. Returns whether it was set.
    pub async fn set_if_absent(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<bool> {
        dispatch!(self, s => s.set_if_absent(key, value, ttl).await)
    }

    /// Returns whether the key existed.
    pub async fn delete(&self, key: &str) -> Result<bool> {
        dispatch!(self, s => s.delete(key).await)
    }

    /// Returns how many subscribers received it (on Redis: across all processes).
    pub async fn publish(&self, channel: &str, message: &[u8]) -> Result<usize> {
        dispatch!(self, s => s.publish(channel, message).await)
    }

    pub async fn subscribe(&self, channel: &str) -> Result<Subscription> {
        dispatch!(self, s => s.subscribe(channel).await)
    }
}

/// Messages published to one channel, from the moment of subscribing.
pub struct Subscription {
    channel: String,
    rx: broadcast::Receiver<Bytes>,
}

impl Subscription {
    pub(crate) fn new(channel: String, rx: broadcast::Receiver<Bytes>) -> Self {
        Self { channel, rx }
    }

    /// The next message; `None` once the backend has shut down. A subscriber that falls more than
    /// the channel's capacity behind skips what it missed.
    pub async fn recv(&mut self) -> Option<Bytes> {
        loop {
            match self.rx.recv().await {
                Ok(message) => return Some(message),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(channel = %self.channel, missed = n, "subscriber lagged");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests;
