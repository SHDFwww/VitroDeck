// 圓角矩形 —— 整套 UI 的主力。一個 draw call 可以畫幾千個。
//
// CSS 白送的 border-radius / box-shadow / border 在原生都要自己算。
// 這裡用 SDF（signed distance field）一次解決三件事：形狀、抗鋸齒、陰影，
// 因為它們本質上都是「離邊界多遠」的函數。

struct Globals {
    res: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct Instance {
    @location(0) rect: vec4<f32>,          // x, y, w, h（像素，左上為原點）
    @location(1) radius: vec4<f32>,        // tl, tr, br, bl
    @location(2) color: vec4<f32>,
    @location(3) uv: vec4<f32>,            // u0, v0, u1, v1；u1<=u0 代表不貼圖
    @location(4) shadow: vec4<f32>,        // dx, dy, blur, spread
    @location(5) shadow_color: vec4<f32>,
    @location(6) border: vec4<f32>,        // 寬度, 形狀, 飽和度, 旋轉（弧度）
    @location(7) border_color: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,         // 相對於矩形中心的像素座標
    @location(1) @interpolate(flat) i_rect: vec4<f32>,
    @location(2) @interpolate(flat) i_radius: vec4<f32>,
    @location(3) @interpolate(flat) i_color: vec4<f32>,
    @location(4) @interpolate(flat) i_uv: vec4<f32>,
    @location(5) @interpolate(flat) i_shadow: vec4<f32>,
    @location(6) @interpolate(flat) i_shadow_color: vec4<f32>,
    @location(7) @interpolate(flat) i_border: vec4<f32>,
    @location(8) @interpolate(flat) i_border_color: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    // 兩個三角形組成的矩形，不用 vertex buffer。
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let c = corners[vi];

    // 幾何要撐大到足以容納陰影，否則陰影會被自己的矩形切掉。
    let pad = inst.shadow.z * 1.5 + inst.shadow.w
            + max(abs(inst.shadow.x), abs(inst.shadow.y)) + 2.0;

    let half = inst.rect.zw * 0.5;
    let center = inst.rect.xy + half;
    let local = (c * 2.0 - 1.0) * (half + pad);
    // 旋轉只動頂點位置；片段那邊的 local 留在沒轉過的座標系，
    // 形狀、貼圖、陰影就整組跟著轉
    let a = inst.border.w;
    let cs = cos(a);
    let sn = sin(a);
    let px = center + vec2<f32>(local.x * cs - local.y * sn, local.x * sn + local.y * cs);

    var out: VsOut;
    // 像素座標 → NDC。y 要翻過來，畫面座標是往下長的。
    out.pos = vec4<f32>(px.x / g.res.x * 2.0 - 1.0, 1.0 - px.y / g.res.y * 2.0, 0.0, 1.0);
    out.local = local;
    out.i_rect = inst.rect;
    out.i_radius = inst.radius;
    out.i_color = inst.color;
    out.i_uv = inst.uv;
    out.i_shadow = inst.shadow;
    out.i_shadow_color = inst.shadow_color;
    out.i_border = inst.border;
    out.i_border_color = inst.border_color;
    return out;
}

// r 的排列是 (tl, tr, br, bl)，p 是相對中心、y 往下為正。
fn pick_radius(p: vec2<f32>, r: vec4<f32>) -> f32 {
    if (p.x < 0.0) {
        if (p.y < 0.0) { return r.x; }
        return r.w;
    }
    if (p.y < 0.0) { return r.y; }
    return r.z;
}

fn sd_round_box(p: vec2<f32>, b: vec2<f32>, r: vec4<f32>) -> f32 {
    let rad = min(pick_radius(p, r), min(b.x, b.y));
    let q = abs(p) - b + vec2<f32>(rad, rad);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - rad;
}

/// 等腰三角形，指向 +x。標準的三段線段距離取最小值，符號用繞向判定。
///
/// 為什麼不用字型的 `▶`：那個字在 Windows 會被 Segoe UI Emoji 接走，
/// 變成一顆藍色的表情符號塞在灰階介面裡。自己畫才控制得住外觀，
/// 而且跟圓角矩形共用同一條管線、同一套抗鋸齒。
fn sd_triangle(p: vec2<f32>, b: vec2<f32>) -> f32 {
    let p0 = vec2<f32>(-b.x, -b.y);
    let p1 = vec2<f32>(-b.x, b.y);
    let p2 = vec2<f32>(b.x, 0.0);
    let e0 = p1 - p0;
    let e1 = p2 - p1;
    let e2 = p0 - p2;
    let v0 = p - p0;
    let v1 = p - p1;
    let v2 = p - p2;
    let q0 = v0 - e0 * clamp(dot(v0, e0) / dot(e0, e0), 0.0, 1.0);
    let q1 = v1 - e1 * clamp(dot(v1, e1) / dot(e1, e1), 0.0, 1.0);
    let q2 = v2 - e2 * clamp(dot(v2, e2) / dot(e2, e2), 0.0, 1.0);
    let s = sign(e0.x * e2.y - e0.y * e2.x);
    let d = min(
        min(
            vec2<f32>(dot(q0, q0), s * (v0.x * e0.y - v0.y * e0.x)),
            vec2<f32>(dot(q1, q1), s * (v1.x * e1.y - v1.y * e1.x)),
        ),
        vec2<f32>(dot(q2, q2), s * (v2.x * e2.y - v2.y * e2.x)),
    );
    return -sqrt(d.x) * sign(d.y);
}

fn dot2(v: vec2<f32>) -> f32 {
    return dot(v, v);
}

/// 愛心（Inigo Quilez 的版本）。底尖在原點、往 +y 長，範圍約
/// x ∈ [-0.604, 0.604]、y ∈ [0, 1.104]。`♥` 這個字一樣會被
/// Segoe UI Emoji 接走變成紅色的表情符號，所以也自己畫。
fn sd_heart(p_in: vec2<f32>) -> f32 {
    let p = vec2<f32>(abs(p_in.x), p_in.y);
    if (p.y + p.x > 1.0) {
        return sqrt(dot2(p - vec2<f32>(0.25, 0.75))) - sqrt(2.0) / 4.0;
    }
    return sqrt(min(dot2(p - vec2<f32>(0.0, 1.0)), dot2(p - 0.5 * max(p.x + p.y, 0.0))))
        * sign(p.x - p.y);
}

/// 0→peak→0 的三角形波峰，用來畫 CSS gradient 那種「透明 → 亮 → 透明」的帶
fn band(x: f32, a: f32, peak: f32, b: f32) -> f32 {
    return clamp(min((x - a) / (peak - a), (b - x) / (b - peak)), 0.0, 1.0);
}

/// 黑膠唱片的顏色。照 Python 版 style.css 的 `#np-disc .grooves` 與 `.sheen`：
/// 底色 #121215、2.8px 一圈的溝紋、標籤外緣（64%）與外圈（96.5%）各一道亮線、
/// 最外緣壓暗，再疊一層固定在燈的方向上的反光（不跟唱片轉）。
fn vinyl(p: vec2<f32>, r: f32) -> vec3<f32> {
    let d = length(p);
    let f = d / r;
    // 溝紋。CSS 是硬邊的兩色帶，這裡用正弦：一樣的週期，小尺寸時不會閃
    let g = 0.5 + 0.5 * sin(d * 6.2831853 / 2.8);
    var c = mix(vec3<f32>(0.051, 0.051, 0.063), vec3<f32>(0.094, 0.094, 0.110), g);
    c = c + vec3<f32>(0.05) * band(f, 0.635, 0.64, 0.655);
    c = c + vec3<f32>(0.07) * band(f, 0.955, 0.965, 0.975);
    c = mix(c, vec3<f32>(0.039, 0.039, 0.047), smoothstep(0.975, 0.985, f));
    // 反光：CSS 的 conic-gradient(from 10deg) —— 角度從正上方順時針算
    let ang = atan2(p.x, -p.y) / 6.2831853;
    let t = fract(ang - 10.0 / 360.0);
    let sheen = 0.11 * band(t, 0.06, 0.11, 0.17) + 0.07 * band(t, 0.52, 0.60, 0.67);
    // 只落在溝紋區，不蓋到標籤
    let mask = smoothstep(0.63, 0.645, f);
    return c + vec3<f32>(sheen * mask);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let half = in.i_rect.zw * 0.5;
    // border.y 是形狀種類：0=圓角矩形 1=右指三角形 2=左指三角形 3=愛心 4=黑膠唱片。
    // 三角形的「圓角」用 radius.x 當內縮量，收出來的尖角才不會刺。
    let kind = in.i_border.y;
    var sd: f32;
    if (kind > 3.5) {
        sd = length(in.local) - half.x;
    } else if (kind > 2.5) {
        // 畫面座標 y 朝下，愛心的座標 y 朝上；整顆縮放到塞進矩形、置中
        let k = min(half.x / 0.604, half.y / 0.552);
        let hp = vec2<f32>(in.local.x, -in.local.y) / k + vec2<f32>(0.0, 0.552);
        sd = sd_heart(hp) * k;
    } else if (kind > 0.5) {
        let r = min(in.i_radius.x, min(half.x, half.y) * 0.8);
        let p = select(in.local, vec2<f32>(-in.local.x, in.local.y), kind > 1.5);
        sd = sd_triangle(p, half - vec2<f32>(r)) - r;
    } else {
        sd = sd_round_box(in.local, half, in.i_radius);
    }

    // 1 像素的抗鋸齒帶。fwidth 會讓它在縮放時也維持一像素寬。
    let aa = max(fwidth(sd), 0.0001);
    let body = 1.0 - smoothstep(-aa, aa, sd);

    // ---- 本體顏色 ----
    var rgb = in.i_color.rgb;
    var alpha = in.i_color.a;
    if (in.i_uv.z > in.i_uv.x) {
        let t = (in.local + half) / in.i_rect.zw;      // 0..1
        let uv = mix(in.i_uv.xy, in.i_uv.zw, t);
        var tx = textureSample(tex, samp, uv);
        // border.z = 飽和度倍率（0 代表不調整）。背景那張要 saturate(1.7)，
        // 不然模糊之後專輯的顏色會被平均掉，整片灰。
        if (in.i_border.z > 0.0) {
            let l = dot(tx.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
            tx = vec4<f32>(
                clamp(mix(vec3<f32>(l), tx.rgb, in.i_border.z), vec3<f32>(0.0), vec3<f32>(1.0)),
                tx.a,
            );
        }
        rgb = tx.rgb * in.i_color.rgb;
        alpha = tx.a * in.i_color.a;
    }
    if (kind > 3.5) {
        rgb = vinyl(in.local, half.x);
        alpha = 1.0;
    }

    // ---- 外框 ----
    let bw = in.i_border.x;
    if (bw > 0.0) {
        // 往內縮 bw 的那一圈才是外框
        let inner = sd + bw;
        let ring = (1.0 - smoothstep(-aa, aa, inner)) ;
        let edge = body - ring;                         // 0..1，只在框上是 1
        rgb = mix(rgb, in.i_border_color.rgb, edge * in.i_border_color.a);
        alpha = max(alpha, edge * in.i_border_color.a);
    }

    // ---- 陰影 ----
    // 位移之後重算一次距離，再用 blur 當作 smoothstep 的寬度近似高斯。
    //
    // 這裡一定要**預乘** alpha。管線用的是 PREMULTIPLIED_ALPHA_BLENDING，
    // 直接輸出直通色的話 src.rgb 不會被 src.a 縮放，半透明會全部變成不透明。
    var out = vec4<f32>(rgb * alpha, alpha) * body;
    let blur = max(in.i_shadow.z, 0.001);
    if (in.i_shadow_color.a > 0.0) {
        let sp = in.local - in.i_shadow.xy;
        let sds = sd_round_box(sp, half + in.i_shadow.w, in.i_radius);
        let sa = (1.0 - smoothstep(-blur, blur, sds)) * in.i_shadow_color.a;
        let shadow = vec4<f32>(in.i_shadow_color.rgb, 1.0) * sa;
        // 陰影墊在本體底下
        out = out + shadow * (1.0 - out.a);
    }
    return out;
}
