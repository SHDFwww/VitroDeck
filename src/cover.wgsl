// 封面流的透視四邊形。
//
// CSS 版是容器上掛 `perspective: 1100px`，每張封面套
// `translate3d(x,0,z) rotateY(rot) scale(s)`。這裡照同一套數學算，
// 所以外觀跟原版對得起來。
//
// 只繞 Y 軸轉的話不需要完整的 4x4 矩陣：
//   縮放  → (s·lx, s·ly, 0)
//   繞 Y  → (s·lx·cosθ, s·ly, −s·lx·sinθ)
//   位移  → (tx + …, s·ly, tz − …)
//
// 投影用 w = (d − z)/d，再把 `vec4(ndc·w, 0, w)` 交給光柵器 ——
// 它會自己做透視正確的內插，貼圖才不會在側邊的封面上被拉歪。

struct Globals {
    res: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct Instance {
    @location(0) xform: vec4<f32>,   // tx, tz, scale, rot(弧度)
    @location(1) size: vec4<f32>,    // w, h, 圓角, 透視距離 d
    @location(2) tint: vec4<f32>,
    @location(3) extra: vec4<f32>,   // cx, cy, 是否為倒影, 倒影長度比例
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) local: vec2<f32>,
    @location(2) @interpolate(flat) size: vec4<f32>,
    @location(3) @interpolate(flat) tint: vec4<f32>,
    @location(4) @interpolate(flat) extra: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let c = corners[vi];

    let w = inst.size.x;
    let h = inst.size.y;
    let is_reflect = inst.extra.z > 0.5;
    let fade_len = inst.extra.w;

    // 本體：local y 從 −h/2 到 +h/2。
    // 倒影：接在下緣往下長，而且 uv.y 翻過來 —— 跟 CSS 的 box-reflect 一樣
    // 是在元素自己的平面上鏡射，所以會跟著一起被透視變形。
    var ly: f32;
    var uvy: f32;
    if (is_reflect) {
        ly = h * 0.5 + c.y * h * fade_len + 2.0;   // CSS 那邊是 below 2px
        uvy = 1.0 - c.y * fade_len;
    } else {
        ly = (c.y - 0.5) * h;
        uvy = c.y;
    }
    let lx = (c.x - 0.5) * w;

    let s = inst.xform.z;
    let rot = inst.xform.w;
    let cs = cos(rot);
    let sn = sin(rot);

    let px = inst.xform.x + s * lx * cs;
    let py = s * ly;
    let pz = inst.xform.y - s * lx * sn;

    let d = inst.size.w;
    let pw = (d - pz) / d;
    let screen = inst.extra.xy + vec2<f32>(px, py) / pw;

    let ndc = vec2<f32>(screen.x / g.res.x * 2.0 - 1.0, 1.0 - screen.y / g.res.y * 2.0);

    var out: VsOut;
    // 乘回 w 交給光柵器除，這樣 varying 才是透視正確的。
    out.pos = vec4<f32>(ndc * pw, 0.0, pw);
    out.uv = vec2<f32>(c.x, uvy);
    out.local = vec2<f32>(lx, (c.y - 0.5) * h);
    out.size = inst.size;
    out.tint = inst.tint;
    out.extra = inst.extra;
    return out;
}

fn sd_round_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let rad = min(r, min(b.x, b.y));
    let q = abs(p) - b + vec2<f32>(rad, rad);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - rad;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    var col = textureSample(tex, samp, in.uv);
    col = col * in.tint;

    // 圓角在封面自己的平面上算，所以側面的封面圓角也不會被拉歪。
    let half = in.size.xy * 0.5;
    let sd = sd_round_box(in.local, half, in.size.z);
    let aa = max(fwidth(sd), 0.5);
    var a = col.a * (1.0 - smoothstep(-aa, aa, sd));

    if (in.extra.z > 0.5) {
        // 倒影：往下越遠越淡，而且整體壓暗。二次方比線性更接近玻璃桌面。
        // uv.y 在倒影裡是從 1.0（貼著封面下緣）往 1−fade_len 遞減，
        // 所以 t=1 是最靠近封面的那端 —— 漸層要以它為最亮，別寫反了。
        let t = clamp((in.uv.y - (1.0 - in.extra.w)) / max(in.extra.w, 0.001), 0.0, 1.0);
        let fade = pow(t, 2.2) * 0.38;
        a = a * fade;
        col = vec4<f32>(col.rgb * 0.82, col.a);
    }

    return vec4<f32>(col.rgb * a, a);
}
