//! The weather: systems (fair-weather cumulus fields, showers, thunderstorms, rain fronts and stratocumulus decks)
//! that are born, drift with the wind and die across the world round the player, and the grid of cells the shader
//! draws them from.
//!
//! Everything is at the real world's scale, one block a metre: cumulus bases near a kilometre up, thunderstorms
//! towering ten kilometres with anvils spreading for tens more, fronts a hundred kilometres long. Nothing is global
//! the way vanilla weather is: every system has a place, so a storm can rumble twenty kilometres away while the sun
//! shines here, and walk over you later in the day.
//!
//! Each in-game day has a character (clear, fair, showery, thundery, overcast or rainy) that follows from the day
//! before, so a rainy day gives way to clearing showers, a run of fair days builds to thunder, and so on. The day's
//! character sets which systems are born, the wind, and how high the clouds sit.
//!
//! The ground shapes the sky too: the world's biomes (their temperature and how wet they are) say how moist and warm
//! the air is over them, so jungles and the sea breed showers and storms, deserts keep high, fair skies, cold country
//! takes grey decks, and the freezing level (low over cold land, high over warm) decides where what falls is snow.

use crate::noise::{Rng, hash, smooth, vnoise1, vnoise2};

/// A grid cell's width in metres, and the grid's width in cells: 128 x 512 m, 65 km across.
pub const CELL: f32 = 512.0;
pub const G: usize = 128;
/// Four words a cell.
pub const WORDS: usize = G * G * 4;
/// After the grid: a summary of the sky beyond it (TAIL words), then the clouds' noise volume.
pub const TAIL: usize = 16;
pub const NOISE_AT: usize = WORDS + TAIL;
/// Then the waterfalls round the player (falls.rs).
pub const FALLS_AT: usize = NOISE_AT + crate::noise::VOL * crate::noise::VOL * crate::noise::VOL;
/// Then what keeps the rain off round the player (lib.rs's `look_for_cover`): the map's corner x and z, whether it
/// is filled, a spare word, and COVER_N x COVER_N heights (i32): rain falls down to each column's height on the
/// motion-blocking heightmap (the first free block over a roof, a tree, the ground or water), i32::MIN where unknown.
pub const COVER_AT: usize = FALLS_AT + crate::falls::WORDS;
pub const COVER_N: usize = 64;
pub const DATA: usize = COVER_AT + 4 + COVER_N * COVER_N;

/// Systems further than this from the player are dropped; new ones are born between the two spawn radii.
const KEEP: f64 = 50_000.0;
const SPAWN_MIN: f64 = 9_000.0;
const SPAWN_MAX: f64 = 34_000.0;
/// Storms are born nearer than other systems, within sight: a tower building on the horizon in the morning comes over
/// by evening.
const STORM_SPAWN_MAX: f64 = 22_000.0;

/// Weather seconds a game second at `/skies speed 1`. A Minecraft day is twenty minutes, so weather at the clock's
/// own pace took days of them to change; three times as fast, a storm seen building in the morning arrives by evening
/// and a day sees the changes of a real one. Lightning and thunder keep real time.
pub const PACE: f32 = 3.0;

/// The climate grid: cells this wide (metres), this many a side (98 km across, past where systems are kept).
pub const CLIMATE_CELL: f64 = 2048.0;
pub const CLIMATE_N: usize = 48;
/// How fast the air cools with height, in vanilla's biome temperature a block: about a degree Celsius every 100
/// blocks (the real world's is 0.65 a hundred metres, but the world's mountains are lower than the real world's).
const LAPSE: f32 = 0.0043;
/// Vanilla's temperature where rain turns to snow.
const SNOW_TEMPERATURE: f32 = 0.15;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A field of fair-weather cumulus.
    Fair,
    /// A cumulus congestus that rains for a while.
    Shower,
    /// A cumulonimbus: a tower to ten kilometres with an anvil, a rain core, lightning.
    Storm,
    /// A long band of nimbostratus and steady rain, lying across the wind, with cirrus running ahead.
    Front,
    /// A grey deck of stratocumulus: an overcast sky.
    Deck,
}

pub const KINDS: [Kind; 5] = [Kind::Fair, Kind::Shower, Kind::Storm, Kind::Front, Kind::Deck];

impl Kind {
    pub fn from_u8(v: u8) -> Kind {
        match v {
            1 => Kind::Shower,
            2 => Kind::Storm,
            3 => Kind::Front,
            4 => Kind::Deck,
            _ => Kind::Fair,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::Fair => "fair-weather cumulus",
            Kind::Shower => "shower",
            Kind::Storm => "thunderstorm",
            Kind::Front => "rain front",
            Kind::Deck => "overcast deck",
        }
    }
}

/// A day's character.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Regime {
    Clear,
    Fair,
    Showery,
    Thundery,
    Overcast,
    Rainy,
}

pub const REGIMES: [Regime; 6] = [Regime::Clear, Regime::Fair, Regime::Showery, Regime::Thundery, Regime::Overcast, Regime::Rainy];

impl Regime {
    pub fn name(self) -> &'static str {
        match self {
            Regime::Clear => "clear",
            Regime::Fair => "fair",
            Regime::Showery => "showery",
            Regime::Thundery => "thundery",
            Regime::Overcast => "overcast",
            Regime::Rainy => "rainy",
        }
    }
    pub fn parse(s: &str) -> Option<Regime> {
        REGIMES.into_iter().find(|r| r.name() == s)
    }
    pub fn index(self) -> u8 {
        REGIMES.iter().position(|&r| r == self).unwrap() as u8
    }
    /// What tomorrow is likely to be, as weights over REGIMES: weather keeps to its course for a while, fronts bring
    /// rain, rain clears to showers, settled spells build to thunder.
    fn next(self) -> [u32; 6] {
        match self {
            Regime::Clear => [40, 35, 5, 5, 10, 5],
            Regime::Fair => [20, 35, 15, 15, 10, 5],
            Regime::Showery => [15, 30, 25, 10, 10, 10],
            Regime::Thundery => [10, 25, 25, 20, 5, 15],
            Regime::Overcast => [5, 15, 10, 5, 35, 30],
            Regime::Rainy => [5, 10, 40, 5, 15, 25],
        }
    }
    /// How many systems of each kind (KINDS order) the sky round the player holds on such a day.
    fn targets(self) -> [f32; 5] {
        match self {
            Regime::Clear => [3.0, 0.0, 0.0, 0.0, 0.0],
            Regime::Fair => [14.0, 1.0, 0.0, 0.0, 0.0],
            Regime::Showery => [10.0, 9.0, 1.0, 0.0, 0.0],
            Regime::Thundery => [8.0, 5.0, 5.0, 0.0, 0.0],
            Regime::Overcast => [3.0, 0.0, 0.0, 0.0, 3.0],
            Regime::Rainy => [2.0, 3.0, 0.0, 2.0, 2.0],
        }
    }
    /// The thin scattered cloud everywhere: coverage, and how high cumulus bases sit (dry air lifts them).
    fn background(self) -> (f32, f32) {
        match self {
            Regime::Clear => (0.02, 1500.0),
            Regime::Fair => (0.14, 1150.0),
            Regime::Showery => (0.18, 900.0),
            Regime::Thundery => (0.12, 1000.0),
            Regime::Overcast => (0.3, 700.0),
            Regime::Rainy => (0.3, 600.0),
        }
    }
    /// Where the wind comes from (degrees: 0 east, 90 south, 180 west, 270 north) and how hard it blows (m/s) on
    /// such a day. Fronts come in on south-westerlies; behind them the wind veers north-westerly; settled days are
    /// light westerlies.
    fn wind(self) -> (f32, f32) {
        match self {
            Regime::Clear => (180.0, 4.0),
            Regime::Fair => (190.0, 5.0),
            Regime::Showery => (225.0, 8.0),
            Regime::Thundery => (200.0, 6.0),
            Regime::Overcast => (160.0, 5.0),
            Regime::Rainy => (135.0, 10.0),
        }
    }
    /// How the day's air mass moves the freezing level (blocks): warm and humid before thunder, cold behind fronts.
    fn freeze_shift(self) -> f32 {
        match self {
            Regime::Clear => 0.0,
            Regime::Fair => 10.0,
            Regime::Showery => -25.0,
            Regime::Thundery => 35.0,
            Regime::Overcast => -10.0,
            Regime::Rainy => -20.0,
        }
    }
    /// The chance of valley fog the next morning.
    pub fn fog_chance(self) -> f32 {
        match self {
            Regime::Clear => 0.6,
            Regime::Fair => 0.45,
            Regime::Showery => 0.35,
            Regime::Thundery => 0.35,
            Regime::Overcast => 0.15,
            Regime::Rainy => 0.5,
        }
    }
}

#[derive(Clone)]
pub struct System {
    pub kind: Kind,
    pub x: f64,
    pub z: f64,
    /// Radius across the long axis is `r * stretch`; `angle` turns the long axis (radians, from +x).
    pub r: f32,
    pub stretch: f32,
    pub angle: f32,
    pub age: f32,
    pub life: f32,
    /// How strong it gets at its peak, 0.7..1.15.
    pub power: f32,
    /// Cloud base, metres (blocks) up.
    pub base: f32,
    pub seed: u32,
    /// Its own drift on top of the wind, metres a second.
    pub vx: f32,
    pub vz: f32,
    /// Seconds to its next lightning strike (storms).
    pub strike_in: f32,
    /// The player has been told about it.
    pub told: bool,
}

/// The forms a storm takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StormForm {
    /// One great tower.
    Cell,
    /// Several towers in a knot, at different stages.
    Cluster,
    /// A squall line across the wind.
    Line,
    /// A small, tall lone storm.
    Pulse,
}

impl StormForm {
    pub fn name(self) -> &'static str {
        match self {
            StormForm::Cell => "thunderstorm",
            StormForm::Cluster => "cluster of thunderstorms",
            StormForm::Line => "squall line",
            StormForm::Pulse => "lone thunderstorm",
        }
    }
}

/// What a system (or the sky as a whole) does above one point.
#[derive(Clone, Copy, Default, Debug)]
pub struct Sample {
    pub cov: f32,
    pub base: f32,
    pub top: f32,
    pub precip: f32,
    pub storm: f32,
    /// Anvil or cirrus coverage, and its height.
    pub high: f32,
    pub high_y: f32,
    /// 0 heaped cumulus .. 1 layered stratus.
    pub layered: f32,
}

/// The climate under the sky, from the world's biomes (`world.read-climate`): a grid of CLIMATE_N cells a side round
/// `origin` (its north-west corner); empty until the world has answered.
#[derive(Clone, Default)]
pub struct Climate {
    pub origin: (f64, f64),
    /// Per cell: the air's temperature (vanilla's biome temperature, of the lowland round it), how wet the land is
    /// (0..1), and how much of it is sea (0..1).
    pub cells: Vec<[f32; 3]>,
}

/// Biomes that are cold for their height, not their air: peaks and high slopes. The air over them is the lowland's.
fn alpine(name: &str) -> bool {
    ["peaks", "slopes", "grove", "meadow", "windswept"].iter().any(|k| name.contains(k))
}

impl Climate {
    /// The climate from the world's answer: `names`, `temperatures` and `downfalls` per biome met, and each column's
    /// biome (cells in rows, north to south).
    pub fn from_answer(origin: (f64, f64), names: &[String], temperatures: &[f32], downfalls: &[f32], columns: &[Option<u16>]) -> Climate {
        let n = CLIMATE_N;
        let mut known = 0;
        // (temperature, or none for alpine land or the unknown; wetness; sea)
        let raw: Vec<(Option<f32>, f32, f32)> = (0..n * n)
            .map(|i| {
                let Some(b) = columns.get(i).copied().flatten().map(usize::from) else {
                    return (None, 0.5, 0.0);
                };
                known += 1;
                let name = names.get(b).map_or("", |s| s.as_str());
                let t = temperatures.get(b).copied().unwrap_or(0.7);
                let sea = name.contains("ocean") as u8 as f32;
                // the sea's own air is moist, whatever its biome's downfall
                let wet = if sea > 0.0 { 0.75 } else { downfalls.get(b).copied().unwrap_or(0.5).clamp(0.0, 1.0) };
                (if alpine(name) { None } else { Some(t.clamp(-1.0, 2.0)) }, wet, sea)
            })
            .collect();
        if known == 0 {
            return Climate::default();
        }
        // smoothed over the cells round each (about 10 km), the alpine ones taking the lowland's air
        let mut cells = vec![[0.0; 3]; n * n];
        for j in 0..n {
            for i in 0..n {
                let (mut ts, mut tw, mut ws, mut ss, mut w) = (0.0, 0.0, 0.0, 0.0, 0.0);
                for dj in -2i32..=2 {
                    for di in -2i32..=2 {
                        let (a, b) = (i as i32 + di, j as i32 + dj);
                        if a < 0 || b < 0 || a >= n as i32 || b >= n as i32 {
                            continue;
                        }
                        let (t, wet, sea) = raw[b as usize * n + a as usize];
                        let k = 1.0 / (1.0 + (di * di + dj * dj) as f32);
                        if let Some(t) = t {
                            ts += t * k;
                            tw += k;
                        }
                        ws += wet * k;
                        ss += sea * k;
                        w += k;
                    }
                }
                cells[j * n + i] = [if tw > 0.0 { ts / tw } else { 0.6 }, ws / w, ss / w];
            }
        }
        Climate { origin, cells }
    }

    /// The climate at `x, z` (blended between cells), if the world has said.
    pub fn at(&self, x: f64, z: f64) -> Option<[f32; 3]> {
        if self.cells.is_empty() {
            return None;
        }
        let n = CLIMATE_N as f64;
        let gx = ((x - self.origin.0) / CLIMATE_CELL - 0.5).clamp(0.0, n - 1.001);
        let gz = ((z - self.origin.1) / CLIMATE_CELL - 0.5).clamp(0.0, n - 1.001);
        let (i, j) = (gx.floor() as usize, gz.floor() as usize);
        let (fx, fz) = ((gx - i as f64) as f32, (gz - j as f64) as f32);
        let c = |i: usize, j: usize| self.cells[j * CLIMATE_N + i];
        let (a, b, c0, d) = (c(i, j), c(i + 1, j), c(i, j + 1), c(i + 1, j + 1));
        Some(std::array::from_fn(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c0[k] + (d[k] - c0[k]) * fx;
            top + (bot - top) * fz
        }))
    }

    /// The middle of the grid: where it was asked for.
    pub fn centre(&self) -> Option<(f64, f64)> {
        let half = CLIMATE_CELL * CLIMATE_N as f64 / 2.0;
        (!self.cells.is_empty()).then_some((self.origin.0 + half, self.origin.1 + half))
    }
}

impl System {
    /// 0 when born and when gone, 1 through its prime.
    pub fn envelope(&self) -> f32 {
        let a = self.age / self.life;
        smooth(0.0, 0.2, a) * (1.0 - smooth(0.72, 1.0, a))
    }

    /// What to call it: a storm by its form.
    pub fn label(&self) -> &'static str {
        if self.kind == Kind::Storm { self.storm_form().name() } else { self.kind.name() }
    }

    /// Eight bits of the seed from `at` (a system's own choices: its form, how hard it rains).
    fn bits(&self, at: u32) -> u32 {
        hash(self.seed ^ at.wrapping_mul(0x9E37_79B9)) & 0xff
    }

    /// The form a storm takes.
    pub fn storm_form(&self) -> StormForm {
        match self.bits(16) % 6 {
            0 | 1 => StormForm::Cell,
            2 | 3 => StormForm::Cluster,
            4 => StormForm::Line,
            _ => StormForm::Pulse,
        }
    }

    /// One cell of a cluster (the `k`th): its centre (from the system's), radius and strength. The first sits in the
    /// middle; the others round it, `spread` of the radius out, `size` of the radius across, `stage` the most they
    /// fall short of the system's strength.
    fn cell(&self, k: usize, spread: f32, size: f32, stage: f32, e: f32) -> (f32, f32, f32, f32) {
        let b = |n: u32| self.bits(100 + k as u32 * 8 + n) as f32 / 255.0;
        let a = b(0) * std::f32::consts::TAU;
        let d = if k == 0 { 0.0 } else { self.r * spread * (0.6 + 0.4 * b(1)) };
        let rk = self.r * size * (0.8 + 0.4 * b(2));
        (d * a.cos(), d * a.sin(), rk, e * (1.0 - stage * b(3)))
    }

    fn reach(&self) -> f32 {
        match self.kind {
            // the anvil runs far downwind
            Kind::Storm => self.r * 5.0,
            // and cirrus far ahead of a front
            Kind::Front => self.r * self.stretch * 1.3 + self.r * 3.0,
            _ => self.r * self.stretch,
        }
    }

    /// The system above `x, z`, given the wind's direction (unit vector).
    pub fn sample(&self, x: f64, z: f64, wind: (f32, f32)) -> Sample {
        let mut s = Sample::default();
        let e = self.envelope() * self.power;
        if e <= 0.001 {
            return s;
        }
        let (dx, dz) = ((x - self.x) as f32, (z - self.z) as f32);
        let (sa, ca) = self.angle.sin_cos();
        let (lx, lz) = (dx * ca + dz * sa, -dx * sa + dz * ca);
        let mut u = ((lx / (self.r * self.stretch)).powi(2) + (lz / self.r).powi(2)).sqrt();
        // ragged edges, not ellipses
        let sd = (self.seed & 0xffff) as f64;
        let rag = 0.8 + 0.4 * vnoise2(dx as f64 / (self.r as f64 * 0.6) + sd, dz as f64 / (self.r as f64 * 0.6), self.seed);
        u *= rag;
        s.base = self.base;
        match self.kind {
            Kind::Fair => {
                let f = 1.0 - smooth(0.3, 1.0, u);
                s.cov = 0.62 * e * f;
                s.top = s.base + 400.0 + 1300.0 * e * f;
            }
            Kind::Shower => {
                // how hard it rains: a passing sprinkle to a downpour
                let wet = 0.55 + 0.45 * self.bits(8) as f32 / 255.0;
                let shower = |s: &mut Sample, u: f32, e: f32| {
                    let f = 1.0 - smooth(0.25, 1.0, u);
                    s.cov = s.cov.max((0.95 * e * f).min(1.0));
                    s.top = s.top.max(s.base + (1500.0 + 2800.0 * e) * (1.0 - 0.6 * smooth(0.0, 1.0, u)));
                    s.precip = s.precip.max((0.8 * wet * e * (1.0 - smooth(0.1, 0.5, u))).min(1.0));
                };
                s.top = s.base;
                match self.bits(16) % 3 {
                    // a knot of two or three smaller showers
                    1 => {
                        for k in 0..2 + self.bits(24) as usize % 2 {
                            let (cx, cz, rk, ek) = self.cell(k, 0.75, 0.55, 0.25, e);
                            shower(&mut s, ((dx - cx).powi(2) + (dz - cz).powi(2)).sqrt() / rk * rag, ek);
                        }
                    }
                    // a band of showers trailing downwind
                    2 => {
                        let along = dx * wind.0 + dz * wind.1;
                        let across = -dx * wind.1 + dz * wind.0;
                        let ub = ((along / (self.r * 2.4)).powi(2) + (across / (self.r * 0.7)).powi(2)).sqrt();
                        let beads = 0.75 + 0.5 * vnoise1(along as f64 / (self.r as f64 * 0.5), self.seed);
                        shower(&mut s, ub * rag * beads, e);
                    }
                    _ => shower(&mut s, u, e),
                }
            }
            Kind::Storm => {
                let top = 6000.0 + 6000.0 * e;
                let tower = |s: &mut Sample, u: f32, e: f32, top: f32| {
                    let f = 1.0 - smooth(0.4, 1.0, u);
                    s.cov = s.cov.max((1.25 * e * f).min(1.0));
                    let dome = (1.0 - 0.85 * u * u).max(0.0).sqrt();
                    s.top = s.top.max(s.base + (top - s.base) * dome);
                    s.precip = s.precip.max((1.1 * e * (1.0 - smooth(0.15, 0.6, u))).min(1.0));
                    s.storm = s.storm.max((e * (1.0 - smooth(0.1, 0.7, u))).min(1.0));
                };
                s.top = s.base;
                let form = self.storm_form();
                match form {
                    // a cluster of towers, each at its own stage: one building, one at its height, one raining out
                    StormForm::Cluster => {
                        for k in 0..3 + self.bits(24) as usize % 2 {
                            let (cx, cz, rk, ek) = self.cell(k, 0.6, 0.5, 0.4, e);
                            let u = ((dx - cx).powi(2) + (dz - cz).powi(2)).sqrt() / rk * rag;
                            tower(&mut s, u, ek, 5000.0 + 6500.0 * ek);
                        }
                    }
                    // a squall line: a wall of towers across the wind, heavy rain along its leading edge and lighter
                    // rain from the layered cloud trailing behind
                    StormForm::Line => {
                        let along = dx * wind.0 + dz * wind.1;
                        let across = -dx * wind.1 + dz * wind.0;
                        let ul = ((across / (self.r * 3.2)).powi(2) + (along / (self.r * 0.6)).powi(2)).sqrt();
                        let bumps = 0.65 + 0.35 * vnoise1(across as f64 / (self.r as f64 * 0.45), self.seed);
                        let line_top = s.base + (top - s.base) * bumps;
                        tower(&mut s, ul * rag, e, line_top);
                        let back = along + self.r * 1.4;
                        let ut = ((across / (self.r * 2.9)).powi(2) + (back / (self.r * 1.1)).powi(2)).sqrt() * rag;
                        let ft = 1.0 - smooth(0.4, 1.0, ut);
                        s.cov = s.cov.max(0.95 * e * ft);
                        s.top = s.top.max(s.base + 2200.0 * e * ft);
                        s.precip = s.precip.max(0.4 * e * (1.0 - smooth(0.2, 0.8, ut)));
                        s.layered = s.layered.max(0.7 * ft * (1.0 - smooth(0.3, 0.9, ul)));
                    }
                    // a lone pulse storm: small and tall
                    StormForm::Pulse => tower(&mut s, u * 1.5, e, top + 1500.0),
                    StormForm::Cell => tower(&mut s, u, e, top),
                }
                // the anvil: spread downwind at the top of the tower (along a squall line, the whole line's)
                let wide = if form == StormForm::Line { 3.4 } else { 1.8 };
                let (ax, az) = (dx - wind.0 * self.r * 1.1, dz - wind.1 * self.r * 1.1);
                let along = ax * wind.0 + az * wind.1;
                let across = -ax * wind.1 + az * wind.0;
                let ua = ((along / (self.r * 2.8)).powi(2) + (across / (self.r * wide)).powi(2)).sqrt();
                // (an anvil has a fairly sharp edge)
                s.high = 0.95 * smooth(0.35, 0.8, e) * (1.0 - smooth(0.6, 1.0, ua));
                // (low enough that the tower's flared head runs into it)
                s.high_y = top - 1800.0;
            }
            Kind::Front => {
                let f = 1.0 - smooth(0.45, 1.0, u);
                s.cov = (1.05 * e * f).min(1.0);
                s.top = s.base + 2500.0 + 2000.0 * e * f;
                s.precip = (0.65 * e * (1.0 - smooth(0.15, 0.7, u))).min(1.0);
                s.layered = 0.85;
                // a sheet of cirrostratus runs ahead of it
                let ahead = self.r * 2.6;
                let (ax, az) = (dx - wind.0 * ahead, dz - wind.1 * ahead);
                let (lx2, lz2) = (ax * ca + az * sa, -ax * sa + az * ca);
                let uh = ((lx2 / (self.r * self.stretch * 1.2)).powi(2) + (lz2 / (self.r * 2.4)).powi(2)).sqrt();
                s.high = 0.75 * e * (1.0 - smooth(0.3, 1.0, uh));
                s.high_y = 8500.0;
            }
            Kind::Deck => {
                let f = 1.0 - smooth(0.5, 1.0, u);
                s.cov = (0.97 * e * f).min(1.0);
                s.top = s.base + 350.0 + 450.0 * e;
                s.precip = 0.12 * e * (1.0 - smooth(0.0, 0.5, u));
                s.layered = 1.0;
            }
        }
        s
    }
}

pub struct Strike {
    pub x: f64,
    pub z: f64,
    /// Real seconds (`Sim::real`) when it struck.
    pub at: f64,
    pub seed: u32,
}

pub struct Thunder {
    /// Real seconds (`Sim::real`) when it is heard.
    pub due: f64,
    pub x: f64,
    pub z: f64,
}

pub struct Sim {
    pub seed: u32,
    /// Seconds of weather since the world was first seen.
    pub time: f64,
    /// Real seconds this session: a flash and its thunder keep real time however fast the weather runs, so sound
    /// still crosses the distance at 343 m/s and a flash fades even with the weather held still.
    pub real: f64,
    /// How far the wind has carried the air since then (for the clouds' fine detail), wrapped.
    pub drift: (f64, f64),
    pub systems: Vec<System>,
    pub rng: Rng,
    /// Wind override: angle (radians, the way it blows towards) and speed (m/s).
    pub wind_fix: Option<(f32, f32)>,
    /// The day's character, held; none lets the days follow one another.
    pub regime_fix: Option<Regime>,
    /// Weather seconds a game second (time-lapse).
    pub speed: f32,
    pub strike: Option<Strike>,
    pub thunder: Vec<Thunder>,
    /// The game clock in days (0 is 6:00 on the first day), set by the caller.
    pub day: f64,
    /// How much the hour stirs up cumulus, 0..1 (set by the caller from the time of day).
    pub convection: f32,
    /// The climate under the sky (asked of the world by the caller; not saved).
    pub climate: Climate,
    spawn_wait: [f32; 5],
    /// The regimes of the days so far, worked out once.
    chain: Vec<Regime>,
}

/// Where a drift wraps: a whole number of every noise period the shader uses.
pub const DRIFT_WRAP: f64 = 1_048_576.0;

impl Sim {
    pub fn new(seed: u32) -> Sim {
        Sim {
            seed,
            time: 0.0,
            real: 0.0,
            drift: (0.0, 0.0),
            systems: Vec::new(),
            rng: Rng(hash(seed) | 1),
            wind_fix: None,
            regime_fix: None,
            speed: 1.0,
            strike: None,
            thunder: Vec::new(),
            day: 0.0,
            convection: 1.0,
            climate: Climate::default(),
            spawn_wait: [0.0; 5],
            chain: vec![Regime::Fair],
        }
    }

    /// The character of day `d` (each day follows from the one before).
    pub fn regime_of(&mut self, d: i64) -> Regime {
        let d = d.clamp(0, 200_000) as usize;
        while self.chain.len() <= d {
            let prev = *self.chain.last().unwrap();
            let w = prev.next();
            let total: u32 = w.iter().sum();
            let mut roll = hash(self.seed ^ (self.chain.len() as u32).wrapping_mul(0x9E37_79B1)) % total;
            let mut pick = Regime::Fair;
            for (i, &wi) in w.iter().enumerate() {
                if roll < wi {
                    pick = REGIMES[i];
                    break;
                }
                roll -= wi;
            }
            self.chain.push(pick);
        }
        self.chain[d]
    }

    /// Today's and tomorrow's character, and how far the change has come (it turns over through evening and night).
    pub fn regimes(&mut self) -> (Regime, Regime, f32) {
        if let Some(r) = self.regime_fix {
            return (r, r, 0.0);
        }
        let d = self.day.floor() as i64;
        let f = (self.day - self.day.floor()) as f32;
        (self.regime_of(d), self.regime_of(d + 1), smooth(0.55, 1.0, f))
    }

    /// Where the wind blows towards (radians from +x, so 0 is east) and how fast (m/s): the day's wind, turning as
    /// one day gives way to the next, wandering and gusting through the day.
    pub fn wind(&mut self) -> (f32, f32) {
        if let Some(w) = self.wind_fix {
            return w;
        }
        let (a, b, t) = self.regimes();
        let (fa, sa) = a.wind();
        let (fb, sb) = b.wind();
        // blend the directions the short way round
        let mut d = (fb - fa) % 360.0;
        if d > 180.0 {
            d -= 360.0;
        } else if d < -180.0 {
            d += 360.0;
        }
        let from = fa + d * t + (vnoise1(self.day * 3.0, self.seed ^ 0x51) - 0.5) * 70.0;
        let speed = (sa + (sb - sa) * t) * (0.75 + 0.5 * vnoise1(self.day * 12.0 + 17.0, self.seed ^ 0x99));
        ((from + 180.0).to_radians(), speed)
    }

    pub fn wind_dir(&mut self) -> (f32, f32) {
        let (a, _) = self.wind();
        (a.cos(), a.sin())
    }

    /// Weather seconds a game second: the pace times `/skies speed`.
    pub fn rate(&self) -> f32 {
        self.speed * PACE
    }

    /// How wet the air is over `x, z`, 0..1: wet regions breed storms and fog, dry ones keep fair skies. Over land the
    /// world has described, half of it is the land's own (jungle and swamp wet, desert and savanna dry, the sea moist),
    /// the rest the day's moist and dry tongues of air wandering over it.
    pub fn humidity(&self, x: f64, z: f64) -> f32 {
        let a = vnoise2(x / 40_000.0, z / 40_000.0, self.seed ^ 0xabc);
        let b = vnoise2(x / 12_000.0, z / 12_000.0, self.seed ^ 0xdef);
        let air = (a * 0.7 + b * 0.3).clamp(0.0, 1.0);
        match self.climate.at(x, z) {
            Some([_, wet, _]) => (0.5 * wet + 0.5 * air).clamp(0.0, 1.0),
            None => air,
        }
    }

    /// The air's warmth over `x, z` in vanilla's biome temperature (snow below 0.15, plains 0.8, desert 2), and how
    /// much of the ground is sea. Mild until the world has said.
    pub fn air(&self, x: f64, z: f64) -> (f32, f32) {
        match self.climate.at(x, z) {
            Some([t, _, sea]) => (t, sea),
            None => (0.7, 0.0),
        }
    }

    /// The height (y) of the freezing level over `x, z`: above it what falls is snow, below it rain. Low over cold
    /// land, high over warm; the day's air mass moves it, and it sinks a little at night.
    pub fn freezing_level(&mut self, x: f64, z: f64) -> f32 {
        let shift = self.freeze_shift();
        freeze_at(self.air(x, z).0, shift)
    }

    /// What the day's air mass and the hour do to the freezing level, in blocks.
    fn freeze_shift(&mut self) -> f32 {
        let (a, b, f) = self.regimes();
        let shift = a.freeze_shift() + (b.freeze_shift() - a.freeze_shift()) * f;
        let hour = self.day.rem_euclid(1.0) as f32;
        // coldest before dawn, warmest in the afternoon
        shift - 20.0 * (1.0 - smooth(0.0, 0.25, hour) * (1.0 - smooth(0.55, 0.95, hour)))
    }

    /// Everything above `x, z`, combined.
    pub fn at(&mut self, x: f64, z: f64) -> Sample {
        let wind = self.wind_dir();
        let mut acc = Acc::default();
        for s in &self.systems {
            let r = s.reach() as f64;
            if (x - s.x).abs() > r || (z - s.z).abs() > r {
                continue;
            }
            acc.add(s.sample(x, z, wind));
        }
        acc.finish()
    }

    /// Adds a system of `kind` centred at `x, z`, already `phase` of the way through its life.
    pub fn add(&mut self, kind: Kind, x: f64, z: f64, phase: f32) {
        let wa = self.wind().0;
        let h = self.humidity(x, z);
        let (warmth, _) = self.air(x, z);
        let (_, base_bg) = self.background();
        let r = &mut self.rng;
        // (radius m, stretch, life s, angle)
        let (radius, stretch, life, angle) = match kind {
            Kind::Fair => (r.range(3000.0, 7000.0), r.range(1.0, 1.8), r.range(1500.0, 3000.0), r.range(0.0, 6.283)),
            Kind::Shower => (r.range(1500.0, 3000.0), r.range(1.0, 1.4), r.range(700.0, 1300.0), r.range(0.0, 6.283)),
            Kind::Storm => (r.range(3000.0, 5500.0), r.range(1.0, 1.3), r.range(2400.0, 3800.0), r.range(0.0, 6.283)),
            // a front lies across the wind
            Kind::Front => (r.range(10_000.0, 16_000.0), r.range(3.0, 5.0), r.range(4000.0, 7000.0), wa + 1.5708 + r.range(-0.3, 0.3)),
            Kind::Deck => (r.range(14_000.0, 26_000.0), r.range(1.0, 2.0), r.range(4000.0, 8000.0), r.range(0.0, 6.283)),
        };
        // cloud bases: lower in wet air, higher over hot, dry land
        let lift = 500.0 * (warmth - 0.8).clamp(0.0, 1.2) * (1.0 - h);
        let base = lift
            + match kind {
            Kind::Fair => base_bg + 300.0 * (0.5 - h) + r.range(-120.0, 120.0),
            Kind::Shower => base_bg * 0.85 + r.range(-80.0, 80.0),
            Kind::Storm => 650.0 + 350.0 * (1.0 - h) + r.range(-80.0, 80.0),
            Kind::Front => 380.0 + r.range(-80.0, 160.0),
            Kind::Deck => 450.0 + r.range(0.0, 450.0),
        };
        let seed = r.next();
        let power = r.range(0.7, 1.15);
        let (vx, vz) = (r.range(-1.2, 1.2), r.range(-1.2, 1.2));
        let strike_in = r.range(3.0, 12.0);
        self.systems.push(System {
            kind,
            x,
            z,
            r: radius,
            stretch,
            angle,
            age: life * phase,
            life,
            power,
            base: base.max(250.0),
            seed,
            vx,
            vz,
            strike_in,
            told: false,
        });
    }

    /// The scattered cloud everywhere: coverage and cumulus base, for the day as it stands.
    pub fn background(&mut self) -> (f32, f32) {
        let (a, b, t) = self.regimes();
        let (ca, ba) = a.background();
        let (cb, bb) = b.background();
        (ca + (cb - ca) * t, ba + (bb - ba) * t)
    }

    /// How many of each kind the sky round the player should hold now.
    fn target(&mut self, kind: Kind) -> usize {
        let (a, b, t) = self.regimes();
        let i = KINDS.iter().position(|&k| k == kind).unwrap();
        let n = a.targets()[i] * (1.0 - t) + b.targets()[i] * t;
        // heaped cloud grows with the day's heating and sinks away at night
        let heat = match kind {
            Kind::Fair => 0.25 + 0.75 * self.convection,
            Kind::Shower => 0.35 + 0.65 * self.convection,
            Kind::Storm => 0.3 + 0.7 * self.convection,
            _ => 1.0,
        };
        (n * heat).round() as usize
    }

    /// A place for a new system round the player: further away for storms and fronts, and more often upwind, so the
    /// weather that matters arrives rather than pops up overhead.
    fn spawn_point(&mut self, px: f64, pz: f64, kind: Kind) -> (f64, f64) {
        let (wx, wz) = self.wind_dir();
        let upwind = matches!(kind, Kind::Storm | Kind::Front | Kind::Deck) && self.rng.f() < 0.75;
        let ang = if upwind {
            (-wz).atan2(-wx) as f64 + self.rng.range(-0.8, 0.8) as f64
        } else {
            self.rng.range(0.0, std::f32::consts::TAU) as f64
        };
        let lo = if kind == Kind::Fair { 3000.0 } else { SPAWN_MIN };
        let hi = if kind == Kind::Storm { STORM_SPAWN_MAX } else { SPAWN_MAX };
        let d = lo + (hi - lo) * (self.rng.f() as f64).sqrt();
        (px + ang.cos() * d, pz + ang.sin() * d)
    }

    /// Fills the sky round a player who has just arrived: systems scattered through their lives.
    /// Whether no system is within reach of (px, pz): a fresh world, or the sky left behind after a long journey.
    pub fn empty_around(&self, px: f64, pz: f64) -> bool {
        !self.systems.iter().any(|s| ((s.x - px).powi(2) + (s.z - pz).powi(2)).sqrt() < SPAWN_MAX)
    }

    pub fn populate(&mut self, px: f64, pz: f64) {
        for kind in KINDS {
            let n = self.target(kind);
            for _ in 0..n {
                let ang = self.rng.range(0.0, std::f32::consts::TAU) as f64;
                let lo = if matches!(kind, Kind::Storm | Kind::Front) { 12_000.0 } else { 0.0 };
                let d = lo + (32_000.0 - lo) * (self.rng.f() as f64).sqrt();
                let phase = self.rng.range(0.2, 0.65);
                self.add(kind, px + ang.cos() * d, pz + ang.sin() * d, phase);
            }
        }
    }

    /// Advances the weather by `dt` game seconds round a player at `px, pz`.
    pub fn step(&mut self, dt: f32, px: f64, pz: f64) {
        self.real += dt as f64;
        let real = self.real;
        // lightning keeps real time (it stops with the weather held, and does not hurry in a time-lapse)
        let flash_dt = dt * self.speed.min(1.0);
        let dt = dt * self.rate();
        self.time += dt as f64;
        let (wa, ws) = self.wind();
        let (wx, wz) = (wa.cos() * ws, wa.sin() * ws);
        self.drift.0 = (self.drift.0 + (wx * dt) as f64).rem_euclid(DRIFT_WRAP);
        self.drift.1 = (self.drift.1 + (wz * dt) as f64).rem_euclid(DRIFT_WRAP);

        let time = self.time;
        let mut strikes = Vec::new();
        for s in &mut self.systems {
            s.age += dt;
            s.x += ((wx + s.vx) * dt) as f64;
            s.z += ((wz + s.vz) * dt) as f64;
            if s.kind == Kind::Storm {
                let e = s.envelope() * s.power;
                if e > 0.3 {
                    s.strike_in -= flash_dt;
                    if s.strike_in <= 0.0 {
                        let h = hash(s.seed ^ (time as u32).wrapping_mul(2654435761));
                        let a = (h & 0xffff) as f32 / 65536.0 * std::f32::consts::TAU;
                        let d = ((h >> 16) as f32 / 65536.0).sqrt() * s.r * 0.45;
                        strikes.push((s.x + (a.cos() * d) as f64, s.z + (a.sin() * d) as f64, h));
                        let gap = 2.0 + 9.0 * ((h >> 5) & 0xff) as f32 / 255.0;
                        s.strike_in = gap / e;
                    }
                }
            }
        }
        for (x, z, h) in strikes {
            self.strike = Some(Strike { x, z, at: real, seed: h });
            let d = ((x - px).powi(2) + (z - pz).powi(2)).sqrt();
            // sound covers 343 metres a second; thunder is heard within about twelve kilometres
            if d < crate::THUNDER_RANGE {
                self.thunder.push(Thunder { due: real + d / 343.0, x, z });
            }
        }
        self.systems.retain(|s| s.age < s.life && ((s.x - px).powi(2) + (s.z - pz).powi(2)).sqrt() < KEEP);

        // refill the sky
        for (i, kind) in KINDS.into_iter().enumerate() {
            self.spawn_wait[i] -= dt;
            if self.spawn_wait[i] > 0.0 {
                continue;
            }
            let have = self.systems.iter().filter(|s| s.kind == kind).count();
            if have < self.target(kind) {
                // try a few places; wet air takes storms and showers, dry air keeps fair skies; thunderstorms need
                // warm air, and cold land and the sea take grey decks
                for _ in 0..4 {
                    let (x, z) = self.spawn_point(px, pz, kind);
                    let h = self.humidity(x, z);
                    let (t, sea) = self.air(x, z);
                    let cold = 1.0 - smooth(0.0, 0.5, t);
                    let want = match kind {
                        Kind::Fair => (0.5 + 0.5 * (1.0 - h)) * (1.0 - 0.5 * cold),
                        Kind::Front => 0.6 + 0.4 * h,
                        Kind::Deck => ((0.6 + 0.4 * h) * (0.7 + 0.6 * cold + 0.5 * sea)).min(1.0),
                        Kind::Storm => smooth(0.2, 0.75, h) * smooth(0.15, 0.7, t),
                        Kind::Shower => smooth(0.2, 0.75, h),
                    };
                    if self.rng.f() < want {
                        self.add(kind, x, z, 0.0);
                        break;
                    }
                }
            }
            self.spawn_wait[i] = match kind {
                Kind::Fair => 40.0,
                Kind::Shower => 60.0,
                Kind::Storm => 150.0,
                Kind::Front => 600.0,
                Kind::Deck => 500.0,
            };
        }
    }

    /// Starts drawing the weather into the grid centred on the cell holding `px, pz`.
    pub fn raster_begin(&self, px: f64, pz: f64) -> Raster {
        let c = CELL as f64;
        let ox = ((px / c).floor() - (G / 2) as f64) * c;
        let oz = ((pz / c).floor() - (G / 2) as f64) * c;
        Raster { origin: (ox, oz), ..Default::default() }
    }

    /// Draws the next `rows` rows of the grid `r` into `words`, and once it is whole, the summary of the sky past its
    /// edge; true then. `fog` is how much valley fog the hour allows (0..1), cell by cell scaled by the air's
    /// humidity. The grid is drawn a band at a time so no one tick carries the cost of all of it.
    pub fn raster_rows(&mut self, r: &mut Raster, rows: usize, fog: f32, words: &mut [u32]) -> bool {
        let c = CELL as f64;
        let (ox, oz) = r.origin;
        let (ja, jb) = (r.row, (r.row + rows).min(G));
        let wind = self.wind_dir();
        let (bg_cov, bg_base) = self.background();
        let mut acc = vec![Acc::default(); (jb - ja) * G];
        let freeze_shift = self.freeze_shift();
        for s in &self.systems {
            let reach = s.reach() as f64;
            let i0 = (((s.x - reach - ox) / c).floor().max(0.0)) as usize;
            let i1 = (((s.x + reach - ox) / c).ceil().min(G as f64 - 1.0)).max(-1.0) as isize;
            let j0 = (((s.z - reach - oz) / c).floor().max(ja as f64)) as usize;
            let j1 = (((s.z + reach - oz) / c).ceil().min(jb as f64 - 1.0)).max(-1.0) as isize;
            if i1 < i0 as isize || j1 < j0 as isize {
                continue;
            }
            for j in j0..=j1 as usize {
                for i in i0..=i1 as usize {
                    let x = ox + (i as f64 + 0.5) * c;
                    let z = oz + (j as f64 + 0.5) * c;
                    let smp = s.sample(x, z, wind);
                    if smp.cov > 0.002 || smp.high > 0.002 {
                        acc[(j - ja) * G + i].add(smp);
                    }
                }
            }
        }
        // the scattered cumulus everywhere else: more in wet air and by day
        let day = 0.3 + 0.7 * self.convection;
        for j in ja..jb {
            for i in 0..G {
                let x = ox + (i as f64 + 0.5) * c;
                let z = oz + (j as f64 + 0.5) * c;
                let h = self.humidity(x, z);
                let a = &mut acc[(j - ja) * G + i];
                let bg = bg_cov * (0.5 + 1.0 * h) * day;
                let base = bg_base + 250.0 * (0.5 - h);
                a.add(Sample { cov: bg, base, top: base + 300.0 + 700.0 * h * day, ..Default::default() });
                let s = a.finish();
                let fogc = (fog * smooth(0.25, 0.7, h) + s.precip * 0.35).min(1.0);
                let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
                let m = |v: f32| (v.clamp(0.0, 65535.0) + 0.5) as u32;
                let k = (j * G + i) * 4;
                words[k] = q(s.cov) | q(s.high) << 8 | q(s.precip) << 16 | q(s.storm) << 24;
                words[k + 1] = m(s.base) | m(s.top) << 16;
                words[k + 2] = q(fogc) | q(s.layered) << 8 | q(s.high_y / 16000.0) << 16 | q(h) << 24;
                // the freezing level (64 under y 0 .. ), the air's warmth and the sea
                let (warmth, sea) = self.air(x, z);
                let freeze = freeze_at(warmth, freeze_shift);
                words[k + 3] = m(freeze + 64.0) | q(warmth / 2.0) << 16 | q(sea) << 24;
                r.freeze += freeze;
                r.warm += warmth;
                if s.cov > 0.01 {
                    r.max_top = r.max_top.max(s.top);
                }
                r.high_sum += s.high * s.high_y;
                r.high_w += s.high;
                if i < 4 || j < 4 || i >= G - 4 || j >= G - 4 {
                    // the outer ring, averaged (heights weighted by cover): how the sky carries on to the horizon
                    let w = s.cov.max(0.02);
                    r.ring[0] += s.cov;
                    r.ring[1] += s.base * w;
                    r.ring[2] += s.top * w;
                    r.ring[3] += s.layered * w;
                    r.ring[4] += s.precip;
                    r.ring[5] += s.high;
                    r.ring[6] += s.high_y;
                    r.ring[7] += h;
                    r.n += 1.0;
                    r.wsum += w;
                }
            }
        }
        r.row = jb;
        if jb < G {
            return false;
        }
        // (the freezing level's average over the whole grid, for the sky past it)
        let freeze = r.freeze / (G * G) as f32;
        let (n, w, g) = (r.n.max(1.0), r.wsum.max(1e-3), &r.ring);
        let vals = [g[0] / n, g[1] / w, g[2] / w, g[3] / w, g[4] / n, g[5] / n, g[6] / n, g[7] / n, r.max_top.max(bg_base + 1000.0)];
        for (t, v) in words[WORDS..WORDS + TAIL].iter_mut().zip(vals) {
            *t = v.to_bits();
        }
        // the height of the high cloud, one for all of it (anvils and cirrus), weighted by its cover
        let high_y = if r.high_w > 0.01 { r.high_sum / r.high_w } else { 9000.0 };
        words[WORDS + 11] = high_y.to_bits();
        words[WORDS + 12] = freeze.to_bits();
        words[WORDS + 13] = (r.warm / (G * G) as f32).to_bits();
        true
    }
}

/// The freezing level (y) over air of warmth `t` (vanilla's biome temperature, at sea level), moved `shift` blocks by
/// the day and hour: plains air (0.8) freezes about y 230, a snowy taiga's (-0.5) under the sea.
pub fn freeze_at(t: f32, shift: f32) -> f32 {
    (63.0 + (t - SNOW_TEMPERATURE) / LAPSE + shift).clamp(-64.0, 2000.0)
}

/// A grid being drawn, and the sums for the summary of the sky past its edge.
#[derive(Clone, Copy, Default)]
pub struct Raster {
    pub origin: (f64, f64),
    row: usize,
    ring: [f32; 8],
    n: f32,
    wsum: f32,
    max_top: f32,
    high_sum: f32,
    high_w: f32,
    freeze: f32,
    warm: f32,
}

/// Systems over one place, combined: coverage as a union, cloud base and layering weighted by coverage, the tallest
/// top, the highest anvil.
#[derive(Clone, Copy, Default)]
pub struct Acc {
    clear: f32,
    wsum: f32,
    base: f32,
    layered: f32,
    top: f32,
    precip: f32,
    storm: f32,
    high_clear: f32,
    high_y: f32,
    any: bool,
}

impl Acc {
    pub fn add(&mut self, s: Sample) {
        if !self.any {
            self.any = true;
            self.clear = 1.0;
            self.high_clear = 1.0;
        }
        self.clear *= 1.0 - s.cov.clamp(0.0, 1.0);
        if s.high > 0.0 {
            self.high_clear *= 1.0 - s.high.clamp(0.0, 1.0);
            self.high_y = self.high_y.max(s.high_y);
        }
        if s.cov > 0.0 {
            self.wsum += s.cov;
            self.base += s.base * s.cov;
            self.layered += s.layered * s.cov;
            let top = s.base + (s.top - s.base) * (s.cov * 2.5).min(1.0);
            self.top = self.top.max(top);
        }
        self.precip = self.precip.max(s.precip);
        self.storm = self.storm.max(s.storm);
    }
    pub fn finish(&self) -> Sample {
        if !self.any {
            return Sample { base: 1200.0, top: 1200.0, high_y: 8000.0, ..Default::default() };
        }
        let (base, layered) = if self.wsum > 0.0 { (self.base / self.wsum, self.layered / self.wsum) } else { (1200.0, 0.0) };
        Sample {
            cov: 1.0 - self.clear,
            base,
            top: self.top.max(base),
            precip: self.precip,
            storm: self.storm,
            high: 1.0 - self.high_clear,
            high_y: if self.high_y > 0.0 { self.high_y } else { 8000.0 },
            layered,
        }
    }
}
