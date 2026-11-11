//! One-shot mode: join, send the commands, print every chat line to stdout, leave after a set
//! time. A thin driver over the [`engine`](crate::engine).

use crate::engine::{Bot, Cmd, Config, End, Game, Log};
use crate::text::Lang;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use crate::engine::{offline_uuid, uuid_string};

#[derive(Clone, Debug)]
pub struct Options {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub commands: Vec<String>,
    /// How long to stay connected after joining (commands are sent at the start of it).
    pub seconds: f64,
    /// Force a protocol version instead of asking the server.
    pub protocol: Option<i32>,
    /// Pause between commands.
    pub delay: Duration,
    /// Give up if play isn't reached within this long.
    pub join_timeout: Duration,
    pub lang: Option<PathBuf>,
    pub verbose: bool,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Joined and stayed for the whole window.
    Completed,
    /// Never got into the game (refused, timed out, connection failed...).
    FailedBeforeJoin(String),
    /// Joined, then the server disconnected us or the connection dropped.
    Disconnected(String),
}

fn outcome(end: &End) -> Outcome {
    match end {
        End::NotJoined(m) => Outcome::FailedBeforeJoin(m.clone()),
        End::Left(m) => Outcome::Disconnected(m.clone()),
    }
}

/// Print the chat lines after `printed` (a closed stdout must not stop the session).
fn print_new(g: &Game, printed: &mut u64, out: &mut dyn Write) {
    for line in g.chat_since(*printed) {
        let _ = writeln!(out, "{}", line.display());
        *printed = line.seq;
    }
    let _ = out.flush();
}

/// Connect, join, run the commands, print the chat. Chat lines go to `out`.
pub fn run(o: &Options, lang: Arc<Lang>, out: &mut dyn Write) -> Outcome {
    let log = Log::new(o.verbose);
    let cfg = Config {
        host: o.host.clone(),
        port: o.port,
        username: o.username.clone(),
        protocol: o.protocol,
        view_distance: 2, // fewest chunks: this mode doesn't look at the world
        join_timeout: o.join_timeout,
        verbose: o.verbose,
    };
    let bot = match Bot::start(cfg, lang, log.clone()) {
        Ok(bot) => bot,
        Err(e) => return Outcome::FailedBeforeJoin(e),
    };
    let mut printed = 0u64;
    // Until in the game (printing anything that arrives, e.g. a disconnect reason).
    loop {
        let g = bot.wait_until(Duration::from_millis(500), |g| g.ready || g.ended.is_some() || g.chat_seq > printed);
        print_new(&g, &mut printed, out);
        if let Some(end) = &g.ended {
            return outcome(end);
        }
        if g.ready {
            break;
        }
    }
    log.info(format!("sending {} command(s), listening for {}s", o.commands.len(), o.seconds));
    let start = Instant::now();
    let end = start + Duration::from_secs_f64(o.seconds);
    let mut next = 0;
    let mut next_at = start;
    loop {
        let now = Instant::now();
        if now >= end {
            let unsent = &o.commands[next..];
            if !unsent.is_empty() {
                log.info(format!("time is up before these commands were sent: {}", unsent.join(" || ")));
            }
            log.info("done; disconnecting");
            print_new(&bot.game(), &mut printed, out);
            bot.disconnect();
            return Outcome::Completed;
        }
        if next < o.commands.len() && now >= next_at {
            if let Err(e) = bot.send(Cmd::Command(o.commands[next].clone())) {
                return Outcome::Disconnected(e);
            }
            next += 1;
            next_at = now + o.delay;
        }
        let wake = if next < o.commands.len() { next_at.min(end) } else { end };
        let g = bot.wait_until(wake.saturating_duration_since(now), |g| g.chat_seq > printed || g.ended.is_some());
        print_new(&g, &mut printed, out);
        if let Some(e) = &g.ended {
            return outcome(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_uuid_matches_java() {
        // Expected values computed independently (Python's MD5 + the same version/variant bits).
        assert_eq!(uuid_string(offline_uuid("Notch")), "b50ad385-829d-3141-a216-7e7d7539ba7f");
        assert_eq!(uuid_string(offline_uuid("Probe")), "3f0ca0c8-9f16-3b87-92fb-b626b9a1bb99");
    }
}
