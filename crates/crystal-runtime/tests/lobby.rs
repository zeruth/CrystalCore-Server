//! A whole server (SQLite, in-process state, lobby on an ephemeral port) receives the β client's
//! captured lobby traffic and captures it again byte for byte.

use std::time::Duration;

use crystal_common::config::DatabaseEndpoint;
use crystal_common::{Config, Role};
use crystal_runtime::Server;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

const CAPTURE: &[u8] = include_bytes!("../../../protocol/672261/captures/lobby-encryption-init.bin");

#[tokio::test]
async fn lobby_receives_and_captures_client_frames() {
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name).to_string_lossy().replace('\\', "/");
    let mut config = Config::default();
    config.database.login = DatabaseEndpoint::sqlite(&path("var/login.db"));
    config.database.characters = DatabaseEndpoint::sqlite(&path("var/characters.db"));
    config.database.world = DatabaseEndpoint::sqlite(&path("var/world.db"));
    config.lobby.bind = "127.0.0.1:0".parse().unwrap();
    config.lobby.capture_dir = Some(dir.path().join("captures"));

    let server = Server::start(config).await.unwrap();
    let addr = server.local_addr(Role::Lobby).unwrap();

    let mut client = TcpStream::connect(addr).await.unwrap();
    // Split mid-frame, as TCP may.
    for piece in CAPTURE.chunks(500) {
        client.write_all(piece).await.unwrap();
    }
    client.flush().await.unwrap();

    let captured = async {
        loop {
            if let Some(entry) = std::fs::read_dir(dir.path().join("captures")).ok().and_then(|mut d| d.next()) {
                let bytes = std::fs::read(entry.unwrap().path()).unwrap();
                if bytes.len() >= CAPTURE.len() {
                    return bytes;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    let bytes = tokio::time::timeout(Duration::from_secs(10), captured).await.expect("capture written");
    assert_eq!(bytes, CAPTURE);

    // Shutdown ends the open connection instead of waiting for the client.
    tokio::time::timeout(Duration::from_secs(5), server.shutdown()).await.expect("shutdown");
}

#[tokio::test]
async fn refuses_roles_from_later_phases() {
    let config = Config { roles: vec![Role::Chat], ..Config::default() };
    assert!(Server::start(config).await.is_err());
}
