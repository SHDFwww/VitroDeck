// 文字 PV：SDF 字形與幾何裝飾，全部吃同一個鏡頭（縮放、旋轉、晃動）。
//
// 字形的材質是距離場（pv/sdf.rs）：0.5 是字的邊，往上是字裡面，±SPREAD 個圖集像素。
// 放大多少都是同一張圖，描邊與光暈只是換門檻 —— 所以字可以任意放大、旋轉、傾斜。
// 矩形、圓環、圓盤不讀材質，距離直接算，跟字形共用後面那一段上色。

struct Cam {
    // 邏輯解析度、鏡頭的支點
    a: vec4<f32>,
    // 縮放、旋轉（弧度）、位移
    b: vec4<f32>,
    // 實體像素／邏輯像素、SDF 延伸量（圖集像素）、進出遮罩的邊界與漸變寬（見 `fs`）
    c: vec4<f32>,
};

@group(0) @binding(0) var<uniform> cam: Cam;
@group(0) @binding(1) var sdf: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct Inst {
    @location(0) pos: vec4<f32>,   // 中心 x, y、半寬、半高（邏輯 px）
    @location(1) uv: vec4<f32>,    // 圖集裡的 u0, v0, u1, v1
    @location(2) color: vec4<f32>, // 填色（直通 alpha）
    @location(3) line: vec4<f32>,  // 描邊色 rgb、描邊寬（邏輯 px）
    @location(4) xf: vec4<f32>,    // 旋轉、傾斜、每圖集像素多少實體 px、形狀（0 字 1 矩形 2 圓環 3 圓盤 4 漸層矩形 5 柔光條 6 暗角 7 顆粒 8 網點）
    @location(5) fx: vec4<f32>,    // 光暈寬（邏輯 px）、光暈強度、填色倍率（空心字 0）、不受鏡頭影響（1）
    @location(6) ex: vec4<f32>,    // 失焦（邏輯 px）、光澤掃到哪（-1.5..1.5）、光澤強度、由下往上露出的進度（0 是不遮）
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) half_size: vec2<f32>,
    @location(3) @interpolate(flat) color: vec4<f32>,
    @location(4) @interpolate(flat) line: vec4<f32>,
    @location(5) @interpolate(flat) xf: vec4<f32>,
    @location(6) @interpolate(flat) fx: vec4<f32>,
    // 漸層矩形右端的顏色（借 uv 那格傳）
    @location(7) @interpolate(flat) c2: vec4<f32>,
    @location(8) @interpolate(flat) ex: vec4<f32>,
};

fn rot2(p: vec2<f32>, a: f32) -> vec2<f32> {
    let s = sin(a);
    let c = cos(a);
    return vec2<f32>(p.x * c - p.y * s, p.x * s + p.y * c);
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Inst) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let c = corners[vi];
    let half_size = inst.pos.zw;
    // 字形的格子裡本來就留了 SPREAD；幾何形狀要自己撐出描邊、光暈與抗鋸齒的空間
    var pad = 0.0;
    if (inst.xf.w > 0.5) {
        pad = inst.line.w + inst.fx.x * 3.0 + inst.ex.x * 1.5 + 2.0;
    }
    var local = (c * 2.0 - 1.0) * (half_size + vec2<f32>(pad, pad));
    var p = local;
    p.x = p.x + inst.xf.y * p.y;
    p = rot2(p, inst.xf.x) + inst.pos.xy;
    if (inst.fx.w < 0.5) {
        let pivot = cam.a.zw;
        p = pivot + rot2(p - pivot, cam.b.y) * cam.b.x + cam.b.zw;
    }

    var out: VsOut;
    out.pos = vec4<f32>(p.x / cam.a.x * 2.0 - 1.0, 1.0 - p.y / cam.a.y * 2.0, 0.0, 1.0);
    out.local = local;
    out.uv = mix(inst.uv.xy, inst.uv.zw, c);
    out.half_size = half_size;
    out.color = inst.color;
    out.line = inst.line;
    out.xf = inst.xf;
    out.fx = inst.fx;
    out.c2 = inst.uv;
    out.ex = inst.ex;
    return out;
}

// 整數雜湊（顆粒用）。sin 那種寫法在大座標會出條紋
fn hash3(x: u32, y: u32, z: u32) -> u32 {
    var h = x * 374761393u + y * 668265263u + z * 2246822519u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    return h ^ (h >> 16u);
}

fn sd_box(p: vec2<f32>, b: vec2<f32>) -> f32 {
    let q = abs(p) - b;
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0);
}

// 整層進出的柔邊遮罩：只留下 x 在 c.z 右邊的部分，邊界前後各 c.w 漸變（邏輯 px）。
// c.w 是 0 就是沒在進出、整層都留
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = shade(in);
    if (cam.c.w <= 0.0) {
        return c;
    }
    let x = in.pos.x / cam.c.x;
    return c * smoothstep(cam.c.z - cam.c.w, cam.c.z + cam.c.w, x);
}

fn shade(in: VsOut) -> vec4<f32> {
    let k = cam.c.x;
    let mode = in.xf.w;
    // d：離邊界多少實體 px，往裡面為正
    var d: f32;
    if (mode < 0.5) {
        let v = textureSample(sdf, samp, in.uv).r;
        d = (v - 0.5) * 2.0 * cam.c.y * in.xf.z;
    } else if (mode < 1.5) {
        d = -sd_box(in.local, in.half_size) * k;
    } else if (mode < 2.5) {
        // 圓環：半徑是半寬，環寬是描邊寬
        d = -(abs(length(in.local) - in.half_size.x) - in.line.w * 0.5) * k;
    } else if (mode < 3.5) {
        d = -(length(in.local) - in.half_size.x) * k;
    } else if (mode < 4.5) {
        // 漸層矩形：左邊 color、右邊 c2（都是直通 alpha），中間 ±line.w 那一段平滑過渡
        // （0 就是整條都在過渡）。預乘之後才混，一邊透明的時候顏色才不會被拉黑
        let inside = clamp(-sd_box(in.local, in.half_size) * k + 0.5, 0.0, 1.0);
        let r = select(in.half_size.x, in.line.w, in.line.w > 0.0);
        let s = smoothstep(-r, r, in.local.x);
        let c = mix(vec4<f32>(in.color.rgb * in.color.a, in.color.a), vec4<f32>(in.c2.rgb * in.c2.a, in.c2.a), s);
        return c * inside;
    } else if (mode < 5.5) {
        // 柔光條：橫切面是高斯（半高大約是光的半徑），長的方向兩頭淡掉。
        // line.x 是 0 就兩頭對稱；大於 0 是彗星，頭在 +x 端、往 -x 拖一條尾巴，越大尾巴越短
        let u = in.local.x / in.half_size.x;
        let v = in.local.y / in.half_size.y;
        let across = exp(-v * v * 4.0);
        var along = 1.0 - smoothstep(0.25, 1.0, abs(u));
        if (in.line.x > 0.0) {
            along = pow(clamp((u + 1.0) * 0.5, 0.0, 1.0), in.line.x) * (1.0 - smoothstep(0.8, 1.0, u));
        }
        let a = across * along * in.color.a;
        return vec4<f32>(in.color.rgb * a, a);
    } else if (mode < 6.5) {
        // 暗角：中間透明、往四角慢慢暗下去（橢圓，跟著畫面比例）
        let r = length(in.local / in.half_size);
        let a = smoothstep(0.45, 1.5, r) * in.color.a;
        return vec4<f32>(in.color.rgb * a, a);
    } else if (mode < 7.5) {
        // 底片顆粒：大約 1.5 個邏輯 px 一顆，各自亮一點或暗一點；line.y 換一組
        let cell = vec2<u32>(in.pos.xy / max(k * 1.5, 1.0));
        let n = f32(hash3(cell.x, cell.y, u32(in.line.y)) & 0xffffu) / 65535.0 * 2.0 - 1.0;
        let a = abs(n) * in.color.a;
        let c = select(vec3<f32>(0.0), vec3<f32>(1.0), n > 0.0);
        return vec4<f32>(c * a, a);
    } else {
        // 網點：45° 斜的點陣，點距 line.x 邏輯 px；圓心的點最大（佔格子 line.y 成）、往外縮到沒有，
        // 所以邊緣沒有硬切的線。line.z 讓整片點陣斜著平移
        let sp = max(in.line.x, 2.0);
        let p = rot2(in.pos.xy / k, 0.7853982) + vec2<f32>(in.line.z, 0.0);
        let cell = (fract(p / sp) - 0.5) * sp;
        let g = clamp(1.0 - length(in.local) / max(in.half_size.x, 1.0), 0.0, 1.0);
        let r = sp * 0.5 * in.line.y * pow(g, 0.8);
        let a = clamp((r - length(cell)) * k + 0.5, 0.0, 1.0) * in.color.a;
        return vec4<f32>(in.color.rgb * a, a);
    }

    var ow = 0.0;
    if (mode < 0.5 || mode > 2.5 || (mode > 0.5 && mode < 1.5)) {
        ow = in.line.w * k;
    }
    // 抗鋸齒一個實體 px；鏡頭放大時邊緣跟著變寬一點也看不出來。
    // 失焦就是把這段過渡拉寬。字形的距離場只到 SPREAD，拉太寬會碰到格子邊、變成一塊方的，
    // 所以字形最多拉到描邊外緣還在距離場裡面
    var soft = 1.0 + in.ex.x * k;
    if (mode < 0.5) {
        let range = cam.c.y * in.xf.z;
        soft = min(soft, max(1.8 * (range - ow), 1.0));
    }
    let inner = clamp(d / soft + 0.5, 0.0, 1.0);
    let outer = clamp((d + ow) / soft + 0.5, 0.0, 1.0);
    let fill = inner * in.fx.z;
    let band = outer - inner;
    var glow = 0.0;
    if (in.fx.y > 0.0) {
        glow = in.fx.y * exp(-max(-d - ow, 0.0) / max(in.fx.x * k, 1.0)) * (1.0 - outer);
        // 字形的距離場只到 SPREAD，再外面都是同一個值：光暈沒在那之前退到 0 的話，
        // 格子的邊會變成一圈看得見的方框
        if (mode < 0.5) {
            let range = cam.c.y * in.xf.z;
            glow = glow * clamp((d + range) / (range * 0.5), 0.0, 1.0);
        }
    }

    // 光澤：一道斜的亮帶掃過字面（只亮填色的部分）
    var fill_rgb = in.color.rgb;
    if (in.ex.z > 0.0) {
        let s = (in.local.x / max(in.half_size.x, 1.0) + in.local.y / max(in.half_size.y, 1.0) * 0.5) / 1.5;
        let q = (s - in.ex.y) / 0.2;
        fill_rgb = mix(fill_rgb, vec3<f32>(1.0), exp(-q * q) * in.ex.z);
    }
    var rgb = fill_rgb * fill + in.line.rgb * band + in.color.rgb * glow;
    var a = fill + band + glow;
    if (mode > 1.5 && mode < 2.5) {
        // 圓環整條就是「填色」
        rgb = in.color.rgb * inner + in.color.rgb * glow;
        a = inner + glow;
    }
    var alpha = in.color.a;
    // 由下往上露出來：邊界從底下往上推，邊界上下有一小段漸變
    if (in.ex.w > 0.0) {
        let v = in.local.y / max(in.half_size.y, 1.0);
        let edge = 1.0 - 2.4 * in.ex.w;
        alpha = alpha * smoothstep(edge - 0.2, edge + 0.2, v);
    }
    return vec4<f32>(rgb * alpha, clamp(a, 0.0, 1.0) * alpha);
}
