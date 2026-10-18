// MilkDrop 的三條管線：warp（變形網格取樣上一幀）、wave（波形）、comp（合成）。

struct Uni {
    misc: vec4<f32>,   // warp: x=decay ／ comp: x=gamma y=brighten z=darken w=solarize
    res: vec4<f32>,    // comp: xy=尺寸 z=invert w=fshader
    echo: vec4<f32>,   // comp: x=echo_alpha y=echo_zoom z=echo_orient
    hue: array<vec4<f32>, 4>,
};

@group(0) @binding(0) var<uniform> u: Uni;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

// ---------------------------------------------------------------- warp

struct WarpOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_warp(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>) -> WarpOut {
    var o: WarpOut;
    o.pos = vec4<f32>(pos, 0.0, 1.0);
    o.uv = uv;
    return o;
}

@fragment
fn fs_warp(in: WarpOut) -> @location(0) vec4<f32> {
    // 取樣上一幀再乘 decay。這個乘法就是「殘影會慢慢淡掉」的全部原理。
    let c = textureSample(tex, samp, in.uv);
    return vec4<f32>(c.rgb * u.misc.x, 1.0);
}

// ---------------------------------------------------------------- wave

struct WaveOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_wave(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> WaveOut {
    var o: WaveOut;
    o.pos = vec4<f32>(pos, 0.0, 1.0);
    o.color = color;
    return o;
}

@fragment
fn fs_wave(in: WaveOut) -> @location(0) vec4<f32> {
    return in.color;
}

// ------------------------------------------------- 貼圖形狀
//
// MilkDrop 的 custom shape 可以把**上一幀的畫面**貼在自己身上
// （396 個啟用的形狀裡有 174 個是這樣）。少了它那些 preset 只剩純色多邊形。

struct ShapeTexOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
};

@vertex
fn vs_shape_tex(
    @location(0) pos: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
) -> ShapeTexOut {
    var o: ShapeTexOut;
    o.pos = vec4<f32>(pos, 0.0, 1.0);
    o.color = color;
    o.uv = uv;
    return o;
}

@fragment
fn fs_shape_tex(in: ShapeTexOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv) * in.color;
}

// ---------------------------------------------------------------- comp

struct FullOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> FullOut {
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) & 1) * 4.0 - 1.0;
    var o: FullOut;
    o.pos = vec4<f32>(x, y, 0.0, 1.0);
    o.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return o;
}

// hue_shader：四個角各一個顏色，中間雙線性內插。
// 原版是逐頂點算在 32x24 的網格上，雙線性的結果一樣，直接在片段算就好。
fn hue_at(uv: vec2<f32>) -> vec3<f32> {
    let x = uv.x;
    let y = uv.y;
    return u.hue[0].rgb * x * y
         + u.hue[1].rgb * (1.0 - x) * y
         + u.hue[2].rgb * x * (1.0 - y)
         + u.hue[3].rgb * (1.0 - x) * (1.0 - y);
}

// preset 沒有自己的 comp 著色器時走這裡。內容照抄 butterchurn
// 在 comp 留空時塞的那段預設程式，順序不能動 ——
// gamma 在 hue 之前、brighten 那四個在最後，換了順序顏色就不一樣。
@fragment
fn fs_comp(in: FullOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    // 視訊回音：同一張畫面放大／翻轉一次再疊上來
    let orient_horiz = u.echo.z % 2.0;
    let ox = select(1.0, -1.0, orient_horiz != 0.0);
    let oy = select(1.0, -1.0, u.echo.z >= 2.0);
    let uv_echo = (uv - 0.5) * (1.0 / u.echo.y) * vec2<f32>(ox, oy) + 0.5;
    var c = mix(
        textureSample(tex, samp, uv).rgb,
        textureSample(tex, samp, uv_echo).rgb,
        u.echo.x,
    );
    // gammaadj 在 MilkDrop 裡是「整體提亮」的乘數，不是真的做 gamma 曲線。
    c = c * u.misc.x;
    let f = u.res.w;
    if (f > 0.001) {
        let h = hue_at(uv);
        if (f >= 1.0) {
            c = c * h;
        } else {
            c = c * ((1.0 - f) + f * h);
        }
    }
    if (u.misc.y != 0.0) { c = sqrt(c); }
    if (u.misc.z != 0.0) { c = c * c; }
    if (u.misc.w != 0.0) { c = c * (1.0 - c) * 4.0; }
    if (u.res.z != 0.0) { c = 1.0 - c; }
    return vec4<f32>(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

// ---------------------------------------------------------------- preset 著色器用的頂點階段
//
// 這兩支的 @location 必須跟 naga 從 GLSL 翻出來的 fragment 對得上：
//   warp: 0=uv 1=uv_orig 2=vColor 3=hue_shader
//   comp: 0=vUv 1=vColor 2=hue_shader

// preset 著色器的 Globals（binding 20）。頂點階段只要尾巴那四個角的顏色，
// 前面 496 個位元組（= 31 個 vec4）原樣跳過就好。
struct PresetGlobals {
    head: array<vec4<f32>, 31>,
    hue: array<vec4<f32>, 4>,
};
@group(0) @binding(20) var<uniform> pg: PresetGlobals;

fn preset_hue(uv: vec2<f32>) -> vec3<f32> {
    let x = uv.x;
    let y = uv.y;
    return pg.hue[0].rgb * x * y
         + pg.hue[1].rgb * (1.0 - x) * y
         + pg.hue[2].rgb * x * (1.0 - y)
         + pg.hue[3].rgb * (1.0 - x) * (1.0 - y);
}

struct WarpShaderOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) uv_orig: vec2<f32>,
    @location(2) v_color: vec4<f32>,
    @location(3) hue: vec3<f32>,
};

@vertex
fn vs_warp_shader(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>) -> WarpShaderOut {
    var o: WarpShaderOut;
    o.pos = vec4<f32>(pos, 0.0, 1.0);
    o.uv = uv;
    // uv_orig 是「這個頂點原本在畫面上的位置」，沒有被變形過
    o.uv_orig = vec2<f32>(pos.x * 0.5 + 0.5, 0.5 - pos.y * 0.5);
    o.v_color = vec4<f32>(1.0);
    o.hue = vec3<f32>(1.0);
    return o;
}

struct CompShaderOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) v_color: vec4<f32>,
    @location(2) hue: vec3<f32>,
};

@vertex
fn vs_comp_shader(@builtin(vertex_index) vi: u32) -> CompShaderOut {
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) & 1) * 4.0 - 1.0;
    var o: CompShaderOut;
    o.pos = vec4<f32>(x, y, 0.0, 1.0);
    o.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    o.v_color = vec4<f32>(1.0);
    // 43 個 preset 的 comp 著色器會乘上 hue_shader —— 給白色的話
    // 它們的顏色循環整個不見，畫面會偏灰。
    o.hue = preset_hue(o.uv);
    return o;
}

// ---------------------------------------------------------------- mipmap
//
// 原版每幀對主貼圖 `generateMipmap`，warp 取樣用的是 LINEAR_MIPMAP_LINEAR。
// 沒有 mip 的話 preset 一縮小取樣就會鋸齒閃爍。wgpu 沒有內建的產生器，
// 只能自己一級一級往下 blit：四個來源 texel 平均成一個。

@fragment
fn fs_mip(in: FullOut) -> @location(0) vec4<f32> {
    let d = 0.25 / vec2<f32>(textureDimensions(tex));
    var c = textureSample(tex, samp, in.uv + vec2<f32>(-d.x, -d.y));
    c = c + textureSample(tex, samp, in.uv + vec2<f32>(d.x, -d.y));
    c = c + textureSample(tex, samp, in.uv + vec2<f32>(-d.x, d.y));
    c = c + textureSample(tex, samp, in.uv + vec2<f32>(d.x, d.y));
    return c * 0.25;
}

// ---------------------------------------------------------------- blur 金字塔

@fragment
fn fs_blur(in: FullOut) -> @location(0) vec4<f32> {
    // 取樣間距用來源的 texel，每級只降一半，所以這是溫和的模糊。
    let d = 1.0 / vec2<f32>(textureDimensions(tex));
    var sum = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var j = -2; j <= 2; j = j + 1) {
        for (var i = -2; i <= 2; i = i + 1) {
            let o = vec2<f32>(f32(i), f32(j));
            // 高斯權重，邊角比中心輕
            let w = exp(-dot(o, o) * 0.35);
            sum = sum + textureSample(tex, samp, in.uv + o * d).rgb * w;
            wsum = wsum + w;
        }
    }
    return vec4<f32>(sum / wsum, 1.0);
}
