//! What the bot knows about the world: the block states of the chunks the server sent, kept up to
//! date by block updates, with block names/properties, approximate collision boxes and a block
//! search.
//!
//! ## Packets (same layout in protocols 774-777 except where noted)
//! - `level_chunk_with_light`: chunk x, z (Int); heightmaps (VarInt count of a VarInt type and a
//!   VarInt-prefixed long array each, since 1.21.5); the section data (VarInt-prefixed bytes);
//!   block entities and light, which are not read. The data holds `height / 16` sections: the
//!   non-empty block count (Short), **in 26.1+ (775-777) also a fluid count (Short), absent in
//!   1.21.11 (774)**, then the block-state and biome paletted containers. (26.3 also changed the
//!   light masks from long to byte arrays; they come after the part read here.)
//! - Paletted container (since 1.21.5 the long array has no length prefix: its length follows from
//!   the bits per entry; entries never span longs): a bits byte; 0 = one value (VarInt); blocks
//!   1-8 / biomes 1-3 = a palette (VarInt count + VarInts), blocks stored with at least 4 bits;
//!   otherwise direct, with vanilla's `ceil(log2(registry size))` bits whatever the byte says:
//!   blocks 16 bits in 26.3 (35723 states), 15 in 774-776; biomes from `biome_count`.
//! - `forget_level_chunk`: ChunkPos as one Long, x in the low 32 bits, z in the high.
//! - `block_update`: packed Position (Long: x 26 bits, z 26, y 12) + VarInt state.
//! - `section_blocks_update`: SectionPos (Long: x 22 bits, z 22, y 20) + VarInt count of VarLongs
//!   `state << 12 | x << 8 | z << 4 | y`.
//!
//! State ids differ per protocol: see [`blocks`] and the generated `blocks_<protocol>.rs`.
//!
//! ## Storage
//! A chunk is its sections; a section is one value when uniform, else the server's palette with
//! its packed indices (4-8 bits per block, so 2-4 KiB), else (more than 256 distinct states) one
//! `u16` per block (8 KiB). Updates grow a palette as vanilla does (4..8 bits, then direct). A
//! 9x9-chunk overworld area is typically well under 2 MiB.

mod blocks;
mod blocks_774;
mod blocks_775;
mod blocks_776;
mod blocks_777;
mod shape;
#[cfg(test)]
mod tests;

use crate::net::{Reader, invalid};
use blocks::Table;
use shape::Shape;
use std::collections::{BinaryHeap, HashMap};
use std::io;

/// An axis-aligned box in world coordinates (blocks).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

/// Blocks per section (16^3), indexed `(y * 16 + z) * 16 + x`.
const BLOCKS: usize = 4096;
/// Biome cells per section (4^3).
const BIOMES: usize = 64;

/// Blocks whose fluid state is never empty (besides `waterlogged=true` states).
const FLUID_BLOCKS: &[&str] =
    &["water", "lava", "bubble_column", "kelp", "kelp_plant", "seagrass", "tall_seagrass"];

enum Section {
    /// Every block the same state.
    Single(u32),
    /// Palette of states and `bits`-wide indices into it (`64 / bits` per long, low bits first).
    Packed { palette: Vec<u32>, bits: u32, data: Box<[u64]> },
    /// Each block's state.
    Direct(Box<[u16]>),
}

impl Section {
    fn get(&self, i: usize) -> u32 {
        match self {
            Section::Single(v) => *v,
            Section::Packed { palette, bits, data } => palette[unpack(data, *bits, i) as usize],
            Section::Direct(d) => u32::from(d[i]),
        }
    }

    fn set(&mut self, i: usize, state: u32) {
        match self {
            Section::Single(v) if *v == state => {}
            Section::Single(v) => {
                let mut data = vec![0; longs(BLOCKS, 4)].into_boxed_slice();
                pack(&mut data, 4, i, 1);
                *self = Section::Packed { palette: vec![*v, state], bits: 4, data };
            }
            Section::Packed { palette, bits, data } => {
                let index = match palette.iter().position(|&p| p == state) {
                    Some(index) => index,
                    None if palette.len() < 1 << *bits => {
                        palette.push(state);
                        palette.len() - 1
                    }
                    None if *bits < 8 => {
                        // Grow the palette by one bit, as vanilla's HashMapPalette resize.
                        let wider = *bits + 1;
                        let mut grown = vec![0; longs(BLOCKS, wider)].into_boxed_slice();
                        for j in 0..BLOCKS {
                            pack(&mut grown, wider, j, unpack(data, *bits, j));
                        }
                        (*bits, *data) = (wider, grown);
                        palette.push(state);
                        palette.len() - 1
                    }
                    None => {
                        let mut direct: Box<[u16]> =
                            (0..BLOCKS).map(|j| palette[unpack(data, *bits, j) as usize] as u16).collect();
                        direct[i] = state as u16;
                        *self = Section::Direct(direct);
                        return;
                    }
                };
                pack(data, *bits, i, index as u64);
            }
            Section::Direct(d) => d[i] = state as u16,
        }
    }

    /// Whether any block of the section may satisfy `wanted` (by state).
    fn may_contain(&self, wanted: &impl Fn(u32) -> bool) -> bool {
        match self {
            Section::Single(v) => wanted(*v),
            Section::Packed { palette, .. } => palette.iter().any(|&p| wanted(p)),
            Section::Direct(_) => true,
        }
    }

    #[cfg(test)]
    fn heap_bytes(&self) -> usize {
        match self {
            Section::Single(_) => 0,
            Section::Packed { palette, data, .. } => palette.capacity() * 4 + data.len() * 8,
            Section::Direct(d) => d.len() * 2,
        }
    }
}

/// Longs holding `entries` values of `bits` bits, `64 / bits` per long.
fn longs(entries: usize, bits: u32) -> usize {
    if bits == 0 { 0 } else { entries.div_ceil(64 / bits as usize) }
}

fn unpack(data: &[u64], bits: u32, i: usize) -> u64 {
    let per = 64 / bits as usize;
    (data[i / per] >> ((i % per) as u32 * bits)) & ((1 << bits) - 1)
}

fn pack(data: &mut [u64], bits: u32, i: usize, value: u64) {
    let per = 64 / bits as usize;
    let shift = (i % per) as u32 * bits;
    let mask = ((1u64 << bits) - 1) << shift;
    data[i / per] = (data[i / per] & !mask) | ((value << shift) & mask);
}

/// Index of a block within its section.
fn index(x: i32, y_in_section: i32, z: i32) -> usize {
    (((y_in_section & 15) << 8) | ((z & 15) << 4) | (x & 15)) as usize
}

/// The block states of the loaded chunks of one dimension.
pub struct World {
    table: &'static Table,
    /// Collision class per block (index into `table.blocks`).
    shapes: Vec<Shape>,
    /// Per block: whether its fluid state is never empty (water, lava, kelp, ...).
    fluid_blocks: Vec<bool>,
    /// Sections carry a fluid count after the block count (26.1+, protocol 775 and up).
    fluid_count: bool,
    min_y: i32,
    height: i32,
    biome_count: usize,
    chunks: HashMap<(i32, i32), Box<[Section]>>,
}

impl World {
    /// An empty world with the block states of `protocol` (unknown protocols: the newest table),
    /// sized like the overworld (min y -64, height 384) until [`World::reset`].
    pub fn new(protocol: i32) -> Self {
        let table = Table::for_protocol(protocol);
        World {
            table,
            shapes: table.blocks.iter().map(|b| shape::classify(b.0)).collect(),
            fluid_blocks: table.blocks.iter().map(|b| FLUID_BLOCKS.contains(&b.0)).collect(),
            fluid_count: !matches!(protocol, 774),
            min_y: -64,
            height: 384,
            biome_count: 0,
            chunks: HashMap::new(),
        }
    }

    /// New dimension (login/respawn): forget all chunks. `min_y` and `height` come from the
    /// dimension type (multiples of 16); `biome_count` is the number of `worldgen/biome` registry
    /// entries (it sets the bits of direct biome containers; 0 = unknown, trust the bits byte the
    /// server sends, which vanilla servers set to the same value).
    pub fn reset(&mut self, min_y: i32, height: i32, biome_count: usize) {
        self.chunks.clear();
        self.min_y = min_y;
        self.height = height.max(0);
        self.biome_count = biome_count;
    }

    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    fn sections(&self) -> usize {
        (self.height / 16) as usize
    }

    /// `level_chunk_with_light` body (after the packet id): stores the chunk's block states
    /// (replacing a chunk already there) and returns its chunk x, z.
    pub fn load_chunk(&mut self, body: &[u8]) -> io::Result<(i32, i32)> {
        let mut r = Reader::new(body);
        let x = r.i32()?;
        let z = r.i32()?;
        let heightmaps = r.prefixed_len()?;
        for _ in 0..heightmaps {
            r.varint()?;
            let n = r.prefixed_len()?;
            r.take(n * 8)?;
        }
        let n = r.prefixed_len()?;
        let mut data = Reader::new(r.take(n)?);
        let sections = (0..self.sections())
            .map(|_| self.read_section(&mut data))
            .collect::<io::Result<Box<[Section]>>>()
            .map_err(|e| invalid(format!("chunk {x}, {z}: {e}")))?;
        // Block entities and light follow; nothing here needs them.
        self.chunks.insert((x, z), sections);
        Ok((x, z))
    }

    fn read_section(&self, r: &mut Reader) -> io::Result<Section> {
        r.i16()?; // non-empty block count
        if self.fluid_count {
            r.i16()?; // fluid count (26.1+)
        }
        let states = self.read_states(r)?;
        self.skip_biomes(r)?;
        Ok(states)
    }

    fn state_id(&self, v: i64) -> io::Result<u32> {
        match u32::try_from(v) {
            Ok(s) if s < self.table.states => Ok(s),
            _ => Err(invalid(format!("block state {v} out of range (0..{})", self.table.states))),
        }
    }

    fn read_states(&self, r: &mut Reader) -> io::Result<Section> {
        let bits = u32::from(r.u8()?);
        if bits == 0 {
            return Ok(Section::Single(self.state_id(r.varint()?.into())?));
        }
        if bits <= 8 {
            let bits = bits.max(4);
            let n = r.varint()?;
            if !(1..=1 << bits).contains(&n) {
                return Err(invalid(format!("block palette of {n} entries for {bits} bits")));
            }
            let palette =
                (0..n).map(|_| self.state_id(r.varint()?.into())).collect::<io::Result<Vec<_>>>()?;
            let data = read_longs(r, longs(BLOCKS, bits))?;
            if (0..BLOCKS).any(|i| unpack(&data, bits, i) >= palette.len() as u64) {
                return Err(invalid("block palette index out of range"));
            }
            return Ok(Section::Packed { palette, bits, data });
        }
        let bits = self.table.global_bits();
        let data = read_longs(r, longs(BLOCKS, bits))?;
        let direct = (0..BLOCKS)
            .map(|i| self.state_id(unpack(&data, bits, i) as i64).map(|s| s as u16))
            .collect::<io::Result<_>>()?;
        Ok(Section::Direct(direct))
    }

    fn skip_biomes(&self, r: &mut Reader) -> io::Result<()> {
        let mut bits = u32::from(r.u8()?);
        match bits {
            0 => {
                r.varint()?;
            }
            1..=3 => {
                let n = r.varint()?;
                if !(1..=1 << bits).contains(&n) {
                    return Err(invalid(format!("biome palette of {n} entries for {bits} bits")));
                }
                for _ in 0..n {
                    r.varint()?;
                }
            }
            _ => {
                if self.biome_count > 0 {
                    bits = blocks::ceil_log2(self.biome_count.min(u32::MAX as usize) as u32);
                }
                if bits > 32 {
                    return Err(invalid(format!("{bits} bits per biome")));
                }
            }
        }
        r.take(longs(BIOMES, bits) * 8)?;
        Ok(())
    }

    /// `forget_level_chunk` body.
    pub fn forget_chunk(&mut self, body: &[u8]) -> io::Result<()> {
        let pos = Reader::new(body).i64()?;
        self.chunks.remove(&(pos as i32, (pos >> 32) as i32));
        Ok(())
    }

    /// `block_update` body. A block in a chunk that isn't loaded (or outside the height) is
    /// ignored, as vanilla does.
    pub fn block_update(&mut self, body: &[u8]) -> io::Result<()> {
        let mut r = Reader::new(body);
        let pos = r.i64()?;
        let state = self.state_id(r.varint()?.into())?;
        let (x, y, z) = ((pos >> 38) as i32, (pos << 52 >> 52) as i32, (pos << 26 >> 38) as i32);
        self.set(x, y, z, state);
        Ok(())
    }

    /// `section_blocks_update` body.
    pub fn section_update(&mut self, body: &[u8]) -> io::Result<()> {
        let mut r = Reader::new(body);
        let pos = r.i64()?;
        let (sx, sy, sz) = ((pos >> 42) as i32, (pos << 44 >> 44) as i32, (pos << 22 >> 42) as i32);
        let n = r.prefixed_len()?;
        for _ in 0..n {
            let v = read_varlong(&mut r)?;
            let state = self.state_id(v >> 12)?;
            let (x, z, y) = ((v >> 8) as i32 & 15, (v >> 4) as i32 & 15, v as i32 & 15);
            self.set(sx * 16 + x, sy * 16 + y, sz * 16 + z, state);
        }
        Ok(())
    }

    fn set(&mut self, x: i32, y: i32, z: i32, state: u32) {
        let Some(sy) = self.section_of(y) else { return };
        let i = index(x, y - self.min_y, z);
        if let Some(section) = self.chunks.get_mut(&(x >> 4, z >> 4)).and_then(|c| c.get_mut(sy)) {
            section.set(i, state);
        }
    }

    /// Section index of block y, if within the dimension's height.
    fn section_of(&self, y: i32) -> Option<usize> {
        let dy = y.checked_sub(self.min_y)?;
        (0..self.height).contains(&dy).then_some((dy / 16) as usize)
    }

    /// The block state at a block position; None when its chunk isn't loaded or y is outside the
    /// dimension's height.
    pub fn state_at(&self, x: i32, y: i32, z: i32) -> Option<u32> {
        let sy = self.section_of(y)?;
        let chunk = self.chunks.get(&(x >> 4, z >> 4))?;
        Some(chunk.get(sy)?.get(index(x, y - self.min_y, z)))
    }

    /// The block's name without namespace ("oak_stairs"); None for an unknown state id.
    pub fn block_name(&self, state: u32) -> Option<&'static str> {
        self.table.name(state)
    }

    /// The state's properties, sorted by name ("facing" before "half"); empty for blocks without
    /// properties and unknown state ids.
    pub fn block_properties(&self, state: u32) -> Vec<(&'static str, &'static str)> {
        self.table.properties(state)
    }

    /// Whether the state holds a fluid, as vanilla's non-empty FluidState: water and lava (any
    /// level), bubble columns, kelp and seagrass (always water-filled), and any `waterlogged=true`
    /// state (a waterlogged slab or stair still collides as a slab or stair).
    pub fn is_fluid(&self, state: u32) -> bool {
        let Some(i) = self.table.block_index(state) else { return false };
        self.fluid_blocks[i] || self.table.property(state, "waterlogged") == Some("true")
    }

    /// Approximate collision boxes of the block at (x,y,z), in world coordinates (appended to
    /// `out`); see [`shape`] for the heuristic. Outside the dimension's height there are none; a
    /// position in a chunk that isn't loaded gets a full cube, so a walking bot treats unloaded
    /// terrain as a wall (vanilla doesn't move the player into unloaded chunks either).
    pub fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if self.section_of(y).is_none() {
            return;
        }
        let (state, shape) = match self.state_at(x, y, z) {
            None => (0, Shape::Full),
            Some(s) => (s, self.table.block_index(s).map_or(Shape::Full, |i| self.shapes[i])),
        };
        shape::world_boxes(shape, self.table, state, [x, y, z], out);
    }

    /// Whether the chunk holding block column (x, z) is loaded.
    pub fn is_loaded(&self, x: i32, z: i32) -> bool {
        self.chunks.contains_key(&(x >> 4, z >> 4))
    }

    /// Blocks whose name contains `filter` (a leading `minecraft:` is ignored), within `radius`
    /// blocks of `center` on every axis (Chebyshev distance), nearest first (Euclidean distance,
    /// ties by x, y, z), at most `limit`. Only loaded chunks are searched.
    pub fn find_blocks(
        &self,
        filter: &str,
        center: [i32; 3],
        radius: i32,
        limit: usize,
    ) -> Vec<([i32; 3], u32)> {
        if radius < 0 || limit == 0 {
            return Vec::new();
        }
        let filter = filter.strip_prefix("minecraft:").unwrap_or(filter);
        let wanted_blocks: Vec<bool> = self.table.blocks.iter().map(|b| b.0.contains(filter)).collect();
        let wanted = |state: u32| self.table.block_index(state).is_some_and(|i| wanted_blocks[i]);
        let [cx, cy, cz] = center.map(i64::from);
        let r = i64::from(radius);
        let lo_y = (cy - r).max(i64::from(self.min_y));
        let hi_y = (cy + r).min(i64::from(self.min_y) + i64::from(self.height) - 1);
        // Max-heap of the best `limit` so far: (distance², x, y, z, state).
        let mut best: BinaryHeap<(i64, i64, i64, i64, u32)> = BinaryHeap::new();
        for (&(chunk_x, chunk_z), sections) in &self.chunks {
            let (x0, z0) = (i64::from(chunk_x) * 16, i64::from(chunk_z) * 16);
            let (lo_x, hi_x) = ((cx - r).max(x0), (cx + r).min(x0 + 15));
            let (lo_z, hi_z) = ((cz - r).max(z0), (cz + r).min(z0 + 15));
            if lo_x > hi_x || lo_z > hi_z {
                continue;
            }
            for (sy, section) in sections.iter().enumerate() {
                let y0 = i64::from(self.min_y) + sy as i64 * 16;
                let (lo, hi) = (lo_y.max(y0), hi_y.min(y0 + 15));
                if lo > hi || !section.may_contain(&wanted) {
                    continue;
                }
                // Per palette entry (or the single value) once, not per block.
                let palette_hits: Vec<bool> = match section {
                    Section::Packed { palette, .. } => palette.iter().map(|&p| wanted(p)).collect(),
                    _ => Vec::new(),
                };
                for y in lo..=hi {
                    for z in lo_z..=hi_z {
                        for x in lo_x..=hi_x {
                            let i = index(x as i32, (y - y0) as i32, z as i32);
                            let (hit, state) = match section {
                                Section::Single(v) => (true, *v),
                                Section::Packed { palette, bits, data } => {
                                    let p = unpack(data, *bits, i) as usize;
                                    (palette_hits[p], palette[p])
                                }
                                Section::Direct(d) => {
                                    let s = u32::from(d[i]);
                                    (wanted(s), s)
                                }
                            };
                            if !hit {
                                continue;
                            }
                            let d2 = (x - cx).pow(2) + (y - cy).pow(2) + (z - cz).pow(2);
                            let entry = (d2, x, y, z, state);
                            if best.len() < limit {
                                best.push(entry);
                            } else if best.peek().is_some_and(|worst| entry < *worst) {
                                best.pop();
                                best.push(entry);
                            }
                        }
                    }
                }
            }
        }
        best.into_sorted_vec()
            .into_iter()
            .map(|(_, x, y, z, state)| ([x as i32, y as i32, z as i32], state))
            .collect()
    }

    /// Number of chunks loaded.
    pub fn loaded_chunks(&self) -> usize {
        self.chunks.len()
    }

    /// Heap bytes held by the block storage (approximate, for tests and diagnostics).
    #[cfg(test)]
    fn heap_bytes(&self) -> usize {
        self.chunks
            .values()
            .map(|c| {
                c.len() * std::mem::size_of::<Section>() + c.iter().map(Section::heap_bytes).sum::<usize>()
            })
            .sum()
    }
}

/// `n` big-endian longs, as u64.
fn read_longs(r: &mut Reader, n: usize) -> io::Result<Box<[u64]>> {
    let (longs, _) = r.take(n * 8)?.as_chunks::<8>();
    Ok(longs.iter().map(|c| u64::from_be_bytes(*c)).collect())
}

fn read_varlong(r: &mut Reader) -> io::Result<i64> {
    let mut value: u64 = 0;
    for i in 0..10 {
        let b = r.u8()?;
        value |= u64::from(b & 0x7F) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(value as i64);
        }
    }
    Err(invalid("VarLong longer than 10 bytes"))
}
