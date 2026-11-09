// The picture with the bloom added (in linear light, near and wide), then a gentle S-curve and a touch more colour,
// and half a step of noise against banding in the 8-bit picture.

override bloom: f32 = 1.0;
override saturation: f32 = 1.05;
override contrast: f32 = 0.08;

fn post(input: PostInput) -> vec4<f32> {
    let base = input.color;
    let day = clamp(a_light_strength(), 0.0, 1.0);
    let strength = bloom * (1.0 - 0.68 * day) * 0.5;
    let near = textureSampleLevel(bloom_half, linear_sampler, input.uv, 0.0).rgb;
    let wide = textureSampleLevel(bloom_quarter, linear_sampler, input.uv, 0.0).rgb;
    let added = (near * 0.4 + wide * 0.6) * strength;
    var c = pow(min(pow(max(base.rgb, vec3<f32>(0.0)), vec3<f32>(2.2)) + added, vec3<f32>(1.0)), vec3<f32>(1.0 / 2.2));
    c = mix(c, c * c * (3.0 - 2.0 * c), contrast);
    c = max(mix(vec3<f32>(a_luma(c)), c, saturation), vec3<f32>(0.0));
    let dither = a_noise(input.pixel) - 0.5;
    return vec4<f32>(clamp(c + dither / 255.0, vec3<f32>(0.0), vec3<f32>(1.0)), base.a);
}
