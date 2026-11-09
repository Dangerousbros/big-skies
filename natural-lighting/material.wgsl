// Natural Lighting's light on terrain: vanilla's lightmap, with the share the sky gives dimmed and cooled where the sun (or
// the moon) can't reach and warmed where it does; soft shadows; the sun's light through water onto what lies
// under it. And water: gentle waves, the world under it bent by them and fading into the water's own colour with
// depth, the world mirrored in it at grazing angles, and the sun's glint.

override shadows: bool = true;
override shadow_quality: u32 = 1u;
override shadow_softness: f32 = 1.0;
override shade: f32 = 0.55;
override caustics: bool = true;
override waves: bool = true;
override reflections: bool = true;
override water_clarity: f32 = 12.0;

const POISSON = array<vec2<f32>, 16>(
    vec2<f32>(-0.94201624, -0.39906216), vec2<f32>(0.94558609, -0.76890725), vec2<f32>(-0.09418410, -0.92938870),
    vec2<f32>(0.34495938, 0.29387760), vec2<f32>(-0.91588581, 0.45771432), vec2<f32>(-0.81544232, -0.87912464),
    vec2<f32>(-0.38277543, 0.27676845), vec2<f32>(0.97484398, 0.75648379), vec2<f32>(0.44323325, -0.97511554),
    vec2<f32>(0.53742981, -0.47373420), vec2<f32>(-0.26496911, -0.41893023), vec2<f32>(0.79197514, 0.19090188),
    vec2<f32>(-0.24188840, 0.99706507), vec2<f32>(-0.81409955, 0.91437590), vec2<f32>(0.19984126, 0.78641367),
    vec2<f32>(0.14383161, -0.14100790),
);

// Blocks along the light for each unit of the shadow map's depth (the shadow camera is orthographic).
fn a_shadow_range() -> f32 {
    let m = frame.shadow_view_proj;
    return 1.0 / max(length(vec3<f32>(m[0].z, m[1].z, m[2].z)), 1.0e-6);
}

// How far out in the shadow map `pos` is, 0 at its middle and 1 at its edge (before the distortion).
fn a_shadow_reach(pos: vec3<f32>) -> f32 {
    let c = frame.shadow_view_proj * vec4<f32>(pos, 1.0);
    return max(abs(c.x), abs(c.y));
}

// The ground one texel of the shadow map covers at `pos`: the map is finest in its middle.
fn a_shadow_texel(pos: vec3<f32>) -> f32 {
    let c = frame.shadow_view_proj * vec4<f32>(pos, 1.0);
    let k = frame.shadow_distortion;
    let r = length(c.xy);
    return frame.shadow_center_texel * pow((r * k + 1.0 - k) / max(1.0 - k, 0.05), 2.0);
}

fn a_in_map(s: vec3<f32>) -> bool {
    return s.x > 0.0 && s.y > 0.0 && s.x < 1.0 && s.y < 1.0 && s.z > 0.0 && s.z < 1.0;
}

// How much of the sun's (or moon's) light reaches the camera-relative `pos` on a face with normal `n`: soft edges,
// softer the farther the shadow falls from what casts it (Shadow edges medium and high search the casters first).
fn a_sun(pos: vec3<f32>, n: vec3<f32>, pixel: vec2<f32>) -> f32 {
    if (!shadows || frame.has_shadow == 0u) {
        return 1.0;
    }
    let l = frame.light_direction;
    let texel = max(a_shadow_texel(pos), 0.01);
    // a texel or two off the surface, toward its normal and the light, so it never shadows itself
    let p = pos + (n * 1.5 + l * 0.5) * texel;
    let s = shadow_position(p);
    if (!a_in_map(s)) {
        return 1.0;
    }
    // the filter's taps lie across the light, turned per pixel
    let side = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(l.y) > 0.9);
    let u = normalize(cross(l, side));
    let v = cross(l, u);
    let angle = a_noise(pixel) * 6.2831853;
    let turn = mat2x2<f32>(cos(angle), sin(angle), -sin(angle), cos(angle));
    var radius = max(0.1, texel * 1.5) * shadow_softness;
    if (shadow_quality >= 1u) {
        let size = vec2<f32>(textureDimensions(shadow_map));
        var blockers = 0.0;
        var gap = 0.0;
        for (var i = 0; i < 8; i += 1) {
            let o = turn * POISSON[i] * 0.9;
            let q = shadow_position(p + u * o.x + v * o.y);
            let at = vec2<i32>(clamp(q.xy * size, vec2<f32>(0.0), size - 1.0));
            let d = textureLoad(shadow_map, at, 0);
            if (d < q.z) {
                blockers += 1.0;
                gap += q.z - d;
            }
        }
        if (blockers < 0.5) {
            return 1.0;
        }
        radius = clamp(gap / blockers * a_shadow_range() * 0.04, max(0.06, texel), 1.0) * shadow_softness;
    }
    var taps = 5;
    if (shadow_quality == 1u) {
        taps = 12;
    } else if (shadow_quality >= 2u) {
        taps = 16;
    }
    var lit = 0.0;
    for (var i = 0; i < taps; i += 1) {
        let o = turn * POISSON[i] * radius;
        let q = shadow_position(p + u * o.x + v * o.y);
        lit += textureSampleCompareLevel(shadow_map, shadow_sampler, q.xy, q.z);
    }
    // fading out toward the map's edge, where the light carries on unshadowed
    return mix(lit / f32(taps), 1.0, smoothstep(0.85, 0.98, a_shadow_reach(pos)));
}

// Light through moving water: bright threads that drift and change, about two blocks across (0..1).
fn a_caustic_pattern(q: vec2<f32>, t: f32) -> f32 {
    let a = sin(q.x * 1.7 + sin(q.y * 1.3 + t * 0.9) * 1.2 + t * 0.7);
    let b = sin(q.y * 1.9 + sin(q.x * 1.1 - t * 0.8) * 1.2 - t * 0.6);
    let c = sin((q.x + q.y) * 1.3 + sin(q.x * 0.7 - q.y * 0.9 + t) * 1.1 + t * 0.5);
    return pow(1.0 - abs(a + b + c) / 3.0, 6.0);
}

// The sun's light through water onto `pos` (0..1): where water lies between it and the light, fading with the
// depth under the surface and toward the map's edge.
fn a_caustics(pos: vec3<f32>) -> f32 {
    let s = shadow_position(pos);
    if (!a_in_map(s)) {
        return 0.0;
    }
    let size = vec2<f32>(textureDimensions(shadow_map_translucent));
    let surface = textureLoad(shadow_map_translucent, vec2<i32>(clamp(s.xy * size, vec2<f32>(0.0), size - 1.0)), 0);
    let csize = vec2<f32>(textureDimensions(shadow_color));
    let what = textureLoad(shadow_color, vec2<i32>(clamp(s.xy * csize, vec2<f32>(0.0), csize - 1.0)), 0);
    // water (alpha 1), not glass or ice
    if (what.a < 0.995) {
        return 0.0;
    }
    let under = (s.z - surface) * a_shadow_range();
    if (under < 0.05) {
        return 0.0;
    }
    // on a plane across the light, so the pattern falls along it
    let p = pos + frame.camera_position;
    let l = frame.light_direction;
    let q = (p.xz - l.xz * (p.y / max(l.y, 0.25))) * 2.0;
    let fade = exp(-under / 6.0) * smoothstep(0.05, 0.5, under) * (1.0 - smoothstep(0.85, 0.98, a_shadow_reach(pos)));
    return a_caustic_pattern(q, frame.time) * fade;
}

// Vanilla's brightness for a light level (0..1), as its lightmap ramps it.
fn a_curve(l: f32) -> f32 {
    return l / (4.0 - 3.0 * l);
}

fn a_terrain(input: MaterialInput) -> vec4<f32> {
    let base = input.albedo * input.tint * vec4<f32>(input.lightmap, 1.0);
    let strength = a_light_strength();
    let n = input.normal;
    var direct = 1.0;
    if (input.light.y > 0.0 && strength > 0.0) {
        direct = a_sun(input.position, n, input.screen * frame.resolution);
        // A block's faces turned from the light are in its own shadow; faces edge-on keep vanilla's side shading.
        // Plants' crossed quads are diagonal and lit from either side.
        if (max(abs(n.x), max(abs(n.y), abs(n.z))) > 0.99) {
            direct *= smoothstep(-0.35, 0.0, dot(n, frame.light_direction));
        }
    }
    // The sky's share of the light here (the rest is torchlight, which Natural Lighting leaves as it is).
    let day = 0.2 + 0.8 * smoothstep(-0.2, 0.3, frame.sun_direction.y);
    let sky = a_curve(input.light.y) * day;
    let torch = a_curve(input.light.x);
    let share = sky * sky / (sky * sky + torch * torch + 1.0e-4);
    let shaded = strength * (1.0 - direct);
    let warm = mix(vec3<f32>(1.0), a_light_tint(), strength * direct);
    let cool = mix(vec3<f32>(1.0), vec3<f32>(0.90, 0.95, 1.06), shaded);
    var lit = mix(vec3<f32>(1.0), mix(1.0, shade, shaded) * warm * cool, share);
    if (caustics && strength > 0.0 && direct > 0.0 && input.light.y > 0.0 && frame.has_shadow == 1u) {
        lit *= 1.0 + 0.6 * a_caustics(input.position) * direct * strength;
    }
    var c = base.rgb * lit;
    // held and dropped lights, mods' lights and glowing particles
    c += input.albedo.rgb * input.tint.rgb * input.colored_light * 0.6;
    return vec4<f32>(c, base.a);
}

// The slope of still water facing up: a few gentle swells crossing at different angles and speeds. `foot` is how
// much water one pixel covers (blocks): a swell too short to draw at that size fades out rather than alias into
// rings across distant water.
fn a_wave_normal(pos: vec3<f32>, foot: f32) -> vec3<f32> {
    let p = pos.xz + frame.camera_position.xz;
    let t = frame.time;
    // xy: direction, z: wave number (per block), w: speed
    let w = array<vec4<f32>, 4>(
        vec4<f32>(0.8, 0.6, 0.9, 1.1), vec4<f32>(-0.55, 0.835, 1.7, 1.5),
        vec4<f32>(0.2, -0.98, 2.9, 1.9), vec4<f32>(-0.92, -0.39, 4.3, 2.4),
    );
    var slope = vec2<f32>(0.0);
    for (var i = 0; i < 4; i += 1) {
        let k = w[i];
        // height a sin(k (d . p) + s t), whose slope is a k cos(...) d; taller swells are longer
        let a = 0.03 / k.z;
        let span = 6.2832 / k.z;
        let seen = 1.0 - smoothstep(span * 0.12, span * 0.35, foot);
        slope += k.xy * (a * k.z * seen * cos(k.z * dot(k.xy, p) + k.w * t));
    }
    return normalize(vec3<f32>(-slope.x, 1.0, -slope.y));
}

// The camera-relative position of what lies at screen `uv` and depth `d`.
fn a_view_pos(uv: vec2<f32>, d: f32) -> vec3<f32> {
    let v = frame.projection_inverse * vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, d, 1.0);
    return (frame.view_inverse * vec4<f32>(v.xyz / v.w, 0.0)).xyz;
}

fn a_water(input: MaterialInput) -> vec4<f32> {
    let color = input.albedo * input.tint * vec4<f32>(input.lightmap, 1.0);
    let pos = input.position;
    let size = vec2<f32>(textureDimensions(opaque_depth));
    let view = normalize(pos);
    let face = input.normal;
    let top = abs(face.y) > 0.9;
    var n = face;
    if (waves && top) {
        // one pixel's reach on the water: the view's spread over the screen's height, stretched at grazing angles
        let foot = length(pos) * 1.4 / frame.resolution.y / max(abs(view.y), 0.04);
        let w = a_wave_normal(pos, foot);
        n = vec3<f32>(w.x, w.y * sign(face.y), w.z);
    }
    let uv = input.screen;
    let px = vec2<i32>(clamp(uv * size, vec2<f32>(0.0), size - 1.0));
    // how deep the water is along the view, to what lies behind it (the sky counts as deep)
    let behind = textureLoad(opaque_depth, px, 0);
    var depth = 48.0;
    if (behind > 0.0) {
        depth = min(distance(a_view_pos(uv, behind), pos), 48.0);
    }
    // The world under the water, bent by the waves, more where it is deeper; unbent where the bent sample would be
    // something in front of the water. The bend is a fraction of a block wherever the water is, so distant kelp
    // and stone don't shift by blocks and flicker as the waves pass.
    let near = min(1.0, 12.0 / max(length(pos), 1.0));
    var ruv = uv + n.xz * 0.025 * near * clamp(depth / 3.0, 0.0, 1.0);
    let clip = frame.projection * (frame.view * vec4<f32>(pos, 1.0));
    let rpx = vec2<i32>(clamp(ruv * size, vec2<f32>(0.0), size - 1.0));
    var through_px = rpx;
    if (textureLoad(opaque_depth, rpx, 0) > clip.z / clip.w) {
        through_px = px;
        ruv = uv;
    }
    let through = textureLoad(opaque_color, through_px, 0).rgb;
    // clear in the shallows, the water's own colour over a dozen blocks or so
    let thick = 1.0 - exp(-depth / water_clarity);
    let body = mix(through, color.rgb, clamp(mix(color.a * 0.35, 0.85, thick), 0.0, 1.0));
    if (face.y < -0.9) {
        // from under the surface: what is above, through it
        return vec4<f32>(body, 1.0);
    }
    let cos_v = clamp(dot(-view, n), 0.0, 1.0);
    let fresnel = 0.02 + 0.98 * pow(1.0 - cos_v, 5.0);
    let r = reflect(view, n);
    // the sky, darker where it doesn't reach (the sky light level)
    var reflected = a_sky(smoothstep(-0.05, 0.5, r.y)) * smoothstep(0.0, 1.0, input.light.y);
    // the world mirrored about the water's plane, where this water lies on that plane
    let on_plane = abs(pos.y + frame.camera_position.y - frame.water_plane) < 0.6;
    if (reflections && top && frame.has_reflection == 1u && on_plane) {
        let m = textureSampleLevel(reflection, reflection_sampler, uv + n.xz * 0.03 * near, 0.0).rgb;
        reflected = m;
    }
    var out = mix(body, reflected, clamp(fresnel, 0.0, 0.85));
    let strength = a_light_strength();
    if (strength > 0.0 && input.light.y > 0.0) {
        // the sun's (or moon's) glint, where its light reaches the surface
        let h = normalize(frame.light_direction - view);
        let spec = pow(max(dot(n, h), 0.0), 900.0) * 1.2 * strength;
        let lit = a_sun(pos, face, uv * frame.resolution) * smoothstep(8.0 / 15.0, 1.0, input.light.y);
        out += a_light_tint() * spec * lit;
    }
    return vec4<f32>(out, 1.0);
}

fn material(input: MaterialInput) -> Surface {
    var s: Surface;
    if (input.fluid == 1u && input.translucent) {
        s.color = a_water(input);
    } else {
        s.color = a_terrain(input);
    }
    return s;
}
