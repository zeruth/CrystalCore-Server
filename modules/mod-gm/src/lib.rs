//! GM chat commands, in AzerothCore's style. Their replies mark optional arguments with
//! parentheses: the client's chat log drops a line's text from its first `[` on. The levels are AzerothCore's: 1 moderator, 2 game
//! master, 3 administrator (`crystal-tools account set-gm`).
//!
//! `.bnpc` places battle NPCs by hand (Phase 4's third source, after imports and captures).
//! `.level` and `.exp` change the current class's progress; `.item` puts items in the bags.

use crystal_db::{BETA_PATCH, BnpcSpawn};
use crystal_world::BnpcPlacement;
use crystal_zone::commands::{Command, Commands, Context};
use futures_util::future::BoxFuture;

pub const MODERATOR: u8 = 1;
pub const GAME_MASTER: u8 = 2;
pub const ADMINISTRATOR: u8 = 3;

/// Adds this module's commands.
pub fn register(commands: &mut Commands) {
    commands.register(Gm);
    commands.register(Help);
    commands.register(Gps);
    commands.register(Who);
    commands.register(Tele);
    commands.register(Goto);
    commands.register(Bnpc);
    commands.register(Level);
    commands.register(Exp);
    commands.register(GiveItem);
}

/// `.item <id> (quantity)`: an item into the GM's bags.
struct GiveItem;

impl Command for GiveItem {
    fn name(&self) -> &'static str {
        "item"
    }
    fn usage(&self) -> &'static str {
        "<Item row> (quantity)"
    }
    fn help(&self) -> &'static str {
        "puts an item in your bags"
    }
    fn level(&self) -> u8 {
        GAME_MASTER
    }
    fn run<'a>(&'a self, cx: &'a mut Context<'_>, args: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let parts: Vec<&str> = args.split_whitespace().collect();
            let id = parts.first().and_then(|p| p.parse::<u32>().ok());
            let quantity = parts.get(1).map_or(Some(1), |q| q.parse::<u32>().ok()).filter(|q| *q > 0);
            match (id, quantity) {
                (Some(id), Some(q)) if id > 0 => cx.give_item(id, q),
                _ => cx.reply("Usage: .item <Item row> (quantity)"),
            }
        })
    }
}

/// `.level <1-50>`: the current class's level.
struct Level;

impl Command for Level {
    fn name(&self) -> &'static str {
        "level"
    }
    fn usage(&self) -> &'static str {
        "<1-50>"
    }
    fn help(&self) -> &'static str {
        "sets your level in your current class"
    }
    fn level(&self) -> u8 {
        GAME_MASTER
    }
    fn run<'a>(&'a self, cx: &'a mut Context<'_>, args: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            match args.trim().parse::<u8>() {
                Ok(l) if (1..=50).contains(&l) => {
                    cx.set_level(l);
                    cx.reply(format!("Level {l}."));
                }
                _ => cx.reply("Usage: .level <1-50>"),
            }
        })
    }
}

/// `.exp <amount>`: experience in the current class (levelling up as the table allows).
struct Exp;

impl Command for Exp {
    fn name(&self) -> &'static str {
        "exp"
    }
    fn usage(&self) -> &'static str {
        "<amount>"
    }
    fn help(&self) -> &'static str {
        "gives you experience in your current class"
    }
    fn level(&self) -> u8 {
        GAME_MASTER
    }
    fn run<'a>(&'a self, cx: &'a mut Context<'_>, args: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            match args.trim().parse::<u32>() {
                Ok(n) => {
                    cx.add_exp(n);
                    cx.reply(format!("{n} experience."));
                }
                Err(_) => cx.reply("Usage: .exp <amount>"),
            }
        })
    }
}

/// `.bnpc add <BNpcBase> <BNpcName> (level)`, `.bnpc near (yalms)`: battle NPCs placed by hand.
struct Bnpc;

/// The singular name of a `BNpcName` row, when the game data is there.
fn bnpc_name(cx: &Context<'_>, name_id: u32) -> String {
    cx.data
        .and_then(|d| d.sheet("BNpcName").ok())
        .and_then(|s| s.row(name_id).and_then(|r| r.first()).and_then(|v| v.as_str().map(str::to_owned)))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("#{name_id}"))
}

impl Command for Bnpc {
    fn name(&self) -> &'static str {
        "bnpc"
    }
    fn usage(&self) -> &'static str {
        "add <BNpcBase> <BNpcName> (level) | near (yalms)"
    }
    fn help(&self) -> &'static str {
        "places a battle NPC where you stand (saved as source hand), or lists those near you"
    }
    fn level(&self) -> u8 {
        GAME_MASTER
    }
    fn run<'a>(&'a self, cx: &'a mut Context<'_>, args: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let mut parts: Vec<&str> = args.split_whitespace().collect();
            // `.bnpc 49 49 3` is `.bnpc add 49 49 3`
            if parts.first().is_some_and(|p| p.parse::<u32>().is_ok()) {
                parts.insert(0, "add");
            }
            match parts.as_slice() {
                ["add", base, name, rest @ ..] => {
                    let (Ok(base_id), Ok(name_id)) = (base.parse::<u32>(), name.parse::<u32>()) else {
                        cx.reply("Usage: .bnpc add <BNpcBase> <BNpcName> (level)");
                        return;
                    };
                    let level = rest.first().and_then(|l| l.parse::<u16>().ok()).unwrap_or(1);
                    let p = cx.player.clone();
                    if let Some(data) = cx.data {
                        match data.bnpc_misfit(p.territory, base_id, name_id, "") {
                            Ok(Some(why)) => {
                                cx.reply(format!("Not placed: {why}."));
                                return;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                cx.reply(format!("Not placed: {e}"));
                                return;
                            }
                        }
                    }
                    let spawn = BnpcSpawn {
                        territory: p.territory,
                        base_id,
                        name_id,
                        level,
                        pos: p.pos,
                        rotation: p.rotation,
                        source: "hand".into(),
                        ..Default::default()
                    };
                    if let Err(e) = cx.db.world.add_bnpc_spawn(BETA_PATCH, &spawn).await {
                        cx.reply(format!("Not saved: {e}"));
                        return;
                    }
                    let placement =
                        BnpcPlacement { base_id, name_id, level, pos: p.pos, rotation: p.rotation, aggressive: false };
                    let actor = cx.world.zone(p.territory).add_bnpc(placement).await;
                    let name = bnpc_name(cx, name_id);
                    cx.reply(format!(
                        "Placed {name} (base {base_id}, level {level}) at {:.1} {:.1} {:.1}{}.",
                        p.pos[0],
                        p.pos[1],
                        p.pos[2],
                        actor.map(|a| format!(", actor {a:#x}")).unwrap_or_default()
                    ));
                }
                ["near", rest @ ..] | rest @ [] if rest.len() <= 1 => {
                    let radius = rest.first().and_then(|r| r.parse::<f32>().ok()).unwrap_or(30.0);
                    let p = cx.player.clone();
                    let spawns = match cx.db.world.bnpc_spawns(BETA_PATCH, Some(p.territory)).await {
                        Ok(s) => s,
                        Err(e) => {
                            cx.reply(format!("{e}"));
                            return;
                        }
                    };
                    let d = |s: &BnpcSpawn| {
                        ((s.pos[0] - p.pos[0]).powi(2) + (s.pos[1] - p.pos[1]).powi(2) + (s.pos[2] - p.pos[2]).powi(2))
                            .sqrt()
                    };
                    let mut near: Vec<(f32, &BnpcSpawn)> =
                        spawns.iter().map(|s| (d(s), s)).filter(|(d, _)| *d <= radius).collect();
                    near.sort_by(|a, b| a.0.total_cmp(&b.0));
                    cx.reply(format!("{} battle NPCs within {radius} yalms:", near.len()));
                    let lines: Vec<String> = near
                        .iter()
                        .take(15)
                        .map(|(dist, s)| {
                            format!(
                                "{:.1}y  {} (base {}, level {})  {}  ({})",
                                dist,
                                bnpc_name(cx, s.name_id),
                                s.base_id,
                                s.level,
                                if s.layer.is_empty() { "-" } else { &s.layer },
                                s.source
                            )
                        })
                        .collect();
                    for l in lines {
                        cx.reply(l);
                    }
                }
                _ => cx.reply("Usage: .bnpc add <BNpcBase> <BNpcName> (level) | .bnpc near (yalms)"),
            }
        })
    }
}

/// `.gm (on|off)`
struct Gm;

impl Command for Gm {
    fn name(&self) -> &'static str {
        "gm"
    }
    fn usage(&self) -> &'static str {
        "(on|off)"
    }
    fn help(&self) -> &'static str {
        "switches GM mode, or tells whether it is on"
    }
    fn level(&self) -> u8 {
        MODERATOR
    }
    fn run<'a>(&'a self, cx: &'a mut Context<'_>, args: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let on = match args.to_ascii_lowercase().as_str() {
                "on" => true,
                "off" => false,
                "" => {
                    let state = if cx.player.gm_mode { "on" } else { "off" };
                    cx.reply(format!("GM mode is {state} (GM level {}).", cx.player.gm_level));
                    return;
                }
                _ => {
                    cx.reply("Usage: .gm (on|off)");
                    return;
                }
            };
            cx.set_gm_mode(on);
            cx.reply(if on { "GM mode is on." } else { "GM mode is off." });
        })
    }
}

/// `.help`
struct Help;

impl Command for Help {
    fn name(&self) -> &'static str {
        "help"
    }
    fn help(&self) -> &'static str {
        "lists the commands you can use"
    }
    fn level(&self) -> u8 {
        MODERATOR
    }
    fn run<'a>(&'a self, cx: &'a mut Context<'_>, _args: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let level = cx.player.gm_level;
            let lines: Vec<String> = cx
                .commands
                .list()
                .iter()
                .filter(|c| c.level() <= level)
                .map(|c| {
                    let usage = if c.usage().is_empty() { String::new() } else { format!(" {}", c.usage()) };
                    format!(".{}{usage}: {}", c.name(), c.help())
                })
                .collect();
            for line in lines {
                cx.reply(line);
            }
        })
    }
}

/// `.gps`
struct Gps;

impl Command for Gps {
    fn name(&self) -> &'static str {
        "gps"
    }
    fn help(&self) -> &'static str {
        "where you stand"
    }
    fn level(&self) -> u8 {
        MODERATOR
    }
    fn run<'a>(&'a self, cx: &'a mut Context<'_>, _args: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let p = &cx.player;
            let line = format!(
                "Territory {} at {:.2} {:.2} {:.2}, facing {:.2}",
                p.territory, p.pos[0], p.pos[1], p.pos[2], p.rotation
            );
            cx.reply(line);
        })
    }
}

/// `.who`
struct Who;

impl Command for Who {
    fn name(&self) -> &'static str {
        "who"
    }
    fn help(&self) -> &'static str {
        "who is online, and in which territory"
    }
    fn level(&self) -> u8 {
        MODERATOR
    }
    fn run<'a>(&'a self, cx: &'a mut Context<'_>, _args: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let online = cx.world.online();
            let names: Vec<String> = online.iter().take(50).map(|p| format!("{} ({})", p.name, p.territory)).collect();
            let more = if online.len() > names.len() {
                format!(", and {} more", online.len() - names.len())
            } else {
                String::new()
            };
            cx.reply(format!("{} online: {}{more}", online.len(), names.join(", ")));
        })
    }
}

/// `.tele <territory> (x y z)`
struct Tele;

impl Command for Tele {
    fn name(&self) -> &'static str {
        "tele"
    }
    fn usage(&self) -> &'static str {
        "<territory> (x y z)"
    }
    fn help(&self) -> &'static str {
        "goes to a territory (TerritoryType row), at a point or where its exits arrive"
    }
    fn level(&self) -> u8 {
        GAME_MASTER
    }
    fn run<'a>(&'a self, cx: &'a mut Context<'_>, args: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let parts: Vec<&str> = args.split_whitespace().collect();
            let Some(territory) = parts.first().and_then(|t| t.parse::<u16>().ok()) else {
                cx.reply("Usage: .tele <territory> (x y z)");
                return;
            };
            if let Some(data) = cx.data
                && !data.has_territory(territory)
            {
                cx.reply(format!("No territory {territory} in the game data."));
                return;
            }
            match parts.len() {
                1 => {
                    let Some(data) = cx.data else {
                        cx.reply("No game data configured: give coordinates (.tele <territory> x y z).");
                        return;
                    };
                    match data.arrival(territory) {
                        Ok(a) => cx.teleport(territory, a.pos, a.rotation),
                        Err(e) => cx.reply(format!("Nowhere to stand in {territory}: {e}. Give coordinates.")),
                    }
                }
                4 => {
                    let xyz: Option<Vec<f32>> = parts[1..].iter().map(|v| v.parse().ok()).collect();
                    match xyz {
                        Some(v) => cx.teleport(territory, [v[0], v[1], v[2]], cx.player.rotation),
                        None => cx.reply("Usage: .tele <territory> (x y z)"),
                    }
                }
                _ => cx.reply("Usage: .tele <territory> (x y z)"),
            }
        })
    }
}

/// `.goto <name>`
struct Goto;

impl Command for Goto {
    fn name(&self) -> &'static str {
        "goto"
    }
    fn usage(&self) -> &'static str {
        "<character name>"
    }
    fn help(&self) -> &'static str {
        "goes to where an online character stands"
    }
    fn level(&self) -> u8 {
        GAME_MASTER
    }
    fn run<'a>(&'a self, cx: &'a mut Context<'_>, args: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Some(target) = cx.world.find(args) else {
                cx.reply(format!("No one called {args} is online."));
                return;
            };
            if target.actor == cx.player.actor {
                cx.reply("That is you.");
                return;
            }
            match cx.world.locate(target.actor).await {
                Some((territory, pos, rotation)) => cx.teleport(territory, pos, rotation),
                None => cx.reply(format!("{} is between territories; try again.", target.name)),
            }
        })
    }
}
