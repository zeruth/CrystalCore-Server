//! What can be typed at the console. Setting GM levels is here and only here: an account's level
//! is changed by whoever runs the server, never from inside the game (in game, a GM can only
//! switch its own GM mode).

use std::net::SocketAddr;
use std::time::Instant;

use anyhow::{Context, bail};
use crystal_common::Role;
use crystal_runtime::Server;

use crate::logs::Logging;

pub struct Spec {
    pub name: &'static str,
    pub usage: &'static str,
    pub help: &'static str,
}

pub const COMMANDS: &[Spec] = &[
    Spec { name: "help", usage: "", help: "this list" },
    Spec { name: "status", usage: "", help: "services, players online, uptime" },
    Spec { name: "who", usage: "", help: "who is online, and in which territory" },
    Spec {
        name: "gm",
        usage: "<character> <level>",
        help: "sets the GM level of the character's account: 0 player, 1 moderator, 2 game master, 3 administrator",
    },
    Spec { name: "gm-account", usage: "<username> <level>", help: "sets an account's GM level by its username" },
    Spec { name: "level", usage: "<error|warn|info|debug|trace>", help: "sets the log level (F2 steps through them)" },
    Spec { name: "log", usage: "[filter]", help: "shows or sets the full log filter (info,crystal_zone=debug, ...)" },
    Spec {
        name: "client",
        usage: "",
        help: "starts the client from [data] game, pointed at this server's lobby (also: launch)",
    },
    Spec { name: "clear", usage: "", help: "clears the log view" },
    Spec { name: "quit", usage: "", help: "stops the server (also exit, shutdown, or Ctrl+C twice)" },
];

/// The highest GM level (AzerothCore's administrator).
pub const MAX_GM_LEVEL: u8 = 3;

pub enum Outcome {
    Continue,
    Clear,
    Quit,
}

/// Command names that start with `prefix`.
pub fn complete(prefix: &str) -> Vec<&'static str> {
    COMMANDS.iter().map(|c| c.name).filter(|n| n.starts_with(prefix)).collect()
}

/// Runs one line; the lines to show, or an error to show.
pub async fn run(
    line: &str,
    server: &Server,
    logging: &Logging,
    started: Instant,
) -> anyhow::Result<(Vec<String>, Outcome)> {
    let line = line.trim().trim_start_matches(['/', '.']);
    let (name, args) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let args = args.trim();
    let mut out = Vec::new();
    match name.to_ascii_lowercase().as_str() {
        "" => {}
        "help" | "?" => {
            for c in COMMANDS {
                let usage = if c.usage.is_empty() { String::new() } else { format!(" {}", c.usage) };
                out.push(format!("{}{usage}  —  {}", c.name, c.help));
            }
        }
        "status" => {
            let secs = started.elapsed().as_secs();
            out.push(format!("up {:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60));
            for role in [Role::Lobby, Role::Zone, Role::Chat] {
                if let Some(addr) = server.local_addr(role) {
                    out.push(format!("{role:?} on {addr}"));
                }
            }
            match server.world() {
                Some(w) => out.push(format!("{} online", w.online().len())),
                None => out.push("no zone service in this process".into()),
            }
        }
        "who" => {
            let world = server.world().context("no zone service in this process")?;
            let online = world.online();
            if online.is_empty() {
                out.push("no one online".into());
            }
            for p in online {
                out.push(format!("{}  territory {}  actor {:#x}", p.name, p.territory, p.actor));
            }
        }
        "gm" => {
            let (name, level) = name_and_level(args, "gm <character> <level>")?;
            let chara = server
                .db
                .characters
                .character_by_name(name)
                .await?
                .with_context(|| format!("no character named {name}"))?;
            if !server.db.login.set_gm_level_of(chara.account_id, level).await? {
                bail!("{}'s account ({}) is not in the login database", chara.name, chara.account_id);
            }
            let account = server.db.login.account_name(chara.account_id).await?.unwrap_or_default();
            tracing::info!(character = %chara.name, account = %account, level, "gm level set from the console");
            out.push(format!("{}'s account {account} is now GM level {level} ({}).", chara.name, level_name(level)));
            out.push("It applies to the next command they type; .gm on in game switches GM mode on.".into());
        }
        "gm-account" => {
            let (username, level) = name_and_level(args, "gm-account <username> <level>")?;
            if !server.db.login.set_gm_level(username, level).await? {
                bail!("no account named {username}");
            }
            tracing::info!(account = %username, level, "gm level set from the console");
            out.push(format!("Account {username} is now GM level {level} ({}).", level_name(level)));
        }
        "level" => {
            let level = args.to_ascii_lowercase();
            if !["error", "warn", "info", "debug", "trace"].contains(&level.as_str()) {
                bail!("usage: level <error|warn|info|debug|trace>");
            }
            logging.set_filter(&level)?;
            out.push(format!("log level: {level}"));
        }
        "log" => {
            if args.is_empty() {
                out.push(format!("log filter: {}", logging.filter()));
            } else {
                logging.set_filter(args)?;
                out.push(format!("log filter is now {args}"));
            }
        }
        "client" | "launch" => out.push(launch_client(server)?),
        "clear" | "cls" => return Ok((out, Outcome::Clear)),
        "quit" | "exit" | "shutdown" | "stop" => return Ok((out, Outcome::Quit)),
        other => bail!("no command {other}; help lists them"),
    }
    Ok((out, Outcome::Continue))
}

/// The β client's version, as its launcher passes it.
const CLIENT_VERSION: &str = "2013.08.09.0001.0000";

/// The client's command line for lobby hosts at `hosts` (01, then 02; one serves both), as the
/// launcher would give it: the test ticket, SqPack data, and this server's lobby addresses.
pub fn client_args(hosts: &[SocketAddr]) -> Vec<String> {
    // a server listening on every interface is reached locally
    let reach = |a: SocketAddr| -> SocketAddr {
        if a.ip().is_unspecified() { SocketAddr::from(([127, 0, 0, 1], a.port())) } else { a }
    };
    let one = reach(hosts[0]);
    let two = reach(*hosts.get(1).unwrap_or(&hosts[0]));
    [
        "DEV.TestSID=0".to_owned(),
        "DEV.UseSqPack=1".to_owned(),
        "DEV.DataPathType=1".to_owned(),
        format!("DEV.LobbyHost01={}", one.ip()),
        format!("DEV.LobbyPort01={}", one.port()),
        format!("DEV.LobbyHost02={}", two.ip()),
        format!("DEV.LobbyPort02={}", two.port()),
        "SYS.Region=3".to_owned(),
        "language=1".to_owned(),
        format!("ver={CLIENT_VERSION}"),
    ]
    .into()
}

fn launch_client(server: &Server) -> anyhow::Result<String> {
    let game = server.game.as_ref().context("no client configured: set [data] game to the install's game folder")?;
    let hosts = server.lobby_hosts();
    if hosts.is_empty() {
        bail!("no lobby in this process to point the client at");
    }
    let exe = game.join("ffxiv.exe");
    if !exe.exists() {
        bail!("no ffxiv.exe in {}", game.display());
    }
    let args = client_args(hosts);
    let child = std::process::Command::new(&exe)
        .args(&args)
        .current_dir(game)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;
    tracing::info!(pid = child.id(), exe = %exe.display(), args = args.join(" "), "client started");
    Ok(format!("Client started (pid {}), lobby {} / {}.", child.id(), &args[4][16..], &args[6][16..]))
}

/// `Null Crystal 3` → ("Null Crystal", 3): the level is the last word.
fn name_and_level<'a>(args: &'a str, usage: &str) -> anyhow::Result<(&'a str, u8)> {
    let (name, level) = args.rsplit_once(char::is_whitespace).with_context(|| format!("usage: {usage}"))?;
    let level: u8 = level.parse().ok().filter(|l| *l <= MAX_GM_LEVEL).with_context(|| {
        format!("the level is 0 to {MAX_GM_LEVEL} (0 player, 1 moderator, 2 game master, 3 administrator)")
    })?;
    let name = name.trim();
    if name.is_empty() {
        bail!("usage: {usage}");
    }
    Ok((name, level))
}

fn level_name(level: u8) -> &'static str {
    match level {
        0 => "player",
        1 => "moderator",
        2 => "game master",
        _ => "administrator",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_with_spaces_and_levels() {
        assert_eq!(name_and_level("Null Crystal 3", "").unwrap(), ("Null Crystal", 3));
        assert!(name_and_level("Null Crystal 4", "").is_err());
        assert!(name_and_level("Null", "").is_err());
        assert!(name_and_level(" 2", "").is_err());
    }

    #[test]
    fn client_points_at_the_lobby() {
        let two: Vec<SocketAddr> = vec!["127.0.0.1:54994".parse().unwrap(), "0.0.0.0:54995".parse().unwrap()];
        let args = client_args(&two);
        assert!(args.contains(&"DEV.LobbyPort01=54994".into()));
        assert!(args.contains(&"DEV.LobbyHost02=127.0.0.1".into()), "unspecified becomes local");
        assert!(args.contains(&"DEV.LobbyPort02=54995".into()));
        let one = client_args(&two[..1]);
        assert!(one.contains(&"DEV.LobbyPort02=54994".into()), "one port serves both hosts");
        assert_eq!(&args[4][16..], "54994");
    }

    #[test]
    fn completes_names() {
        assert_eq!(complete("gm"), ["gm", "gm-account"]);
        assert_eq!(complete("q"), ["quit"]);
        assert_eq!(complete("cli"), ["client"]);
    }
}
