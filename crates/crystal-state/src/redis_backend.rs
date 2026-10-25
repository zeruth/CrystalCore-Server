//! The Redis backend (Redis 7+ or Valkey), over RESP3.
//!
//! Two multiplexed connections per process, however many tasks use them: one for commands (requests
//! from all tasks are pipelined onto it), one for subscriptions. Both reconnect on their own and the
//! subscription connection re-subscribes after a reconnect. Incoming messages are fanned out to
//! local subscribers through one `broadcast` channel per Redis channel, so a thousand zone tasks
//! listening on one channel cost Redis one subscription.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use parking_lot::Mutex;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{Client, PushInfo, PushKind, Value};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, warn};

use crate::{CHANNEL_CAPACITY, Result, Subscription};

type Channels = Arc<Mutex<HashMap<String, broadcast::Sender<Bytes>>>>;

#[derive(Clone)]
pub struct RedisState(Arc<Inner>);

struct Inner {
    prefix: String,
    commands: ConnectionManager,
    subscriber: ConnectionManager,
    channels: Channels,
}

impl RedisState {
    pub async fn connect(url: &str, prefix: &str) -> Result<Self> {
        let client = Client::open(with_resp3(url))?;
        let base = || {
            ConnectionManagerConfig::new()
                .set_connection_timeout(Some(Duration::from_secs(5)))
                .set_response_timeout(Some(Duration::from_secs(5)))
                .set_max_delay(Duration::from_secs(5))
        };
        let commands = client.get_connection_manager_with_config(base()).await?;

        let (push_tx, push_rx) = mpsc::unbounded_channel();
        let subscriber = client
            .get_connection_manager_with_config(base().set_push_sender(push_tx).set_automatic_resubscription())
            .await?;

        let channels = Channels::default();
        tokio::spawn(fan_out(push_rx, channels.clone()));
        Ok(Self(Arc::new(Inner { prefix: prefix.to_owned(), commands, subscriber, channels })))
    }

    pub fn backend(&self) -> &'static str {
        "redis"
    }

    fn key(&self, key: &str) -> String {
        format!("{}:{key}", self.0.prefix)
    }

    fn commands(&self) -> ConnectionManager {
        self.0.commands.clone()
    }

    pub async fn ping(&self) -> Result<()> {
        Ok(redis::cmd("PING").query_async::<()>(&mut self.commands()).await?)
    }

    pub async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        let value: Option<Vec<u8>> = redis::cmd("GET").arg(self.key(key)).query_async(&mut self.commands()).await?;
        Ok(value.map(Bytes::from))
    }

    fn set_cmd(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> redis::Cmd {
        let mut cmd = redis::cmd("SET");
        cmd.arg(self.key(key)).arg(value);
        if let Some(ttl) = ttl {
            cmd.arg("PX").arg(ttl.as_millis().max(1) as u64);
        }
        cmd
    }

    pub async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<()> {
        Ok(self.set_cmd(key, value, ttl).query_async::<()>(&mut self.commands()).await?)
    }

    pub async fn set_if_absent(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> Result<bool> {
        let reply: Option<String> = self.set_cmd(key, value, ttl).arg("NX").query_async(&mut self.commands()).await?;
        Ok(reply.is_some())
    }

    pub async fn delete(&self, key: &str) -> Result<bool> {
        let n: u64 = redis::cmd("DEL").arg(self.key(key)).query_async(&mut self.commands()).await?;
        Ok(n > 0)
    }

    pub async fn publish(&self, channel: &str, message: &[u8]) -> Result<usize> {
        let n: u64 =
            redis::cmd("PUBLISH").arg(self.key(channel)).arg(message).query_async(&mut self.commands()).await?;
        Ok(n as usize)
    }

    pub async fn subscribe(&self, channel: &str) -> Result<Subscription> {
        let channel = self.key(channel);
        let (rx, new) = {
            let mut channels = self.0.channels.lock();
            match channels.get(&channel) {
                Some(tx) => (tx.subscribe(), false),
                None => {
                    let (tx, rx) = broadcast::channel(CHANNEL_CAPACITY);
                    channels.insert(channel.clone(), tx);
                    (rx, true)
                }
            }
        };
        if new && let Err(e) = self.0.subscriber.clone().subscribe(&channel).await {
            self.0.channels.lock().remove(&channel);
            return Err(e.into());
        }
        Ok(Subscription::new(channel, rx))
    }
}

/// Hands each pushed message to the local subscribers of its channel.
async fn fan_out(mut pushes: mpsc::UnboundedReceiver<PushInfo>, channels: Channels) {
    while let Some(push) = pushes.recv().await {
        if push.kind != PushKind::Message {
            debug!(kind = ?push.kind, "redis push");
            continue;
        }
        let [Value::BulkString(channel), Value::BulkString(payload)] = &push.data[..] else {
            warn!(data = ?push.data, "unexpected redis message shape");
            continue;
        };
        let Ok(channel) = std::str::from_utf8(channel) else { continue };
        let mut map = channels.lock();
        if let Some(tx) = map.get(channel)
            && tx.send(Bytes::copy_from_slice(payload)).is_err()
        {
            // Nobody listens locally any more. The Redis subscription is left in place (a later
            // subscribe re-issues SUBSCRIBE, which is idempotent); unsubscribing here would race it.
            map.remove(channel);
        }
    }
}

/// The subscription connection needs RESP3; ask for it unless the URL already says.
fn with_resp3(url: &str) -> String {
    if url.contains("protocol=") {
        url.to_owned()
    } else if url.contains('?') {
        format!("{url}&protocol=resp3")
    } else {
        format!("{url}?protocol=resp3")
    }
}
