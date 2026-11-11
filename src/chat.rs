//! The three clientbound chat packets turned into display lines, plus the `minecraft:chat_type`
//! registry that says how a player/disguised message is decorated (`<%s> %s`, `[%s] %s`, ...).

use crate::nbt::{self, Tag};
use crate::net::{Reader, invalid};
use crate::text::{self, Lang};
use std::io;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Param {
    Sender,
    Target,
    Content,
}

/// ChatTypeDecoration: a translation key and which values fill its `%s` slots.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoration {
    pub key: String,
    pub params: Vec<Param>,
}

/// The vanilla chat types, for entries the server sent without data (it omits the data of
/// entries from known packs, which this client claims to have).
fn builtin(name: &str) -> Option<Decoration> {
    use Param::*;
    let (key, params): (&str, &[Param]) = match name {
        "minecraft:chat" => ("chat.type.text", &[Sender, Content]),
        "minecraft:say_command" => ("chat.type.announcement", &[Sender, Content]),
        "minecraft:msg_command_incoming" => ("commands.message.display.incoming", &[Sender, Content]),
        "minecraft:msg_command_outgoing" => ("commands.message.display.outgoing", &[Target, Content]),
        "minecraft:team_msg_command_incoming" => ("chat.type.team.text", &[Target, Sender, Content]),
        "minecraft:team_msg_command_outgoing" => ("chat.type.team.sent", &[Target, Sender, Content]),
        "minecraft:emote_command" => ("chat.type.emote", &[Sender, Content]),
        _ => return None,
    };
    Some(Decoration { key: key.into(), params: params.to_vec() })
}

/// The `minecraft:chat_type` registry as the server sent it (entry order = numeric id).
#[derive(Default)]
pub struct ChatTypes {
    entries: Vec<(String, Option<Decoration>)>,
}

impl ChatTypes {
    /// Registry Data entries (after the registry id): Prefixed Array of
    /// { entry id, Prefixed Optional NBT }.
    pub fn load(&mut self, r: &mut Reader) -> io::Result<()> {
        let n = r.prefixed_len()?;
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            let name = r.string()?;
            let data = if r.bool()? { nbt::read_network(r)? } else { None };
            entries.push((name, data));
        }
        self.set_entries(&entries);
        Ok(())
    }

    /// The registry's entries as (name, data the server sent, if any).
    pub fn set_entries(&mut self, entries: &[(String, Option<Tag>)]) {
        self.entries = entries
            .iter()
            .map(|(name, data)| {
                let decoration = data
                    .as_ref()
                    .and_then(|d| d.get("chat"))
                    .and_then(decoration_from_nbt)
                    .or_else(|| builtin(name));
                (name.clone(), decoration)
            })
            .collect();
    }

    fn by_id(&self, id: usize) -> Option<Decoration> {
        match self.entries.get(id) {
            Some((_, decoration)) => decoration.clone(),
            None => None,
        }
    }
}

fn param(name: &str) -> Option<Param> {
    match name {
        "sender" => Some(Param::Sender),
        "target" => Some(Param::Target),
        "content" => Some(Param::Content),
        _ => None,
    }
}

fn decoration_from_nbt(chat: &Tag) -> Option<Decoration> {
    let key = chat.get("translation_key")?.as_str()?.to_string();
    let params = match chat.get("parameters") {
        Some(Tag::List(items)) => items.iter().filter_map(|t| t.as_str().and_then(param)).collect(),
        _ => Vec::new(),
    };
    Some(Decoration { key, params })
}

/// "ID or Chat Type": VarInt 0 = an inline definition follows, otherwise registry id + 1.
fn read_chat_type(r: &mut Reader, types: &ChatTypes) -> io::Result<Option<Decoration>> {
    let id = r.varint()?;
    if id < 0 {
        return Err(invalid("negative chat type id"));
    }
    if id > 0 {
        return Ok(types.by_id(id as usize - 1));
    }
    let chat = read_inline_decoration(r)?;
    let _narration = read_inline_decoration(r)?;
    Ok(Some(chat))
}

/// ChatTypeDecoration's stream codec: key, VarInt parameter ids, style (NBT).
fn read_inline_decoration(r: &mut Reader) -> io::Result<Decoration> {
    let key = r.string()?;
    let n = r.prefixed_len()?;
    let mut params = Vec::with_capacity(n);
    for _ in 0..n {
        match r.varint()? {
            0 => params.push(Param::Sender),
            1 => params.push(Param::Target),
            2 => params.push(Param::Content),
            _ => {}
        }
    }
    let _style = nbt::read_network(r)?;
    Ok(Decoration { key, params })
}

/// A network text component (NBT) as plain text.
pub fn read_text(r: &mut Reader, lang: &Lang) -> io::Result<String> {
    Ok(match nbt::read_network(r)? {
        Some(tag) => text::plain(&tag, lang),
        None => String::new(),
    })
}

fn read_optional_text(r: &mut Reader, lang: &Lang) -> io::Result<Option<String>> {
    if r.bool()? { read_text(r, lang).map(Some) } else { Ok(None) }
}

/// Format a decorated chat line the way the vanilla client does (the key itself is the pattern
/// when the language table doesn't know it); `<sender> content` without a chat type.
fn decorate(
    decoration: Option<&Decoration>,
    sender: &str,
    target: Option<&str>,
    content: &str,
    lang: &Lang,
) -> String {
    let Some(d) = decoration else { return format!("<{sender}> {content}") };
    let args: Vec<String> = d
        .params
        .iter()
        .map(|p| match p {
            Param::Sender => sender.to_string(),
            Param::Target => target.unwrap_or_default().to_string(),
            Param::Content => content.to_string(),
        })
        .collect();
    let pattern = lang.get(&d.key).unwrap_or(&d.key);
    text::strip_formatting(&text::format_java(pattern, &args))
}

/// System Chat Message: (text, is_action_bar).
pub fn system_chat(body: &[u8], lang: &Lang) -> io::Result<(String, bool)> {
    let mut r = Reader::new(body);
    let content = read_text(&mut r, lang)?;
    let overlay = r.bool().unwrap_or(false);
    Ok((content, overlay))
}

/// Player Chat Message. The signed body's plain message comes early; if anything after it
/// can't be read, the message is still shown (with an unknown sender).
pub fn player_chat(body: &[u8], types: &ChatTypes, lang: &Lang) -> io::Result<String> {
    let mut r = Reader::new(body);
    let _global_index = r.varint()?;
    let _sender = r.uuid()?;
    let _index = r.varint()?;
    if r.bool()? {
        r.take(256)?; // message signature
    }
    let message = text::strip_formatting(&r.string()?);
    Ok(player_chat_rest(&mut r, &message, types, lang).unwrap_or_else(|_| format!("<?> {message}")))
}

fn player_chat_rest(r: &mut Reader, message: &str, types: &ChatTypes, lang: &Lang) -> io::Result<String> {
    let _timestamp = r.i64()?;
    let _salt = r.i64()?;
    let previous = r.varint()?;
    if !(0..=20).contains(&previous) {
        return Err(invalid("bad previous-messages count"));
    }
    for _ in 0..previous {
        if r.varint()? == 0 {
            r.take(256)?;
        }
    }
    let unsigned = read_optional_text(r, lang)?;
    if r.varint()? == 2 {
        // PARTIALLY_FILTERED: a BitSet (VarInt count of longs).
        let longs = r.varint()?;
        if longs < 0 {
            return Err(invalid("bad filter mask"));
        }
        r.take(longs as usize * 8)?;
    }
    let chat_type = read_chat_type(r, types)?;
    let sender = read_text(r, lang)?;
    let target = read_optional_text(r, lang)?;
    let content = unsigned.as_deref().unwrap_or(message);
    Ok(decorate(chat_type.as_ref(), &sender, target.as_deref(), content, lang))
}

/// Disguised Chat Message (the console's /say, /tell, /me, ...): no signature data.
pub fn disguised_chat(body: &[u8], types: &ChatTypes, lang: &Lang) -> io::Result<String> {
    let mut r = Reader::new(body);
    let message = read_text(&mut r, lang)?;
    let rest = (|| -> io::Result<String> {
        let chat_type = read_chat_type(&mut r, types)?;
        let sender = read_text(&mut r, lang)?;
        let target = read_optional_text(&mut r, lang)?;
        Ok(decorate(chat_type.as_ref(), &sender, target.as_deref(), &message, lang))
    })();
    Ok(rest.unwrap_or_else(|_| format!("<?> {message}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::Writer;

    fn nbt_string(w: &mut Writer, s: &str) {
        w.u8(8).u16(s.len() as u16).bytes(s.as_bytes());
    }

    fn types() -> ChatTypes {
        // [minecraft:chat (no data), minecraft:say_command (no data)]
        let mut w = Writer::new();
        w.varint(2).string("minecraft:chat").bool(false).string("minecraft:say_command").bool(false);
        let mut t = ChatTypes::default();
        t.load(&mut Reader::new(&w.data)).unwrap();
        t
    }

    #[test]
    fn player_chat_uses_the_chat_type() {
        let lang = Lang::load(None).unwrap();
        let mut w = Writer::new();
        w.varint(0).uuid(1).varint(0).bool(false).string("hello world").i64(0).i64(0).varint(0);
        w.bool(false).varint(0).varint(1); // no unsigned content, PASS_THROUGH, minecraft:chat
        nbt_string(&mut w, "Alex");
        w.bool(false);
        assert_eq!(player_chat(&w.data, &types(), &lang).unwrap(), "<Alex> hello world");
    }

    #[test]
    fn disguised_chat_say_command() {
        let lang = Lang::load(None).unwrap();
        let mut w = Writer::new();
        nbt_string(&mut w, "hi all");
        w.varint(2); // minecraft:say_command
        nbt_string(&mut w, "Server");
        w.bool(false);
        assert_eq!(disguised_chat(&w.data, &types(), &lang).unwrap(), "[Server] hi all");
    }

    #[test]
    fn truncated_player_chat_still_shows_the_message() {
        let lang = Lang::load(None).unwrap();
        let mut w = Writer::new();
        w.varint(0).uuid(1).varint(0).bool(false).string("partial");
        assert_eq!(player_chat(&w.data, &types(), &lang).unwrap(), "<?> partial");
    }
}
