//! MCP (Model Context Protocol) server on stdin/stdout: newline-delimited JSON-RPC 2.0. It keeps
//! one bot connected between calls and exposes it as tools (chat, commands, movement, menus,
//! inventory, entities, blocks). Logs go to stderr; stdout carries only protocol messages.

use crate::engine::{Bot, ChatLine, Cmd, Config, End, Game, INVENTORY_SLOTS, Log, Walk};
use crate::item::ItemStack;
use crate::text::Lang;
use serde_json::{Map, Value, json};
use std::io::{self, BufRead, Write};
use std::sync::Arc;
use std::time::Duration;

const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "A headless Minecraft: Java Edition bot (offline mode) you control in a running server. \
Use `connect` first (or rely on the configured defaults), then act: `command` (without the leading /), `chat`, \
`walk_to`, `look`, `use_item` / `use_block` / `interact` (right clicks; they report a menu if one opens), `menu` + \
`click_slot` for chest GUIs, `inventory`, `entities`, `find_blocks`. Action tools return the chat lines that \
arrived while they waited (`wait_ms`). Coordinates are block coordinates; y is the feet height.";

/// Defaults from the command line, used by `connect` (and to connect automatically).
#[derive(Clone, Debug, Default)]
pub struct Defaults {
    pub host: Option<String>,
    pub port: u16,
    pub username: Option<String>,
    pub protocol: Option<i32>,
    pub view_distance: i8,
    pub join_timeout: Duration,
    pub verbose: bool,
}

struct Server {
    defaults: Defaults,
    lang: Arc<Lang>,
    log: Log,
    bot: Option<Bot>,
    /// `read_chat`'s cursor.
    chat_read: u64,
}

pub fn serve(defaults: Defaults, lang: Arc<Lang>) -> io::Result<()> {
    let log = Log::new(defaults.verbose);
    log.info("MCP server ready on stdio");
    let mut server = Server { defaults, lang, log, bot: None, chat_read: 0 };
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> = batch.into_iter().filter_map(|m| server.message(m)).collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            Ok(msg) => server.message(msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}})),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    if let Some(bot) = server.bot.take() {
        bot.disconnect();
    }
    Ok(())
}

impl Server {
    /// One JSON-RPC message: a reply for requests, None for notifications.
    fn message(&mut self, msg: Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        id.as_ref()?; // notifications (initialized, cancelled, ...) need no answer
        let result = match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
                let version = SUPPORTED_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&SUPPORTED_VERSIONS[0]);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "mcclient", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                self.log.debug(format!("tool {name} {args}"));
                let (text, is_error) = match self.call(&name, &args) {
                    Ok(v) => (if let Value::String(s) = v { s } else { serde_json::to_string_pretty(&v).unwrap_or_default() }, false),
                    Err(e) => (e, true),
                };
                Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
            }
            "resources/list" => Ok(json!({"resources": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            _ => Err(json!({"code": -32601, "message": format!("method not found: {method}")})),
        };
        Some(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        })
    }

    fn connect(&mut self, args: &Value) -> Result<Value, String> {
        let d = self.defaults.clone();
        let (host, port) = match args.get("host").and_then(Value::as_str) {
            Some(addr) => crate::cli::parse_address(addr)?,
            None => (d.host.clone().ok_or("no host: pass `host` (or start the server with --server)")?, d.port),
        };
        let port = args.get("port").and_then(Value::as_u64).map_or(port, |p| p as u16);
        let username = match args.get("username").and_then(Value::as_str) {
            Some(u) => u.to_string(),
            None => d.username.clone().ok_or("no username: pass `username` (or start the server with --name)")?,
        };
        crate::cli::check_username(&username)?;
        let protocol = args.get("protocol").and_then(Value::as_i64).map(|p| p as i32).or(d.protocol);
        if let Some(old) = self.bot.take() {
            old.disconnect();
        }
        let cfg = Config {
            host: host.clone(),
            port,
            username: username.clone(),
            protocol,
            view_distance: d.view_distance,
            join_timeout: d.join_timeout,
            verbose: d.verbose,
        };
        let bot = Bot::start(cfg, self.lang.clone(), self.log.clone())?;
        let joined = bot.wait_ready(d.join_timeout + Duration::from_secs(5));
        let chat: Vec<String> = bot.game().chat_since(0).iter().map(ChatLine::display).collect();
        self.chat_read = bot.game().chat_seq;
        match joined {
            Ok(()) => {
                // A moment for the inventory, players and nearby chunks to arrive.
                let g = bot.wait_until(Duration::from_millis(700), |_| false);
                let status = status_json(&g);
                drop(g);
                self.bot = Some(bot);
                Ok(json!({"joined": format!("{username} on {host}:{port}"), "status": status, "chat": chat}))
            }
            Err(e) => Err(format!("could not join {host}:{port} as {username}: {e}\nchat: {chat:?}")),
        }
    }

    /// The connected bot, connecting with the defaults if there is none yet.
    fn bot(&mut self) -> Result<&Bot, String> {
        let alive = self.bot.as_ref().is_some_and(|b| b.game().ended.is_none());
        if !alive {
            if let Some(b) = &self.bot
                && let Some(End::NotJoined(m) | End::Left(m)) = &b.game().ended
            {
                return Err(format!("disconnected ({m}); call `connect` to join again"));
            }
            if self.defaults.host.is_some() && self.defaults.username.is_some() {
                self.connect(&json!({}))?;
            } else {
                return Err("not connected: call `connect` first".into());
            }
        }
        let bot = self.bot.as_ref().unwrap();
        bot.wait_ready(Duration::from_secs(10))?;
        Ok(bot)
    }

    /// Send a command, wait `wait_ms`, report the chat (and menu changes) that followed.
    fn act(&mut self, cmd: Cmd, args: &Value, default_wait: u64) -> Result<Value, String> {
        let wait = Duration::from_millis(arg_u64(args, "wait_ms").unwrap_or(default_wait).min(30_000));
        let bot = self.bot()?;
        let (seq, menu_before) = {
            let g = bot.game();
            (g.chat_seq, menu_signature(&g))
        };
        bot.send(cmd)?;
        let g = bot.wait_until(wait, |g| g.ended.is_some());
        Ok(after(&g, seq, menu_before))
    }

    fn call(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        match name {
            "connect" => self.connect(args),
            "disconnect" => {
                let bot = self.bot.take().ok_or("not connected")?;
                bot.disconnect();
                Ok("disconnected".into())
            }
            "status" => {
                let bot = self.bot()?;
                Ok(status_json(&bot.game()))
            }
            "command" => {
                let c = arg_str(args, "command")?;
                let c = c.trim().trim_start_matches('/').to_string();
                check_chat_chars(&c)?;
                self.act(Cmd::Command(c), args, 800)
            }
            "chat" => {
                let m = arg_str(args, "message")?.to_string();
                check_chat_chars(&m)?;
                if m.chars().count() > 256 {
                    return Err("chat messages are limited to 256 characters".into());
                }
                self.act(Cmd::Chat(m), args, 600)
            }
            "read_chat" => {
                let since = arg_u64(args, "since").unwrap_or(self.chat_read);
                let limit = arg_u64(args, "limit").unwrap_or(100) as usize;
                let bot = self.bot()?;
                let g = bot.game();
                let lines = g.chat_since(since);
                let skipped = lines.len().saturating_sub(limit);
                let lines: Vec<Value> = lines[skipped..].iter().map(line_json).collect();
                let next = g.chat_seq;
                drop(g);
                self.chat_read = next;
                Ok(json!({"lines": lines, "omitted_older": skipped, "next_since": next}))
            }
            "wait" => {
                let ms = arg_u64(args, "ms").unwrap_or(1000).min(60_000);
                let until = args.get("until_chat").and_then(Value::as_str).map(str::to_lowercase);
                let bot = self.bot()?;
                let seq = bot.game().chat_seq;
                let g = bot.wait_until(Duration::from_millis(ms), |g| {
                    g.ended.is_some()
                        || until.as_ref().is_some_and(|u| g.chat_since(seq).iter().any(|l| l.text.to_lowercase().contains(u)))
                });
                let lines: Vec<String> = g.chat_since(seq).iter().map(ChatLine::display).collect();
                Ok(json!({"chat": lines, "ended": ended_text(&g)}))
            }
            "look" => {
                let cmd = match (arg_f64(args, "x"), arg_f64(args, "y"), arg_f64(args, "z")) {
                    (Some(x), Some(y), Some(z)) => Cmd::LookAt([x, y, z]),
                    _ => Cmd::Look {
                        yaw: arg_f64(args, "yaw").ok_or("pass yaw/pitch or x/y/z")? as f32,
                        pitch: arg_f64(args, "pitch").unwrap_or(0.0) as f32,
                    },
                };
                self.act(cmd, args, 100)
            }
            "walk_to" => self.walk_to(args),
            "stop" => self.act(Cmd::Stop, args, 50),
            "jump" => self.act(Cmd::Jump, args, 600),
            "sneak" => self.act(Cmd::Sneak(args.get("on").and_then(Value::as_bool).unwrap_or(true)), args, 100),
            "select_slot" => {
                let slot = arg_u64(args, "slot").ok_or("slot (0-8) is required")?;
                if slot > 8 {
                    return Err("slot must be 0-8 (the hotbar)".into());
                }
                self.act(Cmd::SelectSlot(slot as usize), args, 150)
            }
            "use_item" => self.act(Cmd::UseItem { offhand: arg_bool(args, "offhand") }, args, 800),
            "use_block" => {
                let pos = arg_pos(args)?;
                let face = face(args)?;
                self.act(Cmd::UseBlock { pos, face, offhand: arg_bool(args, "offhand") }, args, 800)
            }
            "dig" => self.dig(args),
            "attack" => {
                let id = self.entity_arg(args)?;
                self.act(Cmd::Attack(id), args, 400)
            }
            "interact" => {
                let id = self.entity_arg(args)?;
                self.act(Cmd::Interact { id, offhand: arg_bool(args, "offhand") }, args, 800)
            }
            "inventory" => {
                let bot = self.bot()?;
                let g = bot.game();
                let slots: Vec<Value> = g
                    .inventory
                    .iter()
                    .enumerate()
                    .filter_map(|(i, s)| s.as_ref().map(|it| slot_json(i, it, inventory_slot_name(i))))
                    .collect();
                Ok(json!({
                    "held_slot": g.held,
                    "held_item": g.held_item().map(ItemStack::to_json),
                    "cursor": g.cursor.as_ref().map(ItemStack::to_json),
                    "slots": slots,
                    "layout": "0 craft result, 1-4 craft grid, 5-8 armor head..feet, 9-35 main, 36-44 hotbar (hotbar slot n = 36+n), 45 offhand",
                    "item_decode_errors": g.item_errors,
                }))
            }
            "menu" => {
                let bot = self.bot()?;
                let g = bot.game();
                Ok(menu_json(&g, true))
            }
            "click_slot" => {
                let slot = arg_u64(args, "slot").ok_or("slot is required")? as i16;
                let mode = match args.get("mode").and_then(Value::as_str).unwrap_or("pickup") {
                    "pickup" => 0,
                    "quick_move" | "shift" => 1,
                    "swap" => 2,
                    "clone" => 3,
                    "throw" => 4,
                    "pickup_all" | "double" => 6,
                    m => return Err(format!("unknown mode {m}")),
                };
                let button = arg_u64(args, "button").unwrap_or(0) as i8;
                self.act(Cmd::Click { slot, button, mode }, args, 600)
            }
            "close_menu" => self.act(Cmd::CloseMenu, args, 200),
            "drop_item" => self.act(Cmd::Drop { all: arg_bool(args, "all") }, args, 300),
            "swap_hands" => self.act(Cmd::SwapHands, args, 300),
            "respawn" => self.act(Cmd::Respawn, args, 1500),
            "sign" => {
                let lines: Vec<String> = args
                    .get("lines")
                    .and_then(Value::as_array)
                    .ok_or("lines (up to 4 strings) is required")?
                    .iter()
                    .map(|l| l.as_str().unwrap_or("").to_string())
                    .collect();
                let mut four: [String; 4] = Default::default();
                for (i, l) in lines.into_iter().take(4).enumerate() {
                    four[i] = l;
                }
                self.act(Cmd::Sign { lines: four }, args, 500)
            }
            "entities" => {
                let radius = arg_f64(args, "radius").unwrap_or(32.0);
                let limit = arg_u64(args, "limit").unwrap_or(30) as usize;
                let kind = args.get("type").and_then(Value::as_str).map(str::to_lowercase);
                let named = args.get("name").and_then(Value::as_str).map(str::to_lowercase);
                let bot = self.bot()?;
                let g = bot.game();
                let mut list: Vec<(f64, Value)> = g
                    .entities
                    .values()
                    .filter(|e| e.id != g.entity_id)
                    .filter(|e| kind.as_ref().is_none_or(|k| e.kind.contains(k.as_str())))
                    .filter_map(|e| {
                        let d = dist(g.pos, e.pos);
                        let label = g.entity_label(e);
                        let name_ok = named.as_ref().is_none_or(|n| label.to_lowercase().contains(n.as_str()));
                        (d <= radius && name_ok).then(|| {
                            (d, json!({"id": e.id, "type": e.kind, "name": label, "pos": round3(e.pos), "distance": round2(d)}))
                        })
                    })
                    .collect();
                list.sort_by(|a, b| a.0.total_cmp(&b.0));
                let total = list.len();
                Ok(json!({"entities": list.into_iter().take(limit).map(|(_, v)| v).collect::<Vec<_>>(), "matching": total}))
            }
            "players" => {
                let bot = self.bot()?;
                let g = bot.game();
                let mut players: Vec<Value> = g
                    .players
                    .values()
                    .filter(|p| p.listed != Some(false) && !p.name.is_empty())
                    .map(|p| json!({"name": p.name, "gamemode": p.gamemode.map(gamemode_name), "ping_ms": p.latency, "display_name": p.display_name}))
                    .collect();
                players.sort_by_key(|p| p["name"].as_str().unwrap_or("").to_lowercase());
                Ok(json!({"online": players.len(), "players": players}))
            }
            "block_at" => {
                let p = arg_pos(args)?;
                let bot = self.bot()?;
                let g = bot.game();
                Ok(match g.world.state_at(p[0], p[1], p[2]) {
                    None => json!({"pos": p, "loaded": false}),
                    Some(state) => block_json(&g, p, state),
                })
            }
            "find_blocks" => {
                let name = arg_str(args, "name")?.trim_start_matches("minecraft:").to_lowercase();
                let radius = arg_u64(args, "radius").unwrap_or(16).min(64) as i32;
                let limit = arg_u64(args, "limit").unwrap_or(10).min(200) as usize;
                let bot = self.bot()?;
                let g = bot.game();
                let center = [g.pos[0].floor() as i32, g.pos[1].floor() as i32, g.pos[2].floor() as i32];
                let found: Vec<Value> = g.world.find_blocks(&name, center, radius, limit).into_iter().map(|(p, s)| block_json(&g, p, s)).collect();
                Ok(json!({"center": center, "found": found, "chunks_loaded": g.world.loaded_chunks()}))
            }
            other => Err(format!("unknown tool {other}")),
        }
    }

    fn walk_to(&mut self, args: &Value) -> Result<Value, String> {
        let timeout = Duration::from_secs_f64(arg_f64(args, "timeout_s").unwrap_or(60.0).clamp(1.0, 300.0));
        let relative = arg_bool(args, "relative");
        let bot = self.bot()?;
        let (pos, seq, id) = {
            let g = bot.game();
            (g.pos, g.chat_seq, g.walk_id + 1)
        };
        let x = arg_f64(args, "x").ok_or("x is required")?;
        let z = arg_f64(args, "z").ok_or("z is required")?;
        let y = arg_f64(args, "y");
        let target = if relative {
            [pos[0] + x, pos[1] + y.unwrap_or(0.0), pos[2] + z]
        } else {
            [x, y.unwrap_or(pos[1]), z]
        };
        let sprint = arg_bool(args, "sprint");
        bot.send(Cmd::WalkTo { id, target, has_y: y.is_some(), sprint, timeout })?;
        let g = bot.wait_until(timeout + Duration::from_secs(2), |g| {
            g.ended.is_some() || (g.walk_id == id && !matches!(g.walk, Walk::Walking { .. }))
        });
        let (result, at) = match &g.walk {
            _ if g.walk_id != id => ("the bot did not start walking".to_string(), g.pos),
            Walk::Arrived { at } => ("arrived".to_string(), *at),
            Walk::Failed { reason, at } => (format!("failed: {reason}"), *at),
            Walk::Walking { .. } => ("still walking".to_string(), g.pos),
            Walk::Idle => ("idle".to_string(), g.pos),
        };
        let chat: Vec<String> = g.chat_since(seq).iter().map(ChatLine::display).collect();
        Ok(json!({"result": result, "position": round3(at), "target": round3(target), "distance_left": round2(dist(at, target)), "chat": chat}))
    }

    fn dig(&mut self, args: &Value) -> Result<Value, String> {
        let pos = arg_pos(args)?;
        let face = face(args)?;
        let timeout = Duration::from_secs_f64(arg_f64(args, "timeout_s").unwrap_or(10.0).clamp(0.5, 60.0));
        let bot = self.bot()?;
        let (before, seq) = {
            let g = bot.game();
            (g.world.state_at(pos[0], pos[1], pos[2]), g.chat_seq)
        };
        bot.send(Cmd::Dig { pos, face })?;
        let g = bot.wait_until(timeout, |g| {
            g.ended.is_some() || (before.is_some() && g.world.state_at(pos[0], pos[1], pos[2]) != before)
        });
        let after = g.world.state_at(pos[0], pos[1], pos[2]);
        let chat: Vec<String> = g.chat_since(seq).iter().map(ChatLine::display).collect();
        let name = |s: Option<u32>| s.and_then(|s| g.world.block_name(s)).unwrap_or("unknown").to_string();
        Ok(json!({
            "pos": pos,
            "before": name(before),
            "after": name(after),
            "broken": before.is_some() && after != before,
            "note": if before.is_none() { Some("the block's chunk isn't loaded, so the result can't be checked") } else { None },
            "chat": chat,
        }))
    }

    /// `entity_id`, or the nearest entity whose name/type contains `name`.
    fn entity_arg(&mut self, args: &Value) -> Result<i32, String> {
        if let Some(id) = args.get("entity_id").and_then(Value::as_i64) {
            return Ok(id as i32);
        }
        let name = arg_str(args, "name").map_err(|_| "pass entity_id or name".to_string())?.to_lowercase();
        let bot = self.bot()?;
        let g = bot.game();
        g.entities
            .values()
            .filter(|e| e.id != g.entity_id)
            .filter(|e| g.entity_label(e).to_lowercase().contains(&name) || e.kind.contains(&name))
            .min_by(|a, b| dist(g.pos, a.pos).total_cmp(&dist(g.pos, b.pos)))
            .map(|e| e.id)
            .ok_or_else(|| format!("no entity matching \"{name}\" nearby"))
    }
}

// ---------------------------------------------------------------------------------------------
// JSON helpers

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| format!("{key} is required"))
}
fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
}
fn arg_f64(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}
fn arg_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}
fn arg_pos(args: &Value) -> Result<[i32; 3], String> {
    let c = |k: &str| arg_f64(args, k).map(|v| v.floor() as i32).ok_or_else(|| format!("{k} is required"));
    Ok([c("x")?, c("y")?, c("z")?])
}
fn face(args: &Value) -> Result<u8, String> {
    Ok(match args.get("face").and_then(Value::as_str).unwrap_or("up") {
        "down" | "bottom" => 0,
        "up" | "top" => 1,
        "north" => 2,
        "south" => 3,
        "west" => 4,
        "east" => 5,
        f => return Err(format!("face must be up/down/north/south/west/east, not {f}")),
    })
}
fn check_chat_chars(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("empty".into());
    }
    match s.chars().find(|&c| c == '§' || c < ' ' || c == '\u{7f}') {
        Some(c) => Err(format!("contains {c:?}, which the server rejects (it kicks for illegal characters)")),
        None => Ok(()),
    }
}
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
fn round3(p: [f64; 3]) -> [f64; 3] {
    [round2(p[0]), round2(p[1]), round2(p[2])]
}
fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}
fn gamemode_name(g: i32) -> &'static str {
    match g {
        0 => "survival",
        1 => "creative",
        2 => "adventure",
        3 => "spectator",
        _ => "unknown",
    }
}
fn inventory_slot_name(i: usize) -> Option<String> {
    Some(match i {
        0 => "craft_result".into(),
        1..=4 => format!("craft_{}", i - 1),
        5 => "head".into(),
        6 => "chest".into(),
        7 => "legs".into(),
        8 => "feet".into(),
        9..=35 => return None,
        36..=44 => format!("hotbar_{}", i - 36),
        45 => "offhand".into(),
        _ => return None,
    })
}
fn slot_json(slot: usize, item: &ItemStack, label: Option<String>) -> Value {
    let mut v = item.to_json();
    if let Value::Object(o) = &mut v {
        let mut out = Map::new();
        out.insert("slot".into(), slot.into());
        if let Some(l) = label {
            out.insert("where".into(), l.into());
        }
        out.extend(std::mem::take(o));
        return Value::Object(out);
    }
    v
}
fn line_json(l: &ChatLine) -> Value {
    json!({"seq": l.seq, "t": round2(l.at), "kind": l.kind, "text": l.text})
}
fn ended_text(g: &Game) -> Option<String> {
    g.ended.as_ref().map(|e| match e {
        End::NotJoined(m) => format!("never joined: {m}"),
        End::Left(m) => format!("disconnected: {m}"),
    })
}
fn menu_signature(g: &Game) -> Option<(i32, i32)> {
    g.menu.as_ref().map(|m| (m.id, m.state_id))
}
fn menu_json(g: &Game, with_items: bool) -> Value {
    let Some(m) = &g.menu else {
        return json!({"open": false, "note": "no container menu is open (the player inventory is the `inventory` tool)"});
    };
    let top = m.container_slots();
    let mut out = json!({"open": true, "title": m.title, "type": m.kind, "container_slots": top, "total_slots": m.slots.len()});
    if with_items {
        let items: Vec<Value> = m.slots.iter().take(top).enumerate().filter_map(|(i, s)| s.as_ref().map(|it| slot_json(i, it, None))).collect();
        out["items"] = Value::Array(items);
        out["note"] = format!("slots 0-{} are the container, {}-{} the player's inventory (main then hotbar)", top.saturating_sub(1), top, m.slots.len().saturating_sub(1)).into();
        if g.cursor.is_some() {
            out["cursor"] = g.cursor.as_ref().map(ItemStack::to_json).into();
        }
    }
    out
}
/// What an action led to: new chat, a menu that opened/changed, a disconnect.
fn after(g: &Game, seq: u64, menu_before: Option<(i32, i32)>) -> Value {
    let chat: Vec<String> = g.chat_since(seq).iter().map(ChatLine::display).collect();
    let mut out = json!({"chat": chat});
    if menu_signature(g) != menu_before {
        out["menu"] = menu_json(g, true);
    }
    if let Some(e) = ended_text(g) {
        out["ended"] = e.into();
    }
    if let Some((pos, _)) = g.sign_editor {
        out["sign_editor_open"] = json!(pos);
    }
    out
}
fn block_json(g: &Game, p: [i32; 3], state: u32) -> Value {
    let props: Map<String, Value> = g.world.block_properties(state).into_iter().map(|(k, v)| (k.to_string(), v.into())).collect();
    let mut v = json!({"pos": p, "block": g.world.block_name(state).unwrap_or("unknown"), "state": state});
    if !props.is_empty() {
        v["properties"] = Value::Object(props);
    }
    v
}
pub fn status_json(g: &Game) -> Value {
    let walk = match &g.walk {
        Walk::Idle => Value::Null,
        Walk::Walking { target } => json!({"walking_to": round3(*target)}),
        Walk::Arrived { at } => json!({"last_walk": "arrived", "at": round3(*at)}),
        Walk::Failed { reason, at } => json!({"last_walk": format!("failed: {reason}"), "at": round3(*at)}),
    };
    json!({
        "connected": g.ended.is_none(),
        "in_game": g.ready,
        "ended": ended_text(g),
        "username": g.username,
        "version": format!("{} (protocol {})", g.version, g.protocol),
        "position": round3(g.pos),
        "yaw": (g.yaw * 10.0).round() / 10.0,
        "pitch": (g.pitch * 10.0).round() / 10.0,
        "on_ground": g.on_ground,
        "dimension": g.dimension,
        "gamemode": gamemode_name(g.gamemode),
        "health": g.health,
        "food": g.food,
        "xp_level": g.xp_level,
        "dead": g.dead,
        "flying": g.abilities & 2 != 0,
        "sneaking": g.sneaking,
        "held_slot": g.held,
        "held_item": g.held_item().map(ItemStack::to_json),
        "menu": g.menu.as_ref().map(|m| json!({"title": m.title, "type": m.kind})),
        "walk": walk,
        "entities_tracked": g.entities.len(),
        "players_online": g.players.values().filter(|p| p.listed != Some(false)).count(),
        "chunks_loaded": g.world.loaded_chunks(),
        "inventory_items": g.inventory.iter().take(INVENTORY_SLOTS).filter(|s| s.is_some()).count(),
    })
}

// ---------------------------------------------------------------------------------------------
// Tool definitions

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
    })
}

fn tools() -> Vec<Value> {
    let wait = json!({"type": "integer", "description": "How long to wait for the server's reaction before returning (ms)."});
    let xyz = |what: &str| {
        json!({
            "x": {"type": "number", "description": format!("{what} x")},
            "y": {"type": "number", "description": format!("{what} y")},
            "z": {"type": "number", "description": format!("{what} z")},
        })
    };
    let mut block = xyz("Block");
    block["face"] = json!({"type": "string", "enum": ["up", "down", "north", "south", "west", "east"], "description": "Clicked face (default up)."});
    let mut use_block = block.clone();
    use_block["offhand"] = json!({"type": "boolean"});
    use_block["wait_ms"] = wait.clone();
    let mut dig = block.clone();
    dig["timeout_s"] = json!({"type": "number", "description": "Give up waiting for the block to break after this long (default 10)."});
    let entity = json!({
        "entity_id": {"type": "integer", "description": "Entity id (from `entities`)."},
        "name": {"type": "string", "description": "Or: the nearest entity whose name or type contains this."},
        "offhand": {"type": "boolean"},
        "wait_ms": wait,
    });
    vec![
        tool("connect", "Join a server in offline mode (replaces any current connection). Defaults come from the server's command line.",
            json!({"host": {"type": "string", "description": "host or host:port"}, "port": {"type": "integer"}, "username": {"type": "string", "description": "1-16 chars, letters/digits/_"}, "protocol": {"type": "integer", "description": "774-777; default: ask the server"}}), &[]),
        tool("disconnect", "Leave the server.", json!({}), &[]),
        tool("status", "Position, health, food, game mode, dimension, held item, open menu, walk state, counts of entities/players/chunks.", json!({}), &[]),
        tool("command", "Run a command (without the leading /), e.g. \"gamemode creative\" or a plugin command. Returns the chat that followed.",
            json!({"command": {"type": "string"}, "wait_ms": wait}), &["command"]),
        tool("chat", "Send a chat message (max 256 chars). Returns the chat that followed.", json!({"message": {"type": "string"}, "wait_ms": wait}), &["message"]),
        tool("read_chat", "Chat lines received (system, player, action bar, titles, deaths, disconnects) since the last read_chat, or since `since` (a seq).",
            json!({"since": {"type": "integer"}, "limit": {"type": "integer", "description": "Newest lines to return (default 100)."}}), &[]),
        tool("wait", "Wait, collecting chat; stops early when a line contains `until_chat`.",
            json!({"ms": {"type": "integer", "description": "Max 60000 (default 1000)."}, "until_chat": {"type": "string"}}), &[]),
        tool("look", "Turn to yaw/pitch (degrees) or to look at a point x/y/z.",
            json!({"yaw": {"type": "number"}, "pitch": {"type": "number"}, "x": {"type": "number"}, "y": {"type": "number"}, "z": {"type": "number"}}), &[]),
        tool("walk_to", "Walk (path-finding around obstacles, jumping up blocks) to x/z (and y if given). Blocks until arrived, stuck or timeout. Note: `command` with tp is faster if allowed.",
            json!({"x": {"type": "number"}, "z": {"type": "number"}, "y": {"type": "number", "description": "Feet height; omit for any height."}, "relative": {"type": "boolean", "description": "x/y/z are offsets from the current position."}, "sprint": {"type": "boolean"}, "timeout_s": {"type": "number", "description": "Default 60."}}), &["x", "z"]),
        tool("stop", "Stop walking.", json!({}), &[]),
        tool("jump", "Jump once (or rise when flying).", json!({}), &[]),
        tool("sneak", "Start (on=true) or stop sneaking.", json!({"on": {"type": "boolean"}}), &[]),
        tool("select_slot", "Select a hotbar slot (0-8).", json!({"slot": {"type": "integer"}, "wait_ms": wait}), &["slot"]),
        tool("use_item", "Right-click the air with the held item (e.g. a compass/menu item that opens a plugin GUI). Reports a menu if one opens.",
            json!({"offhand": {"type": "boolean"}, "wait_ms": wait}), &[]),
        tool("use_block", "Right-click a block: open chests/doors, press buttons, place the held block against that face, etc. Reports a menu if one opens.", use_block, &["x", "y", "z"]),
        tool("dig", "Break a block (the server times it; creative breaks instantly). Waits until it's gone.", dig, &["x", "y", "z"]),
        tool("attack", "Hit an entity.", entity.clone(), &[]),
        tool("interact", "Right-click an entity (NPCs, villagers, armor stands...). Reports a menu if one opens.", entity, &[]),
        tool("inventory", "The player's inventory (non-empty slots, with names/lore/enchantments), held slot and cursor item.", json!({}), &[]),
        tool("menu", "The open container/GUI menu: title, type and items in its slots.", json!({}), &[]),
        tool("click_slot", "Click a slot of the open menu (or of the inventory when none is open). mode: pickup (button 0 left / 1 right), quick_move (shift-click), swap (button = hotbar 0-8 or 40 offhand), clone, throw, pickup_all. Returns chat and the updated menu.",
            json!({"slot": {"type": "integer"}, "button": {"type": "integer"}, "mode": {"type": "string", "enum": ["pickup", "quick_move", "swap", "clone", "throw", "pickup_all"]}, "wait_ms": wait}), &["slot"]),
        tool("close_menu", "Close the open menu.", json!({}), &[]),
        tool("drop_item", "Drop the held item (one, or the stack with all=true).", json!({"all": {"type": "boolean"}}), &[]),
        tool("swap_hands", "Swap the main-hand and offhand items.", json!({}), &[]),
        tool("respawn", "Respawn after dying.", json!({}), &[]),
        tool("sign", "Write the lines of a sign editor the server opened.", json!({"lines": {"type": "array", "items": {"type": "string"}, "maxItems": 4}}), &["lines"]),
        tool("entities", "Entities near the bot (players, mobs, NPCs, holograms...), nearest first, with id, type, name, position, distance.",
            json!({"radius": {"type": "number", "description": "Default 32."}, "type": {"type": "string", "description": "e.g. player, villager, armor_stand"}, "name": {"type": "string"}, "limit": {"type": "integer"}}), &[]),
        tool("players", "Players in the tab list with game mode and ping.", json!({}), &[]),
        tool("block_at", "The block at x/y/z (name and properties), if its chunk is loaded.", xyz("Block"), &["x", "y", "z"]),
        tool("find_blocks", "Find blocks whose name contains `name` (e.g. chest, sign, diamond_ore) around the bot, nearest first.",
            json!({"name": {"type": "string"}, "radius": {"type": "integer", "description": "Default 16, max 64."}, "limit": {"type": "integer"}}), &["name"]),
    ]
}
