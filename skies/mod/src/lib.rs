//! Big Skies (Horizon Skies): regional dynamic weather.
//!
//! The mod keeps a simulation of weather systems that live and drift across the world round the player (sim.rs), at
//! the real world's scale (a block is a metre), draws them into a grid of cells 65 kilometres across, and hands the
//! grid to a post pass (shaders/skies.wgsl) that draws a physically based sky, volumetric clouds, distant rain shafts,
//! rainbows, cloud shadows, lightning and valley fog over the world and Horizon's distant terrain. Vanilla's own
//! weather follows whatever is overhead, so the rain you stand in, its sound and snow in the cold are vanilla's.
//!
//! The world's biomes, asked of its generator for 50 kilometres round (`world.read-climate`), shape the weather: how
//! wet and warm the air is over them, and so what clouds form, and the freezing level, above which what falls is
//! snow.
//!
//! Waterfalls (water falling ten blocks or more into a pool, vanilla's or a generator's) are found in the blocks round
//! the player (falls.rs); the shader draws mist and a spray rainbow over their pools, and the mod adds spray and
//! bubbles.

mod falls;
mod noise;
mod sim;

use horizon_mod_sdk::{Mod, Registration, chat, commands, game, render, storage, world};
use noise::{hash, smooth};
use sim::{COVER_AT, COVER_N, DATA, FALLS_AT, G, Kind, NOISE_AT, Regime, Sim, WORDS};

const SHADER: &str = include_str!("../shaders/skies.wgsl");
/// Ticks between grid redraws; the shader carries the clouds on with the wind in between.
const RASTER_EVERY: u64 = 20;
/// Grid rows drawn a tick: the grid takes eight ticks to draw.
const RASTER_ROWS: usize = 16;
/// How far away thunder is heard, metres.
pub const THUNDER_RANGE: f64 = 12_000.0;
/// Seconds a rainbow lingers after the rain stops.
const RAINBOW_FOR: f32 = 150.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Vanilla {
    Clear,
    Rain,
    Thunder,
}

struct Skies {
    sim: Sim,
    words: Vec<u32>,
    origin: (f64, f64),
    /// Sim time when the grid was drawn.
    raster_at: f64,
    /// The grid being drawn, and when it was begun.
    pending: Option<(sim::Raster, f64)>,
    ticks: u64,
    since_tick: f32,
    /// Every frame time so far, hashed: the one thing that differs between first runs (a new world's clock and
    /// spawn are much the same each time, and a mod has no random source).
    jitter: u32,
    on: bool,
    pass_live: bool,
    started: bool,
    vanilla: Option<Vanilla>,
    /// Keep vanilla's weather in step with the sky overhead.
    sync: bool,
    player: Option<(f64, f64, f64)>,
    day_time: u64,
    fog: (f32, f32),
    /// The coming morning's fog, forced on.
    fog_forced: Option<u64>,
    /// Tonight's northern lights, forced on (the night's number).
    aurora_forced: Option<u64>,
    /// The northern lights' strength as the shader has it, eased toward tonight's.
    aurora: f32,
    quality: u32,
    /// Seconds of rainbow left after rain stopped here.
    rainbow: f32,
    /// Whether the ground round the player is snowy (so vanilla snows here), 0..1.
    snowy: f32,
    /// A climate query on its way: the grid's corner, and the tick it was asked.
    climate_asked: Option<(f64, f64, u64)>,
    /// The tick the climate was last asked for.
    climate_at: u64,
    falls: falls::Falls,
    /// The rain-cover map on its way: its corner, and the tick it was asked; and the corner of the one the shader has.
    cover_asked: Option<(i32, i32, u64)>,
    cover_at: Option<(i32, i32)>,
    /// What the game's own lighting (a shader pack such as Natural Lighting) was last told about the sky (`render.set-lighting`), eased toward the
    /// sky now so a passing cloud dims the sun over a second or so: direct light, then the horizon's and zenith's
    /// colours.
    lit: Option<[f32; 7]>,
}

fn say(text: &str) {
    chat::show(text);
}

fn compass(dx: f64, dz: f64) -> &'static str {
    // +x east, +z south
    let a = dz.atan2(dx).to_degrees();
    let a = (a + 360.0 + 22.5) % 360.0;
    ["east", "south-east", "south", "south-west", "west", "north-west", "north", "north-east"][(a / 45.0) as usize % 8]
}

impl Skies {
    fn day_fraction(&self) -> f32 {
        (self.day_time % 24000) as f32 / 24000.0
    }

    /// Valley fog: (amount, lift). It gathers in the small hours on some mornings (likelier after clear or rainy
    /// days, rare under an overcast) and lifts off the valleys in the two hours after sunrise.
    fn fog_now(&mut self) -> (f32, f32) {
        let f = self.day_fraction();
        let morning = (self.day_time + 12000) / 24000;
        let eve = self.sim.regime_of(morning as i64 - 1);
        let roll = (hash(self.sim.seed ^ (morning as u32).wrapping_mul(0x9E37_79B1)) & 0xffff) as f32 / 65536.0;
        let foggy = self.fog_forced == Some(morning) || roll < eve.fog_chance();
        if !foggy {
            return (0.0, 0.0);
        }
        if f >= 0.5 {
            (smooth(0.6, 0.86, f), 0.0)
        } else {
            (1.0 - smooth(0.07, 0.15, f), smooth(0.0, 0.14, f))
        }
    }

    /// Heaped cloud builds through the morning, peaks in the afternoon and sinks away in the evening.
    fn convection(&self) -> f32 {
        let f = self.day_fraction();
        if f < 0.62 { smooth(0.04, 0.3, f) * (1.0 - smooth(0.48, 0.62, f)) } else { 0.0 }
    }

    fn save(&self) {
        let s = &self.sim;
        let mut b = Vec::with_capacity(64 + s.systems.len() * 52);
        b.extend_from_slice(b"SKY2");
        b.extend_from_slice(&s.seed.to_le_bytes());
        b.extend_from_slice(&s.time.to_le_bytes());
        b.extend_from_slice(&s.drift.0.to_le_bytes());
        b.extend_from_slice(&s.drift.1.to_le_bytes());
        b.extend_from_slice(&s.speed.to_le_bytes());
        let (wf, wa, ws) = match s.wind_fix {
            Some((a, v)) => (1u8, a, v),
            None => (0, 0.0, 0.0),
        };
        b.push(wf);
        b.extend_from_slice(&wa.to_le_bytes());
        b.extend_from_slice(&ws.to_le_bytes());
        b.push(s.regime_fix.map_or(255, |r| r.index()));
        b.extend_from_slice(&self.quality.to_le_bytes());
        b.push(self.sync as u8);
        b.extend_from_slice(&(s.systems.len() as u32).to_le_bytes());
        for y in &s.systems {
            b.push(y.kind as u8);
            b.extend_from_slice(&y.x.to_le_bytes());
            b.extend_from_slice(&y.z.to_le_bytes());
            for v in [y.r, y.stretch, y.angle, y.age, y.life, y.power, y.base, y.vx, y.vz] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            b.extend_from_slice(&y.seed.to_le_bytes());
        }
        let _ = storage::set("state", &b);
    }

    fn load(&mut self, b: &[u8]) -> Option<()> {
        let mut at = 0usize;
        let mut take = |n: usize| -> Option<&[u8]> {
            let s = b.get(at..at + n)?;
            at += n;
            Some(s)
        };
        if take(4)? != b"SKY2" {
            return None;
        }
        let u32_ = |s: &[u8]| u32::from_le_bytes(s.try_into().unwrap());
        let f32_ = |s: &[u8]| f32::from_le_bytes(s.try_into().unwrap());
        let f64_ = |s: &[u8]| f64::from_le_bytes(s.try_into().unwrap());
        let finite = |v: f64| v.is_finite().then_some(v);
        let mut sim = Sim::new(u32_(take(4)?));
        sim.time = finite(f64_(take(8)?))?;
        sim.drift = (finite(f64_(take(8)?))?, finite(f64_(take(8)?))?);
        sim.speed = (finite(f32_(take(4)?) as f64)? as f32).clamp(0.0, 60.0);
        let wf = take(1)?[0];
        let (wa, ws) = (finite(f32_(take(4)?) as f64)? as f32, finite(f32_(take(4)?) as f64)? as f32);
        sim.wind_fix = (wf == 1).then_some((wa, ws));
        let rf = take(1)?[0];
        sim.regime_fix = sim::REGIMES.get(rf as usize).copied();
        let quality = u32_(take(4)?);
        let sync = take(1)?[0] != 0;
        let n = u32_(take(4)?) as usize;
        for _ in 0..n.min(400) {
            let kind = Kind::from_u8(take(1)?[0]);
            let (x, z) = (f64_(take(8)?), f64_(take(8)?));
            let mut v = [0f32; 9];
            for k in &mut v {
                *k = f32_(take(4)?);
            }
            let seed = u32_(take(4)?);
            if !(x.is_finite() && z.is_finite() && v.iter().all(|k| k.is_finite()) && v[0] > 0.0 && v[4] > 0.0) {
                return None;
            }
            sim.systems.push(sim::System {
                kind,
                x,
                z,
                r: v[0],
                stretch: v[1],
                angle: v[2],
                age: v[3],
                life: v[4],
                power: v[5],
                base: v[6],
                seed,
                vx: v[7],
                vz: v[8],
                strike_in: 5.0,
                told: true,
            });
        }
        sim.rng = noise::Rng(hash(sim.seed ^ sim.time as u32) | 1);
        self.sim = sim;
        self.quality = quality.min(2);
        self.sync = sync;
        Some(())
    }

    fn start_pass(&mut self) {
        render::set_post(SHADER);
        self.pass_live = true;
        self.push_params();
    }

    fn stop_pass(&mut self) {
        if self.pass_live {
            render::clear_post();
            render::clear_lighting();
            self.pass_live = false;
            self.lit = None;
        }
    }

    /// Toward the sun (x east, y up), as vanilla turns it through the day.
    fn sun_dir(&self) -> (f32, f32) {
        let f = ((self.day_time % 24000) as f32 / 24000.0 - 0.25).rem_euclid(1.0);
        let g = 0.5 - (f * std::f32::consts::PI).cos() / 2.0;
        let a = (f * 2.0 + g) / 3.0 * std::f32::consts::TAU;
        (-a.sin(), a.cos())
    }

    /// Tells the game's own lighting (a shader pack such as Natural Lighting, when one is on) about the sky: how much of the sun's (or
    /// the moon's) light gets through the clouds between it and the player, and the sky's colours for what water
    /// reflects. Without this it would go by vanilla's rain, which follows the sky overhead only roughly.
    fn push_lighting(&mut self) {
        let Some((px, py, pz)) = self.player else { return };
        let (sx, sy) = self.sun_dir();
        // the light: the sun by day, the moon (opposite) by night
        let (lx, ly) = if sy > 0.0 { (sx, sy) } else { (-sx, -sy) };
        let bg = self.sim.background().0;
        let here = self.sim.at(px, pz);
        // where the line toward the light crosses the cloud base (no further than 8 km)
        let rise = (here.base as f64 - py).max(200.0);
        let reach = (rise * (lx / ly.max(0.12)) as f64).clamp(-8000.0, 8000.0);
        let toward = self.sim.at(px + reach, pz);
        let thick = smooth(0.0, 2500.0, toward.top - toward.base);
        // the scattered cumulus of the day covers part of the sky everywhere
        let cov = 1.0 - (1.0 - toward.cov) * (1.0 - bg * 0.6);
        let opacity = (0.6 + 0.35 * thick).max(toward.storm);
        let direct = (1.0 - cov * opacity) * (1.0 - 0.35 * toward.high) * (1.0 - 0.3 * here.precip);
        // The sky, near enough to the shader's: blue by day, warm at the horizon round sunrise and sunset, deep blue
        // at night, and grey under cloud overhead (darker in storms).
        let day = smooth(-0.1, 0.3, sy);
        let low = smooth(0.35, 0.0, sy.abs()) * smooth(-0.15, 0.02, sy);
        let mix3 = |a: [f32; 3], b: [f32; 3], t: f32| [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t);
        let mut zenith = mix3([0.02, 0.035, 0.065], [0.36, 0.54, 0.76], day);
        let mut horizon = mix3([0.06, 0.08, 0.10], [0.84, 0.88, 0.90], day);
        horizon = mix3(horizon, [0.80, 0.66, 0.55], low * 0.5);
        zenith = mix3(zenith, [0.32, 0.40, 0.52], low * 0.5);
        let over = 1.0 - (1.0 - here.cov) * (1.0 - bg * 0.3);
        let grey = (0.2126 * horizon[0] + 0.7152 * horizon[1] + 0.0722 * horizon[2]) * 0.8 * (1.0 - 0.55 * here.storm);
        let cloud = [grey * 0.97, grey, grey * 1.04];
        horizon = mix3(horizon, cloud, over);
        zenith = mix3(zenith, cloud, over * 0.95);
        let target = [direct, horizon[0], horizon[1], horizon[2], zenith[0], zenith[1], zenith[2]];
        let lit = match self.lit {
            // a twentieth of the way a tick: most of the way in a second
            Some(mut lit) => {
                for (l, t) in lit.iter_mut().zip(target) {
                    *l += (t - *l) * 0.12;
                }
                lit
            }
            None => target,
        };
        self.lit = Some(lit);
        render::set_lighting(render::Lighting {
            direct: lit[0].clamp(0.0, 1.0),
            light: (1.0, 1.0, 1.0),
            horizon: (lit[1], lit[2], lit[3]),
            zenith: (lit[4], lit[5], lit[6]),
        });
    }

    fn push_params(&mut self) {
        let dt = (self.since_tick.min(0.1) * self.sim.rate()) as f64;
        let now = self.sim.time + dt;
        let (wa, ws) = self.sim.wind();
        let s = &self.sim;
        let (wx, wz) = ((wa.cos() * ws) as f64, (wa.sin() * ws) as f64);
        let since = now - self.raster_at;
        let real = s.real + self.since_tick.min(0.1) as f64;
        let (strike_x, strike_z, strike_age, strike_seed) = match &s.strike {
            Some(k) => (k.x as f32, k.z as f32, (real - k.at) as f32, (k.seed >> 8) as f32),
            None => (0.0, 0.0, 99.0, 0.0),
        };
        let bow = (self.rainbow / RAINBOW_FOR).clamp(0.0, 1.0);
        render::set_params(&[
            self.origin.0 as f32,
            self.origin.1 as f32,
            (wx * since) as f32,
            (wz * since) as f32,
            (s.drift.0 + wx * dt).rem_euclid(sim::DRIFT_WRAP) as f32,
            (s.drift.1 + wz * dt).rem_euclid(sim::DRIFT_WRAP) as f32,
            (now % 20000.0) as f32,
            // the moon's phase (0 full, as vanilla counts them) and the valley fog
            ((self.day_time / 24000) % 8) as f32 + self.fog.0 * 0.999,
            strike_x,
            strike_z,
            strike_age,
            strike_seed,
            self.fog.1,
            ws,
            // quality in the whole part, the lingering rainbow in the fraction
            self.quality as f32 + bow * 0.99,
            wa,
        ]);
    }

    /// Starts redrawing the grid; each tick draws a band of it, and the whole goes to the shader when done.
    fn raster(&mut self) {
        let Some((px, _, pz)) = self.player else { return };
        self.pending = Some((self.sim.raster_begin(px, pz), self.sim.time));
    }

    fn raster_step(&mut self) {
        let Some((mut r, at)) = self.pending.take() else { return };
        self.sim.convection = self.convection();
        if !self.sim.raster_rows(&mut r, RASTER_ROWS, self.fog.0, &mut self.words) {
            self.pending = Some((r, at));
            return;
        }
        self.origin = r.origin;
        self.raster_at = at;
        // for the shader's own rain close by: snow or rain, and how fast the weather runs (it falls in real time)
        self.words[WORDS + 9] = self.snow_here().to_bits();
        self.words[WORDS + 10] = self.sim.rate().to_bits();
        let aurora = self.aurora_tonight();
        self.aurora += (aurora - self.aurora) * 0.35;
        self.words[WORDS + 14] = self.aurora.to_bits();
        render::set_data(&self.words);
        if !self.pass_live {
            self.start_pass();
        }
    }

    /// How much what falls here is snow, 0..1: above the freezing level, or where snow lies on the ground.
    fn snow_here(&mut self) -> f32 {
        let Some((px, py, pz)) = self.player else { return self.snowy };
        if self.sim.climate.cells.is_empty() {
            return self.snowy;
        }
        let level = self.sim.freezing_level(px, pz) as f64;
        self.snowy.max(smooth(-20.0, 20.0, (py - level) as f32))
    }

    /// The northern lights over the player tonight, 0..1: on about one night in four, where the lowland air is cold
    /// (taiga, tundra, the frozen north; a mountain's own cold does not count), stronger some nights than others.
    /// The shader shows them only in the dark, and clouds hide them.
    fn aurora_tonight(&self) -> f32 {
        let night = (self.day_time + 12000) / 24000;
        if self.aurora_forced == Some(night) {
            return 1.0;
        }
        let Some((px, _, pz)) = self.player else { return 0.0 };
        if self.sim.climate.cells.is_empty() {
            return 0.0;
        }
        let cold = smooth(0.5, 0.2, self.sim.air(px, pz).0);
        let h = hash(self.sim.seed ^ hash(night as u32 ^ 0xA0_5EA1));
        if (h & 0xffff) as f32 / 65535.0 > 0.27 {
            return 0.0;
        }
        cold * (0.45 + 0.55 * ((h >> 16) & 0xff) as f32 / 255.0)
    }

    /// Vanilla weather follows the sky overhead (with some give, so the edge of a shower does not flicker).
    fn sync_vanilla(&mut self) {
        let Some((px, _, pz)) = self.player else { return };
        let here = self.sim.at(px, pz);
        let was = self.vanilla.unwrap_or(Vanilla::Clear);
        let raining = matches!(was, Vanilla::Rain | Vanilla::Thunder);
        let rain = if raining { here.precip > 0.14 } else { here.precip > 0.26 };
        let thunder = rain && if was == Vanilla::Thunder { here.storm > 0.3 } else { here.storm > 0.5 };
        // snow above the freezing level where vanilla's biome would rain: the shader's snow, and vanilla quiet
        let silent = rain && self.snowy < 0.3 && self.snow_here() > 0.5;
        let want = if silent {
            Vanilla::Clear
        } else if thunder {
            Vanilla::Thunder
        } else if rain {
            Vanilla::Rain
        } else {
            Vanilla::Clear
        };
        if self.vanilla != Some(want) {
            if raining && want == Vanilla::Clear && !silent {
                // the shower has passed: a rainbow, if the sun is out behind you
                self.rainbow = RAINBOW_FOR;
            }
            self.vanilla = Some(want);
            let cmd = match want {
                Vanilla::Clear => "weather clear 1000000",
                Vanilla::Rain => "weather rain 1000000",
                Vanilla::Thunder => "weather thunder 1000000",
            };
            world::run_commands(&[cmd.to_string()]);
        }
    }

    /// Thunder, softened by distance: the crack and roar of a strike close by, a long grumble a few kilometres off,
    /// a low rumble at the edge of hearing, and nothing further away.
    fn thunder(&mut self) {
        let Some((px, py, pz)) = self.player else { return };
        let now = self.sim.real;
        let due: Vec<_> = {
            let (ready, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut self.sim.thunder).into_iter().partition(|t| t.due <= now);
            self.sim.thunder = wait;
            ready
        };
        for t in due {
            let (dx, dz) = (t.x - px, t.z - pz);
            let d = (dx * dx + dz * dz).sqrt().max(1.0);
            if d > THUNDER_RANGE {
                continue;
            }
            // 0 overhead .. 1 at the edge of hearing
            let far = (d / THUNDER_RANGE) as f32;
            let at = (px + dx / d * 3.0, py + 2.0, pz + dz / d * 3.0);
            let vol = (1.3 * (-(d as f32) / 2600.0).exp()).max(0.07 * (1.0 - far));
            // distance takes the top off the sound: further thunder is lower
            let pitch = 0.95 - 0.42 * smooth(0.0, 1.0, far.sqrt());
            world::play_sound("minecraft:entity.lightning_bolt.thunder", Some(at), vol, pitch);
            if d < 900.0 {
                world::play_sound("minecraft:entity.lightning_bolt.impact", Some(at), (1.2 - d as f32 / 900.0).max(0.3), 0.9 + 0.2 * (1.0 - far));
            }
        }
    }

    /// Whether it snows rather than rains here, from the ground round the player: snow and ice lie where it is cold
    /// enough to snow (the mod has no word of biomes). Asked every few seconds, answered a tick or two later.
    fn look_for_snow(&mut self, answers: &[world::ColumnBlocks]) {
        const TAG: u32 = 0x5_0057;
        for ans in answers {
            if ans.tag != TAG {
                continue;
            }
            let cold: Vec<bool> = ans.palette.iter().map(|n| n.contains("snow") || n.ends_with("ice")).collect();
            let seen = ans.bases.iter().filter(|b| b.is_some()).count().max(1);
            let n = ans.blocks.chunks(2).filter(|c| c.iter().any(|&b| cold.get(b as usize).copied().unwrap_or(false))).count();
            self.snowy = n as f32 / seen as f32;
        }
        if self.ticks % 100 == 30 {
            if let Some((px, _, pz)) = self.player {
                let (x, z) = (px.floor() as i32, pz.floor() as i32);
                let cols: Vec<(i32, i32)> = (-2..=2).flat_map(|i| (-2..=2).map(move |j| (x + i * 6, z + j * 6))).collect();
                world::read_columns(TAG, &cols, world::Heightmap::WorldSurface, 1, 1);
            }
        }
    }

    /// What keeps the rain off round the player, for the shader's rain close by (which stops at each column's height
    /// on the motion-blocking heightmap, as vanilla's does): asked every second, and at once when the player nears
    /// the map's edge or leaves it (a teleport); answered a tick or two later.
    fn look_for_cover(&mut self, answers: &[world::ColumnBlocks]) {
        const TAG: u32 = 0x5_C0E5;
        let n = COVER_N as i32;
        for ans in answers {
            if ans.tag != TAG {
                continue;
            }
            let Some((ox, oz, _)) = self.cover_asked.take() else { continue };
            if ans.bases.len() != COVER_N * COVER_N {
                continue;
            }
            let w = &mut self.words[COVER_AT..COVER_AT + 4 + COVER_N * COVER_N];
            w[0] = ox as u32;
            w[1] = oz as u32;
            w[2] = 1;
            for (out, b) in w[4..].iter_mut().zip(&ans.bases) {
                *out = b.unwrap_or(i32::MIN) as u32;
            }
            self.cover_at = Some((ox, oz));
            if self.pass_live {
                render::set_data(&self.words);
            }
        }
        let Some((px, _, pz)) = self.player else { return };
        let (x, z) = (px.floor() as i32, pz.floor() as i32);
        if let Some((_, _, at)) = self.cover_asked {
            if self.ticks.saturating_sub(at) < 100 {
                return;
            }
            // never answered
            self.cover_asked = None;
        }
        // the player within 20 blocks of the map's middle: the rain drawn reaches about 30 blocks out
        let off = self.cover_at.is_none_or(|(ox, oz)| (x - ox - n / 2).abs().max((z - oz - n / 2).abs()) > 12);
        if !(off || self.ticks % 20 == 7) {
            return;
        }
        if off {
            // until the new map comes, the old one would put the roofs in the wrong places: no rain drawn close by
            self.words[COVER_AT + 2] = 0;
        }
        let (ox, oz) = (x - n / 2, z - n / 2);
        let cols: Vec<(i32, i32)> = (0..n).flat_map(|j| (0..n).map(move |i| (ox + i, oz + j))).collect();
        world::read_columns(TAG, &cols, world::Heightmap::MotionBlocking, 1, 0);
        self.cover_asked = Some((ox, oz, self.ticks));
    }

    /// Asks the world for the climate round the player: when there is none, the player has gone far from where it was
    /// asked, or every few minutes (another world). Answered a tick or two later.
    fn ask_climate(&mut self) {
        const TAG: u32 = 0x5_C11A;
        for ans in world::poll_climate() {
            if ans.tag != TAG {
                continue;
            }
            let Some((ox, oz, _)) = self.climate_asked.take() else { continue };
            self.sim.climate = sim::Climate::from_answer((ox, oz), &ans.biomes, &ans.temperatures, &ans.downfalls, &ans.columns);
            self.raster();
        }
        if let Some((_, _, at)) = self.climate_asked {
            if self.ticks.saturating_sub(at) < 400 {
                return;
            }
            // never answered
            self.climate_asked = None;
        }
        let Some((px, _, pz)) = self.player else { return };
        let since = self.ticks.saturating_sub(self.climate_at);
        let far = self.sim.climate.centre().is_none_or(|(cx, cz)| (px - cx).abs().max((pz - cz).abs()) > 16_000.0);
        if !(far && since > 100 || since > 3000) {
            return;
        }
        let (n, c) = (sim::CLIMATE_N, sim::CLIMATE_CELL);
        let ox = (px / c).round() * c - c * n as f64 / 2.0;
        let oz = (pz / c).round() * c - c * n as f64 / 2.0;
        let cols: Vec<(i32, i32)> = (0..n)
            .flat_map(|j| (0..n).map(move |i| ((ox + (i as f64 + 0.5) * c) as i32, (oz + (j as f64 + 0.5) * c) as i32)))
            .collect();
        world::read_climate(TAG, &cols);
        self.climate_asked = Some((ox, oz, self.ticks));
        self.climate_at = self.ticks;
    }

    /// A word in chat when a storm or front is on its way.
    fn tell(&mut self) {
        let Some((px, _, pz)) = self.player else { return };
        let (wa, ws) = self.sim.wind();
        let (wx, wz) = (wa.cos() * ws, wa.sin() * ws);
        let speed = self.sim.rate().max(0.01) as f64;
        for s in &mut self.sim.systems {
            if s.told || !matches!(s.kind, Kind::Storm | Kind::Front) || s.envelope() < 0.4 {
                continue;
            }
            let (dx, dz) = (s.x - px, s.z - pz);
            let d = (dx * dx + dz * dz).sqrt();
            if d > 30_000.0 || d < s.r as f64 {
                continue;
            }
            let closing = -((wx + s.vx) as f64 * dx + (wz + s.vz) as f64 * dz) / d;
            if closing < 1.0 {
                continue;
            }
            s.told = true;
            let edge = s.r as f64 * if s.kind == Kind::Front { 1.5 } else { 0.8 };
            let eta = ((d - edge).max(0.0) / closing / speed / 60.0).ceil();
            let what = if s.kind == Kind::Storm { "A thunderstorm" } else { "A rain front" };
            say(&format!("{what} {:.0} km to the {}, heading this way (about {eta} min)", d / 1000.0, compass(dx, dz)));
            break;
        }
    }

    fn status(&mut self) -> String {
        let Some((px, _, pz)) = self.player else { return "No player".into() };
        let here = self.sim.at(px, pz);
        let (wa, ws) = self.sim.wind();
        let from = compass(-(wa.cos() as f64), -(wa.sin() as f64));
        let (today, tomorrow, _) = self.sim.regimes();
        let mut lines = vec![format!(
            "Today {}, tomorrow {}. Overhead: cloud {:.0}% (base {:.0} m), rain {:.0}%, storm {:.0}%. Wind from the {from} at {:.0} m/s. {} systems.",
            today.name(),
            tomorrow.name(),
            here.cov * 100.0,
            here.base,
            here.precip * 100.0,
            here.storm * 100.0,
            ws,
            self.sim.systems.len()
        )];
        let mut near: Vec<_> = self
            .sim
            .systems
            .iter()
            .filter(|s| s.kind != Kind::Fair && s.envelope() > 0.15)
            .map(|s| {
                let (dx, dz) = (s.x - px, s.z - pz);
                ((dx * dx + dz * dz).sqrt(), s, compass(dx, dz))
            })
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (d, s, dir) in near.into_iter().take(4) {
            let stage = match s.age / s.life {
                a if a < 0.2 => "growing",
                a if a < 0.72 => "mature",
                _ => "dying",
            };
            lines.push(format!("{} ({stage}) {:.0} km {dir}", s.label(), d / 1000.0));
        }
        if !self.sim.climate.cells.is_empty() {
            let (warmth, sea) = self.sim.air(px, pz);
            let wet = self.sim.humidity(px, pz);
            let air = match warmth {
                t if t < 0.15 => "freezing",
                t if t < 0.5 => "cold",
                t if t < 0.95 => "mild",
                t if t < 1.5 => "warm",
                _ => "hot",
            };
            let damp = if wet > 0.62 { "humid" } else if wet < 0.38 { "dry" } else { "fair" };
            let over = if sea > 0.5 { ", over the sea" } else { "" };
            let level = self.sim.freezing_level(px, pz);
            lines.push(format!("Air {air} and {damp}{over}; snow above about y {level:.0}"));
        }
        if self.aurora_tonight() > 0.05 {
            lines.push("Northern lights tonight, low in the north once it is dark".into());
        }
        let (fog, lift) = self.fog;
        if fog > 0.01 {
            lines.push(format!("Valley fog {:.0}%{}", fog * 100.0, if lift > 0.01 { ", lifting" } else { "" }));
        }
        lines.join("\n")
    }

    /// A system of `kind`, `dist` metres upwind (so the wind brings it over), or right overhead with dist 0.
    fn summon(&mut self, kind: Kind, dist: f64, form: Option<sim::StormForm>) -> Result<String, String> {
        let (px, _, pz) = self.player.ok_or("No player")?;
        let (wx, wz) = self.sim.wind_dir();
        let (x, z) = (px - wx as f64 * dist, pz - wz as f64 * dist);
        let phase = if dist <= 0.0 { 0.35 } else { 0.12 };
        self.sim.add(kind, x, z, phase);
        let ws = self.sim.wind().1.max(0.5) as f64;
        let mut name = kind.name();
        if let Some(s) = self.sim.systems.last_mut() {
            s.told = true;
            // a storm of the form asked for: another seed until it is
            if let Some(form) = form {
                let mut k = 0;
                while s.storm_form() != form && k < 256 {
                    s.seed = hash(s.seed.wrapping_add(0x9E37_79B9));
                    k += 1;
                }
            }
            name = s.label();
            // it lives long enough to reach you and pass over
            let need = (dist / ws) as f32 * 1.4 + 900.0;
            if s.life * (1.0 - phase) < need {
                s.life = need / (1.0 - phase);
                s.age = s.life * phase;
            }
        }
        self.raster();
        Ok(if dist > 0.0 {
            format!(
                "A {} is building {:.0} km to the {}; the wind brings it over in about {:.0} min",
                name,
                dist / 1000.0,
                compass(x - px, z - pz),
                (dist / ws / self.sim.rate().max(0.01) as f64 / 60.0).max(0.1)
            )
        } else {
            format!("A {name} right overhead")
        })
    }
}

impl Mod for Skies {
    fn init() -> Result<(Self, Registration), String> {
        let mut m = Skies {
            sim: Sim::new(1),
            words: vec![0; DATA],
            origin: (0.0, 0.0),
            raster_at: 0.0,
            pending: None,
            ticks: 0,
            since_tick: 0.0,
            jitter: 0,
            on: true,
            pass_live: false,
            started: false,
            vanilla: None,
            sync: true,
            player: None,
            day_time: 0,
            fog: (0.0, 0.0),
            fog_forced: None,
            aurora_forced: None,
            aurora: 0.0,
            quality: 1,
            rainbow: 0.0,
            snowy: 0.0,
            climate_asked: None,
            climate_at: 0,
            falls: falls::Falls::default(),
            cover_asked: None,
            cover_at: None,
            lit: None,
        };
        let vol = noise::volume(0x5C1E_5C1E);
        m.words[NOISE_AT..FALLS_AT].copy_from_slice(&vol);
        let loaded = storage::get("state").and_then(|b| m.load(&b));
        if loaded.is_none() {
            let seed = game::get_clock().map(|c| c.game_time as u32).unwrap_or(0) ^ 0x5EED_5C1E;
            m.sim = Sim::new(hash(seed));
        } else {
            m.started = true;
        }
        // what the chat suggests after /skies
        let usages = [
            "status",
            "storm [cell|cluster|line|pulse] [<km>|here]",
            "shower|front|overcast|fair [<km>|here]",
            "clear",
            "weather auto|clear|fair|showery|thundery|overcast|rainy",
            "wind auto|north|east|south|west [<m/s>]",
            "speed <x>",
            "dawn",
            "fog",
            "aurora",
            "rainbow",
            "falls",
            "bolt [<m>]",
            "sync on|off",
            "quality low|medium|high",
            "on",
            "off",
            "reset",
        ];
        commands::usage("skies", &usages.map(String::from));
        Ok((m, Registration::new().command("skies").on_tick().on_frame()))
    }

    fn tick(&mut self, _tick: u64) {
        self.ticks += 1;
        self.since_tick = 0.0;
        let p = game::get_player();
        let overworld = p.as_ref().is_some_and(|p| p.dimension == "minecraft:overworld");
        self.player = p.map(|p| (p.x, p.y, p.z));
        if let Some(c) = game::get_clock() {
            self.day_time = c.day_time;
        }
        self.sim.day = self.day_time as f64 / 24000.0;
        self.sim.convection = self.convection();
        let Some((px, _, pz)) = self.player else { return };
        if !self.on || !overworld {
            self.stop_pass();
            return;
        }
        if !self.started {
            // A first run: seeded now, from the frame times of the title and loading screens, the world's clock and
            // the player's place (when the mod loads there is no world yet, so every install would otherwise share
            // one sequence of days).
            self.started = true;
            let clock = game::get_clock().map_or(0, |c| c.game_time);
            let place = clock ^ px.to_bits() ^ pz.to_bits().rotate_left(29);
            let seed = hash(self.jitter) ^ place as u32 ^ (place >> 32) as u32;
            self.sim = Sim::new(hash(seed ^ 0x5EED_5C1E));
            self.sim.day = self.day_time as f64 / 24000.0;
            self.sim.convection = self.convection();
            self.sim.populate(px, pz);
        } else if self.ticks % 100 == 0 && self.sim.empty_around(px, pz) {
            // another world (the state is kept per install), or far from where the sky was
            self.sim.climate = sim::Climate::default();
            self.climate_at = 0;
            self.sim.populate(px, pz);
            self.raster();
        }
        self.fog = self.fog_now();
        self.rainbow = (self.rainbow - 0.05).max(0.0);
        self.sim.step(0.05, px, pz);
        if self.pending.is_none() && (self.ticks % RASTER_EVERY == 0 || !self.pass_live) {
            self.raster();
        }
        self.raster_step();
        if self.pass_live {
            self.push_lighting();
        }
        self.thunder();
        // (the answers to every column query come back together)
        let answers = world::poll_columns();
        self.look_for_snow(&answers);
        self.look_for_cover(&answers);
        if let Some(p) = self.player {
            if self.falls.step(&answers, p, self.ticks) {
                self.falls.write(&mut self.words[FALLS_AT..FALLS_AT + falls::WORDS]);
            }
            let cmds = self.falls.particles(p, self.ticks);
            if !cmds.is_empty() {
                world::run_commands(&cmds);
            }
        }
        self.ask_climate();
        if self.sync && self.ticks % 20 == 5 {
            self.sync_vanilla();
        }
        if self.ticks % 100 == 50 {
            self.tell();
        }
        if self.ticks % 1200 == 0 {
            self.save();
        }
    }

    fn frame(&mut self, seconds: f32) {
        self.since_tick += seconds;
        if !self.started {
            self.jitter = hash(self.jitter ^ seconds.to_bits());
        }
        if self.pass_live {
            self.push_params();
        }
    }

    fn command(&mut self, _name: &str, args: &str) -> Result<Option<String>, String> {
        let mut it = args.split_whitespace();
        let sub = it.next().unwrap_or("status");
        // `parse` takes "nan" and "inf", which would poison the saved weather
        let num = |s: Option<&str>| s.and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite());
        let r: Result<String, String> = match sub {
            "status" | "forecast" => Ok(self.status()),
            "storm" | "shower" | "front" | "fair" | "overcast" | "deck" => {
                let kind = match sub {
                    "storm" => Kind::Storm,
                    "shower" => Kind::Shower,
                    "front" => Kind::Front,
                    "overcast" | "deck" => Kind::Deck,
                    _ => Kind::Fair,
                };
                let mut arg = it.next();
                // a storm's form: one great cell, a cluster, a squall line, or a lone pulse storm
                let form = match arg {
                    Some("cell") => Some(sim::StormForm::Cell),
                    Some("cluster") => Some(sim::StormForm::Cluster),
                    Some("line") | Some("squall") => Some(sim::StormForm::Line),
                    Some("pulse") => Some(sim::StormForm::Pulse),
                    _ => None,
                };
                if form.is_some() {
                    arg = it.next();
                }
                // distances in kilometres
                let km = if arg == Some("here") {
                    0.0
                } else {
                    num(arg).unwrap_or(match kind {
                        Kind::Front | Kind::Deck => 30.0,
                        Kind::Storm => 16.0,
                        _ => 8.0,
                    })
                };
                self.summon(kind, km.clamp(0.0, 45.0) * 1000.0, form.filter(|_| kind == Kind::Storm))
            }
            "clear" => {
                self.sim.systems.retain(|s| s.kind == Kind::Fair);
                self.sim.regime_fix = Some(Regime::Clear);
                self.raster();
                Ok("Cleared the sky; the weather stays clear (/skies weather auto to let the days run on)".into())
            }
            "weather" | "mood" | "day" => match it.next() {
                Some("auto") | None => {
                    self.sim.regime_fix = None;
                    let (a, b, _) = self.sim.regimes();
                    Ok(format!("The days run their course: today {}, tomorrow {}", a.name(), b.name()))
                }
                Some(v) => {
                    let r = match v {
                        "calm" | "settled" => Some(Regime::Clear),
                        "mixed" => Some(Regime::Showery),
                        "stormy" | "wild" => Some(Regime::Thundery),
                        v => Regime::parse(v),
                    }
                    .ok_or("/skies weather auto|clear|fair|showery|thundery|overcast|rainy")?;
                    self.sim.regime_fix = Some(r);
                    Ok(format!("The weather holds {} (systems come and go to match)", r.name()))
                }
            },
            "wind" => match (it.next(), it.next()) {
                (Some("auto"), _) | (None, _) => {
                    self.sim.wind_fix = None;
                    Ok("The wind follows the weather".into())
                }
                (Some(from), speed) => {
                    let deg = match from.to_ascii_lowercase().as_str() {
                        "e" | "east" => 0.0,
                        "s" | "south" => 90.0,
                        "w" | "west" => 180.0,
                        "n" | "north" => 270.0,
                        v => v
                            .parse::<f32>()
                            .ok()
                            .filter(|v| v.is_finite())
                            .ok_or_else(|| "/skies wind auto | <from: n|e|s|w|degrees> [speed m/s]".to_string())?,
                    };
                    // "from the west" blows towards the east
                    let toward = (deg + 180.0).to_radians();
                    let speed = speed.and_then(|s| s.parse::<f32>().ok()).filter(|v| v.is_finite()).unwrap_or(8.0).clamp(0.0, 40.0);
                    self.sim.wind_fix = Some((toward, speed));
                    Ok(format!("Wind held from {from} at {speed:.0} m/s"))
                }
            },
            "speed" | "timelapse" => {
                let v = num(it.next()).unwrap_or(1.0).clamp(0.0, 60.0) as f32;
                self.sim.speed = v;
                Ok(format!("Weather runs at {v}x"))
            }
            "dawn" => {
                // the small hours of a foggy morning, to watch the fog lift
                let target = self.day_time / 24000 * 24000 + 22600;
                self.fog_forced = Some((target + 12000) / 24000);
                world::run_commands(&[format!("time set {target}")]);
                Ok("Set the clock to just before dawn on a foggy morning; watch the valleys from high ground".into())
            }
            "aurora" => {
                // tonight's, or by day the coming night's (a night's number runs from its dusk to the next dusk)
                let night = (self.day_time + 12000) / 24000 + u64::from(self.day_time % 24000 < 12000);
                self.aurora_forced = Some(night);
                Ok("The northern lights tonight, low in the north once it is dark (clouds hide them)".into())
            }
            "fog" => {
                let morning = (self.day_time + 12000) / 24000;
                self.fog_forced = Some(morning);
                Ok("The coming morning will be foggy".into())
            }
            "falls" | "waterfalls" => {
                let (px, _, pz) = self.player.ok_or("No player")?;
                let near = &self.falls.near;
                if near.is_empty() {
                    Ok(format!("No waterfalls within about 140 blocks (water falling {} or more into a pool)", falls::MIN_DROP))
                } else {
                    let lines: Vec<String> = near
                        .iter()
                        .map(|f| {
                            let (dx, dz) = (f.x as f64 - px, f.z as f64 - pz);
                            format!(
                                "a {:.0}-block fall, {:.0} wide, into the pool at {:.0} {:.0} {:.0} ({:.0} blocks {})",
                                f.drop, f.width, f.x.floor(), f.pool, f.z.floor(), (dx * dx + dz * dz).sqrt(), compass(dx, dz)
                            )
                        })
                        .collect();
                    Ok(lines.join("
"))
                }
            }
            "rainbow" => {
                self.rainbow = RAINBOW_FOR;
                Ok("A rainbow, if the sun is out and low enough behind you".into())
            }
            "bolt" => {
                // a strike where you look, `dist` metres away (default 2000), with its thunder
                let p = game::get_player().ok_or("No player")?;
                let dist = num(it.next()).unwrap_or(2000.0).clamp(20.0, 30_000.0);
                let yaw = (p.yaw as f64).to_radians();
                let (x, z) = (p.x - yaw.sin() * dist, p.z + yaw.cos() * dist);
                let seed = hash(self.ticks as u32 ^ self.sim.seed);
                self.sim.strike = Some(sim::Strike { x, z, at: self.sim.real, seed });
                self.sim.thunder.push(sim::Thunder { due: self.sim.real + dist / 343.0, x, z });
                self.push_params();
                Ok(String::new())
            }
            "sync" => {
                self.sync = !matches!(it.next(), Some("off"));
                self.vanilla = None;
                Ok(format!("Vanilla weather {} the sky overhead", if self.sync { "follows" } else { "no longer follows" }))
            }
            "quality" => {
                self.quality = match it.next() {
                    Some("low") => 0,
                    Some("high") => 2,
                    _ => 1,
                };
                Ok(format!("Sky quality: {}", ["low", "medium", "high"][self.quality as usize]))
            }
            "off" => {
                self.on = false;
                self.stop_pass();
                Ok("Big Skies paused (vanilla weather is left as it is)".into())
            }
            "on" => {
                self.on = true;
                Ok("Big Skies running".into())
            }
            "reset" => {
                let (px, _, pz) = self.player.ok_or("No player")?;
                let seed = self.sim.seed;
                let (day, time, real, drift) = (self.sim.day, self.sim.time, self.sim.real, self.sim.drift);
                self.sim = Sim::new(hash(seed ^ self.ticks as u32));
                (self.sim.day, self.sim.time, self.sim.real, self.sim.drift) = (day, time, real, drift);
                self.sim.convection = self.convection();
                self.sim.populate(px, pz);
                self.raster();
                Ok("A fresh sky".into())
            }
            _ => Err("/skies status | storm [cell|cluster|line|pulse] [km|here] | shower | front | overcast | fair | clear | weather auto|clear|fair|showery|thundery|overcast|rainy | wind auto|<from> [m/s] | speed <x> | dawn | fog | aurora | rainbow | falls | bolt [m] | sync on|off | quality low|medium|high | on | off | reset".into()),
        };
        if matches!(sub, "weather" | "mood" | "day" | "wind" | "speed" | "timelapse" | "quality" | "sync" | "clear") {
            self.save();
        }
        r.map(|m| if m.is_empty() { None } else { Some(m) })
    }
}

#[allow(dead_code)]
// (the shader's TAIL_AT, NOISE_AT and FALLS_AT are WORDS, NOISE_AT and FALLS_AT here)
const _: () = assert!(G * G * 4 == WORDS && WORDS == 65_536 && NOISE_AT == 65_552 && FALLS_AT == 98_320 && COVER_AT == 98_388 && DATA <= 262_144);

horizon_mod_sdk::export_mod!(Skies);
