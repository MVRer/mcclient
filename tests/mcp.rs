//! End-to-end: `mcclient mcp` driven over JSON-RPC (as an MCP client would) while connected to a
//! scripted 26.3 mock server with real chunks (a stone floor and a wall), a player, a named NPC,
//! an inventory and a shop menu.

use mcclient::client::offline_uuid;
use mcclient::ids::V777;
use mcclient::net::{Conn, Reader, Writer};
use mcclient::registries;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

fn w() -> Writer {
    Writer::new()
}

fn nbt_string(s: &str) -> Vec<u8> {
    let mut v = vec![8];
    v.extend((s.len() as u16).to_be_bytes());
    v.extend(s.as_bytes());
    v
}

fn id_of(table: &[&str], name: &str) -> i32 {
    table.iter().position(|n| *n == name).unwrap_or_else(|| panic!("{name} not in registry")) as i32
}

/// level_chunk_with_light (26.1+ layout): 24 single-valued sections, stone below y=16, air above.
fn flat_chunk(cx: i32, cz: i32) -> Vec<u8> {
    let mut sections = w();
    for i in 0..24 {
        let stone = i < 5; // sections 0-4 = y -64..15
        sections.i16(if stone { 4096 } else { 0 }).i16(0); // block count, fluid count
        sections.u8(0).varint(if stone { 1 } else { 0 }); // block states: single value (1 = stone)
        sections.u8(0).varint(0); // biomes: single value
    }
    let mut c = w();
    c.i32(cx).i32(cz).varint(0); // no heightmaps
    c.varint(sections.data.len() as i32).bytes(&sections.data);
    c.varint(0); // block entities
    c.bytes(&[0; 8]); // light data (not read)
    c.data
}

fn slot(item: i32, count: i32) -> Vec<u8> {
    w().varint(count).varint(item).varint(0).varint(0).data.clone()
}

/// A stack with a custom name and lore, like a plugin GUI button.
fn named_slot(item: i32, count: i32, name: &str, lore: &str) -> Vec<u8> {
    let components = registries::for_protocol(777).data_component_type;
    let mut s = w();
    s.varint(count).varint(item).varint(2).varint(0);
    s.varint(id_of(components, "custom_name")).bytes(&nbt_string(name));
    s.varint(id_of(components, "lore")).varint(1).bytes(&nbt_string(lore));
    s.data.clone()
}

struct Mock {
    conn: Conn,
}

impl Mock {
    /// Next packet from the client; None once it hung up.
    fn recv(&mut self, deadline: Instant) -> Option<(i32, Vec<u8>)> {
        while Instant::now() < deadline {
            match self.conn.poll() {
                Ok(Some(p)) => return Some((p.id, p.body)),
                Ok(None) => {}
                Err(_) => return None,
            }
        }
        panic!("mock: timed out");
    }
    fn expect(&mut self, id: i32) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.recv(deadline) {
                Some((i, body)) if i == id => return body,
                Some(_) => {}
                None => panic!("mock: connection closed waiting for 0x{id:02X}"),
            }
        }
    }
    fn send(&mut self, id: i32, body: &[u8]) {
        self.conn.send(id, body).unwrap();
    }
    fn system(&mut self, text: &str) {
        let body = [nbt_string(text), vec![0]].concat();
        self.send(V777.play.cb_system_chat, &body);
    }
}

/// The server side; returns notes of what the client did, for the test to check.
fn mock_server(listener: TcpListener) -> Vec<String> {
    let regs = registries::for_protocol(777);
    let (cb, sb) = (&V777.play, &V777.play);
    let (stream, _) = listener.accept().unwrap();
    let mut m = Mock { conn: Conn::from_stream(stream).unwrap() };
    let mut notes = Vec::new();
    m.expect(0x00); // handshake
    m.expect(0x00); // hello
    m.send(0x02, &w().uuid(offline_uuid("Probe")).string("Probe").varint(0).uuid(1).data);
    m.expect(0x03);
    m.send(V777.cfg.cb_select_known_packs, &w().varint(1).string("minecraft").string("core").string("26.3").data);
    m.expect(V777.cfg.sb_select_known_packs);
    m.send(V777.cfg.cb_finish_configuration, &[]);
    m.expect(V777.cfg.sb_finish_configuration);

    // Join Game: entity 7, overworld (no dimension data: -64..320), creative.
    let mut login = w();
    login.i32(7).bool(false).varint(1).string("minecraft:overworld").varint(20).varint(6).varint(6);
    login.bool(false).bool(true).bool(false);
    login.varint(0).string("minecraft:overworld").i64(0).varint(1).varint(0).bool(false).bool(false);
    login.bool(false).varint(0).varint(63).bool(false).bool(false);
    m.send(cb.cb_login, &login.data);
    for cx in -1..=1 {
        for cz in -1..=1 {
            m.send(cb.cb_level_chunk_with_light, &flat_chunk(cx, cz));
        }
    }
    // A wall at x=3, z=-2..2, two blocks high: walking from x=0.5 to x=5.5 must go around it.
    for z in -2..=2 {
        for y in 16..=17 {
            m.send(cb.cb_block_update, &w().position([3, y, z]).varint(1).data);
        }
    }
    // Spawn on the floor.
    m.send(cb.cb_player_position, &w().varint(1).f64(0.5).f64(16.0).f64(0.5).f64(0.0).f64(0.0).f64(0.0).f32(0.0).f32(0.0).i32(0).data);
    m.expect(sb.sb_accept_teleportation);
    // Tab list: Alex. Entities: Alex (player) and a villager NPC named Shopkeeper.
    let alex = 0xA1E7_u128;
    m.send(cb.cb_player_info_update, &w().u8(1 | 4 | 8).varint(1).uuid(alex).string("Alex").varint(0).varint(0).bool(true).data);
    let player_type = id_of(regs.entity_type, "player");
    let villager_type = id_of(regs.entity_type, "villager");
    m.send(cb.cb_add_entity, &w().varint(100).uuid(alex).varint(player_type).f64(3.5).f64(16.0).f64(6.5).u8(0).u8(0).u8(0).u8(0).varint(0).data);
    m.send(cb.cb_add_entity, &w().varint(101).uuid(0xB0B).varint(villager_type).f64(-3.5).f64(16.0).f64(2.5).u8(0).u8(0).u8(0).u8(0).varint(0).data);
    let mut name = w();
    name.varint(101).u8(0).varint(0).u8(0); // index 0 (byte) first, to be stepped over
    name.u8(2).varint(6).bool(true).bytes(&nbt_string("Shopkeeper")).u8(0xFF);
    m.send(cb.cb_set_entity_data, &name.data);
    // Inventory: a diamond sword in hotbar slot 0 (menu slot 36).
    let sword = id_of(regs.item, "diamond_sword");
    let mut inv = w();
    inv.varint(0).varint(1).varint(46);
    for i in 0..46 {
        inv.bytes(&if i == 36 { slot(sword, 1) } else { vec![0] });
    }
    inv.u8(0);
    m.send(cb.cb_container_set_content, &inv.data);

    // React to what the client does until it leaves.
    let emerald = id_of(regs.item, "emerald");
    let generic_9x3 = id_of(regs.menu, "generic_9x3");
    let deadline = Instant::now() + Duration::from_secs(60);
    while let Some((id, body)) = m.recv(deadline) {
        let mut r = Reader::new(&body);
        if id == sb.sb_chat_command {
            let command = r.string().unwrap();
            notes.push(format!("command {command}"));
            if command == "shop" {
                m.send(cb.cb_open_screen, &[w().varint(1).varint(generic_9x3).data.clone(), nbt_string("Shop")].concat());
                let mut content = w();
                content.varint(1).varint(5).varint(27 + 36);
                for i in 0..63 {
                    content.bytes(&if i == 13 { named_slot(emerald, 3, "§aBuy Emerald", "Price: 5 coins") } else { slot(emerald, 1) });
                }
                content.u8(0);
                m.send(cb.cb_container_set_content, &content.data);
            } else {
                m.system(&format!("Unknown command: {command}"));
            }
        } else if id == sb.sb_container_click {
            let (container, state, clicked) = (r.varint().unwrap(), r.varint().unwrap(), r.i16().unwrap());
            notes.push(format!("click container {container} state {state} slot {clicked}"));
            m.system("You bought an emerald");
            m.send(cb.cb_container_close, &w().varint(1).data);
        } else if id == sb.sb_chat {
            let message = r.string().unwrap();
            notes.push(format!("chat {message}"));
            m.system(&format!("[echo] {message}"));
        } else if id == sb.sb_interact {
            let target = r.varint().unwrap();
            notes.push(format!("interact {target}"));
            if target == 101 {
                m.system("Shopkeeper: welcome!");
            }
        } else if id == sb.sb_move_player_pos || id == sb.sb_move_player_pos_rot {
            let pos = [r.f64().unwrap(), r.f64().unwrap(), r.f64().unwrap()];
            // Never inside the wall.
            let inside_wall = pos[0] > 2.7 && pos[0] < 4.3 && pos[2] > -2.3 && pos[2] < 3.3 && pos[1] < 17.9;
            if inside_wall {
                notes.push(format!("INSIDE WALL at {pos:?}"));
            }
            notes.push(format!("pos {:.2} {:.2} {:.2}", pos[0], pos[1], pos[2]));
        }
    }
    notes
}

// ---------------------------------------------------------------------------------------------
// MCP client side

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl McpClient {
    fn start(addr: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mcclient"))
            .args(["mcp", "--server", addr, "--name", "Probe", "--protocol", "777", "--join-timeout", "10"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        Self { child, stdin, lines, next_id: 1 }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        self.stdin.flush().unwrap();
        let line = self.lines.recv_timeout(Duration::from_secs(90)).expect("no reply from the MCP server");
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    /// tools/call -> (text, is_error); the text parsed as JSON when it is JSON.
    fn call(&mut self, tool: &str, args: Value) -> Value {
        let reply = self.request("tools/call", json!({"name": tool, "arguments": args}));
        let result = &reply["result"];
        let text = result["content"][0]["text"].as_str().unwrap_or_default().to_string();
        assert_eq!(result["isError"], false, "{tool} failed: {text}");
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }
}

#[test]
fn an_agent_drives_the_bot_over_mcp() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let server = thread::spawn(move || mock_server(listener));
    let mut mcp = McpClient::start(&addr);

    let init = mcp.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "mcclient");
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    writeln!(mcp.stdin, "{}", json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).unwrap();
    let tools = mcp.request("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in ["connect", "command", "walk_to", "menu", "click_slot", "interact", "entities", "inventory", "find_blocks"] {
        assert!(names.contains(&expected), "missing tool {expected}: {names:?}");
    }

    let joined = mcp.call("connect", json!({}));
    assert_eq!(joined["status"]["position"], json!([0.5, 16.0, 0.5]), "{joined}");
    assert_eq!(joined["status"]["chunks_loaded"], 9, "{joined}");
    assert_eq!(joined["status"]["gamemode"], "creative", "{joined}");

    let entities = mcp.call("entities", json!({}));
    let labels: Vec<&str> = entities["entities"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert!(labels.contains(&"Shopkeeper") && labels.contains(&"Alex"), "{entities}");
    let players = mcp.call("players", json!({}));
    assert_eq!(players["players"][0]["name"], "Alex", "{players}");

    let inventory = mcp.call("inventory", json!({}));
    assert_eq!(inventory["held_item"]["item"], "diamond_sword", "{inventory}");
    assert_eq!(inventory["slots"][0]["where"], "hotbar_0", "{inventory}");

    let block = mcp.call("block_at", json!({"x": 0, "y": 15, "z": 0}));
    assert_eq!(block["block"], "stone", "{block}");
    let wall = mcp.call("find_blocks", json!({"name": "stone", "radius": 4, "limit": 3}));
    assert_eq!(wall["found"].as_array().unwrap().len(), 3, "{wall}");

    // A plugin-style GUI: open by command, click the emerald.
    let shop = mcp.call("command", json!({"command": "/shop"}));
    assert_eq!(shop["menu"]["title"], "Shop", "{shop}");
    assert_eq!(shop["menu"]["container_slots"], 27, "{shop}");
    let button = shop["menu"]["items"].as_array().unwrap().iter().find(|i| i["slot"] == 13).cloned().unwrap_or_default();
    assert_eq!(button["item"], "emerald", "{shop}");
    assert_eq!(button["custom_name"], "Buy Emerald", "formatting codes stripped: {shop}");
    assert_eq!(button["lore"], json!(["Price: 5 coins"]), "{shop}");
    assert_eq!(shop["menu"]["items"].as_array().unwrap().len(), 27, "every container slot holds an item: {shop}");
    let bought = mcp.call("click_slot", json!({"slot": 13, "mode": "pickup"}));
    assert_eq!(bought["chat"][0], "You bought an emerald", "{bought}");
    assert_eq!(bought["menu"]["open"], false, "the server closed the menu: {bought}");

    let echoed = mcp.call("chat", json!({"message": "hello"}));
    assert_eq!(echoed["chat"][0], "[echo] hello", "{echoed}");
    let npc = mcp.call("interact", json!({"name": "shopkeeper"}));
    assert_eq!(npc["chat"][0], "Shopkeeper: welcome!", "{npc}");

    // Around the wall to the other side.
    let walk = mcp.call("walk_to", json!({"x": 5.5, "z": 0.5, "timeout_s": 20}));
    assert_eq!(walk["result"], "arrived", "{walk}");
    assert!((walk["position"][0].as_f64().unwrap() - 5.5).abs() < 0.3, "{walk}");
    assert!((walk["position"][1].as_f64().unwrap() - 16.0).abs() < 0.01, "stays on the floor: {walk}");

    let status = mcp.call("status", json!({}));
    assert_eq!(status["in_game"], true, "{status}");
    let read = mcp.call("read_chat", json!({"since": 0}));
    assert!(read["lines"].as_array().unwrap().len() >= 3, "{read}");

    assert_eq!(mcp.call("disconnect", json!({})), Value::String("disconnected".into()));
    drop(mcp.stdin);
    let _ = mcp.child.wait();
    let notes = server.join().expect("mock server");
    assert!(notes.contains(&"command shop".to_string()), "{notes:?}");
    assert!(notes.contains(&"click container 1 state 5 slot 13".to_string()), "{notes:?}");
    assert!(notes.contains(&"chat hello".to_string()), "{notes:?}");
    assert!(notes.contains(&"interact 101".to_string()), "{notes:?}");
    assert!(!notes.iter().any(|n| n.starts_with("INSIDE WALL")), "walked through the wall: {notes:?}");
}
