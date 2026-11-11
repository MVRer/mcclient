//! The bot engine: one connection driven by a background thread that answers the server, runs a
//! 20-ticks-per-second loop (movement, physics, tick-end), keeps the game state the server sends
//! (position, health, inventory, open menu, entities, players, chat, chunks) in [`Game`], and
//! carries out [`Cmd`]s. The one-shot CLI and the MCP server both drive a [`Bot`].

use crate::chat::{self, ChatTypes};
use crate::ids::{self, Ids, handshake, login, status};
use crate::item::{self, ItemContext, ItemStack};
use crate::nbt::{self, Tag};
use crate::net::{Conn, Packet, Reader, Writer, invalid};
use crate::physics::{self, Body, Input};
use crate::registries::{self, Dynamic, Registries};
use crate::text::{self, Lang};
use crate::world::World;
use md5::{Digest, Md5};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BRAND: &str = "mcclient";
const TICK: Duration = Duration::from_millis(50);
/// How long after Join Game without a spawn position the bot counts as in the game anyway.
const SPAWN_GRACE: Duration = Duration::from_secs(2);
const CHAT_KEEP: usize = 1000;
/// The player inventory menu (container 0): 0 craft result, 1-4 craft grid, 5-8 armor (head..feet),
/// 9-35 main, 36-44 hotbar, 45 offhand.
pub const INVENTORY_SLOTS: usize = 46;

// ---------------------------------------------------------------------------------------------
// Small shared helpers

/// UUID.nameUUIDFromBytes(("OfflinePlayer:" + name).getBytes(UTF_8)): MD5, version 3, IETF variant.
pub fn offline_uuid(name: &str) -> u128 {
    let digest = Md5::digest(format!("OfflinePlayer:{name}").as_bytes());
    let mut h = [0u8; 16];
    h.copy_from_slice(&digest);
    h[6] = (h[6] & 0x0F) | 0x30;
    h[8] = (h[8] & 0x3F) | 0x80;
    u128::from_be_bytes(h)
}

/// The canonical 8-4-4-4-12 form.
pub fn uuid_string(u: u128) -> String {
    let h = format!("{u:032x}");
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// Progress messages on stderr (stdout is the chat in CLI mode and the JSON-RPC channel in MCP mode).
#[derive(Clone)]
pub struct Log {
    start: Instant,
    verbose: bool,
}

impl Log {
    pub fn new(verbose: bool) -> Self {
        Self { start: Instant::now(), verbose }
    }
    pub fn info(&self, msg: impl AsRef<str>) {
        eprintln!("[{:>7.3}s] {}", self.start.elapsed().as_secs_f64(), msg.as_ref());
    }
    pub fn debug(&self, msg: impl AsRef<str>) {
        if self.verbose {
            self.info(msg);
        }
    }
}

fn handshake_body(protocol: i32, host: &str, port: u16, intent: i32) -> Vec<u8> {
    let mut w = Writer::new();
    w.varint(protocol).string(host).u16(port).varint(intent);
    w.data
}

pub struct ServerStatus {
    pub protocol: i32,
    pub name: String,
}

/// Server List Ping: what version the server (or proxy) says it runs.
pub fn ping(host: &str, port: u16, timeout: Duration) -> io::Result<ServerStatus> {
    let deadline = Instant::now() + timeout;
    let mut c = Conn::connect(host, port, timeout)?;
    c.send(handshake::INTENTION, &handshake_body(ids::V777.protocol, host, port, handshake::INTENT_STATUS))?;
    c.send(status::SB_STATUS_REQUEST, &[])?;
    while Instant::now() < deadline {
        if let Some(p) = c.poll()?
            && p.id == status::CB_STATUS_RESPONSE
        {
            let json = Reader::new(&p.body).string()?;
            let v: serde_json::Value =
                serde_json::from_str(&json).map_err(|e| invalid(format!("status response is not JSON: {e}")))?;
            let protocol = v["version"]["protocol"]
                .as_i64()
                .ok_or_else(|| invalid("status response has no version.protocol"))?;
            let name = v["version"]["name"].as_str().unwrap_or("?").to_string();
            return Ok(ServerStatus { protocol: protocol as i32, name });
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "no status response"))
}

/// Pick the protocol: forced, else whatever the server reports if supported, else 26.3.
pub fn choose_protocol(host: &str, port: u16, forced: Option<i32>, log: &Log) -> Result<&'static Ids, String> {
    if let Some(p) = forced {
        return ids::for_protocol(p)
            .ok_or_else(|| format!("protocol {p} is not supported (supported: {})", ids::supported_list()));
    }
    let default = ids::SUPPORTED[0];
    match ping(host, port, Duration::from_secs(3)) {
        Ok(s) => {
            log.info(format!("server reports \"{}\" (protocol {})", text::strip_formatting(&s.name), s.protocol));
            Ok(ids::for_protocol(s.protocol).unwrap_or_else(|| {
                log.info(format!(
                    "protocol {} is not one this client speaks ({}); trying {} ({}) anyway, use --protocol to choose",
                    s.protocol,
                    ids::supported_list(),
                    default.protocol,
                    default.name
                ));
                default
            }))
        }
        Err(e) => {
            log.info(format!("status ping failed ({e}); assuming {} ({})", default.protocol, default.name));
            Ok(default)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Shared state

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Connecting,
    Login,
    Configuration,
    Play,
    Closed,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Connecting => "connecting",
            Phase::Login => "login",
            Phase::Configuration => "config",
            Phase::Play => "play",
            Phase::Closed => "closed",
        }
    }
}

/// Why the connection ended.
#[derive(Clone, Debug, PartialEq)]
pub enum End {
    /// Never got into the game (refused, timed out, connection failed...).
    NotJoined(String),
    /// Was in the game, then disconnected / lost the connection / was told to leave.
    Left(String),
}

#[derive(Clone, Debug)]
pub struct ChatLine {
    pub seq: u64,
    /// Seconds since the bot started.
    pub at: f64,
    /// system, actionbar, player, title, subtitle, death, disconnect
    pub kind: &'static str,
    pub text: String,
}

impl ChatLine {
    /// The CLI's stdout form.
    pub fn display(&self) -> String {
        match self.kind {
            "system" | "player" => self.text.clone(),
            kind => format!("[{kind}] {}", self.text),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entity {
    pub id: i32,
    pub uuid: u128,
    pub kind: String,
    pub pos: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    /// Custom name (entity data index 2), e.g. an NPC or an armor-stand hologram.
    pub custom_name: Option<String>,
    /// A text display's text (holograms).
    pub text: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PlayerEntry {
    pub name: String,
    pub gamemode: Option<i32>,
    pub latency: Option<i32>,
    pub listed: Option<bool>,
    pub display_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Menu {
    pub id: i32,
    pub kind: String,
    pub title: String,
    /// The menu's slots followed by the 36 player inventory slots.
    pub slots: Vec<Option<ItemStack>>,
    pub state_id: i32,
}

impl Menu {
    /// Slots belonging to the container itself (the rest is the player's inventory).
    pub fn container_slots(&self) -> usize {
        self.slots.len().saturating_sub(36)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Walk {
    Idle,
    Walking { target: [f64; 3] },
    Arrived { at: [f64; 3] },
    Failed { reason: String, at: [f64; 3] },
}

/// Everything known about the connection and the game. Read it through [`Bot::game`].
pub struct Game {
    pub phase: Phase,
    pub ended: Option<End>,
    pub protocol: i32,
    pub version: &'static str,
    pub username: String,
    pub uuid: u128,
    pub started: Instant,
    /// In the game and spawned: commands and actions are accepted.
    pub ready: bool,
    pub entity_id: i32,
    pub pos: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub health: f32,
    pub food: i32,
    pub saturation: f32,
    pub xp_level: i32,
    pub gamemode: i32,
    pub dimension: String,
    /// Player abilities flags: invulnerable 1, flying 2, may fly 4, instant build 8.
    pub abilities: u8,
    pub dead: bool,
    pub inventory: Vec<Option<ItemStack>>,
    pub inventory_state: i32,
    /// Selected hotbar slot, 0-8.
    pub held: usize,
    pub cursor: Option<ItemStack>,
    pub menu: Option<Menu>,
    /// A sign editor the server opened: (position, front side).
    pub sign_editor: Option<([i32; 3], bool)>,
    pub entities: HashMap<i32, Entity>,
    pub players: HashMap<u128, PlayerEntry>,
    pub chat: VecDeque<ChatLine>,
    pub chat_seq: u64,
    pub world: World,
    pub dynamic: Dynamic,
    pub walk: Walk,
    /// Id of the walk `walk` is about (the latest `Cmd::WalkTo`).
    pub walk_id: u64,
    pub sneaking: bool,
    pub sprinting: bool,
    /// Item packets that could not be decoded (inventory/menu contents may be stale).
    pub item_errors: u64,
    last_action_bar: Option<String>,
}

impl Game {
    fn new(username: &str, ids: &'static Ids) -> Self {
        Self {
            phase: Phase::Connecting,
            ended: None,
            protocol: ids.protocol,
            version: ids.name,
            username: username.to_string(),
            uuid: offline_uuid(username),
            started: Instant::now(),
            ready: false,
            entity_id: -1,
            pos: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
            health: 20.0,
            food: 20,
            saturation: 5.0,
            xp_level: 0,
            gamemode: 0,
            dimension: String::new(),
            abilities: 0,
            dead: false,
            inventory: vec![None; INVENTORY_SLOTS],
            inventory_state: 0,
            held: 0,
            cursor: None,
            menu: None,
            sign_editor: None,
            entities: HashMap::new(),
            players: HashMap::new(),
            chat: VecDeque::new(),
            chat_seq: 0,
            world: World::new(ids.protocol),
            dynamic: Dynamic::new(),
            walk: Walk::Idle,
            walk_id: 0,
            sneaking: false,
            sprinting: false,
            item_errors: 0,
            last_action_bar: None,
        }
    }

    fn push_chat(&mut self, kind: &'static str, text: String) {
        if kind == "actionbar" {
            // Action bars are often re-sent every second: keep each new one once.
            if self.last_action_bar.as_deref() == Some(text.as_str()) {
                return;
            }
            self.last_action_bar = Some(text.clone());
        }
        self.chat_seq += 1;
        self.chat.push_back(ChatLine { seq: self.chat_seq, at: self.started.elapsed().as_secs_f64(), kind, text });
        while self.chat.len() > CHAT_KEEP {
            self.chat.pop_front();
        }
    }

    pub fn chat_since(&self, seq: u64) -> Vec<ChatLine> {
        self.chat.iter().filter(|l| l.seq > seq).cloned().collect()
    }

    pub fn held_item(&self) -> Option<&ItemStack> {
        self.inventory.get(36 + self.held).and_then(Option::as_ref)
    }

    /// The name shown for an entity: custom name, player name, text, else its type.
    pub fn entity_label(&self, e: &Entity) -> String {
        if let Some(n) = e.custom_name.as_ref().filter(|n| !n.is_empty()) {
            return n.clone();
        }
        if let Some(p) = self.players.get(&e.uuid) {
            return p.name.clone();
        }
        if let Some(t) = e.text.as_ref().filter(|t| !t.is_empty()) {
            return t.clone();
        }
        e.kind.clone()
    }
}

pub struct Shared {
    pub game: Mutex<Game>,
    /// Notified whenever the game state changes.
    pub changed: Condvar,
}

// ---------------------------------------------------------------------------------------------
// Commands

#[derive(Clone, Debug)]
pub enum Cmd {
    Command(String),
    Chat(String),
    Look { yaw: f32, pitch: f32 },
    LookAt([f64; 3]),
    /// `id`: pick one above `Game::walk_id`; `walk_id`/`walk` report on it.
    WalkTo { id: u64, target: [f64; 3], has_y: bool, sprint: bool, timeout: Duration },
    Stop,
    Jump,
    Sneak(bool),
    SelectSlot(usize),
    UseItem { offhand: bool },
    UseBlock { pos: [i32; 3], face: u8, offhand: bool },
    Dig { pos: [i32; 3], face: u8 },
    Attack(i32),
    Interact { id: i32, offhand: bool },
    Swing,
    Click { slot: i16, button: i8, mode: i32 },
    CloseMenu,
    Drop { all: bool },
    SwapHands,
    Respawn,
    Sign { lines: [String; 4] },
    Disconnect,
}

pub struct Config {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub protocol: Option<i32>,
    pub view_distance: i8,
    pub join_timeout: Duration,
    pub verbose: bool,
}

/// A running bot. Dropping it disconnects.
pub struct Bot {
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
}

impl Bot {
    /// Pick the protocol (status ping unless forced), connect and start logging in, in the
    /// background. Errors only for an unsupported forced protocol.
    pub fn start(cfg: Config, lang: Arc<Lang>, log: Log) -> Result<Bot, String> {
        let ids = choose_protocol(&cfg.host, cfg.port, cfg.protocol, &log)?;
        let shared = Arc::new(Shared { game: Mutex::new(Game::new(&cfg.username, ids)), changed: Condvar::new() });
        let (tx, rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let thread = thread::Builder::new()
            .name("mcclient-engine".into())
            .spawn(move || Engine::run(cfg, ids, lang, log, thread_shared, rx))
            .map_err(|e| e.to_string())?;
        Ok(Bot { shared, tx, thread: Some(thread) })
    }

    pub fn game(&self) -> MutexGuard<'_, Game> {
        self.shared.game.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wait (up to `timeout`) until `done(game)` holds; returns the guard either way.
    pub fn wait_until(&self, timeout: Duration, mut done: impl FnMut(&Game) -> bool) -> MutexGuard<'_, Game> {
        let deadline = Instant::now() + timeout;
        let mut g = self.game();
        while !done(&g) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            g = self.shared.changed.wait_timeout(g, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
        g
    }

    /// Wait until the bot is in the game (Ok) or the connection ended (Err with why).
    pub fn wait_ready(&self, timeout: Duration) -> Result<(), String> {
        let g = self.wait_until(timeout, |g| g.ready || g.ended.is_some());
        match &g.ended {
            Some(End::NotJoined(m) | End::Left(m)) => Err(m.clone()),
            None if g.ready => Ok(()),
            None => Err("still joining".into()),
        }
    }

    pub fn send(&self, cmd: Cmd) -> Result<(), String> {
        if let Some(e) = &self.game().ended {
            return Err(match e {
                End::NotJoined(m) | End::Left(m) => format!("not connected: {m}"),
            });
        }
        self.tx.send(cmd).map_err(|_| "the connection is closed".to_string())
    }

    pub fn disconnect(mut self) {
        let _ = self.tx.send(Cmd::Disconnect);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Bot {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Disconnect);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The engine thread

struct Goal {
    target: [f64; 3],
    has_y: bool,
    sprint: bool,
    path: Vec<[f64; 3]>,
    next: usize,
    started: u64,
    timeout_ticks: u64,
    best: f64,
    last_progress: u64,
    repathed: bool,
}

struct Engine {
    cfg: Config,
    ids: &'static Ids,
    regs: &'static Registries,
    lang: Arc<Lang>,
    log: Log,
    shared: Arc<Shared>,
    rx: Receiver<Cmd>,
    conn: Conn,
    chat_types: ChatTypes,
    /// (name, (min_y, height) when the server sent the data) of the dimension_type registry.
    dimension_types: Vec<(String, Option<(i32, i32)>)>,
    recent: VecDeque<(Phase, i32)>,
    joined_at: Option<Instant>,
    position_seen: bool,
    loaded_sent: bool,
    body: Body,
    yaw: f32,
    pitch: f32,
    jump_once: bool,
    goal: Option<Goal>,
    last_sent: Option<([f64; 3], f32, f32, u8)>,
    ticks_since_sent: u32,
    input_flags: u8,
    sequence: i32,
    tick: u64,
    corrections: VecDeque<u64>,
    /// Was in the game at least once (a later disconnect is "left", not "never joined").
    was_ready: bool,
    quit: bool,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

impl Engine {
    fn run(cfg: Config, ids: &'static Ids, lang: Arc<Lang>, log: Log, shared: Arc<Shared>, rx: Receiver<Cmd>) {
        log.info(format!("connecting to {}:{} with protocol {} ({})", cfg.host, cfg.port, ids.protocol, ids.name));
        let conn = match Conn::connect(&cfg.host, cfg.port, Duration::from_secs(10)) {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("cannot connect to {}:{}: {e}", cfg.host, cfg.port);
                let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
                g.phase = Phase::Closed;
                g.ended = Some(End::NotJoined(msg));
                shared.changed.notify_all();
                return;
            }
        };
        let _ = conn.set_poll_timeout(Duration::from_millis(10));
        let mut e = Engine {
            regs: registries::for_protocol(ids.protocol),
            cfg,
            ids,
            lang,
            log,
            shared,
            rx,
            conn,
            chat_types: ChatTypes::default(),
            dimension_types: Vec::new(),
            recent: VecDeque::new(),
            joined_at: None,
            position_seen: false,
            loaded_sent: false,
            body: Body::default(),
            yaw: 0.0,
            pitch: 0.0,
            jump_once: false,
            goal: None,
            last_sent: None,
            ticks_since_sent: 0,
            input_flags: 0,
            sequence: 0,
            tick: 0,
            corrections: VecDeque::new(),
            was_ready: false,
            quit: false,
        };
        let end = e.main_loop();
        let shared = e.shared.clone();
        let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
        g.phase = Phase::Closed;
        g.ready = false;
        if g.ended.is_none() {
            g.ended = Some(end);
        }
        if let Walk::Walking { .. } = g.walk {
            g.walk = Walk::Failed { reason: "disconnected".into(), at: g.pos };
        }
        shared.changed.notify_all();
    }

    fn game(&self) -> MutexGuard<'_, Game> {
        self.shared.game.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn phase(&self) -> Phase {
        self.game().phase
    }

    fn set_phase(&self, phase: Phase) {
        self.game().phase = phase;
        self.shared.changed.notify_all();
    }

    fn main_loop(&mut self) -> End {
        let join_deadline = Instant::now() + self.cfg.join_timeout;
        let uuid = offline_uuid(&self.cfg.username);
        self.log.info(format!("logging in as {} (offline UUID {})", self.cfg.username, uuid_string(uuid)));
        let hs = handshake_body(self.ids.protocol, &self.cfg.host, self.cfg.port, handshake::INTENT_LOGIN);
        let mut hello = Writer::new();
        hello.string(&self.cfg.username).uuid(uuid);
        self.set_phase(Phase::Login);
        if let Err(e) = self.send(handshake::INTENTION, &hs).and_then(|_| self.send(login::SB_HELLO, &hello.data)) {
            return End::NotJoined(format!("cannot send the login: {e}"));
        }
        let mut next_tick = Instant::now() + TICK;
        loop {
            // Commands from the driver.
            loop {
                match self.rx.try_recv() {
                    Ok(cmd) => {
                        if let Err(e) = self.command(cmd) {
                            return self.ended(format!("connection lost: {e}"));
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.quit = true;
                        break;
                    }
                }
            }
            if self.quit {
                self.log.info("disconnecting");
                return self.ended("disconnected by the client".into());
            }
            let now = Instant::now();
            if !self.game().ready {
                if now >= join_deadline {
                    return End::NotJoined(self.stall_report());
                }
                self.check_ready();
            }
            if now >= next_tick {
                next_tick += TICK;
                if now > next_tick + Duration::from_secs(1) {
                    next_tick = now + TICK; // fell far behind (suspended?): don't replay ticks
                }
                if let Err(e) = self.game_tick() {
                    return self.ended(format!("connection lost: {e}"));
                }
            }
            match self.conn.poll() {
                Ok(Some(p)) => match self.handle(p) {
                    Ok(None) => {}
                    Ok(Some(end)) => return end,
                    Err(e) => return self.ended(format!("connection lost: {e}")),
                },
                Ok(None) => {}
                Err(e) => return self.ended(format!("connection lost: {e}")),
            }
        }
    }

    fn check_ready(&mut self) {
        let Some(at) = self.joined_at else { return };
        if self.phase() == Phase::Play && (self.position_seen || at.elapsed() >= SPAWN_GRACE) {
            let shared = self.shared.clone();
            let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
            if !g.ready {
                g.ready = true;
                self.was_ready = true;
                self.log.info("in the game");
                shared.changed.notify_all();
            }
        }
    }

    /// The end of the connection now: a failure to join, or leaving after joining.
    fn ended(&self, msg: String) -> End {
        if self.was_ready {
            End::Left(msg)
        } else {
            End::NotJoined(format!("{msg} ({})", self.stage()))
        }
    }

    fn stage(&self) -> String {
        match self.phase() {
            Phase::Connecting | Phase::Login => "in the login phase: no Login Success from the server".into(),
            Phase::Configuration => "in the configuration phase: the server never sent Finish Configuration".into(),
            Phase::Play if self.joined_at.is_none() => "in play, but no Join Game (login) packet arrived".into(),
            Phase::Play | Phase::Closed => "in play, waiting for the spawn position".into(),
        }
    }

    fn stall_report(&self) -> String {
        let recent: Vec<String> = self.recent.iter().map(|(s, id)| format!("{} 0x{id:02X}", s.name())).collect();
        format!(
            "timed out after {}s, stuck {}; last packets received: {}",
            self.cfg.join_timeout.as_secs_f64(),
            self.stage(),
            if recent.is_empty() { "none".into() } else { recent.join(", ") }
        )
    }

    fn send(&mut self, id: i32, body: &[u8]) -> io::Result<()> {
        if id < 0 {
            return Ok(()); // a packet this protocol version doesn't have
        }
        self.log.debug(format!("-> {} 0x{id:02X} ({} bytes)", self.phase().name(), body.len()));
        self.conn.send(id, body)
    }

    fn chat(&self, kind: &'static str, text: String) {
        self.log.debug(format!("chat [{kind}] {text}"));
        self.game().push_chat(kind, text);
        self.shared.changed.notify_all();
    }

    // -----------------------------------------------------------------------------------------
    // Incoming packets

    fn handle(&mut self, p: Packet) -> io::Result<Option<End>> {
        let phase = self.phase();
        self.log.debug(format!("<- {} 0x{:02X} ({} bytes)", phase.name(), p.id, p.body.len()));
        self.recent.push_back((phase, p.id));
        if self.recent.len() > 8 {
            self.recent.pop_front();
        }
        let out = match phase {
            Phase::Connecting | Phase::Login => self.handle_login(p),
            Phase::Configuration => self.handle_configuration(p),
            Phase::Play => self.handle_play(p),
            Phase::Closed => Ok(None),
        };
        self.shared.changed.notify_all();
        out
    }

    fn handle_login(&mut self, p: Packet) -> io::Result<Option<End>> {
        let mut r = Reader::new(&p.body);
        match p.id {
            login::CB_SET_COMPRESSION => {
                let threshold = r.varint()?;
                self.log.debug(format!("compression threshold {threshold}"));
                self.conn.set_compression(threshold);
            }
            login::CB_LOGIN_FINISHED => {
                self.send(login::SB_LOGIN_ACKNOWLEDGED, &[])?;
                self.set_phase(Phase::Configuration);
                self.log.info("login accepted; configuration phase");
                self.send_client_settings()?;
            }
            login::CB_DISCONNECT => {
                let reason = text::json_plain(&r.string()?, &self.lang);
                self.chat("disconnect", reason.clone());
                return Ok(Some(End::NotJoined(format!("refused during login: {reason}"))));
            }
            login::CB_ENCRYPTION_REQUEST => {
                return Ok(Some(End::NotJoined(
                    "the server is in online mode (it asked for encryption); this client only joins \
                     offline-mode servers (online-mode=false)"
                        .into(),
                )));
            }
            login::CB_CUSTOM_QUERY => {
                let transaction = r.varint()?;
                self.log.debug(format!("login plugin request {transaction} on {}", r.string()?));
                let mut w = Writer::new();
                w.varint(transaction).bool(false);
                self.send(login::SB_CUSTOM_QUERY_ANSWER, &w.data)?;
            }
            login::CB_COOKIE_REQUEST => {
                let mut w = Writer::new();
                w.string(&r.string()?).bool(false);
                self.send(login::SB_COOKIE_RESPONSE, &w.data)?;
            }
            _ => {}
        }
        Ok(None)
    }

    fn send_client_settings(&mut self) -> io::Result<()> {
        let cfg = &self.ids.cfg;
        let (payload_id, info_id) = (cfg.sb_custom_payload, cfg.sb_client_information);
        let mut brand = Writer::new();
        brand.string("minecraft:brand").string(BRAND);
        self.send(payload_id, &brand.data)?;
        let mut info = Writer::new();
        info.string("en_us")
            .i8(self.cfg.view_distance)
            .varint(0) // chat: full
            .bool(true) // chat colors
            .u8(0x7F) // skin parts
            .varint(1) // main hand: right
            .bool(false) // text filtering
            .bool(true) // allow server listings
            .varint(2); // particles: minimal
        self.send(info_id, &info.data)
    }

    fn handle_configuration(&mut self, p: Packet) -> io::Result<Option<End>> {
        let cfg = &self.ids.cfg;
        let mut r = Reader::new(&p.body);
        let id = p.id;
        if id == cfg.cb_keep_alive {
            let mut w = Writer::new();
            w.i64(r.i64()?);
            self.send(cfg.sb_keep_alive, &w.data)?;
        } else if id == cfg.cb_ping {
            let mut w = Writer::new();
            w.i32(r.i32()?);
            self.send(cfg.sb_pong, &w.data)?;
        } else if id == cfg.cb_select_known_packs {
            // Claim the server's packs (up to the 64 it reads back): it then leaves out registry
            // data we don't need. This is what a same-version vanilla client does.
            let n = r.prefixed_len()?;
            let mut w = Writer::new();
            let mut packs = Vec::new();
            for _ in 0..n {
                let pack = (r.string()?, r.string()?, r.string()?);
                if packs.len() < 64 {
                    packs.push(pack);
                }
            }
            self.log.debug(format!("known packs: {packs:?}"));
            w.varint(packs.len() as i32);
            for (namespace, pack, version) in &packs {
                w.string(namespace).string(pack).string(version);
            }
            self.send(cfg.sb_select_known_packs, &w.data)?;
        } else if id == cfg.cb_registry_data {
            if let Err(e) = self.registry_data(&mut r) {
                self.log.debug(format!("could not read registry data: {e}"));
            }
        } else if id == cfg.cb_code_of_conduct {
            self.log.info("accepting the server's code of conduct");
            self.send(cfg.sb_accept_code_of_conduct, &[])?;
        } else if id == cfg.cb_resource_pack_push {
            let response = cfg.sb_resource_pack;
            self.accept_resource_pack(response, r.uuid()?)?;
        } else if id == cfg.cb_cookie_request {
            let mut w = Writer::new();
            w.string(&r.string()?).bool(false);
            self.send(cfg.sb_cookie_response, &w.data)?;
        } else if id == cfg.cb_disconnect {
            let reason = chat::read_text(&mut r, &self.lang)?;
            self.chat("disconnect", reason.clone());
            return Ok(Some(self.ended(format!("disconnected during configuration: {reason}"))));
        } else if id == cfg.cb_finish_configuration {
            self.send(cfg.sb_finish_configuration, &[])?;
            self.set_phase(Phase::Play);
            self.log.info("configuration done; entering play");
        }
        Ok(None)
    }

    /// Registry Data: remember every registry's entry names (items refer to enchantments etc. by
    /// id), the chat types and the dimension heights.
    fn registry_data(&mut self, r: &mut Reader) -> io::Result<()> {
        let registry = r.string()?;
        let n = r.prefixed_len()?;
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            let name = r.string()?;
            let data = if r.bool()? { nbt::read_network(r)? } else { None };
            entries.push((name, data));
        }
        let short = registry.strip_prefix("minecraft:").unwrap_or(&registry).to_string();
        match short.as_str() {
            "chat_type" => self.chat_types.set_entries(&entries),
            "dimension_type" => {
                self.dimension_types = entries
                    .iter()
                    .map(|(name, data)| {
                        let int = |k: &str| match data.as_ref().and_then(|d| d.get(k)) {
                            Some(Tag::Int(v)) => Some(*v),
                            _ => None,
                        };
                        (name.clone(), int("min_y").zip(int("height")))
                    })
                    .collect();
            }
            _ => {}
        }
        let names = entries
            .into_iter()
            .map(|(n, _)| n.strip_prefix("minecraft:").map(str::to_string).unwrap_or(n))
            .collect();
        self.game().dynamic.insert(short, names);
        Ok(())
    }

    /// (min_y, height) of a dimension type id.
    fn dimension_height(&self, type_id: i32) -> (i32, i32) {
        let entry = usize::try_from(type_id).ok().and_then(|i| self.dimension_types.get(i));
        if let Some((_, Some(h))) = entry {
            return *h;
        }
        match entry.map(|(n, _)| n.as_str()) {
            Some("minecraft:the_nether" | "minecraft:the_end") => (0, 256),
            _ => (-64, 384),
        }
    }

    fn item_context<'a>(&'a self, dynamic: &'a Dynamic) -> ItemContext<'a> {
        ItemContext { registries: self.regs, dynamic, lang: &self.lang }
    }

    fn read_slots(&self, g: &Game, r: &mut Reader, n: usize) -> io::Result<Vec<Option<ItemStack>>> {
        let cx = self.item_context(&g.dynamic);
        (0..n).map(|_| item::read_slot(r, &cx)).collect()
    }

    fn read_slot(&self, g: &Game, r: &mut Reader) -> io::Result<Option<ItemStack>> {
        item::read_slot(r, &self.item_context(&g.dynamic))
    }

    /// CommonPlayerSpawnInfo: (dimension type, dimension, game mode). 26.3 sends the game modes as
    /// VarInts, older versions as bytes.
    fn spawn_info(&self, r: &mut Reader) -> io::Result<(i32, String, i32)> {
        let dimension_type = r.varint()?;
        let dimension = r.string()?;
        let _seed = r.i64()?;
        let gamemode = if self.ids.protocol >= 777 { r.varint()? } else { r.u8()? as i8 as i32 };
        Ok((dimension_type, dimension, gamemode))
    }

    fn enter_dimension(&mut self, dimension_type: i32, dimension: String, gamemode: i32) {
        let (min_y, height) = self.dimension_height(dimension_type);
        let shared = self.shared.clone();
        let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
        let biomes = g.dynamic.get("worldgen/biome").map_or(64, Vec::len);
        g.world.reset(min_y, height, biomes);
        g.dimension = dimension;
        g.gamemode = gamemode;
        g.entities.clear();
        g.menu = None;
        g.dead = false;
    }

    fn handle_play(&mut self, p: Packet) -> io::Result<Option<End>> {
        let pl = &self.ids.play;
        let mut r = Reader::new(&p.body);
        let id = p.id;
        let new_layout = self.ids.protocol >= 777;
        let shared = self.shared.clone();

        if id == pl.cb_keep_alive {
            let mut w = Writer::new();
            w.i64(r.i64()?);
            self.send(pl.sb_keep_alive, &w.data)?;
        } else if id == pl.cb_ping {
            let mut w = Writer::new();
            w.i32(r.i32()?);
            self.send(pl.sb_pong, &w.data)?;
        } else if id == pl.cb_login {
            let parsed = (|| -> io::Result<(i32, i32, String, i32)> {
                let entity = r.i32()?;
                r.bool()?; // hardcore
                let levels = r.prefixed_len()?;
                for _ in 0..levels {
                    r.string()?;
                }
                r.varint()?; // max players
                r.varint()?; // view distance
                r.varint()?; // simulation distance
                r.take(3)?; // reduced debug info, death screen, limited crafting
                let (dt, dim, gm) = self.spawn_info(&mut r)?;
                Ok((entity, dt, dim, gm))
            })();
            if self.joined_at.is_none() {
                self.log.info("joined the game");
            }
            self.joined_at = Some(Instant::now());
            match parsed {
                Ok((entity, dt, dim, gm)) => {
                    shared.game.lock().unwrap_or_else(|e| e.into_inner()).entity_id = entity;
                    self.enter_dimension(dt, dim, gm);
                }
                Err(e) => self.log.debug(format!("unreadable Join Game ({e})")),
            }
        } else if id == pl.cb_respawn {
            if let Ok((dt, dim, gm)) = self.spawn_info(&mut r) {
                self.enter_dimension(dt, dim, gm);
            }
            self.position_seen = false;
            self.loaded_sent = false;
        } else if id == pl.cb_player_position {
            self.teleported(&mut r)?;
        } else if id == pl.cb_system_chat {
            match chat::system_chat(&p.body, &self.lang) {
                Ok((line, overlay)) => self.chat(if overlay { "actionbar" } else { "system" }, line),
                Err(e) => self.log.info(format!("unreadable system chat ({e})")),
            }
        } else if id == pl.cb_player_chat {
            match chat::player_chat(&p.body, &self.chat_types, &self.lang) {
                Ok(line) => self.chat("player", line),
                Err(e) => self.log.info(format!("unreadable player chat ({e})")),
            }
        } else if id == pl.cb_disguised_chat {
            match chat::disguised_chat(&p.body, &self.chat_types, &self.lang) {
                Ok(line) => self.chat("player", line),
                Err(e) => self.log.info(format!("unreadable chat ({e})")),
            }
        } else if id == pl.cb_set_title_text || id == pl.cb_set_subtitle_text || id == pl.cb_set_action_bar_text {
            let kind = if id == pl.cb_set_title_text {
                "title"
            } else if id == pl.cb_set_subtitle_text {
                "subtitle"
            } else {
                "actionbar"
            };
            let line = chat::read_text(&mut r, &self.lang)?;
            if !line.is_empty() {
                self.chat(kind, line);
            }
        } else if id == pl.cb_disconnect {
            let reason = chat::read_text(&mut r, &self.lang)?;
            self.chat("disconnect", reason.clone());
            return Ok(Some(self.ended(format!("disconnected by the server: {reason}"))));
        } else if id == pl.cb_start_configuration {
            self.send(pl.sb_configuration_acknowledged, &[])?;
            self.set_phase(Phase::Configuration);
            self.joined_at = None;
            self.position_seen = false;
            self.loaded_sent = false;
            let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
            g.ready = false;
            g.menu = None;
            drop(g);
            self.finish_goal(Walk::Failed { reason: "the server reconfigured the connection".into(), at: self.body.pos });
            self.log.info("server sent us back to the configuration phase");
        } else if id == pl.cb_resource_pack_push {
            let response = pl.sb_resource_pack;
            self.accept_resource_pack(response, r.uuid()?)?;
        } else if id == pl.cb_cookie_request {
            let mut w = Writer::new();
            w.string(&r.string()?).bool(false);
            self.send(pl.sb_cookie_response, &w.data)?;
        } else if id == pl.cb_set_health {
            let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
            g.health = r.f32()?;
            g.food = r.varint()?;
            g.saturation = r.f32()?;
            if g.health <= 0.0 {
                g.dead = true;
            }
        } else if id == pl.cb_set_experience {
            let _progress = r.f32()?;
            shared.game.lock().unwrap_or_else(|e| e.into_inner()).xp_level = r.varint()?;
        } else if id == pl.cb_game_event {
            let event = r.u8()?;
            let param = r.f32()?;
            if event == 3 {
                shared.game.lock().unwrap_or_else(|e| e.into_inner()).gamemode = param as i32;
            }
        } else if id == pl.cb_player_abilities {
            shared.game.lock().unwrap_or_else(|e| e.into_inner()).abilities = r.u8()?;
        } else if id == pl.cb_player_combat_kill {
            let victim = r.varint()?;
            let message = chat::read_text(&mut r, &self.lang).unwrap_or_default();
            let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
            if victim == g.entity_id {
                g.dead = true;
                g.push_chat("death", if message.is_empty() { "you died".into() } else { message });
            }
        } else if id == pl.cb_set_held_slot {
            let slot = r.varint()?;
            shared.game.lock().unwrap_or_else(|e| e.into_inner()).held = slot.clamp(0, 8) as usize;
        } else if id == pl.cb_open_screen {
            let container = r.varint()?;
            let kind = Registries::name(self.regs.menu, r.varint()?);
            let title = chat::read_text(&mut r, &self.lang).unwrap_or_default();
            self.log.debug(format!("menu {container} opened: {kind} \"{title}\""));
            shared.game.lock().unwrap_or_else(|e| e.into_inner()).menu =
                Some(Menu { id: container, kind, title, slots: Vec::new(), state_id: 0 });
        } else if id == pl.cb_container_set_content {
            let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
            let container = r.varint()?;
            let state = r.varint()?;
            let n = r.prefixed_len()?;
            match self.read_slots(&g, &mut r, n).and_then(|slots| Ok((slots, self.read_slot(&g, &mut r)?))) {
                Ok((slots, carried)) => {
                    g.cursor = carried;
                    if container == 0 {
                        let mut slots = slots;
                        slots.resize(INVENTORY_SLOTS, None);
                        g.inventory = slots;
                        g.inventory_state = state;
                    } else if let Some(menu) = g.menu.as_mut().filter(|m| m.id == container) {
                        menu.slots = slots;
                        menu.state_id = state;
                    }
                }
                Err(e) => {
                    g.item_errors += 1;
                    self.log.info(format!("could not read the items of container {container}: {e}"));
                    if let Some(menu) = g.menu.as_mut().filter(|m| m.id == container) {
                        menu.state_id = state;
                    }
                }
            }
        } else if id == pl.cb_container_set_slot {
            let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
            let container = r.varint()?;
            let state = r.varint()?;
            let slot = r.i16()?;
            match self.read_slot(&g, &mut r) {
                Ok(item) => {
                    if container == -1 {
                        g.cursor = item;
                    } else if container == 0 {
                        if let Some(s) = usize::try_from(slot).ok().filter(|&s| s < INVENTORY_SLOTS) {
                            g.inventory[s] = item;
                        }
                        g.inventory_state = state;
                    } else if let Some(menu) = g.menu.as_mut().filter(|m| m.id == container) {
                        if let Ok(s) = usize::try_from(slot) {
                            if s >= menu.slots.len() {
                                menu.slots.resize(s + 1, None);
                            }
                            menu.slots[s] = item;
                        }
                        menu.state_id = state;
                    }
                }
                Err(e) => {
                    g.item_errors += 1;
                    self.log.info(format!("could not read the item in slot {slot}: {e}"));
                }
            }
        } else if id == pl.cb_set_cursor_item {
            let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
            match self.read_slot(&g, &mut r) {
                Ok(item) => g.cursor = item,
                Err(_) => g.item_errors += 1,
            }
        } else if id == pl.cb_set_player_inventory {
            // Player Inventory numbering: 0-8 hotbar, 9-35 main, 36-39 armor (feet..head), 40 offhand.
            let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
            let slot = r.varint()?;
            match self.read_slot(&g, &mut r) {
                Ok(item) => {
                    let menu_slot = match slot {
                        0..=8 => Some(36 + slot as usize),
                        9..=35 => Some(slot as usize),
                        36..=39 => Some(8 - (slot as usize - 36)),
                        40 => Some(45),
                        _ => None,
                    };
                    if let Some(s) = menu_slot {
                        g.inventory[s] = item;
                    }
                }
                Err(_) => g.item_errors += 1,
            }
        } else if id == pl.cb_container_close {
            let container = r.varint()?;
            let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
            if g.menu.as_ref().is_some_and(|m| m.id == container) {
                g.menu = None;
            }
        } else if id == pl.cb_open_sign_editor {
            let pos = r.position()?;
            let front = r.bool()?;
            shared.game.lock().unwrap_or_else(|e| e.into_inner()).sign_editor = Some((pos, front));
            self.log.info(format!("the server opened a sign editor at {pos:?}"));
        } else if id == pl.cb_level_chunk_with_light {
            let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(e) = g.world.load_chunk(&p.body) {
                self.log.debug(format!("could not read a chunk: {e}"));
            }
        } else if id == pl.cb_forget_level_chunk {
            let _ = shared.game.lock().unwrap_or_else(|e| e.into_inner()).world.forget_chunk(&p.body);
        } else if id == pl.cb_block_update {
            let _ = shared.game.lock().unwrap_or_else(|e| e.into_inner()).world.block_update(&p.body);
        } else if id == pl.cb_section_blocks_update {
            let _ = shared.game.lock().unwrap_or_else(|e| e.into_inner()).world.section_update(&p.body);
        } else if id == pl.cb_chunk_batch_finished {
            // Ask for more: without this acknowledgement the server stops sending chunks.
            let mut w = Writer::new();
            w.f32(10.0);
            self.send(pl.sb_chunk_batch_received, &w.data)?;
        } else if id == pl.cb_add_entity {
            let entity = (|| -> io::Result<Entity> {
                let id = r.varint()?;
                let uuid = r.uuid()?;
                let kind = Registries::name(self.regs.entity_type, r.varint()?);
                let pos = r.vec3d()?;
                let _velocity = r.lp_vec3()?;
                let pitch = r.i8()? as f32 * 360.0 / 256.0;
                let yaw = r.i8()? as f32 * 360.0 / 256.0;
                Ok(Entity { id, uuid, kind, pos, yaw, pitch, custom_name: None, text: None })
            })();
            match entity {
                Ok(e) => {
                    shared.game.lock().unwrap_or_else(|e| e.into_inner()).entities.insert(e.id, e);
                }
                Err(e) => self.log.debug(format!("unreadable entity ({e})")),
            }
        } else if id == pl.cb_remove_entities {
            let n = r.prefixed_len()?;
            let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
            for _ in 0..n {
                let e = r.varint()?;
                g.entities.remove(&e);
            }
        } else if id == pl.cb_move_entity_pos || id == pl.cb_move_entity_pos_rot || id == pl.cb_move_entity_rot {
            let (pos, rot) = (id != pl.cb_move_entity_rot, id != pl.cb_move_entity_pos);
            let _ = self.entity_moved(&mut r, pos, rot, new_layout);
        } else if id == pl.cb_entity_position_sync {
            let _ = self.entity_synced(&mut r, new_layout);
        } else if id == pl.cb_teleport_entity {
            let _ = (|| -> io::Result<()> {
                let id = r.varint()?;
                let pos = r.vec3d()?;
                let _delta = r.vec3d()?;
                let yaw = r.f32()?;
                let pitch = r.f32()?;
                let relatives = r.i32()?;
                let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(e) = g.entities.get_mut(&id) {
                    for (i, (p, new)) in e.pos.iter_mut().zip(pos).enumerate() {
                        *p = if relatives & (1 << i) != 0 { *p + new } else { new };
                    }
                    e.yaw = if relatives & 8 != 0 { e.yaw + yaw } else { yaw };
                    e.pitch = if relatives & 16 != 0 { e.pitch + pitch } else { pitch };
                }
                Ok(())
            })();
        } else if id == pl.cb_set_entity_data {
            let _ = self.entity_data(&mut r);
        } else if id == pl.cb_player_info_update {
            if let Err(e) = self.player_info(&mut r) {
                self.log.debug(format!("unreadable player info ({e})"));
            }
        } else if id == pl.cb_player_info_remove {
            let n = r.prefixed_len()?;
            let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
            for _ in 0..n {
                let uuid = r.uuid()?;
                g.players.remove(&uuid);
            }
        }
        Ok(None)
    }

    fn entity_moved(&self, r: &mut Reader, pos: bool, rot: bool, new_layout: bool) -> io::Result<()> {
        let id = r.varint()?;
        let mut delta = [0i64; 3];
        let mut angles = None;
        if new_layout {
            // 26.3: a rotation-only move starts with on-ground; a move packs on-ground with the
            // delta's step count, then the (linear or stepped) 1/4096-block deltas.
            if !pos {
                r.bool()?;
                angles = Some((r.i8()?, r.i8()?));
            } else {
                let properties = r.varint()? as u32;
                let steps = (properties >> 1) as usize;
                if steps == 0 {
                    delta = [r.i16()? as i64, r.i16()? as i64, r.i16()? as i64];
                } else {
                    if steps > r.remaining() / 7 {
                        return Err(invalid("too many movement steps"));
                    }
                    for _ in 0..steps {
                        for d in &mut delta {
                            *d += r.i16()? as i64;
                        }
                        r.varint()?;
                    }
                }
                if rot {
                    angles = Some((r.i8()?, r.i8()?));
                }
            }
        } else {
            if pos {
                delta = [r.i16()? as i64, r.i16()? as i64, r.i16()? as i64];
            }
            if rot {
                angles = Some((r.i8()?, r.i8()?));
            }
        }
        let mut g = self.game();
        if let Some(e) = g.entities.get_mut(&id) {
            for (p, d) in e.pos.iter_mut().zip(delta) {
                if d != 0 {
                    *p = ((*p * 4096.0).round() + d as f64) / 4096.0;
                }
            }
            if let Some((yaw, pitch)) = angles {
                e.yaw = yaw as f32 * 360.0 / 256.0;
                e.pitch = pitch as f32 * 360.0 / 256.0;
            }
        }
        Ok(())
    }

    fn entity_synced(&self, r: &mut Reader, new_layout: bool) -> io::Result<()> {
        let id = r.varint()?;
        let pos = if new_layout {
            // 26.3: a path: kind 1 = timed steps (take the last), else one position.
            if r.varint()? == 1 {
                let n = r.prefixed_len()?;
                let mut last = None;
                for _ in 0..n {
                    last = Some(r.vec3d()?);
                    r.varint()?;
                }
                last
            } else {
                Some(r.vec3d()?)
            }
        } else {
            let p = r.vec3d()?;
            r.vec3d()?; // motion
            Some(p)
        };
        let yaw = r.f32()?;
        let pitch = r.f32()?;
        let mut g = self.game();
        if let Some(e) = g.entities.get_mut(&id) {
            if let Some(p) = pos {
                e.pos = p;
            }
            e.yaw = yaw;
            e.pitch = pitch;
        }
        Ok(())
    }

    /// Entity data: pick out the custom name (index 2) and a text display's text (index 23),
    /// stepping over the entries before them. Stops at a serializer it can't step over.
    fn entity_data(&self, r: &mut Reader) -> io::Result<()> {
        let id = r.varint()?;
        let mut g = self.game();
        let is_text_display = g.entities.get(&id).is_some_and(|e| e.kind == "text_display");
        let modern = self.ids.protocol >= 777;
        loop {
            let index = r.u8()?;
            if index == 0xFF {
                break;
            }
            let serializer = r.varint()?;
            let mut value: Option<String> = None;
            match serializer {
                0 | 8 => {
                    r.u8()?;
                }
                1 | 12 | 14 | 15 | 19 | 20 => {
                    r.varint()?;
                }
                2 => {
                    r.varlong()?;
                }
                3 => {
                    r.f32()?;
                }
                4 => {
                    r.string()?;
                }
                5 => value = Some(chat::read_text(r, &self.lang)?),
                6 => {
                    if r.bool()? {
                        value = Some(chat::read_text(r, &self.lang)?);
                    }
                }
                7 => {
                    let cx = self.item_context(&g.dynamic);
                    item::read_slot(r, &cx)?;
                }
                9 => {
                    r.take(12)?;
                }
                10 => {
                    r.i64()?;
                }
                11 => {
                    if r.bool()? {
                        r.i64()?;
                    }
                }
                13 => {
                    if r.bool()? {
                        r.uuid()?;
                    }
                }
                18 => {
                    r.varint()?;
                    r.varint()?;
                    r.varint()?;
                }
                21..=32 | 35..=38 | 42 | 43 if modern => {
                    r.varint()?;
                }
                33 if modern => {
                    if r.bool()? {
                        r.string()?;
                        r.i64()?;
                    }
                }
                39 if modern => {
                    r.take(12)?;
                }
                40 if modern => {
                    r.take(16)?;
                }
                _ => break, // can't step over it: the rest of the list is unreadable
            }
            if let Some(e) = g.entities.get_mut(&id) {
                if index == 2 && matches!(serializer, 6) {
                    e.custom_name = value;
                } else if index == 23 && is_text_display && serializer == 5 {
                    e.text = value;
                }
            }
        }
        Ok(())
    }

    fn player_info(&self, r: &mut Reader) -> io::Result<()> {
        let actions = r.u8()?;
        let n = r.prefixed_len()?;
        let mut g = self.game();
        for _ in 0..n {
            let uuid = r.uuid()?;
            let mut e = g.players.get(&uuid).cloned().unwrap_or_default();
            if actions & 1 != 0 {
                e.name = r.string()?;
                let props = r.prefixed_len()?;
                for _ in 0..props {
                    r.string()?;
                    r.string()?;
                    if r.bool()? {
                        r.string()?;
                    }
                }
            }
            if actions & 2 != 0 && r.bool()? {
                r.uuid()?;
                r.i64()?;
                let key = r.prefixed_len()?;
                r.take(key)?;
                let sig = r.prefixed_len()?;
                r.take(sig)?;
            }
            if actions & 4 != 0 {
                e.gamemode = Some(r.varint()?);
            }
            if actions & 8 != 0 {
                e.listed = Some(r.bool()?);
            }
            if actions & 16 != 0 {
                e.latency = Some(r.varint()?);
            }
            if actions & 32 != 0 {
                e.display_name = if r.bool()? { Some(chat::read_text(r, &self.lang)?) } else { None };
            }
            if actions & 64 != 0 {
                r.varint()?;
            }
            if actions & 128 != 0 {
                r.bool()?;
            }
            if uuid == g.uuid
                && let Some(gm) = e.gamemode
            {
                g.gamemode = gm;
            }
            g.players.insert(uuid, e);
        }
        Ok(())
    }

    /// Player Position: the server moved us (spawn, /tp, or a correction).
    fn teleported(&mut self, r: &mut Reader) -> io::Result<()> {
        let teleport = r.varint()?;
        let pos = r.vec3d()?;
        let delta = r.vec3d()?;
        let yaw = r.f32()?;
        let pitch = r.f32()?;
        let relatives = r.i32()?;
        for i in 0..3 {
            self.body.pos[i] = if relatives & (1 << i) != 0 { self.body.pos[i] + pos[i] } else { pos[i] };
            self.body.vel[i] = if relatives & (32 << i) != 0 { self.body.vel[i] + delta[i] } else { delta[i] };
        }
        self.yaw = if relatives & 8 != 0 { self.yaw + yaw } else { yaw };
        self.pitch = if relatives & 16 != 0 { self.pitch + pitch } else { pitch };
        self.body.on_ground = false;
        // 26.3 also states where the client ended up; older versions only the teleport's id.
        let mut w = Writer::new();
        w.varint(teleport);
        if self.ids.protocol >= 777 {
            w.f64(self.body.pos[0]).f64(self.body.pos[1]).f64(self.body.pos[2]).f32(self.yaw).f32(self.pitch);
        }
        self.send(self.ids.play.sb_accept_teleportation, &w.data)?;
        // Vanilla follows the confirmation with its full position.
        self.send_position(true)?;
        if self.goal.is_some() && self.position_seen {
            self.corrections.push_back(self.tick);
        }
        self.position_seen = true;
        if !self.loaded_sent {
            self.send(self.ids.play.sb_player_loaded, &[])?;
            self.loaded_sent = true;
        }
        let mut g = self.game();
        g.pos = self.body.pos;
        g.yaw = self.yaw;
        g.pitch = self.pitch;
        Ok(())
    }

    fn accept_resource_pack(&mut self, response_id: i32, uuid: u128) -> io::Result<()> {
        self.log.debug(format!("resource pack {} pushed; reporting it loaded", uuid_string(uuid)));
        for result in [3, 4, 0] {
            // ACCEPTED, DOWNLOADED, SUCCESSFULLY_LOADED
            let mut w = Writer::new();
            w.uuid(uuid).varint(result);
            self.send(response_id, &w.data)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------------
    // Ticking: movement and physics

    fn game_tick(&mut self) -> io::Result<()> {
        if self.phase() != Phase::Play || !self.position_seen {
            return Ok(());
        }
        self.tick += 1;
        let shared = self.shared.clone();
        let mut g = shared.game.lock().unwrap_or_else(|e| e.into_inner());
        let flying = g.abilities & 2 != 0;
        let mut input = Input { flying, ..Input::default() };
        let mut finished = None;
        if let Some(goal) = self.goal.as_mut() {
            if self.tick - goal.started > goal.timeout_ticks {
                finished = Some(Walk::Failed { reason: "timed out".into(), at: self.body.pos });
            } else {
                let pos = self.body.pos;
                let final_index = goal.path.len().saturating_sub(1);
                let mut waypoint = goal.path.get(goal.next).copied().unwrap_or(goal.target);
                let close = |w: [f64; 3], last: bool| {
                    let d = (w[0] - pos[0]).hypot(w[2] - pos[2]);
                    d < if last { 0.2 } else { 0.35 } && (w[1] - pos[1]).abs() < 1.3
                };
                while goal.next < final_index && close(waypoint, false) {
                    goal.next += 1;
                    waypoint = goal.path[goal.next];
                }
                let to_target = (goal.target[0] - pos[0]).hypot(goal.target[2] - pos[2]);
                let y_ok = !goal.has_y || (goal.target[1] - pos[1]).abs() < 1.3;
                if to_target < 0.2 && y_ok && (goal.path.is_empty() || goal.next >= final_index) {
                    finished = Some(Walk::Arrived { at: pos });
                } else {
                    let (dx, dz) = (waypoint[0] - pos[0], waypoint[2] - pos[2]);
                    let len = dx.hypot(dz);
                    if len > 1e-6 {
                        input.dir = Some([dx / len, dz / len]);
                        self.yaw = (-dx).atan2(dz).to_degrees() as f32;
                    }
                    let speed = if goal.sprint && !g.sneaking {
                        physics::SPRINT_SPEED
                    } else if g.sneaking {
                        physics::SNEAK_SPEED
                    } else {
                        physics::WALK_SPEED
                    };
                    // Don't overshoot the last stretch.
                    input.speed = if flying { physics::FLY_SPEED.min(len) } else { speed.min(len.max(0.05)) };
                    if flying {
                        let dy = waypoint[1] - pos[1];
                        input.fly_vertical = if goal.has_y || goal.next > 0 { dy.clamp(-0.4, 0.4) } else { 0.0 };
                    }
                    let climb = waypoint[1] > pos[1] + 0.5 && len < 1.6;
                    input.jump = !flying && self.body.on_ground && (climb || self.body.horizontal_collision);
                    // Progress: the distance to the target must keep shrinking.
                    let dist = to_target + if goal.has_y { (goal.target[1] - pos[1]).abs() } else { 0.0 };
                    if dist < goal.best - 0.1 {
                        goal.best = dist;
                        goal.last_progress = self.tick;
                    } else if self.tick - goal.last_progress > 50 {
                        if goal.repathed {
                            finished = Some(Walk::Failed { reason: "stuck: no progress".into(), at: pos });
                        } else {
                            goal.repathed = true;
                            goal.last_progress = self.tick;
                            if let Some(path) = physics::find_path(&g.world, pos, goal.target, goal.has_y, 6000) {
                                goal.path = path;
                                goal.next = 0;
                            }
                        }
                    }
                    while self.corrections.front().is_some_and(|&t| self.tick - t > 60) {
                        self.corrections.pop_front();
                    }
                    if self.corrections.len() >= 5 {
                        finished = Some(Walk::Failed {
                            reason: "the server kept correcting our position (blocked or anti-cheat)".into(),
                            at: pos,
                        });
                    }
                }
            }
        }
        if self.jump_once {
            input.jump = self.body.on_ground || flying;
            if flying {
                input.fly_vertical = 0.4;
            }
            self.jump_once = false;
        }
        let dead = g.dead;
        if !dead {
            physics::step(&g.world, &mut self.body, &input);
        }
        g.pos = self.body.pos;
        g.yaw = self.yaw;
        g.pitch = self.pitch;
        g.on_ground = self.body.on_ground;
        let sneaking = g.sneaking;
        let sprinting_wanted = self.goal.as_ref().is_some_and(|goal| goal.sprint) && input.dir.is_some() && !sneaking;
        let was_sprinting = g.sprinting;
        g.sprinting = sprinting_wanted;
        drop(g);
        if let Some(result) = finished {
            self.finish_goal(result);
        }
        if sprinting_wanted != was_sprinting {
            let entity = self.game().entity_id;
            let mut w = Writer::new();
            w.varint(entity).varint(if sprinting_wanted { 1 } else { 2 }).varint(0);
            self.send(self.ids.play.sb_player_command, &w.data)?;
        }
        // Keys held this tick (forward 1, jump 16, shift 32, sprint 64).
        let flags = (input.dir.is_some() as u8) | ((input.jump as u8) << 4) | ((sneaking as u8) << 5) | ((sprinting_wanted as u8) << 6);
        if flags != self.input_flags {
            self.input_flags = flags;
            self.send(self.ids.play.sb_player_input, &[flags])?;
        }
        self.send_position(false)?;
        self.send(self.ids.play.sb_client_tick_end, &[])?;
        self.shared.changed.notify_all();
        Ok(())
    }

    /// Movement packet for this tick, like LocalPlayer.sendPosition: position when it moved (or
    /// every 20 ticks), rotation when it turned, else just the flags when they changed.
    fn send_position(&mut self, full: bool) -> io::Result<()> {
        let pl = &self.ids.play;
        let flags = (self.body.on_ground as u8) | ((self.body.horizontal_collision as u8) << 1);
        let (pos, yaw, pitch) = (self.body.pos, self.yaw, self.pitch);
        self.ticks_since_sent += 1;
        let (moved, turned, flags_changed) = match self.last_sent {
            _ if full => (true, true, true),
            None => (true, true, true),
            Some((p, y, pi, f)) => {
                let d2 = (0..3).map(|i| (pos[i] - p[i]).powi(2)).sum::<f64>();
                (d2 > 4e-8 || self.ticks_since_sent >= 20, y != yaw || pi != pitch, f != flags)
            }
        };
        let mut w = Writer::new();
        let id = if moved && turned {
            w.f64(pos[0]).f64(pos[1]).f64(pos[2]).f32(yaw).f32(pitch).u8(flags);
            pl.sb_move_player_pos_rot
        } else if moved {
            w.f64(pos[0]).f64(pos[1]).f64(pos[2]).u8(flags);
            pl.sb_move_player_pos
        } else if turned {
            w.f32(yaw).f32(pitch).u8(flags);
            pl.sb_move_player_rot
        } else if flags_changed {
            w.u8(flags);
            pl.sb_move_player_status_only
        } else {
            return Ok(());
        };
        if moved {
            self.ticks_since_sent = 0;
        }
        self.last_sent = Some((pos, yaw, pitch, flags));
        self.send(id, &w.data)
    }

    fn finish_goal(&mut self, result: Walk) {
        if self.goal.take().is_some() {
            self.log.debug(format!("walk finished: {result:?}"));
            self.game().walk = result;
            self.shared.changed.notify_all();
        }
    }

    // -----------------------------------------------------------------------------------------
    // Commands

    fn hand(offhand: bool) -> i32 {
        offhand as i32
    }

    fn next_sequence(&mut self) -> i32 {
        self.sequence += 1;
        self.sequence
    }

    /// player_action ids: 26.3 inserted "change destroy direction" at 1, moving the later ones up.
    fn action_id(&self, action_777: i32) -> i32 {
        if self.ids.protocol >= 777 || action_777 == 0 { action_777 } else { action_777 - 1 }
    }

    fn look_at(&mut self, target: [f64; 3]) -> io::Result<()> {
        let eye = [self.body.pos[0], self.body.pos[1] + physics::EYE_HEIGHT, self.body.pos[2]];
        let (yaw, pitch) = physics::look_angles(eye, target);
        self.yaw = yaw;
        self.pitch = pitch;
        self.send_position(false)
    }

    fn swing(&mut self) -> io::Result<()> {
        let pl = &self.ids.play;
        if pl.sb_punch >= 0 {
            self.send(pl.sb_punch, &[])
        } else {
            let id = pl.sb_swing;
            self.send(id, &[0])
        }
    }

    fn command(&mut self, cmd: Cmd) -> io::Result<()> {
        if matches!(cmd, Cmd::Disconnect) {
            self.quit = true;
            return Ok(());
        }
        if self.phase() != Phase::Play || !self.position_seen {
            self.log.info(format!("not in the game yet; ignoring {cmd:?}"));
            return Ok(());
        }
        let pl = &self.ids.play;
        match cmd {
            Cmd::Disconnect => {}
            Cmd::Command(c) => {
                self.log.info(format!("> /{c}"));
                let mut w = Writer::new();
                w.string(&c);
                self.send(pl.sb_chat_command, &w.data)?;
            }
            Cmd::Chat(m) => {
                self.log.info(format!("> {m}"));
                let mut w = Writer::new();
                let salt = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos() as i64) ^ 0x5DEE_CE66;
                // Unsigned (offline mode), with an empty last-seen update and checksum 0 = unchecked.
                w.string(&m).i64(now_ms()).i64(salt).bool(false).varint(0).bytes(&[0, 0, 0]).u8(0);
                self.send(pl.sb_chat, &w.data)?;
            }
            Cmd::Look { yaw, pitch } => {
                self.yaw = yaw;
                self.pitch = pitch.clamp(-90.0, 90.0);
                self.send_position(false)?;
            }
            Cmd::LookAt(target) => self.look_at(target)?,
            Cmd::WalkTo { id, target, has_y, sprint, timeout } => {
                let path = {
                    let g = self.game();
                    physics::find_path(&g.world, self.body.pos, target, has_y, 20_000).unwrap_or_default()
                };
                self.log.info(format!(
                    "walking to {:.1} {:.1} {:.1} ({} waypoints)",
                    target[0],
                    target[1],
                    target[2],
                    path.len()
                ));
                self.finish_goal(Walk::Failed { reason: "replaced by a new walk".into(), at: self.body.pos });
                self.corrections.clear();
                self.goal = Some(Goal {
                    target,
                    has_y,
                    sprint,
                    path,
                    next: 0,
                    started: self.tick,
                    timeout_ticks: (timeout.as_millis() / 50) as u64,
                    best: f64::MAX,
                    last_progress: self.tick,
                    repathed: false,
                });
                let mut g = self.game();
                g.walk_id = id;
                g.walk = Walk::Walking { target };
            }
            Cmd::Stop => self.finish_goal(Walk::Failed { reason: "stopped".into(), at: self.body.pos }),
            Cmd::Jump => self.jump_once = true,
            Cmd::Sneak(on) => self.game().sneaking = on,
            Cmd::SelectSlot(slot) => {
                let slot = slot.min(8);
                let mut w = Writer::new();
                w.i16(slot as i16);
                self.send(pl.sb_set_carried_item, &w.data)?;
                self.game().held = slot;
            }
            Cmd::UseItem { offhand } => {
                let seq = self.next_sequence();
                let mut w = Writer::new();
                w.varint(Self::hand(offhand)).varint(seq).f32(self.yaw).f32(self.pitch);
                self.send(pl.sb_use_item, &w.data)?;
                self.swing()?;
            }
            Cmd::UseBlock { pos, face, offhand } => {
                self.look_at([pos[0] as f64 + 0.5, pos[1] as f64 + 0.5, pos[2] as f64 + 0.5])?;
                let seq = self.next_sequence();
                let mut w = Writer::new();
                w.varint(Self::hand(offhand)).position(pos).varint(face as i32);
                w.f32(0.5).f32(0.5).f32(0.5).bool(false).bool(false).varint(seq);
                self.send(pl.sb_use_item_on, &w.data)?;
                self.swing()?;
            }
            Cmd::Dig { pos, face } => {
                // Start, then stop at once: the server finishes the break itself once enough time
                // has passed ("delayed destroy"); creative and instant blocks break on start.
                self.look_at([pos[0] as f64 + 0.5, pos[1] as f64 + 0.5, pos[2] as f64 + 0.5])?;
                for action in [0, 3] {
                    let seq = self.next_sequence();
                    let mut w = Writer::new();
                    w.varint(self.action_id(action)).position(pos).u8(face).varint(seq);
                    self.send(pl.sb_player_action, &w.data)?;
                    self.swing()?;
                }
            }
            Cmd::Attack(id) => {
                let target = self.game().entities.get(&id).map(|e| e.pos);
                if let Some(target) = target {
                    self.look_at([target[0], target[1] + 1.0, target[2]])?;
                }
                let sneaking = self.game().sneaking;
                let mut w = Writer::new();
                if pl.sb_attack >= 0 {
                    w.varint(id);
                    self.send(pl.sb_attack, &w.data)?;
                } else {
                    w.varint(id).varint(1).bool(sneaking); // 1.21.11: interact with action ATTACK
                    self.send(pl.sb_interact, &w.data)?;
                }
                self.swing()?;
            }
            Cmd::Interact { id, offhand } => {
                let target = self.game().entities.get(&id).map(|e| e.pos);
                if let Some(target) = target {
                    self.look_at([target[0], target[1] + 1.0, target[2]])?;
                }
                let sneaking = self.game().sneaking;
                let hand = Self::hand(offhand);
                if self.ids.protocol >= 775 {
                    // entity, hand, where on it (LpVec3; one zero byte = its origin), sneaking
                    let mut w = Writer::new();
                    w.varint(id).varint(hand).u8(0).bool(sneaking);
                    self.send(pl.sb_interact, &w.data)?;
                } else {
                    // 1.21.11, like its client: INTERACT_AT, then INTERACT.
                    let mut at = Writer::new();
                    at.varint(id).varint(2).f32(0.0).f32(0.0).f32(0.0).varint(hand).bool(sneaking);
                    self.send(pl.sb_interact, &at.data)?;
                    let mut w = Writer::new();
                    w.varint(id).varint(0).varint(hand).bool(sneaking);
                    self.send(pl.sb_interact, &w.data)?;
                }
                self.swing()?;
            }
            Cmd::Swing => self.swing()?,
            Cmd::Click { slot, button, mode } => {
                let (container, state) = {
                    let g = self.game();
                    g.menu.as_ref().map_or((0, g.inventory_state), |m| (m.id, m.state_id))
                };
                // No changed-slot predictions and no carried item: the server does the click and
                // sends back what really changed.
                let mut w = Writer::new();
                w.varint(container).varint(state).i16(slot).i8(button).varint(mode).varint(0).bool(false);
                self.send(pl.sb_container_click, &w.data)?;
            }
            Cmd::CloseMenu => {
                let container = self.game().menu.as_ref().map_or(0, |m| m.id);
                let mut w = Writer::new();
                w.varint(container);
                self.send(pl.sb_container_close, &w.data)?;
                self.game().menu = None;
            }
            Cmd::Drop { all } => {
                let mut w = Writer::new();
                w.varint(self.action_id(if all { 4 } else { 5 })).position([0, 0, 0]).u8(0).varint(0);
                self.send(pl.sb_player_action, &w.data)?;
            }
            Cmd::SwapHands => {
                let mut w = Writer::new();
                w.varint(self.action_id(7)).position([0, 0, 0]).u8(0).varint(0);
                self.send(pl.sb_player_action, &w.data)?;
            }
            Cmd::Respawn => {
                self.send(pl.sb_client_command, &[0])?;
                self.game().dead = false;
            }
            Cmd::Sign { lines } => {
                let Some((pos, front)) = self.game().sign_editor.take() else {
                    self.log.info("no sign editor is open");
                    return Ok(());
                };
                let mut w = Writer::new();
                w.position(pos);
                if self.ids.protocol >= 777 {
                    lines.iter().for_each(|l| {
                        w.string(l);
                    });
                    w.varint(front as i32);
                } else {
                    w.bool(front);
                    lines.iter().for_each(|l| {
                        w.string(l);
                    });
                }
                self.send(pl.sb_sign_update, &w.data)?;
            }
        }
        self.shared.changed.notify_all();
        Ok(())
    }
}
