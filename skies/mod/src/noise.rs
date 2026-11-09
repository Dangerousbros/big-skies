//! Small deterministic randomness: a seeded generator, integer hashes and smooth value noise.

pub struct Rng(pub u32);

impl Rng {
    pub fn next(&mut self) -> u32 {
        // xorshift32
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// 0..1
    pub fn f(&mut self) -> f32 {
        (self.next() >> 8) as f32 / 16_777_216.0
    }
    pub fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
}

pub fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

fn hash2(i: i32, j: i32, seed: u32) -> f32 {
    let h = hash((i as u32).wrapping_mul(0x9E37_79B1) ^ hash((j as u32) ^ seed.wrapping_mul(0x85EB_CA77)));
    (h >> 8) as f32 / 16_777_216.0
}

fn fade(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Value noise in 0..1 with unit cells.
pub fn vnoise2(x: f64, z: f64, seed: u32) -> f32 {
    let (fx, fz) = (x.floor(), z.floor());
    let (i, j) = (fx as i32, fz as i32);
    let (tx, tz) = (fade((x - fx) as f32), fade((z - fz) as f32));
    let a = hash2(i, j, seed);
    let b = hash2(i + 1, j, seed);
    let c = hash2(i, j + 1, seed);
    let d = hash2(i + 1, j + 1, seed);
    let ab = a + (b - a) * tx;
    let cd = c + (d - c) * tx;
    ab + (cd - ab) * tz
}

pub fn vnoise1(t: f64, seed: u32) -> f32 {
    vnoise2(t, 0.5, seed)
}

pub fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The side of the cloud-noise volume, in voxels.
pub const VOL: usize = 32;

fn hash3(i: i32, j: i32, k: i32, seed: u32) -> u32 {
    hash((i as u32).wrapping_mul(0x9E37_79B1) ^ hash((j as u32).wrapping_mul(0x85EB_CA77) ^ hash((k as u32) ^ seed)))
}

fn unit(h: u32) -> f32 {
    (h >> 8) as f32 / 16_777_216.0
}

/// Gradient noise, about -1..1, repeating every `period` units.
fn perlin(p: [f32; 3], period: i32, seed: u32) -> f32 {
    const GRAD: [[f32; 3]; 12] = [
        [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0], [1.0, -1.0, 0.0], [-1.0, -1.0, 0.0],
        [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0], [1.0, 0.0, -1.0], [-1.0, 0.0, -1.0],
        [0.0, 1.0, 1.0], [0.0, -1.0, 1.0], [0.0, 1.0, -1.0], [0.0, -1.0, -1.0],
    ];
    let i = p.map(|v| v.floor() as i32);
    let f = [p[0] - i[0] as f32, p[1] - i[1] as f32, p[2] - i[2] as f32];
    let quintic = |t: f32| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    let u = f.map(quintic);
    let corner = |dx: i32, dy: i32, dz: i32| {
        let w = |a: i32| a.rem_euclid(period);
        let g = GRAD[(hash3(w(i[0] + dx), w(i[1] + dy), w(i[2] + dz), seed) % 12) as usize];
        g[0] * (f[0] - dx as f32) + g[1] * (f[1] - dy as f32) + g[2] * (f[2] - dz as f32)
    };
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), u[0]);
    let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), u[0]);
    let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), u[0]);
    let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), u[0]);
    lerp(lerp(x00, x10, u[1]), lerp(x01, x11, u[1]), u[2])
}

/// Cellular noise, inverted (1 at the feature points, falling to 0 between them), `cells` cells to the volume.
fn worley(v: [f32; 3], cells: i32, seed: u32) -> f32 {
    let p = v.map(|c| c / VOL as f32 * cells as f32);
    let c = p.map(|x| x.floor() as i32);
    let mut best = 9.0f32;
    for dz in -1..=1 {
        for dy in -1..=1 {
            for dx in -1..=1 {
                let n = [c[0] + dx, c[1] + dy, c[2] + dz];
                let w = n.map(|a| a.rem_euclid(cells));
                let h = hash3(w[0], w[1], w[2], seed);
                let fp = [
                    n[0] as f32 + unit(h),
                    n[1] as f32 + unit(hash(h ^ 0x68e3_1da4)),
                    n[2] as f32 + unit(hash(h ^ 0xb529_7a4d)),
                ];
                let d = (fp[0] - p[0]).powi(2) + (fp[1] - p[1]).powi(2) + (fp[2] - p[2]).powi(2);
                best = best.min(d);
            }
        }
    }
    (1.0 - best.sqrt()).clamp(0.0, 1.0)
}

/// The clouds' noise: a volume of VOL^3 voxels that repeats seamlessly in every direction, four channels a voxel
/// packed into a word: Perlin-Worley (the shapes of cloud masses), and Worley at three frequencies (the billows
/// that erode them). After the cloud textures of Schneider's "The real-time volumetric cloudscapes of Horizon: Zero
/// Dawn" (SIGGRAPH 2015), worked out here.
pub fn volume(seed: u32) -> Vec<u32> {
    let n = VOL;
    let mut ch = vec![[0f32; 4]; n * n * n];
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                let v = [x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5];
                let pp = |period: i32| v.map(|c| c / n as f32 * period as f32);
                let fbm = perlin(pp(4), 4, seed) * 0.6 + perlin(pp(8), 8, seed ^ 1) * 0.28 + perlin(pp(16), 16, seed ^ 2) * 0.12;
                let perl = (fbm * 0.9 + 0.5).clamp(0.0, 1.0);
                let w4 = worley(v, 4, seed ^ 11);
                let w8 = worley(v, 8, seed ^ 12);
                let w16 = worley(v, 16, seed ^ 13);
                let wf = w4 * 0.625 + w8 * 0.25 + w16 * 0.125;
                // Perlin, pinched in to the Worley cells, so the masses come out billowy rather than smeared
                let pw = ((perl - (1.0 - wf)) / wf.max(0.05) * 0.5 + perl * 0.5).clamp(0.0, 1.0);
                ch[(z * n + y) * n + x] = [pw, wf, w8 * 0.75 + w16 * 0.25, w16];
            }
        }
    }
    // stretch each channel over the whole byte
    let mut lo = [f32::MAX; 4];
    let mut hi = [f32::MIN; 4];
    for c in &ch {
        for k in 0..4 {
            lo[k] = lo[k].min(c[k]);
            hi[k] = hi[k].max(c[k]);
        }
    }
    ch.iter()
        .map(|c| {
            (0..4).fold(0u32, |w, k| {
                let t = (c[k] - lo[k]) / (hi[k] - lo[k]).max(1e-6);
                w | (((t * 255.0 + 0.5) as u32).min(255) << (8 * k))
            })
        })
        .collect()
}
