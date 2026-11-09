// Natural Lighting: helpers every stage shares.
//
// Big Skies tells the pack about its sky through `frame.mods` (its native build's
// `shader_params` hook): mods[0] = how much of the sun's (or moon's) light gets through the clouds where the player
// is (0..1), then the light's colour; mods[1] = the sky's colour at the horizon, and 1 when Skies is there; mods[2] =
// the sky's colour overhead. Without Skies they are 0 and Natural Lighting goes by vanilla's rain and thunder.

fn a_luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// Interleaved gradient noise: a different value per pixel, against banding.
fn a_noise(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715))));
}

fn a_skies() -> bool {
    return frame.mods[1].w > 0.5;
}

// How strongly the sun (or by night the moon, far less) lights the world where the player is, 0..1: low suns
// fade, and clouds (Skies'), or vanilla's rain and thunder, take their share.
fn a_light_strength() -> f32 {
    if (frame.dimension != 0) {
        return 0.0;
    }
    let s = frame.sun_direction.y;
    var k = select(0.35 * smoothstep(0.02, 0.2, -s), smoothstep(0.02, 0.2, s), s > 0.0);
    if (a_skies()) {
        k *= clamp(frame.mods[0].x, 0.0, 1.0);
    } else {
        k *= (1.0 - 0.85 * frame.rain) * (1.0 - 0.5 * frame.thunder);
    }
    return k;
}

// The light's colour where it falls: warmer as the sun nears the horizon; the moon's is cool.
fn a_light_tint() -> vec3<f32> {
    let sun = frame.sun_direction;
    var t = vec3<f32>(0.94, 0.97, 1.04);
    if (sun.y > 0.0) {
        let low = 1.0 - smoothstep(0.1, 0.6, sun.y);
        t = mix(vec3<f32>(1.0, 0.99, 0.96), vec3<f32>(1.08, 0.94, 0.82), low);
    }
    if (a_skies()) {
        t *= clamp(frame.mods[0].yzw, vec3<f32>(0.0), vec3<f32>(2.0));
    }
    return t;
}

// The sky's colour along a direction `up` (0 at the horizon, 1 overhead): Skies' own when it is there, else
// vanilla's sky and fog colours.
fn a_sky(up: f32) -> vec3<f32> {
    if (a_skies()) {
        return mix(frame.mods[1].xyz, frame.mods[2].xyz, up);
    }
    return mix(frame.fog_color, frame.sky_color, up);
}
