//! Player movement: a simplified vanilla body (0.6 x 1.8 box, gravity, jumping, 0.6-block step-up,
//! swimming) moved with swept collisions against the world's block boxes, and an A* path finder
//! over the loaded chunks. The server re-checks every move and corrects us with a teleport when it
//! disagrees, so close is good enough.
//!
//! With no chunks loaded (a world the client can't read) movement is "blind": straight lines at a
//! constant height, collisions left to the server.

use crate::world::{Aabb, World};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

pub const WIDTH: f64 = 0.6;
pub const HEIGHT: f64 = 1.8;
pub const EYE_HEIGHT: f64 = 1.62;
pub const STEP_HEIGHT: f64 = 0.6;
/// Blocks per tick: walking 4.317 m/s, sprinting 5.612 m/s, sneaking 1.295 m/s, flying 10.92 m/s.
pub const WALK_SPEED: f64 = 0.215_85;
pub const SPRINT_SPEED: f64 = 0.280_6;
pub const SNEAK_SPEED: f64 = 0.064_75;
pub const FLY_SPEED: f64 = 0.546;
const GRAVITY: f64 = 0.08;
const DRAG: f64 = 0.98;
const JUMP_VELOCITY: f64 = 0.42;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Body {
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub on_ground: bool,
    pub horizontal_collision: bool,
}

/// What the player "presses" this tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct Input {
    /// Unit horizontal direction (x, z) to move in, if any.
    pub dir: Option<[f64; 2]>,
    pub speed: f64,
    pub jump: bool,
    /// Creative/spectator flight: no gravity; `fly_vertical` blocks per tick up (+) or down (-).
    pub flying: bool,
    pub fly_vertical: f64,
}

fn player_box(pos: [f64; 3]) -> Aabb {
    let h = WIDTH / 2.0;
    Aabb { min: [pos[0] - h, pos[1], pos[2] - h], max: [pos[0] + h, pos[1] + HEIGHT, pos[2] + h] }
}

fn offset(b: Aabb, d: [f64; 3]) -> Aabb {
    Aabb {
        min: [b.min[0] + d[0], b.min[1] + d[1], b.min[2] + d[2]],
        max: [b.max[0] + d[0], b.max[1] + d[1], b.max[2] + d[2]],
    }
}

/// Block boxes around `region`. Columns whose chunk isn't loaded count as solid so nobody walks off
/// the known world.
fn collect_boxes(world: &World, region: Aabb) -> Vec<Aabb> {
    let mut out = Vec::new();
    let (x0, x1) = (region.min[0].floor() as i32, region.max[0].floor() as i32);
    let (z0, z1) = (region.min[2].floor() as i32, region.max[2].floor() as i32);
    // One block lower than the region: fences and walls reach 1.5 blocks up.
    let (y0, y1) = (region.min[1].floor() as i32 - 1, region.max[1].floor() as i32);
    for x in x0..=x1 {
        for z in z0..=z1 {
            if !world.is_loaded(x, z) {
                out.push(Aabb {
                    min: [x as f64, y0 as f64, z as f64],
                    max: [x as f64 + 1.0, y1 as f64 + 1.0, z as f64 + 1.0],
                });
                continue;
            }
            for y in y0..=y1 {
                world.collision_boxes(x, y, z, &mut out);
            }
        }
    }
    out
}

/// How far `b` can move along `axis` by `d` before touching one of `boxes`.
fn clip(boxes: &[Aabb], b: Aabb, axis: usize, mut d: f64) -> f64 {
    const EPS: f64 = 1e-7;
    for o in boxes {
        let overlaps = (0..3)
            .filter(|&a| a != axis)
            .all(|a| o.max[a] > b.min[a] + EPS && o.min[a] < b.max[a] - EPS);
        if !overlaps {
            continue;
        }
        if d > 0.0 && o.min[axis] >= b.max[axis] - EPS {
            d = d.min(o.min[axis] - b.max[axis]);
        } else if d < 0.0 && o.max[axis] <= b.min[axis] + EPS {
            d = d.max(o.max[axis] - b.min[axis]);
        }
    }
    d
}

/// Move the box by `delta` against `boxes`: y first, then the larger horizontal axis.
fn sweep(boxes: &[Aabb], start: Aabb, delta: [f64; 3]) -> [f64; 3] {
    let mut b = start;
    let mut out = [0.0; 3];
    out[1] = clip(boxes, b, 1, delta[1]);
    b = offset(b, [0.0, out[1], 0.0]);
    let order = if delta[0].abs() >= delta[2].abs() { [0, 2] } else { [2, 0] };
    for axis in order {
        out[axis] = clip(boxes, b, axis, delta[axis]);
        let mut d = [0.0; 3];
        d[axis] = out[axis];
        b = offset(b, d);
    }
    out
}

pub fn feet_in_fluid(world: &World, pos: [f64; 3]) -> bool {
    world
        .state_at(pos[0].floor() as i32, pos[1].floor() as i32, pos[2].floor() as i32)
        .is_some_and(|s| world.is_fluid(s))
}

/// Advance the body by one tick.
pub fn step(world: &World, body: &mut Body, input: &Input) {
    let (dx, dz) = input.dir.map_or((0.0, 0.0), |[x, z]| (x * input.speed, z * input.speed));
    let blind = world.loaded_chunks() == 0;
    if blind {
        body.pos[0] += dx;
        body.pos[2] += dz;
        if input.flying {
            body.pos[1] += input.fly_vertical;
        }
        body.vel = [dx, 0.0, dz];
        body.on_ground = !input.flying;
        body.horizontal_collision = false;
        return;
    }
    let here_loaded = world.is_loaded(body.pos[0].floor() as i32, body.pos[2].floor() as i32);
    let in_fluid = feet_in_fluid(world, body.pos);
    let mut vy = body.vel[1];
    if input.flying {
        vy = input.fly_vertical;
    } else if !here_loaded {
        vy = 0.0; // never fall into a chunk we can't see
    } else if in_fluid {
        if input.jump {
            vy = vy.max(0.0) + 0.04;
        }
    } else if body.on_ground && input.jump {
        vy = JUMP_VELOCITY;
    }
    let delta = [dx, vy, dz];
    let start = player_box(body.pos);
    let region = Aabb {
        min: [start.min[0] + dx.min(0.0), start.min[1] + vy.min(0.0) - 0.01, start.min[2] + dz.min(0.0)],
        max: [start.max[0] + dx.max(0.0), start.max[1] + vy.max(0.0) + STEP_HEIGHT, start.max[2] + dz.max(0.0)],
    };
    let boxes = collect_boxes(world, region);
    let mut moved = sweep(&boxes, start, delta);
    let hit_horizontal = moved[0] != dx || moved[2] != dz;
    let landed = vy < 0.0 && moved[1] > vy + 1e-9;
    // Step up a slab/stair edge when walking into it on the ground.
    if hit_horizontal && (body.on_ground || landed) && !input.flying {
        let up = clip(&boxes, start, 1, STEP_HEIGHT);
        let raised = offset(start, [0.0, up, 0.0]);
        let mut stepped = sweep(&boxes, raised, [dx, 0.0, dz]);
        let lifted = offset(raised, [stepped[0], 0.0, stepped[2]]);
        stepped[1] = up + clip(&boxes, lifted, 1, -up);
        if stepped[0].hypot(stepped[2]) > moved[0].hypot(moved[2]) + 1e-6 {
            moved = stepped;
        }
    }
    body.pos = [body.pos[0] + moved[0], body.pos[1] + moved[1], body.pos[2] + moved[2]];
    body.horizontal_collision = moved[0] != dx || moved[2] != dz;
    // Like vanilla, gravity pulls every tick; standing still is a downward move the floor blocks.
    // (A step-up also ends above where gravity would have taken us: on the ground.)
    let blocked_y = (moved[1] - vy).abs() > 1e-9;
    body.on_ground = vy < 0.0 && moved[1] > vy + 1e-9;
    let vy_after = if blocked_y { 0.0 } else { vy };
    body.vel[0] = moved[0];
    body.vel[2] = moved[2];
    body.vel[1] = if input.flying || !here_loaded {
        0.0
    } else if in_fluid {
        (vy_after - 0.02) * 0.8
    } else {
        (vy_after - GRAVITY) * DRAG
    };
}

// ---------------------------------------------------------------------------------------------
// Path finding

/// Blocks a path must not go through or stand in.
fn dangerous(name: &str) -> bool {
    matches!(name, "lava" | "fire" | "soul_fire" | "cactus" | "sweet_berry_bush" | "powder_snow" | "cobweb" | "magma_block" | "wither_rose" | "campfire" | "soul_campfire")
}

struct Grid<'a> {
    world: &'a World,
    cache: HashMap<[i32; 3], (f64, bool, bool)>,
}

impl Grid<'_> {
    /// (top of the block's collision relative to its base: 0 = none, 1 = full; fluid; dangerous)
    fn info(&mut self, p: [i32; 3]) -> (f64, bool, bool) {
        if let Some(v) = self.cache.get(&p) {
            return *v;
        }
        let v = match self.world.state_at(p[0], p[1], p[2]) {
            None => (1.0, false, true), // unloaded or outside the world: never path through
            Some(state) => {
                let mut boxes = Vec::new();
                self.world.collision_boxes(p[0], p[1], p[2], &mut boxes);
                let top = boxes.iter().fold(0.0_f64, |t, b| t.max(b.max[1] - p[1] as f64));
                let name = self.world.block_name(state).unwrap_or("");
                (top, self.world.is_fluid(state), dangerous(name))
            }
        };
        self.cache.insert(p, v);
        v
    }
    /// Feet and head fit (a block no higher than a slab counts as walkable ground).
    fn passable(&mut self, p: [i32; 3]) -> bool {
        let (top, _, bad) = self.info(p);
        let (head_top, _, head_bad) = self.info([p[0], p[1] + 1, p[2]]);
        top <= 0.5 && head_top == 0.0 && !bad && !head_bad
    }
    /// Can stand with feet in block `p`.
    fn standable(&mut self, p: [i32; 3]) -> bool {
        if !self.passable(p) {
            return false;
        }
        let (top_here, fluid_here, _) = self.info(p);
        let (top_below, _, bad_below) = self.info([p[0], p[1] - 1, p[2]]);
        fluid_here || top_here > 0.0 || (top_below > 0.5 && !bad_below)
    }
}

#[derive(PartialEq)]
struct Open(f64, [i32; 3]);
impl Eq for Open {}
impl Ord for Open {
    fn cmp(&self, other: &Self) -> Ordering {
        other.0.partial_cmp(&self.0).unwrap_or(Ordering::Equal)
    }
}
impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A walkable route (block-center waypoints, ending at `goal` itself) from `start` to `goal`, or
/// None. Without `goal_y` any height at the goal column will do. If the goal can't be reached
/// within `max_nodes`, the route to the closest point found (if it gets meaningfully closer).
pub fn find_path(world: &World, start: [f64; 3], goal: [f64; 3], goal_y: bool, max_nodes: usize) -> Option<Vec<[f64; 3]>> {
    if world.loaded_chunks() == 0 {
        return None;
    }
    let mut g = Grid { world, cache: HashMap::new() };
    let s = [start[0].floor() as i32, (start[1] + 0.2).floor() as i32, start[2].floor() as i32];
    let t = [goal[0].floor() as i32, goal[1].floor() as i32, goal[2].floor() as i32];
    let h = |p: [i32; 3]| {
        let dx = (p[0] - t[0]) as f64;
        let dz = (p[2] - t[2]) as f64;
        let dy = if goal_y { (p[1] - t[1]) as f64 } else { 0.0 };
        (dx * dx + dz * dz + dy * dy).sqrt()
    };
    let reached = |p: [i32; 3]| p[0] == t[0] && p[2] == t[2] && (!goal_y || (p[1] - t[1]).abs() <= 1);
    let mut open = BinaryHeap::new();
    let mut cost: HashMap<[i32; 3], f64> = HashMap::new();
    let mut came: HashMap<[i32; 3], [i32; 3]> = HashMap::new();
    cost.insert(s, 0.0);
    open.push(Open(h(s), s));
    let (mut best, mut best_h) = (s, h(s));
    let mut expanded = 0;
    let mut end = None;
    while let Some(Open(_, p)) = open.pop() {
        if reached(p) {
            end = Some(p);
            break;
        }
        expanded += 1;
        if expanded > max_nodes {
            break;
        }
        let here = cost[&p];
        if h(p) < best_h {
            (best, best_h) = (p, h(p));
        }
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
            let diagonal = dx != 0 && dz != 0;
            if diagonal && !(g.passable([p[0] + dx, p[1], p[2]]) && g.passable([p[0], p[1], p[2] + dz])) {
                continue;
            }
            let base = if diagonal { std::f64::consts::SQRT_2 } else { 1.0 };
            let mut candidates = Vec::with_capacity(5);
            // Level, one up (a jump: needs headroom above us), or a drop of up to three.
            candidates.push(([p[0] + dx, p[1], p[2] + dz], base));
            if !diagonal && g.info([p[0], p[1] + 2, p[2]]).0 == 0.0 {
                candidates.push(([p[0] + dx, p[1] + 1, p[2] + dz], base + 0.5));
            }
            for drop in 1..=3 {
                let q = [p[0] + dx, p[1] - drop, p[2] + dz];
                let column_clear = (0..drop).all(|k| g.passable([p[0] + dx, p[1] - k, p[2] + dz]));
                if !column_clear {
                    break;
                }
                candidates.push((q, base + 0.3 * drop as f64));
            }
            for (q, step) in candidates {
                if !g.standable(q) {
                    continue;
                }
                let next = here + step;
                if cost.get(&q).is_none_or(|&c| next < c - 1e-9) {
                    cost.insert(q, next);
                    came.insert(q, p);
                    open.push(Open(next + h(q), q));
                }
            }
        }
    }
    let last = match end {
        Some(e) => e,
        None if h(s) - best_h >= 2.0 => best,
        None => return None,
    };
    let mut blocks = vec![last];
    let mut p = last;
    while let Some(&q) = came.get(&p) {
        blocks.push(q);
        p = q;
    }
    blocks.reverse();
    let mut points: Vec<[f64; 3]> = blocks
        .iter()
        .skip(1)
        .map(|b| [b[0] as f64 + 0.5, b[1] as f64, b[2] as f64 + 0.5])
        .collect();
    if end.is_some() {
        if let Some(lastp) = points.last_mut() {
            lastp[0] = goal[0];
            lastp[2] = goal[2];
        } else {
            points.push([goal[0], start[1], goal[2]]);
        }
    }
    Some(points)
}

/// Yaw and pitch (degrees, Minecraft convention) to look from `eye` at `target`.
pub fn look_angles(eye: [f64; 3], target: [f64; 3]) -> (f32, f32) {
    let (dx, dy, dz) = (target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]);
    let yaw = (-dx).atan2(dz).to_degrees();
    let pitch = -(dy.atan2(dx.hypot(dz))).to_degrees();
    (yaw as f32, pitch as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn look_angles_follow_minecraft_conventions() {
        // +z is yaw 0, -x is yaw 90; looking down is positive pitch.
        let (yaw, pitch) = look_angles([0.0, 0.0, 0.0], [0.0, 0.0, 5.0]);
        assert!((yaw - 0.0).abs() < 1e-4 && pitch.abs() < 1e-4);
        let (yaw, _) = look_angles([0.0, 0.0, 0.0], [-5.0, 0.0, 0.0]);
        assert!((yaw - 90.0).abs() < 1e-4);
        let (_, pitch) = look_angles([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
        assert!((pitch - 45.0).abs() < 1e-4);
    }

    #[test]
    fn clipping_stops_at_a_wall() {
        let wall = [Aabb { min: [1.0, 0.0, -5.0], max: [2.0, 3.0, 5.0] }];
        let b = player_box([0.0, 0.0, 0.0]);
        assert!((clip(&wall, b, 0, 2.0) - 0.7).abs() < 1e-9);
        assert_eq!(clip(&wall, b, 0, -2.0), -2.0);
        assert_eq!(clip(&wall, b, 2, 2.0), 2.0);
    }

    #[test]
    fn blind_world_moves_straight() {
        let world = World::new(777);
        let mut body = Body { pos: [0.5, 64.0, 0.5], ..Body::default() };
        step(&world, &mut body, &Input { dir: Some([1.0, 0.0]), speed: WALK_SPEED, ..Input::default() });
        assert!((body.pos[0] - (0.5 + WALK_SPEED)).abs() < 1e-9);
        assert_eq!(body.pos[1], 64.0);
    }
}
