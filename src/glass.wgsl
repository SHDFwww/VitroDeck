// Liquid Glass。
//
// CSS 版是 `backdrop-filter` 加一張 `feDisplacementMap`（由 app.js 的
// buildWarpMap() 現場產生）。原生沒有 backdrop 這個概念，要自己來：
//
//   1. 先把玻璃底下的東西畫進一張離屏材質
//   2. 模糊它
//   3. 畫面板時去取樣那張模糊圖，靠近邊緣的地方沿著形狀的法線把取樣點
//      往內拉 —— 那就是折射
//
// 比 CSS 那條路好的一點：位移不必預先烘成一張圖。形狀是 SDF，法線可以
// 當場用有限差分算出來，所以任何圓角、任何尺寸都自動正確，
// 不像 displacement map 綁死在固定解析度上。

struct Globals {
    res: vec2<f32>,
    // 視窗失去焦點的程度 0..1
    unfocus: f32,
    _pad: f32,
    // 這批玻璃裡最亮的那團互動光：xy=位置 z=亮度。鄰近玻璃的邊緣會映到它
    world: vec4<f32>,
};

/// 樣式表的一列，從 `theme::GlassLook` 打包（`glass.rs` 的 `pack`）。
/// 長度是邏輯像素；高光的位置是相對面板寬高。
struct Look {
    // 遠的那層投影，小元件與大元件各一組，中間依短邊內插。x=往下 y=模糊 z=alpha
    // shadow_lo.w = 四邊留給投影的空間
    shadow_lo: vec4<f32>,
    shadow_hi: vec4<f32>,
    // 貼著元件的那層：x=往下 y=模糊 z=alpha
    shadow_near: vec4<f32>,
    // xy=尺寸內插的起訖（短邊 px）  zw=折射帶寬與強度在小、大兩端的倍率
    size: vec4<f32>,
    // x=模糊圖跟著往內拉的比例 y=色散 z=飽和度 w=亮度
    optics: vec4<f32>,
    // x=倒角斜率上限 y=由清晰漸漸變糊的那段有多寬（折射帶寬的倍數） z=體積感範圍 w=體積感 alpha
    bevel: vec4<f32>,
    // xy=光往哪個方向照  z=亮環集中在受光處的程度（次方）  w=大元件的內光寬度倍率
    light: vec4<f32>,
    // 亮環：x=受光面 y=對面 z=兩側的底
    rim: vec4<f32>,
    // 內光（倒角接到的光）：x=受光面 y=對面 z=寬度 px
    glow: vec4<f32>,
    // x=由清晰漸漸變糊那段的上限 px（大面板不能因為折射帶寬就一路透進去）
    // y=邊緣比中央清晰多少，0 = 整片同樣的模糊
    // z=大元件翻成淺色玻璃時的白色 alpha（小元件的在 ambient.w）
    clear: vec4<f32>,
    // ---- 依背景調整 ----
    // x,y=背景亮度在這段之間時往反面壓  z=壓到底的 alpha  w=很雜時明暗起伏縮成幾倍
    adapt: vec4<f32>,
    // x,y=雜亂度從乾淨到很雜的範圍  z=四周很雜時投影加深的倍率  w=四周是淺色純色時投影變淡的倍率
    busy: vec4<f32>,
    // 環境光：x=邊緣染色 alpha  y=從邊緣往內多寬 px  z=滲進投影的比例  w=翻成淺色玻璃時的色調 alpha
    ambient: vec4<f32>,
    // 染色玻璃：x=暗端亮度倍率  y=亮端混白比例  z=整體強度
    tinted: vec4<f32>,
    // ---- 互動與動態 ----
    // 指標處的內部光：x=白色 alpha y=半徑 px z=指著時是按下的幾成 w=光源往指標轉多少
    touch: vec4<f32>,
    // x=映到鄰近玻璃的半徑 px y=亮環 alpha  z,w=出現時：存在度到多少開始起霧、到多少本體才完全蓋住
    near: vec4<f32>,
    // 失去焦點：x=亮環與內光剩幾成 y=飽和度的增量剩幾成 z=色調多蓋多少 w=投影剩幾成
    inactive: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
/// 長度要跟 `theme::LOOK_SLOTS` 一樣
@group(0) @binding(1) var<uniform> looks: array<Look, 8>;
/// 融合的玻璃的形狀表：每格兩個 vec4（矩形、圓角）。長度要跟 `glass.rs` 的 `SHAPE_SLOTS` 一樣
@group(0) @binding(2) var<uniform> shapes: array<vec4<f32>, 32>;
@group(1) @binding(0) var backdrop: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;
/// 近乎清晰的同一個畫面。**折射要彎的是它，不是模糊圖。**
@group(1) @binding(2) var clear_tex: texture_2d<f32>;
/// 輕微模糊的中間層。清晰圖直接混到毛玻璃會像「一圈透明框＋一片霧」，
/// 中間墊一層才看得出是由邊緣往內漸漸變糊。
@group(1) @binding(3) var mid_tex: texture_2d<f32>;
/// 很寬的模糊（`theme::SIGMA_GLASS_WIDE`）。量玻璃底下與四周的平均亮度、
/// 取周圍內容的顏色都用它 —— 用玻璃本身那張的話，細節會讓結果跟著晃
@group(1) @binding(4) var wide_tex: texture_2d<f32>;

struct Instance {
    @location(0) rect: vec4<f32>,
    @location(1) radius: vec4<f32>,
    @location(2) tint: vec4<f32>,
    // x = 折射帶寬度(px)  y = 折射強度(px)  z = 亮環寬度(px)  w = 亮環強度
    @location(3) edge: vec4<f32>,
    // 神燈變形：x=cosθ y=sinθ z=水平縮放 w=垂直縮放
    @location(4) xform: vec4<f32>,
    // x,y=洞的位置 z=往洞移動的距離 w=上緣收窄比例
    @location(5) genie: vec4<f32>,
    // x = 整片的不透明度  y = 樣式表的列號  z = 往淺色玻璃翻多少
    @location(6) fade: vec4<f32>,
    // 染色玻璃：rgb = 選定的顏色  w = 強度（0 = 不染）
    @location(7) accent: vec4<f32>,
    // 互動：xy = 光從哪裡擴散  z = 指著的程度  w = 按下的程度
    @location(8) touch: vec4<f32>,
    // 融合：x = 自己在形狀表的格號  y = 這組從第幾格開始  z = 幾塊  w = 融合距離
    @location(9) merge: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) @interpolate(flat) i_rect: vec4<f32>,
    @location(2) @interpolate(flat) i_radius: vec4<f32>,
    @location(3) @interpolate(flat) i_tint: vec4<f32>,
    @location(4) @interpolate(flat) i_edge: vec4<f32>,
    /// 變形之後的螢幕座標。折射要拿它去取樣背景 ——
    /// 用原本的位置的話玻璃飛走了、折射卻還留在原地。
    @location(5) screen: vec2<f32>,
    @location(6) @interpolate(flat) i_xform: vec4<f32>,
    @location(7) @interpolate(flat) i_fade: vec4<f32>,
    @location(8) @interpolate(flat) i_look: u32,
    /// 背景：x=底下的平均亮度 y=底下的雜亂度 z=四周的平均亮度 w=四周的雜亂度
    @location(9) @interpolate(flat) i_stats: vec4<f32>,
    @location(10) @interpolate(flat) i_accent: vec4<f32>,
    @location(11) @interpolate(flat) i_touch: vec4<f32>,
    @location(12) @interpolate(flat) i_merge: vec4<f32>,
};

fn lum_of(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

/// 一個點的（寬模糊亮度, 細節偏離它多少）。細節取玻璃本身那張模糊圖。
/// 頂點著色器沒有導數，要用 `textureSampleLevel`
fn probe(p: vec2<f32>) -> vec2<f32> {
    let uv = clamp(p / g.res, vec2<f32>(0.0), vec2<f32>(1.0));
    let w = lum_of(textureSampleLevel(wide_tex, samp, uv, 0.0).rgb);
    let d = lum_of(textureSampleLevel(backdrop, samp, uv, 0.0).rgb);
    return vec2<f32>(w, abs(d - w));
}

/// 矩形裡 4x4 個點的平均
fn probe_inside(r: vec4<f32>) -> vec2<f32> {
    var s = vec2<f32>(0.0);
    for (var j = 0; j < 4; j++) {
        for (var i = 0; i < 4; i++) {
            let f = (vec2<f32>(f32(i), f32(j)) + 0.5) / 4.0;
            s += probe(r.xy + r.zw * f);
        }
    }
    return s / 16.0;
}

/// 矩形外面一圈（離邊 `d`）每邊 3 個點的平均：投影落在的地方
fn probe_ring(r: vec4<f32>, d: f32) -> vec2<f32> {
    var s = vec2<f32>(0.0);
    for (var i = 0; i < 3; i++) {
        let f = (f32(i) + 0.5) / 3.0;
        s += probe(vec2<f32>(r.x + r.z * f, r.y - d));
        s += probe(vec2<f32>(r.x + r.z * f, r.y + r.w + d));
        s += probe(vec2<f32>(r.x - d, r.y + r.w * f));
        s += probe(vec2<f32>(r.x + r.z + d, r.y + r.w * f));
    }
    return s / 12.0;
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let c = corners[vi];
    let half = inst.rect.zw * 0.5;
    let center = inst.rect.xy + half;
    let look = min(u32(inst.fade.y), 7u);
    // 除了抗鋸齒的 2px，還要留得下投影（CSS 那邊是 0 14px 44px，所以留 62）
    let local = (c * 2.0 - 1.0) * (half + looks[look].shadow_lo.w);
    var px = center + local;

    // ---- 神燈變形 ----
    // 順序不能換，這是 macOS 那個效果的全部內容：
    //   1. 整塊往洞的方向移，上緣貼到洞口
    //   2. 以上緣為軸拉長（本體還留在原處 → 拉出頸部）
    //   3. 以洞的 x 為軸收窄，上緣收得比下緣多 → 漏斗
    //   4. 繞著洞公轉
    let hole = inst.genie.xy;
    let ty = inst.genie.z;
    let taper = inst.genie.w;
    px.y = px.y + ty;
    let top = inst.rect.y + ty;
    px.y = top + (px.y - top) * inst.xform.w;
    // c.y：0 是上緣、1 是下緣
    let sx = inst.xform.z * mix(taper, 1.0, c.y);
    px.x = hole.x + (px.x - hole.x) * sx;
    let d = px - hole;
    let cs = inst.xform.x;
    let sn = inst.xform.y;
    px = hole + vec2<f32>(d.x * cs - d.y * sn, d.x * sn + d.y * cs);

    var out: VsOut;
    out.pos = vec4<f32>(px.x / g.res.x * 2.0 - 1.0, 1.0 - px.y / g.res.y * 2.0, 0.0, 1.0);
    out.local = local;
    out.screen = px;
    out.i_xform = inst.xform;
    out.i_rect = inst.rect;
    out.i_radius = inst.radius;
    out.i_tint = inst.tint;
    out.i_edge = inst.edge;
    out.i_fade = inst.fade;
    out.i_look = look;
    out.i_accent = inst.accent;
    out.i_touch = inst.touch;
    out.i_merge = inst.merge;
    // 背景量在這裡：每塊玻璃一組值，片段著色器整片共用，不會一塊玻璃上
    // 這邊深那邊淺。用變形前的位置 —— 神燈吸走的途中照原本的背景就好
    let L = looks[look];
    let ts = smoothstep(L.size.x, L.size.y, min(inst.rect.z, inst.rect.w));
    let ring_d = mix(L.shadow_lo.y, L.shadow_hi.y, ts) * 0.5 + 4.0;
    out.i_stats = vec4<f32>(probe_inside(inst.rect), probe_ring(inst.rect, ring_d));
    return out;
}

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

/// 平滑聯集（多項式 smooth min）。兩邊的距離差小於 k 才會長出頸子
fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = max(k - abs(a - b), 0.0) / max(k, 1e-4);
    return min(a, b) - h * h * k * 0.25;
}

/// 形狀表第 `k` 格那塊在螢幕座標 `p` 的距離
fn shape_at(k: u32, p: vec2<f32>) -> f32 {
    let r = shapes[k * 2u];
    let h = r.zw * 0.5;
    return sd_round_box(p - r.xy - h, h, shapes[k * 2u + 1u]);
}

/// 形狀的 SDF，`local` 是相對自己中心的座標（變形前）。融合的那組是整組的平滑聯集，
/// 而且順序固定照形狀表走 —— 每一塊算出來的必須是同一個形狀，接縫才對得起來。
/// 不融合的直接用 `local` 算，不繞去螢幕座標：繞一圈的捨入誤差會讓邊緣差一點點
fn shape_sd(local: vec2<f32>, rect: vec4<f32>, radius: vec4<f32>, merge: vec4<f32>) -> f32 {
    let n = u32(merge.z);
    let half = rect.zw * 0.5;
    if (n < 2u) {
        return sd_round_box(local, half, radius);
    }
    let p = rect.xy + half + local;
    var d = 1e6;
    for (var i = 0u; i < n; i++) {
        d = smin(d, shape_at(u32(merge.y) + i, p), merge.w);
    }
    return d;
}

/// 融合的那組裡這一點歸誰畫：離它最近的那塊（一樣近取格號小的）。
/// 每塊各畫自己那份，重疊的地方才不會疊兩次
fn owns(p: vec2<f32>, merge: vec4<f32>) -> bool {
    let n = u32(merge.z);
    if (n < 2u) {
        return true;
    }
    var best = 1e9;
    var who = 0u;
    for (var i = 0u; i < n; i++) {
        let k = u32(merge.y) + i;
        let d = shape_at(k, p);
        if (d < best) {
            best = d;
            who = k;
        }
    }
    return who == u32(merge.x);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let L = looks[in.i_look];
    let half = in.i_rect.zw * 0.5;
    // 變形前的螢幕座標。融合的那組用它分誰畫哪裡
    let p0 = in.i_rect.xy + half + in.local;
    let sd = shape_sd(in.local, in.i_rect, in.i_radius, in.i_merge);
    let aa = max(fwidth(sd), 0.0001);
    if (!owns(p0, in.i_merge)) {
        discard;
    }
    let body = 1.0 - smoothstep(-aa, aa, sd);
    // 出現與消失：存在度 m。先是一片透明的鏡片，m 高一點才起霧、蓋住
    let m = 1.0 - in.i_fade.w;
    let present = smoothstep(0.0, max(L.near.w, 1e-4), m);
    // 視窗失去焦點：亮環、飽和度、投影都收一點，玻璃退到後面去
    let un = g.unfocus;

    // 元件有多大：短邊在 size.x 以下算小、size.y 以上算大。
    // 越大的玻璃越厚 —— 折射越明顯、投影越深越散、光越柔（Liquid Glass 的尺寸規則）
    let ts = smoothstep(L.size.x, L.size.y, min(in.i_rect.z, in.i_rect.w));

    // 投影要先算 —— 它在**本體外面**，body<=0 就直接 discard 的話投影會不見。
    //
    // 兩層：遠的那層隨尺寸變，近的那層貼著元件。位移不一樣，所以**要各算各的**；
    // 兩層共用同一個位移的話，近的那層會整片壓在遠的那層上，控制列底下會黑掉一塊。
    //
    // 合成也不是相加。兩層是先後疊上去的，等於 1-(1-a1)(1-a2)；
    // 直接相加在兩層都滿的地方會多出不該有的不透明度。
    let sf = mix(L.shadow_lo.xyz, L.shadow_hi.xyz, ts);
    let sn0 = L.shadow_near;
    let sd1 = shape_sd(in.local - vec2<f32>(0.0, sf.x), in.i_rect, in.i_radius, in.i_merge);
    let sd2 = shape_sd(in.local - vec2<f32>(0.0, sn0.x), in.i_rect, in.i_radius, in.i_merge);
    // 投影依背景調整：四周很雜（壓在字上）要加深才分得出層次，
    // 四周是淺色純色時本來就分得出來，太深反而髒
    let busy_out = smoothstep(L.busy.x, L.busy.y, in.i_stats.w);
    let bright_out = smoothstep(L.adapt.x, L.adapt.y, in.i_stats.z) * (1.0 - busy_out);
    let sk = mix(1.0, L.busy.z, busy_out) * mix(1.0, L.busy.w, bright_out) * m * mix(1.0, L.inactive.w, un);
    let a1 = min((1.0 - smoothstep(-sf.y, sf.y, sd1)) * sf.z * sk, 1.0);
    let a2 = min((1.0 - smoothstep(-sn0.y, sn0.y, sd2)) * sn0.z * sk, 1.0);
    let shadow = (1.0 - (1.0 - a1) * (1.0 - a2)) * (1.0 - body);
    // 大元件四周的彩色內容透過玻璃，光滲進投影：投影帶一點周圍的顏色
    let shade_rgb = textureSampleLevel(wide_tex, samp, clamp(in.screen / g.res, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).rgb
                  * L.ambient.z * ts;
    if (body <= 0.0) {
        if (shadow <= 0.002) {
            discard;
        }
        // 預乘：顏色乘上 alpha
        return vec4<f32>(shade_rgb * shadow, shadow) * in.i_fade.x;
    }

    // 形狀的外法線。有限差分比預烘的位移圖準，而且跟解析度無關。
    let e = 1.0;
    let nx = shape_sd(in.local + vec2<f32>(e, 0.0), in.i_rect, in.i_radius, in.i_merge)
           - shape_sd(in.local - vec2<f32>(e, 0.0), in.i_rect, in.i_radius, in.i_merge);
    let ny = shape_sd(in.local + vec2<f32>(0.0, e), in.i_rect, in.i_radius, in.i_merge)
           - shape_sd(in.local - vec2<f32>(0.0, e), in.i_rect, in.i_radius, in.i_merge);
    let n = normalize(vec2<f32>(nx, ny) + vec2<f32>(1e-6, 0.0));

    // 只有靠近邊界的一圈才折射，中央是平的 —— 真玻璃也是這樣，
    // 厚度變化集中在邊緣的倒角上。
    let kz = mix(L.size.z, L.size.w, ts);
    let band = max(in.i_edge.x * kz, 0.001);
    let t = clamp(1.0 + sd / band, 0.0, 1.0);      // 0=中央, 1=邊界

    // 倒角的形狀用四分之一圓：厚度 z(t)=sqrt(1-t²)，位移量正比於斜率
    // t/sqrt(1-t²)。**這條曲線的重點在最外圈會陡到爆** —— 背景在那裡被
    // 壓成細細一條，那正是「看起來像一塊有厚度的玻璃」的訊號。
    // 用 pow(t,2.5) 這種溫吞的曲線，邊緣只會糊掉，不會有壓縮感。
    let slope = t / sqrt(max(1.0 - t * t, 0.02));
    // 出現的途中折射跟著存在度長出來
    let push = min(slope, L.bevel.x) * in.i_edge.y * kz * m;

    // 法線也要跟著轉，不然玻璃轉了但折射的方向沒轉。
    let cs = in.i_xform.x;
    let sn = in.i_xform.y;
    let nr = vec2<f32>(n.x * cs - n.y * sn, n.x * sn + n.y * cs);
    // 三層要用差不多的位移，否則過渡那段會出現錯開的兩份影像
    let blurred = textureSample(backdrop, samp, (in.screen - nr * push * L.optics.x) / g.res).rgb;

    // 折射帶：拿清晰的那張來彎，而且 RGB 各彎一點點不一樣 ——
    // 玻璃對不同波長的折射率本來就不同，邊緣那一圈的色散是真玻璃
    // 最容易認出來的特徵，少了它怎麼調都只是「有點亮的霧面板」。
    let base = in.screen - nr * push;
    let disp = nr * push * L.optics.y;
    var bent = vec3<f32>(
        textureSample(clear_tex, samp, (base - disp) / g.res).r,
        textureSample(clear_tex, samp, base / g.res).g,
        textureSample(clear_tex, samp, (base + disp) / g.res).b,
    );
    let soft = textureSample(mid_tex, samp, base / g.res).rgb;
    // 中央是毛玻璃，越靠邊越透 —— 玻璃的中間厚、邊緣薄。
    // 透明度由邊緣往內在「折射帶寬 × bevel.y」這段距離裡平滑降到 0，
    // 比折射帶本身寬得多；只在折射帶裡切換的話，看起來是一圈透明框
    // 突然接上一片霧。c 從 1 到 0 依序經過 清晰 → 中間層 → 毛玻璃。
    let reach = max(min(band * L.bevel.y, L.clear.x), 0.001);
    let c = (1.0 - smoothstep(0.0, reach, -sd)) * in.i_edge.w * L.clear.y;
    var col = mix(mix(blurred, soft, clamp(c * 2.0, 0.0, 1.0)), bent, clamp(c * 2.0 - 1.0, 0.0, 1.0));

    // ---- 動態範圍依背景調整 ----
    // 底下很雜（字、條紋）的時候，把明暗起伏往周圍的平均收，玻璃上的字才不會
    // 跟背後的字打架。乾淨的背景不動
    let busy_in = smoothstep(L.busy.x, L.busy.y, in.i_stats.y);
    let calm = textureSampleLevel(wide_tex, samp, clamp(in.screen / g.res, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).rgb;
    col = calm + (col - calm) * mix(1.0, L.adapt.w, busy_in);

    // ---- 提高飽和與亮度 ----
    // CSS 那邊是 `saturate(200%) brightness(1.12)`，跟 blur 寫在同一行。
    // **少了這個就只是毛玻璃** —— 專輯的顏色透不上來，整片灰灰的。
    //
    // 係數比 CSS 的 200% 低：那邊的背景是原圖，我這邊取樣的是**已經模糊過**
    // 的畫面。模糊本身就是在平均，平均會降飽和，再乘 2 會把主色調推爆
    // （封面是綠的，整條就變螢光綠）。1.5 看起來才跟 web 版對得上。
    let luma = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
    let sat = 1.0 + (L.optics.z - 1.0) * mix(1.0, L.inactive.y, un);
    col = clamp(mix(vec3<f32>(luma), col, sat), vec3<f32>(0.0), vec3<f32>(1.0)) * L.optics.w;

    // ---- 色調量依背景調整、明暗切換 ----
    // tone 0 = 深色玻璃（白字），1 = 淺色玻璃（深字），由 CPU 依背景決定（`Glass::tone`）。
    // 還沒翻之前，深色玻璃壓在偏亮的背景上要先自己壓暗，白字才讀得到；
    // 淺色玻璃反過來。壓的量隨背景亮度平滑變，不是一個門檻
    let tone = in.i_fade.z;
    let lum = smoothstep(L.adapt.x, L.adapt.y, in.i_stats.x);
    let off = mix(lum, 1.0 - lum, tone);
    col = mix(col, vec3<f32>(tone), off * L.adapt.z);

    // 玻璃本身的淡色。tint.a 是它蓋過背景的程度；翻成淺色玻璃時換成樣式表的那個
    // 淺色玻璃的白：小元件 ambient.w、大元件 clear.z，依短邊內插
    col = mix(col, in.i_tint.rgb, mix(in.i_tint.a, mix(L.ambient.w, L.clear.z, ts), tone));
    col = mix(col, in.i_tint.rgb, L.inactive.z * un);

    // ---- 染色玻璃（主要動作）----
    // 不是蓋一層固定的顏色：底下亮的地方用淺一階、暗的地方用深一階，
    // 同一個顏色在不同背景上都還看得出是玻璃
    if (in.i_accent.w > 0.0) {
        let k = smoothstep(0.05, 0.75, lum_of(col));
        let shade = mix(in.i_accent.rgb * L.tinted.x, mix(in.i_accent.rgb, vec3<f32>(1.0), L.tinted.y), k);
        col = mix(col, shade, in.i_accent.w * L.tinted.z);
    }

    // ---- 環境光 ----
    // 大面板旁邊有彩色內容時，那個顏色從邊緣滲進來。每一點取「往外法線方向、
    // 出了邊緣再走 ambient.y」那裡的顏色，所以左邊染左邊的、上面染上面的。
    // 用濾色（screen）疊，亮的地方不會爆
    let amb_w = max(L.ambient.y, 1.0);
    let near_edge = (1.0 - smoothstep(0.0, amb_w, -sd)) * ts * L.ambient.x;
    let out_uv = clamp((in.screen + nr * (amb_w - sd)) / g.res, vec2<f32>(0.0), vec2<f32>(1.0));
    let amb = textureSampleLevel(wide_tex, samp, out_uv, 0.0).rgb;
    let amb_lit = clamp(mix(vec3<f32>(lum_of(amb)), amb, 1.6), vec3<f32>(0.0), vec3<f32>(1.0));
    col = 1.0 - (1.0 - col) * (1.0 - amb_lit * near_edge);

    // ---- 互動：從指標處往外擴散的內部光 ----
    // 指著是按下的幾成（touch.z），按下才全亮。大元件散得開一點
    let gi = in.i_touch.z * (L.touch.z + clamp(in.i_touch.w, 0.0, 1.0) * (1.0 - L.touch.z));
    let gr = L.touch.y * mix(1.0, 1.6, ts);
    let dt = in.screen - in.i_touch.xy;
    let spot = exp(-dot(dt, dt) / (gr * gr)) * gi * L.touch.x;
    // 沒作用的時候整段跳過，不讓捨入誤差改到平常的畫面（下同）
    if (spot > 0.0) {
        col = 1.0 - (1.0 - col) * (1.0 - spot);
    }

    // 還沒起霧的那段：彎著清晰背景的鏡片
    if (m < 1.0) {
        col = mix(bent, col, smoothstep(L.near.z, 1.0, m));
    }

    // ---- 沿著輪廓的高光 ----
    // 光從一個方向來，倒角朝著光的那段最亮，正對面也會亮一點（光穿過玻璃
    // 從另一邊的倒角反射出來），跟光線垂直的兩側幾乎不亮。所以亮度只跟
    // 「這一點的法線」與光線的夾角有關，形狀是什麼都自動沿著輪廓走。
    // 舊版是固定角度的線性漸層加兩塊固定位置的光斑，跟形狀無關。
    // 指著的時候光源轉向指標，亮的那段沿著輪廓繞過去。轉的是角度，
    // 不是把兩個方向混起來 —— 指標在光的正對面時，混起來會變成零向量
    let center = in.i_rect.xy + half;
    let to_p = in.i_touch.xy - center;
    let steer = in.i_touch.z * L.touch.w * smoothstep(2.0, 12.0, length(to_p));
    var ld = L.light.xy;
    if (steer > 0.0) {
        let a0 = atan2(-L.light.y, -L.light.x);
        var da = atan2(to_p.y, to_p.x) - a0;
        da = da - 6.2831853 * round(da / 6.2831853);
        ld = -vec2<f32>(cos(a0 + da * steer), sin(a0 + da * steer));
    }
    let lit = pow(max(dot(nr, -ld), 0.0), L.light.z);
    let back = pow(max(dot(nr, ld), 0.0), L.light.z);
    // **亮環是一條細線，不是一圈暈開的光。** 糊成三四個像素寬之後就不像
    // 玻璃的倒角，只像描了一道灰邊。
    let rw = max(in.i_edge.z, 0.3);
    let ring = smoothstep(-rw - aa, -rw + aa, sd);
    let rim = L.rim.z + lit * L.rim.x + back * L.rim.y;
    // 內光：倒角接到的光往內淡開。大元件散得比較開（光比較柔）
    let gw = L.glow.z * mix(1.0, L.light.w, ts);
    let inner = 1.0 - smoothstep(0.0, gw, -sd);
    let glow = inner * (lit * L.glow.x + back * L.glow.y);
    // 附近有一團互動光（可能在別塊玻璃上）：靠近它的那段邊緣也亮起來
    let dw = in.screen - g.world.xy;
    let near = exp(-dot(dw, dw) / (L.near.x * L.near.x)) * g.world.z * L.near.y;
    // **這些是白色疊層的 alpha，不是加上去的亮度。** 當成加法的話
    // 整條會白掉。
    let lit_k = m * mix(1.0, L.inactive.x, un);
    col = mix(col, vec3<f32>(1.0), clamp((ring * (rim + near) + glow + inner * near * 0.5) * lit_k, 0.0, 1.0));

    // 玻璃體積感：整片往內一點點的亮度，CSS 是 inset 0 0 22px .045
    col = mix(col, vec3<f32>(1.0), smoothstep(-L.bevel.z, 0.0, sd) * L.bevel.w * m);

    // 預乘 alpha：管線是 PREMULTIPLIED_ALPHA_BLENDING。
    // 邊緣那一圈 body 介於 0 與 1，投影在那裡補上剩下的不透明度。
    let b = body * present;
    return vec4<f32>(col * b + shade_rgb * shadow, b + shadow) * in.i_fade.x;
}
