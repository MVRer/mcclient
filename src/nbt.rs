//! Just enough NBT to read text components and chat-type registry entries: the network form
//! (since 1.20.2 the root tag has a type byte but no name), read into a small tree.

use crate::net::{Reader, invalid};
use std::io;

#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    String(String),
    List(Vec<Tag>),
    Compound(Vec<(String, Tag)>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Tag {
    pub fn get(&self, key: &str) -> Option<&Tag> {
        match self {
            Tag::Compound(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Tag::String(s) => Some(s),
            _ => None,
        }
    }
}

/// Vanilla's NbtAccounter depth limit.
const MAX_DEPTH: usize = 512;

/// A network NBT value: a type byte, then that tag's payload with no name. `None` is TAG_End
/// (an empty/absent value).
pub fn read_network(r: &mut Reader) -> io::Result<Option<Tag>> {
    let kind = r.u8()?;
    if kind == 0 {
        return Ok(None);
    }
    read_payload(r, kind, 0).map(Some)
}

fn read_payload(r: &mut Reader, kind: u8, depth: usize) -> io::Result<Tag> {
    if depth > MAX_DEPTH {
        return Err(invalid("NBT nested too deeply"));
    }
    Ok(match kind {
        1 => Tag::Byte(r.u8()? as i8),
        2 => Tag::Short(r.i16()?),
        3 => Tag::Int(r.i32()?),
        4 => Tag::Long(r.i64()?),
        5 => Tag::Float(f32::from_bits(r.i32()? as u32)),
        6 => Tag::Double(f64::from_bits(r.i64()? as u64)),
        7 => {
            let n = array_len(r, 1)?;
            Tag::ByteArray(r.take(n)?.to_vec())
        }
        8 => Tag::String(read_string(r)?),
        9 => {
            let elem = r.u8()?;
            let n = r.i32()?;
            if n <= 0 || elem == 0 {
                Tag::List(Vec::new())
            } else {
                // Every element is at least one byte (or TAG_End, handled above), so a
                // count larger than what is left is malformed: don't allocate for it.
                if n as usize > r.remaining() {
                    return Err(invalid("NBT list longer than the packet"));
                }
                let mut items = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    items.push(read_payload(r, elem, depth + 1)?);
                }
                Tag::List(items)
            }
        }
        10 => {
            let mut entries = Vec::new();
            loop {
                let kind = r.u8()?;
                if kind == 0 {
                    break;
                }
                let name = read_string(r)?;
                entries.push((name, read_payload(r, kind, depth + 1)?));
            }
            Tag::Compound(entries)
        }
        11 => {
            let n = array_len(r, 4)?;
            Tag::IntArray((0..n).map(|_| r.i32()).collect::<io::Result<_>>()?)
        }
        12 => {
            let n = array_len(r, 8)?;
            Tag::LongArray((0..n).map(|_| r.i64()).collect::<io::Result<_>>()?)
        }
        other => return Err(invalid(format!("unknown NBT tag type {other}"))),
    })
}

fn array_len(r: &mut Reader, elem_size: usize) -> io::Result<usize> {
    let n = r.i32()?;
    if n < 0 || (n as usize).saturating_mul(elem_size) > r.remaining() {
        return Err(invalid("NBT array longer than the packet"));
    }
    Ok(n as usize)
}

/// NBT strings: an unsigned short byte length, then Java's "modified UTF-8".
fn read_string(r: &mut Reader) -> io::Result<String> {
    let n = r.u16()? as usize;
    Ok(decode_mutf8(r.take(n)?))
}

/// Java's modified UTF-8: NUL is 0xC0 0x80 and characters outside the BMP are two 3-byte
/// surrogates. Plain UTF-8 (the usual case) takes the fast path.
pub fn decode_mutf8(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let cont = |k: usize| bytes.get(i + k).map_or(0, |c| (c & 0x3F) as u16);
        if b < 0x80 {
            units.push(b as u16);
            i += 1;
        } else if b & 0xE0 == 0xC0 {
            units.push(((b & 0x1F) as u16) << 6 | cont(1));
            i += 2;
        } else if b & 0xF0 == 0xE0 {
            units.push(((b & 0x0F) as u16) << 12 | cont(1) << 6 | cont(2));
            i += 3;
        } else {
            units.push(0xFFFD);
            i += 1;
        }
    }
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_nameless_compound() {
        // {text: "hi", extra: [{text: "!"}]}
        let mut b = vec![10];
        b.extend([8, 0, 4]);
        b.extend(b"text");
        b.extend([0, 2]);
        b.extend(b"hi");
        b.extend([9, 0, 5]);
        b.extend(b"extra");
        b.extend([10, 0, 0, 0, 1]);
        b.extend([8, 0, 4]);
        b.extend(b"text");
        b.extend([0, 1]);
        b.extend(b"!");
        b.extend([0, 0]);
        let tag = read_network(&mut Reader::new(&b)).unwrap().unwrap();
        assert_eq!(tag.get("text").and_then(Tag::as_str), Some("hi"));
        assert!(matches!(tag.get("extra"), Some(Tag::List(items)) if items.len() == 1));
    }

    #[test]
    fn decodes_modified_utf8() {
        assert_eq!(decode_mutf8(b"plain"), "plain");
        assert_eq!(decode_mutf8(&[b'a', 0xC0, 0x80, b'b']), "a\0b");
        // U+1F600 as a CESU-8 surrogate pair.
        assert_eq!(decode_mutf8(&[0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]), "\u{1F600}");
    }
}
