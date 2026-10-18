// M0 的驗證畫面：對數頻譜長條 + 一點輝光。
// 目的是把「uniform 上傳 → shader 讀到 → 畫出來」這條路走通，不是美術。

struct Uniforms {
    // uniform 位址空間裡 array 的每個元素會被墊到 16 bytes，
    // 所以 128 個 f32 要用 32 個 vec4 裝，不能寫 array<f32, 128>。
    bins: array<vec4<f32>, 32>,
    level: vec4<f32>,   // x=bass y=mid z=treb w=alive
    res:   vec4<f32>,   // xy=畫面尺寸（像素）
};

@group(0) @binding(0) var<uniform> u: Uniforms;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // 全螢幕三角形，不用 vertex buffer。
    let x = f32(i32(i) / 2) * 4.0 - 1.0;
    let y = f32(i32(i) & 1) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

fn bin(i: i32) -> f32 {
    return u.bins[i / 4][i % 4];
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    var uv = pos.xy / u.res.xy;
    uv.y = 1.0 - uv.y;

    let idx = i32(uv.x * 128.0);
    let h = bin(clamp(idx, 0, 127));

    // 長條本體
    let bar = step(uv.y, h);
    // 頂端亮線
    let cap = smoothstep(0.012, 0.0, abs(uv.y - h));

    let hue = uv.x;
    let col = vec3<f32>(
        0.5 + 0.5 * cos(6.2831 * (hue + 0.00)),
        0.5 + 0.5 * cos(6.2831 * (hue + 0.33)),
        0.5 + 0.5 * cos(6.2831 * (hue + 0.67)),
    );

    var rgb = col * bar * 0.85 + col * cap * 1.2;
    // 底部依 bass 給一層暈，順便驗證 level 這條路也通了
    rgb += col * 0.12 * u.level.x * smoothstep(0.35, 0.0, uv.y);

    // 沒訊號時壓暗，好分辨「沒聲音」跟「沒接上」
    rgb *= mix(0.25, 1.0, u.level.w);
    return vec4<f32>(rgb, 1.0);
}
