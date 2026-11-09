// Sunrays: the bright sky around the sun, blurred outward from the sun's place on the screen, only where the
// world partly blocks it (shafts through leaves, past hills and between clouds); open sky keeps its own glow.

override sunrays: f32 = 0.7;

// How much light a pixel gives the rays (x: bright sky only), and whether the world is in the way there (y: 1).
fn a_ray_source(uv: vec2<f32>) -> vec2<f32> {
    if (depth_at(uv) > 0.0) {
        return vec2<f32>(0.0, 1.0);
    }
    let c = textureSampleLevel(previous, linear_sampler, uv, 0.0).rgb;
    return vec2<f32>(smoothstep(0.6, 1.0, a_luma(c)), 0.0);
}

fn post(input: PostInput) -> vec4<f32> {
    let sun = frame.sun_direction;
    if (sunrays <= 0.0 || frame.dimension != 0 || sun.y <= 0.0 || frame.eye_in != 0u) {
        return input.color;
    }
    let v = frame.view * vec4<f32>(sun, 0.0);
    let clip = frame.projection * vec4<f32>(v.xyz, 0.0);
    if (clip.w <= 0.0) {
        return input.color;
    }
    let ndc = clip.xy / clip.w;
    let at = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    // fading as the sun leaves the screen
    let off = max(abs(ndc.x), abs(ndc.y));
    let fade = 1.0 - smoothstep(1.0, 1.6, off);
    let strength = sunrays * a_light_strength() * fade * fade;
    if (strength <= 0.001) {
        return input.color;
    }
    let to_sun = at - input.uv;
    let samples = 24;
    let stride = to_sun * (0.85 / f32(samples));
    var p = input.uv + stride * a_noise(input.pixel);
    var light = 0.0;
    var blocked = 0.0;
    var total = 0.0;
    var weight = 1.0;
    for (var i = 0; i < samples; i += 1) {
        let s = a_ray_source(p);
        light += weight * s.x;
        blocked += s.y;
        total += weight;
        weight *= 0.97;
        p += stride;
    }
    let shafts = smoothstep(0.0, 0.35, blocked / f32(samples));
    let aspect = frame.resolution.x / frame.resolution.y;
    let dist = length(to_sun * vec2<f32>(aspect, 1.0));
    let r = light / total * shafts * strength * exp(-dist * 1.2);
    return vec4<f32>(input.color.rgb + a_light_tint() * r, input.color.a);
}
