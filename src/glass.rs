//! Liquid Glass 的算圖流程。
//!
//! CSS 那 24 處 `backdrop-filter` 在原生要拆成一條真正的算圖流程：
//!
//! ```text
//!   1. scene     把玻璃底下的東西畫進離屏材質
//!   2. blur      半解析度、水平＋垂直兩趟高斯
//!   3. glass     畫面板，取樣模糊圖，邊緣沿法線位移 = 折射
//!   4. 其餘       面板上的文字與圖示照常畫
//! ```
//!
//! 折射不需要像 CSS 版那樣預先烘一張 displacement map —— 形狀是 SDF，
//! 法線當場用有限差分算，任何尺寸與圓角都自動正確。

use crate::gfx::{Gpu, Offscreen};

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Panel {
    pub rect: [f32; 4],
    pub radius: [f32; 4],
    /// rgb = 玻璃的色調，a = 蓋過背景的程度
    pub tint: [f32; 4],
    /// x=折射帶寬度(px) y=折射強度(px) z=亮環寬度(px) w=折射帶的清晰度
    pub edge: [f32; 4],
    /// 神燈變形。x=cosθ y=sinθ z=水平縮放 w=垂直縮放
    pub xform: [f32; 4],
    /// x,y=「洞」的位置（螢幕座標） z=往洞移動的距離 w=上緣收窄比例
    pub genie: [f32; 4],
    /// x=整片的不透明度（含投影），通知列淡入淡出用  y=樣式表的列號（`theme::glass_looks`）
    /// z=往淺色玻璃翻多少（`Glass::tone`）
    pub fade: [f32; 4],
    /// 染色玻璃：rgb=選定的顏色，w=強度（0 = 不染）。只給主要動作用
    pub accent: [f32; 4],
    /// 互動（`Glass::touch`）：x,y=光從哪裡擴散（螢幕座標） z=指著的程度 w=按下的程度（彈簧，會超過 0..1）
    pub touch: [f32; 4],
    /// 融合成一塊（`Panel::merge`）：x=組號（0 = 不融合） w=融合距離。
    /// 送進著色器前 `Glass::render` 改寫成 x=自己在形狀表的格號 y=這組從第幾格開始 z=這組幾塊
    pub merge: [f32; 4],
}

/// 一塊玻璃這一幀的互動狀態，`Glass::touch` 的回傳值
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Touch {
    /// 指著的程度 0..1（有過渡）
    pub hover: f32,
    /// 按下的程度。彈簧，放開後會短暫小於 0 —— 那就是凝膠彈回來晃的那一下
    pub press: f32,
    /// 光從哪裡擴散：指標在這塊上就是指標，不然是被指著的那顆按鈕的中心
    pub at: (f32, f32),
}

/// 凝膠形變的彈簧走 `dt` 秒：x'' = -ω²(x - 目標) - 2ζω x'（`theme::GEL_SPRING`）。
/// 切成 1/240 秒的小步積分，幀率低也不會炸開；夠靠近目標就直接停在目標上
fn spring(mut x: f32, mut v: f32, goal: f32, dt: f32) -> (f32, f32) {
    let (w, z) = crate::theme::GEL_SPRING;
    let n = (dt / (1.0 / 240.0)).ceil().max(1.0) as u32;
    let h = dt / n as f32;
    for _ in 0..n {
        let acc = -w * w * (x - goal) - 2.0 * z * w * v;
        v += acc * h;
        x += v * h;
    }
    if (x - goal).abs() < 1e-3 && v.abs() < 1e-2 {
        return (goal, 0.0);
    }
    (x, v)
}

/// 凝膠形變：按住時整塊放大一點、往指標那邊拉長、中心往指標偏。
/// `press` 小於 0（彈回來的那一下）時反過來縮一點
pub fn gel_rect(rect: [f32; 4], press: f32, at: (f32, f32)) -> [f32; 4] {
    use crate::theme::{GEL_GROW, GEL_PULL, GEL_STRETCH};
    if press == 0.0 {
        return rect;
    }
    let (hw, hh) = (rect[2] * 0.5, rect[3] * 0.5);
    let (cx, cy) = (rect[0] + hw, rect[1] + hh);
    // 指標相對中心的方向，以半寬半高為 1，超出就截在 1
    let dx = ((at.0 - cx) / hw.max(1.0)).clamp(-1.0, 1.0);
    let dy = ((at.1 - cy) / hh.max(1.0)).clamp(-1.0, 1.0);
    let sx = 1.0 + press * (GEL_GROW + GEL_STRETCH * dx.abs());
    let sy = 1.0 + press * (GEL_GROW + GEL_STRETCH * dy.abs());
    let (ncx, ncy) = (cx + dx * hw * GEL_PULL * press, cy + dy * hh * GEL_PULL * press);
    [ncx - hw * sx, ncy - hh * sy, rect[2] * sx, rect[3] * sy]
}

/// macOS 神燈效果的關鍵影格，照抄 `viz.js` 的 `genie()`。
///
/// 三件事同時發生：繞著洞公轉一整圈、上緣被吸到洞口、本體被拉成一條
/// 又長又窄的帶子。`sy` 會先變大再收掉 —— 那個「先拉長」就是漏斗頸，
/// 少了它只是單純縮小，看起來完全不像。
///
/// 欄位順序：進度、角度（度）、朝洞移動的比例、水平縮放、垂直縮放
const GENIE: &[(f32, f32, f32, f32, f32)] = &[
    (0.00, 0.0, 0.00, 1.000, 1.000),
    (0.30, 80.0, 0.55, 0.550, 3.200),
    (0.62, 200.0, 1.00, 0.270, 3.800),
    (0.88, 330.0, 1.00, 0.130, 6.900),
    (1.00, 360.0, 1.00, 0.004, 0.004),
];

fn genie_at(p: f32) -> (f32, f32, f32, f32) {
    let p = p.clamp(0.0, 1.0);
    let mut k = 0usize;
    while k + 2 < GENIE.len() && p > GENIE[k + 1].0 {
        k += 1;
    }
    let (p0, d0, t0, x0, y0) = GENIE[k];
    let (p1, d1, t1, x1, y1) = GENIE[k + 1];
    let f = if (p1 - p0).abs() < 1e-6 { 0.0 } else { (p - p0) / (p1 - p0) };
    let lerp = |a: f32, b: f32| a + (b - a) * f;
    (lerp(d0, d1).to_radians(), lerp(t0, t1), lerp(x0, x1), lerp(y0, y1))
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            rect: [0.0; 4],
            radius: [24.0; 4],
            tint: [1.0, 1.0, 1.0, 0.10],
            edge: [18.0, 9.0, 1.5, 0.35],
            xform: [1.0, 0.0, 1.0, 1.0],
            genie: [0.0, 0.0, 0.0, 1.0],
            fade: [1.0, 0.0, 0.0, 0.0],
            accent: [0.0; 4],
            touch: [0.0; 4],
            merge: [0.0; 4],
        }
    }
}

impl Panel {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { rect: [x, y, w, h], ..Default::default() }
    }
    pub fn radius(mut self, r: f32) -> Self {
        self.radius = [r; 4];
        self
    }
    pub fn tint(mut self, c: [f32; 4]) -> Self {
        self.tint = c;
        self
    }
    /// `clarity` = 邊緣折射帶裡「清晰畫面」佔的比例。0 就退回純毛玻璃。
    pub fn edge(mut self, band: f32, strength: f32, ring_w: f32, clarity: f32) -> Self {
        self.edge = [band, strength, ring_w, clarity];
        self
    }

    /// 套一組玻璃預設（色調與邊緣折射）。模糊 σ 由呼叫端用 `style.sigma` 取。
    pub fn style(mut self, s: &crate::theme::GlassStyle) -> Self {
        self.tint = s.tint();
        self.edge = [s.edge.0, s.edge.1, s.edge.2, s.edge.3];
        self.fade[1] = s.look as f32;
        self
    }

    /// 整片（連投影）一起淡。預乘 alpha 下直接把輸出整個乘上去就對了。
    /// 玻璃的出現與消失改用 `materialize`，這個留給真的要整片變淡的場合
    #[allow(dead_code)]
    pub fn opacity(mut self, a: f32) -> Self {
        self.fade[0] = a.clamp(0.0, 1.0);
        self
    }

    /// 往淺色玻璃翻多少，通常是 `Glass::tone` 的回傳值。
    /// 上面的字要用同一個值開 `theme::scoped_tone`，兩邊才會一起翻
    pub fn tone(mut self, t: f32) -> Self {
        self.fade[2] = t.clamp(0.0, 1.0);
        self
    }

    /// 染色玻璃：用 `rgb` 依底下內容的亮度產生一組深淺，亮的地方淺、暗的地方深
    pub fn tinted(mut self, rgb: [f32; 4]) -> Self {
        self.accent = [rgb[0], rgb[1], rgb[2], 1.0];
        self
    }

    /// 互動：指標處發光、光源轉向指標。`gel` 的話按住時還會像凝膠一樣變形 ——
    /// 形狀會變，所以只給單顆的按鈕用，整段控制列不用。要在 `radius` 之後叫
    pub fn touch(mut self, t: &Touch, gel: bool) -> Self {
        self.touch = [t.at.0, t.at.1, t.hover.clamp(0.0, 1.0), t.press];
        if gel {
            self.rect = gel_rect(self.rect, t.press, t.at);
        }
        self
    }

    /// 出現與消失：`a` 是存在度 0..1。不是淡出 —— 先出現一片透明的鏡片把背景彎過去，
    /// 存在度再高一點才起霧、上色。上面的字照樣自己淡
    pub fn materialize(mut self, a: f32) -> Self {
        self.fade[3] = 1.0 - a.clamp(0.0, 1.0);
        self
    }

    /// 跟同一組（`group` 相同、連著推進去）的其他玻璃融合成一塊：靠得夠近就長出
    /// 一條像液體的頸子連起來。`k` 是融合距離（`theme::MERGE_K`）
    pub fn merge(mut self, group: u32, k: f32) -> Self {
        self.merge = [group as f32, 0.0, 0.0, k];
        self
    }

    /// 神燈收起。`p` 是收起的進度：0 = 完全正常，1 = 已經吸進洞裡。
    /// `hole` 是要吸進去的位置（通常是畫面中心）。
    pub fn genie(mut self, p: f32, hole: (f32, f32)) -> Self {
        if p <= 0.0 {
            return self;
        }
        let (ang, tyf, sx, sy) = genie_at(p);
        // 上緣要移到洞口，所以位移量是「洞到上緣的距離」乘上進度
        let ty = (hole.1 - self.rect[1]) * tyf;
        self.xform = [ang.cos(), ang.sin(), sx, sy];
        // 上緣同時往洞的 x 收成一點，下緣維持原寬 —— 那就是漏斗的輪廓
        self.genie = [hole.0, hole.1, ty, 1.0 - 0.75 * p];
        self
    }
}

/// 樣式表一列在著色器裡的樣子：17 個 vec4，欄位順序見 `glass.wgsl` 的 `Look`
type LookRow = [[f32; 4]; 17];

fn pack(l: &crate::theme::GlassLook) -> LookRow {
    let (lo, hi, n) = (l.shadow_small, l.shadow_large, l.shadow_near);
    // 光線方向先正規化，樣式表裡寫角度比較好讀
    let (s, c) = l.light_deg.to_radians().sin_cos();
    [
        [lo.0, lo.1, lo.2, l.shadow_pad],
        [hi.0, hi.1, hi.2, 0.0],
        [n.0, n.1, n.2, 0.0],
        [l.size_range.0, l.size_range.1, l.refract_scale.0, l.refract_scale.1],
        [l.blur_pull, l.dispersion, l.saturate, l.brightness],
        [l.slope_cap, l.clear_reach, l.volume.0, l.volume.1],
        [s, -c, l.rim_focus, l.glow_soften],
        [l.rim.0, l.rim.1, l.rim.2, 0.0],
        [l.glow.0, l.glow.1, l.glow.2, 0.0],
        [l.clear_max, l.edge_clear, l.tone_tint.1, 0.0],
        [l.adapt_lum.0, l.adapt_lum.1, l.adapt_amount, l.range_busy],
        [l.busy_range.0, l.busy_range.1, l.shadow_busy, l.shadow_bright],
        [l.ambient.0, l.ambient.1, l.ambient.2, l.tone_tint.0],
        [l.tinted.0, l.tinted.1, l.tinted.2, 0.0],
        [l.touch_glow.0, l.touch_glow.1, l.touch_glow.2, l.touch_light],
        [l.near_light.0, l.near_light.1, l.materialize.0, l.materialize.1],
        [l.inactive.0, l.inactive.1, l.inactive.2, l.inactive.3],
    ]
}

fn pack_all(looks: &[crate::theme::GlassLook]) -> Vec<LookRow> {
    assert!(looks.len() <= crate::theme::LOOK_SLOTS, "樣式表超過 glass.wgsl 的陣列長度");
    let mut rows = vec![[[0.0; 4]; 17]; crate::theme::LOOK_SLOTS];
    for (row, l) in rows.iter_mut().zip(looks) {
        *row = pack(l);
    }
    rows
}

// ---- 背景讀回 ----
//
// 玻璃本身怎麼依背景調整，著色器自己取樣就知道（見 `glass.wgsl` 的頂點著色器）。
// 但玻璃上的字是 glyphon 畫的，顏色在 CPU 端就定了 —— 要讓字跟著玻璃翻明暗，
// CPU 得知道背景多亮。做法是每次 `sharp_scene` 把很寬的那張模糊圖（160x90 左右）
// 複製一份到可映射的緩衝，非同步讀回來。實機上因此晚一兩幀，翻的時候本來就有
// 0.3 秒的過渡，看不出來；`--shot` 整個畫兩次，第二次用第一次讀回的（見 `settle`）。

const PROBE_IDLE: u8 = 0;
/// 複製指令錄好了，還沒送出去或還沒開始映射
const PROBE_COPIED: u8 = 1;
const PROBE_MAPPING: u8 = 2;
const PROBE_READY: u8 = 3;
const PROBE_FAILED: u8 = 4;
/// 一幀最多幾個場景各拍一張，加上讀回的延遲
const PROBE_SLOTS: usize = 6;

struct Probe {
    buf: wgpu::Buffer,
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    size: (u32, u32),
    bpr: u32,
    /// 這一幀第幾次 `sharp_scene` 拍的，跟 `Glass::call` 對應
    call: usize,
    /// 拍的時候畫面的邏輯尺寸
    logical: (f32, f32),
}

/// 讀回來的寬模糊圖，RGBA、每列的 padding 已經拿掉
struct Snap {
    w: u32,
    h: u32,
    logical: (f32, f32),
    px: Vec<u8>,
}

impl Snap {
    /// `rect`（邏輯像素）底下的平均 luma，sRGB 編碼值 0..1，跟著色器算法一樣
    fn luma(&self, rect: [f32; 4]) -> Option<f32> {
        let (sx, sy) = (self.w as f32 / self.logical.0, self.h as f32 / self.logical.1);
        let span = |a: f32, len: f32, s: f32, n: u32| {
            let lo = ((a * s).floor().max(0.0) as u32).min(n.saturating_sub(1));
            let hi = (((a + len) * s).ceil().max(0.0) as u32).clamp(lo + 1, n);
            (lo, hi)
        };
        let (x0, x1) = span(rect[0], rect[2], sx, self.w);
        let (y0, y1) = span(rect[1], rect[3], sy, self.h);
        let mut sum = 0.0f64;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * self.w + x) * 4) as usize;
                let p = &self.px[i..i + 3];
                sum += 0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64;
            }
        }
        let n = ((x1 - x0) * (y1 - y0)) as f64;
        (n > 0.0).then(|| (sum / n / 255.0) as f32)
    }
}

/// 一個會翻明暗的元件目前的狀態
struct Tone {
    /// 目前要翻成淺色嗎（有遲滯，見 `theme::TONE_UP`）
    light: bool,
    /// 動畫中的值 0..1
    value: f32,
    at: std::time::Instant,
}

/// 一個會互動的元件跨幀保留的狀態
struct TouchState {
    hover: f32,
    press: f32,
    vel: f32,
    at: (f32, f32),
    t: std::time::Instant,
}

/// 形狀表的格數（每格兩個 vec4：矩形、圓角），要跟 `glass.wgsl` 的 `shapes` 一樣
const SHAPE_SLOTS: usize = 16;

pub struct Glass {
    glass_pipe: wgpu::RenderPipeline,
    blur_pipe: wgpu::RenderPipeline,
    down_pipe: wgpu::RenderPipeline,
    #[allow(dead_code)] // 建管線時用過，之後沒有人讀
    globals_layout: wgpu::BindGroupLayout,
    tex_layout: wgpu::BindGroupLayout,
    pair_layout: wgpu::BindGroupLayout,
    #[allow(dead_code)]
    blur_params_layout: wgpu::BindGroupLayout,
    globals: wgpu::Buffer,
    /// 玻璃樣式表（`theme::glass_looks`）。每次 render 都重寫，調值不必重開
    looks: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    /// 模糊參數的環形緩衝。**一幀裡的每一趟都要拿到自己的那一格。**
    ///
    /// `queue.write_buffer` 不是照錄製順序插進命令流裡的，它是在 submit
    /// 之前一次全部灌進去 —— 同一幀寫兩次同一個 buffer，兩趟看到的都是
    /// 最後那次的值。背景那層（σ 70）因此一直在用玻璃那層（σ 30）的參數，
    /// 而且不會有任何錯誤訊息。
    params: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    cursor: usize,
    sampler: wgpu::Sampler,
    instances: wgpu::Buffer,
    capacity: u64,
    /// 模糊來回用的暫存。key 是（降取樣倍率, σ）—— 只看倍率的話，
    /// 同一幀裡兩個倍率相同的模糊會寫進同一張材質，先算完的那張會被蓋掉。
    blurs: std::collections::HashMap<(u32, u32), (Offscreen, Offscreen)>,
    /// 折射用的半解析度清晰圖
    sharp: Option<Offscreen>,
    /// 清晰圖與毛玻璃之間的那層（`theme::SIGMA_GLASS_MID`），跟 `sharp` 一起更新
    mid: Option<wgpu::TextureView>,
    /// 量背景用的寬模糊（`theme::SIGMA_GLASS_WIDE`），跟 `sharp` 一起更新
    wide: Option<wgpu::TextureView>,
    probes: Vec<Probe>,
    /// 依 `call` 存的最新讀回結果
    snaps: Vec<Option<Snap>>,
    /// 這一幀已經叫過幾次 `sharp_scene`。每個場景叫一次，所以也是場景的序號
    call: usize,
    /// 明暗直接跳到定位、不做過渡（`--shot` 用）
    instant: bool,
    tones: std::collections::HashMap<&'static str, Tone>,
    touches: std::collections::HashMap<&'static str, TouchState>,
    /// 融合的玻璃互相要看得到對方的形狀：每組的矩形與圓角
    shapes: wgpu::Buffer,
    /// 指標位置（邏輯像素），不在視窗裡是 `None`
    pointer: Option<(f32, f32)>,
    /// 左鍵按著
    down: bool,
    focused: bool,
    /// 失去焦點的程度 0..1（有過渡）
    unfocus: f32,
    focus_at: std::time::Instant,
    panels: Vec<Panel>,
}

const INITIAL_CAPACITY: u64 = 64;
/// 環形緩衝的格數。一幀最多用掉「模糊次數 x 3」格，留寬一點。
const PARAM_SLOTS: usize = 32;

/// 依 σ 挑降取樣倍率：小圖上的 σ 要落在 5 個 texel 以內。
///
/// 高斯那兩趟固定一個 texel 走一步、單邊 16 個 tap，蓋得到 ±16 個 texel。
/// σ 超過 5 的話尾巴被砍掉，等效 σ 縮水，看起來就是「模糊不夠」。
/// 反過來 σ 太小就不必降那麼多，否則整張圖被糊掉的是細節不是頻率。
fn blur_div(sigma: f32) -> u32 {
    ((sigma / 5.0).ceil().max(2.0) as u32).next_power_of_two().min(32)
}

impl Glass {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let device = &gpu.device;

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glass globals"),
            entries: &[
                uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                uniform_entry(1, wgpu::ShaderStages::VERTEX_FRAGMENT),
                uniform_entry(2, wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let blur_params_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur params"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::FRAGMENT)],
        });
        // 玻璃要兩張背景：模糊的那張給本體，近乎清晰的那張給邊緣折射。
        // **只有一張是不夠的** —— 位移一張已經糊掉的圖，位移前後長得一樣，
        // 折射等於沒發生，看起來就只是一塊毛玻璃。
        // 模糊圖、取樣器與寬模糊頂點著色器也要用：每塊玻璃底下與四周的亮度在那裡量
        let tex_entry = |binding: u32, visibility: wgpu::ShaderStages| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let pair_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glass backdrop pair"),
            entries: &[
                tex_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex_entry(4, wgpu::ShaderStages::VERTEX_FRAGMENT),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glass backdrop"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        // 解析度、失焦程度，加上這批玻璃裡最亮的那團互動光（映到鄰近的玻璃上）
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glass globals"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let rows = pack_all(crate::theme::glass_looks());
        let looks = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glass looks"),
            size: std::mem::size_of_val(rows.as_slice()) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shapes = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glass shapes"),
            size: (SHAPE_SLOTS * 32) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &globals_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: looks.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: shapes.as_entire_binding() },
            ],
        });

        let mut params = Vec::with_capacity(PARAM_SLOTS);
        for _ in 0..PARAM_SLOTS {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("blur params"),
                // vec2 + vec2 + f32 + pad
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &blur_params_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() }],
            });
            params.push((buf, bind));
        }

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glass sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        // ---- 玻璃管線 ----
        let shader = device.create_shader_module(wgpu::include_wgsl!("glass.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&globals_layout), Some(&pair_layout)],
            immediate_size: 0,
        });
        let attrs: Vec<wgpu::VertexAttribute> = (0..10)
            .map(|i| wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: i * 16,
                shader_location: i as u32,
            })
            .collect();
        let glass_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glass"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Panel>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attrs,
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        // ---- 模糊管線 ----
        let blur_shader = device.create_shader_module(wgpu::include_wgsl!("blur.wgsl"));
        let blur_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&blur_params_layout), Some(&tex_layout)],
            immediate_size: 0,
        });
        let blur_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blur"),
            layout: Some(&blur_layout),
            vertex: wgpu::VertexState {
                module: &blur_shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &blur_shader,
                entry_point: Some("fs"),
                targets: &[Some(format.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        let down_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blur down"),
            layout: Some(&blur_layout),
            vertex: wgpu::VertexState {
                module: &blur_shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &blur_shader,
                entry_point: Some("fs_down"),
                targets: &[Some(format.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glass instances"),
            size: INITIAL_CAPACITY * std::mem::size_of::<Panel>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            glass_pipe,
            blur_pipe,
            down_pipe,
            globals_layout,
            tex_layout,
            pair_layout,
            blur_params_layout,
            globals,
            looks,
            globals_bind,
            params,
            cursor: 0,
            sampler,
            instances,
            capacity: INITIAL_CAPACITY,
            blurs: std::collections::HashMap::new(),
            sharp: None,
            mid: None,
            wide: None,
            probes: Vec::new(),
            snaps: Vec::new(),
            call: 0,
            instant: false,
            tones: std::collections::HashMap::new(),
            touches: std::collections::HashMap::new(),
            shapes,
            pointer: None,
            down: false,
            focused: true,
            unfocus: 0.0,
            focus_at: std::time::Instant::now(),
            panels: Vec::new(),
        }
    }

    pub fn begin(&mut self) {
        self.panels.clear();
    }

    /// 每幀開頭叫一次：場景序號從 0 重新數
    pub fn end_frame(&mut self) {
        self.call = 0;
    }

    /// 明暗直接跳到定位、不做過渡。`--shot` 用，每張圖才會是確定的
    pub fn set_instant(&mut self, on: bool) {
        self.instant = on;
    }

    /// 這塊玻璃該往淺色翻多少（0..1）。`key` 認元件（狀態跨幀保留），`rect` 是它的位置。
    /// 要在這個場景的 `sharp_scene` 之後叫。
    ///
    /// 只給小元件用 —— 選單、側欄這種大面板不翻（Liquid Glass 的規則）
    pub fn tone(&mut self, key: &'static str, rect: [f32; 4]) -> f32 {
        use crate::theme::{dur, mode, Mode, TONE_DOWN, TONE_UP};
        let lum = self.call.checked_sub(1).and_then(|k| self.snaps.get(k)).and_then(|s| s.as_ref()).and_then(|s| s.luma(rect));
        let now = std::time::Instant::now();
        let st = self.tones.entry(key).or_insert(Tone { light: false, value: 0.0, at: now });
        if let Some(l) = lum {
            // 淺色外觀反過來：背景夠暗才翻（第 6 階段才真的用到）
            let l = if mode() == Mode::Light { 1.0 - l } else { l };
            if st.light && l < TONE_DOWN {
                st.light = false;
            } else if !st.light && l > TONE_UP {
                st.light = true;
            }
        }
        if std::env::var_os("VITRO_TONE_DEBUG").is_some() {
            println!("tone {key} 背景亮度={lum:?} 淺色={}", st.light);
        }
        let goal = if st.light { 1.0 } else { 0.0 };
        if self.instant {
            st.value = goal;
        } else {
            let step = now.duration_since(st.at).as_secs_f32() / dur::TONE;
            st.value = if goal > st.value { (st.value + step).min(goal) } else { (st.value - step).max(goal) };
        }
        st.at = now;
        crate::anim::EASE.at(st.value)
    }

    /// 指標與左鍵，每幀開頭由主程式給。`at` 是邏輯像素，不在視窗裡給 `None`
    pub fn set_pointer(&mut self, at: Option<(f32, f32)>, down: bool) {
        self.pointer = at;
        self.down = down;
    }

    /// 視窗有沒有焦點。沒有的話玻璃變淡退後（亮環、飽和度、投影都收一點）
    pub fn set_focus(&mut self, on: bool) {
        self.focused = on;
    }

    /// 失去焦點的程度，順便往目標走一步
    fn unfocus_now(&mut self) -> f32 {
        let now = std::time::Instant::now();
        let goal = if self.focused { 0.0 } else { 1.0 };
        if self.instant {
            self.unfocus = goal;
        } else {
            let step = now.duration_since(self.focus_at).as_secs_f32() / crate::theme::dur::FOCUS;
            self.unfocus = if goal > self.unfocus { (self.unfocus + step).min(goal) } else { (self.unfocus - step).max(goal) };
        }
        self.focus_at = now;
        crate::anim::EASE.at(self.unfocus)
    }

    /// 這塊玻璃的互動狀態。`key` 認元件（狀態跨幀保留），`rect` 是它的位置，
    /// `hot` 是呼叫端的命中測試說「指著它（或它上面的某顆按鈕）」。
    /// `spot` 是被指著的那顆按鈕的中心：指標不在這塊上的時候（`--shot`、鍵盤）光從那裡擴散
    pub fn touch(&mut self, key: &'static str, rect: [f32; 4], hot: bool, spot: (f32, f32)) -> Touch {
        use crate::theme::dur;
        let now = std::time::Instant::now();
        let inside = |p: (f32, f32)| p.0 >= rect[0] && p.0 <= rect[0] + rect[2] && p.1 >= rect[1] && p.1 <= rect[1] + rect[3];
        let ptr = self.pointer.filter(|p| inside(*p));
        let (down, instant) = (self.down, self.instant);
        let st = self.touches.entry(key).or_insert(TouchState { hover: 0.0, press: 0.0, vel: 0.0, at: spot, t: now });
        if hot {
            st.at = ptr.unwrap_or(spot);
        }
        let pressing = hot && down && ptr.is_some();
        let (hg, pg) = (if hot { 1.0 } else { 0.0 }, if pressing { 1.0 } else { 0.0 });
        if instant {
            st.hover = hg;
            st.press = pg;
            st.vel = 0.0;
        } else {
            // 一幀最多算 0.1 秒：視窗被拖著或藏起來的那段不要一次跳完
            let dt = now.duration_since(st.t).as_secs_f32().min(0.1);
            let step = dt / dur::TOUCH;
            st.hover = if hg > st.hover { (st.hover + step).min(hg) } else { (st.hover - step).max(hg) };
            (st.press, st.vel) = spring(st.press, st.vel, pg, dt);
        }
        st.t = now;
        Touch { hover: crate::anim::EASE.at(st.hover), press: st.press, at: st.at }
    }

    pub fn push(&mut self, mut p: Panel) {
        // `Panel::tone` 是「往外觀模式的反面翻多少」（跟字的 `scoped_tone` 同一個意思），
        // 著色器要的是絕對值：0 = 深色玻璃、1 = 淺色玻璃。淺色模式在這裡反過來，
        // 沒叫 `tone` 的大面板也就跟著外觀是淺色玻璃
        if crate::theme::mode() == crate::theme::Mode::Light {
            p.fade[2] = 1.0 - p.fade[2];
        }
        self.panels.push(p);
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.panels.is_empty()
    }

    fn bind_tex(&self, gpu: &Gpu, view: &wgpu::TextureView) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.tex_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// 拿一格參數緩衝並填好。回傳格號 —— 直接回傳 `&BindGroup` 會把
    /// `&mut self` 借出去，後面就沒辦法再拿 `&mut self` 去建材質了。
    fn param(&mut self, gpu: &Gpu, inv: (f32, f32), dir: (f32, f32), sigma: f32) -> usize {
        let k = self.cursor % PARAM_SLOTS;
        self.cursor += 1;
        gpu.queue.write_buffer(
            &self.params[k].0,
            0,
            bytemuck::cast_slice(&[inv.0, inv.1, dir.0, dir.1, sigma, 0.0, 0.0, 0.0]),
        );
        k
    }

    fn bind_pair(
        &self,
        gpu: &Gpu,
        blurred: &wgpu::TextureView,
        sharp: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        // 沒先叫 sharp_scene 的話沒有中間層與寬模糊，退回清晰圖與模糊圖
        let mid = self.mid.as_ref().unwrap_or(sharp);
        let wide = self.wide.as_ref().unwrap_or(blurred);
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.pair_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(blurred),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(sharp),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(mid),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(wide),
                },
            ],
        })
    }

    /// 折射要用的那張：半解析度、只做一次盒狀平均，幾乎沒糊。
    ///
    /// 不能直接綁 `scene` 本身 —— 玻璃就是畫在 scene 上，同一張材質不能
    /// 同時當算圖目標跟取樣來源（wgpu 會直接擋下來）。半解析度也夠用，
    /// 折射帶只有二十幾個像素寬。
    pub fn sharp_scene(
        &mut self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        scene: &Offscreen,
    ) -> wgpu::TextureView {
        // 量背景用的寬模糊，順便讀回一份給 CPU 決定玻璃上的字要深還是淺
        let wide = self.blur_scene(gpu, enc, scene, crate::theme::SIGMA_GLASS_WIDE);
        self.wide = Some(wide);
        self.probe(gpu, enc, crate::gfx::logical(scene.width, scene.height));

        let (bw, bh) = ((scene.width / 2).max(1), (scene.height / 2).max(1));
        let tex = match self.sharp.take() {
            Some(t) if t.width == bw && t.height == bh => t,
            _ => Offscreen::new(&gpu.device, bw, bh),
        };
        // 清晰圖每次都算：玻璃出現與消失的途中是一片彎著清晰背景的鏡片（`Panel::materialize`）
        let p = self.param(gpu, (1.0 / bw as f32, 1.0 / bh as f32), (0.0, 0.0), 2.0);
        let src = self.bind_tex(gpu, &scene.view);
        self.pass(enc, &self.down_pipe, &tex.view, p, &src);
        let out = tex.texture.create_view(&Default::default());
        self.sharp = Some(tex);
        // 樣式表裡沒有一列要邊緣清晰的話，著色器給中間層的權重是 0，不必算
        if !crate::theme::glass_looks().iter().any(|l| l.edge_clear > 0.0) {
            self.mid = None;
            return out;
        }
        // 中間層。模糊圖的快取以 σ 為鍵，不會跟各場景自己的那張撞在一起
        let mid = self.blur_scene(gpu, enc, scene, crate::theme::SIGMA_GLASS_MID);
        self.mid = Some(mid);
        out
    }

    /// 把剛算好的寬模糊錄一趟複製到讀回緩衝，並收下之前已經讀回來的。
    ///
    /// **一個 encoder 裡只能叫一次。** 開始映射的是之前錄好的那些，它們必須
    /// 已經送出去了；同一個 encoder 錄兩次的話，第二次會去映射還沒送出的緩衝。
    fn probe(&mut self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, logical: (f32, f32)) {
        use std::sync::atomic::Ordering;
        let call = self.call;
        self.call += 1;

        // 之前的場景錄好的已經送出去了，可以開始映射。`--shot` 當場等到好，
        // 每一幀用的就一定是上一幀的背景，圖才會每次都一樣
        self.start_maps();
        let _ = gpu.device.poll(if self.instant { wgpu::PollType::wait_indefinitely() } else { wgpu::PollType::Poll });
        self.collect();

        // 寬模糊在 `blurs` 裡的那張。鍵的算法跟 `blur_scene` 一樣
        let sigma = crate::theme::SIGMA_GLASS_WIDE * crate::gfx::scale();
        let Some((src, _)) = self.blurs.get(&(blur_div(sigma), sigma.to_bits())) else { return };
        let (w, h) = (src.width, src.height);
        let bpr = (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let idle = self.probes.iter().position(|p| p.state.load(Ordering::Acquire) == PROBE_IDLE);
        let k = match idle {
            Some(k) if self.probes[k].size == (w, h) => k,
            Some(k) => {
                self.probes[k] = new_probe(gpu, w, h, bpr);
                k
            }
            None if self.probes.len() < PROBE_SLOTS => {
                self.probes.push(new_probe(gpu, w, h, bpr));
                self.probes.len() - 1
            }
            // 全都還在路上：這次不拍，沿用舊的
            None => return,
        };
        let p = &mut self.probes[k];
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &src.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &p.buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        p.call = call;
        p.logical = logical;
        p.state.store(PROBE_COPIED, Ordering::Release);
    }

    /// 已經送出去的讀回全部等到好、收下來。`--shot` 在兩次作畫之間叫。
    ///
    /// 不能在 `sharp_scene` 裡當場送出去等：`QuadRenderer` 的資料是 submit 前
    /// 才一次寫上去的，中途送出的話背景那幾趟會畫到上一幀的矩形
    pub fn settle(&mut self, gpu: &Gpu) {
        self.start_maps();
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        self.collect();
    }

    /// 錄好複製、而且已經送出去的，開始映射
    fn start_maps(&self) {
        use std::sync::atomic::Ordering;
        for p in &self.probes {
            if p.state.load(Ordering::Acquire) == PROBE_COPIED {
                p.state.store(PROBE_MAPPING, Ordering::Release);
                let st = p.state.clone();
                p.buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                    st.store(if r.is_ok() { PROBE_READY } else { PROBE_FAILED }, Ordering::Release);
                });
            }
        }
    }

    /// 映射好的讀回緩衝搬進 `snaps`，緩衝還回去
    fn collect(&mut self) {
        use std::sync::atomic::Ordering;
        for p in &mut self.probes {
            match p.state.load(Ordering::Acquire) {
                PROBE_READY => {
                    let (w, h) = p.size;
                    let mut px = Vec::with_capacity((w * h * 4) as usize);
                    if let Ok(data) = p.buf.slice(..).get_mapped_range() {
                        for row in 0..h {
                            let at = (row * p.bpr) as usize;
                            px.extend_from_slice(&data[at..at + (w * 4) as usize]);
                        }
                    }
                    p.buf.unmap();
                    p.state.store(PROBE_IDLE, Ordering::Release);
                    if px.len() == (w * h * 4) as usize {
                        if self.snaps.len() <= p.call {
                            self.snaps.resize_with(p.call + 1, || None);
                        }
                        self.snaps[p.call] = Some(Snap { w, h, logical: p.logical, px });
                    }
                }
                PROBE_FAILED => p.state.store(PROBE_IDLE, Ordering::Release),
                _ => {}
            }
        }
    }

    /// 把 `scene` 模糊成可以當背景取樣的那張圖。回傳模糊後的 view。
    ///
    /// `sigma` 就是 CSS `blur()` 裡的那個數字 —— 它是標準差，不是半徑。
    /// 兩者差三倍，當成半徑用的話看起來就只是「有點糊」而不是毛玻璃。
    pub fn blur_scene(
        &mut self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        scene: &Offscreen,
        sigma: f32,
    ) -> wgpu::TextureView {
        // σ 是邏輯像素，模糊是在實體解析度的材質上做的
        let sigma = sigma * crate::gfx::scale();
        let div = blur_div(sigma);
        let (bw, bh) = ((scene.width / div).max(1), (scene.height / div).max(1));
        // 先整組搬出來再用。留在 map 裡的話 `self.blurs` 的可變借用會擋住
        // 後面的 `self.bind_tex`／`self.blur_pass`。
        let key = (div, sigma.to_bits());
        let mut pair = self.blurs.remove(&key).filter(|p| p.0.width == bw && p.0.height == bh);
        let (ping, pong) = pair.take().unwrap_or_else(|| {
            (Offscreen::new(&gpu.device, bw, bh), Offscreen::new(&gpu.device, bw, bh))
        });

        let (inv_w, inv_h) = (1.0f32 / bw as f32, 1.0f32 / bh as f32);
        // 降取樣那趟借 sigma 這格傳「降幾倍」
        let p_down = self.param(gpu, (inv_w, inv_h), (0.0, 0.0), div as f32);
        // 小圖上的 σ。兩趟的方向不同、其餘一樣。
        let st = sigma / div as f32;
        let p_h = self.param(gpu, (inv_w, inv_h), (inv_w, 0.0), st);
        let p_v = self.param(gpu, (inv_w, inv_h), (0.0, inv_h), st);
        if std::env::var_os("VITRO_BLUR_DEBUG").is_some() {
            println!("blur σ={sigma} div={div} 小圖={bw}x{bh} σ_texel={st} slots={p_down}/{p_h}/{p_v}");
        }

        // 第一趟：scene（全解析度）→ ping（小圖），盒狀平均
        let src_bind = self.bind_tex(gpu, &scene.view);
        self.pass(enc, &self.down_pipe, &ping.view, p_down, &src_bind);
        // 第二、三趟：水平、垂直高斯
        let ping_bind = self.bind_tex(gpu, &ping.view);
        self.pass(enc, &self.blur_pipe, &pong.view, p_h, &ping_bind);
        let pong_bind = self.bind_tex(gpu, &pong.view);
        self.pass(enc, &self.blur_pipe, &ping.view, p_v, &pong_bind);

        // 回傳新建的 view 而不是借出 self 裡那個 —— 借出去的話呼叫端就沒辦法
        // 再拿 &mut self 去畫玻璃了。建 view 很便宜。
        let out = ping.texture.create_view(&Default::default());
        self.blurs.insert(key, (ping, pong));
        out
    }

    fn pass(
        &self,
        enc: &mut wgpu::CommandEncoder,
        pipe: &wgpu::RenderPipeline,
        target: &wgpu::TextureView,
        param: usize,
        src: &wgpu::BindGroup,
    ) {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blur"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipe);
        pass.set_bind_group(0, &self.params[param].1, &[]);
        pass.set_bind_group(1, src, &[]);
        pass.draw(0..3, 0..1);
    }

    /// 畫玻璃面板。`backdrop` 是 `blur_scene` 的輸出。
    pub fn render(
        &mut self,
        gpu: &Gpu,
        pass: &mut wgpu::RenderPass<'_>,
        backdrop: &wgpu::TextureView,
        sharp: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        if self.panels.is_empty() {
            return;
        }
        let needed = self.panels.len() as u64;
        if needed > self.capacity {
            self.capacity = needed.next_power_of_two();
            self.instances = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("glass instances"),
                size: self.capacity * std::mem::size_of::<Panel>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let (panels, shapes) = resolve_merge(&self.panels);
        gpu.queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&panels));
        gpu.queue.write_buffer(&self.shapes, 0, bytemuck::cast_slice(&shapes));
        // 面板座標是邏輯像素；`screen / res` 取樣背景，跟背景圖的實體解析度無關
        let (lw, lh) = crate::gfx::logical(width, height);
        let unfocus = self.unfocus_now();
        let (wx, wy, wi) = world_light(&panels);
        gpu.queue.write_buffer(&self.globals, 0, bytemuck::cast_slice(&[lw, lh, unfocus, 0.0, wx, wy, wi, 0.0]));
        let rows = pack_all(crate::theme::glass_looks());
        gpu.queue.write_buffer(&self.looks, 0, bytemuck::cast_slice(&rows));
        let bind = self.bind_pair(gpu, backdrop, sharp);
        pass.set_pipeline(&self.glass_pipe);
        pass.set_bind_group(0, &self.globals_bind, &[]);
        pass.set_bind_group(1, &bind, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..6, 0..self.panels.len() as u32);
    }
}

/// 互動光有多亮，跟 `glass.wgsl` 的算法一樣：指著是按下的幾成（樣式表的 `touch_glow.2`）
fn touch_intensity(p: &Panel) -> f32 {
    let looks = crate::theme::glass_looks();
    let h = looks.get(p.fade[1] as usize).unwrap_or(&looks[0]).touch_glow.2;
    p.touch[2] * (h + p.touch[3].clamp(0.0, 1.0) * (1.0 - h))
}

/// 這批玻璃裡最亮的那團互動光：(x, y, 亮度)。鄰近的玻璃邊緣會映到它
fn world_light(panels: &[Panel]) -> (f32, f32, f32) {
    panels
        .iter()
        .map(|p| (p.touch[0], p.touch[1], touch_intensity(p)))
        .fold((0.0, 0.0, 0.0), |a, b| if b.2 > a.2 { b } else { a })
}

/// 把 `merge` 的組號換成形狀表的位置。同一組要連著推進去；
/// 形狀表滿了的那組就不融合（各畫各的，不會錯，只是沒有頸子）
fn resolve_merge(panels: &[Panel]) -> (Vec<Panel>, Vec<[f32; 4]>) {
    let mut out = panels.to_vec();
    let mut shapes = vec![[0.0f32; 4]; SHAPE_SLOTS * 2];
    let mut used = 0usize;
    let mut i = 0usize;
    while i < out.len() {
        let g = out[i].merge[0];
        let mut j = i + 1;
        while g > 0.0 && j < out.len() && out[j].merge[0] == g {
            j += 1;
        }
        let n = j - i;
        if g <= 0.0 || n < 2 || used + n > SHAPE_SLOTS {
            for p in &mut out[i..j] {
                p.merge = [0.0; 4];
            }
        } else {
            for (k, p) in out[i..j].iter_mut().enumerate() {
                shapes[(used + k) * 2] = p.rect;
                shapes[(used + k) * 2 + 1] = p.radius;
                p.merge = [(used + k) as f32, used as f32, n as f32, p.merge[3]];
            }
            used += n;
        }
        i = j;
    }
    (out, shapes)
}

fn new_probe(gpu: &Gpu, w: u32, h: u32, bpr: u32) -> Probe {
    Probe {
        buf: gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glass probe"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        }),
        state: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(PROBE_IDLE)),
        size: (w, h),
        bpr,
        call: 0,
        logical: (1.0, 1.0),
    }
}

fn uniform_entry(binding: u32, vis: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: vis,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 沒按就不變形() {
        let r = [10.0, 20.0, 100.0, 40.0];
        assert_eq!(gel_rect(r, 0.0, (60.0, 40.0)), r);
    }

    #[test]
    fn 按下往指標那邊長() {
        let r = [0.0, 0.0, 100.0, 40.0];
        // 指在右緣中間：變寬比變高多，中心往右偏
        let g = gel_rect(r, 1.0, (100.0, 20.0));
        assert!(g[2] > r[2] && g[3] > r[3]);
        assert!(g[2] / r[2] > g[3] / r[3]);
        assert!(g[0] + g[2] * 0.5 > 50.0);
        // 彈回來那下比原本小
        let s = gel_rect(r, -0.3, (50.0, 20.0));
        assert!(s[2] < r[2] && s[3] < r[3]);
    }

    #[test]
    fn 放開後彈回來會晃一下再停住() {
        // 按到底之後放開：會短暫小於 0（縮一點），最後停在 0
        let (mut x, mut v) = (1.0f32, 0.0f32);
        let mut low = 1.0f32;
        for _ in 0..120 {
            (x, v) = spring(x, v, 0.0, 1.0 / 60.0);
            low = low.min(x);
        }
        assert!(low < -0.05, "沒有晃：最低 {low}");
        assert_eq!((x, v), (0.0, 0.0));
        // 一幀很久（視窗被拖著）也不會炸開
        let (x, _) = spring(1.0, 0.0, 0.0, 0.1);
        assert!(x.abs() <= 1.0);
    }

    #[test]
    fn 融合只認連著推的同一組() {
        let p = |g: u32| Panel::new(0.0, 0.0, 10.0, 10.0).merge(g, 30.0);
        let (out, shapes) = resolve_merge(&[Panel::new(0.0, 0.0, 5.0, 5.0), p(1), p(1), p(2), p(1)]);
        assert_eq!(out[0].merge, [0.0; 4]);
        // 第一組兩塊佔形狀表 0、1 格
        assert_eq!(out[1].merge, [0.0, 0.0, 2.0, 30.0]);
        assert_eq!(out[2].merge, [1.0, 0.0, 2.0, 30.0]);
        // 只有自己一塊的組不融合
        assert_eq!(out[3].merge, [0.0; 4]);
        assert_eq!(out[4].merge, [0.0; 4]);
        assert_eq!(shapes[0], [0.0, 0.0, 10.0, 10.0]);
    }

    #[test]
    fn 形狀表滿了就不融合() {
        let ps: Vec<Panel> = (0..SHAPE_SLOTS + 2).map(|_| Panel::new(0.0, 0.0, 10.0, 10.0).merge(1, 30.0)).collect();
        let (out, _) = resolve_merge(&ps);
        assert!(out.iter().all(|p| p.merge == [0.0; 4]));
    }

    #[test]
    fn 最亮的那團光() {
        let t = |h: f32, pr: f32, x: f32| Panel::default().touch(&Touch { hover: h, press: pr, at: (x, 0.0) }, false);
        assert_eq!(world_light(&[Panel::default()]).2, 0.0);
        let (x, _, i) = world_light(&[t(1.0, 0.0, 1.0), t(1.0, 1.0, 2.0), t(0.5, 1.0, 3.0)]);
        assert_eq!(x, 2.0);
        assert!((i - 1.0).abs() < 1e-6);
    }
}
