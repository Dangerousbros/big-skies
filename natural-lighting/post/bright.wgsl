// Bloom, first step, at half size: the bright part of the picture (in linear light), with a soft knee so it fades
// in. Each of four taps is weighed down by its brightness, so a lone bright pixel can't flicker into a big glow
// (Karis). The sky gives less: its glow is its own.

override bloom: f32 = 1.0;

fn a_bright(c: vec3<f32>, threshold: f32) -> vec3<f32> {
    let k = 0.2;
    let b = max(c.r, max(c.g, c.b));
    var soft = clamp(b - threshold + k, 0.0, 2.0 * k);
    soft = soft * soft / (4.0 * k + 1.0e-4);
    return c * (max(soft, b - threshold) / max(b, 1.0e-4));
}

fn post(input: PostInput) -> vec4<f32> {
    if (bloom <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    // softer and higher by day, when far more of the picture is bright
    let day = clamp(a_light_strength(), 0.0, 1.0);
    let threshold = 0.5 + 0.25 * day;
    let texel = 1.0 / frame.resolution;
    var sum = vec3<f32>(0.0);
    var weights = 0.0;
    for (var i = 0; i < 4; i += 1) {
        let o = vec2<f32>(f32(i & 1) * 2.0 - 1.0, f32(i >> 1u) * 2.0 - 1.0) * texel;
        let uv = input.uv + o;
        var c = pow(max(textureSampleLevel(previous, linear_sampler, uv, 0.0).rgb, vec3<f32>(0.0)), vec3<f32>(2.2));
        if (depth_at(uv) == 0.0) {
            c *= select(0.6, 0.3, a_skies());
        }
        c = a_bright(c, threshold);
        let w = 1.0 / (1.0 + a_luma(c));
        sum += c * w;
        weights += w;
    }
    return vec4<f32>(sum / weights, 1.0);
}
