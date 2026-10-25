//! The same checks on every backend: in-process always, Redis when `CRYSTAL_TEST_REDIS` names a
//! server (`redis://127.0.0.1:6379`, see `tools/dev.ps1`).

use std::time::Duration;

use super::*;

fn unique_prefix() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!("crystal-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

async fn backends() -> Vec<State> {
    let mut states = vec![State::memory(&unique_prefix())];
    match std::env::var("CRYSTAL_TEST_REDIS") {
        Ok(url) => {
            let config = StateConfig { redis_url: Some(url), prefix: unique_prefix() };
            states.push(State::connect(&config).await.unwrap());
        }
        Err(_) => eprintln!("CRYSTAL_TEST_REDIS not set: Redis skipped"),
    }
    states
}

#[tokio::test]
async fn keys() {
    for state in backends().await {
        let b = state.backend();
        state.ping().await.unwrap();
        assert_eq!(state.get("a").await.unwrap(), None, "{b}");
        state.set("a", b"one", None).await.unwrap();
        assert_eq!(state.get("a").await.unwrap().as_deref(), Some(&b"one"[..]), "{b}");

        assert!(!state.set_if_absent("a", b"two", None).await.unwrap(), "{b}");
        assert!(state.set_if_absent("b", b"two", None).await.unwrap(), "{b}");
        assert_eq!(state.get("b").await.unwrap().as_deref(), Some(&b"two"[..]), "{b}");

        assert!(state.delete("a").await.unwrap(), "{b}");
        assert!(!state.delete("a").await.unwrap(), "{b}");
        assert!(state.delete("b").await.unwrap(), "{b}");
    }
}

#[tokio::test]
async fn expiry() {
    for state in backends().await {
        let b = state.backend();
        state.set("short", b"x", Some(Duration::from_millis(50))).await.unwrap();
        assert!(state.get("short").await.unwrap().is_some(), "{b}");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(state.get("short").await.unwrap(), None, "{b}");
        // An expired key no longer blocks set_if_absent.
        assert!(state.set_if_absent("short", b"y", Some(Duration::from_secs(5))).await.unwrap(), "{b}");
        state.delete("short").await.unwrap();
    }
}

#[tokio::test]
async fn pubsub() {
    for state in backends().await {
        let b = state.backend();
        let mut first = state.subscribe("chat").await.unwrap();
        let mut second = state.subscribe("chat").await.unwrap();
        let mut other = state.subscribe("other").await.unwrap();

        state.publish("chat", b"hello").await.unwrap();
        assert_eq!(recv(&mut first).await.unwrap().as_deref(), Some(&b"hello"[..]), "{b}");
        assert_eq!(recv(&mut second).await.unwrap().as_deref(), Some(&b"hello"[..]), "{b}");
        assert!(recv(&mut other).await.is_err(), "{b}: no message on another channel");

        // A publish with no subscribers anywhere reaches nobody.
        assert_eq!(state.publish("nobody", b"x").await.unwrap(), 0, "{b}");
    }
}

async fn recv(s: &mut Subscription) -> std::result::Result<Option<Bytes>, tokio::time::error::Elapsed> {
    tokio::time::timeout(Duration::from_secs(2), s.recv()).await
}
