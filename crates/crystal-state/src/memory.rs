//! The in-process backend: one process, or the embedded build.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::broadcast;

use crate::{CHANNEL_CAPACITY, Result, Subscription};

/// Expired keys are removed when read, and by a sweep every this many writes.
const SWEEP_EVERY: u32 = 1024;

#[derive(Clone)]
pub struct MemoryState(Arc<Inner>);

struct Inner {
    prefix: String,
    keys: Mutex<HashMap<String, Entry>>,
    channels: Mutex<HashMap<String, broadcast::Sender<Bytes>>>,
    writes: AtomicU32,
}

struct Entry {
    value: Bytes,
    expires: Option<Instant>,
}

impl Entry {
    fn live(&self, now: Instant) -> bool {
        self.expires.is_none_or(|t| t > now)
    }
}

impl MemoryState {
    pub fn new(prefix: &str) -> Self {
        Self(Arc::new(Inner {
            prefix: prefix.to_owned(),
            keys: Mutex::default(),
            channels: Mutex::default(),
            writes: AtomicU32::new(0),
        }))
    }

    pub fn backend(&self) -> &'static str {
        "memory"
    }

    fn key(&self, key: &str) -> String {
        format!("{}:{key}", self.0.prefix)
    }

    fn entry(value: &[u8], ttl: Option<Duration>) -> Entry {
        Entry { value: Bytes::copy_from_slice(value), expires: ttl.map(|t| Instant::now() + t) }
    }

    fn wrote(&self, keys: &mut HashMap<String, Entry>) {
        if self.0.writes.fetch_add(1, Ordering::Relaxed) % SWEEP_EVERY == SWEEP_EVERY - 1 {
            let now = Instant::now();
            keys.retain(|_, e| e.live(now));
        }
    }

    pub async fn ping(&self) -> Result<()> {
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        let key = self.key(key);
        let mut keys = self.0.keys.lock();
        match keys.get(&key) {
            Some(e) if e.live(Instant::now()) => Ok(Some(e.value.clone())),
            Some(_) => {
                keys.remove(&key);
                Ok(None)
            }
            None => Ok(None),
        }
    }

    pub async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<()> {
        let mut keys = self.0.keys.lock();
        keys.insert(self.key(key), Self::entry(value, ttl));
        self.wrote(&mut keys);
        Ok(())
    }

    pub async fn set_if_absent(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<bool> {
        let key = self.key(key);
        let mut keys = self.0.keys.lock();
        if keys.get(&key).is_some_and(|e| e.live(Instant::now())) {
            return Ok(false);
        }
        keys.insert(key, Self::entry(value, ttl));
        self.wrote(&mut keys);
        Ok(true)
    }

    pub async fn delete(&self, key: &str) -> Result<bool> {
        let removed = self.0.keys.lock().remove(&self.key(key));
        Ok(removed.is_some_and(|e| e.live(Instant::now())))
    }

    pub async fn publish(&self, channel: &str, message: &[u8]) -> Result<usize> {
        let channel = self.key(channel);
        let mut channels = self.0.channels.lock();
        let Some(tx) = channels.get(&channel) else { return Ok(0) };
        match tx.send(Bytes::copy_from_slice(message)) {
            Ok(n) => Ok(n),
            Err(_) => {
                channels.remove(&channel);
                Ok(0)
            }
        }
    }

    pub async fn subscribe(&self, channel: &str) -> Result<Subscription> {
        let channel = self.key(channel);
        let rx = self
            .0
            .channels
            .lock()
            .entry(channel.clone())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .subscribe();
        Ok(Subscription::new(channel, rx))
    }
}
