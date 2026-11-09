// Bloom, second step, at quarter size: the bright light spread wide, a 13-tap blur of the half-size one over
// about twelve of the world's pixels each way.

fn post(input: PostInput) -> vec4<f32> {
    let t = 2.0 / frame.resolution;
    var c = textureSampleLevel(bloom_half, linear_sampler, input.uv, 0.0).rgb * 0.125;
    let ring_a = array<vec2<f32>, 4>(vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, 1.0));
    let ring_b = array<vec2<f32>, 8>(
        vec2<f32>(-2.0, -2.0), vec2<f32>(0.0, -2.0), vec2<f32>(2.0, -2.0), vec2<f32>(-2.0, 0.0),
        vec2<f32>(2.0, 0.0), vec2<f32>(-2.0, 2.0), vec2<f32>(0.0, 2.0), vec2<f32>(2.0, 2.0),
    );
    for (var i = 0; i < 4; i += 1) {
        c += textureSampleLevel(bloom_half, linear_sampler, input.uv + ring_a[i] * t, 0.0).rgb * 0.125;
    }
    for (var i = 0; i < 8; i += 1) {
        let w = select(0.0625, 0.03125, i == 0 || i == 2 || i == 5 || i == 7);
        c += textureSampleLevel(bloom_half, linear_sampler, input.uv + ring_b[i] * 2.0 * t, 0.0).rgb * w;
    }
    return vec4<f32>(c, 1.0);
}
