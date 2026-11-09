// Big Skies: the weather over the world and Horizon's distant terrain, drawn at the real world's scale (a block
// is a metre): a physically based sky, volumetric clouds from fair-weather cumulus to ten-kilometre thunderstorms,
// distant rain shafts, cloud shadows, rainbows, lightning, valley fog, the sun, the moon and the stars, and the mist
// and spray rainbows of waterfalls.
//
// mod_data, from the mod (src/sim.rs):
//   a grid of 128 x 128 cells, 512 m each, centred on the player, three words a cell: [cloud cover, high cloud cover,
//   rain, storm] as bytes; [cloud base, cloud top] in metres, 16 bits each; [valley fog, layered (0 heaped cumulus
//   .. 1 stratus), high cloud height / 16 km, humidity] as bytes;
//   then sixteen floats: the sky past the grid's edge (cover, base, top, layered, rain, high cover, high cloud height,
//   humidity) and the tallest cloud top in the grid;
//   then a 32^3 cloud-noise volume that repeats in every direction (Perlin-Worley, and Worley at three frequencies);
//   then the waterfalls round the player (src/falls.rs): a count, three spare words, and for each of up to eight, eight
//   floats: the plunge point x, the pool's surface y, z; the drop; the width in columns; the mist's strength; a seed.
// mod_params:
//   [0] grid corner x, z; how far the wind has carried the air since the grid was drawn, x, z
//   [1] the wind's total drift x, z (for the clouds' noise); weather seconds (repeating every 20000); the moon's phase
//       (whole part, 0 full) plus the valley fog amount x 0.999
//   [2] the latest lightning strike x, z, its age in seconds, its seed
//   [3] valley fog lift (0..1); wind speed; quality 0..2 plus the lingering rainbow x 0.99; wind angle
//
// The costly part (sky, clouds, rain, fog, shadows) is drawn at half size into two pictures: the light laid over the
// world (already tone-mapped) with how much of the sky's lights still shows through, and the depth with how much of
// the world shows through (a colour, for the air reddens what it does not hide). The join brings them to full size
// against the world's edges and adds the sun, the moon, the stars and lightning bolts.
//
// The atmosphere follows Hillaire's "A Scalable and Production Ready Sky and Atmosphere Rendering Technique" (2020)
// for its constants, and Schuler's approximate Chapman function ("An Approximation to the Chapman Grazing-Incidence
// Function for Atmospheric Scattering", GPU Pro 3) for the light reaching each point; the clouds follow Schneider and
// Vos, "The real-time volumetric cloudscapes of Horizon: Zero Dawn" (SIGGRAPH 2015), with Wrenninge's multiple
// scattering approximation ("Oz: The Great and Volumetric", 2013). All written afresh for this pass.

@group(0) @binding(6) var half_picture: texture_2d<f32>;
@group(0) @binding(7) var half_extra: texture_2d<f32>;

const PI: f32 = 3.14159265;
const G: i32 = 128;
const CELL: f32 = 512.0;
// (four words a cell)
const TAIL_AT: u32 = 65536u;
const NOISE_AT: u32 = 65552u;
const FALLS_AT: u32 = 98320u;
const MAX_FALLS: u32 = 8u;
const VOL: i32 = 32;
const SKY_LD: f32 = 1.0e7;

// the planet and its air, in metres
const R_PLANET: f32 = 6360000.0;
const R_ATMO: f32 = 6460000.0;
const BETA_R: vec3<f32> = vec3<f32>(5.802e-6, 13.558e-6, 33.1e-6);
const BETA_M: f32 = 3.996e-6;
const BETA_O: vec3<f32> = vec3<f32>(0.650e-6, 1.881e-6, 0.085e-6);
// the heights over which Rayleigh and Mie scatterers halve
const H50_R: f32 = 5545.0;
const H50_M: f32 = 832.0;
// the full moon's light, as a share of the sun's (far brighter than the real moon's: the eye adapts at night)
const MOON_E: f32 = 0.012;
// the eye adapts to a darker sky, up to so many times, towards this brightness of the sky overhead
const ADAPT_MAX: f32 = 7.0;
const ZENITH_REF: f32 = 0.008;
const EXPOSURE: f32 = 9.0;
// the extinction of the thickest cloud, per metre
const SIGMA: f32 = 0.045;
// the sun's and the moon's angular radii: a little larger than the real ones, so they read on screen
const SUN_R: f32 = 0.0095;
const MOON_R: f32 = 0.013;

fn P(i: u32) -> vec4<f32> {
    return frame.mod_params[i];
}
// Towards the sun, in the world. Horizon (as of this writing) turns the sun about the X axis, (0, cos, -sin), where
// Minecraft turns its sky a quarter round first so the sun rises in the east and sets in the west: (-sin, cos, 0).
// Either way, this gives the sun where the game draws it.
fn sun_vec() -> vec3<f32> {
    let s = frame.sun.xyz;
    if (abs(s.x) < 1.0e-6) {
        return normalize(vec3<f32>(s.z, s.y, s.x) + vec3<f32>(0.0, 1.0e-6, 0.0));
    }
    return normalize(s);
}
fn drift3() -> vec3<f32> {
    let d = P(1u).xy;
    return vec3<f32>(d.x, 0.0, d.y);
}
fn wtime() -> f32 {
    return P(1u).z;
}
fn fog_amount() -> f32 {
    return min(fract(max(P(1u).w, 0.0)) / 0.999, 1.0);
}
fn moon_phase() -> f32 {
    return floor(max(P(1u).w, 0.0));
}
// The moon's light as a share of the sun's: brightest full, a little left at new moon.
fn moon_e() -> f32 {
    return MOON_E * (0.12 + 0.88 * (0.5 + 0.5 * cos(moon_phase() * PI / 4.0)));
}
fn quality() -> u32 {
    return min(u32(max(P(3u).z, 0.0)), 2u);
}
fn rainbow_amount() -> f32 {
    return min(fract(max(P(3u).z, 0.0)) / 0.99, 1.0);
}
fn wind_vec() -> vec2<f32> {
    let a = P(3u).w;
    return vec2<f32>(cos(a), sin(a)) * P(3u).y;
}

fn hash_u(x: u32) -> u32 {
    var h = x;
    h ^= h >> 16u;
    h *= 0x7feb352du;
    h ^= h >> 15u;
    h *= 0x846ca68bu;
    h ^= h >> 16u;
    return h;
}
fn hash3i(x: i32, y: i32, z: i32) -> f32 {
    let h = hash_u(u32(x) * 0x9E3779B1u ^ hash_u(u32(y) * 0x85EBCA77u ^ hash_u(u32(z))));
    return f32(h >> 8u) / 16777216.0;
}
fn ign(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715))));
}
fn remap(v: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    return c + (v - a) / (b - a) * (d - c);
}
// Henyey-Greenstein, normalised over the sphere
fn hg(mu: f32, g: f32) -> f32 {
    let g2 = g * g;
    return (1.0 - g2) / (4.0 * PI * pow(max(1.0 + g2 - 2.0 * g * mu, 1.0e-4), 1.5));
}
fn unpack4(w: u32) -> vec4<f32> {
    return vec4<f32>(f32(w & 255u), f32((w >> 8u) & 255u), f32((w >> 16u) & 255u), f32(w >> 24u));
}
fn aces(x: vec3<f32>) -> vec3<f32> {
    return clamp(x * (2.51 * x + 0.03) / (x * (2.43 * x + 0.59) + 0.14), vec3<f32>(0.0), vec3<f32>(1.0));
}

// ---- the noise volume ----

fn vox(i: vec3<i32>) -> vec4<f32> {
    let w = i & vec3<i32>(VOL - 1);
    let k = min(NOISE_AT + u32((w.z * VOL + w.y) * VOL + w.x), arrayLength(&mod_data) - 1u);
    return unpack4(mod_data[k]) / 255.0;
}
// The volume at `p`, in voxels, smoothly between them; it repeats every 32.
fn vol(p: vec3<f32>) -> vec4<f32> {
    let q = p - 0.5;
    let fl = floor(q);
    let f = q - fl;
    let i = vec3<i32>(fl);
    let a = mix(vox(i), vox(i + vec3<i32>(1, 0, 0)), f.x);
    let b = mix(vox(i + vec3<i32>(0, 1, 0)), vox(i + vec3<i32>(1, 1, 0)), f.x);
    let c = mix(vox(i + vec3<i32>(0, 0, 1)), vox(i + vec3<i32>(1, 0, 1)), f.x);
    let d = mix(vox(i + vec3<i32>(0, 1, 1)), vox(i + vec3<i32>(1, 1, 1)), f.x);
    return mix(mix(a, b, f.y), mix(c, d, f.y), f.z);
}

// ---- the grid ----

struct Cell {
    cov: f32,
    high: f32,
    precip: f32,
    storm: f32,
    base: f32,
    top: f32,
    fog: f32,
    layered: f32,
    high_y: f32,
    hum: f32,
    // the freezing level (y): above it what falls is snow
    freeze: f32,
}
struct Raw {
    a: vec4<f32>,
    b: vec4<f32>,
    c: vec4<f32>,
}
fn tailf(k: u32) -> f32 {
    let i = TAIL_AT + k;
    if (i >= arrayLength(&mod_data)) {
        return 0.0;
    }
    return bitcast<f32>(mod_data[i]);
}
// The sky past the grid's edge: how its outer ring was, on average.
fn far_raw() -> Raw {
    return Raw(vec4<f32>(tailf(0u), tailf(5u), tailf(4u), 0.0), vec4<f32>(tailf(1u), tailf(2u), tailf(12u), 0.0),
        vec4<f32>(0.0, tailf(3u), tailf(6u), tailf(7u)));
}
fn raw(i: i32, j: i32) -> Raw {
    if (i < 0 || j < 0 || i >= G || j >= G) {
        return far_raw();
    }
    let k = u32(j * G + i) * 4u;
    if (k + 3u >= arrayLength(&mod_data)) {
        return far_raw();
    }
    let w1 = mod_data[k + 1u];
    let c = unpack4(mod_data[k + 2u]) / 255.0;
    let freeze = f32(mod_data[k + 3u] & 0xffffu) - 64.0;
    return Raw(unpack4(mod_data[k]) / 255.0, vec4<f32>(f32(w1 & 0xffffu), f32(w1 >> 16u), freeze, 0.0),
        vec4<f32>(c.x, c.y, c.z * 16000.0, c.w));
}
// The weather over a point of the world, now: the grid was drawn a moment ago, and the air has moved on since.
fn grid(xz: vec2<f32>) -> Cell {
    let g = (xz - P(0u).zw - P(0u).xy) / CELL - 0.5;
    let fl = floor(g);
    let f = g - fl;
    let i = vec2<i32>(fl);
    let r00 = raw(i.x, i.y);
    let r10 = raw(i.x + 1, i.y);
    let r01 = raw(i.x, i.y + 1);
    let r11 = raw(i.x + 1, i.y + 1);
    var a = mix(mix(r00.a, r10.a, f.x), mix(r01.a, r11.a, f.x), f.y);
    var b = mix(mix(r00.b, r10.b, f.x), mix(r01.b, r11.b, f.x), f.y);
    var c = mix(mix(r00.c, r10.c, f.x), mix(r01.c, r11.c, f.x), f.y);
    // towards the edge, the sky blends into how it carries on to the horizon
    let e = max(abs(g.x - 63.5), abs(g.y - 63.5)) / 64.0;
    let fade = smoothstep(0.9, 1.0, e);
    if (fade > 0.0) {
        let r = far_raw();
        a = mix(a, r.a, fade);
        b = mix(b, r.b, fade);
        c = mix(c, r.c, fade);
    }
    return Cell(a.x, a.y, a.z, a.w, b.x, b.y, c.x, c.y, c.z, c.w, b.z);
}

// ---- the atmosphere ----

fn ray_sphere(o: vec3<f32>, d: vec3<f32>, r: f32) -> vec2<f32> {
    let l = length(o);
    let b = dot(o, d);
    let c = (l - r) * (l + r);
    let h = b * b - c;
    if (h < 0.0) {
        return vec2<f32>(-1.0, -1.0);
    }
    let s = sqrt(h);
    return vec2<f32>(-b - s, -b + s);
}
// Schuler's approximate Chapman function: the air along a ray from height `h` (in halving heights over a planet of
// radius `x`) at the cosine `mu` from straight up, as a multiple of the air in one halving height there.
fn chapman(x: f32, h: f32, mu: f32) -> f32 {
    let c = sqrt(x + h);
    if (mu >= 0.0) {
        return c / (c * mu + 1.0) * exp2(-h);
    }
    let x0 = sqrt(max(1.0 - mu * mu, 0.0)) * (x + h);
    let c0 = sqrt(x0);
    return max(2.0 * c0 * exp2(min(x - x0, 60.0)) - c / (1.0 - c * mu) * exp2(-h), 0.0);
}
// What reaches `o` (centred on the planet) of the light from `l`: through the air, and not behind the planet.
fn light_trans(o: vec3<f32>, l: vec3<f32>, haze: f32) -> vec3<f32> {
    let r = length(o);
    let h = max(r - R_PLANET, 0.0);
    let mu = dot(o, l) / r;
    var shade = 1.0;
    if (mu < 0.0) {
        // the light grazes the planet at this height: its shadow, softened
        let graze = r * sqrt(max(1.0 - mu * mu, 0.0)) - R_PLANET;
        shade = smoothstep(-2500.0, 1500.0, graze);
        if (shade <= 0.0) {
            return vec3<f32>(0.0);
        }
    }
    let cr = chapman(R_PLANET / H50_R, h / H50_R, mu);
    let cm = chapman(R_PLANET / H50_M, h / H50_M, mu);
    // the ozone layer, as a column thirty kilometres up seen along the same slant as the air's
    let slant = cr / max(chapman(R_PLANET / H50_R, h / H50_R, 1.0), 1.0e-6);
    let od = BETA_R * H50_R * cr + vec3<f32>(BETA_M * 1.11 * haze * H50_M * cm) + BETA_O * 15000.0 * slant;
    return exp(-od) * shade;
}

struct Air {
    // light scattered towards the eye (from the eye out to a, to b, and all the way), and what gets through
    sa: vec3<f32>,
    ta: vec3<f32>,
    sb: vec3<f32>,
    tb: vec3<f32>,
    s: vec3<f32>,
    t: vec3<f32>,
    // where the ray meets the ground, or -1
    ground: f32,
}
fn mie_phase(mu: f32, g: f32) -> f32 {
    // Cornette-Shanks
    let g2 = g * g;
    return 3.0 / (8.0 * PI) * (1.0 - g2) * (1.0 + mu * mu) / ((2.0 + g2) * pow(max(1.0 + g2 - 2.0 * g * mu, 1.0e-4), 1.5));
}
// The air along the ray from height `alt` in the direction `rd`, `n` steps: single scattering of the sun's and the
// moon's light by molecules and haze, with a little more for the light scattered many times over. Under the cloud
// overhead (`under`: what gets through of the sunlight, and the cloud base) the air sees little of the sun, out to
// some kilometres away.
fn atmosphere(alt: f32, rd: vec3<f32>, ta: f32, tb: f32, haze: f32, n: i32, under: vec2<f32>) -> Air {
    let o = vec3<f32>(0.0, R_PLANET + max(alt, 1.0), 0.0);
    let sun = sun_vec();
    let top = ray_sphere(o, rd, R_ATMO).y;
    let gr = ray_sphere(o, rd, R_PLANET);
    var a: Air;
    a.ground = select(-1.0, gr.x, gr.x > 0.0);
    let tend = select(top, gr.x, gr.x > 0.0);
    let mu_s = dot(rd, sun);
    let ph_rs = 3.0 / (16.0 * PI) * (1.0 + mu_s * mu_s);
    let ph_ms = mie_phase(mu_s, 0.8);
    let ph_mm = mie_phase(-mu_s, 0.8);
    let sun_on = sun.y > -0.35;
    let moon_on = sun.y < 0.3;
    var s = vec3<f32>(0.0);
    var t = vec3<f32>(1.0);
    a.sa = s;
    a.ta = t;
    a.sb = s;
    a.tb = t;
    var got_a = ta <= 0.0;
    var got_b = tb <= 0.0;
    var t0 = 0.0;
    for (var i = 0; i < n; i += 1) {
        let u = f32(i + 1) / f32(n);
        let t1 = tend * u * u;
        let dt = t1 - t0;
        let tm = t0 + dt * 0.5;
        let p = o + rd * tm;
        let h = max(length(p) - R_PLANET, 0.0);
        let shaded = mix(under.x, 1.0, max(smoothstep(under.y - 150.0, under.y + 150.0, h),
            smoothstep(6000.0, 20000.0, tm * length(rd.xz))));
        let dr = exp2(-h / H50_R);
        let dm = exp2(-h / H50_M) * haze;
        let doz = max(0.0, 1.0 - abs(h - 25000.0) / 15000.0);
        let sr = BETA_R * dr;
        let sm = BETA_M * dm;
        let ext = sr + vec3<f32>(sm * 1.11) + BETA_O * doz;
        var l = vec3<f32>(0.0);
        if (sun_on) {
            let ts = light_trans(p, sun, haze) * shaded;
            l += ts * (sr * ph_rs + vec3<f32>(sm * ph_ms));
            // light scattered more than once, roughly: an even glow in proportion to the sunlight about
            l += (sr + vec3<f32>(sm)) * ts * 0.1 * smoothstep(-0.2, 0.2, sun.y + 0.1);
        }
        if (moon_on) {
            let tmo = light_trans(p, -sun, haze) * shaded;
            l += tmo * (sr * ph_rs + vec3<f32>(sm * ph_mm)) * moon_e();
        }
        let ex = exp(-ext * dt);
        let k = (vec3<f32>(1.0) - ex) / max(ext, vec3<f32>(1.0e-12));
        if (!got_a && ta < t1) {
            let e = exp(-ext * (ta - t0));
            a.sa = s + t * l * (vec3<f32>(1.0) - e) / max(ext, vec3<f32>(1.0e-12));
            a.ta = t * e;
            got_a = true;
        }
        if (!got_b && tb < t1) {
            let e = exp(-ext * (tb - t0));
            a.sb = s + t * l * (vec3<f32>(1.0) - e) / max(ext, vec3<f32>(1.0e-12));
            a.tb = t * e;
            got_b = true;
        }
        s += t * l * k;
        t *= ex;
        t0 = t1;
    }
    if (!got_a) {
        a.sa = s;
        a.ta = t;
    }
    if (!got_b) {
        a.sb = s;
        a.tb = t;
    }
    a.s = s;
    a.t = t;
    return a;
}

// ---- light ----

struct Lights {
    sun: vec3<f32>,
    // what lights the clouds: the sun, or the moon at night; its colour low (1.5 km) and high (9 km) in the sky and at
    // the ground
    dir: vec3<f32>,
    lo: vec3<f32>,
    hi: vec3<f32>,
    gnd_light: vec3<f32>,
    // the sky's light from above, and the ground's from below
    amb: vec3<f32>,
    bounce: vec3<f32>,
    haze: f32,
    night: f32,
    // how far the eye has opened to a dim sky
    adapt: f32,
    // what gets through of the sunlight, past the cloud over the eye
    sun_cam: f32,
}
fn lights(ro: vec3<f32>, here: Cell) -> Lights {
    var lt: Lights;
    let sun = sun_vec();
    lt.sun = sun;
    lt.haze = 1.0 + 2.5 * here.hum + 12.0 * here.precip;
    let by_sun = sun.y > -0.07;
    lt.dir = select(-sun, sun, by_sun);
    let e = select(moon_e(), 1.0, by_sun);
    lt.lo = e * light_trans(vec3<f32>(0.0, R_PLANET + 1500.0, 0.0), lt.dir, lt.haze);
    lt.hi = e * light_trans(vec3<f32>(0.0, R_PLANET + 9000.0, 0.0), lt.dir, lt.haze);
    lt.gnd_light = e * light_trans(vec3<f32>(0.0, R_PLANET + max(ro.y, 1.0), 0.0), lt.dir, lt.haze);
    // the sky overhead from the cloud layer, as the light falling on a cloud from all round above
    let zen = atmosphere(1500.0, vec3<f32>(0.0, 1.0, 0.0), 0.0, 0.0, lt.haze, 6, vec2<f32>(1.0, 0.0)).s;
    lt.amb = zen * 1.7 + vec3<f32>(0.0004, 0.0006, 0.0011);
    // the ground's light, as much of it as the clouds let through here
    lt.sun_cam = sun_through(ro, lt);
    lt.bounce = lt.gnd_light * max(lt.dir.y, 0.0) * (0.15 + 0.85 * lt.sun_cam) * vec3<f32>(0.10, 0.11, 0.08) / PI + lt.amb * 0.12;
    lt.night = 1.0 - smoothstep(-0.2, 0.02, sun.y);
    // through dusk and by night the eye opens up, part of the way: the night stays darker than the day
    lt.adapt = clamp(sqrt(ZENITH_REF / max(luminance(zen), 1.0e-7)), 1.0, ADAPT_MAX);
    return lt;
}
fn light_at(lt: Lights, alt: f32) -> vec3<f32> {
    return mix(lt.lo, lt.hi, smoothstep(1500.0, 9000.0, alt));
}

// Lightning: a few strokes in quick succession, then an afterglow.
fn strokes(age: f32) -> f32 {
    let r = fract(P(2u).w * 0.371);
    var f = step(age, 0.07);
    f += step(0.13, age) * step(age, 0.2) * 0.8;
    f += step(0.3 + r * 0.2, age) * step(age, 0.36 + r * 0.2) * 0.6;
    return f;
}
fn flicker(age: f32) -> f32 {
    if (age > 1.2 || age < 0.0) {
        return 0.0;
    }
    return strokes(age) + exp(-age * 5.0) * 0.4;
}
// The flash lighting up the cloud round a strike, two kilometres up in the storm.
fn flash_at(p: vec3<f32>) -> f32 {
    let s = P(2u);
    if (s.z > 1.2 || s.z < 0.0) {
        return 0.0;
    }
    let d = distance(p, vec3<f32>(s.x, 2500.0, s.y));
    return flicker(s.z) * 0.5 * exp(-d / 2200.0);
}

// ---- clouds ----

// The cloud at `air` (a point in the drifting air) at height `alt`, for the weather `c` over it: 0 clear .. 1 the
// densest. Heaped cumulus have flat bases and billowing heads; layered cloud is a slab with a soft, lumpy top.
fn density(c: Cell, air: vec3<f32>, alt: f32, detail: bool) -> f32 {
    if (c.cov < 0.004) {
        return 0.0;
    }
    let span = max(c.top - c.base, 60.0);
    let h = (alt - c.base) / span;
    if (h <= 0.0 || h >= 1.0) {
        return 0.0;
    }
    // (a thunderstorm's tower stays broad right up to its top, where it spreads into the anvil)
    let heap = smoothstep(0.0, 0.06, h) * (1.0 - smoothstep(mix(0.35, 0.9, c.storm), 1.0, h));
    let slab = smoothstep(0.0, 0.18, h) * (1.0 - smoothstep(0.6, 1.0, h));
    let prof = mix(heap, slab, c.layered);
    let t = wtime();
    // masses a kilometre or so across (the volume spans 4096 m), slowly boiling upwards; in a tall tower they are
    // drawn out upwards, so it holds together as one column
    let vs = 1.4 * clamp(2200.0 / span, 0.6, 1.0);
    let sh = vol(vec3<f32>(air.x, alt * vs + t * 0.6144, air.z) / 128.0);
    var base = clamp(remap(sh.x, sh.y - 1.0, 1.0, 0.0, 1.0), 0.0, 1.0);
    // towers lean back into rounded domes
    base = base * mix(1.0 - 0.35 * h, 1.0, max(c.layered, c.storm));
    // a sky a tenth covered still holds plenty of separate cumulus: the threshold falls fast at first
    // and a storm's head flares out under the anvil
    let flare = c.high * min(c.storm * 4.0, 1.0) * smoothstep(0.72, 0.92, h);
    let cov = clamp(c.cov * (1.0 + 0.25 * c.storm) + 0.3 * flare, 0.0, 1.0);
    let thr = 1.0 - 0.88 * sqrt(cov);
    var d = clamp(remap(base * mix(0.6 + 0.4 * prof, prof, c.layered), thr, 1.0, 0.0, 1.0), 0.0, 1.0) * prof;
    if (detail && d > 0.0) {
        // billows tens of metres across eat into the edges: wisps underneath, cauliflower heads above
        let dn = vol(vec3<f32>(air.x, alt + t * 1.536, air.z) / 16.0);
        let fbm = dn.y * 0.625 + dn.z * 0.25 + dn.w * 0.125;
        let det = mix(fbm, 1.0 - fbm, clamp(h * 4.0, 0.0, 1.0));
        d = clamp(remap(d, det * mix(0.38, 0.18, c.layered), 1.0, 0.0, 1.0), 0.0, 1.0);
    }
    // denser with height in a heaped cloud
    return d * mix(0.55 + 0.45 * smoothstep(0.0, 0.5, h), 0.85, c.layered);
}

// The sunlight (or moonlight) inside a cloud at `air`, `alt`, scattered towards the eye.
fn cloud_light(lt: Lights, mu: f32, c: Cell, air: vec3<f32>, alt: f32, h: f32, steps: i32, p: vec3<f32>) -> vec3<f32> {
    var tau = 0.0;
    var s = 0.0;
    var ds = 45.0;
    for (var k = 0; k < steps; k += 1) {
        let q = s + ds * 0.5;
        tau += density(c, air + lt.dir * q, alt + lt.dir.y * q, false) * ds;
        s += ds;
        ds *= 2.3;
    }
    tau *= SIGMA;
    // and the bulk of a deep cloud above, which a short march misses: a storm or a deck sits in its own shade. Not
    // where the march already came out into the open: the sunlit flank of a tower, or the top of a deck
    let deep = c.cov * c.cov * max(0.5, max(c.layered, c.storm));
    let inside = smoothstep(0.3, 3.0, tau);
    let above = max(c.top - (alt + lt.dir.y * s), 0.0) * deep;
    tau += above * SIGMA * 0.25 / max(lt.dir.y, 0.12) * inside;
    // multiple scattering, after Wrenninge: fainter, softer, more forward copies of the single scattering
    var ms = 0.0;
    var a = 1.0;
    var b = 1.0;
    var g = 1.0;
    for (var k = 0; k < 3; k += 1) {
        // forwards and back, and (light bounced about inside a thick cloud) some every way
        let ph = mix(hg(mu, 0.8 * g), hg(mu, -0.3 * g), 0.3) + 0.05;
        ms += a * ph * exp(-tau * b);
        a *= 0.5;
        b *= 0.45;
        g *= 0.5;
    }
    // light diffusing down through a thick layer from its sunlit top
    let diffuse = 0.06 / (1.0 + 0.02 * above * SIGMA * 40.0);
    let sunlit = light_at(lt, alt) * (ms + diffuse * deep);
    let ao = exp(-above * SIGMA * 0.005 * mix(0.3, 1.0, inside));
    // the sky lights the tops; the flat bases see only the dim ground
    let amb = lt.amb * (0.15 + 0.85 * h * h) * ao + lt.bounce * (1.0 - h);
    // a storm's base and its rain core go slate dark
    let dark = (1.0 - 0.7 * c.storm * (1.0 - h) * (1.0 - h)) * (1.0 - 0.25 * c.precip * (1.0 - h));
    return (sunlit + amb) * dark + vec3<f32>(0.75, 0.8, 1.0) * flash_at(p);
}

// Heights along the ray where the air is `a` metres up over the curved ground (near enough for a few hundred km):
// the ray is above `a` between the two.
fn crossings(roy: f32, rdy: f32, k: f32, a: f32) -> vec2<f32> {
    // k t^2 - rdy t + (a - roy) = 0
    let c = a - roy;
    let d = rdy * rdy - 4.0 * k * c;
    if (d < 0.0) {
        return vec2<f32>(1.0e9, -1.0e9);
    }
    let q = 0.5 * (rdy + select(-1.0, 1.0, rdy >= 0.0) * sqrt(d));
    var r1 = q / max(k, 1.0e-12);
    var r2 = c / select(q, 1.0e-9, abs(q) < 1.0e-9);
    return vec2<f32>(min(r1, r2), max(r1, r2));
}

struct Clouds {
    col: vec3<f32>,
    tr: f32,
    // the clouds' distance, weighted by what they add
    depth: f32,
    // rain along the ray, as optical depth (for rainbows)
    rain: f32,
}
// Clouds and rain along a ray, up to `tscene` (the world's surface, or the sky): light added, and what gets through.
fn clouds(ro: vec3<f32>, rd: vec3<f32>, tscene: f32, jit: f32, lt: Lights) -> Clouds {
    var out: Clouds;
    out.col = vec3<f32>(0.0);
    out.tr = 1.0;
    out.depth = 0.0;
    out.rain = 0.0;
    let q = quality();
    let k = (1.0 - rd.y * rd.y) / (2.0 * R_PLANET);
    let hi = clamp(tailf(8u) + 300.0, 2500.0, 15000.0);
    // the march runs from the ground (for the rain) to the tallest tops
    let lo_x = crossings(ro.y, rd.y, k, 0.0);
    let hi_x = crossings(ro.y, rd.y, k, hi);
    var t0 = max(lo_x.x, 0.0);
    var t1 = min(min(lo_x.y, tscene), 160000.0);
    if (ro.y < hi && hi_x.x < 1.0e8 && hi_x.x > 0.0) {
        t1 = min(t1, hi_x.x);
    }
    let steps = select(select(110, 150, q == 2u), 72, q == 0u);
    let grow = select(select(0.010, 0.007, q == 2u), 0.016, q == 0u);
    let detail_far = select(select(16000.0, 30000.0, q == 2u), 8000.0, q == 0u);
    let lsteps = select(4, 3, q == 0u);
    let mu = dot(rd, lt.dir);
    let dr = drift3();
    let wv = wind_vec();
    let tm = wtime();
    var wsum = 0.0;
    var dsum = 0.0;
    if (t1 > t0) {
        var t = t0 + jit * 40.0;
        for (var i = 0; i < steps; i += 1) {
            if (t > t1 || out.tr < 0.01) {
                break;
            }
            let step = clamp(t * grow, 40.0, 1800.0);
            let p = ro + rd * t;
            let off = rd.xz * t;
            let alt = p.y - dot(off, off) / (2.0 * R_PLANET);
            let c = grid(p.xz);
            let in_cloud = c.cov > 0.004 && alt > c.base - 10.0 && alt < c.top + 10.0;
            let in_rain = c.precip > 0.01 && alt < c.base + 10.0;
            if (!in_cloud && !in_rain) {
                // clear air: stride on
                t += max(step * 3.0, t * 0.04);
                continue;
            }
            let air = p - dr;
            var sig = 0.0;
            var lc = vec3<f32>(0.0);
            if (in_cloud) {
                let d = density(c, air, alt, t < detail_far);
                if (d > 0.0) {
                    sig = d * SIGMA;
                    let h = clamp((alt - c.base) / max(c.top - c.base, 60.0), 0.0, 1.0);
                    lc = cloud_light(lt, mu, c, air, alt, h, lsteps, p);
                }
            }
            // rain shafts under the cloud base, leaning with the wind; near the eye vanilla's rain takes over. Above
            // the freezing level it falls as snow: slower (so it leans further), softer, whiter, and thicker to see
            // through; it melts to rain through the 150 m below.
            if (in_rain) {
                let sn = smoothstep(c.freeze - 150.0, c.freeze, alt);
                let lean = wv * (c.base - alt) / mix(9.0, 3.0, sn);
                let ra = air.xz - lean;
                let scale = mix(128.0, 220.0, sn);
                // broad shafts, streaked finely top to bottom (the fall streaks seen from far off)
                let broad = vol(vec3<f32>(ra.x / scale, tm * 0.0128, ra.y / scale)).x;
                let streak = vol(vec3<f32>(ra.x / 34.0 + 17.0, tm * 0.04, ra.y / 34.0)).x;
                let curtain = broad * 0.78 + streak * 0.3 - 0.04;
                let shafts = smoothstep(mix(0.35, 0.2, sn), 0.75, curtain * 0.7 + c.precip * 0.4);
                // Light rain from a high base dries out before it reaches the ground (virga): how far below the base
                // it gets, more the heavier it is.
                let reach = c.base * 0.4 + 4000.0 * c.precip + 3000.0 * c.storm;
                let lands = smoothstep(c.base - reach - 150.0, c.base - reach + 150.0, alt);
                // heavier rain is thicker than its share: a downpour's core goes dark and solid, a sprinkle stays thin
                let r = c.precip * (0.5 + 0.9 * c.precip) * shafts * 0.0016 * (1.0 + 2.0 * c.storm)
                    * (1.0 - smoothstep(c.base - 200.0, c.base + 10.0, alt)) * (1.0 + 1.5 * sn) * lands;
                out.rain += r * step;
                let rn = r * smoothstep(150.0, 600.0, t);
                if (rn > 0.0) {
                    // in the shadow of the cloud it falls from: a heavy cloud's rain (or snow) hangs grey under it,
                    // darkest in a storm's core; a thin shower's stays pale
                    let shade = 1.0 - 0.65 * c.cov * smoothstep(300.0, 4500.0, c.top - c.base);
                    // (the sun reaches it only round the cloud's edges)
                    let sun = shade * shade * (1.0 - c.cov);
                    let wet = lt.amb * 0.9 * shade * (1.0 - 0.5 * c.storm) + lt.gnd_light * hg(mu, 0.6) * 0.25 * sun;
                    let flake = lt.amb * 1.15 * shade * (1.0 - 0.4 * c.storm) + lt.gnd_light * (hg(mu, 0.3) * 0.35 + 0.1) * sun;
                    let rcol = mix(wet, flake, sn) + vec3<f32>(0.75, 0.8, 1.0) * flash_at(p) * 0.3;
                    lc = (lc * sig + rcol * rn) / (sig + rn);
                    sig += rn;
                }
            }
            if (sig > 0.0) {
                let ex = exp(-sig * step);
                let add = out.tr * (1.0 - ex);
                out.col += add * lc;
                wsum += add;
                dsum += add * t;
                out.tr *= ex;
            }
            t += step;
        }
    }
    // the high layer: anvils spreading off storm tops, cirrus ahead of fronts
    if (rd.y > -0.02 && out.tr > 0.01) {
        let high_y = clamp(tailf(11u), 5000.0, 14000.0);
        let th = crossings(ro.y, rd.y, k, high_y).x;
        {
            if (th > 0.0 && th < min(tscene, 2.4e5)) {
                let p = ro + rd * th;
                let c = grid(p.xz);
                if (c.high > 0.01) {
                    let air = p.xz - dr.xz;
                    let w = normalize(wv + vec2<f32>(1.0e-4, 0.0));
                    let along = dot(air, w);
                    let across = dot(air, vec2<f32>(-w.y, w.x));
                    // cirrus: streaks drawn out along the wind; an anvil: a broad, dense sheet
                    let sheet = max(c.storm, smoothstep(0.55, 0.95, c.high));
                    // (two fixed stretches blended: a stretch that changed from place to place would wind the noise
                    // into rings)
                    let cirrus = vol(vec3<f32>(along / 900.0, tm * 0.0032, across / 260.0)).x * 0.7
                        + vol(vec3<f32>(along / 220.0, 7.0, across / 75.0)).y * 0.3;
                    let flat = vol(vec3<f32>(along / 900.0, 11.0 + tm * 0.0032, across / 760.0)).x * 0.75
                        + vol(vec3<f32>(along / 140.0, 19.0, across / 120.0)).y * 0.25;
                    let n = mix(cirrus, flat, sheet);
                    let a = clamp((n - (1.0 - c.high)) * mix(2.4, 4.0, sheet), 0.0, 1.0) * mix(0.55, 0.95, sheet);
                    // seen from beneath, an anvil is a grey ceiling; its sunlit edges and top are bright
                    let under = select(1.0, 1.0 - 0.75 * sheet, ro.y < high_y);
                    let hc = lt.hi * (hg(mu, 0.6) * 1.6 + 0.08 + 0.06 * sheet) * under + lt.amb * mix(0.9, 0.5, sheet * (1.0 - under))
                        + vec3<f32>(0.75, 0.8, 1.0) * flash_at(p) * 0.4;
                    let add = out.tr * a;
                    out.col += add * hc;
                    wsum += add;
                    dsum += add * th;
                    out.tr *= 1.0 - a;
                }
            }
        }
    }
    out.depth = select(0.0, dsum / max(wsum, 1.0e-6), wsum > 1.0e-4);
    return out;
}

// What gets through to the ground at `g` of the sunlight, past the clouds.
fn sun_through(g: vec3<f32>, lt: Lights) -> f32 {
    let s = lt.sun;
    if (s.y < 0.02) {
        return 1.0;
    }
    let c0 = grid(g.xz);
    let ym0 = c0.base + (c0.top - c0.base) * 0.3;
    if (g.y > ym0) {
        return 1.0;
    }
    // the cloud above, towards the sun
    let q0 = g + s * (ym0 - g.y) / s.y;
    let c = grid(q0.xz);
    let ym = c.base + min(c.top - c.base, 3000.0) * 0.3;
    let q = g + s * (ym - g.y) / s.y;
    let d = density(c, q - drift3(), ym, false);
    let thick = min(c.top - c.base, 2500.0);
    let tau = d * SIGMA * thick * 0.3 / max(s.y, 0.2) + c.cov * max(c.layered, c.storm) * 3.0;
    return exp(-tau);
}
// The ground in the clouds' shade keeps the sky's light: about half.
fn shadow(g: vec3<f32>, lt: Lights) -> f32 {
    let day = smoothstep(0.02, 0.2, lt.sun.y);
    let c = grid(g.xz);
    let wet = 1.0 - 0.12 * c.precip - 0.1 * c.storm;
    return mix(1.0, 0.5 + 0.5 * sun_through(g, lt), day) * wet;
}

// Morning fog lying in the valleys: a layer whose top sits above the valley floors and below the hills, gathering
// in the small hours, then rising off the ground and thinning away after sunrise.
fn valley_fog(ro: vec3<f32>, rd: vec3<f32>, tscene: f32, jit: f32, lt: Lights) -> vec4<f32> {
    let amt = fog_amount();
    if (amt < 0.004) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let lift = P(3u).x;
    let top = 80.0 + lift * 16.0;
    let bot = 20.0 + lift * lift * 72.0;
    var t0 = 0.0;
    var t1 = min(tscene, 3600.0);
    if (abs(rd.y) > 1.0e-4) {
        let ta = (bot - ro.y) / rd.y;
        let tb = (top - ro.y) / rd.y;
        t0 = max(t0, min(ta, tb));
        t1 = min(t1, max(ta, tb));
    } else if (ro.y < bot || ro.y > top) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    if (t1 <= t0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let n = 14;
    let dt = (t1 - t0) / f32(n);
    let mu = dot(rd, lt.dir);
    let light = lt.amb * 1.1 + lt.gnd_light * (0.03 + hg(mu, 0.55) * 0.6) * 0.8;
    let dr = drift3() * 0.375;
    let tm = wtime();
    var col = vec3<f32>(0.0);
    var tr = 1.0;
    for (var i = 0; i < n; i += 1) {
        let t = t0 + (f32(i) + jit) * dt;
        let p = ro + rd * t;
        let c = grid(p.xz);
        if (c.fog < 0.01) {
            continue;
        }
        let v = (1.0 - smoothstep(top - 12.0, top, p.y)) * smoothstep(bot, bot + 8.0, p.y);
        let air = p - dr;
        let w = vol(vec3<f32>(air.x / 8.0, p.y / 3.0 + tm * 0.0128, air.z / 8.0)).y;
        let dens = amt * c.fog * v * (0.35 + 1.3 * w) * 0.028 * pow(1.0 - lift, 1.6);
        let a = 1.0 - exp(-dens * dt);
        col += tr * a * light;
        tr *= 1.0 - a;
        if (tr < 0.02) {
            break;
        }
    }
    return vec4<f32>(col, tr);
}

// The rainbow: sunlight turned back by the raindrops of a passing shower, at 42 degrees round the point opposite the
// sun (red outside), with a fainter second bow at 51 (red inside) and the sky brighter within the first.
fn bow_colour(x: f32) -> vec3<f32> {
    // 0 violet .. 1 red
    let r = max(1.0 - pow((x - 0.85) / 0.32, 2.0), 0.0);
    let g = max(1.0 - pow((x - 0.52) / 0.28, 2.0), 0.0);
    let b = max(1.0 - pow((x - 0.18) / 0.3, 2.0), 0.0);
    return vec3<f32>(r + 0.35 * b * smoothstep(0.2, 0.0, x), g, b);
}
// The bows at `ang` degrees from the point opposite the light.
fn bows(ang: f32) -> vec3<f32> {
    let p1 = (ang - 40.6) / 1.8;
    let p2 = (53.4 - ang) / 3.0;
    var c = vec3<f32>(0.0);
    if (p1 > -0.1 && p1 < 1.1) {
        c += bow_colour(p1) * smoothstep(-0.1, 0.1, p1) * (1.0 - smoothstep(0.9, 1.1, p1));
    }
    if (p2 > -0.1 && p2 < 1.1) {
        c += bow_colour(p2) * 0.2 * smoothstep(-0.1, 0.1, p2) * (1.0 - smoothstep(0.9, 1.1, p2));
    }
    c += vec3<f32>(0.08) * smoothstep(30.0, 40.0, ang) * (1.0 - smoothstep(40.0, 41.0, ang));
    return c;
}
fn rainbow(rd: vec3<f32>, lt: Lights, rain: f32, sun_t: f32, ts: f32) -> vec3<f32> {
    let amt = rainbow_amount();
    let up = smoothstep(0.0, 0.06, lt.sun.y) * (1.0 - smoothstep(0.6, 0.75, lt.sun.y));
    if (amt < 0.01 || up <= 0.0) {
        return vec3<f32>(0.0);
    }
    let c = bows(degrees(acos(clamp(dot(rd, -lt.sun), -1.0, 1.0))));
    // the drops between the eye and the ground in front: few, against nearby land
    let drops = max(1.0 - exp(-rain * 2.0), 0.45) * (1.0 - exp(-min(ts, 1.0e5) / 2500.0));
    return c * lt.gnd_light * amt * up * sun_t * drops * 0.14;
}

// The ground beyond the world's edge, under the air: a dim, generic land.
fn far_ground(alt: f32, rd: vec3<f32>, tg: f32, lt: Lights) -> vec3<f32> {
    let p = vec3<f32>(0.0, R_PLANET + max(alt, 1.0), 0.0) + rd * tg;
    let n = normalize(p);
    let sun = lt.sun;
    let lit = light_trans(p, sun, lt.haze) * max(dot(n, sun), 0.0) + light_trans(p, -sun, lt.haze) * max(dot(n, -sun), 0.0) * moon_e();
    return vec3<f32>(0.09, 0.1, 0.07) / PI * lit + lt.amb * 0.03;
}

// ---- waterfalls ----

struct Fall {
    // the plunge point, on the pool's surface
    p: vec3<f32>,
    drop: f32,
    width: f32,
    power: f32,
    seed: f32,
}
fn fall_count() -> u32 {
    if (FALLS_AT + 4u + MAX_FALLS * 8u > arrayLength(&mod_data)) {
        return 0u;
    }
    return min(mod_data[FALLS_AT], MAX_FALLS);
}
fn fall_at(i: u32) -> Fall {
    let k = FALLS_AT + 4u + i * 8u;
    var f: Fall;
    f.p = vec3<f32>(bitcast<f32>(mod_data[k]), bitcast<f32>(mod_data[k + 1u]), bitcast<f32>(mod_data[k + 2u]));
    f.drop = bitcast<f32>(mod_data[k + 3u]);
    f.width = bitcast<f32>(mod_data[k + 4u]);
    f.power = bitcast<f32>(mod_data[k + 5u]);
    f.seed = bitcast<f32>(mod_data[k + 6u]);
    return f;
}
// The spray of one fall at `q` (from its plunge point), `r` across: a cloud of fine drops boiling up off the pool where
// the water strikes it, rolling outwards and upwards and climbing the fall's foot. As the clouds are drawn: billows a
// few blocks across, the more of them the nearer the foot, their edges eaten into wisps.
fn spray(f: Fall, q: vec3<f32>, r: f32, tm: f32) -> f32 {
    let h = q.y / r;
    if (h < -0.05 || h > 1.5) {
        return 0.0;
    }
    let rh = length(q.xz) / r;
    // where the spray reaches: a cloud over the pool, thinning upwards, and a sheath round the foot of the fall
    let k = length(q.xz) / (1.2 + 0.5 * f.width);
    let reach = exp(-rh * rh * 2.0) * exp(-max(h, 0.0) * 2.0) + 0.8 * exp(-k * k) * exp(-max(q.y, 0.0) / (1.5 + 0.3 * f.drop));
    if (reach < 0.02) {
        return 0.0;
    }
    // boiling up and out from the foot
    let rise = tm * (0.7 + 0.03 * f.drop);
    let spread = 1.0 / (1.0 + 0.3 * max(h, 0.0));
    let p = vec3<f32>(q.x * spread, q.y - rise, q.z * spread) + vec3<f32>(f.seed * 2.9, 0.0, f.seed * 1.7);
    let sh = vol(p / 0.8);
    let base = clamp(remap(sh.x, sh.y - 1.0, 1.0, 0.0, 1.0), 0.0, 1.0);
    // (no stray puffs far out: the cloud holds together round the foot)
    let thr = 1.0 - 1.05 * sqrt(min(reach, 1.0));
    var d = clamp(remap(base, thr, 1.0, 0.0, 1.0), 0.0, 1.0) * smoothstep(0.03, 0.15, reach);
    if (d > 0.0) {
        let dn = vol(p / 0.3 + vec3<f32>(0.0, tm * 0.6, 0.0));
        let fbm = dn.y * 0.625 + dn.z * 0.25 + dn.w * 0.125;
        d = clamp(remap(d, fbm * 0.45, 1.0, 0.0, 1.0), 0.0, 1.0);
    }
    return (d + 0.15 * reach) * smoothstep(-0.05, 0.1, h) * f.power;
}
// The mist of the falls near enough to see: light laid over the world (rgb) and what shows through (a). In sunlight
// from behind you, a bow in the spray at 42 degrees round your shadow's head.
fn falls_mist(ro: vec3<f32>, rd: vec3<f32>, tscene: f32, jit: f32, lt: Lights) -> vec4<f32> {
    let nf = fall_count();
    if (nf == 0u) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let tm = frame.time.x;
    let mu = dot(rd, lt.dir);
    // drops scatter forward strongly; many scatterings flatten that
    let lit = lt.gnd_light * lt.sun_cam;
    let light = lt.amb * 1.1 + lt.bounce + lit * (hg(mu, 0.72) * 0.45 + hg(mu, -0.15) * 0.35 + 0.05);
    // (by sunlight only: a moonbow is too faint to show its colours)
    let bow = bows(degrees(acos(clamp(-mu, -1.0, 1.0)))) * lit * smoothstep(-0.02, 0.08, lt.sun.y) * 0.65;
    let q = quality();
    let steps = select(select(24, 32, q == 2u), 14, q == 0u);
    var col = vec3<f32>(0.0);
    var tr = 1.0;
    for (var i = 0u; i < nf; i += 1u) {
        let f = fall_at(i);
        // a small fall's spray stays round its foot; a big one's billows up and out
        let r = clamp(1.5 + 0.3 * f.drop + 0.5 * f.width, 2.5, 22.0);
        // a sphere round the cloud
        let c = f.p + vec3<f32>(0.0, r * 0.5, 0.0);
        let rad = r * 1.6;
        let oc = ro - c;
        if (dot(oc, oc) > 400.0 * 400.0) {
            continue;
        }
        let b = dot(oc, rd);
        let disc = b * b - (dot(oc, oc) - rad * rad);
        if (disc <= 0.0) {
            continue;
        }
        let sq = sqrt(disc);
        let t0 = max(-b - sq, 0.0);
        let t1 = min(-b + sq, tscene);
        if (t1 <= t0) {
            continue;
        }
        let dt = (t1 - t0) / f32(steps);
        for (var s = 0; s < steps; s += 1) {
            let t = t0 + (f32(s) + jit) * dt;
            let d = spray(f, ro + rd * t - f.p, r, tm);
            if (d < 1.0e-3) {
                continue;
            }
            let a = 1.0 - exp(-d * 0.3 * dt);
            // (the bow shows in the billows, not in the thin haze round them)
            col += tr * a * (light + bow * smoothstep(0.08, 0.45, d / max(f.power, 0.1)));
            tr *= 1.0 - a;
        }
        if (tr < 0.02) {
            break;
        }
    }
    return vec4<f32>(col, tr);
}

fn ray_dir(uv: vec2<f32>) -> vec3<f32> {
    return normalize(world_offset(uv, 0.5));
}

struct Half {
    @location(0) color: vec4<f32>,
    @location(1) extra: vec4<f32>,
}

// Everything the half-size stage draws for one pixel: the light laid over the world, what shows through of the sky's
// lights (colour.a) and of the world (extra.yzw).
fn shade(uv: vec2<f32>, depth: f32, jit: f32) -> Half {
    let ro = frame.camera_position.xyz;
    let rd = ray_dir(uv);
    let sky = is_sky(depth);
    var ts = 1.0e9;
    var ld = SKY_LD;
    if (!sky) {
        ts = length(world_offset(uv, depth));
        ld = linear_depth(depth);
    }
    let here = grid(ro.xz);
    let lt = lights(ro, here);
    let cl = clouds(ro, rd, ts, jit, lt);
    let q = quality();
    let n = select(select(14, 18, q == 2u), 10, q == 0u);
    // (the clouds hazed as if a little nearer than they are: in a game's view the distant storms should still tell)
    let air = atmosphere(ro.y, rd, cl.depth * 0.65, ts, lt.haze, n, vec2<f32>(0.12 + 0.88 * lt.sun_cam, here.base));
    // the light of the clouds, reddened and hazed by the air between, then the air beyond them
    var add = air.sa + air.ta * cl.col + cl.tr * (air.sb - air.sa);
    var through = cl.tr * air.tb;
    var fade = 1.0;
    if (!sky) {
        // the world's far edge melts into the land and sky beyond it
        let dh = length((rd * ts).xz);
        let z = frame.view_params.z;
        fade = smoothstep(0.8 * z, 0.97 * z, dh);
        through *= (1.0 - fade) * shadow(ro + rd * ts, lt);
    }
    // the faint glow of the night sky itself (airglow and starlight), dimmed towards the horizon by the air
    var beyond = air.s - air.sb + vec3<f32>(0.00012, 0.00016, 0.00032) * luminance(air.t) * select(1.0, 0.0, air.ground > 0.0);
    if (air.ground > 0.0) {
        beyond += air.t * far_ground(ro.y, rd, air.ground, lt);
    }
    add += cl.tr * beyond * fade;
    let fg = valley_fog(ro, rd, ts, jit, lt);
    add = fg.rgb + fg.a * add;
    through *= fg.a;
    add += rainbow(rd, lt, cl.rain, lt.sun_cam, ts) * fg.a;
    // the mist of waterfalls, close by and so in front of the rest
    let ms = falls_mist(ro, rd, ts, jit, lt);
    add = ms.rgb + ms.a * add;
    through *= ms.a;
    let exposure = EXPOSURE * lt.adapt;
    let col = aces(add * exposure);
    var shows = 0.0;
    if (sky && air.ground < 0.0) {
        shows = cl.tr * fg.a * ms.a * luminance(air.t);
    }
    if (sky) {
        through = vec3<f32>(0.0);
    }
    return Half(vec4<f32>(col, shows), vec4<f32>(ld, through));
}

// ---- at full size: the sun, the moon, the stars, lightning ----

fn celestial(rd: vec3<f32>, sky: vec3<f32>) -> vec3<f32> {
    let ro = frame.camera_position.xyz;
    let sun = sun_vec();
    let o = vec3<f32>(0.0, R_PLANET + max(ro.y, 1.0), 0.0);
    var out = vec3<f32>(0.0);
    let cs = dot(rd, sun);
    if (cs > 0.995) {
        let ang = acos(min(cs, 1.0));
        let r = ang / SUN_R;
        let ts = light_trans(o, sun, 2.0);
        let limb = 1.0 - 0.6 * (1.0 - sqrt(max(1.0 - r * r, 0.0)));
        let disc = (1.0 - smoothstep(0.92, 1.0, r)) * limb;
        out += ts * (disc * 4.0 + exp(-ang / 0.006) * 0.35);
    }
    let m = -sun;
    let cm = dot(rd, m);
    if (cm > 0.995) {
        let tm = light_trans(o, m, 2.0);
        // the disc, lit as the phase has it
        let right = normalize(cross(m, vec3<f32>(0.0, 0.0, 1.0)));
        let upv = cross(right, m);
        let p = vec2<f32>(dot(rd, right), dot(rd, upv)) / MOON_R;
        let r2 = dot(p, p);
        let ph = moon_phase() * PI / 4.0;
        if (r2 < 1.0) {
            let nrm = vec3<f32>(p, sqrt(1.0 - r2));
            let l = vec3<f32>(sin(ph), 0.0, cos(ph));
            let lit = smoothstep(-0.04, 0.08, dot(nrm, l));
            // the seas: darker patches
            let mare = vol(vec3<f32>(p.x * 5.0 + 3.0, p.y * 5.0 + 9.0, 4.0)).x;
            let albedo = 0.75 + 0.25 * smoothstep(0.35, 0.65, mare);
            out += tm * vec3<f32>(0.95, 0.97, 1.0) * (lit * albedo * 0.85 + 0.015) * (1.0 - smoothstep(0.9, 1.0, r2));
        }
        // its glow, as bright as the moon is full
        let full = 0.5 + 0.5 * cos(ph);
        out += tm * vec3<f32>(0.6, 0.7, 0.9) * exp(-acos(min(cm, 1.0)) / 0.03) * 0.08 * full;
    }
    // the stars, turning with the sky
    let dark = 1.0 - smoothstep(0.01, 0.07, luminance(sky));
    if (dark > 0.0) {
        let a = -frame.sun.w;
        let cr = vec3<f32>(rd.x * cos(a) - rd.y * sin(a), rd.x * sin(a) + rd.y * cos(a), rd.z);
        let sc = 240.0;
        let qv = cr * sc;
        let cell = floor(qv);
        let ci = vec3<i32>(cell);
        let h = hash3i(ci.x, ci.y, ci.z);
        if (h > 0.965) {
            let j = vec3<f32>(hash3i(ci.x + 7, ci.y, ci.z), hash3i(ci.x, ci.y + 7, ci.z), hash3i(ci.x, ci.y, ci.z + 7));
            let sp = normalize(cell + 0.3 + 0.4 * j);
            let d2 = 2.0 * max(1.0 - dot(sp, cr), 0.0);
            let pix = 2.0 * tan(frame.view_params.w * 0.5) * frame.resolution.w;
            let size = pix * 0.9;
            let mag = pow(fract(h * 37.31), 7.0);
            let tw = 0.75 + 0.25 * sin(frame.time.x * 4.0 + h * 400.0);
            let tint = mix(vec3<f32>(1.0, 0.85, 0.7), vec3<f32>(0.75, 0.85, 1.0), fract(h * 91.7));
            out += tint * exp(-d2 / (size * size)) * (0.06 + 1.4 * mag) * tw * dark;
        }
        out += (shooting_star(rd) + northern_lights(rd, tailf(14u))) * dark;
    }
    return out;
}

// Now and then a shooting star: a short bright streak across the upper sky, gone in under a second. About one
// every ten seconds somewhere overhead, so a few a minute where you look.
fn shooting_star(rd: vec3<f32>) -> vec3<f32> {
    let slot_len = 2.5;
    let t = frame.time.x;
    let slot = floor(t / slot_len);
    let si = i32(slot);
    if (hash3i(si, 71, 3) > 0.25) {
        return vec3<f32>(0.0);
    }
    // where it starts, which way it goes, and when
    let az = hash3i(si, 72, 3) * 2.0 * PI;
    let el = 0.45 + 0.75 * hash3i(si, 73, 3);
    let s0 = vec3<f32>(cos(az) * cos(el), sin(el), sin(az) * cos(el));
    let east = normalize(cross(vec3<f32>(0.0, 1.0, 0.0), s0));
    let down = cross(east, s0);
    let turn = (hash3i(si, 74, 3) - 0.5) * 2.4;
    let dir = normalize(down * 0.8 + east * turn);
    let side = cross(s0, dir);
    let life = 0.45 + 0.4 * hash3i(si, 75, 3);
    let start = hash3i(si, 76, 3) * (slot_len - life);
    let age = (t - slot * slot_len - start) / life;
    if (age < 0.0 || age > 1.0) {
        return vec3<f32>(0.0);
    }
    let c = dot(rd, s0);
    if (c < 0.8) {
        return vec3<f32>(0.0);
    }
    // in the plane of the sky round its start: along its path and across it
    let q = vec2<f32>(dot(rd, dir), dot(rd, side)) / c;
    let reach = 0.18 + 0.2 * hash3i(si, 77, 3);
    let head = age * reach;
    let tail = max(head - 0.1, 0.0);
    let along = clamp((q.x - tail) / max(head - tail, 1.0e-4), 0.0, 1.0);
    let inside = step(tail, q.x) * step(q.x, head + 0.004);
    let pix = 2.0 * tan(frame.view_params.w * 0.5) * frame.resolution.w;
    let w = pix * 0.8;
    let across = exp(-q.y * q.y / (w * w));
    // flares up, burns, fades
    let glow = smoothstep(0.0, 0.15, age) * (1.0 - smoothstep(0.6, 1.0, age));
    return vec3<f32>(0.9, 0.95, 1.0) * across * inside * along * along * glow * 1.6;
}

// The northern lights (the mod's strength for tonight, 0..1): curtains a few hundred kilometres to the north,
// 100 to 300 km up, so low over the northern horizon: green at their foot fading to violet at the top, with fine
// rays along them, folding and drifting slowly.
fn northern_lights(rd: vec3<f32>, amount: f32) -> vec3<f32> {
    if (amount <= 0.001 || rd.y <= 0.0 || rd.z >= -0.05) {
        return vec3<f32>(0.0);
    }
    let t = frame.time.x;
    var acc = vec3<f32>(0.0);
    // two curtains, the nearer brighter (distances in 100 km)
    for (var k = 0; k < 2; k += 1) {
        let kf = f32(k);
        let base = 5.0 + 2.5 * kf;
        // where the line of sight reaches the curtain's wavy path: a couple of steps of z = path(x)
        var u = base / -rd.z;
        for (var i = 0; i < 3; i += 1) {
            let x = rd.x * u;
            let zc = base + 1.1 * sin(x * 0.3 + t * 0.012 + kf * 2.0) + 0.45 * sin(x * 0.85 - t * 0.02 + kf);
            u = zc / -rd.z;
        }
        let x = rd.x * u;
        let h = rd.y * u;
        // its foot wavers a little; it is brightest just above the foot and fades upward
        let foot = 1.0 + 0.12 * sin(x * 1.7 + t * 0.05 + kf * 3.0);
        let rise = h - foot;
        let shape = smoothstep(-0.06, 0.05, rise) * exp(-max(rise, 0.0) * 1.6);
        if (shape < 0.002) {
            continue;
        }
        // rays: fine bright streaks up the curtain, sliding along it
        let r1 = 0.5 + 0.5 * sin(x * 9.0 + sin(x * 2.1 + t * 0.07) * 2.5 + t * 0.12);
        let r2 = 0.5 + 0.5 * sin(x * 23.0 - t * 0.2 + kf * 5.0);
        let rays = 0.35 + 0.65 * r1 * (0.6 + 0.4 * r2);
        // the curtain's length: brighter in parts, ending east and west
        let span = exp(-x * x / 160.0) * (0.55 + 0.45 * sin(x * 0.45 - t * 0.015 + kf * 4.0));
        let hue = clamp(rise / 1.6, 0.0, 1.0);
        let col = mix(vec3<f32>(0.15, 1.0, 0.45), vec3<f32>(0.55, 0.2, 0.75), smoothstep(0.15, 0.9, hue));
        acc += col * shape * rays * span * (1.0 - 0.45 * kf);
    }
    // a slow pulse through the night, and haze low on the horizon
    let pulse = 0.75 + 0.25 * sin(t * 0.04) * sin(t * 0.017 + 1.0);
    return acc * amount * pulse * 0.12 * smoothstep(0.0, 0.08, rd.y);
}

// Rain (or snow) falling close by, drawn here in place of the game's own (whose streaks would stand in front of
// this pass's sky as if they were the world). Each drop falls down a straight line in the world, slanted with the
// wind, so the streaks stay straight however the view turns. The lines stand on a grid of cells (one line a cell, or
// none), finer close by: three layers, out to 6, 14 and 35 m. A drop is hidden behind whatever is nearer and under
// whatever keeps the rain off (the mod's cover map: roofs, trees, the cave's ceiling, the ground and water).

// what keeps the rain off: after the waterfalls (src/sim.rs COVER_AT)
const COVER_AT: u32 = 98388u;
const COVER_N: i32 = 64;

// The height rain falls down to over the column at (x, z): a huge one where the map has not come yet or does not
// reach (so no rain is drawn there), and none where the column is not loaded.
fn cover_top(xz: vec2<f32>) -> f32 {
    if (COVER_AT + 4u + u32(COVER_N * COVER_N) > arrayLength(&mod_data) || mod_data[COVER_AT + 2u] == 0u) {
        return 1.0e9;
    }
    let o = vec2<i32>(bitcast<i32>(mod_data[COVER_AT]), bitcast<i32>(mod_data[COVER_AT + 1u]));
    let c = vec2<i32>(floor(xz)) - o;
    if (c.x < 0 || c.y < 0 || c.x >= COVER_N || c.y >= COVER_N) {
        return 1.0e9;
    }
    let h = bitcast<i32>(mod_data[COVER_AT + 4u + u32(c.y * COVER_N + c.x)]);
    if (h == -2147483647 - 1) {
        return -1.0e9;
    }
    return f32(h);
}

struct Precip {
    snow: bool,
    t: f32,
    pix: f32,
    fall: f32,
    amt: f32,
    drizzle: f32,
    heavy: f32,
    col: vec3<f32>,
    // how far a drop drifts sideways for each metre it falls
    sh: vec2<f32>,
}

// One layer of drops: the lines in the cells the ray crosses (seen from above, where every line is a point once the
// wind's slant is taken out) between t0 and t1 along the ray.
fn precip_layer(ro: vec3<f32>, rd: vec3<f32>, ts: f32, t0: f32, t1: f32, cell: f32, k: i32, pr: Precip, rgb: vec3<f32>) -> vec3<f32> {
    let tend = min(t1, ts);
    if (tend <= t0) {
        return rgb;
    }
    // the ray in the slant-free plan, in cells
    let o2 = (ro.xz + pr.sh * (ro.y - 64.0)) / cell;
    let d2 = (rd.xz + pr.sh * rd.y) / cell;
    let dd = dot(d2, d2);
    if (dd < 1.0e-8) {
        return rgb;
    }
    let p0 = o2 + d2 * t0;
    var ci = vec2<i32>(floor(p0));
    let stp = vec2<i32>(select(-1, 1, d2.x >= 0.0), select(-1, 1, d2.y >= 0.0));
    let inv = 1.0 / max(abs(d2), vec2<f32>(1.0e-6));
    let fr = p0 - floor(p0);
    var tx = t0 + select(fr.x, 1.0 - fr.x, d2.x >= 0.0) * inv.x;
    var tz = t0 + select(fr.y, 1.0 - fr.y, d2.y >= 0.0) * inv.y;
    let colw = select(mix(0.012, 0.008, pr.drizzle), 0.025, pr.snow);
    let gap = select(mix(mix(4.0, 1.3, pr.drizzle), 3.0, pr.heavy), 0.9, pr.snow);
    let len = select(mix(mix(0.7, 0.18, pr.drizzle), 1.15, pr.heavy), 0.05, pr.snow);
    let strength = select(mix(mix(0.3, 0.18, pr.drizzle), 0.42, pr.heavy), 0.75, pr.snow) * (1.0 - f32(k) * 0.18);
    var out = rgb;
    var tc0 = t0;
    for (var n = 0; n < 20; n += 1) {
        if (tc0 >= tend) {
            break;
        }
        let h = hash3i(ci.x, ci.y, 101 + k * 17);
        if (h < pr.amt * mix(0.8, 1.0, pr.heavy)) {
            // the line's place in its cell, kept off the cell's edges
            let c2 = vec2<f32>(ci) + 0.2 + 0.6 * vec2<f32>(hash3i(ci.x, ci.y, 211 + k), hash3i(ci.x, ci.y, 307 + k));
            let tc = clamp(dot(c2 - o2, d2) / dd, t0, tend);
            let perp = length(o2 + d2 * tc - c2) * cell;
            let width = max(colw, tc * pr.pix * 0.6);
            let y = ro.y + rd.y * tc;
            let v = y + pr.t * pr.fall + h * 53.0;
            let wob = select(0.0, sin(v * 1.7 + h * 40.0) * 0.08, pr.snow);
            let across = 1.0 - smoothstep(0.0, width, abs(perp + wob));
            // where the line is at this height, in the world, for what keeps the rain off it
            let at = c2 * cell - pr.sh * (y - 64.0);
            if (across > 0.0 && y >= cover_top(at)) {
                let cl = floor(v / gap);
                let fv = (v / gap - cl) * gap;
                let along = smoothstep(0.0, len * 0.3, fv) * (1.0 - smoothstep(len * 0.5, len, fv));
                // (and gone where the lines are too close together on screen to draw without shimmering)
                let fine = smoothstep(1.5, 4.0, cell / max(tc * pr.pix, 1.0e-5));
                // the gusts' sheets, sweeping round
                let ang = atan2(rd.z, rd.x) * 6.0;
                let sheet = 1.0 - pr.heavy * 0.55 * (0.5 + 0.5 * sin(ang + pr.t * 2.2 + sin(pr.t * 0.7) * 2.0));
                // and thinning out toward the layer's far end, where the next takes over
                let ends = 1.0 - 0.5 * smoothstep(t1 * 0.75, t1, tc);
                out = mix(out, pr.col, across * along * strength * sheet * fine * ends);
            }
        }
        // on to the next cell
        if (tx < tz) {
            tc0 = tx;
            tx += inv.x;
            ci.x += stp.x;
        } else {
            tc0 = tz;
            tz += inv.y;
            ci.y += stp.y;
        }
    }
    return out;
}

fn near_precip(rd: vec3<f32>, ts: f32, rgb: vec3<f32>) -> vec3<f32> {
    let ro = frame.camera_position.xyz;
    let c = grid(ro.xz);
    if (c.precip <= 0.03) {
        return rgb;
    }
    var pr: Precip;
    pr.snow = tailf(9u) > 0.3;
    // it falls in real time, however fast the weather runs
    pr.t = frame.time.x;
    pr.pix = 2.0 * tan(frame.view_params.w * 0.5) * frame.resolution.w;
    // Drizzle (a deck's, a shower's edge): fine, short, slow drops, many of them. Rain: streaks. A downpour (a storm's
    // core, a heavy shower): long dense streaks, swept round in sheets by the gusts.
    pr.drizzle = select(1.0 - smoothstep(0.12, 0.3, c.precip), 0.0, pr.snow);
    pr.heavy = select(smoothstep(0.6, 0.95, c.precip + 0.3 * c.storm), 0.0, pr.snow);
    pr.fall = select(mix(mix(8.5, 4.5, pr.drizzle), 10.0, pr.heavy), 1.1, pr.snow);
    // how many of the cells hold a line of drops: drizzle is many fine ones even when it is light
    pr.amt = max(smoothstep(0.03, 0.45, c.precip), smoothstep(0.03, 0.1, c.precip) * 0.75 * pr.drizzle);
    let sky = srgb_to_linear(frame.sky_color.rgb);
    pr.col = select(vec3<f32>(0.62, 0.66, 0.72), vec3<f32>(0.95, 0.96, 1.0), pr.snow) * (0.1 + 0.9 * luminance(sky));
    pr.sh = wind_vec() / pr.fall;
    let cell = select(mix(0.45, 0.32, pr.drizzle), 0.6, pr.snow);
    var out = precip_layer(ro, rd, ts, 0.3, 6.0, cell, 0, pr, rgb);
    out = precip_layer(ro, rd, ts, 6.0, 14.4, cell * 2.2, 1, pr, out);
    out = precip_layer(ro, rd, ts, 14.4, 34.6, cell * 4.8, 2, pr, out);
    return out;
}

// One step of a bolt's path: down a stretch, wandering sideways (a random walk, so it jags like the real thing).
fn bolt_step(k: i32, seed: i32, branch: i32, drop: f32, wander: f32) -> vec3<f32> {
    let j = vec2<f32>(hash3i(k, seed, 1 + branch * 7), hash3i(k, seed, 2 + branch * 7)) - 0.5;
    return vec3<f32>(j.x * wander, -drop * (0.6 + 0.8 * hash3i(k, seed, 3 + branch * 7)), j.y * wander);
}
// The closest approach of the ray `ro + rd t` to the segment `a b`: distance, and t there.
fn seg_dist(ro: vec3<f32>, rd: vec3<f32>, a: vec3<f32>, b: vec3<f32>) -> vec2<f32> {
    let ba = b - a;
    let oa = ro - a;
    let bb = dot(rd, ba);
    let cc = dot(ba, ba);
    let dd = dot(rd, oa);
    let ee = dot(ba, oa);
    let u = clamp((ee - bb * dd) / max(cc - bb * bb, 1.0e-4), 0.0, 1.0);
    let t = max(u * bb - dd, 0.0);
    return vec2<f32>(length(oa + rd * t - ba * u), t);
}
// The bolt's segment nearest the ray, core and glow; `lit` keeps the brightest.
fn bolt_seg(ro: vec3<f32>, rd: vec3<f32>, a: vec3<f32>, b: vec3<f32>, ts: f32, pix: f32, k: f32, lit: vec2<f32>) -> vec2<f32> {
    let r = seg_dist(ro, rd, a, b);
    if (r.y >= ts) {
        return lit;
    }
    let w = max(1.5, r.y * pix * 1.2) * k;
    let core = (1.0 - smoothstep(0.0, w, r.x)) * k;
    let glow = exp(-r.x / max(12.0, r.y * pix * 14.0)) * k;
    return max(lit, vec2<f32>(core, glow));
}
fn lightning(uv: vec2<f32>, depth: f32, rgb: vec3<f32>) -> vec3<f32> {
    let s = P(2u);
    if (s.z > 1.2 || s.z < 0.0) {
        return rgb;
    }
    let ro = frame.camera_position.xyz;
    let rd = ray_dir(uv);
    var ts = 1.0e9;
    if (!is_sky(depth)) {
        ts = length(world_offset(uv, depth));
    }
    var out = rgb;
    let vis = strokes(s.z);
    if (vis > 0.0) {
        let c = grid(s.xy);
        let top = clamp(c.base, 300.0, 2500.0) + 60.0;
        let pix = 2.0 * tan(frame.view_params.w * 0.5) * frame.resolution.w;
        let seed = i32(s.w);
        let drop = (top - 50.0) / 22.0;
        var lit = vec2<f32>(0.0);
        var a = vec3<f32>(s.x, top, s.y);
        var fork = a;
        for (var k = 0; k < 22; k += 1) {
            let b = a + bolt_step(k, seed, 0, drop, 90.0);
            lit = bolt_seg(ro, rd, a, b, ts, pix, 1.0, lit);
            if (k == 6) {
                fork = b;
            }
            a = b;
        }
        // a fainter branch off the upper part
        let side = select(-1.0, 1.0, hash3i(seed, 9, 9) > 0.5);
        a = fork;
        for (var k = 0; k < 9; k += 1) {
            let b = a + bolt_step(k, seed, 1, drop * 0.7, 100.0) + vec3<f32>(side * 30.0, 0.0, 12.0);
            lit = bolt_seg(ro, rd, a, b, ts, pix, 0.5, lit);
            a = b;
        }
        out += vec3<f32>(0.85, 0.9, 1.0) * (lit.x * 2.0 + lit.y * 0.35) * vis;
    }
    // a strike close by lights up everything
    let dcam = distance(ro.xz, s.xy);
    out *= 1.0 + flicker(s.z) * 0.7 * exp(-dcam / 1800.0);
    return out;
}

// ---- the stages ----

@fragment fn fs_half(in: StageIn) -> Half {
    let hp = vec2<i32>(in.position.xy);
    let res = vec2<i32>(frame.resolution.xy);
    let base = hp * 2;
    // the nearest and the farthest of each two by two, alternately, so both sides of an edge are drawn
    let near = ((hp.x + hp.y) & 1) == 0;
    var best = min(base, res - 1);
    var bd = textureLoad(scene_depth, best, 0);
    for (var k = 1; k < 4; k += 1) {
        let q = min(base + vec2<i32>(k & 1, k >> 1), res - 1);
        let d = textureLoad(scene_depth, q, 0);
        if ((near && d > bd) || (!near && d < bd)) {
            bd = d;
            best = q;
        }
    }
    let uv = (vec2<f32>(best) + 0.5) * frame.resolution.zw;
    return shade(uv, bd, ign(in.position.xy));
}

fn finish(uv: vec2<f32>, d: f32, scene: vec4<f32>, c: vec4<f32>, through: vec3<f32>) -> vec4<f32> {
    var rgb: vec3<f32>;
    if (is_sky(d)) {
        let cel = celestial(ray_dir(uv), c.rgb) * c.a;
        rgb = c.rgb + vec3<f32>(1.0) - exp(-cel);
    } else {
        rgb = srgb_to_linear(scene.rgb) * through + c.rgb;
    }
    var ts = 1.0e9;
    if (!is_sky(d)) {
        ts = length(world_offset(uv, d));
    }
    rgb = near_precip(ray_dir(uv), ts, rgb);
    rgb = lightning(uv, d, rgb);
    return vec4<f32>(linear_to_srgb(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0))), scene.a);
}

@fragment fn fs_join(in: StageIn) -> @location(0) vec4<f32> {
    let scene = textureSampleLevel(scene_color, nearest_sampler, in.uv, 0.0);
    let d = depth_at(in.uv);
    var ld = SKY_LD;
    if (!is_sky(d)) {
        ld = linear_depth(d);
    }
    let hs = vec2<i32>(textureDimensions(half_picture));
    // a tent three half-size texels across: wide enough to melt the march's dither on soft, thin cloud
    let h = in.position.xy * 0.5 - 0.5;
    let c0 = vec2<i32>(round(h));
    var acc = vec4<f32>(0.0);
    var thr = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var k = 0; k < 9; k += 1) {
        let q0 = c0 + vec2<i32>(k % 3 - 1, k / 3 - 1);
        let q = clamp(q0, vec2<i32>(0), hs - 1);
        let dd = abs(h - vec2<f32>(q0)) / 1.5;
        let bw = max(1.0 - dd.x, 0.0) * max(1.0 - dd.y, 0.0);
        let ex = textureLoad(half_extra, q, 0);
        let rel = abs(ex.x - ld) / max(min(ex.x, ld), 1.0);
        let w = bw * (exp(-rel * 24.0) + 1.0e-4);
        acc += textureLoad(half_picture, q, 0) * w;
        thr += ex.yzw * w;
        wsum += w;
    }
    let iw = 1.0 / max(wsum, 1.0e-6);
    return finish(in.uv, d, scene, acc * iw, thr * iw);
}

// Without the half-size stage: everything at full size.
@fragment fn fs_main(in: StageIn) -> @location(0) vec4<f32> {
    let scene = textureSampleLevel(scene_color, nearest_sampler, in.uv, 0.0);
    let d = depth_at(in.uv);
    let s = shade(in.uv, d, ign(in.position.xy));
    return finish(in.uv, d, scene, s.color, s.extra.yzw);
}
