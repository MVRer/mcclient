//! Approximate collision shapes, for walking. Vanilla's shapes live in Java code, not in the data
//! reports, so blocks are classified by name (once per [`super::World`]) and the few shapes that
//! depend on the state read its properties. The heuristic:
//!
//! - **Full cube** for anything not listed below (stone, logs, leaves, glass, ores, machines, ...),
//!   and for blocks whose real shape is close to a cube or hollow (hopper, cauldron, composter,
//!   anvil, lectern, grindstone, bell, piston head, chorus plant, azalea, shelves, statues, 26.3's
//!   shelf mushrooms (a solid shelf you stand on), ...).
//! - **No collision**: air variants, water, lava, bubble columns, `light`, `structure_void`,
//!   fire, cobweb, sweet berry bushes, powder snow (you sink into it), portals and end gateways,
//!   moving pistons, frogspawn; plants: grass and ferns (short/tall/dry), bushes, dead bushes and
//!   26.3's red shrubs,
//!   flowers (incl. tall flowers, petals, wildflowers, leaf litter, eyeblossoms), saplings,
//!   propagules, crops and stems, nether wart, cocoa, mushrooms and fungi, roots and sprouts,
//!   sugar cane, kelp, seagrass, vines (incl. cave/twisting/weeping), glow lichen, sculk veins,
//!   resin clumps, hanging roots/moss, spore blossoms, small dripleaf and dripleaf stems, coral
//!   plants and fans; and torches, signs and hanging signs, banners, buttons, levers, pressure
//!   plates, rails, redstone wire, tripwire and tripwire hooks.
//! - **By state**: slabs (`type`: bottom/top half, double full); stairs (half slab plus the raised
//!   quadrants of `facing`/`shape`/`half`, as vanilla's StairBlock); snow (`layers`: (layers-1)/8
//!   high); fences (1.5 high post and arms toward `north`/`east`/`south`/`west` = true); walls (1.5
//!   high post if `up`, arms where a side is low/tall); fence gates (1.5 high bar across when
//!   closed, nothing when `open`); glass panes and bars (thin post and arms, 1 high); doors (3/16
//!   thick panel on the side vanilla uses for `facing`/`open`/`hinge`); trapdoors (3/16 slab at the
//!   bottom/top when closed, a panel against `facing` when open); ladders (3/16 panel); chains and
//!   rods (thin bar along `axis`/`facing`); big dripleaf (leaf only while `tilt` is none/unstable);
//!   scaffolding (its top 2/16, so it can be stood on and walked through).
//! - **Fixed partial boxes**: carpets 1/16; lily pads 1.5/16; repeaters, comparators 2/16;
//!   daylight detectors 6/16; campfires 7/16; sculk sensors/shriekers 8/16; beds 9/16;
//!   stonecutters 9/16; enchanting tables 12/16; end portal frames 13/16; soul sand, mud 14/16;
//!   farmland, dirt paths 15/16; chests (all kinds) 14/16 inset by 1/16; cakes 8/16 inset;
//!   cactus, honey blocks 15/16 inset; and small centred boxes for flower pots, heads and skulls,
//!   lanterns, candles, sea pickles, turtle eggs, amethyst buds, conduits, heavy cores, brewing
//!   stands, dried ghasts, bamboo, pointed dripstone and sulfur spikes (orientation ignored).

use super::Aabb;
use super::blocks::Table;

/// A block's collision class.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Empty,
    Full,
    /// Fixed boxes in 1/16 block units: `[x0, y0, z0, x1, y1, z1]`.
    Boxes(&'static [[f64; 6]]),
    Slab,
    Stairs,
    Snow,
    Fence,
    Wall,
    FenceGate,
    Pane,
    Door,
    Trapdoor,
    Ladder,
    /// Thin bar along `axis` (chains).
    Chain,
    /// Thin bar along `facing` (end rods, lightning rods).
    Rod,
    BigDripleaf,
    Scaffolding,
}

const fn height(h: f64) -> [[f64; 6]; 1] {
    [[0.0, 0.0, 0.0, 16.0, h, 16.0]]
}
const fn inset(i: f64, h: f64) -> [[f64; 6]; 1] {
    [[i, 0.0, i, 16.0 - i, h, 16.0 - i]]
}

static CARPET: [[f64; 6]; 1] = height(1.0);
static LILY_PAD: [[f64; 6]; 1] = [[1.0, 0.0, 1.0, 15.0, 1.5, 15.0]];
static H2: [[f64; 6]; 1] = height(2.0);
static H6: [[f64; 6]; 1] = height(6.0);
static H7: [[f64; 6]; 1] = height(7.0);
static H8: [[f64; 6]; 1] = height(8.0);
static H9: [[f64; 6]; 1] = height(9.0);
static H12: [[f64; 6]; 1] = height(12.0);
static H13: [[f64; 6]; 1] = height(13.0);
static H14: [[f64; 6]; 1] = height(14.0);
static H15: [[f64; 6]; 1] = height(15.0);
static CHEST: [[f64; 6]; 1] = inset(1.0, 14.0);
static CAKE: [[f64; 6]; 1] = inset(1.0, 8.0);
static CACTUS: [[f64; 6]; 1] = inset(1.0, 15.0);
static POT: [[f64; 6]; 1] = inset(5.0, 6.0);
static HEAD: [[f64; 6]; 1] = inset(4.0, 8.0);
static LANTERN: [[f64; 6]; 2] = [[5.0, 0.0, 5.0, 11.0, 7.0, 11.0], [6.0, 7.0, 6.0, 10.0, 9.0, 10.0]];
static SMALL: [[f64; 6]; 1] = inset(6.0, 6.0);
static EGG: [[f64; 6]; 1] = inset(3.0, 7.0);
static CONDUIT: [[f64; 6]; 1] = [[5.0, 5.0, 5.0, 11.0, 11.0, 11.0]];
static BREWING: [[f64; 6]; 2] = [[1.0, 0.0, 1.0, 15.0, 2.0, 15.0], [7.0, 0.0, 7.0, 9.0, 14.0, 9.0]];
static GHAST: [[f64; 6]; 1] = inset(3.0, 10.0);
static BAMBOO: [[f64; 6]; 1] = inset(6.5, 16.0);
static SPIKE: [[f64; 6]; 1] = inset(5.0, 16.0);
static DECORATED_POT: [[f64; 6]; 1] = inset(1.0, 16.0);

/// Blocks without collision, by exact name (suffix rules are in [`classify`]).
#[rustfmt::skip]
const EMPTY: &[&str] = &[
    "air", "cave_air", "void_air", "water", "lava", "bubble_column", "light", "structure_void",
    "fire", "soul_fire", "cobweb", "sweet_berry_bush", "powder_snow", "nether_portal", "end_portal",
    "end_gateway", "moving_piston", "frogspawn", "tripwire", "tripwire_hook", "redstone_wire",
    "lever", "rail", "short_grass", "tall_grass", "fern", "large_fern", "short_dry_grass",
    "tall_dry_grass", "dead_bush", "bush", "firefly_bush", "red_shrub", "seagrass", "tall_seagrass",
    "kelp", "kelp_plant", "sugar_cane", "vine", "glow_lichen", "sculk_vein", "resin_clump",
    "hanging_roots", "spore_blossom", "pale_hanging_moss", "cave_vines", "cave_vines_plant",
    "twisting_vines", "twisting_vines_plant", "weeping_vines", "weeping_vines_plant", "wheat",
    "carrots", "potatoes", "beetroots", "melon_stem", "pumpkin_stem", "attached_melon_stem",
    "attached_pumpkin_stem", "torchflower_crop", "pitcher_crop", "nether_wart", "cocoa",
    "brown_mushroom", "red_mushroom", "crimson_fungus", "warped_fungus", "crimson_roots",
    "warped_roots", "nether_sprouts", "mangrove_propagule", "small_dripleaf", "big_dripleaf_stem",
    "dandelion", "golden_dandelion", "poppy", "blue_orchid", "allium", "azure_bluet", "red_tulip",
    "orange_tulip", "white_tulip", "pink_tulip", "oxeye_daisy", "cornflower", "lily_of_the_valley",
    "wither_rose", "torchflower", "sunflower", "lilac", "rose_bush", "peony", "pitcher_plant",
    "open_eyeblossom", "closed_eyeblossom", "cactus_flower", "pink_petals", "wildflowers",
    "leaf_litter",
];

/// The collision class of the block called `name` (no namespace).
pub fn classify(name: &str) -> Shape {
    use Shape::*;
    let ends = |s: &str| name.ends_with(s);
    if EMPTY.contains(&name)
        || (ends("_sapling") && !name.starts_with("potted_"))
        || ends("torch")
        || ends("_sign")
        || ends("_banner")
        || ends("_button")
        || ends("_pressure_plate")
        || ends("_rail")
        || ends("_coral")
        || ends("_coral_fan")
        || ends("_coral_wall_fan")
    {
        return Empty;
    }
    if name == "flower_pot" || name.starts_with("potted_") {
        return Boxes(&POT);
    }
    if ends("_slab") {
        return Slab;
    }
    if ends("_stairs") {
        return Stairs;
    }
    if ends("_fence_gate") {
        return FenceGate;
    }
    if ends("_fence") {
        return Fence;
    }
    if ends("_wall") {
        return Wall;
    }
    if ends("_pane") || ends("_bars") {
        return Pane;
    }
    if ends("_trapdoor") {
        return Trapdoor;
    }
    if ends("_door") {
        return Door;
    }
    if (ends("_head") && name != "piston_head") || ends("_skull") {
        return Boxes(&HEAD);
    }
    if ends("lantern") && name != "jack_o_lantern" && name != "sea_lantern" {
        return Boxes(&LANTERN);
    }
    if ends("carpet") {
        return Boxes(&CARPET);
    }
    if ends("_bed") {
        return Boxes(&H9);
    }
    if ends("chest") {
        return Boxes(&CHEST);
    }
    if name == "cake" || ends("candle_cake") {
        return Boxes(&CAKE);
    }
    if name == "candle" || ends("_candle") || name == "sea_pickle" {
        return Boxes(&SMALL);
    }
    if ends("chain") {
        return Chain;
    }
    if ends("_rod") {
        return Rod;
    }
    if ends("amethyst_bud") || name == "amethyst_cluster" || name == "turtle_egg" {
        return Boxes(&EGG);
    }
    match name {
        "snow" => Snow,
        "ladder" => Ladder,
        "big_dripleaf" => BigDripleaf,
        "scaffolding" => Scaffolding,
        "lily_pad" => Boxes(&LILY_PAD),
        "repeater" | "comparator" => Boxes(&H2),
        "daylight_detector" => Boxes(&H6),
        "campfire" | "soul_campfire" => Boxes(&H7),
        "sculk_sensor" | "calibrated_sculk_sensor" | "sculk_shrieker" => Boxes(&H8),
        "stonecutter" => Boxes(&H9),
        "enchanting_table" => Boxes(&H12),
        "end_portal_frame" => Boxes(&H13),
        "soul_sand" | "mud" => Boxes(&H14),
        "farmland" | "dirt_path" => Boxes(&H15),
        "cactus" | "honey_block" => Boxes(&CACTUS),
        "conduit" => Boxes(&CONDUIT),
        "heavy_core" => Boxes(&HEAD),
        "brewing_stand" => Boxes(&BREWING),
        "dried_ghast" => Boxes(&GHAST),
        "bamboo" => Boxes(&BAMBOO),
        "pointed_dripstone" | "sulfur_spike" => Boxes(&SPIKE),
        "decorated_pot" => Boxes(&DECORATED_POT),
        _ => Full,
    }
}

/// Raised quadrants of a stair (bit 1 = north-west, 2 = north-east, 4 = south-west, 8 =
/// south-east), by `shape` ordinal * 4 + `facing` (south, west, north, east): vanilla's
/// StairBlock.SHAPE_BY_STATE.
const STAIR_QUADRANTS: [u8; 20] = [12, 5, 3, 10, 14, 13, 7, 11, 13, 7, 11, 14, 8, 4, 1, 2, 4, 1, 2, 8];

/// A 3/16 panel against the side vanilla's door/trapdoor/ladder shapes use for `dir`.
fn panel(dir: &str) -> [f64; 6] {
    match dir {
        "north" => [0.0, 0.0, 13.0, 16.0, 16.0, 16.0],
        "south" => [0.0, 0.0, 0.0, 16.0, 16.0, 3.0],
        "west" => [13.0, 0.0, 0.0, 16.0, 16.0, 16.0],
        _ => [0.0, 0.0, 0.0, 3.0, 16.0, 16.0],
    }
}

fn rotate(dir: &str, clockwise: bool) -> &'static str {
    match (dir, clockwise) {
        ("north", true) | ("south", false) => "east",
        ("east", true) | ("west", false) => "south",
        ("south", true) | ("north", false) => "west",
        _ => "north",
    }
}

/// A centred post (half width `post`) plus arms (half width `arm`) toward the connected sides
/// (north, east, south, west), `h` high.
fn cross(out: &mut impl FnMut([f64; 6]), post: Option<f64>, arm: f64, h: f64, sides: [bool; 4]) {
    let (a0, a1) = (8.0 - arm, 8.0 + arm);
    if let Some(p) = post {
        out([8.0 - p, 0.0, 8.0 - p, 8.0 + p, h, 8.0 + p]);
    }
    let [north, east, south, west] = sides;
    if north {
        out([a0, 0.0, 0.0, a1, h, 8.0]);
    }
    if south {
        out([a0, 0.0, 8.0, a1, h, 16.0]);
    }
    if west {
        out([0.0, 0.0, a0, 8.0, h, a1]);
    }
    if east {
        out([8.0, 0.0, a0, 16.0, h, a1]);
    }
}

/// Calls `out` with each box (1/16 units, block-local) of `state`, a state of a block of class
/// `shape`.
pub fn local_boxes(shape: Shape, table: &Table, state: u32, mut out: impl FnMut([f64; 6])) {
    let prop = |name: &str| table.property(state, name).unwrap_or("");
    let sides = |on: &dyn Fn(&str) -> bool| ["north", "east", "south", "west"].map(|s| on(prop(s)));
    match shape {
        Shape::Empty => {}
        Shape::Full => out([0.0, 0.0, 0.0, 16.0, 16.0, 16.0]),
        Shape::Boxes(b) => b.iter().for_each(|&b| out(b)),
        Shape::Slab => out(match prop("type") {
            "bottom" => [0.0, 0.0, 0.0, 16.0, 8.0, 16.0],
            "top" => [0.0, 8.0, 0.0, 16.0, 16.0, 16.0],
            _ => [0.0, 0.0, 0.0, 16.0, 16.0, 16.0],
        }),
        Shape::Stairs => {
            let top = prop("half") == "top";
            let (slab, step) = if top { (8.0, 0.0) } else { (0.0, 8.0) };
            out([0.0, slab, 0.0, 16.0, slab + 8.0, 16.0]);
            let facing = ["south", "west", "north", "east"].iter().position(|&d| d == prop("facing"));
            let kind = ["straight", "inner_left", "inner_right", "outer_left", "outer_right"]
                .iter()
                .position(|&s| s == prop("shape"));
            let bits = STAIR_QUADRANTS[kind.unwrap_or(0) * 4 + facing.unwrap_or(2)];
            for (bit, x, z) in [(1, 0.0, 0.0), (2, 8.0, 0.0), (4, 0.0, 8.0), (8, 8.0, 8.0)] {
                if bits & bit != 0 {
                    out([x, step, z, x + 8.0, step + 8.0, z + 8.0]);
                }
            }
        }
        Shape::Snow => {
            let layers: f64 = prop("layers").parse().unwrap_or(1.0);
            if layers > 1.0 {
                out([0.0, 0.0, 0.0, 16.0, (layers - 1.0) * 2.0, 16.0]);
            }
        }
        Shape::Fence => cross(&mut out, Some(2.0), 2.0, 24.0, sides(&|v| v == "true")),
        Shape::Pane => cross(&mut out, Some(1.0), 1.0, 16.0, sides(&|v| v == "true")),
        Shape::Wall => {
            let post = (prop("up") != "false").then_some(4.0);
            cross(&mut out, post, 3.0, 24.0, sides(&|v| v == "low" || v == "tall"));
        }
        Shape::FenceGate => {
            if prop("open") != "true" {
                out(match prop("facing") {
                    "north" | "south" => [0.0, 0.0, 6.0, 16.0, 24.0, 10.0],
                    _ => [6.0, 0.0, 0.0, 10.0, 24.0, 16.0],
                });
            }
        }
        Shape::Door => {
            let facing = prop("facing");
            let dir = if prop("open") == "true" { rotate(facing, prop("hinge") != "right") } else { facing };
            out(panel(dir));
        }
        Shape::Trapdoor => out(if prop("open") == "true" {
            panel(prop("facing"))
        } else if prop("half") == "top" {
            [0.0, 13.0, 0.0, 16.0, 16.0, 16.0]
        } else {
            [0.0, 0.0, 0.0, 16.0, 3.0, 16.0]
        }),
        Shape::Ladder => out(panel(prop("facing"))),
        Shape::Chain | Shape::Rod => {
            let axis = match (prop("axis"), prop("facing")) {
                ("x", _) | (_, "east" | "west") => 0,
                ("z", _) | (_, "north" | "south") => 2,
                _ => 1,
            };
            let mut b = [6.5, 6.5, 6.5, 9.5, 9.5, 9.5];
            if shape == Shape::Rod {
                b = [6.0, 6.0, 6.0, 10.0, 10.0, 10.0];
            }
            b[axis] = 0.0;
            b[axis + 3] = 16.0;
            out(b);
        }
        Shape::BigDripleaf => {
            if matches!(prop("tilt"), "none" | "unstable" | "") {
                out([0.0, 11.0, 0.0, 16.0, 15.0, 16.0]);
            }
        }
        Shape::Scaffolding => out([0.0, 14.0, 0.0, 16.0, 16.0, 16.0]),
    }
}

/// [`local_boxes`] moved to the block at `pos`, in blocks.
pub fn world_boxes(shape: Shape, table: &Table, state: u32, pos: [i32; 3], out: &mut Vec<Aabb>) {
    let [x, y, z] = pos.map(f64::from);
    local_boxes(shape, table, state, |b| {
        out.push(Aabb {
            min: [x + b[0] / 16.0, y + b[1] / 16.0, z + b[2] / 16.0],
            max: [x + b[3] / 16.0, y + b[4] / 16.0, z + b[5] / 16.0],
        })
    });
}
