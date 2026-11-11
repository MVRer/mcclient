//! Text components flattened to plain text: literal text, `translate` keys resolved through a
//! language table with Java `String.format`-style `%s` / `%n$s` arguments, `keybind`,
//! `selector`, `score`, then `extra` siblings. Legacy `§` formatting codes are stripped.

use crate::nbt::Tag;
use std::collections::HashMap;
use std::path::Path;

/// Translation key -> pattern.
pub struct Lang {
    map: HashMap<String, String>,
}

impl Lang {
    /// The built-in subset, plus (overriding it) a full `en_us.json` if one is given.
    pub fn load(file: Option<&Path>) -> Result<Self, String> {
        let mut map: HashMap<String, String> = crate::lang::BUILTIN
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if let Some(path) = file {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let json: HashMap<String, String> = serde_json::from_str(&text)
                .map_err(|e| format!("{} is not a language file: {e}", path.display()))?;
            map.extend(json);
        }
        Ok(Self { map })
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(String::as_str)
    }
}

/// Flatten a component to text (formatting codes removed).
pub fn plain(tag: &Tag, lang: &Lang) -> String {
    let mut out = String::new();
    append(tag, lang, &mut out, 0);
    strip_formatting(&out)
}

fn append(tag: &Tag, lang: &Lang, out: &mut String, depth: usize) {
    if depth > 64 {
        return;
    }
    match tag {
        Tag::String(s) => out.push_str(s),
        Tag::Byte(v) => out.push_str(&v.to_string()),
        Tag::Short(v) => out.push_str(&v.to_string()),
        Tag::Int(v) => out.push_str(&v.to_string()),
        Tag::Long(v) => out.push_str(&v.to_string()),
        Tag::Float(v) => out.push_str(&v.to_string()),
        Tag::Double(v) => out.push_str(&v.to_string()),
        // A list is its first element with the rest as siblings.
        Tag::List(items) => items.iter().for_each(|t| append(t, lang, out, depth + 1)),
        Tag::Compound(_) => {
            if let Some(text) = tag.get("text").and_then(Tag::as_str) {
                out.push_str(text);
            } else if let Some(value) = tag.get("") {
                // NbtOps wraps bare values in mixed lists (a translation's number arguments).
                append(value, lang, out, depth + 1);
            } else if let Some(key) = tag.get("translate").and_then(Tag::as_str) {
                let args: Vec<String> = match tag.get("with") {
                    Some(Tag::List(args)) => args
                        .iter()
                        .map(|a| {
                            let mut s = String::new();
                            append(a, lang, &mut s, depth + 1);
                            s
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                match lang
                    .get(key)
                    .or_else(|| tag.get("fallback").and_then(Tag::as_str))
                {
                    Some(pattern) => out.push_str(&format_java(pattern, &args)),
                    // Unknown key: show it with its arguments, so nothing is lost.
                    None if args.is_empty() => out.push_str(key),
                    None => out.push_str(&format!("{key} [{}]", args.join(", "))),
                }
            } else if let Some(key) = tag.get("keybind").and_then(Tag::as_str) {
                out.push_str(lang.get(key).unwrap_or(key));
            } else if let Some(selector) = tag.get("selector").and_then(Tag::as_str) {
                out.push_str(selector);
            } else if let Some(name) = tag
                .get("score")
                .and_then(|s| s.get("name"))
                .and_then(Tag::as_str)
            {
                out.push_str(name);
            }
            if let Some(Tag::List(extra)) = tag.get("extra") {
                extra.iter().for_each(|e| append(e, lang, out, depth + 1));
            }
        }
        _ => {}
    }
}

/// TranslatableContents' template: `%s`, `%n$s` and `%%`.
pub fn format_java(pattern: &str, args: &[String]) -> String {
    let mut out = String::new();
    let mut next = 0usize;
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut digits = String::new();
        while let Some(d) = chars.peek().copied().filter(char::is_ascii_digit) {
            digits.push(d);
            chars.next();
        }
        match chars.next() {
            Some('%') if digits.is_empty() => out.push('%'),
            Some('s') if digits.is_empty() => {
                out.push_str(args.get(next).map_or("", String::as_str));
                next += 1;
            }
            Some('$') if !digits.is_empty() && chars.peek() == Some(&'s') => {
                chars.next();
                let i = digits.parse::<usize>().unwrap_or(1).saturating_sub(1);
                out.push_str(args.get(i).map_or("", String::as_str));
            }
            Some(other) => {
                out.push('%');
                out.push_str(&digits);
                out.push(other);
            }
            None => {
                out.push('%');
                out.push_str(&digits);
            }
        }
    }
    out
}

/// Drop legacy formatting codes (`§` and the character after it).
pub fn strip_formatting(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// A JSON text component (the login-phase disconnect reason) as the same tree.
pub fn from_json(v: &serde_json::Value) -> Tag {
    use serde_json::Value;
    match v {
        Value::Null => Tag::String(String::new()),
        Value::Bool(b) => Tag::String(b.to_string()),
        Value::Number(n) => Tag::String(n.to_string()),
        Value::String(s) => Tag::String(s.clone()),
        Value::Array(items) => Tag::List(items.iter().map(from_json).collect()),
        Value::Object(map) => {
            Tag::Compound(map.iter().map(|(k, v)| (k.clone(), from_json(v))).collect())
        }
    }
}

/// A JSON text component string flattened to text (raw text if it is not JSON).
pub fn json_plain(json: &str, lang: &Lang) -> String {
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(v) => plain(&from_json(&v), lang),
        Err(_) => strip_formatting(json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Tag {
        Tag::String(v.into())
    }
    fn c(entries: &[(&str, Tag)]) -> Tag {
        Tag::Compound(entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    #[test]
    fn java_format() {
        let args = vec!["Steve".to_string(), "hi".to_string()];
        assert_eq!(format_java("<%s> %s", &args), "<Steve> hi");
        assert_eq!(format_java("%2$s by %1$s", &args), "hi by Steve");
        assert_eq!(format_java("100%% %s", &args), "100% Steve");
        assert_eq!(format_java("%s %s %s", &args), "Steve hi ");
    }

    #[test]
    fn flattens_translations_and_siblings() {
        let lang = Lang::load(None).unwrap();
        let joined = c(&[
            ("translate", s("multiplayer.player.joined")),
            ("with", Tag::List(vec![c(&[("text", s("Alex"))])])),
        ]);
        assert_eq!(plain(&joined, &lang), "Alex joined the game");
        let list = c(&[
            ("translate", s("commands.list.players")),
            ("with", Tag::List(vec![c(&[("", Tag::Int(1))]), c(&[("", Tag::Int(20))]), s("Probe")])),
        ]);
        assert_eq!(plain(&list, &lang), "There are 1 of a max of 20 players online: Probe");
        let nested = c(&[("text", s("§aa")), ("extra", Tag::List(vec![s("b"), c(&[("text", s("c"))])]))]);
        assert_eq!(plain(&nested, &lang), "abc");
        let unknown = c(&[("translate", s("my.plugin.key")), ("with", Tag::List(vec![s("x")]))]);
        assert_eq!(plain(&unknown, &lang), "my.plugin.key [x]");
        let fallback = c(&[("translate", s("my.plugin.key")), ("fallback", s("Hi %s")), ("with", Tag::List(vec![s("x")]))]);
        assert_eq!(plain(&fallback, &lang), "Hi x");
    }

    #[test]
    fn json_components() {
        let lang = Lang::load(None).unwrap();
        assert_eq!(json_plain(r#"{"translate":"multiplayer.disconnect.server_full"}"#, &lang), "The server is full!");
        assert_eq!(json_plain(r#""plain""#, &lang), "plain");
        assert_eq!(json_plain(r#"[{"text":"a"},"b"]"#, &lang), "ab");
    }
}
