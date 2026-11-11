//! Wire basics: VarInts, a body reader/writer, and the framed TCP connection
//! (length-prefixed packets, optional zlib compression once the server asks for it).

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Largest frame vanilla accepts (2^21 - 1, a 3-byte VarInt).
pub const MAX_FRAME: usize = (1 << 21) - 1;
/// Largest uncompressed packet we inflate (vanilla's client has no limit; 8 MiB is plenty).
pub const MAX_UNCOMPRESSED: usize = 8 << 20;

pub fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

// ---------------------------------------------------------------------------------------------
// Writing

pub fn write_varint(out: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        if v & !0x7F == 0 {
            out.push(v as u8);
            return;
        }
        out.push((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
}

/// A packet body under construction (the packet id is added by [`Conn::send`]).
#[derive(Default)]
pub struct Writer {
    pub data: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn varint(&mut self, v: i32) -> &mut Self {
        write_varint(&mut self.data, v);
        self
    }
    pub fn string(&mut self, s: &str) -> &mut Self {
        write_varint(&mut self.data, s.len() as i32);
        self.data.extend_from_slice(s.as_bytes());
        self
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.data.push(v);
        self
    }
    pub fn i8(&mut self, v: i8) -> &mut Self {
        self.data.push(v as u8);
        self
    }
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.data.push(v as u8);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.data.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.data.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.data.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn uuid(&mut self, v: u128) -> &mut Self {
        self.data.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn i16(&mut self, v: i16) -> &mut Self {
        self.data.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.data.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn f64(&mut self, v: f64) -> &mut Self {
        self.data.extend_from_slice(&v.to_be_bytes());
        self
    }
    /// A block position packed into a long (x 26 bits, z 26 bits, y 12 bits).
    pub fn position(&mut self, p: [i32; 3]) -> &mut Self {
        self.i64(pack_position(p))
    }
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.data.extend_from_slice(b);
        self
    }
}

// ---------------------------------------------------------------------------------------------
// Reading

/// Cursor over a packet body.
pub struct Reader<'a> {
    buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }
    pub fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(invalid(format!(
                "packet too short: wanted {n} bytes, {} left",
                self.remaining()
            )));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn bool(&mut self) -> io::Result<bool> {
        Ok(self.u8()? != 0)
    }
    pub fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn i16(&mut self) -> io::Result<i16> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn i64(&mut self) -> io::Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn uuid(&mut self) -> io::Result<u128> {
        Ok(u128::from_be_bytes(self.take(16)?.try_into().unwrap()))
    }
    pub fn i8(&mut self) -> io::Result<i8> {
        Ok(self.u8()? as i8)
    }
    pub fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_bits(self.i32()? as u32))
    }
    pub fn f64(&mut self) -> io::Result<f64> {
        Ok(f64::from_bits(self.i64()? as u64))
    }
    pub fn vec3d(&mut self) -> io::Result<[f64; 3]> {
        Ok([self.f64()?, self.f64()?, self.f64()?])
    }
    pub fn position(&mut self) -> io::Result<[i32; 3]> {
        Ok(unpack_position(self.i64()?))
    }
    pub fn varlong(&mut self) -> io::Result<i64> {
        let mut value: u64 = 0;
        for i in 0..10 {
            let b = self.u8()?;
            value |= ((b & 0x7F) as u64) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(value as i64);
            }
        }
        Err(invalid("VarLong longer than 10 bytes"))
    }
    /// LpVec3 (1.21.9+ velocities): one zero byte for no motion, else three 15-bit components
    /// packed with a scale (Horizon's `buf::LpVec3`).
    pub fn lp_vec3(&mut self) -> io::Result<[f64; 3]> {
        let lowest = self.u8()? as u64;
        if lowest == 0 {
            return Ok([0.0; 3]);
        }
        let middle = self.u8()? as u64;
        let highest = self.i32()? as u32 as u64;
        let buffer = highest << 16 | middle << 8 | lowest;
        let mut scale = lowest & 3;
        if lowest & 4 == 4 {
            scale |= (self.varint()? as u32 as u64) << 2;
        }
        let unpack = |v: u64| ((v & 32767) as f64).min(32766.0) * 2.0 / 32766.0 - 1.0;
        let s = scale as f64;
        Ok([unpack(buffer >> 3) * s, unpack(buffer >> 18) * s, unpack(buffer >> 33) * s])
    }
    pub fn varint(&mut self) -> io::Result<i32> {
        let mut value: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            value |= ((b & 0x7F) as u32) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        Err(invalid("VarInt longer than 5 bytes"))
    }
    /// A VarInt length that must fit in what is left of the packet.
    pub fn prefixed_len(&mut self) -> io::Result<usize> {
        let n = self.varint()?;
        if n < 0 || n as usize > self.remaining() {
            return Err(invalid(format!("bad length {n}")));
        }
        Ok(n as usize)
    }
    /// A VarInt-prefixed UTF-8 string.
    pub fn string(&mut self) -> io::Result<String> {
        let n = self.prefixed_len()?;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

pub fn pack_position([x, y, z]: [i32; 3]) -> i64 {
    ((x as i64 & 0x3FF_FFFF) << 38) | ((z as i64 & 0x3FF_FFFF) << 12) | (y as i64 & 0xFFF)
}

pub fn unpack_position(v: i64) -> [i32; 3] {
    [(v >> 38) as i32, (v << 52 >> 52) as i32, (v << 26 >> 38) as i32]
}

/// Decode a VarInt at the start of `buf`: Ok(None) if `buf` ends before it does.
fn peek_varint(buf: &[u8]) -> io::Result<Option<(i32, usize)>> {
    let mut value: u32 = 0;
    for i in 0..5 {
        let Some(&b) = buf.get(i) else { return Ok(None) };
        value |= ((b & 0x7F) as u32) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(Some((value as i32, i + 1)));
        }
    }
    Err(invalid("frame length VarInt longer than 5 bytes"))
}

// ---------------------------------------------------------------------------------------------
// Connection

/// One received packet: its id and body (the bytes after the id).
pub struct Packet {
    pub id: i32,
    pub body: Vec<u8>,
}

pub struct Conn {
    stream: TcpStream,
    /// Bytes received but not yet framed. Survives read timeouts, so a timeout never splits a packet.
    rbuf: Vec<u8>,
    chunk: Vec<u8>,
    /// Compression threshold once the server sent Set Compression with a non-negative value.
    threshold: Option<usize>,
}

impl Conn {
    /// Resolve `host` and connect to the first address that answers within `timeout`.
    pub fn connect(host: &str, port: u16, timeout: Duration) -> io::Result<Self> {
        let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs()?.collect();
        if addrs.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{host} did not resolve")));
        }
        let mut last = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, timeout) {
                Ok(stream) => return Self::from_stream(stream),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap())
    }

    /// Wrap an open socket (either end: the tests use it for a mock server).
    pub fn from_stream(stream: TcpStream) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        stream.set_read_timeout(Some(Duration::from_millis(50)))?;
        Ok(Self {
            stream,
            rbuf: Vec::new(),
            chunk: vec![0; 64 * 1024],
            threshold: None,
        })
    }

    /// How long [`Conn::poll`] waits for data before returning `Ok(None)`.
    pub fn set_poll_timeout(&self, timeout: Duration) -> io::Result<()> {
        self.stream.set_read_timeout(Some(timeout))
    }

    /// Set Compression: a negative threshold turns compression off.
    pub fn set_compression(&mut self, threshold: i32) {
        self.threshold = (threshold >= 0).then_some(threshold as usize);
    }

    /// The next complete packet, or Ok(None) when nothing complete arrived within the short
    /// socket read timeout (so the caller can run its timers).
    pub fn poll(&mut self) -> io::Result<Option<Packet>> {
        loop {
            if let Some(p) = self.take_frame()? {
                return Ok(Some(p));
            }
            match self.stream.read(&mut self.chunk) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the server closed the connection",
                    ));
                }
                Ok(n) => self.rbuf.extend_from_slice(&self.chunk[..n]),
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    return Ok(None);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn take_frame(&mut self) -> io::Result<Option<Packet>> {
        let Some((len, header)) = peek_varint(&self.rbuf)? else { return Ok(None) };
        if len < 0 || len as usize > MAX_FRAME {
            return Err(invalid(format!("bad frame length {len}")));
        }
        let end = header + len as usize;
        if self.rbuf.len() < end {
            return Ok(None);
        }
        let frame = &self.rbuf[header..end];
        let data = match self.threshold {
            None => frame.to_vec(),
            Some(_) => {
                let mut r = Reader::new(frame);
                let size = r.varint()?;
                if size == 0 {
                    r.rest().to_vec()
                } else {
                    if size < 0 || size as usize > MAX_UNCOMPRESSED {
                        return Err(invalid(format!("bad uncompressed size {size}")));
                    }
                    let mut out = Vec::with_capacity(size as usize);
                    ZlibDecoder::new(r.rest())
                        .take(size as u64 + 1)
                        .read_to_end(&mut out)?;
                    if out.len() != size as usize {
                        return Err(invalid(format!(
                            "inflated {} bytes, the frame said {size}",
                            out.len()
                        )));
                    }
                    out
                }
            }
        };
        self.rbuf.drain(..end);
        let mut r = Reader::new(&data);
        let id = r.varint()?;
        let body = data[r.pos..].to_vec();
        Ok(Some(Packet { id, body }))
    }

    /// Frame and send one packet (compressing it when it reaches the threshold).
    pub fn send(&mut self, id: i32, body: &[u8]) -> io::Result<()> {
        let mut data = Vec::with_capacity(body.len() + 5);
        write_varint(&mut data, id);
        data.extend_from_slice(body);
        let payload = match self.threshold {
            None => data,
            Some(threshold) => {
                let mut inner = Vec::with_capacity(data.len() + 5);
                if data.len() >= threshold {
                    write_varint(&mut inner, data.len() as i32);
                    let mut z = ZlibEncoder::new(inner, Compression::default());
                    z.write_all(&data)?;
                    z.finish()?
                } else {
                    write_varint(&mut inner, 0);
                    inner.extend_from_slice(&data);
                    inner
                }
            }
        };
        let mut frame = Vec::with_capacity(payload.len() + 3);
        write_varint(&mut frame, payload.len() as i32);
        frame.extend_from_slice(&payload);
        self.stream.write_all(&frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips() {
        for v in [0, 1, 127, 128, 255, 25565, 2097151, i32::MAX, -1, i32::MIN] {
            let mut buf = Vec::new();
            write_varint(&mut buf, v);
            assert_eq!(Reader::new(&buf).varint().unwrap(), v);
            assert_eq!(peek_varint(&buf).unwrap(), Some((v, buf.len())));
            assert_eq!(peek_varint(&buf[..buf.len() - 1]).unwrap(), None);
        }
        let mut buf = Vec::new();
        write_varint(&mut buf, -1);
        assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
    }

    #[test]
    fn positions_round_trip() {
        for p in [[0, 0, 0], [1, -64, -1], [-30_000_000, 319, 29_999_999], [123, -2048, -456], [-1, 2047, 1]] {
            assert_eq!(unpack_position(pack_position(p)), p);
        }
    }

    #[test]
    fn lp_vec3_zero_is_one_byte() {
        let mut r = Reader::new(&[0, 0xAA]);
        assert_eq!(r.lp_vec3().unwrap(), [0.0; 3]);
        assert_eq!(r.remaining(), 1);
    }
}
