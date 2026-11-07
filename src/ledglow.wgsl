// 燈條追光：整幀縮成 GW×GH 格，每格是那一塊的平均色。全螢幕三角形，不用 vertex buffer。
// 格數要跟 ledglow.rs 的 GW、GH 一樣

const GW: f32 = 64.0;
const GH: f32 = 32.0;
// 每格取 TAPS×TAPS 個雙線性取樣，每個本身是 2×2 的平均
const TAPS: i32 = 8;

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) & 1) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let cell = vec2<f32>(1.0 / GW, 1.0 / GH);
    let origin = floor(pos.xy) * cell;
    var sum = vec3<f32>(0.0);
    for (var j = 0; j < TAPS; j++) {
        for (var i = 0; i < TAPS; i++) {
            let f = (vec2<f32>(f32(i), f32(j)) + 0.5) / f32(TAPS);
            sum += textureSampleLevel(src, samp, origin + f * cell, 0.0).rgb;
        }
    }
    return vec4<f32>(sum / f32(TAPS * TAPS), 1.0);
}
