//! Command-line parsing.

use crate::client::Options;
use crate::ids;
use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "\
mcclient - headless Minecraft: Java Edition client (offline mode)

USAGE:
    mcclient <host[:port]> <username> \"<cmd1>||<cmd2>||...\" <seconds> [OPTIONS]
    mcclient mcp [--server <host[:port]>] [--name <username>] [MCP OPTIONS]

    One-shot mode joins the server as <username> (offline mode, no Mojang/Microsoft login),
    sends each command (typed without the leading /) once in the game, prints every chat
    message it receives to stdout, and disconnects <seconds> after joining. Progress goes to
    stderr.

    `mcp` runs a Model Context Protocol server on stdin/stdout that keeps a bot connected and
    lets an AI agent drive it with tools: chat, commands, walking, looking, using items and
    blocks, digging, entities, inventory, menus (chest GUIs), blocks. Register it with e.g.
        claude mcp add mcclient -- /path/to/mcclient mcp --server localhost:25565 --name Bot

MCP OPTIONS:
    --server <ADDR>      default server for `connect` (and to connect on first use)
    --name <NAME>        default username
    --view-distance <N>  chunks around the bot to load [default: 6]
    --protocol, --join-timeout, --lang, -v   as below

ARGS:
    <host[:port]>   server or proxy address; port defaults to 25565 (IPv6: [::1]:25565)
    <username>      the player name (offline mode: 1-16 chars, no spaces; e.g. Bot)
    <commands>      commands separated by ||, without the leading / (\"\" for none)
    <seconds>       how long to stay connected after joining (decimals allowed)

OPTIONS:
    --protocol <N>       protocol to speak instead of asking the server (status ping);
                         one of: PROTOCOLS
    --delay-ms <N>       pause between commands [default: 500]
    --join-timeout <S>   give up if not in the game after S seconds [default: 30]
    --lang <FILE>        an en_us.json to translate every message key (a built-in subset
                         covers chat, common command feedback and disconnect reasons)
    -v, --verbose        log every packet id sent and received (stderr)
    -h, --help           print this help
    -V, --version        print the version

EXIT STATUS:
    0  joined and stayed for <seconds>
    1  could not join (refused, kicked while joining, timed out, connection failed)
    2  bad arguments
    3  joined, then got disconnected / lost the connection

EXAMPLE:
    mcclient localhost:25565 Bot \"help||list\" 10
";

pub fn usage() -> String {
    USAGE.replace("PROTOCOLS", &ids::supported_list())
}

pub enum Parsed {
    Run(Options),
    Mcp(crate::mcp::Defaults, Option<PathBuf>),
    Help,
    Version,
}

/// `mcclient mcp [options]`.
fn parse_mcp(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut d = crate::mcp::Defaults {
        port: 25565,
        view_distance: 6,
        join_timeout: Duration::from_secs(30),
        ..Default::default()
    };
    let mut lang = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline.clone().or_else(|| args.next()).ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-v" | "--verbose" => d.verbose = true,
            "--server" => {
                let (host, port) = parse_address(&value("--server")?)?;
                d.host = Some(host);
                d.port = port;
            }
            "--name" => {
                let name = value("--name")?;
                check_username(&name)?;
                d.username = Some(name);
            }
            "--protocol" => {
                let v = value("--protocol")?;
                let p: i32 = v.parse().map_err(|_| format!("bad --protocol \"{v}\""))?;
                if ids::for_protocol(p).is_none() {
                    return Err(format!("protocol {p} is not supported (supported: {})", ids::supported_list()));
                }
                d.protocol = Some(p);
            }
            "--view-distance" => {
                let v = value("--view-distance")?;
                d.view_distance = match v.parse::<i8>() {
                    Ok(n) if (2..=32).contains(&n) => n,
                    _ => return Err(format!("--view-distance must be 2-32, not \"{v}\"")),
                };
            }
            "--join-timeout" => {
                let v = value("--join-timeout")?;
                d.join_timeout = seconds(&v).map_err(|e| format!("--join-timeout: {e}"))?;
            }
            "--lang" => lang = Some(PathBuf::from(value("--lang")?)),
            _ => return Err(format!("unknown mcp option {arg}")),
        }
    }
    Ok(Parsed::Mcp(d, lang))
}

/// `host`, `host:port`, `[v6]`, `[v6]:port`, or a bare IPv6 address.
pub fn parse_address(s: &str) -> Result<(String, u16), String> {
    let port_of = |p: &str| -> Result<u16, String> {
        match p.parse::<u16>() {
            Ok(port) if port > 0 => Ok(port),
            _ => Err(format!("bad port \"{p}\" in \"{s}\"")),
        }
    };
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| format!("missing ] in \"{s}\""))?;
        match after.strip_prefix(':') {
            Some(p) => (host.to_string(), port_of(p)?),
            None if after.is_empty() => (host.to_string(), 25565),
            None => return Err(format!("bad address \"{s}\"")),
        }
    } else if s.matches(':').count() == 1 {
        let (host, p) = s.split_once(':').unwrap();
        (host.to_string(), port_of(p)?)
    } else {
        (s.to_string(), 25565)
    };
    if host.is_empty() {
        return Err(format!("no host in \"{s}\""));
    }
    Ok((host, port))
}

/// Vanilla's StringUtil.isValidPlayerName: at most 16 characters, no spaces or control/non-ASCII.
pub fn check_username(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().count() > 16 {
        return Err(format!("username \"{name}\" must be 1-16 characters"));
    }
    if let Some(c) = name.chars().find(|&c| c <= ' ' || c >= '\u{7f}') {
        return Err(format!("username \"{name}\" contains {c:?}; use letters, digits and _"));
    }
    Ok(())
}

/// Split on `||`, trim, drop one leading `/`, skip empties.
pub fn parse_commands(s: &str) -> Result<Vec<String>, String> {
    let mut commands = Vec::new();
    for part in s.split("||") {
        let part = part.trim();
        let command = part.strip_prefix('/').unwrap_or(part).trim();
        if command.is_empty() {
            continue;
        }
        // The vanilla server disconnects a player for these ("Illegal characters in chat").
        if let Some(c) = command.chars().find(|&c| c == '§' || c < ' ' || c == '\u{7f}') {
            return Err(format!("command \"{command}\" contains {c:?}, which the server rejects"));
        }
        commands.push(command.to_string());
    }
    Ok(commands)
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut args = args.into_iter().peekable();
    if args.peek().map(String::as_str) == Some("mcp") {
        args.next();
        return parse_mcp(args);
    }
    let mut positional = Vec::new();
    let mut protocol = None;
    let mut delay = Duration::from_millis(500);
    let mut join_timeout = Duration::from_secs(30);
    let mut lang = None;
    let mut verbose = false;
    let mut options_done = false;
    while let Some(arg) = args.next() {
        if options_done || !arg.starts_with('-') || arg == "-" {
            positional.push(arg);
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            "--" => options_done = true,
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "-v" | "--verbose" => verbose = true,
            "--protocol" => {
                let v = value("--protocol")?;
                let p: i32 = v.parse().map_err(|_| format!("bad --protocol \"{v}\""))?;
                if ids::for_protocol(p).is_none() {
                    return Err(format!(
                        "protocol {p} is not supported (supported: {})",
                        ids::supported_list()
                    ));
                }
                protocol = Some(p);
            }
            "--delay-ms" => {
                let v = value("--delay-ms")?;
                delay = Duration::from_millis(v.parse().map_err(|_| format!("bad --delay-ms \"{v}\""))?);
            }
            "--join-timeout" => {
                let v = value("--join-timeout")?;
                join_timeout = seconds(&v).map_err(|e| format!("--join-timeout: {e}"))?;
            }
            "--lang" => lang = Some(PathBuf::from(value("--lang")?)),
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    let [address, username, commands, secs] = <[String; 4]>::try_from(positional).map_err(|p| {
        format!("expected 4 arguments (address, username, commands, seconds), got {}", p.len())
    })?;
    let (host, port) = parse_address(&address)?;
    check_username(&username)?;
    let commands = parse_commands(&commands)?;
    let seconds = seconds(&secs).map_err(|e| format!("<seconds>: {e}"))?.as_secs_f64();
    Ok(Parsed::Run(Options {
        host,
        port,
        username,
        commands,
        seconds,
        protocol,
        delay,
        join_timeout,
        lang,
        verbose,
    }))
}

fn seconds(s: &str) -> Result<Duration, String> {
    match s.parse::<f64>() {
        Ok(v) if (0.0..1e9).contains(&v) => Ok(Duration::from_secs_f64(v)),
        _ => Err(format!("\"{s}\" is not a number of seconds")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Options, String> {
        match parse(args.iter().map(|s| s.to_string()))? {
            Parsed::Run(o) => Ok(o),
            _ => Err("not a run".into()),
        }
    }

    #[test]
    fn the_documented_example() {
        let o = run(&["localhost:25565", "Bot", "help||list", "10"]).unwrap();
        assert_eq!((o.host.as_str(), o.port), ("localhost", 25565));
        assert_eq!(o.username, "Bot");
        assert_eq!(o.commands, ["help", "list"]);
        assert_eq!(o.seconds, 10.0);
        assert_eq!(o.protocol, None);
        assert_eq!(o.delay, Duration::from_millis(500));
    }

    #[test]
    fn options_anywhere() {
        let o = run(&["--protocol", "775", "mc.local", "Bot_1", "/say hi || /tp @s 0 64 0 ||", "2.5", "-v", "--delay-ms=100"]).unwrap();
        assert_eq!((o.host.as_str(), o.port), ("mc.local", 25565));
        assert_eq!(o.commands, ["say hi", "tp @s 0 64 0"]);
        assert_eq!(o.seconds, 2.5);
        assert_eq!(o.protocol, Some(775));
        assert!(o.verbose);
        assert_eq!(o.delay, Duration::from_millis(100));
    }

    #[test]
    fn addresses() {
        assert_eq!(parse_address("[::1]:25566").unwrap(), ("::1".into(), 25566));
        assert_eq!(parse_address("[::1]").unwrap(), ("::1".into(), 25565));
        assert_eq!(parse_address("::1").unwrap(), ("::1".into(), 25565));
        assert!(parse_address("host:0").is_err());
        assert!(parse_address("host:abc").is_err());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(run(&["h", "Probe", "list"]).is_err());
        assert!(run(&["h", "has space", "list", "1"]).is_err());
        assert!(run(&["h", "WayTooLongUsername17", "list", "1"]).is_err());
        assert!(run(&["h", "Probe", "say §chi", "1"]).is_err());
        assert!(run(&["h", "Probe", "list", "-1"]).is_err());
        assert!(run(&["h", "Probe", "list", "1", "--protocol", "760"]).is_err());
        assert!(run(&["h", "Probe", "list", "1", "--bogus"]).is_err());
        assert_eq!(run(&["h", "Probe", "", "1"]).unwrap().commands, Vec::<String>::new());
    }
}
