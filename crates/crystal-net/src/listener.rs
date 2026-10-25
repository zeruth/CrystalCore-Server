//! Accepts TCP connections and runs a task per connection until shutdown.

use std::future::Future;
use std::io;
use std::net::SocketAddr;

use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{Instrument, debug, info, info_span, warn};

/// A bound listener. Binding is separate from serving so callers learn the address (port 0 binds
/// an ephemeral port) before connections arrive.
pub struct Listener {
    name: &'static str,
    inner: TcpListener,
}

impl Listener {
    pub async fn bind(name: &'static str, addr: SocketAddr) -> io::Result<Self> {
        let inner = TcpListener::bind(addr).await?;
        info!(service = name, addr = %inner.local_addr()?, "listening");
        Ok(Self { name, inner })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}

/// Runs `handle` for each accepted connection until `shutdown` is cancelled, then waits for the
/// connection tasks, which see the same token, to finish.
pub async fn serve<H, F>(listener: Listener, shutdown: CancellationToken, handle: H)
where
    H: Fn(TcpStream, SocketAddr, CancellationToken) -> F,
    F: Future<Output = ()> + Send + 'static,
{
    let tasks = TaskTracker::new();
    loop {
        let accepted = tokio::select! {
            _ = shutdown.cancelled() => break,
            accepted = listener.inner.accept() => accepted,
        };
        match accepted {
            Ok((stream, peer)) => {
                if let Err(e) = stream.set_nodelay(true) {
                    debug!(%peer, "set_nodelay: {e}");
                }
                let span = info_span!("conn", service = listener.name, %peer);
                tasks.spawn(handle(stream, peer, shutdown.child_token()).instrument(span));
            }
            // Per-connection failures (reset before accept, fd exhaustion) must not stop the listener.
            Err(e) => {
                warn!(service = listener.name, "accept: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
    tasks.close();
    tasks.wait().await;
    info!(service = listener.name, "stopped");
}
