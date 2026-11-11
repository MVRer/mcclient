//! End-to-end: the real `mcclient` binary against a scripted mock server on localhost.
//! The mock speaks the server side of each flow with the ids of the version under test and
//! records what the client sends, so the assertions check both directions.

use mcclient::client::offline_uuid;
use mcclient::net::{Conn, Reader, Writer};
use std::net::TcpListener;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// A tiny NBT encoder for the mock's text components and registry entries.

#[derive(Clone)]
enum N {
    S(String),
    I(i32),
    C(Vec<(String, N)>),
    L(Vec<N>),
}

fn s(v: &str) -> N {
    N::S(v.into())
}
fn c(entries: &[(&str, N)]) -> N {
    N::C(entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

impl N {
    fn kind(&self) -> u8 {
        match self {
            N::S(_) => 8,
            N::I(_) => 3,
            N::C(_) => 10,
            N::L(_) => 9,
        }
    }
    fn payload(&self, out: &mut Vec<u8>) {
        match self {
            N::S(v) => {
                out.extend((v.len() as u16).to_be_bytes());
                out.extend(v.as_bytes());
            }
            N::I(v) => out.extend(v.to_be_bytes()),
            N::C(entries) => {
                for (k, v) in entries {
                    out.push(v.kind());
                    out.extend((k.len() as u16).to_be_bytes());
                    out.extend(k.as_bytes());
                    v.payload(out);
                }
                out.push(0);
            }
            N::L(items) => {
                out.push(items.first().map_or(0, N::kind));
                out.extend((items.len() as i32).to_be_bytes());
                items.iter().for_each(|i| i.payload(out));
            }
        }
    }
    /// Network NBT: type byte, nameless payload.
    fn root(&self) -> Vec<u8> {
        let mut out = vec![self.kind()];
        self.payload(&mut out);
        out
    }
}

// ---------------------------------------------------------------------------------------------
// Mock server connection.

struct Mock {
    conn: Conn,
    /// Every packet the client sent: (id, body).
    got: Vec<(i32, Vec<u8>)>,
}

impl Mock {
    fn accept(listener: &TcpListener) -> Self {
        let (stream, _) = listener.accept().expect("accept");
        Self { conn: Conn::from_stream(stream).unwrap(), got: Vec::new() }
    }

    fn send(&mut self, id: i32, body: &[u8]) {
        self.conn.send(id, body).expect("mock send");
    }

    /// Wait for the next packet with this id (recording, and skipping, any others).
    fn expect(&mut self, id: i32) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let seen: Vec<String> = self.got.iter().map(|(i, _)| format!("0x{i:02X}")).collect();
            assert!(Instant::now() < deadline, "mock: no packet 0x{id:02X} from the client; got {seen:?}");
            match self.conn.poll() {
                Ok(Some(p)) => {
                    self.got.push((p.id, p.body.clone()));
                    if p.id == id {
                        return p.body;
                    }
                }
                Ok(None) => {}
                Err(e) => panic!("mock: connection ended while waiting for 0x{id:02X} ({e}); got {seen:?}"),
            }
        }
    }

    /// Keep reading until the client hangs up.
    fn until_closed(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            match self.conn.poll() {
                Ok(Some(p)) => self.got.push((p.id, p.body)),
                Ok(None) => {}
                Err(_) => return,
            }
        }
        panic!("mock: the client never disconnected");
    }

    /// How many packets with this id the client sent after `got[from]` (ids repeat across states).
    fn sent_since(&self, from: usize, id: i32) -> usize {
        self.got[from..].iter().filter(|(i, _)| *i == id).count()
    }
}

fn w() -> Writer {
    Writer::new()
}

/// Answer one status ping, reporting `protocol`.
fn answer_status(listener: &TcpListener, protocol: i32, name: &str) {
    let mut m = Mock::accept(listener);
    let hs = m.expect(0x00);
    let mut r = Reader::new(&hs);
    let _pinged_with = r.varint().unwrap();
    let (_host, _port, intent) = (r.string().unwrap(), r.u16().unwrap(), r.varint().unwrap());
    assert_eq!(intent, 1, "status intent");
    m.expect(0x00); // status request
    let json = format!(
        r#"{{"version":{{"name":"{name}","protocol":{protocol}}},"players":{{"max":20,"online":0}},"description":{{"text":"mock"}}}}"#
    );
    m.send(0x00, &w().string(&json).data);
}

/// Read the login handshake + hello; check the protocol, name and offline UUID.
fn accept_login(listener: &TcpListener, protocol: i32) -> Mock {
    let mut m = Mock::accept(listener);
    let hs = m.expect(0x00);
    let mut r = Reader::new(&hs);
    assert_eq!(r.varint().unwrap(), protocol, "handshake protocol");
    assert_eq!(r.string().unwrap(), "127.0.0.1");
    let _port = r.u16().unwrap();
    assert_eq!(r.varint().unwrap(), 2, "login intent");
    let hello = m.expect(0x00);
    let mut r = Reader::new(&hello);
    assert_eq!(r.string().unwrap(), "Probe");
    assert_eq!(r.uuid().unwrap(), offline_uuid("Probe"));
    m
}

fn login_finished(m: &mut Mock) {
    let mut b = w();
    b.uuid(offline_uuid("Probe")).string("Probe").varint(0).uuid(42); // profile + session id
    m.send(0x02, &b.data);
    m.expect(0x03); // login acknowledged
}

fn run_client(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_mcclient"))
        .args(args)
        .output()
        .expect("run mcclient");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn listener() -> (TcpListener, String) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("127.0.0.1:{}", l.local_addr().unwrap().port());
    (l, addr)
}

// ---------------------------------------------------------------------------------------------

/// The whole happy path on 26.3 (777): status ping, compressed login with a plugin query,
/// configuration (brand, client info, known packs, chat types, keep-alive, code of conduct),
/// play (teleport confirm, player loaded, two commands, ping/keep-alive) and every chat kind.
#[test]
fn full_session_on_26_3() {
    let (l, addr) = listener();
    let server = thread::spawn(move || {
        answer_status(&l, 777, "Horizon 26.3");
        let mut m = accept_login(&l, 777);

        // Login: compression on, a proxy-style plugin query, then success.
        m.send(0x03, &w().varint(64).data);
        m.conn.set_compression(64);
        m.send(0x04, &w().varint(7).string("velocity:player_info").u8(1).data);
        let answer = m.expect(0x02);
        assert_eq!(answer, w().varint(7).bool(false).data, "plugin query answered 'not understood'");
        login_finished(&mut m);

        // Configuration (777 ids).
        let brand = m.expect(0x02);
        assert_eq!(brand, w().string("minecraft:brand").string("mcclient").data);
        m.expect(0x00); // client information
        m.send(0x0F, &w().varint(1).string("minecraft").string("core").string("26.3").data);
        let packs = m.expect(0x07);
        assert_eq!(packs, w().varint(1).string("minecraft").string("core").string("26.3").data, "known packs echoed");
        let shout = c(&[
            ("chat", c(&[("translation_key", s("%s shouts: %s")), ("parameters", N::L(vec![s("sender"), s("content")]))])),
            ("narration", c(&[("translation_key", s("%s shouts: %s")), ("parameters", N::L(vec![s("sender"), s("content")]))])),
        ]);
        let mut types = w();
        types.string("minecraft:chat_type").varint(3);
        types.string("minecraft:chat").bool(false);
        types.string("minecraft:say_command").bool(false);
        types.string("mock:shout").bool(true).bytes(&shout.root());
        m.send(0x07, &types.data);
        let mut big = w(); // well over the threshold: arrives zlib-compressed
        big.string("minecraft:banner_pattern").varint(40);
        for i in 0..40 {
            big.string(&format!("minecraft:a_rather_long_banner_pattern_name_number_{i}")).bool(false);
        }
        m.send(0x07, &big.data);
        m.send(0x04, &w().i64(12345).data);
        assert_eq!(m.expect(0x04), w().i64(12345).data, "configuration keep-alive echoed");
        m.send(0x14, &w().string("Be nice").data);
        m.expect(0x09); // accept code of conduct
        m.send(0x03, &[]);
        m.expect(0x03); // finish configuration acknowledged

        // Play (777 ids).
        let play_start = m.got.len();
        m.send(0x32, &[0u8; 24]); // Join Game (contents ignored by the client)
        m.send(0x49, &w().varint(9).bytes(&[0u8; 60]).data);
        // 26.3's accept_teleportation also states where the client ended up (position, rotation).
        assert_eq!(m.expect(0x00), w().varint(9).f64(0.0).f64(0.0).f64(0.0).f32(0.0).f32(0.0).data, "teleport confirmed");
        m.expect(0x2C); // player loaded
        assert_eq!(m.expect(0x07), w().string("help").data);
        let help = c(&[("text", s("Help: ")), ("extra", N::L(vec![c(&[("text", s("/list"))])]))]);
        m.send(0x7C, &w().bytes(&help.root()).bool(false).data);
        for _ in 0..2 {
            m.send(0x7C, &w().bytes(&s("§eAction").root()).bool(true).data);
        }
        assert_eq!(m.expect(0x07), w().string("list").data, "leading / stripped");
        let list = c(&[
            ("translate", s("commands.list.players")),
            ("with", N::L(vec![c(&[("", N::I(1))]), c(&[("", N::I(20))]), c(&[("text", s("Probe"))])])),
        ]);
        m.send(0x7C, &w().bytes(&list.root()).bool(false).data);
        for (chat_type, message) in [(1, "hello world"), (3, "hey")] {
            let mut p = w();
            p.varint(0).uuid(7).varint(0).bool(false).string(message).i64(0).i64(0).varint(0);
            p.bool(false).varint(0).varint(chat_type).bytes(&s("Alex").root()).bool(false);
            m.send(0x42, &p.data);
        }
        let mut d = w();
        d.bytes(&s("hi all").root()).varint(2).bytes(&s("Server").root()).bool(false);
        m.send(0x21, &d.data);
        m.send(0x3E, &w().i32(77).data);
        assert_eq!(m.expect(0x2D), w().i32(77).data, "pong");
        m.send(0x2D, &w().i64(999).data);
        assert_eq!(m.expect(0x1C), w().i64(999).data, "play keep-alive echoed");
        m.until_closed();
        assert_eq!(m.sent_since(play_start, 0x07), 2, "exactly two commands");
    });

    let (code, stdout, stderr) = run_client(&[&addr, "Probe", "help||/list", "1.5", "--delay-ms", "100", "--join-timeout", "10"]);
    server.join().expect("mock server checks");
    assert_eq!(code, 0, "stderr:\n{stderr}");
    assert_eq!(
        stdout,
        "Help: /list\n\
         [actionbar] Action\n\
         There are 1 of a max of 20 players online: Probe\n\
         <Alex> hello world\n\
         Alex shouts: hey\n\
         [Server] hi all\n",
        "stderr:\n{stderr}"
    );
    assert!(stderr.contains("server reports \"Horizon 26.3\" (protocol 777)"), "{stderr}");
    assert!(stderr.contains("accepting the server's code of conduct"), "{stderr}");
}

/// 26.1 (775) chosen from the status ping, a proxy-style trip back to configuration, then a kick.
#[test]
fn reconfiguration_then_kick_on_26_1() {
    let (l, addr) = listener();
    let server = thread::spawn(move || {
        answer_status(&l, 775, "Paper 26.1.2");
        let mut m = accept_login(&l, 775);
        login_finished(&mut m);
        m.send(0x0E, &w().varint(1).string("minecraft").string("core").string("26.1.2").data);
        m.expect(0x07); // known packs
        m.send(0x03, &[]);
        m.expect(0x03);
        m.send(0x31, &[0u8; 24]); // Join Game (775)
        m.send(0x48, &w().varint(1).bytes(&[0u8; 60]).data);
        m.expect(0x2C); // player loaded (775)
        assert_eq!(m.expect(0x07), w().string("server lobby").data);
        m.send(0x76, &[]); // start configuration (775)
        m.expect(0x10); // configuration acknowledged
        m.send(0x03, &[]);
        m.expect(0x03);
        m.send(0x31, &[0u8; 24]);
        m.send(0x79, &w().bytes(&s("Welcome to the lobby").root()).bool(false).data); // system chat (775)
        let kick = c(&[("translate", s("multiplayer.disconnect.kicked"))]);
        m.send(0x20, &kick.root());
        m.until_closed();
    });
    let (code, stdout, stderr) = run_client(&[&addr, "Probe", "server lobby", "10", "--join-timeout", "10"]);
    server.join().expect("mock server checks");
    assert_eq!(code, 3, "stderr:\n{stderr}");
    assert_eq!(stdout, "Welcome to the lobby\n[disconnect] Kicked by an operator\n", "stderr:\n{stderr}");
    assert!(stderr.contains("protocol 775 (26.1)"), "{stderr}");
    assert!(stderr.contains("back to the configuration phase"), "{stderr}");
}

/// Refused during login (1.21.11 / 774, forced, so no status ping): the JSON reason is shown.
#[test]
fn refused_during_login_on_1_21_11() {
    let (l, addr) = listener();
    let server = thread::spawn(move || {
        let mut m = accept_login(&l, 774);
        m.send(0x00, &w().string(r#"{"translate":"multiplayer.disconnect.not_whitelisted"}"#).data);
        m.until_closed();
    });
    let (code, stdout, stderr) = run_client(&[&addr, "Probe", "list", "5", "--protocol", "774"]);
    server.join().expect("mock server checks");
    assert_eq!(code, 1, "stderr:\n{stderr}");
    assert_eq!(stdout, "[disconnect] You are not white-listed on this server!\n");
    assert!(stderr.contains("refused during login"), "{stderr}");
    assert!(!stderr.contains("status ping"), "no ping with --protocol: {stderr}");
}

/// A server that stalls in configuration: the client gives up and says where it got stuck.
#[test]
fn reports_a_configuration_stall() {
    let (l, addr) = listener();
    let server = thread::spawn(move || {
        let mut m = accept_login(&l, 777);
        login_finished(&mut m);
        m.send(0x07, &w().string("minecraft:dimension_type").varint(0).data);
        m.until_closed();
    });
    let started = Instant::now();
    let (code, stdout, stderr) = run_client(&[&addr, "Probe", "list", "5", "--protocol", "777", "--join-timeout", "1"]);
    server.join().expect("mock server checks");
    assert_eq!(code, 1, "stderr:\n{stderr}");
    assert_eq!(stdout, "");
    assert!(stderr.contains("stuck in the configuration phase"), "{stderr}");
    assert!(stderr.contains("config 0x07"), "{stderr}");
    assert!(started.elapsed() < Duration::from_secs(5));
}
