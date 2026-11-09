//! Waterfalls round the player, found in the world's own blocks: water coming down ten blocks or more, straight
//! through open air or stepping down a slope (a spring on a hillside cascading from ledge to ledge), steeply enough
//! not to be a stream. Vanilla's falls and a generator's alike. The shader draws mist and a spray rainbow at the foot;
//! the mod adds spray, mist and bubbles as particles. Both scale with the fall: a ten-block trickle gets a wisp, a
//! wide fall forty blocks high a cloud.
//!
//! The world round the player is read in tiles of 32 x 32 columns, one a tick, nearest first, from the ocean floor
//! heightmap (which looks through water) up: a fall's column is water from the pool's bed up to its lip.

use horizon_mod_sdk::world;
use std::collections::HashMap;

pub const TAG: u32 = 0x5_FA11;
const TILE: i32 = 32;
/// Tiles each way from the player's: 9 x 9, about 144 blocks round.
const REACH: i32 = 4;
/// Blocks read in each column: from 2 under its height (the pool's bed) to 62 over.
const BELOW: u32 = 2;
const ABOVE: u32 = 62;
/// The least drop that makes a waterfall, blocks.
pub const MIN_DROP: i32 = 10;
/// Falls handed to the shader, the nearest.
pub const MAX: usize = 8;
/// Words a fall takes in the shader's data, after a count and three spare words.
pub const FALL_WORDS: usize = 8;
pub const WORDS: usize = 4 + MAX * FALL_WORDS;

#[derive(Clone, Copy, Debug)]
pub struct Fall {
    /// The plunge point: the middle of the falling water, on the pool's surface (the top of its blocks).
    pub x: f32,
    pub z: f32,
    pub pool: f32,
    /// Blocks from the lip to the pool (at least, where the lip is higher than the tile was read).
    pub drop: f32,
    /// Falling columns.
    pub width: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum B {
    Water,
    Air,
    Solid,
}

#[derive(Default)]
pub struct Falls {
    tiles: HashMap<(i32, i32), Vec<Fall>>,
    /// The tile asked for, and the tick.
    asked: Option<((i32, i32), u64)>,
    next: usize,
    order: Vec<(i32, i32)>,
    /// The nearest falls, as the shader has them.
    pub near: Vec<Fall>,
}

impl Falls {
    /// One step a tick: takes in an answer, asks for the next tile, and keeps the nearest falls current.
    pub fn step(&mut self, answers: &[world::ColumnBlocks], player: (f64, f64, f64), tick: u64) -> bool {
        if self.order.is_empty() {
            let mut o: Vec<(i32, i32)> = (-REACH..=REACH).flat_map(|j| (-REACH..=REACH).map(move |i| (i, j))).collect();
            o.sort_by_key(|&(i, j)| i * i + j * j);
            self.order = o;
        }
        let (px, py, pz) = player;
        let here = ((px.floor() as i32).div_euclid(TILE), (pz.floor() as i32).div_euclid(TILE));
        let mut changed = false;
        for ans in answers.iter().filter(|a| a.tag == TAG) {
            let Some((tile, _)) = self.asked.take() else { continue };
            let found = read_tile(tile, ans);
            let old = self.tiles.insert(tile, found).unwrap_or_default();
            changed |= old.len() != self.tiles[&tile].len()
                || old.iter().zip(&self.tiles[&tile]).any(|(a, b)| (a.x - b.x).abs() + (a.z - b.z).abs() + (a.pool - b.pool).abs() > 0.1);
        }
        if self.asked.is_some_and(|(_, at)| tick.saturating_sub(at) > 40) {
            // never answered
            self.asked = None;
        }
        if self.asked.is_none() {
            let (di, dj) = self.order[self.next % self.order.len()];
            self.next = (self.next + 1) % self.order.len();
            let tile = (here.0 + di, here.1 + dj);
            let w = TILE + 2;
            let cols: Vec<(i32, i32)> =
                (0..w).flat_map(|j| (0..w).map(move |i| (tile.0 * TILE + i - 1, tile.1 * TILE + j - 1))).collect();
            world::read_columns(TAG, &cols, world::Heightmap::OceanFloor, BELOW, ABOVE);
            self.asked = Some((tile, tick));
        }
        let before = self.tiles.len();
        self.tiles.retain(|t, _| (t.0 - here.0).abs() <= REACH + 1 && (t.1 - here.1).abs() <= REACH + 1);
        changed |= before != self.tiles.len();
        if changed || tick % 20 == 0 {
            let mut all: Vec<Fall> = self.tiles.values().flatten().copied().collect();
            let dist = |f: &Fall| {
                let (dx, dy, dz) = (f.x as f64 - px, f.pool as f64 - py, f.z as f64 - pz);
                dx * dx + dy * dy + dz * dz
            };
            all.sort_by(|a, b| dist(a).total_cmp(&dist(b)));
            // a fall across a tile's edge is found in both halves: one mist
            let mut near: Vec<Fall> = Vec::new();
            for f in all {
                if let Some(g) = near.iter_mut().find(|g| (g.x - f.x).abs().max((g.z - f.z).abs()) < 3.5 && (g.pool - f.pool).abs() < 2.5) {
                    let n = g.width + f.width;
                    g.x = (g.x * g.width + f.x * f.width) / n;
                    g.z = (g.z * g.width + f.z * f.width) / n;
                    g.drop = g.drop.max(f.drop);
                    g.width = n;
                } else if near.len() < MAX {
                    near.push(f);
                }
            }
            let moved = near.len() != self.near.len()
                || near.iter().zip(&self.near).any(|(a, b)| (a.x - b.x).abs() + (a.z - b.z).abs() + (a.width - b.width).abs() > 0.05);
            self.near = near;
            return moved;
        }
        false
    }

    /// The falls for the shader: a count, three spare words, then for each the plunge point x, pool y, z, the drop,
    /// the width, the mist's strength and a seed.
    pub fn write(&self, out: &mut [u32]) {
        out.fill(0);
        out[0] = self.near.len() as u32;
        for (k, f) in self.near.iter().enumerate() {
            let power = power(f);
            let seed = ((f.x.floor() as i32).wrapping_mul(73_856_093) ^ (f.z.floor() as i32).wrapping_mul(19_349_663)) as u32 % 1000;
            let v = [f.x, f.pool, f.z, f.drop, f.width, power, seed as f32, 0.0];
            for (i, x) in v.iter().enumerate() {
                out[4 + k * FALL_WORDS + i] = x.to_bits();
            }
        }
    }

    /// Spray, mist and bubbles at the pools near enough to see them (particles are sent within 32 blocks).
    pub fn particles(&self, player: (f64, f64, f64), tick: u64) -> Vec<String> {
        let (px, py, pz) = player;
        let mut cmds = Vec::new();
        for (k, f) in self.near.iter().enumerate() {
            let (dx, dy, dz) = (f.x as f64 - px, f.pool as f64 - py, f.z as f64 - pz);
            if dx * dx + dy * dy + dz * dz > 32.0 * 32.0 || (tick + k as u64) % 4 != 0 {
                continue;
            }
            let (x, y, z) = (f.x, f.pool, f.z);
            let r = 0.5 + 0.35 * f.width.min(8.0);
            let big = power(f);
            let n = |a: f32| (a * (0.6 + 0.4 * f.width.min(6.0) / 3.0)).round().max(1.0) as u32;
            if big > 0.25 {
                cmds.push(format!("particle minecraft:cloud {x:.2} {:.2} {z:.2} {:.2} 0.5 {:.2} 0.02 {}", y + 0.8, r + 0.8, r + 0.8, n(1.0 + 3.0 * big)));
            }
            cmds.push(format!("particle minecraft:splash {x:.2} {:.2} {z:.2} {r:.2} 0.1 {r:.2} 0.2 {}", y + 0.15, n(8.0 + 10.0 * big)));
            cmds.push(format!("particle minecraft:bubble {x:.2} {:.2} {z:.2} {r:.2} 0.4 {r:.2} 0.05 {}", y - 0.9, n(4.0)));
            cmds.push(format!("particle minecraft:bubble_pop {x:.2} {:.2} {z:.2} {:.2} 0 {:.2} 0 {}", y + 0.05, r + 0.6, r + 0.6, n(3.0)));
        }
        cmds
    }
}

/// How much spray a fall throws up, 0..1: a trickle ten blocks high little, a wide fall forty or more high all of it.
fn power(f: &Fall) -> f32 {
    let height = ((f.drop - 6.0) / 40.0).clamp(0.0, 1.0).powf(1.2);
    let size = (f.width / 6.0).min(1.0);
    (height * (0.25 + 0.75 * size)).max(0.03)
}

/// The falls in one tile's answer. Its columns are the tile's and a ring round it, 34 x 34, rows of x.
fn read_tile(tile: (i32, i32), ans: &world::ColumnBlocks) -> Vec<Fall> {
    let w = (TILE + 2) as usize;
    let n = (BELOW + ABOVE) as usize;
    if ans.bases.len() != w * w || ans.blocks.len() != w * w * n {
        return Vec::new();
    }
    let kind: Vec<B> = ans
        .palette
        .iter()
        .map(|s| {
            if s == "minecraft:water" || s == "minecraft:bubble_column" {
                B::Water
            } else if s.ends_with("air") {
                B::Air
            } else {
                B::Solid
            }
        })
        .collect();
    let at = |k: usize, y: i32| -> B {
        let Some(base) = ans.bases[k] else { return B::Solid };
        let lo = base - BELOW as i32;
        if y < lo || y >= lo + n as i32 {
            // (unread: a deep neighbour's water may go on up past what was read of it, so never open air)
            B::Solid
        } else {
            kind.get(ans.blocks[k * n + (y - lo) as usize] as usize).copied().unwrap_or(B::Solid)
        }
    };
    let cols = cascades(tile, w, &at, ans);
    // the falling columns side by side, one fall
    let mut falls: Vec<(Fall, i32)> = Vec::new();
    for (x, z, pool, top) in cols {
        let (fx, fz) = (x as f32 + 0.5, z as f32 + 0.5);
        let surface = pool as f32 + 1.0;
        if let Some((f, n)) = falls.iter_mut().find(|(f, n)| {
            let r = 1.5 + (*n as f32).sqrt();
            (f.x - fx).abs().max((f.z - fz).abs()) <= r && (f.pool - surface).abs() <= 2.0
        }) {
            *n += 1;
            let k = *n as f32;
            f.x += (fx - f.x) / k;
            f.z += (fz - f.z) / k;
            f.pool = f.pool.max(surface);
            f.drop = f.drop.max((top - pool) as f32);
            f.width = k;
        } else {
            falls.push((Fall { x: fx, z: fz, pool: surface, drop: (top - pool) as f32, width: 1.0 }, 1));
        }
    }
    falls.into_iter().map(|(f, _)| f).collect()
}

/// The plunge columns of the water coming down in a tile: (x, z, the surface it lands on, the top it falls from).
///
/// Water open to the air (air above it or beside it: a fall's face, a cascade's steps, the surface of a pool) is
/// gathered into connected bodies. A body that comes down MIN_DROP or more, steeply (no further across than half again
/// its drop, leaving out the water lying over more water, which is pools and rivers), is a fall: its foot is where
/// water lands at its lowest level from straight above or from a step beside.
fn cascades(tile: (i32, i32), w: usize, at: &dyn Fn(usize, i32) -> B, ans: &world::ColumnBlocks) -> Vec<(i32, i32, i32, i32)> {
    let n = (BELOW + ABOVE) as i32;
    let lo: Vec<Option<i32>> = ans.bases.iter().map(|b| b.map(|b| b - BELOW as i32)).collect();
    let cell = |k: usize, y: i32| -> Option<usize> {
        let l = lo[k]?;
        (y >= l && y < l + n).then(|| k * n as usize + (y - l) as usize)
    };
    let water = |k: usize, y: i32| at(k, y) == B::Water;
    let open = |k: usize, y: i32| -> bool {
        if !water(k, y) {
            return false;
        }
        let (i, j) = (k % w, k / w);
        at(k, y + 1) == B::Air
            || (i > 0 && at(k - 1, y) == B::Air)
            || (i + 1 < w && at(k + 1, y) == B::Air)
            || (j > 0 && at(k - w, y) == B::Air)
            || (j + 1 < w && at(k + w, y) == B::Air)
    };
    // Water covered over on a step (walled in, more water falling onto it) still carries the flow on: it joins a
    // body but is not counted in it.
    let link = |k: usize, y: i32| open(k, y) || (water(k, y) && !water(k, y - 1) && open(k, y + 1));
    let mut seen = vec![false; w * w * n as usize];
    let mut out = Vec::new();
    for k0 in 0..w * w {
        let Some(l0) = lo[k0] else { continue };
        for y0 in l0..l0 + n {
            let Some(c0) = cell(k0, y0) else { continue };
            if seen[c0] || !open(k0, y0) {
                continue;
            }
            // one body, flooded through open water
            seen[c0] = true;
            let mut body = vec![(k0, y0)];
            let mut q = 0;
            while q < body.len() {
                let (k, y) = body[q];
                q += 1;
                let (i, j) = (k % w, k / w);
                let mut near = vec![(k, y + 1), (k, y - 1)];
                if i > 0 {
                    near.push((k - 1, y));
                }
                if i + 1 < w {
                    near.push((k + 1, y));
                }
                if j > 0 {
                    near.push((k - w, y));
                }
                if j + 1 < w {
                    near.push((k + w, y));
                }
                for (nk, ny) in near {
                    if let Some(c) = cell(nk, ny)
                        && !seen[c]
                        && link(nk, ny)
                    {
                        seen[c] = true;
                        body.push((nk, ny));
                    }
                }
            }
            body.retain(|&(k, y)| open(k, y));
            let top = body.iter().map(|b| b.1).max().unwrap_or(0);
            let foot = body.iter().map(|b| b.1).min().unwrap_or(0);
            if top - foot < MIN_DROP {
                continue;
            }
            // steep: the moving water (not lying over more water) spans little ground for its drop
            let moving: Vec<&(usize, i32)> = body.iter().filter(|&&(k, y)| !water(k, y - 1)).collect();
            let (mut i0, mut i1, mut j0, mut j1) = (w, 0, w, 0);
            for &&(k, _) in &moving {
                let (i, j) = (k % w, k / w);
                (i0, i1, j0, j1) = (i0.min(i), i1.max(i), j0.min(j), j1.max(j));
            }
            let across = (i1.saturating_sub(i0)).max(j1.saturating_sub(j0)) as i32;
            if across * 2 > (top - foot) * 3 {
                continue;
            }
            // the foot: water at the lowest level (or one above it) fed from straight above or from a step beside
            for &(k, y) in &body {
                if y > foot + 1 {
                    continue;
                }
                let (i, j) = (k % w, k / w);
                if i == 0 || j == 0 || i + 1 == w || j + 1 == w {
                    continue;
                }
                let fed = water(k, y + 1) || [k - 1, k + 1, k - w, k + w].iter().any(|&s| water(s, y + 1) && open(s, y + 1));
                let (x, z) = (tile.0 * TILE + i as i32 - 1, tile.1 * TILE + j as i32 - 1);
                if fed && !out.iter().any(|c: &(i32, i32, i32, i32)| c.0 == x && c.1 == z) {
                    out.push((x, z, foot, top));
                }
            }
        }
    }
    out
}
