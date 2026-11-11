use super::*;
use crate::net::Writer;

const PROTOCOLS: [i32; 4] = [777, 776, 775, 774];

// ---------------------------------------------------------------------------------------------
// Encoders (vanilla's layouts, written by hand)

/// `values` packed `bits` wide, `64 / bits` per long, as big-endian bytes.
fn packed(values: &[u64], bits: u32) -> Vec<u8> {
    let per = 64 / bits as usize;
    values
        .chunks(per)
        .flat_map(|c| c.iter().enumerate().fold(0u64, |l, (k, &v)| l | v << (k as u32 * bits)).to_be_bytes())
        .collect()
}

enum Container<'a> {
    Single(i32),
    /// Wire bits, storage bits, palette, palette index per entry.
    Palette(u8, u32, &'a [i32], &'a [u64]),
    /// Wire bits, storage bits, value per entry.
    Direct(u8, u32, &'a [u64]),
}

fn container(w: &mut Writer, c: &Container) {
    match c {
        Container::Single(v) => {
            w.u8(0).varint(*v);
        }
        Container::Palette(wire, bits, palette, indices) => {
            w.u8(*wire).varint(palette.len() as i32);
            for &p in *palette {
                w.varint(p);
            }
            w.bytes(&packed(indices, *bits));
        }
        Container::Direct(wire, bits, values) => {
            w.u8(*wire).bytes(&packed(values, *bits));
        }
    }
}

/// One chunk section in `protocol`'s layout.
fn section(w: &mut Writer, protocol: i32, states: &Container, biomes: &Container) {
    w.data.extend_from_slice(&4096i16.to_be_bytes()); // non-empty blocks (not checked)
    if protocol != 774 {
        w.data.extend_from_slice(&0i16.to_be_bytes()); // fluid count, 26.1+
    }
    container(w, states);
    container(w, biomes);
}

/// A `level_chunk_with_light` body around the section `data`, with heightmaps, a block entity
/// and light data that the world must skip or ignore.
fn chunk_packet(x: i32, z: i32, data: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    w.i32(x).i32(z);
    w.varint(2);
    for kind in [1, 4] {
        w.varint(kind).varint(37);
        for i in 0..37 {
            w.i64(i * 0x0101_0101);
        }
    }
    w.varint(data.len() as i32).bytes(data);
    // One block entity (packed xz, y, type, empty compound) and some light masks/arrays.
    w.varint(1).u8(0x35).bytes(&64i16.to_be_bytes()).varint(7).u8(10).u8(0);
    w.varint(1).i64(0b110).varint(0).varint(0).varint(0).varint(1).varint(2048).bytes(&[0xFF; 2048]);
    w.varint(0);
    w.data
}

/// A chunk whose sections are all `state` (single-valued), biomes single-valued.
fn uniform_chunk(protocol: i32, x: i32, z: i32, sections: usize, state: i32) -> Vec<u8> {
    let mut w = Writer::new();
    for _ in 0..sections {
        section(&mut w, protocol, &Container::Single(state), &Container::Single(0));
    }
    chunk_packet(x, z, &w.data)
}

fn block_pos(x: i32, y: i32, z: i32) -> i64 {
    ((i64::from(x) & 0x3FF_FFFF) << 38) | ((i64::from(z) & 0x3FF_FFFF) << 12) | (i64::from(y) & 0xFFF)
}

fn block_update(x: i32, y: i32, z: i32, state: u32) -> Vec<u8> {
    let mut w = Writer::new();
    w.i64(block_pos(x, y, z)).varint(state as i32);
    w.data
}

fn write_varlong(w: &mut Writer, v: i64) {
    let mut v = v as u64;
    while v >= 0x80 {
        w.u8((v as u8 & 0x7F) | 0x80);
        v >>= 7;
    }
    w.u8(v as u8);
}

/// `section_blocks_update` body: changes are (local x, y, z, state).
fn section_update(sx: i32, sy: i32, sz: i32, changes: &[(i32, i32, i32, u32)]) -> Vec<u8> {
    let mut w = Writer::new();
    w.i64(
        ((i64::from(sx) & 0x3F_FFFF) << 42)
            | ((i64::from(sz) & 0x3F_FFFF) << 20)
            | (i64::from(sy) & 0xF_FFFF),
    );
    w.varint(changes.len() as i32);
    for &(x, y, z, state) in changes {
        write_varlong(&mut w, i64::from(state) << 12 | i64::from(x << 8 | z << 4 | y));
    }
    w.data
}

fn forget(x: i32, z: i32) -> Vec<u8> {
    ((i64::from(x) & 0xFFFF_FFFF) | (i64::from(z) << 32)).to_be_bytes().to_vec()
}

/// The first state of block `name` with all of `props`.
fn state(world: &World, name: &str, props: &[(&str, &str)]) -> u32 {
    (0..world.table.states)
        .find(|&s| {
            world.block_name(s) == Some(name)
                && props.iter().all(|&(k, v)| world.table.property(s, k) == Some(v))
        })
        .unwrap_or_else(|| panic!("no state {name} {props:?}"))
}

fn boxes(world: &World, x: i32, y: i32, z: i32) -> Vec<Aabb> {
    let mut out = Vec::new();
    world.collision_boxes(x, y, z, &mut out);
    out
}

fn aabb(min: [f64; 3], max: [f64; 3]) -> Aabb {
    Aabb { min, max }
}

// ---------------------------------------------------------------------------------------------
// Chunk decoding

#[test]
fn loads_single_indirect_and_direct_sections_in_every_layout() {
    for protocol in PROTOCOLS {
        let mut world = World::new(protocol);
        world.reset(-64, 384, 65); // 65 biomes: 7-bit direct biome containers
        let global = world.table.global_bits();
        assert_eq!(global, if protocol == 777 { 16 } else { 15 });

        let (air, stone, dirt) = (0, 1, state(&world, "dirt", &[]) as i32);
        // Section 1: three states, wire bits 2 (stored in 4).
        let small: Vec<u64> = (0..4096).map(|i| i % 3).collect();
        // Section 2: 20 states with 5 bits (12 per long, 4 bits of each long unused).
        let palette20: Vec<i32> = (1..=20).collect();
        let mid: Vec<u64> = (0..4096).map(|i| (i * 7) % 20).collect();
        // Section 3: 300 distinct states, direct.
        let wide: Vec<u64> = (0..4096).map(|i| (i % 300) * 97 + 2).collect();
        let biomes_small: Vec<u64> = (0..64).map(|i| i % 3).collect();
        let biomes_direct: Vec<u64> = (0..64).map(|i| i % 65).collect();
        let biomes_one: Vec<u64> = (0..64).map(|i| i % 2).collect();

        let mut w = Writer::new();
        section(&mut w, protocol, &Container::Single(stone), &Container::Single(3));
        section(
            &mut w,
            protocol,
            &Container::Palette(2, 4, &[air, stone, dirt], &small),
            &Container::Palette(2, 2, &[0, 5, 9], &biomes_small),
        );
        section(
            &mut w,
            protocol,
            &Container::Palette(5, 5, &palette20, &mid),
            &Container::Direct(7, 7, &biomes_direct),
        );
        section(
            &mut w,
            protocol,
            &Container::Direct(global as u8, global, &wide),
            &Container::Palette(1, 1, &[4, 6], &biomes_one),
        );
        for _ in 4..24 {
            section(&mut w, protocol, &Container::Single(air), &Container::Single(0));
        }
        assert_eq!(world.load_chunk(&chunk_packet(-3, 5, &w.data)).unwrap(), (-3, 5), "protocol {protocol}");
        assert_eq!(world.loaded_chunks(), 1);

        let (x0, z0) = (-48, 80); // the chunk's north-west corner
        let at = |lx: i32, y: i32, lz: i32| world.state_at(x0 + lx, y, z0 + lz).unwrap();
        let i = |lx: i32, ly: i32, lz: i32| ((ly * 16 + lz) * 16 + lx) as u64;
        assert_eq!(at(0, -64, 0), 1);
        assert_eq!(at(15, -49, 15), 1);
        for (lx, ly, lz) in [(0, 0, 0), (1, 0, 0), (2, 0, 0), (5, 3, 9), (15, 15, 15)] {
            assert_eq!(u64::from(at(lx, -48 + ly, lz)), [0, 1, dirt as u64][(i(lx, ly, lz) % 3) as usize]);
            assert_eq!(
                u64::from(at(lx, -32 + ly, lz)),
                palette20[((i(lx, ly, lz) * 7) % 20) as usize] as u64
            );
            assert_eq!(u64::from(at(lx, -16 + ly, lz)), (i(lx, ly, lz) % 300) * 97 + 2);
        }
        assert_eq!(at(7, 0, 7), 0);
        assert_eq!(at(7, 319, 7), 0);
        assert_eq!(world.state_at(x0, 320, z0), None, "above the build height");
        assert_eq!(world.state_at(x0, -65, z0), None, "below min y");
        assert_eq!(world.state_at(x0 + 16, 0, z0), None, "chunk not loaded");
        assert!(world.is_loaded(-48, 95) && world.is_loaded(-33, 80));
        assert!(!world.is_loaded(-49, 80) && !world.is_loaded(-48, 96));
    }
}

#[test]
fn direct_biomes_use_the_registry_size_or_else_the_wire_bits() {
    let biomes: Vec<u64> = (0..64).map(|i| i % 40).collect();
    let mut w = Writer::new();
    // 40 biomes need 6 bits; the bits byte says 6 too.
    section(&mut w, 777, &Container::Single(1), &Container::Direct(6, 6, &biomes));
    section(&mut w, 777, &Container::Single(2), &Container::Single(0));
    let body = chunk_packet(0, 0, &w.data);
    let mut world = World::new(777);
    world.reset(0, 32, 40);
    world.load_chunk(&body).unwrap();
    assert_eq!(world.state_at(0, 16, 0), Some(2));
    world.reset(0, 32, 0); // unknown registry size: trust the bits byte
    world.load_chunk(&body).unwrap();
    assert_eq!(world.state_at(0, 16, 0), Some(2));
}

#[test]
fn bad_chunks_are_errors() {
    let mut world = World::new(777);
    world.reset(0, 32, 64);
    // Truncated: one section where two are needed.
    let mut w = Writer::new();
    section(&mut w, 777, &Container::Single(1), &Container::Single(0));
    assert!(world.load_chunk(&chunk_packet(0, 0, &w.data)).is_err());
    // A state id past the table.
    let mut w = Writer::new();
    for _ in 0..2 {
        section(&mut w, 777, &Container::Single(40_000), &Container::Single(0));
    }
    assert!(world.load_chunk(&chunk_packet(0, 0, &w.data)).is_err());
    // A palette index past the palette.
    let mut w = Writer::new();
    let indices: Vec<u64> = (0..4096).map(|i| u64::from(i == 77) * 2).collect();
    for _ in 0..2 {
        section(&mut w, 777, &Container::Palette(4, 4, &[0, 1], &indices), &Container::Single(0));
    }
    assert!(world.load_chunk(&chunk_packet(0, 0, &w.data)).is_err());
    assert_eq!(world.loaded_chunks(), 0);
    // 1.21.11's layout (no fluid count) only reads as 1.21.11.
    let body = uniform_chunk(774, 0, 0, 2, 1);
    let mut old = World::new(774);
    old.reset(0, 32, 64);
    assert!(old.load_chunk(&body).is_ok());
    assert!(world.load_chunk(&body).is_err());
}

// ---------------------------------------------------------------------------------------------
// Updates

#[test]
fn block_updates_and_section_updates_change_single_palette_and_direct_sections() {
    for protocol in PROTOCOLS {
        let mut world = World::new(protocol);
        world.reset(-64, 384, 65);
        world.load_chunk(&uniform_chunk(protocol, -1, -1, 24, 0)).unwrap();
        let stone = state(&world, "stone", &[]);

        // A single-valued section gets a palette.
        world.block_update(&block_update(-5, -60, -7, stone)).unwrap();
        assert_eq!(world.state_at(-5, -60, -7), Some(stone));
        assert_eq!(world.state_at(-6, -60, -7), Some(0));
        assert_eq!(world.state_at(-5, -59, -7), Some(0));

        // Section y = -4 (blocks -64..-49) of chunk (-1, -1): 20 distinct states grow the
        // palette past 4 bits, then 300 make it direct.
        let changes: Vec<_> = (0..20).map(|k| (k % 16, k / 16, 3, 10 + k as u32)).collect();
        world.section_update(&section_update(-1, -4, -1, &changes)).unwrap();
        for &(x, y, z, s) in &changes {
            assert_eq!(world.state_at(-16 + x, -64 + y, -16 + z), Some(s));
        }
        assert_eq!(world.state_at(-5, -60, -7), Some(stone), "earlier change kept");
        let changes: Vec<_> =
            (0..300).map(|k| (k % 16, 9 + k / 256, (k / 16) % 16, 1000 + k as u32)).collect();
        world.section_update(&section_update(-1, -4, -1, &changes)).unwrap();
        assert!(matches!(world.chunks[&(-1, -1)][0], Section::Direct(_)));
        for &(x, y, z, s) in &changes {
            assert_eq!(world.state_at(-16 + x, -64 + y, -16 + z), Some(s));
        }
        assert_eq!(world.state_at(-16 + 4, -64, -16 + 3), Some(14), "palette entries survive");
        assert_eq!(world.state_at(-5, -60, -7), Some(stone));
        assert_eq!(world.state_at(-1, -49, -1), Some(0));

        // Updates in chunks that aren't loaded, or outside the height, are ignored.
        world.block_update(&block_update(100, 0, 100, stone)).unwrap();
        world.block_update(&block_update(-5, 400, -7, stone)).unwrap();
        world.section_update(&section_update(5, 0, 5, &[(1, 2, 3, stone)])).unwrap();
        assert_eq!(world.state_at(100, 0, 100), None);
        // Unknown state ids are refused.
        assert!(world.block_update(&block_update(-5, -60, -7, 99_999)).is_err());
        assert!(world.block_update(&[0; 3]).is_err());
    }
}

#[test]
fn forget_chunk_unloads_the_column() {
    let mut world = World::new(777);
    world.reset(0, 16, 64);
    for (x, z) in [(-2, 7), (3, -9), (0, 0)] {
        world.load_chunk(&uniform_chunk(777, x, z, 1, 1)).unwrap();
    }
    assert_eq!(world.loaded_chunks(), 3);
    world.forget_chunk(&forget(-2, 7)).unwrap();
    world.forget_chunk(&forget(3, -9)).unwrap();
    world.forget_chunk(&forget(50, 50)).unwrap(); // never loaded: fine
    assert_eq!(world.loaded_chunks(), 1);
    assert!(!world.is_loaded(-32, 112) && !world.is_loaded(48, -144) && world.is_loaded(0, 0));
    assert!(world.forget_chunk(&[1, 2]).is_err());
    world.reset(-64, 384, 64);
    assert_eq!((world.loaded_chunks(), world.min_y(), world.height()), (0, -64, 384));
}

#[test]
fn a_9x9_area_stays_small() {
    let mut world = World::new(777);
    world.reset(-64, 384, 65);
    let mut w = Writer::new();
    let terrain: Vec<u64> = (0..4096u64).map(|i| (i * 2654435761) % 16).collect();
    let palette: Vec<i32> = (1..=16).collect();
    for sy in 0..24 {
        if (4..8).contains(&sy) {
            section(&mut w, 777, &Container::Palette(4, 4, &palette, &terrain), &Container::Single(0));
        } else {
            section(&mut w, 777, &Container::Single(if sy < 4 { 1 } else { 0 }), &Container::Single(0));
        }
    }
    for x in -4..=4 {
        for z in -4..=4 {
            world.load_chunk(&chunk_packet(x, z, &w.data)).unwrap();
        }
    }
    assert_eq!(world.loaded_chunks(), 81);
    let bytes = world.heap_bytes();
    assert!(bytes < 1 << 20, "{bytes} bytes");
}

// ---------------------------------------------------------------------------------------------
// Tables (expected ids read off the vanilla reports; 775 off minecraft-data 26.1)

#[test]
fn tables_match_the_reports() {
    struct Expect {
        protocol: i32,
        states: u32,
        oak_stairs_default: u32,
        oak_stairs_east_top_outer_left_wet: u32,
        chest_left_north_wet: u32,
        water_0: u32,
        oak_slab_bottom_dry: u32,
        oak_fence_east: u32,
        snow_3: u32,
        diamond_ore: u32,
        note_block_banjo_12_powered: u32,
        oak_log_x: u32,
    }
    #[rustfmt::skip]
    let expected = [
        Expect { protocol: 777, states: 35723, oak_stairs_default: 5474, oak_stairs_east_top_outer_left_wet: 5529, chest_left_north_wet: 5545, water_0: 89, oak_slab_bottom_dry: 15180, oak_fence_east: 8650, snow_3: 8591, diamond_ore: 6863, note_block_banjo_12_powered: 1404, oak_log_x: 139 },
        Expect { protocol: 776, states: 32366, oak_stairs_default: 3918, oak_stairs_east_top_outer_left_wet: 3973, chest_left_north_wet: 3989, water_0: 86, oak_slab_bottom_dry: 13333, oak_fence_east: 6980, snow_3: 6921, diamond_ore: 5307, note_block_banjo_12_powered: 1305, oak_log_x: 136 },
        Expect { protocol: 775, states: 29873, oak_stairs_default: 3918, oak_stairs_east_top_outer_left_wet: 3973, chest_left_north_wet: 3989, water_0: 86, oak_slab_bottom_dry: 13333, oak_fence_east: 6980, snow_3: 6921, diamond_ore: 5307, note_block_banjo_12_powered: 1305, oak_log_x: 136 },
        Expect { protocol: 774, states: 29671, oak_stairs_default: 3717, oak_stairs_east_top_outer_left_wet: 3772, chest_left_north_wet: 3788, water_0: 86, oak_slab_bottom_dry: 13131, oak_fence_east: 6779, snow_3: 6720, diamond_ore: 5106, note_block_banjo_12_powered: 1305, oak_log_x: 136 },
    ];
    for e in expected {
        let w = World::new(e.protocol);
        let p = e.protocol;
        assert_eq!(w.table.states, e.states, "{p}");
        assert!(w.table.states <= 1 << 16, "direct sections store u16");
        assert_eq!((w.block_name(0), w.block_name(1)), (Some("air"), Some("stone")), "{p}");
        assert_eq!(w.block_name(e.states - 1), Some("firefly_bush"), "{p}");
        assert_eq!(w.block_name(e.states), None, "{p}");
        assert!(w.block_properties(e.states).is_empty() && w.block_properties(1).is_empty());
        assert_eq!(w.block_name(e.oak_stairs_default), Some("oak_stairs"));
        assert_eq!(
            w.block_properties(e.oak_stairs_default),
            [("facing", "north"), ("half", "bottom"), ("shape", "straight"), ("waterlogged", "false")],
            "{p}"
        );
        assert_eq!(
            w.block_properties(e.oak_stairs_east_top_outer_left_wet),
            [("facing", "east"), ("half", "top"), ("shape", "outer_left"), ("waterlogged", "true")],
            "{p}"
        );
        // Chest declares type, facing, waterlogged; ids follow the sorted names.
        assert_eq!(w.block_name(e.chest_left_north_wet), Some("chest"));
        assert_eq!(
            w.block_properties(e.chest_left_north_wet),
            [("facing", "north"), ("type", "left"), ("waterlogged", "true")],
            "{p}"
        );
        assert_eq!(
            w.block_properties(e.chest_left_north_wet - 2),
            [("facing", "north"), ("type", "single"), ("waterlogged", "true")]
        );
        assert_eq!(w.block_name(e.water_0), Some("water"));
        assert_eq!(w.block_properties(e.water_0), [("level", "0")]);
        assert_eq!(w.block_properties(e.water_0 + 15), [("level", "15")]);
        assert_eq!(w.block_name(e.water_0 + 16), Some("lava"));
        assert_eq!(
            w.block_properties(e.oak_slab_bottom_dry),
            [("type", "bottom"), ("waterlogged", "false")],
            "{p}"
        );
        assert_eq!(w.block_name(e.oak_fence_east), Some("oak_fence"));
        assert_eq!(
            w.block_properties(e.oak_fence_east),
            [
                ("east", "true"),
                ("north", "false"),
                ("south", "false"),
                ("waterlogged", "false"),
                ("west", "false")
            ],
            "{p}"
        );
        assert_eq!((w.block_name(e.snow_3), w.table.property(e.snow_3, "layers")), (Some("snow"), Some("3")));
        assert_eq!(w.block_name(e.diamond_ore), Some("diamond_ore"));
        assert_eq!(w.block_name(e.note_block_banjo_12_powered), Some("note_block"));
        assert_eq!(
            w.block_properties(e.note_block_banjo_12_powered),
            [("instrument", "banjo"), ("note", "12"), ("powered", "true")],
            "{p}"
        );
        assert_eq!(w.block_properties(e.oak_log_x), [("axis", "x")]);
        assert_eq!(w.block_name(e.oak_log_x), Some("oak_log"));
    }
    // Only 26.2+ has sulfur; only 26.1+ has the trumpet note block instruments.
    let has = |p: i32, name: &str| World::new(p).table.blocks.iter().any(|b| b.0 == name);
    assert!(has(777, "sulfur") && has(776, "sulfur") && !has(775, "sulfur") && !has(774, "sulfur"));
    assert!(World::new(775).table.props.iter().any(|p| p.1.contains(&"trumpet")));
    assert!(!World::new(774).table.props.iter().any(|p| p.1.contains(&"trumpet")));
}

#[test]
fn fluids() {
    let w = World::new(777);
    let water = state(&w, "water", &[("level", "3")]);
    let lava = state(&w, "lava", &[]);
    let wet_slab = state(&w, "oak_slab", &[("waterlogged", "true")]);
    let dry_slab = state(&w, "oak_slab", &[("waterlogged", "false")]);
    let kelp = state(&w, "kelp", &[]);
    assert!(w.is_fluid(water) && w.is_fluid(lava) && w.is_fluid(wet_slab) && w.is_fluid(kelp));
    assert!(!w.is_fluid(dry_slab) && !w.is_fluid(0) && !w.is_fluid(1) && !w.is_fluid(u32::MAX));
}

// ---------------------------------------------------------------------------------------------
// Collision and search

/// A 1x1-section world at chunk (0, 0), y 0..16, filled with air, with `blocks` placed.
fn world_with(blocks: &[([i32; 3], u32)]) -> World {
    let mut world = World::new(777);
    world.reset(0, 16, 64);
    world.load_chunk(&uniform_chunk(777, 0, 0, 1, 0)).unwrap();
    for &([x, y, z], s) in blocks {
        world.block_update(&block_update(x, y, z, s)).unwrap();
    }
    world
}

#[test]
fn collision_boxes() {
    let w = World::new(777);
    let stone = 1;
    let slab = |t| state(&w, "oak_slab", &[("type", t), ("waterlogged", "false")]);
    let carpet = state(&w, "white_carpet", &[]);
    let fence = state(
        &w,
        "oak_fence",
        &[("east", "true"), ("north", "false"), ("south", "false"), ("west", "false")],
    );
    let snow = state(&w, "snow", &[("layers", "3")]);
    let stairs = state(&w, "oak_stairs", &[("facing", "north"), ("half", "bottom"), ("shape", "straight")]);
    let door = state(
        &w,
        "oak_door",
        &[("facing", "north"), ("open", "true"), ("hinge", "right"), ("half", "lower")],
    );
    let gate = state(&w, "oak_fence_gate", &[("facing", "east"), ("open", "false")]);
    let open_gate = state(&w, "oak_fence_gate", &[("open", "true")]);
    let grass = state(&w, "short_grass", &[]);
    let torch = state(&w, "wall_torch", &[]);
    let world = world_with(&[
        ([1, 2, 3], stone),
        ([2, 2, 3], slab("bottom")),
        ([3, 2, 3], slab("top")),
        ([4, 2, 3], slab("double")),
        ([5, 2, 3], carpet),
        ([6, 2, 3], fence),
        ([7, 2, 3], snow),
        ([8, 2, 3], stairs),
        ([9, 2, 3], door),
        ([10, 2, 3], gate),
        ([11, 2, 3], open_gate),
        ([12, 2, 3], grass),
        ([13, 2, 3], torch),
    ]);
    assert_eq!(boxes(&world, 1, 2, 3), [aabb([1.0, 2.0, 3.0], [2.0, 3.0, 4.0])]);
    assert_eq!(boxes(&world, 0, 2, 3), [], "air");
    assert_eq!(boxes(&world, 2, 2, 3), [aabb([2.0, 2.0, 3.0], [3.0, 2.5, 4.0])]);
    assert_eq!(boxes(&world, 3, 2, 3), [aabb([3.0, 2.5, 3.0], [4.0, 3.0, 4.0])]);
    assert_eq!(boxes(&world, 4, 2, 3), [aabb([4.0, 2.0, 3.0], [5.0, 3.0, 4.0])]);
    assert_eq!(boxes(&world, 5, 2, 3), [aabb([5.0, 2.0, 3.0], [6.0, 2.0625, 4.0])]);
    assert_eq!(
        boxes(&world, 6, 2, 3),
        [aabb([6.375, 2.0, 3.375], [6.625, 3.5, 3.625]), aabb([6.5, 2.0, 3.375], [7.0, 3.5, 3.625])],
        "fence post and east arm, 1.5 high"
    );
    assert_eq!(boxes(&world, 7, 2, 3), [aabb([7.0, 2.0, 3.0], [8.0, 2.25, 4.0])], "3 snow layers");
    assert_eq!(
        boxes(&world, 8, 2, 3),
        [
            aabb([8.0, 2.0, 3.0], [9.0, 2.5, 4.0]),
            aabb([8.0, 2.5, 3.0], [8.5, 3.0, 3.5]),
            aabb([8.5, 2.5, 3.0], [9.0, 3.0, 3.5]),
        ],
        "stairs: slab plus the north half"
    );
    assert_eq!(
        boxes(&world, 9, 2, 3),
        [aabb([9.8125, 2.0, 3.0], [10.0, 3.0, 4.0])],
        "open door, right hinge"
    );
    assert_eq!(boxes(&world, 10, 2, 3), [aabb([10.375, 2.0, 3.0], [10.625, 3.5, 4.0])], "closed gate");
    assert_eq!(boxes(&world, 11, 2, 3), [], "open gate");
    assert_eq!(boxes(&world, 12, 2, 3), [], "grass");
    assert_eq!(boxes(&world, 13, 2, 3), [], "torch");
    assert_eq!(boxes(&world, 20, 2, 3), [aabb([20.0, 2.0, 3.0], [21.0, 3.0, 4.0])], "unloaded: a wall");
    assert_eq!(boxes(&world, 1, -1, 3), [], "below the world");
    assert_eq!(boxes(&world, 1, 16, 3), [], "above the world");
    let mut out = vec![aabb([0.0; 3], [0.0; 3])];
    world.collision_boxes(1, 2, 3, &mut out);
    assert_eq!(out.len(), 2, "appends");
}

#[test]
fn classification_spot_checks() {
    use shape::{Shape, classify};
    #[rustfmt::skip]
    const NO_COLLISION: &[&str] = &[
        "air", "cave_air", "water", "lava", "short_grass", "tall_grass", "poppy", "sunflower",
        "oak_sapling", "wheat", "torch", "redstone_wall_torch", "oak_sign", "oak_wall_hanging_sign",
        "white_banner", "stone_button", "lever", "oak_pressure_plate", "rail", "powered_rail",
        "redstone_wire", "tripwire", "fire", "cobweb", "light", "structure_void", "vine", "kelp",
        "seagrass", "brain_coral_fan", "dead_tube_coral_wall_fan", "brown_mushroom", "sugar_cane",
        "glow_lichen",
    ];
    #[rustfmt::skip]
    const FULL: &[&str] = &[
        "stone", "grass_block", "oak_leaves", "brown_mushroom_block", "brain_coral_block",
        "piston_head", "sea_lantern", "jack_o_lantern", "glass", "oak_log", "chain_command_block",
        "barrier",
    ];
    for &name in NO_COLLISION {
        assert_eq!(classify(name), Shape::Empty, "{name}");
    }
    for &name in FULL {
        assert_eq!(classify(name), Shape::Full, "{name}");
    }
    assert!(matches!(classify("potted_oak_sapling"), Shape::Boxes(_)));
    assert!(matches!(classify("zombie_wall_head"), Shape::Boxes(_)));
    assert_eq!(classify("cobblestone_wall"), Shape::Wall);
    assert_eq!(classify("nether_brick_fence"), Shape::Fence);
    assert_eq!(classify("iron_trapdoor"), Shape::Trapdoor);
    assert_eq!(classify("copper_door"), Shape::Door);
    assert_eq!(classify("waxed_copper_bars"), Shape::Pane);
    assert_eq!(classify("iron_chain"), Shape::Chain);
    assert_eq!(classify("waxed_lightning_rod"), Shape::Rod);
}

#[test]
fn find_blocks_nearest_first() {
    let w = World::new(777);
    let diamond = state(&w, "diamond_ore", &[]);
    let deep = state(&w, "deepslate_diamond_ore", &[]);
    let mut world =
        world_with(&[([1, 1, 1], diamond), ([8, 8, 8], deep), ([9, 8, 8], diamond), ([15, 15, 15], diamond)]);
    let found = world.find_blocks("diamond_ore", [8, 8, 9], 16, 10);
    assert_eq!(
        found,
        [([8, 8, 8], deep), ([9, 8, 8], diamond), ([15, 15, 15], diamond), ([1, 1, 1], diamond)]
    );
    assert_eq!(world.find_blocks("minecraft:deepslate_diamond", [0, 0, 0], 16, 10), [([8, 8, 8], deep)]);
    assert_eq!(world.find_blocks("diamond", [8, 8, 9], 16, 2), [([8, 8, 8], deep), ([9, 8, 8], diamond)]);
    assert_eq!(world.find_blocks("diamond", [0, 0, 0], 1, 10), [([1, 1, 1], diamond)], "Chebyshev radius");
    assert_eq!(world.find_blocks("diamond", [0, 0, 0], 0, 10), []);
    assert!(world.find_blocks("emerald", [0, 0, 0], 50, 10).is_empty());
    assert!(world.find_blocks("diamond", [0, 0, 0], 50, 0).is_empty());
    // A uniform section matches as a whole; neighbouring chunks are searched too.
    world.load_chunk(&uniform_chunk(777, 1, 0, 1, diamond as i32)).unwrap();
    let found = world.find_blocks("diamond", [14, 3, 3], 2, 100);
    assert_eq!(found.len(), 25, "the x = 16 slice of the neighbouring chunk");
    assert_eq!(found[0], ([16, 3, 3], diamond));
}
