//! MilkDrop 引擎。
//!
//! 核心是一個回授迴圈：上一幀的畫面經過一張**變形網格**重新取樣、壓暗一點，
//! 就成為這一幀的底；再把波形畫上去。那個「無限往內捲」的視覺就是這樣來的。
//!
//! ```text
//!   prev ──(warp mesh 取樣 + decay)──► curr ──(畫波形)──► curr ──(comp)──► 畫面
//!     ▲                                                  │
//!     └──────────────────  交換  ─────────────────────────┘
//! ```
//!
//! 網格的每個頂點都要跑一次 `pixel_eqs`，算出它該去上一幀的哪個位置取樣。
//! 預設是 48×36 格 = 1813 個頂點，每幀 1813 次直譯 —— 這就是為什麼
//! [`crate::eel`] 要把變數解析成槽位而不是每次查名字。
//!
//! 變形的公式照抄 butterchurn 的 `runPixelEquations`，一個常數都沒改。

use std::collections::HashMap;

use crate::audio;
use crate::eel::{self, Vm};
use crate::gfx::{self, Gpu, Offscreen};
use crate::preset::{compile, Program, RawPreset};

/// 回授緩衝用浮點格式。decay 會讓值反覆相乘，8-bit 會很快踩到色階。
// **8 位元不是省資源，是保真度。** MilkDrop 的回授緩衝就是 8-bit：
// 每次寫入都自動夾在 [0,1]，很多 preset 的亮度平衡靠的就是這個夾值。
// 用 16-bit 浮點的話加法混合會一路累加上去，貼圖形狀取樣上一幀再畫回去
// 就變成正回授 —— 整片白掉。
const FEEDBACK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshVert {
    /// 裁剪空間位置
    pos: [f32; 2],
    /// 要去上一幀取樣的 uv
    uv: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct WarpUniforms {
    /// x=decay y=echo_alpha z=echo_zoom w=echo_orient
    misc: [f32; 4],
    /// 純色邊框等等之後再加，先留著對齊
    _pad: [f32; 4],
}

/// 內建合成階段的參數。**這張表對應 butterchurn 在 comp 著色器留空時
/// 用的那段預設程式**，110 個 preset 走這條路：其中 55 個要 echo、
/// 44 個要 brighten/darken/solarize/invert、30 個要 hue。
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CompUniforms {
    /// x=gamma y=brighten z=darken w=solarize
    misc: [f32; 4],
    /// xy=畫面尺寸 z=invert w=fshader
    res: [f32; 4],
    /// x=echo_alpha y=echo_zoom z=echo_orient
    echo: [f32; 4],
    /// hue_shader 的四個角
    hue: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct WaveVert {
    pos: [f32; 2],
    color: [f32; 4],
}

/// 貼圖形狀的頂點。比 `WaveVert` 多一組 uv。
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct ShapeTexVert {
    pos: [f32; 2],
    color: [f32; 4],
    uv: [f32; 2],
}

/// 一組「這一幀的聲音」。MilkDrop 的方程式全靠這幾個數字驅動。
#[derive(Clone, Copy)]
pub struct Sound {
    pub bass: f32,
    pub mid: f32,
    pub treb: f32,
    pub bass_att: f32,
    pub mid_att: f32,
    pub treb_att: f32,
    /// `spectrum=1` 的 custom wave 要的是頻譜而不是波形
    pub bins: [f32; audio::BINS],
}

// `[f32; 128]` 沒有現成的 Default，只好手寫。
impl Default for Sound {
    fn default() -> Self {
        Self {
            bass: 0.0,
            mid: 0.0,
            treb: 0.0,
            bass_att: 0.0,
            mid_att: 0.0,
            treb_att: 0.0,
            bins: [0.0; audio::BINS],
        }
    }
}

impl Sound {
    /// `_att` 是「衰減後的」版本 —— 比瞬時值慢，方程式常用它來做長期趨勢。
    pub fn update(&mut self, s: &audio::Spectrum) {
        // 頻譜是 0..1 的 dB 壓縮值，MilkDrop 期待的是「相對平均音量的倍率」，
        // 1.0 大約是平常音量。乘 2 之後動態範圍跟原版接近。
        self.bins = s.bins;
        self.bass = s.bass * 2.0;
        self.mid = s.mid * 2.0;
        self.treb = s.treb * 2.0;
        let k = 0.06;
        self.bass_att += (self.bass - self.bass_att) * k;
        self.mid_att += (self.mid - self.mid_att) * k;
        self.treb_att += (self.treb - self.treb_att) * k;
    }
}

/// 方程式讀寫的內建變數，預先解析成槽位，每幀不用再查名字。
struct Slots {
    time: usize,
    fps: usize,
    frame: usize,
    bass: usize,
    mid: usize,
    treb: usize,
    bass_att: usize,
    mid_att: usize,
    treb_att: usize,
    meshx: usize,
    meshy: usize,
    aspectx: usize,
    aspecty: usize,
    pixelsx: usize,
    pixelsy: usize,
    // pixel_eqs 的輸入
    x: usize,
    y: usize,
    rad: usize,
    ang: usize,
    // pixel_eqs 的輸出
    zoom: usize,
    zoomexp: usize,
    rot: usize,
    warp: usize,
    cx: usize,
    cy: usize,
    dx: usize,
    dy: usize,
    sx: usize,
    sy: usize,
    // frame 傳給 pixel 的溝通變數
    q: [usize; 32],
}

impl Slots {
    fn new(v: &mut eel::Vars) -> Self {
        let mut q = [0usize; 32];
        for (i, slot) in q.iter_mut().enumerate() {
            *slot = v.slot(&format!("q{}", i + 1));
        }
        Self {
            time: v.slot("time"),
            fps: v.slot("fps"),
            frame: v.slot("frame"),
            bass: v.slot("bass"),
            mid: v.slot("mid"),
            treb: v.slot("treb"),
            bass_att: v.slot("bass_att"),
            mid_att: v.slot("mid_att"),
            treb_att: v.slot("treb_att"),
            meshx: v.slot("meshx"),
            meshy: v.slot("meshy"),
            aspectx: v.slot("aspectx"),
            aspecty: v.slot("aspecty"),
            pixelsx: v.slot("pixelsx"),
            pixelsy: v.slot("pixelsy"),
            x: v.slot("x"),
            y: v.slot("y"),
            rad: v.slot("rad"),
            ang: v.slot("ang"),
            zoom: v.slot("zoom"),
            zoomexp: v.slot("zoomexp"),
            rot: v.slot("rot"),
            warp: v.slot("warp"),
            cx: v.slot("cx"),
            cy: v.slot("cy"),
            dx: v.slot("dx"),
            dy: v.slot("dy"),
            sx: v.slot("sx"),
            sy: v.slot("sy"),
            q,
        }
    }
}

/// baseVals 裡取一個值，沒有就用預設。
fn bv(map: &HashMap<String, f64>, key: &str, default: f64) -> f64 {
    map.get(key).copied().unwrap_or(default)
}

/// MilkDrop 的內建預設值，照抄 butterchurn 的 `baseVals`。
///
/// **這張表不能省。** 統計過 526 個 preset：`cx` 只有 10 個自己帶、`cy` 4 個、
/// `sy` 41 個 —— 其餘全部靠這些預設。少了它們，`sx=0` 會讓
/// `(u−cx)/sx` 直接炸成幾億，整張網格的取樣座標全部飛出畫面，畫面就是全黑。
const DEFAULTS: &[(&str, f64)] = &[
    ("zoom", 1.0), ("zoomexp", 1.0), ("rot", 0.0), ("warp", 0.01),
    ("cx", 0.5), ("cy", 0.5), ("sx", 1.0), ("sy", 1.0), ("dx", 0.0), ("dy", 0.0),
    ("decay", 0.9), ("gammaadj", 1.25), ("brighten", 0.0), ("darken", 0.0),
    ("darken_center", 0.0), ("solarize", 0.0), ("invert", 0.0), ("red_blue", 0.0),
    ("wrap", 0.0), ("fshader", 0.0),
    ("echo_zoom", 1.0), ("echo_alpha", 0.0), ("echo_orient", 0.0),
    ("warpscale", 1.0), ("warpanimspeed", 1.0),
    ("wave_mode", 0.0), ("wave_r", 0.5), ("wave_g", 0.5), ("wave_b", 0.5), ("wave_a", 1.0),
    ("wave_x", 0.5), ("wave_y", 0.5), ("wave_scale", 1.0), ("wave_smoothing", 0.75),
    ("wave_mystery", -0.2), ("wave_dots", 0.0), ("wave_thick", 0.0), ("wave_brighten", 0.0),
    ("additivewave", 0.0), ("modwavealphabyvolume", 0.0),
    ("modwavealphastart", 0.75), ("modwavealphaend", 0.95),
    ("ob_r", 0.5), ("ob_g", 0.5), ("ob_b", 0.5), ("ob_a", 0.0), ("ob_size", 0.0),
    ("ib_r", 0.5), ("ib_g", 0.5), ("ib_b", 0.5), ("ib_a", 0.0), ("ib_size", 0.0),
    ("mv_x", 12.0), ("mv_y", 9.0), ("mv_dx", 0.0), ("mv_dy", 0.0),
    ("mv_r", 0.5), ("mv_g", 0.5), ("mv_b", 0.5), ("mv_a", 0.0), ("mv_l", 0.0),
    ("bmotionvectorson", 0.0), ("rating", 5.0),
];

/// 形狀的預設值，照抄 butterchurn。
///
/// **這張表跟主表一樣不能省，而且更危險**：JSON 只存非預設的鍵，
/// 而形狀用的名字（`x`、`y`、`rad`、`ang`、`a`、`r`…）全都是 preset 方程式裡
/// 超常見的暫存變數名。不先鋪預設的話，形狀會讀到 `pixel_eqs` 留下來的
/// 殘值 —— 位置亂跑、alpha 變成 1 整片蓋白。
const SHAPE_DEFAULTS: &[(&str, f64)] = &[
    ("enabled", 0.0), ("sides", 4.0), ("additive", 0.0), ("thickoutline", 0.0),
    ("textured", 0.0), ("num_inst", 1.0), ("instance", 0.0),
    ("x", 0.5), ("y", 0.5), ("rad", 0.1), ("ang", 0.0),
    ("r", 1.0), ("g", 0.0), ("b", 0.0), ("a", 1.0),
    ("r2", 0.0), ("g2", 1.0), ("b2", 0.0), ("a2", 0.0),
    ("border_r", 1.0), ("border_g", 1.0), ("border_b", 1.0), ("border_a", 0.1),
    ("tex_ang", 0.0), ("tex_zoom", 1.0),
];

/// custom wave 的預設值，照抄 butterchurn 的 `waveBaseVals`。
///
/// 跟形狀那張表是同一個理由：JSON 只存非預設的鍵，1580 個波形裡
/// 只有 41 個自己帶 `samples`、106 個帶 `scaling`。少了這張表，
/// `samples=0` 直接沒東西可畫，`scaling=0` 則是整條線縮成一點。
const WAVE_DEFAULTS: &[(&str, f64)] = &[
    ("enabled", 0.0), ("samples", 512.0), ("sep", 0.0), ("scaling", 1.0),
    ("smoothing", 0.5), ("spectrum", 0.0), ("usedots", 0.0), ("thick", 0.0),
    ("additive", 0.0), ("r", 1.0), ("g", 1.0), ("b", 1.0), ("a", 1.0),
];

/// 同上，給 custom wave 用。
fn merged_wave(raw: Option<&HashMap<String, f64>>) -> HashMap<String, f64> {
    let mut m: HashMap<String, f64> =
        WAVE_DEFAULTS.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    for (k, v) in raw.into_iter().flatten() {
        m.insert(k.clone(), *v);
    }
    m
}

/// 同上，給 custom shape 用。
fn merged_shape(raw: Option<&HashMap<String, f64>>) -> HashMap<String, f64> {
    let mut m: HashMap<String, f64> =
        SHAPE_DEFAULTS.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    for (k, v) in raw.into_iter().flatten() {
        m.insert(k.clone(), *v);
    }
    m
}

/// 預設值先鋪底，preset 自己帶的再蓋上去。
fn merged_base(raw: &HashMap<String, f64>) -> HashMap<String, f64> {
    let mut m: HashMap<String, f64> =
        DEFAULTS.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    for (k, v) in raw {
        m.insert(k.clone(), *v);
    }
    m
}

pub struct Milk {
    // ---- preset 狀態 ----
    pub name: String,
    raw: RawPreset,
    /// 預設值合併過的 baseVals。所有地方都要用這份，不要直接讀 raw。
    base: HashMap<String, f64>,
    prog: Program,
    vm: Vm,
    slots: Slots,
    mesh_w: usize,
    mesh_h: usize,
    time: f64,
    frame: u64,
    fps: f64,

    // ---- GPU ----
    prev: Offscreen2,
    curr: Offscreen2,
    warp_pipe: wgpu::RenderPipeline,
    wave_pipe: wgpu::RenderPipeline,
    /// `additivewave=1` 的 preset 要的是加法混合而不是 alpha 混合。
    /// 217 個 preset 用到這個旗標 —— 用錯的話波形會亮得蓋掉整個畫面。
    wave_pipe_add: wgpu::RenderPipeline,
    comp_pipe: wgpu::RenderPipeline,
    tex_layout: wgpu::BindGroupLayout,
    #[allow(dead_code)] // 建管線時用過，之後沒有人讀
    uni_layout: wgpu::BindGroupLayout,
    warp_uni: wgpu::Buffer,
    warp_uni_bind: wgpu::BindGroup,
    comp_uni: wgpu::Buffer,
    comp_uni_bind: wgpu::BindGroup,
    /// preset 自帶的著色器管線。編不起來就是 None，退回內建管線。
    pipes: Option<Pipes>,
    assets: std::sync::Arc<crate::milkasset::MilkAssets>,
    globals_buf: wgpu::Buffer,
    /// blur1/2/3，解析度逐級減半
    blur: Vec<Offscreen2>,
    blur_pipe: wgpu::RenderPipeline,
    #[allow(dead_code)]
    wgsl: wgpu::ShaderModule,
    sampler: wgpu::Sampler,
    /// `wrap=1` 的 preset 要的是重複取樣而不是夾邊。兩種都先建好，
    /// 每幀依 preset 挑 —— 這決定畫面跑出邊界時是「抹開」還是「繞回來」。
    sampler_wrap: wgpu::Sampler,
    verts: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    wave_verts: wgpu::Buffer,
    wave_count: u32,
    /// 每段線的 (起點, 長度)。mode 7 是雙線，所以不能只畫一段。
    wave_strips: Vec<(u32, u32)>,
    /// `wave_dots` 的點，展開成小方塊用三角形畫
    wave_dot_tris: Vec<(u32, u32)>,
    /// custom shapes：三角形扇（展開成三角形列）與外框線
    shape_verts: wgpu::Buffer,
    shape_data: Vec<WaveVert>,
    /// (起點, 頂點數, 是否加法混合)
    shape_tris: Vec<(u32, u32, bool)>,
    shape_lines: Vec<(u32, u32)>,
    shape_pipe: wgpu::RenderPipeline,
    shape_pipe_add: wgpu::RenderPipeline,
    /// 貼上一幀畫面的形狀，頂點格式不同所以另開一組
    shape_tex_verts: wgpu::Buffer,
    shape_tex_data: Vec<ShapeTexVert>,
    shape_tex_tris: Vec<(u32, u32, bool)>,
    shape_tex_pipe: wgpu::RenderPipeline,
    shape_tex_pipe_add: wgpu::RenderPipeline,
    mip_pipe: wgpu::RenderPipeline,
    /// custom waves：自己一組緩衝，跟主波形分開
    cwave_verts: wgpu::Buffer,
    cwave_data: Vec<WaveVert>,
    /// (起點, 頂點數, 是否加法混合)
    cwave_strips: Vec<(u32, u32, bool)>,
    cwave_dots: Vec<(u32, u32, bool)>,
    /// 邊框、暗中心、動態向量。這三個都是固定幾何，共用一組緩衝。
    extra_verts: wgpu::Buffer,
    extra_data: Vec<WaveVert>,
    extra_tris: Vec<(u32, u32)>,
    /// 動態向量是**線段對**（LineList），不是連續的線
    extra_lines: Vec<(u32, u32)>,
    mv_pipe: wgpu::RenderPipeline,
    /// 每個 custom wave 自己的變數作用域：(槽位, 上一幀留下的值)。
    /// MilkDrop 裡每個波形的 `t1`、`x` 都是獨立的，這裡靠存取換頁模擬。
    cwave_scope: Vec<Vec<(usize, f64)>>,
    shape_scope: Vec<Vec<(usize, f64)>>,
    /// 這一幀的聲音，`build_wave` 要用（mode 3 的亮度跟 treb 有關）
    snd_bass: f32,
    snd_mid: f32,
    snd_treb: f32,
    /// 長寬比，每幀更新
    aspect: (f64, f64),
    /// hue_shader 的隨機種子，四個一組
    rand_start: [f64; 4],

    // CPU 端的暫存，每幀重填
    vert_data: Vec<MeshVert>,
    wave_data: Vec<WaveVert>,
}

/// custom wave 的頂點上限。四個波形各 512 取樣、平滑後加倍、粗線再乘四，
/// 最壞情況約 64K，這裡留兩倍。
const CWAVE_MAX: usize = 131_072;
const WAVE_MAX: usize = 32_768;
const SHAPE_MAX: usize = 65_536;

/// 浮點回授緩衝。`gfx::Offscreen` 綁死了 sRGB 格式，這裡要自己的。
struct Offscreen2 {
    #[allow(dead_code)]
    texture: wgpu::Texture,
    /// 涵蓋全部 mip 的視圖，取樣用
    view: wgpu::TextureView,
    /// 每一級各自的視圖，產生 mip 時當來源與目標
    mips: Vec<wgpu::TextureView>,
    w: u32,
    h: u32,
}

impl Offscreen2 {
    fn new(device: &wgpu::Device, w: u32, h: u32) -> Self {
        Self::make(device, w, h, false)
    }

    /// 帶 mip 的版本。只有 prev／curr 需要 —— 它們是被當成 `sampler_main`
    /// 取樣的那兩張，blur 金字塔自己就是降取樣，不需要。
    fn mipped(device: &wgpu::Device, w: u32, h: u32) -> Self {
        Self::make(device, w, h, true)
    }

    fn make(device: &wgpu::Device, w: u32, h: u32, mip: bool) -> Self {
        let levels = if mip { (w.max(h) as f32).log2().floor() as u32 + 1 } else { 1 };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("milk feedback"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FEEDBACK_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let mips = (0..levels)
            .map(|i| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: i,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        Self { texture, view, mips, w, h }
    }
}

impl Milk {
    pub fn new(
        gpu: &Gpu,
        assets: std::sync::Arc<crate::milkasset::MilkAssets>,
        name: String,
        raw: RawPreset,
        w: u32,
        h: u32,
        use_preset_shaders: bool,
    ) -> Result<Self, String> {
        let mut prog = compile(&raw)?;
        let slots = Slots::new(&mut prog.vars);
        let mut vm = Vm::new(prog.vars.len());

        // 名字雜湊當亂數種子：同一個 preset 每次的 hue 相位一樣
        let mut seed = 0xcbf2_9ce4_8422_2325u64;
        for b in name.bytes() {
            seed ^= b as u64;
            seed = seed.wrapping_mul(0x100_0000_01b3);
        }
        let mut rand_start = [0.0f64; 4];
        for r in rand_start.iter_mut() {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            *r = (seed >> 11) as f64 / (1u64 << 53) as f64;
        }

        let base = merged_base(&raw.base_vals);
        let mesh_w = bv(&base, "mesh_x", 48.0).clamp(8.0, 128.0) as usize;
        let mesh_h = bv(&base, "mesh_y", 36.0).clamp(8.0, 128.0) as usize;

        // baseVals 是方程式的初始值 —— 先塞進去再跑 init，
        // 不然 preset 裡「zoom += …」這種寫法會從 0 開始而不是從 1。
        for (k, v) in &base {
            let slot = prog.vars.find(k);
            if let Some(s) = slot {
                vm.ensure_vars(prog.vars.len());
                vm.set(s, *v);
            }
        }
        vm.ensure_vars(prog.vars.len());
        vm.run(&prog.init);

        // 每個 custom wave／shape 在 MilkDrop 裡有**自己的變數作用域**：
        // 波形寫的 `t1`、`x`、`sample` 不會污染 preset 的 frame_eqs，
        // 而且它自己的值會一幀幀留著。這邊所有方程式共用一張變數表，
        // 所以先把「這段程式會寫到哪些槽位」框出來，每幀進出時換頁。
        //
        // `init_eqs` 只在載入時跑一次 —— 跑完的值就是這個作用域的起始狀態。
        let scope_extra = ["sample", "value1", "value2", "x", "y", "rad", "ang", "r", "g", "b", "a"];
        let scope_of = |vm: &mut Vm,
                            vars: &eel::Vars,
                            progs: &[&Vec<eel::Stmt>],
                            init: &Vec<eel::Stmt>,
                            base: &HashMap<String, f64>|
         -> Vec<(usize, f64)> {
            let mut slots: Vec<usize> = Vec::new();
            for pr in progs {
                slots.extend(eel::assigned_slots(pr));
            }
            for n in scope_extra {
                if let Some(sl) = vars.find(n) {
                    slots.push(sl);
                }
            }
            slots.sort_unstable();
            slots.dedup();
            // 外層的值等一下要還回去
            let outer: Vec<f64> = slots.iter().map(|sl| vm.get(*sl)).collect();
            for (k, v) in base {
                if let Some(sl) = vars.find(k) {
                    vm.set(sl, *v);
                }
            }
            vm.run(init);
            let saved: Vec<(usize, f64)> =
                slots.iter().map(|sl| (*sl, vm.get(*sl))).collect();
            for (k, sl) in slots.iter().enumerate() {
                vm.set(*sl, outer[k]);
            }
            saved
        };

        let cwave_scope: Vec<Vec<(usize, f64)>> = (0..prog.waves.len())
            .map(|i| {
                let w = &prog.waves[i];
                let b = merged_wave(raw.waves.get(i).map(|x| &x.base_vals));
                scope_of(&mut vm, &prog.vars, &[&w.init, &w.frame, &w.point], &w.init, &b)
            })
            .collect();
        let shape_scope: Vec<Vec<(usize, f64)>> = (0..prog.shapes.len())
            .map(|i| {
                let sh = &prog.shapes[i];
                let b = merged_shape(raw.shapes.get(i).map(|x| &x.base_vals));
                scope_of(&mut vm, &prog.vars, &[&sh.init, &sh.frame], &sh.init, &b)
            })
            .collect();

        let device = &gpu.device;
        let (nx, ny) = (mesh_w + 1, mesh_h + 1);
        let vert_data = vec![MeshVert { pos: [0.0; 2], uv: [0.0; 2] }; nx * ny];

        // 索引只建一次，網格拓撲不會變
        let mut idx: Vec<u32> = Vec::with_capacity(mesh_w * mesh_h * 6);
        for j in 0..mesh_h {
            for i in 0..mesh_w {
                let a = (j * nx + i) as u32;
                let b = a + 1;
                let c = a + nx as u32;
                let d = c + 1;
                idx.extend_from_slice(&[a, b, c, b, d, c]);
            }
        }

        let verts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk mesh"),
            size: (vert_data.len() * std::mem::size_of::<MeshVert>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let indices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk idx"),
            size: (idx.len() * 4) as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue.write_buffer(&indices, 0, bytemuck::cast_slice(&idx));

        let shape_verts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk shapes"),
            // 4 個形狀 × 最多 1024 個實例 × (102 頂點 fan → 300 三角形頂點)
            // 太大，實務上用得到的遠少於此，先給 64K 個頂點再視需要長大
            size: (65536 * std::mem::size_of::<WaveVert>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shape_tex_verts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk textured shapes"),
            size: (SHAPE_MAX * std::mem::size_of::<ShapeTexVert>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let extra_verts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk extras"),
            size: (8192 * std::mem::size_of::<WaveVert>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let cwave_verts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk custom waves"),
            size: (CWAVE_MAX * std::mem::size_of::<WaveVert>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let wave_verts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk wave"),
            // 平滑後點數加倍、粗線畫四次、點模式每點六個頂點 —— 要留得夠
            size: (WAVE_MAX * std::mem::size_of::<WaveVert>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ---- bind group layouts ----
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("milk tex"),
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
        let uni_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("milk uni"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let mk_uni = |label: &str, size: u64| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        // 兩個共用同一個 WGSL struct，所以要一樣大 —— warp 只讀前面幾個欄位
        let uni_size = std::mem::size_of::<CompUniforms>() as u64;
        let warp_uni = mk_uni("milk warp uni", uni_size);
        let comp_uni = mk_uni("milk comp uni", uni_size);
        let bind_uni = |buf: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &uni_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() }],
            })
        };
        let warp_uni_bind = bind_uni(&warp_uni);
        let comp_uni_bind = bind_uni(&comp_uni);

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("milk"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            // 原版是 LINEAR_MIPMAP_LINEAR
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            // MilkDrop 的取樣會跑出邊界，夾邊比重複好看（重複會有接縫）
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let sampler_wrap = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("milk wrap"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::include_wgsl!("milk.wgsl"));
        let wgsl = device.create_shader_module(wgpu::include_wgsl!("milk.wgsl"));
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("milk globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let pipes = if raw.has_shaders() && use_preset_shaders {
            build_pipes(gpu, &raw, &wgsl)
        } else {
            None
        };
        // blur1 只是**半解析度的溫和模糊**，不是重度降取樣。
        // 很多 preset 做 `main + (main − blur1) * k` 的銳化回授；
        // blur1 離 main 太遠的話那個差值等於「減掉全域平均」，
        // 高頻增益會爆掉，畫面幾十幀就推到黑白兩端。
        let blur: Vec<Offscreen2> = (1..=3)
            .map(|k| Offscreen2::new(device, (w >> k).max(1), (h >> k).max(1)))
            .collect();
        let pl_tex_uni = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&uni_layout), Some(&tex_layout)],
            immediate_size: 0,
        });
        let pl_none = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[],
            immediate_size: 0,
        });

        let warp_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("milk warp"),
            layout: Some(&pl_tex_uni),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_warp"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<MeshVert>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_warp"),
                targets: &[Some(FEEDBACK_FORMAT.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        let wave_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("milk wave"),
            layout: Some(&pl_none),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_wave"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<WaveVert>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 8,
                            shader_location: 1,
                        },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_wave"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FEEDBACK_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        let blur_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("milk blur"),
            layout: Some(&pl_tex_uni),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_full"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_blur"),
                targets: &[Some(FEEDBACK_FORMAT.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        // 同一組著色器，只有混合方式不同
        let wave_pipe_add = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("milk wave(add)"),
            layout: Some(&pl_none),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_wave"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<WaveVert>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 8,
                            shader_location: 1,
                        },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_wave"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FEEDBACK_FORMAT,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent::REPLACE,
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        // 形狀用三角形列，頂點格式跟波形一樣（位置＋顏色）
        let mk_shape_pipe = |label: &str, additive: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl_none),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_wave"),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<WaveVert>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 0,
                                shader_location: 0,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 8,
                                shader_location: 1,
                            },
                        ],
                    })],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_wave"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FEEDBACK_FORMAT,
                        blend: Some(if additive {
                            wgpu::BlendState {
                                color: wgpu::BlendComponent {
                                    src_factor: wgpu::BlendFactor::SrcAlpha,
                                    dst_factor: wgpu::BlendFactor::One,
                                    operation: wgpu::BlendOperation::Add,
                                },
                                alpha: wgpu::BlendComponent::REPLACE,
                            }
                        } else {
                            wgpu::BlendState::ALPHA_BLENDING
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        // 動態向量是一段段獨立的線，跟波形的 LineStrip 不一樣
        let mv_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("milk mv"),
            layout: Some(&pl_none),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_wave"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<WaveVert>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 8,
                            shader_location: 1,
                        },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_wave"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FEEDBACK_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        // mip 產生器：跟 blur 共用版面，只是片段程式不同
        let mip_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("milk mip"),
            layout: Some(&pl_tex_uni),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_full"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_mip"),
                targets: &[Some(FEEDBACK_FORMAT.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        let shape_pipe = mk_shape_pipe("milk shape", false);
        let shape_pipe_add = mk_shape_pipe("milk shape(add)", true);

        // 貼圖版。版面跟 warp 一樣是 (uniform, texture)，
        // uniform 這一格用不到但版面要有，畫的時候隨便綁一個上去。
        let mk_shape_tex_pipe = |label: &str, additive: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl_tex_uni),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_shape_tex"),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<ShapeTexVert>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 0,
                                shader_location: 0,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x4,
                                offset: 8,
                                shader_location: 1,
                            },
                            wgpu::VertexAttribute {
                                format: wgpu::VertexFormat::Float32x2,
                                offset: 24,
                                shader_location: 2,
                            },
                        ],
                    })],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_shape_tex"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FEEDBACK_FORMAT,
                        blend: Some(if additive {
                            wgpu::BlendState {
                                color: wgpu::BlendComponent {
                                    src_factor: wgpu::BlendFactor::SrcAlpha,
                                    dst_factor: wgpu::BlendFactor::One,
                                    operation: wgpu::BlendOperation::Add,
                                },
                                alpha: wgpu::BlendComponent::REPLACE,
                            }
                        } else {
                            wgpu::BlendState::ALPHA_BLENDING
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let shape_tex_pipe = mk_shape_tex_pipe("milk shape(tex)", false);
        let shape_tex_pipe_add = mk_shape_tex_pipe("milk shape(tex,add)", true);

        let comp_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("milk comp"),
            layout: Some(&pl_tex_uni),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_full"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_comp"),
                targets: &[Some(gfx::OFFSCREEN_FORMAT.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        Ok(Self {
            name,
            raw,
            base,
            prog,
            vm,
            slots,
            mesh_w,
            mesh_h,
            time: 0.0,
            frame: 0,
            fps: 60.0,
            prev: Offscreen2::mipped(device, w, h),
            curr: Offscreen2::mipped(device, w, h),
            warp_pipe,
            wave_pipe,
            wave_pipe_add,
            comp_pipe,
            pipes,
            assets,
            globals_buf,
            blur,
            blur_pipe,
            wgsl,
            tex_layout,
            uni_layout,
            warp_uni,
            warp_uni_bind,
            comp_uni,
            comp_uni_bind,
            sampler,
            sampler_wrap,
            verts,
            indices,
            index_count: idx.len() as u32,
            wave_verts,
            wave_count: 0,
            wave_strips: Vec::new(),
            wave_dot_tris: Vec::new(),
            shape_verts,
            shape_data: Vec::new(),
            shape_tris: Vec::new(),
            shape_lines: Vec::new(),
            shape_pipe,
            shape_pipe_add,
            shape_tex_verts,
            shape_tex_data: Vec::new(),
            shape_tex_tris: Vec::new(),
            shape_tex_pipe,
            shape_tex_pipe_add,
            mip_pipe,
            extra_verts,
            extra_data: Vec::new(),
            extra_tris: Vec::new(),
            extra_lines: Vec::new(),
            mv_pipe,
            cwave_verts,
            cwave_data: Vec::new(),
            cwave_strips: Vec::new(),
            cwave_dots: Vec::new(),
            cwave_scope,
            shape_scope,
            snd_bass: 0.0,
            snd_mid: 0.0,
            snd_treb: 0.0,
            aspect: (1.0, 1.0),
            rand_start,
            vert_data,
            wave_data: Vec::with_capacity(audio::WAVE),
        })
    }

    pub fn resize(&mut self, gpu: &Gpu, w: u32, h: u32) {
        if self.prev.w == w && self.prev.h == h {
            return;
        }
        self.prev = Offscreen2::mipped(&gpu.device, w, h);
        self.curr = Offscreen2::mipped(&gpu.device, w, h);
        self.blur = (1..=3)
            .map(|k| Offscreen2::new(&gpu.device, (w >> k).max(1), (h >> k).max(1)))
            .collect();
    }

    fn set_builtins(&mut self, snd: &Sound, w: u32, h: u32) {
        let s = &self.slots;
        let aspect = w as f64 / h.max(1) as f64;
        // MilkDrop 的慣例：較長的那邊是 1，另一邊小於 1
        let (ax, ay) = if aspect > 1.0 { (1.0, 1.0 / aspect) } else { (aspect, 1.0) };
        self.snd_bass = snd.bass;
        self.snd_mid = snd.mid;
        self.snd_treb = snd.treb;
        self.aspect = (ax, ay);
        let vm = &mut self.vm;
        vm.set(s.time, self.time);
        vm.set(s.fps, self.fps);
        vm.set(s.frame, self.frame as f64);
        vm.set(s.bass, snd.bass as f64);
        vm.set(s.mid, snd.mid as f64);
        vm.set(s.treb, snd.treb as f64);
        vm.set(s.bass_att, snd.bass_att as f64);
        vm.set(s.mid_att, snd.mid_att as f64);
        vm.set(s.treb_att, snd.treb_att as f64);
        vm.set(s.meshx, self.mesh_w as f64);
        vm.set(s.meshy, self.mesh_h as f64);
        // **方程式看到的 aspectx 跟引擎內部用的是倒數關係。**
        // butterchurn 內部拿 (1, h/w) 去算網格與形狀，但餵進方程式的是
        // invAspectx/invAspecty。61 個 preset 的方程式讀了 aspect，
        // 給錯的話它們的座標全部歪掉。
        vm.set(s.aspectx, 1.0 / ax);
        vm.set(s.aspecty, 1.0 / ay);
        vm.set(s.pixelsx, w as f64);
        vm.set(s.pixelsy, h as f64);
    }

    /// 跑 frame_eqs，然後逐頂點跑 pixel_eqs 算出網格的取樣座標。
    ///
    /// 公式照抄 butterchurn 的 `runPixelEquations`，連常數都一樣 ——
    /// 這一段只要有一個數字不同，畫面就會跟原版不像。
    fn build_mesh(&mut self, w: u32, h: u32) {
        // baseVals 每幀要重設，因為 pixel_eqs 是「在預設值上調整」的寫法
        let base: Vec<(usize, f64)> = self
            .base
            .iter()
            .filter_map(|(k, v)| self.prog.vars.find(k).map(|s| (s, *v)))
            .collect();
        for (s, v) in &base {
            self.vm.set(*s, *v);
        }

        let frame_prog = std::mem::take(&mut self.prog.frame);
        self.vm.run(&frame_prog);
        self.prog.frame = frame_prog;

        // frame_eqs 設好的 q1..q32 要傳給 pixel_eqs（它們共用同一個 vm，
        // 所以其實不用搬，但 baseVals 重設時不能把 q 洗掉）
        let qv: Vec<f64> = self.slots.q.iter().map(|&s| self.vm.get(s)).collect();

        let sl = &self.slots;
        let aspect = w as f64 / h.max(1) as f64;
        let (aspectx, aspecty) = if aspect > 1.0 { (1.0, 1.0 / aspect) } else { (aspect, 1.0) };

        let warp_anim = bv(&self.base, "warpanimspeed", 1.0);
        let warp_scale = bv(&self.base, "warpscale", 1.0).max(1e-6);
        let n = self.time * warp_anim;
        let l = 1.0 / warp_scale;
        let f0 = 11.68 + 4.0 * (1.413 * n + 10.0).cos();
        let f1 = 8.77 + 3.0 * (1.113 * n + 7.0).cos();
        let f2 = 10.54 + 3.0 * (1.233 * n + 3.0).cos();
        let f3 = 11.49 + 4.0 * (0.933 * n + 5.0).cos();

        let (nx, ny) = (self.mesh_w + 1, self.mesh_h + 1);
        let pixel_prog = std::mem::take(&mut self.prog.pixel);
        let has_pixel = !pixel_prog.is_empty();

        for j in 0..ny {
            for i in 0..nx {
                // 網格座標：x,y 是 0..1；big_x/big_y 是 −1..1
                let x01 = i as f64 / self.mesh_w as f64;
                let y01 = j as f64 / self.mesh_h as f64;
                let px = x01 * 2.0 - 1.0;
                let py = y01 * 2.0 - 1.0;
                let rad = (px * px * aspectx * aspectx + py * py * aspecty * aspecty).sqrt();
                let ang = if rad < 1e-9 { 0.0 } else { py.atan2(px) };

                // 每個頂點都從 baseVals 重新開始，不然上一個頂點的修改會累積
                for (s, v) in &base {
                    self.vm.set(*s, *v);
                }
                for (k, slot) in sl.q.iter().enumerate() {
                    self.vm.set(*slot, qv[k]);
                }
                self.vm.set(sl.x, x01);
                self.vm.set(sl.y, 1.0 - y01);
                self.vm.set(sl.rad, rad);
                self.vm.set(sl.ang, ang);

                if has_pixel {
                    self.vm.run(&pixel_prog);
                }

                let zoom = self.vm.get(sl.zoom);
                let zoomexp = self.vm.get(sl.zoomexp);
                let cx = self.vm.get(sl.cx);
                let cy = self.vm.get(sl.cy);
                let sx = nz(self.vm.get(sl.sx));
                let sy = nz(self.vm.get(sl.sy));
                let dx = self.vm.get(sl.dx);
                let dy = self.vm.get(sl.dy);
                let rot = self.vm.get(sl.rot);
                let warp = self.vm.get(sl.warp);

                // ---- 以下逐行對應 butterchurn 的 runPixelEquations ----
                let zoom_pow = zoomexp.powf(2.0 * rad - 1.0);
                let v = 1.0 / nz(zoom.powf(zoom_pow));
                let mut u = 0.5 * px * aspectx * v + 0.5;
                // butterchurn 這裡是 `-R`，因為 **WebGL 的紋理 v 由下往上**。
                // wgpu 的 v 是由上往下，所以要把那個負號拿掉，
                // 否則每一幀的回授都會上下翻一次，畫面會擠到上下兩邊。
                let mut w2 = 0.5 * py * aspecty * v + 0.5;
                u = (u - cx) / sx + cx;
                w2 = (w2 - cy) / sy + cy;
                if warp != 0.0 {
                    u += 0.0035 * warp * (0.333 * n + l * (px * f0 - py * f3)).sin();
                    w2 += 0.0035 * warp * (0.375 * n - l * (px * f2 + py * f1)).cos();
                    u += 0.0035 * warp * (0.753 * n - l * (px * f1 - py * f2)).cos();
                    w2 += 0.0035 * warp * (0.825 * n + l * (px * f0 + py * f3)).sin();
                }
                let k = u - cx;
                let m = w2 - cy;
                let (co, si) = (rot.cos(), rot.sin());
                u = k * co - m * si + cx;
                w2 = k * si + m * co + cy;
                u = ((u - dx) - 0.5) / aspectx + 0.5;
                w2 = ((w2 - dy) - 0.5) / aspecty + 0.5;

                self.vert_data[j * nx + i] = MeshVert {
                    pos: [px as f32, -py as f32],
                    uv: [u as f32, w2 as f32],
                };
            }
        }
        self.prog.pixel = pixel_prog;
    }

    /// 把時域波形變成線段。八種 `wave_mode` 的幾何全部照抄 butterchurn 的
    /// `generateWaveform`，連 `+32` 這種偏移都一樣 —— 376 個 preset 指定了
    /// wave_mode，形狀不對的話每一個看起來都會像同一個。
    ///
    /// 只有單聲道，所以左右兩軌餵同一份資料；原版靠 `+32` 的偏移解相關，
    /// 拿同一份也照樣畫得出 Lissajous。
    fn build_wave(&mut self, wave: &[f32; audio::WAVE]) {
        let g = |k: &str, d: f64| -> f64 {
            self.prog.vars.find(k).map(|s| self.vm.get(s)).unwrap_or(d)
        };
        let mode = (g("wave_mode", 0.0).floor() as i64).rem_euclid(8);
        let scale = g("wave_scale", 1.0);
        let smoothing = g("wave_smoothing", 0.75).clamp(0.0, 0.99);
        let mut mystery = g("wave_mystery", -0.2);
        let wx = 2.0 * g("wave_x", 0.5) - 1.0;
        let wy = 2.0 * g("wave_y", 0.5) - 1.0;
        let mut alpha = g("wave_a", 1.0);
        let vol = (self.snd_bass + self.snd_mid + self.snd_treb) as f64 / 3.0;

        // 0／1／4 這三個模式的 mystery 超出 [-1,1] 時要摺回來
        if matches!(mode, 0 | 1 | 4) && !(-1.0..=1.0).contains(&mystery) {
            let mut v = 0.5 * mystery + 0.5;
            v -= v.floor();
            mystery = 2.0 * v.abs() - 1.0;
        }

        // processWaveform：一階低通。原版除以 128 是因為它的取樣是 0..255，
        // 這裡的取樣已經正規化到 [-1,1]，所以直接用 wave_scale。
        let a = scale * (1.0 - smoothing);
        let mut n = vec![0.0f64; audio::WAVE];
        n[0] = wave[0] as f64 * scale;
        for i in 1..audio::WAVE {
            n[i] = wave[i] as f64 * a + n[i - 1] * smoothing;
        }
        let l = n.clone();
        let len = n.len();
        let at = |v: &Vec<f64>, i: usize| -> f64 { v[i % len] };

        let (ax, ay) = self.aspect;
        let t = self.time;
        // **點數要跟畫面寬度綁定**（原版是 texsizeX/3），不能寫死。
        // 在 320 寬的小圖上硬塞 512 點的話線條重疊五倍，
        // 配上加法混合整片就白掉 —— 這個 bug 只在小解析度出現。
        let max_pts = (self.prev.w as usize / 3).max(16);

        if g("modwavealphabyvolume", 0.0) > 0.0 {
            let start = g("modwavealphastart", 0.75);
            let end = g("modwavealphaend", 0.95);
            let d = end - start;
            if d.abs() > 1e-6 {
                alpha *= (vol - start) / d;
            }
        }

        // 幾個模式的 alpha 有固定係數，那是原版為了讓密集的模式不要糊掉
        // 幾個模式有固定係數，讓密集的模式不要糊掉。
        // **係數跟解析度有關**：原版分 <1024、1024–2048、>=2048 三段，
        // 因為同樣的點數在小畫面上疊得更密。接觸印樣是 320 寬，
        // 用大畫面的係數會過曝。
        let dim = if self.prev.w < 1024 {
            0
        } else if self.prev.w < 2048 {
            1
        } else {
            2
        };
        match mode {
            1 => alpha *= 1.25,
            2 | 5 => alpha *= [0.09, 0.11, 0.13][dim],
            3 => alpha *= [0.15, 0.22, 0.33][dim] * 1.3 * (self.snd_treb * self.snd_treb) as f64,
            _ => {}
        }
        alpha = alpha.clamp(0.0, 1.0);
        // butterchurn 的 generateWaveform 第一行就是這個判斷：
        // **`wave_a` 小於 0.001 代表這個 preset 根本不要波形**。
        // 我早期給 alpha 塞了 0.02 的地板「免得一開始全黑」，
        // 結果那些刻意關掉波形的 preset 全部被一條線蓋掉。
        if alpha <= 0.001 {
            self.wave_data.clear();
            self.wave_strips.clear();
            self.wave_dot_tris.clear();
            self.wave_count = 0;
            return;
        }

        let mut r = g("wave_r", 0.5).clamp(0.0, 1.0) as f32;
        let mut gg = g("wave_g", 0.5).clamp(0.0, 1.0) as f32;
        let mut b = g("wave_b", 0.5).clamp(0.0, 1.0) as f32;
        if g("wave_brighten", 0.0) != 0.0 {
            let m = r.max(gg).max(b);
            if m > 0.01 {
                r /= m;
                gg /= m;
                b /= m;
            }
        }
        let col = [r, gg, b, alpha as f32];

        self.wave_data.clear();
        self.wave_strips.clear();
        self.wave_dot_tris.clear();
        if std::env::var_os("VITRO_NO_WAVE").is_some() {
            self.wave_count = 0;
            return;
        }
        let mut pts: Vec<[f64; 2]> = Vec::with_capacity(len);
        let mut pts2: Vec<[f64; 2]> = Vec::new();

        match mode {
            // 圓形示波器
            0 => {
                let p = len / 2 + 1;
                let e = 1.0 / (p - 1) as f64;
                let off = (len - p) / 2;
                for i in 0..p - 1 {
                    let mut rad = 0.5 + 0.4 * at(&l, i + off) + mystery;
                    let ang = i as f64 * e * 2.0 * std::f64::consts::PI + 0.2 * t;
                    // 開頭十分之一要跟結尾對接，不然圓會有一道縫
                    if i < p / 10 {
                        let y = i as f64 / (0.1 * p as f64);
                        let y = 0.5 - 0.5 * (y * std::f64::consts::PI).cos();
                        rad = (1.0 - y) * (0.5 + 0.4 * at(&l, i + p + off) + mystery) + rad * y;
                    }
                    pts.push([rad * ang.cos() * ay + wx, rad * ang.sin() * ax + wy]);
                }
                if let Some(first) = pts.first().copied() {
                    pts.push(first);
                }
            }
            // 螺旋
            1 => {
                let p = len / 2;
                for i in 0..p {
                    let m = 0.53 + 0.43 * at(&l, i) + mystery;
                    let f = 0.5 * at(&n, i + 32) * std::f64::consts::PI + 2.3 * t;
                    pts.push([m * f.cos() * ay + wx, m * f.sin() * ax + wy]);
                }
            }
            // 置中的 Lissajous（2 與 3 幾何相同，只有亮度不同）
            2 | 3 => {
                for i in 0..len {
                    pts.push([at(&l, i) * ay + wx, at(&n, i + 32) * ax + wy]);
                }
            }
            // 導數線
            4 => {
                let p = len.min(max_pts);
                let v = 1.0 / p as f64;
                let off = (len - p) / 2;
                let x0 = 0.45 + 0.5 * (0.5 * mystery + 0.5);
                let k = 1.0 - x0;
                for i in 0..p {
                    let mut px = 2.0 * i as f64 * v + (wx - 1.0) + 0.44 * at(&l, i + 25 + off);
                    let mut py = 0.47 * at(&n, i + off) + wy;
                    if i > 1 {
                        let a1 = pts[i - 1];
                        let a2 = pts[i - 2];
                        px = px * k + x0 * (2.0 * a1[0] - a2[0]);
                        py = py * k + x0 * (2.0 * a1[1] - a2[1]);
                    }
                    pts.push([px, py]);
                }
            }
            // 爆炸狀
            5 => {
                let (cy, sy) = ((0.3 * t).cos(), (0.3 * t).sin());
                for i in 0..len {
                    let j = i + 32;
                    let kk = at(&l, i) * at(&n, j) + at(&n, i) * at(&l, j);
                    let jj = at(&l, i) * at(&l, i) - at(&n, j) * at(&n, j);
                    pts.push([(kk * cy - jj * sy) * (ay + wx), (kk * sy + jj * cy) * (ax + wy)]);
                }
            }
            // 直線（6 單條、7 雙條）
            _ => {
                let p = (len / 2).min(max_pts);
                let off = (len - p) / 2;
                let ang = 0.5 * std::f64::consts::PI * mystery;
                let (ca, sa) = (ang.cos(), ang.sin());
                let h = ang + 0.5 * std::f64::consts::PI;
                let mut ex = [wx * h.cos() - 3.0 * ca, wx * h.cos() + 3.0 * ca];
                let mut ey = [wx * h.sin() - 3.0 * sa, wx * h.sin() + 3.0 * sa];
                // 把線段夾進 [-1.1, 1.1] 的框，否則超長的線會把網格拉爆
                for e in 0..2 {
                    for case in 0..4 {
                        let o = match case {
                            0 if ex[e] > 1.1 => Some((1.1 - ex[1 - e]) / (ex[e] - ex[1 - e])),
                            1 if ex[e] < -1.1 => Some((-1.1 - ex[1 - e]) / (ex[e] - ex[1 - e])),
                            2 if ey[e] > 1.1 => Some((1.1 - ey[1 - e]) / (ey[e] - ey[1 - e])),
                            3 if ey[e] < -1.1 => Some((-1.1 - ey[1 - e]) / (ey[e] - ey[1 - e])),
                            _ => None,
                        };
                        if let Some(o) = o {
                            let dx = ex[e] - ex[1 - e];
                            let dy = ey[e] - ey[1 - e];
                            ex[e] = ex[1 - e] + dx * o;
                            ey[e] = ey[1 - e] + dy * o;
                        }
                    }
                }
                let sx = (ex[1] - ex[0]) / p as f64;
                let sy2 = (ey[1] - ey[0]) / p as f64;
                let pa = sy2.atan2(sx) + 0.5 * std::f64::consts::PI;
                let (gx, gy) = (pa.cos(), pa.sin());
                if mode == 6 {
                    for i in 0..p {
                        let f = at(&n, i + off);
                        pts.push([
                            ex[0] + sx * i as f64 + 0.25 * gx * f,
                            ey[0] + sy2 * i as f64 + 0.25 * gy * f,
                        ]);
                    }
                } else {
                    let sep = (0.5 * wy + 0.5).powi(2);
                    for i in 0..p {
                        let f = at(&n, i + off);
                        pts.push([
                            ex[0] + sx * i as f64 + gx * (0.25 * f + sep),
                            ey[0] + sy2 * i as f64 + gy * (0.25 * f + sep),
                        ]);
                    }
                    for i in 0..p {
                        let f = at(&l, i + off);
                        pts2.push([
                            ex[0] + sx * i as f64 + gx * (0.25 * f - sep),
                            ey[0] + sy2 * i as f64 + gy * (0.25 * f - sep),
                        ]);
                    }
                }
            }
        }

        // `wave_dots` 是畫點不是畫線，`wave_thick` 是同一條線畫四次各偏一像素。
        // 原版兩者都吃「平滑後」的點列 —— 連點模式也是，所以點數是兩倍。
        let dots = bv(&self.base, "wave_dots", 0.0).abs() > 1e-9;
        let thick = bv(&self.base, "wave_thick", 0.0).abs() > 1e-9;
        let (pw, ph) = (self.prev.w.max(1) as f64, self.prev.h.max(1) as f64);
        let to_verts = |src: &[[f64; 2]]| -> Vec<WaveVert> {
            src.iter()
                .map(|q| WaveVert {
                    pos: [q[0].clamp(-4.0, 4.0) as f32, q[1].clamp(-4.0, 4.0) as f32],
                    color: col,
                })
                .collect()
        };
        for src in [&pts, &pts2] {
            if src.len() < 2 {
                continue;
            }
            let line = smooth_wave(&to_verts(src));
            let reps = if dots || thick { 4 } else { 1 };
            for k in 0..reps {
                let (ox, oy) = thick_offset(k, pw, ph);
                if dots {
                    let size = 1.0 + if pw >= 1024.0 { 1.0 } else { 0.0 };
                    let start = self.wave_data.len() as u32;
                    for q in &line {
                        if self.wave_data.len() + 6 > WAVE_MAX {
                            break;
                        }
                        push_dot(&mut self.wave_data, q, size / pw, size / ph, ox, oy);
                    }
                    let count = self.wave_data.len() as u32 - start;
                    if count > 0 {
                        self.wave_dot_tris.push((start, count));
                    }
                } else {
                    if self.wave_data.len() + line.len() > WAVE_MAX {
                        break;
                    }
                    let start = self.wave_data.len() as u32;
                    for q in &line {
                        self.wave_data.push(WaveVert {
                            pos: [q.pos[0] + ox as f32, q.pos[1] + oy as f32],
                            color: q.color,
                        });
                    }
                    self.wave_strips.push((start, line.len() as u32));
                }
            }
        }
        self.wave_count = self.wave_data.len() as u32;
    }

    /// `hue_shader` 的四個角，照抄 butterchurn 的 `generateHueBase`。
    ///
    /// `rand_start` 原版是 `Math.random()`，我改成用 preset 名字雜湊：
    /// 同一個 preset 每次跑出來一樣，`--shot` 才有前後比較的價值。
    fn hue_base(&self) -> [[f32; 4]; 4] {
        let t = self.time;
        let mut out = [[1.0f32; 4]; 4];
        for (i, slot) in out.iter_mut().enumerate() {
            let fi = i as f64;
            let mut c = [
                0.6 + 0.3 * (30.0 * t * 0.0143 + 3.0 + 21.0 * fi + self.rand_start[3]).sin(),
                0.6 + 0.3 * (30.0 * t * 0.0107 + 1.0 + 13.0 * fi + self.rand_start[1]).sin(),
                0.6 + 0.3 * (30.0 * t * 0.0129 + 6.0 + 9.0 * fi + self.rand_start[2]).sin(),
            ];
            // 先正規化到最亮的那一色是 1，再壓到 [0.5, 1]
            let m = c[0].max(c[1]).max(c[2]).max(1e-6);
            for v in c.iter_mut() {
                *v = 0.5 + 0.5 * (*v / m);
            }
            *slot = [c[0] as f32, c[1] as f32, c[2] as f32, 1.0];
        }
        out
    }

    /// 診斷用：把這一幀的關鍵數字倒出來。畫面全黑時第一個要看的東西。
    pub fn debug_dump(&self) -> String {
        let b = &self.base;
        let uv: Vec<String> = [0usize, self.vert_data.len() / 2, self.vert_data.len() - 1]
            .iter()
            .map(|i| {
                let v = &self.vert_data[*i];
                format!("({:.3},{:.3})", v.uv[0], v.uv[1])
            })
            .collect();
        let w0 = self.wave_data.first();
        format!(
            "decay={:.3} gamma={:.3} mesh={}x{} 波形點={} 顏色={:?}
  uv 取樣點: {}
  zoom={:.3} rot={:.3} warp={:.3} cx={:.2} cy={:.2} sx={:.2} sy={:.2} dx={:.3} dy={:.3}",
            bv(b, "decay", 0.98),
            bv(b, "gammaadj", 2.0),
            self.mesh_w,
            self.mesh_h,
            self.wave_count,
            w0.map(|v| v.color),
            uv.join(" "),
            self.vm.get(self.slots.zoom),
            self.vm.get(self.slots.rot),
            self.vm.get(self.slots.warp),
            self.vm.get(self.slots.cx),
            self.vm.get(self.slots.cy),
            self.vm.get(self.slots.sx),
            self.vm.get(self.slots.sy),
            self.vm.get(self.slots.dx),
            self.vm.get(self.slots.dy),
        )
    }

    fn bind_tex(&self, gpu: &Gpu, view: &wgpu::TextureView) -> wgpu::BindGroup {
        let samp = if bv(&self.base, "wrap", 0.0) > 0.5 {
            &self.sampler_wrap
        } else {
            &self.sampler
        };
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
                    resource: wgpu::BindingResource::Sampler(samp),
                },
            ],
        })
    }


    /// Custom shapes。2104 個形狀裡有 542 個是啟用的，而且 467 個帶 frame_eqs
    /// —— 很多 preset 的主視覺其實是形狀不是波形。
    ///
    /// 幾何照抄 butterchurn：中心一個頂點、邊上 sides+1 個，扇形展開。
    /// 注意兩件事：**y 是翻轉的**（`f = −2y + 1`），而且起始角要加 π/4。
    fn build_shapes(&mut self) {
        self.shape_data.clear();
        self.shape_tris.clear();
        self.shape_lines.clear();
        self.shape_tex_data.clear();
        self.shape_tex_tris.clear();
        // 診斷開關：隔離「是形狀弄壞的還是別的」
        if std::env::var_os("VITRO_NO_SHAPES").is_some() {
            return;
        }
        if self.raw.shapes.is_empty() {
            return;
        }

        // **這裡要用 aspecty 不是 aspectx。** butterchurn 的形狀是
        // `x + rad*cos*aspecty`：橫向畫面 aspectx 恆為 1，拿它乘等於沒修正，
        // 正方形會被拉成 16:9 的長方形。
        let (_ax, ay) = self.aspect;
        let n_shapes = self.raw.shapes.len().min(self.prog.shapes.len());
        for si in 0..n_shapes {
            let merged = merged_shape(Some(&self.raw.shapes[si].base_vals));
            if std::env::var_os("VITRO_DUMP_SHAPES").is_some() && self.frame == 60 {
                println!("  形狀[{si}] enabled={} num_inst={}",
                    bv(&merged, "enabled", 0.0), bv(&merged, "num_inst", 1.0));
            }
            if bv(&merged, "enabled", 0.0) < 0.5 {
                continue;
            }
            let base: Vec<(usize, f64)> = merged
                .iter()
                .filter_map(|(k, v)| self.prog.vars.find(k).map(|s| (s, *v)))
                .collect();
            // 換進這個形狀自己的作用域（見 `cwave_scope` 的說明）
            let mut scope = std::mem::take(&mut self.shape_scope[si]);
            let outer: Vec<f64> = scope.iter().map(|(sl, _)| self.vm.get(*sl)).collect();
            for (sl, v) in scope.iter() {
                self.vm.set(*sl, *v);
            }
            let prog = std::mem::take(&mut self.prog.shapes[si].frame);
            let n_inst = bv(&merged, "num_inst", 1.0).clamp(1.0, 1024.0) as usize;
            let inst_slot = self.prog.vars.find("instance");

            for inst in 0..n_inst {
                // 每個實例都從 baseVals 重新開始，不然上一個的修改會累積
                for (s, v) in &base {
                    self.vm.set(*s, *v);
                }
                if let Some(s) = inst_slot {
                    self.vm.set(s, inst as f64);
                }
                if !prog.is_empty() {
                    self.vm.run(&prog);
                }

                let g = |vm: &Vm, vars: &eel::Vars, k: &str, d: f64| -> f64 {
                    vars.find(k).map(|s| vm.get(s)).unwrap_or(d)
                };
                let v = &self.prog.vars;
                let sides = g(&self.vm, v, "sides", 4.0).clamp(3.0, 100.0) as usize;
                let rad = g(&self.vm, v, "rad", 0.1);
                let ang = g(&self.vm, v, "ang", 0.0);
                let cx = 2.0 * g(&self.vm, v, "x", 0.5) - 1.0;
                let cy = -2.0 * g(&self.vm, v, "y", 0.5) + 1.0;
                let c_in = [
                    g(&self.vm, v, "r", 1.0) as f32,
                    g(&self.vm, v, "g", 0.0) as f32,
                    g(&self.vm, v, "b", 0.0) as f32,
                    g(&self.vm, v, "a", 1.0).clamp(0.0, 1.0) as f32,
                ];
                let c_out = [
                    g(&self.vm, v, "r2", 0.0) as f32,
                    g(&self.vm, v, "g2", 1.0) as f32,
                    g(&self.vm, v, "b2", 0.0) as f32,
                    g(&self.vm, v, "a2", 0.0).clamp(0.0, 1.0) as f32,
                ];
                let c_border = [
                    g(&self.vm, v, "border_r", 1.0) as f32,
                    g(&self.vm, v, "border_g", 1.0) as f32,
                    g(&self.vm, v, "border_b", 1.0) as f32,
                    g(&self.vm, v, "border_a", 0.1).clamp(0.0, 1.0) as f32,
                ];
                let additive = g(&self.vm, v, "additive", 0.0).abs() >= 1.0;
                let textured = g(&self.vm, v, "textured", 0.0).abs() >= 1.0;
                let tex_ang = g(&self.vm, v, "tex_ang", 0.0);
                let tex_zoom = {
                    let z = g(&self.vm, v, "tex_zoom", 1.0);
                    if z.abs() < 1e-4 { 1.0 } else { z }
                };

                if std::env::var_os("VITRO_DUMP_SHAPES").is_some() && self.frame == 60 {
                    println!(
                        "  形狀[{si}] 實例{inst}: sides={sides} rad={rad:.3} ang={ang:.2} 中心=({cx:.2},{cy:.2}) 內={c_in:?} 外={c_out:?} 框={c_border:?} 加法={additive}"
                    );
                }
                if c_in[3] <= 0.001 && c_out[3] <= 0.001 && c_border[3] <= 0.001 {
                    continue;
                }

                // 邊上的點
                let quarter = 0.25 * std::f64::consts::PI;
                let rim: Vec<[f32; 2]> = (0..=sides)
                    .map(|k| {
                        let i = 2.0 * (k as f64 / sides as f64) * std::f64::consts::PI;
                        let x = i + ang + quarter;
                        [(cx + rad * x.cos() * ay) as f32, (cy + rad * x.sin()) as f32]
                    })
                    .collect();

                // 貼圖版：uv 跟著形狀自己的角度與縮放走，取的是上一幀的畫面
                if textured && (c_in[3] > 0.001 || c_out[3] > 0.001) {
                    let quarter = 0.25 * std::f64::consts::PI;
                    let uv_of = |k: usize| -> [f32; 2] {
                        let i = 2.0 * (k as f64 / sides as f64) * std::f64::consts::PI;
                        let a = i + tex_ang + quarter;
                        [
                            (0.5 + 0.5 * a.cos() / tex_zoom * ay) as f32,
                            (0.5 + 0.5 * a.sin() / tex_zoom) as f32,
                        ]
                    };
                    let start = self.shape_tex_data.len() as u32;
                    let centre =
                        ShapeTexVert { pos: [cx as f32, cy as f32], color: c_in, uv: [0.5, 0.5] };
                    for k in 0..sides {
                        if self.shape_tex_data.len() + 3 > SHAPE_MAX {
                            break;
                        }
                        self.shape_tex_data.push(centre);
                        self.shape_tex_data.push(ShapeTexVert {
                            pos: rim[k],
                            color: c_out,
                            uv: uv_of(k),
                        });
                        self.shape_tex_data.push(ShapeTexVert {
                            pos: rim[k + 1],
                            color: c_out,
                            uv: uv_of(k + 1),
                        });
                    }
                    let count = self.shape_tex_data.len() as u32 - start;
                    if count > 0 {
                        self.shape_tex_tris.push((start, count, additive));
                    }
                }

                // 扇形展開成三角形列
                if !textured && (c_in[3] > 0.001 || c_out[3] > 0.001) {
                    let start = self.shape_data.len() as u32;
                    let centre = WaveVert { pos: [cx as f32, cy as f32], color: c_in };
                    for k in 0..sides {
                        self.shape_data.push(centre);
                        self.shape_data.push(WaveVert { pos: rim[k], color: c_out });
                        self.shape_data.push(WaveVert { pos: rim[k + 1], color: c_out });
                    }
                    let count = self.shape_data.len() as u32 - start;
                    self.shape_tris.push((start, count, additive));
                }

                // 外框：沿著邊走一圈
                if c_border[3] > 0.001 {
                    let start = self.shape_data.len() as u32;
                    for q in &rim {
                        self.shape_data.push(WaveVert { pos: *q, color: c_border });
                    }
                    let count = self.shape_data.len() as u32 - start;
                    self.shape_lines.push((start, count));
                }
            }
            self.prog.shapes[si].frame = prog;
            // 把這個形狀的值收回去，外層的換回來
            for (k, (sl, v)) in scope.iter_mut().enumerate() {
                *v = self.vm.get(*sl);
                self.vm.set(*sl, outer[k]);
            }
            self.shape_scope[si] = scope;
        }
    }


    /// Custom waves 的 `point_eqs`。395 個 preset 裡有 295 個波形是啟用的，
    /// 其中 291 個帶 point_eqs —— 對不少 preset 來說這才是主角，
    /// 內建的那條 `wave_mode` 波形只是陪襯。
    ///
    /// 整段照抄 butterchurn 的 `generateWaveform`，包括那兩趟一前一後的
    /// 一階低通：只做單向的話整條線會往一邊歪，因為每個點都只看得到前面。
    fn build_cwaves(&mut self, wave: &[f32; audio::WAVE], bins: &[f32; audio::BINS]) {
        self.cwave_data.clear();
        self.cwave_strips.clear();
        self.cwave_dots.clear();
        // 診斷開關：畫面爆掉時先關這個，判斷是不是 custom wave 幹的
        if std::env::var_os("VITRO_NO_CWAVE").is_some() {
            return;
        }
        let n_waves = self.raw.waves.len().min(self.prog.waves.len());
        if n_waves == 0 {
            return;
        }
        let (ax, ay) = self.aspect;
        let (inv_ax, inv_ay) = (1.0 / ax, 1.0 / ay);
        let (pw, ph) = (self.prev.w.max(1) as f64, self.prev.h.max(1) as f64);
        let wave_scale = self
            .prog
            .vars
            .find("wave_scale")
            .map(|sl| self.vm.get(sl))
            .unwrap_or_else(|| bv(&self.base, "wave_scale", 1.0));

        for wi in 0..n_waves {
            let merged = merged_wave(Some(&self.raw.waves[wi].base_vals));
            if bv(&merged, "enabled", 0.0) < 0.5 {
                continue;
            }

            // 換進這個波形自己的作用域
            let mut scope = std::mem::take(&mut self.cwave_scope[wi]);
            let outer: Vec<f64> = scope.iter().map(|(sl, _)| self.vm.get(*sl)).collect();
            for (sl, v) in scope.iter() {
                self.vm.set(*sl, *v);
            }
            // baseVals 每幀重鋪。它們是靜態的，不跟著作用域留存 ——
            // butterchurn 的合併順序也是 baseVals 先、上一幀的自訂變數後蓋上。
            for (k, v) in &merged {
                if let Some(sl) = self.prog.vars.find(k) {
                    self.vm.set(sl, *v);
                }
            }

            let frame_prog = std::mem::take(&mut self.prog.waves[wi].frame);
            self.vm.run(&frame_prog);
            self.prog.waves[wi].frame = frame_prog;

            // frame_eqs 跑完才讀 —— 直接讀 baseVals 的話等於當它沒跑
            let g = |me: &Self, k: &str, d: f64| -> f64 {
                me.prog
                    .vars
                    .find(k)
                    .map(|sl| me.vm.get(sl))
                    .unwrap_or_else(|| bv(&merged, k, d))
            };
            let truthy = |v: f64| v.abs() > 1e-9;

            'body: {
                let sep = g(self, "sep", 0.0).floor().clamp(0.0, 510.0) as usize;
                let mut samples = g(self, "samples", 512.0).floor().clamp(0.0, 512.0) as usize;
                samples = samples.saturating_sub(sep);
                let usedots = truthy(g(self, "usedots", 0.0));
                let thick = truthy(g(self, "thick", 0.0));
                let additive = truthy(g(self, "additive", 0.0));
                // 少於兩點畫不成線；只有點模式能接受一個點
                if samples < 2 && !(usedots && samples >= 1) {
                    break 'body;
                }

                let spectrum = truthy(g(self, "spectrum", 0.0));
                let scaling = g(self, "scaling", 1.0);
                let smoothing = g(self, "smoothing", 0.5).clamp(0.0, 1.0);
                let amp = (if spectrum { 0.15 } else { 0.004 }) * scaling * wave_scale;

                // 取樣來源。butterchurn 的時域資料是 ±128 的整數，
                // 我這邊已經正規化到 ±1，所以要補回那 128 倍，
                // 不然 0.004 的係數會讓整條線縮成一個點。
                // 頻譜沒有共同基準，用 4 倍讓「滿格」大約等於全幅擺動。
                let src: Vec<f64> = if spectrum {
                    (0..audio::WAVE)
                        .map(|i| {
                            let t = i as f64 * (audio::BINS - 1) as f64
                                / (audio::WAVE - 1) as f64;
                            let a = t.floor() as usize;
                            let b = (a + 1).min(audio::BINS - 1);
                            let f = t - a as f64;
                            (bins[a] as f64 * (1.0 - f) + bins[b] as f64 * f) * 4.0
                        })
                        .collect()
                } else {
                    wave.iter().map(|v| *v as f64 * 128.0).collect()
                };

                // 左右兩軌用 sep 錯開。這邊只有單聲道，錯開本身就足以解相關。
                let half = (audio::WAVE - samples) as f64 / 2.0;
                let (off_l, off_r) = if spectrum {
                    (0.0, 0.0)
                } else {
                    ((half - sep as f64 / 2.0).floor(), (half + sep as f64 / 2.0).floor())
                };
                let step = if spectrum {
                    (audio::WAVE - sep) as f64 / samples as f64
                } else {
                    1.0
                };
                let at = |k: f64| -> f64 { src[(k.max(0.0) as usize).min(audio::WAVE - 1)] };

                let sm = (0.98 * smoothing).max(0.0).sqrt();
                let inv = 1.0 - sm;
                let mut d0 = vec![0.0f64; samples];
                let mut d1 = vec![0.0f64; samples];
                d0[0] = at(off_l);
                d1[0] = at(off_r);
                for i in 1..samples {
                    d0[i] = at(i as f64 * step + off_l) * inv + d0[i - 1] * sm;
                    d1[i] = at(i as f64 * step + off_r) * inv + d1[i - 1] * sm;
                }
                // 回頭再平滑一次，這樣兩端才對稱
                for z in (0..samples - 1).rev() {
                    d0[z] = d0[z] * inv + d0[z + 1] * sm;
                    d1[z] = d1[z] * inv + d1[z + 1] * sm;
                }
                for i in 0..samples {
                    d0[i] *= amp;
                    d1[i] *= amp;
                }

                let (cr, cg, cb, ca) = (
                    g(self, "r", 1.0),
                    g(self, "g", 1.0),
                    g(self, "b", 1.0),
                    g(self, "a", 1.0),
                );

                let point_prog = std::mem::take(&mut self.prog.waves[wi].point);
                let v = &self.prog.vars;
                let (s_sample, s_v1, s_v2) = (v.find("sample"), v.find("value1"), v.find("value2"));
                let (s_x, s_y) = (v.find("x"), v.find("y"));
                let (s_r, s_g, s_b, s_a) =
                    (v.find("r"), v.find("g"), v.find("b"), v.find("a"));
                let mut pts: Vec<WaveVert> = Vec::with_capacity(samples);
                let denom = (samples as f64 - 1.0).max(1.0);
                for i in 0..samples {
                    let (v1, v2) = (d0[i], d1[i]);
                    set_opt(&mut self.vm, s_sample, i as f64 / denom);
                    set_opt(&mut self.vm, s_v1, v1);
                    set_opt(&mut self.vm, s_v2, v2);
                    set_opt(&mut self.vm, s_x, 0.5 + v1);
                    set_opt(&mut self.vm, s_y, 0.5 + v2);
                    set_opt(&mut self.vm, s_r, cr);
                    set_opt(&mut self.vm, s_g, cg);
                    set_opt(&mut self.vm, s_b, cb);
                    set_opt(&mut self.vm, s_a, ca);
                    if !point_prog.is_empty() {
                        self.vm.run(&point_prog);
                    }
                    let rd = |sl: Option<usize>, d: f64| sl.map(|sl| self.vm.get(sl)).unwrap_or(d);
                    let x = rd(s_x, 0.5 + v1);
                    let y = rd(s_y, 0.5 + v2);
                    pts.push(WaveVert {
                        pos: [
                            ((2.0 * x - 1.0) * inv_ax).clamp(-4.0, 4.0) as f32,
                            ((-2.0 * y + 1.0) * inv_ay).clamp(-4.0, 4.0) as f32,
                        ],
                        color: [
                            rd(s_r, cr) as f32,
                            rd(s_g, cg) as f32,
                            rd(s_b, cb) as f32,
                            rd(s_a, ca).clamp(0.0, 1.0) as f32,
                        ],
                    });
                }
                self.prog.waves[wi].point = point_prog;

                if usedots {
                    // 原版是 GL_POINTS 加 gl_PointSize，WGSL 沒有這東西，
                    // 所以每個點展開成一個小方塊。大小照原版：一般 1px、
                    // thick 2px，寬度到 1024 以上再各加一。
                    let size = (if thick { 2.0 } else { 1.0 })
                        + if pw >= 1024.0 { 1.0 } else { 0.0 };
                    let (hx, hy) = (size / pw, size / ph);
                    let start = self.cwave_data.len() as u32;
                    for q in &pts {
                        if self.cwave_data.len() + 6 > CWAVE_MAX {
                            break;
                        }
                        push_dot(&mut self.cwave_data, q, hx, hy, 0.0, 0.0);
                    }
                    let count = self.cwave_data.len() as u32 - start;
                    if count > 0 {
                        self.cwave_dots.push((start, count, additive));
                    }
                } else {
                    let line = smooth_wave(&pts);
                    // thick 的線在原版是畫四次、各偏一個像素
                    let reps = if thick { 4 } else { 1 };
                    for k in 0..reps {
                        let (ox, oy) = thick_offset(k, pw, ph);
                        if self.cwave_data.len() + line.len() > CWAVE_MAX {
                            break;
                        }
                        let start = self.cwave_data.len() as u32;
                        for q in &line {
                            self.cwave_data.push(WaveVert {
                                pos: [q.pos[0] + ox as f32, q.pos[1] + oy as f32],
                                color: q.color,
                            });
                        }
                        self.cwave_strips.push((start, line.len() as u32, additive));
                    }
                }
            }

            // 把這個波形的值收回去，外層的換回來
            for (k, (sl, val)) in scope.iter_mut().enumerate() {
                *val = self.vm.get(*sl);
                self.vm.set(*sl, outer[k]);
            }
            self.cwave_scope[wi] = scope;
        }
    }


    /// 邊框、暗中心、動態向量。三個都是 MilkDrop 的「外framing」元素：
    /// 102 個 preset 有外框、45 個有內框、33 個要暗中心、18 個開動態向量。
    ///
    /// 幾何全部照抄 butterchurn（`generateBorder`／`DarkenCenter`／
    /// `generateMotionVectors`），座標直接就是 NDC。
    fn build_extras(&mut self) {
        self.extra_data.clear();
        self.extra_tris.clear();
        self.extra_lines.clear();
        if std::env::var_os("VITRO_NO_EXTRAS").is_some() {
            return;
        }
        let cv = |me: &Self, k: &str, d: f64| -> f64 {
            let fallback = bv(&me.base, k, d);
            me.prog.vars.find(k).map(|sl| me.vm.get(sl)).unwrap_or(fallback)
        };
        let (_ax, ay) = self.aspect;

        // ---- 動態向量。畫在最底下，順序跟原版一樣 ----
        let mv_a = cv(self, "mv_a", 0.0);
        let (mv_x, mv_y) = (cv(self, "mv_x", 12.0), cv(self, "mv_y", 9.0));
        let (nx_f, ny_f) = (mv_x.floor(), mv_y.floor());
        if mv_a > 0.001 && nx_f > 0.0 && ny_f > 0.0 {
            let (mut nx, mut ny) = (nx_f as usize, ny_f as usize);
            let (mut fx, mut fy) = (mv_x - nx_f, mv_y - ny_f);
            // 上限就是網格本身，再密也沒有資料可取
            if nx > self.mesh_w {
                nx = self.mesh_w;
                fx = 0.0;
            }
            if ny > self.mesh_h {
                ny = self.mesh_h;
                fy = 0.0;
            }
            let (dx, dy) = (cv(self, "mv_dx", 0.0), cv(self, "mv_dy", 0.0));
            let len = cv(self, "mv_l", 0.0);
            let minlen = 1.0 / self.prev.w.max(1) as f64;
            let color = [
                cv(self, "mv_r", 0.5) as f32,
                cv(self, "mv_g", 0.5) as f32,
                cv(self, "mv_b", 0.5) as f32,
                mv_a.clamp(0.0, 1.0) as f32,
            ];
            let start = self.extra_data.len() as u32;
            for j in 0..ny {
                let mut y = (j as f64 + 0.25) / (ny as f64 + fy + 0.25 - 1.0);
                y -= dy;
                if !(y > 1e-4 && y < 0.9999) {
                    continue;
                }
                for i in 0..nx {
                    let mut x = (i as f64 + 0.25) / (nx as f64 + fx + 0.25 - 1.0);
                    x += dx;
                    if !(x > 1e-4 && x < 0.9999) {
                        continue;
                    }
                    let (u, v) = self.motion_dir(x, y);
                    let (mut px, mut py) = ((u - x) * len, (v - y) * len);
                    let d = (px * px + py * py).sqrt();
                    // 太短的向量看不見，原版硬拉到一個像素
                    if d < minlen && d > 1e-8 {
                        let k = minlen / d;
                        px *= k;
                        py *= k;
                    } else if d <= 1e-8 {
                        px = minlen;
                        py = 0.0;
                    }
                    self.extra_data.push(WaveVert {
                        pos: [(2.0 * x - 1.0) as f32, (1.0 - 2.0 * y) as f32],
                        color,
                    });
                    self.extra_data.push(WaveVert {
                        pos: [
                            (2.0 * (x + px) - 1.0) as f32,
                            (1.0 - 2.0 * (y + py)) as f32,
                        ],
                        color,
                    });
                }
            }
            let count = self.extra_data.len() as u32 - start;
            if count > 0 {
                self.extra_lines.push((start, count));
            }
        }

        // ---- 暗中心：一個 5% 大的黑菱形，中心 alpha 3/32、四周 0 ----
        if cv(self, "darken_center", 0.0).abs() > 1e-6 {
            let c = WaveVert { pos: [0.0, 0.0], color: [0.0, 0.0, 0.0, 3.0 / 32.0] };
            let rim = |x: f64, y: f64| WaveVert {
                pos: [x as f32, y as f32],
                color: [0.0, 0.0, 0.0, 0.0],
            };
            let pts = [
                rim(-0.05 * ay, 0.0),
                rim(0.0, -0.05),
                rim(0.05 * ay, 0.0),
                rim(0.0, 0.05),
            ];
            let start = self.extra_data.len() as u32;
            for k in 0..4 {
                self.extra_data.push(c);
                self.extra_data.push(pts[k]);
                self.extra_data.push(pts[(k + 1) % 4]);
            }
            let count = self.extra_data.len() as u32 - start;
            self.extra_tris.push((start, count));
        }

        // ---- 外框、內框。內框要往內縮一個外框的寬度 ----
        let ob_size = cv(self, "ob_size", 0.0);
        let ob = [
            cv(self, "ob_r", 0.5) as f32,
            cv(self, "ob_g", 0.5) as f32,
            cv(self, "ob_b", 0.5) as f32,
            cv(self, "ob_a", 0.0).clamp(0.0, 1.0) as f32,
        ];
        self.push_border(ob, ob_size, 0.0);
        let ib_size = cv(self, "ib_size", 0.0);
        let ib = [
            cv(self, "ib_r", 0.5) as f32,
            cv(self, "ib_g", 0.5) as f32,
            cv(self, "ib_b", 0.5) as f32,
            cv(self, "ib_a", 0.0).clamp(0.0, 1.0) as f32,
        ];
        self.push_border(ib, ib_size, ob_size);
    }

    /// 一圈邊框 = 四個長方形。`offset` 是要讓開的外框寬度。
    fn push_border(&mut self, color: [f32; 4], size: f64, offset: f64) {
        if size <= 0.0 || color[3] <= 0.0 {
            return;
        }
        let s = offset / 2.0;
        let r = size / 2.0 + s;
        let (a, h) = (2.0 * s, 2.0 * s);
        let (o, n) = (2.0 * r, 2.0 * r);
        let start = self.extra_data.len() as u32;
        let mut quad = |x0: f64, x1: f64, y0: f64, y1: f64| {
            let v = |x: f64, y: f64| WaveVert { pos: [x as f32, y as f32], color };
            for (x, y) in [
                (x0, y0),
                (x0, y1),
                (x1, y1),
                (x0, y0),
                (x1, y1),
                (x1, y0),
            ] {
                self.extra_data.push(v(x, y));
            }
        };
        // 左、右、下、上
        quad(-1.0 + a, -1.0 + o, -1.0 + n, 1.0 - n);
        quad(1.0 - a, 1.0 - o, -1.0 + n, 1.0 - n);
        quad(-1.0 + a, 1.0 - a, -1.0 + h, -1.0 + n);
        quad(-1.0 + a, 1.0 - a, 1.0 - n, 1.0 - h);
        let count = self.extra_data.len() as u32 - start;
        self.extra_tris.push((start, count));
    }

    /// 在變形網格的 uv 場上做雙線性取樣，得到「這一點被拉去哪裡」。
    fn motion_dir(&self, x: f64, y: f64) -> (f64, f64) {
        let nx = self.mesh_w + 1;
        let fx = (x * self.mesh_w as f64).clamp(0.0, self.mesh_w as f64 - 1e-6);
        let fy = (y * self.mesh_h as f64).clamp(0.0, self.mesh_h as f64 - 1e-6);
        let (i0, j0) = (fx.floor() as usize, fy.floor() as usize);
        let (tx, ty) = (fx - i0 as f64, fy - j0 as f64);
        let (i1, j1) = ((i0 + 1).min(self.mesh_w), (j0 + 1).min(self.mesh_h));
        let uv = |i: usize, j: usize| {
            let v = &self.vert_data[j * nx + i];
            (v.uv[0] as f64, v.uv[1] as f64)
        };
        let (a, b, c, d) = (uv(i0, j0), uv(i1, j0), uv(i0, j1), uv(i1, j1));
        (
            a.0 * (1.0 - tx) * (1.0 - ty) + b.0 * tx * (1.0 - ty) + c.0 * (1.0 - tx) * ty
                + d.0 * tx * ty,
            a.1 * (1.0 - tx) * (1.0 - ty) + b.1 * tx * (1.0 - ty) + c.1 * (1.0 - tx) * ty
                + d.1 * tx * ty,
        )
    }

    /// 組出 preset 著色器要的那 17 個綁定。
    ///
    /// binding 的號碼必須跟 `shader.rs` 前言裡寫的一致 ——
    /// naga 把 GLSL 翻成 WGSL 時是照原本的數字擺的。
    fn shader_bind(&self, gpu: &Gpu, main: &wgpu::TextureView) -> wgpu::BindGroup {
        let p = self.pipes.as_ref().expect("沒有 pipes 不該走到這裡");
        let a = &self.assets;
        let blur: Vec<&wgpu::TextureView> = self.blur.iter().map(|b| &b.view).collect();
        // main 系列的五張目前都指向同一張；fw/fc/pw/pc 的差別要等分別做出
        // warp-only 與 comp-only 的中繼結果才有意義。
        let views: [&wgpu::TextureView; 13] = [
            main, main, main, main, main,
            blur[0], blur[1], blur[2],
            &a.noise_lq, &a.noise_lq_lite, &a.noise_mq, &a.noise_hq, &a.noise_lq,
        ];
        let mut entries: Vec<wgpu::BindGroupEntry> = Vec::new();
        for (i, v) in views.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: i as u32,
                resource: wgpu::BindingResource::TextureView(v),
            });
        }
        entries.push(wgpu::BindGroupEntry {
            binding: 13,
            resource: wgpu::BindingResource::TextureView(&a.noisevol_lq),
        });
        entries.push(wgpu::BindGroupEntry {
            binding: 14,
            resource: wgpu::BindingResource::TextureView(&a.noisevol_hq),
        });
        // 主貼圖的五種狀態。第 0 個（sampler_main）跟著 preset 的 `wrap` 走。
        let main_smp = if bv(&self.base, "wrap", 0.0) > 0.5 { &a.smp_fw } else { &a.smp_fc };
        for (b, smp) in [
            (15u32, main_smp),
            (16, &a.smp_fw),
            (17, &a.smp_fc),
            (18, &a.smp_pw),
            (19, &a.smp_pc),
            (21, &a.smp_noise),
            (22, &a.smp_noise_pt),
        ] {
            entries.push(wgpu::BindGroupEntry {
                binding: b,
                resource: wgpu::BindingResource::Sampler(smp),
            });
        }
        entries.push(wgpu::BindGroupEntry {
            binding: 20,
            resource: self.globals_buf.as_entire_binding(),
        });
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("milk shader"),
            layout: &p.layout,
            entries: &entries,
        })
    }

    fn fill_globals(&self, gpu: &Gpu, snd: &Sound, w: u32, h: u32) {
        let g = |k: &str, d: f64| -> f32 {
            self.prog.vars.find(k).map(|s| self.vm.get(s)).unwrap_or(d) as f32
        };
        let aspect = w as f32 / h.max(1) as f32;
        let (ax, ay) = if aspect > 1.0 { (1.0, 1.0 / aspect) } else { (aspect, 1.0) };
        let t = self.time as f32;
        // roam_* 是 MilkDrop 給 preset 的慢速漫遊值，讓畫面自己有呼吸
        let roam = |k: f32| {
            [(t * k).cos(), (t * k * 1.3).cos(), (t * k * 0.7).cos(), (t * k * 1.7).cos()]
        };
        let roam_s = |k: f32| {
            [(t * k).sin(), (t * k * 1.3).sin(), (t * k * 0.7).sin(), (t * k * 1.7).sin()]
        };
        let ts = [w as f32, h as f32, 1.0 / w as f32, 1.0 / h as f32];
        let noise_ts = |n: f32| [n, n, 1.0 / n, 1.0 / n];

        let mut q = [[0.0f32; 4]; 8];
        for i in 0..32 {
            q[i / 4][i % 4] = self.vm.get(self.slots.q[i]) as f32;
        }

        let gl = Globals {
            time: t,
            decay: g("decay", 0.9),
            bass: snd.bass,
            mid: snd.mid,
            treb: snd.treb,
            vol: (snd.bass + snd.mid + snd.treb) / 3.0,
            bass_att: snd.bass_att,
            mid_att: snd.mid_att,
            treb_att: snd.treb_att,
            vol_att: (snd.bass_att + snd.mid_att + snd.treb_att) / 3.0,
            frame: self.frame as f32,
            fps: self.fps as f32,
            gamma: g("gammaadj", 1.25),
            echo_zoom: g("echo_zoom", 1.0),
            echo_alpha: g("echo_alpha", 0.0),
            echo_orient: g("echo_orient", 0.0),
            invert: g("invert", 0.0),
            brighten: g("brighten", 0.0),
            darken: g("darken", 0.0),
            solarize: g("solarize", 0.0),
            fshader: g("fshader", 0.0),
            blur1_min: 0.0,
            blur1_max: 1.0,
            blur2_min: 0.0,
            blur2_max: 1.0,
            blur3_min: 0.0,
            blur3_max: 1.0,
            scale1: 1.0,
            scale2: 1.0,
            scale3: 1.0,
            bias1: 0.0,
            bias2: 0.0,
            bias3: 0.0,
            _pad0: 0.0,
            resolution: [w as f32, h as f32],
            aspect: [ax, ay, 1.0 / ax, 1.0 / ay],
            texsize: ts,
            texsize_noise_lq: noise_ts(256.0),
            texsize_noise_mq: noise_ts(256.0),
            texsize_noise_hq: noise_ts(256.0),
            texsize_noise_lq_lite: noise_ts(32.0),
            texsize_noisevol_lq: noise_ts(32.0),
            texsize_noisevol_hq: noise_ts(32.0),
            q,
            slow_roam_cos: roam(0.05),
            roam_cos: roam(0.3),
            slow_roam_sin: roam_s(0.05),
            roam_sin: roam_s(0.3),
            rand_frame: [
                (t * 12.9898).sin().abs(),
                (t * 78.233).sin().abs(),
                (t * 45.164).sin().abs(),
                (t * 94.673).sin().abs(),
            ],
            rand_preset: [0.37, 0.61, 0.13, 0.89],
            hue: self.hue_base(),
        };
        gpu.queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&gl));
    }

    /// 從 `src` 做一條逐級減半的模糊鏈，給 preset 的 sampler_blur1/2/3 用。
    fn build_blur(&self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, src: &wgpu::TextureView) {
        let mut from = src;
        for level in 0..self.blur.len() {
            let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.tex_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(from),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            {
                let mut pass = gfx::pass(enc, &self.blur[level].view, "milk blur", None);
                pass.set_pipeline(&self.blur_pipe);
                pass.set_bind_group(0, &self.warp_uni_bind, &[]);
                pass.set_bind_group(1, &bind, &[]);
                pass.draw(0..3, 0..1);
            }
            from = &self.blur[level].view;
        }
    }

    /// 一級一級把 mip 填滿。**一定要在畫完 curr 的第 0 級之後、
    /// 下一幀拿它當 `sampler_main` 之前**做。
    fn build_mips(&self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, tex: &Offscreen2) {
        for level in 1..tex.mips.len() {
            let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.tex_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&tex.mips[level - 1]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            let mut pass = gfx::pass(enc, &tex.mips[level], "milk mip", None);
            pass.set_pipeline(&self.mip_pipe);
            pass.set_bind_group(0, &self.warp_uni_bind, &[]);
            pass.set_bind_group(1, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// 算一幀，結果寫進 `out`。
    pub fn render(
        &mut self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        out: &Offscreen,
        snd: &Sound,
        wave: &[f32; audio::WAVE],
        dt: f64,
    ) {
        let (w, h) = (out.width, out.height);
        self.resize(gpu, w, h);

        self.time += dt;
        self.frame += 1;
        // fps 用跟 butterchurn 一樣的平滑係數
        if dt > 1e-6 {
            self.fps = 0.93 * self.fps + 0.07 / dt;
        }

        self.set_builtins(snd, w, h);
        self.build_mesh(w, h);
        self.build_wave(wave);
        self.build_shapes();
        self.build_cwaves(wave, &snd.bins);
        self.build_extras();

        gpu.queue.write_buffer(&self.verts, 0, bytemuck::cast_slice(&self.vert_data));
        if !self.wave_data.is_empty() {
            gpu.queue.write_buffer(&self.wave_verts, 0, bytemuck::cast_slice(&self.wave_data));
        }
        // 超過緩衝容量就截掉，不要讓壞掉的 preset 把程式打死
        if !self.shape_data.is_empty() {
            self.shape_data.truncate(SHAPE_MAX);
            gpu.queue.write_buffer(&self.shape_verts, 0, bytemuck::cast_slice(&self.shape_data));
        }
        if !self.shape_tex_data.is_empty() {
            self.shape_tex_data.truncate(SHAPE_MAX);
            gpu.queue.write_buffer(
                &self.shape_tex_verts,
                0,
                bytemuck::cast_slice(&self.shape_tex_data),
            );
        }
        if !self.cwave_data.is_empty() {
            self.cwave_data.truncate(CWAVE_MAX);
            gpu.queue.write_buffer(&self.cwave_verts, 0, bytemuck::cast_slice(&self.cwave_data));
        }
        if !self.extra_data.is_empty() {
            self.extra_data.truncate(8192);
            gpu.queue.write_buffer(&self.extra_verts, 0, bytemuck::cast_slice(&self.extra_data));
        }

        let gv = |vm: &Vm, vars: &eel::Vars, k: &str, d: f64| -> f32 {
            vars.find(k).map(|s| vm.get(s)).unwrap_or(d) as f32
        };
        // decay 與 gammaadj 也可能被 frame_eqs 改，一樣要讀執行後的值
        let decay = gv(&self.vm, &self.prog.vars, "decay", 0.9).clamp(0.0, 1.0);
        let gamma = gv(&self.vm, &self.prog.vars, "gammaadj", 1.25).clamp(0.0, 8.0);
        // 這幾個 baseVals 都可能被 frame_eqs 改，所以要讀執行後的值
        let cv = |k: &str, d: f64| -> f32 {
            let fallback = bv(&self.base, k, d);
            self.prog.vars.find(k).map(|sl| self.vm.get(sl)).unwrap_or(fallback) as f32
        };
        let warp_u = CompUniforms {
            misc: [decay, 0.0, 0.0, 0.0],
            res: [w as f32, h as f32, 0.0, 0.0],
            echo: [0.0; 4],
            hue: [[1.0; 4]; 4],
        };
        let comp_u = CompUniforms {
            misc: [gamma, cv("brighten", 0.0), cv("darken", 0.0), cv("solarize", 0.0)],
            res: [w as f32, h as f32, cv("invert", 0.0), cv("fshader", 0.0)],
            echo: [
                cv("echo_alpha", 0.0).clamp(0.0, 1.0),
                {
                    let z = cv("echo_zoom", 1.0);
                    if z.abs() < 1e-4 { 1.0 } else { z }
                },
                cv("echo_orient", 0.0),
                0.0,
            ],
            hue: self.hue_base(),
        };
        gpu.queue.write_buffer(&self.warp_uni, 0, bytemuck::bytes_of(&warp_u));
        gpu.queue.write_buffer(&self.comp_uni, 0, bytemuck::bytes_of(&comp_u));

        let use_shader = self.pipes.is_some();
        if use_shader {
            self.fill_globals(gpu, snd, w, h);
            self.build_blur(gpu, enc, &self.prev.view);
        }

        // ---- 1：把上一幀經過變形網格重畫進 curr ----
        {
            let prev_bind = self.bind_tex(gpu, &self.prev.view);
            let shader_bind = use_shader.then(|| self.shader_bind(gpu, &self.prev.view));
            // 目標要用單一級別的視圖，帶著全部 mip 的那個不能當附件
            let mut pass =
                gfx::pass(enc, &self.curr.mips[0], "milk warp", Some(wgpu::Color::BLACK));
            // 診斷開關：只關掉其中一段，好分辨是哪一支著色器把畫面弄爆
            let skip = std::env::var_os("VITRO_MILK_NO_WARP_SHADER").is_some();
            match (&self.pipes, &shader_bind) {
                (Some(p), Some(b)) if !skip => {
                    pass.set_pipeline(&p.warp);
                    pass.set_bind_group(0, b, &[]);
                }
                _ => {
                    pass.set_pipeline(&self.warp_pipe);
                    pass.set_bind_group(0, &self.warp_uni_bind, &[]);
                    pass.set_bind_group(1, &prev_bind, &[]);
                }
            }
            pass.set_vertex_buffer(0, self.verts.slice(..));
            pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.index_count, 0, 0..1);

            // ---- 2a：動態向量（原版畫在形狀之前）----
            if !self.extra_lines.is_empty() {
                pass.set_pipeline(&self.mv_pipe);
                pass.set_vertex_buffer(0, self.extra_verts.slice(..));
                for (start, count) in &self.extra_lines {
                    pass.draw(*start..*start + *count, 0..1);
                }
            }

            // ---- 2b：custom shapes（MilkDrop 的順序是形狀先於波形）----
            if !self.shape_data.is_empty() || !self.shape_tex_data.is_empty() {
                pass.set_vertex_buffer(0, self.shape_verts.slice(..));
                for (start, count, additive) in &self.shape_tris {
                    if start + count > SHAPE_MAX as u32 {
                        continue;
                    }
                    pass.set_pipeline(if *additive { &self.shape_pipe_add } else { &self.shape_pipe });
                    pass.draw(*start..*start + *count, 0..1);
                }
                if !self.shape_tex_data.is_empty() {
                    pass.set_bind_group(0, &self.warp_uni_bind, &[]);
                    pass.set_bind_group(1, &prev_bind, &[]);
                    pass.set_vertex_buffer(0, self.shape_tex_verts.slice(..));
                    for (start, count, additive) in &self.shape_tex_tris {
                        pass.set_pipeline(if *additive {
                            &self.shape_tex_pipe_add
                        } else {
                            &self.shape_tex_pipe
                        });
                        pass.draw(*start..*start + *count, 0..1);
                    }
                    pass.set_vertex_buffer(0, self.shape_verts.slice(..));
                }
                pass.set_pipeline(&self.wave_pipe);
                for (start, count) in &self.shape_lines {
                    if start + count > SHAPE_MAX as u32 {
                        continue;
                    }
                    pass.draw(*start..*start + *count, 0..1);
                }
            }

            // ---- 2c：custom waves。順序照原版：形狀、自訂波形、內建波形 ----
            if !self.cwave_data.is_empty() {
                pass.set_vertex_buffer(0, self.cwave_verts.slice(..));
                for (start, count, additive) in &self.cwave_dots {
                    pass.set_pipeline(if *additive {
                        &self.shape_pipe_add
                    } else {
                        &self.shape_pipe
                    });
                    pass.draw(*start..*start + *count, 0..1);
                }
                for (start, count, additive) in &self.cwave_strips {
                    pass.set_pipeline(if *additive {
                        &self.wave_pipe_add
                    } else {
                        &self.wave_pipe
                    });
                    pass.draw(*start..*start + *count, 0..1);
                }
            }

            // ---- 2d：內建波形疊上去 ----
            if self.wave_count > 1 {
                let additive = bv(&self.base, "additivewave", 0.0) > 0.5;
                pass.set_vertex_buffer(0, self.wave_verts.slice(..));
                pass.set_pipeline(if additive { &self.wave_pipe_add } else { &self.wave_pipe });
                for (start, count) in &self.wave_strips {
                    pass.draw(*start..*start + *count, 0..1);
                }
                pass.set_pipeline(if additive { &self.shape_pipe_add } else { &self.shape_pipe });
                for (start, count) in &self.wave_dot_tris {
                    pass.draw(*start..*start + *count, 0..1);
                }
            }

            // ---- 2e：暗中心與邊框，最後蓋上去 ----
            if !self.extra_tris.is_empty() {
                pass.set_pipeline(&self.shape_pipe);
                pass.set_vertex_buffer(0, self.extra_verts.slice(..));
                for (start, count) in &self.extra_tris {
                    pass.draw(*start..*start + *count, 0..1);
                }
            }
        }

        // ---- 3：合成到輸出 ----
        {
            let curr_bind = self.bind_tex(gpu, &self.curr.view);
            let shader_bind = use_shader.then(|| self.shader_bind(gpu, &self.curr.view));
            let mut pass = gfx::pass(enc, &out.view, "milk comp", Some(wgpu::Color::BLACK));
            let skip = std::env::var_os("VITRO_MILK_NO_COMP_SHADER").is_some();
            match (&self.pipes, &shader_bind) {
                (Some(p), Some(b)) if !skip => {
                    pass.set_pipeline(&p.comp);
                    pass.set_bind_group(0, b, &[]);
                }
                _ => {
                    pass.set_pipeline(&self.comp_pipe);
                    pass.set_bind_group(0, &self.comp_uni_bind, &[]);
                    pass.set_bind_group(1, &curr_bind, &[]);
                }
            }
            pass.draw(0..3, 0..1);
        }

        // 下一幀 warp 會把 curr 當成 `sampler_main` 取樣，mip 要先備好
        self.build_mips(gpu, enc, &self.curr);

        std::mem::swap(&mut self.prev, &mut self.curr);
    }
}

/// 變數不一定存在（沒有任何方程式提過它就不會有槽位），所以統一走這個。
#[inline]
fn set_opt(vm: &mut Vm, slot: Option<usize>, v: f64) {
    if let Some(sl) = slot {
        vm.set(sl, v);
    }
}

/// 粗線的四次偏移。原版是一個像素，NDC 裡就是 2/邊長。
#[inline]
fn thick_offset(k: usize, w: f64, h: f64) -> (f64, f64) {
    match k {
        1 => (2.0 / w, 0.0),
        2 => (0.0, 2.0 / h),
        3 => (2.0 / w, 2.0 / h),
        _ => (0.0, 0.0),
    }
}

/// 一個點展開成兩個三角形。WGSL 沒有 `gl_PointSize`，只能自己做方塊。
fn push_dot(out: &mut Vec<WaveVert>, q: &WaveVert, hx: f64, hy: f64, ox: f64, oy: f64) {
    let (x, y) = (q.pos[0] as f64 + ox, q.pos[1] as f64 + oy);
    let c = q.color;
    let corner = |dx: f64, dy: f64| WaveVert { pos: [(x + dx) as f32, (y + dy) as f32], color: c };
    out.push(corner(-hx, -hy));
    out.push(corner(hx, -hy));
    out.push(corner(-hx, hy));
    out.push(corner(hx, -hy));
    out.push(corner(hx, hy));
    out.push(corner(-hx, hy));
}

/// butterchurn 的 `smoothWaveAndColor`：在每兩點之間補一個內插點。
///
/// 係數 (−0.15, 1.15, 1.15, −0.15)/2 是 Catmull-Rom 的變體 —— 會稍微過衝，
/// 那個過衝就是原版線條看起來比取樣點還滑順的原因。顏色不內插，
/// 補出來的點直接沿用前一點的顏色（原版也是）。
fn smooth_wave(src: &[WaveVert]) -> Vec<WaveVert> {
    let n = src.len();
    if n < 2 {
        return src.to_vec();
    }
    let mut out: Vec<WaveVert> = Vec::with_capacity(2 * n - 1);
    let mut prev = 0usize;
    let mut l = 1usize;
    for m in 0..n - 1 {
        let a = l;
        l = (m + 2).min(n - 1);
        out.push(src[m]);
        let mid = |k: usize| -> f32 {
            0.5 * (-0.15 * src[prev].pos[k] + 1.15 * src[m].pos[k] + 1.15 * src[a].pos[k]
                - 0.15 * src[l].pos[k])
        };
        out.push(WaveVert { pos: [mid(0), mid(1)], color: src[m].color });
        prev = m;
    }
    out.push(src[n - 1]);
    out
}

/// 除以零與 0 的次方在這條管線裡會讓整張網格變成 NaN，統一擋掉。
#[inline]
fn nz(v: f64) -> f64 {
    if v.abs() < 1e-9 || !v.is_finite() {
        1e-9
    } else {
        v
    }
}

// =================================================================
// preset 自帶的逐像素著色器
// =================================================================

/// 對應 GLSL 前言裡那個 `layout(std140) uniform Globals` 區塊。
///
/// **欄位順序與補洞必須跟 GLSL 那邊逐位元組對齊**，錯一個就是整片亂碼。
/// std140 的規則：33 個 float 佔 132，vec2 要對齊到 8（所以補 1 個 float
/// 到 136），vec4 要對齊到 16（144 剛好整除）。全長 496。
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    time: f32,
    decay: f32,
    bass: f32,
    mid: f32,
    treb: f32,
    vol: f32,
    bass_att: f32,
    mid_att: f32,
    treb_att: f32,
    vol_att: f32,
    frame: f32,
    fps: f32,
    gamma: f32,
    echo_zoom: f32,
    echo_alpha: f32,
    echo_orient: f32,
    invert: f32,
    brighten: f32,
    darken: f32,
    solarize: f32,
    fshader: f32,
    blur1_min: f32,
    blur1_max: f32,
    blur2_min: f32,
    blur2_max: f32,
    blur3_min: f32,
    blur3_max: f32,
    scale1: f32,
    scale2: f32,
    scale3: f32,
    bias1: f32,
    bias2: f32,
    bias3: f32,
    _pad0: f32,
    resolution: [f32; 2],
    aspect: [f32; 4],
    texsize: [f32; 4],
    texsize_noise_lq: [f32; 4],
    texsize_noise_mq: [f32; 4],
    texsize_noise_hq: [f32; 4],
    texsize_noise_lq_lite: [f32; 4],
    texsize_noisevol_lq: [f32; 4],
    texsize_noisevol_hq: [f32; 4],
    q: [[f32; 4]; 8],
    slow_roam_cos: [f32; 4],
    roam_cos: [f32; 4],
    slow_roam_sin: [f32; 4],
    roam_sin: [f32; 4],
    rand_frame: [f32; 4],
    rand_preset: [f32; 4],
    /// hue_shader 的四個角。**接在最後面**是故意的：前面 496 個位元組
    /// 的排版跟 GLSL 前言逐位元組對齊，這四個 vec4 只有頂點階段讀，
    /// GLSL 那邊不宣告也不影響（緩衝比它的 block 大是允許的）。
    hue: [[f32; 4]; 4],
}

/// 一個 preset 編好的兩支著色器。編不起來就整個是 None，退回內建管線。
pub struct Pipes {
    warp: wgpu::RenderPipeline,
    comp: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

/// naga 把 GLSL 翻成 WGSL 時，所有資源都落在 group 0，binding 沿用
/// GLSL 裡寫的數字 —— 所以這裡的 layout 要跟前言裡的 `layout(binding = N)` 一致。
fn shader_bind_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    let mut entries: Vec<wgpu::BindGroupLayoutEntry> = Vec::new();
    for i in 0..13u32 {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: i,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
    }
    for i in 13..15u32 {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: i,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D3,
                multisampled: false,
            },
            count: None,
        });
    }
    // 15..19 是主貼圖的五種取樣器狀態，21/22 給雜訊。
    // point 取樣的那幾顆要宣告成 NonFiltering，不然 wgpu 會擋。
    for (b, filtering) in [(15u32, true), (16, true), (17, true), (18, false), (19, false),
                           (21, true), (22, false)] {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: b,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(if filtering {
                wgpu::SamplerBindingType::Filtering
            } else {
                wgpu::SamplerBindingType::NonFiltering
            }),
            count: None,
        });
    }
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: 20,
        // 頂點階段要讀 hue_shader 的四個角，所以兩個階段都要
        visibility: wgpu::ShaderStages::FRAGMENT.union(wgpu::ShaderStages::VERTEX),
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    });
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("milk shader"),
        entries: &entries,
    })
}

/// 把 preset 的 warp／comp 編成管線。任何一步失敗就整個放棄 ——
/// 只編出一半的話畫面會半調子，不如乾脆退回內建管線。
fn build_pipes(gpu: &Gpu, raw: &RawPreset, wgsl: &wgpu::ShaderModule) -> Option<Pipes> {
    let device = &gpu.device;
    let layout = shader_bind_layout(device);
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("milk shader"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });

    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

    let make = |src: &str, kind: crate::shader::Kind| -> Option<wgpu::ShaderModule> {
        if src.trim().is_empty() {
            return None;
        }
        let glsl = crate::shader::wrap(src, kind);
        Some(device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("milk preset"),
            source: wgpu::ShaderSource::Glsl {
                shader: glsl.as_str().into(),
                stage: naga::ShaderStage::Fragment,
                defines: Default::default(),
            },
        }))
    };
    let warp_fs = make(&raw.warp, crate::shader::Kind::Warp);
    let comp_fs = make(&raw.comp, crate::shader::Kind::Comp);

    let warp = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("milk warp(preset)"),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: wgsl,
            entry_point: Some("vs_warp_shader"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<MeshVert>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 8,
                        shader_location: 1,
                    },
                ],
            })],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: warp_fs.as_ref()?,
            entry_point: Some("main"),
            targets: &[Some(FEEDBACK_FORMAT.into())],
            compilation_options: Default::default(),
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });

    let comp = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("milk comp(preset)"),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: wgsl,
            entry_point: Some("vs_comp_shader"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: comp_fs.as_ref()?,
            entry_point: Some("main"),
            targets: &[Some(crate::gfx::OFFSCREEN_FORMAT.into())],
            compilation_options: Default::default(),
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    });

    if let Some(e) = pollster::block_on(scope.pop()) {
        crate::warn!("preset 著色器編不起來，退回內建管線：{}", first_error_line(&e.to_string()));
        return None;
    }
    Some(Pipes { warp, comp, layout })
}

fn first_error_line(msg: &str) -> String {
    msg.lines()
        .map(|l| l.trim())
        .find(|l| l.contains("error:"))
        .unwrap_or("?")
        .to_string()
}
