//! 視覺效果視圖的控制器 —— 對應 web 版的 `viz.js`。
//!
//! 管 preset 的載入、分類、洗牌輪播與切換。算圖本身在 [`crate::milk`]。
//!
//! 幾個刻意跟 Python 版一致的行為：
//!   - **每次進入這個視圖就重新洗牌**，不是從上次的位置接下去
//!   - 分類用「有沒有逐像素著色器」分復古／現代 —— 著色器是 MilkDrop 2
//!     才加的，沒有著色器的就是 2001 年那一代
//!   - 我的最愛存在 `config.json` 的 `viz_favourites`，存的是 preset 名稱

use std::sync::mpsc::Receiver;

use crate::audio;
use crate::config::Config;
use crate::gfx::{Gpu, Offscreen};
use crate::milk::{Milk, Sound};
use crate::preset::RawPreset;

/// 換 preset 時把新的那張溶到舊的上面。細節見 `blend.wgsl`。
struct BlendPipe {
    pipeline: wgpu::RenderPipeline,
    uni: wgpu::Buffer,
    uni_bind: wgpu::BindGroup,
    tex_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl BlendPipe {
    fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let uni_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blend uni"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blend tex"),
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
        let shader = device.create_shader_module(wgpu::include_wgsl!("blend.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&uni_layout), Some(&tex_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blend"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: crate::gfx::OFFSCREEN_FORMAT,
                    // 著色器輸出的是預乘 alpha
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
        let uni = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blend uni"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uni_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &uni_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uni.as_entire_binding() }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blend"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self { pipeline, uni, uni_bind, tex_layout, sampler }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    All,
    /// 沒有逐像素著色器 —— MilkDrop 1 那一代的純網格變形
    Retro,
    /// 有著色器但不長
    Mid,
    /// 著色器長度在中位數以上
    Flashy,
    Fav,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            // 用語與順序照 web 版 viz.js 的 CATS，兩邊的設定面板講的是同一件事
            Category::All => "全部",
            Category::Fav => "最愛",
            Category::Retro => "復古",
            Category::Mid => "經典",
            Category::Flashy => "炫技",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Category::All => Category::Fav,
            Category::Fav => Category::Retro,
            Category::Retro => Category::Mid,
            Category::Mid => Category::Flashy,
            Category::Flashy => Category::All,
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "retro" => Category::Retro,
            "mid" => Category::Mid,
            "flashy" => Category::Flashy,
            "fav" => Category::Fav,
            _ => Category::All,
        }
    }
    /// 設定面板要從設定檔的字串反推目前是哪一個。
    pub fn parse_pub(s: &str) -> Self {
        Self::parse(s)
    }
    /// 倒著轉。設定面板的左右鍵與滾輪要用。
    pub fn prev(self) -> Self {
        match self {
            Category::All => Category::Flashy,
            Category::Fav => Category::All,
            Category::Retro => Category::Fav,
            Category::Mid => Category::Retro,
            Category::Flashy => Category::Mid,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Category::All => "all",
            Category::Retro => "retro",
            Category::Mid => "mid",
            Category::Flashy => "flashy",
            Category::Fav => "fav",
        }
    }
}

struct Entry {
    name: String,
    raw: RawPreset,
    cat: Category,
}

/// 自動換下一個的間隔。
const DWELL: f64 = 22.0;

/// 換 preset 的溶解長度。原版 `blendDuration` 預設 5.7 秒，
/// 那是配合它「連取樣座標都內插」的做法；這裡是兩張畫面互溶，
/// 拖那麼久會看到兩個畫面打架，取一半比較乾淨。
const BLEND_S: f64 = 2.8;

pub struct Viz {
    rx: Option<Receiver<Vec<(String, RawPreset)>>>,
    /// 載入開始過了沒。視覺效果關掉（`viz_enabled`）的話開程式時不載，
    /// 等真的進視圖才載 —— 跟 Python 版一樣
    load_started: bool,
    items: Vec<Entry>,
    /// 洗好的播放順序（存的是 `items` 的索引）
    queue: Vec<usize>,
    qpos: usize,
    milk: Option<Milk>,
    /// 上一顆 preset。換的時候留著溶解，溶完就丟。
    prev: Option<Milk>,
    prev_tex: Option<Offscreen>,
    /// 溶解進度，1 = 已經完全是新的那顆
    blend: f32,
    /// 這次換用的溶解圖樣，見 `blend.wgsl`
    pattern: [f32; 4],
    blend_pipe: Option<BlendPipe>,
    pub cat: Category,
    pub favourites: Vec<String>,
    /// 鎖住就不自動換下一個（P 鍵，或從選單挑過）。
    pub locked: bool,
    preset_shaders: bool,
    /// MilkDrop 的方程式是按「幀」推進的，不是按時間 —— 在 165Hz 的螢幕上
    /// 放著不管，preset 會跑得比當年快五倍多。所以自己限幀：
    /// 還沒到下一格的時間，就把上一格原樣貼出去。0 = 不限。
    cap: f64,
    accum: f64,
    /// 限幀用的中繼畫布。**不能直接畫進 `out`** —— 跳過的那幾幀
    /// `out` 會留著上一幀「已經疊過控制列」的內容，疊層會愈疊愈濃。
    frame_tex: Option<Offscreen>,
    blit: Option<crate::gfx::Blit>,
    since_switch: f64,
    sound: Sound,
    /// 雜訊貼圖等固定資源，整個程式一份，換 preset 時不重建
    assets: Option<std::sync::Arc<crate::milkasset::MilkAssets>>,
    rng: u64,
    pub message: String,
    /// 進入視圖的時刻。「沒有偵測到聲音」從這裡起算寬限，一進來不會立刻跳出來。
    pub entered: std::time::Instant,
}

impl Viz {
    pub fn new(cfg: &Config) -> Self {
        let mut v = Self::idle(cfg);
        if cfg.viz_enabled {
            v.start_load();
        }
        v
    }

    /// 3 MB 的 JSON 加上 395 個 preset 的剖析要幾百毫秒，
    /// 不能卡在開啟視圖的那一幀上，所以丟背景執行緒。
    fn start_load(&mut self) {
        if self.load_started {
            return;
        }
        self.load_started = true;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("preset-load".into())
            .spawn(move || {
                // 外面有資料夾就用外面的（DATA 旁邊，或目前目錄），沒有才用編進 exe 的那份
                let dirs = [crate::config::data_dir().join("presets"), "presets".into()];
                let got = match dirs.iter().find(|d| d.join("base.json").exists()) {
                    Some(d) => crate::preset::load_all(d),
                    None => crate::preset::load_embedded(),
                };
                match got {
                    Ok(v) => {
                        let _ = tx.send(v);
                    }
                    Err(e) => {
                        crate::warn!("preset 載入失敗：{e}");
                        let _ = tx.send(Vec::new());
                    }
                }
            })
            .ok();
        self.rx = Some(rx);
    }

    fn idle(cfg: &Config) -> Self {
        Self {
            rx: None,
            load_started: false,
            items: Vec::new(),
            queue: Vec::new(),
            qpos: 0,
            milk: None,
            prev: None,
            prev_tex: None,
            blend: 1.0,
            pattern: [1.0, 0.0, 5.0, 0.0],
            blend_pipe: None,
            cat: Category::parse(&cfg.viz_category),
            favourites: cfg.viz_favourites.clone(),
            locked: false,
            preset_shaders: cfg.viz_preset_shaders,
            cap: cfg.viz_fps as f64,
            accum: 0.0,
            frame_tex: None,
            blit: None,
            since_switch: 0.0,
            sound: Sound::default(),
            assets: None,
            rng: 0x9E37_79B9_7F4A_7C15,
            message: "正在載入 preset…".into(),
            entered: std::time::Instant::now(),
        }
    }

    pub fn ready(&self) -> bool {
        !self.items.is_empty()
    }

    pub fn current_name(&self) -> &str {
        self.milk.as_ref().map(|m| m.name.as_str()).unwrap_or("")
    }

    pub fn count(&self) -> usize {
        self.queue.len()
    }

    pub fn position(&self) -> usize {
        self.qpos + 1
    }

    pub fn take_load(&mut self) {
        let Some(rx) = &self.rx else { return };
        let Ok(list) = rx.try_recv() else { return };
        self.rx = None;
        if list.is_empty() {
            self.message = "找不到 preset —— 先跑一次 --extract-presets".into();
            return;
        }

        // 分類：沒有著色器的是復古；有的再用長度的中位數切成中等／炫砲。
        // 這個判準跟 web 版一樣，而且是執行期算的中位數不是寫死的閾值。
        let mut lens: Vec<usize> =
            list.iter().filter(|(_, p)| p.has_shaders()).map(|(_, p)| p.shader_len()).collect();
        lens.sort_unstable();
        let median = lens.get(lens.len() / 2).copied().unwrap_or(0);

        self.items = list
            .into_iter()
            .map(|(name, raw)| {
                let cat = if !raw.has_shaders() {
                    Category::Retro
                } else if raw.shader_len() < median {
                    Category::Mid
                } else {
                    Category::Flashy
                };
                Entry { name, raw, cat }
            })
            .collect();

        let retro = self.items.iter().filter(|e| e.cat == Category::Retro).count();
        let mid = self.items.iter().filter(|e| e.cat == Category::Mid).count();
        let flashy = self.items.iter().filter(|e| e.cat == Category::Flashy).count();
        println!(
            "preset 載入完成：{} 個（復古 {retro}／經典 {mid}／炫技 {flashy}），著色器長度中位數 {median}",
            self.items.len()
        );
        self.message.clear();
        self.reshuffle();
    }

    fn matches(&self, e: &Entry) -> bool {
        match self.cat {
            Category::All => true,
            Category::Fav => self.favourites.iter().any(|f| f == &e.name),
            c => e.cat == c,
        }
    }

    fn rand(&mut self) -> u64 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Fisher–Yates。每次進入視圖都重洗，所以不會每次開都看到同樣的開頭。
    pub fn reshuffle(&mut self) {
        let mut q: Vec<usize> = (0..self.items.len())
            .filter(|i| self.matches(&self.items[*i]))
            .collect();
        if q.is_empty() {
            // 這個分類是空的（最愛還沒加東西），退回全部
            q = (0..self.items.len()).collect();
            if self.cat == Category::Fav {
                self.message = "我的最愛還是空的，先播全部".into();
            }
        }
        for i in (1..q.len()).rev() {
            let j = (self.rand() % (i as u64 + 1)) as usize;
            q.swap(i, j);
        }
        self.queue = q;
        self.qpos = 0;
        // 重洗是「換一整組」，不是換下一顆 —— 溶解中途重洗的話舊的那顆
        // 已經不屬於這一組了，直接丟掉比較乾淨。
        self.milk = None;
        self.prev = None;
        self.prev_tex = None;
        self.blend = 1.0;
        self.since_switch = 0.0;
    }

    /// 切到視圖時呼叫。
    pub fn enter(&mut self) {
        self.entered = std::time::Instant::now();
        self.start_load();
        if self.ready() {
            self.reshuffle();
        }
    }

    /// 換分類。**目前這顆也在新分類裡的話就留著**，只換後面的輪播順序 ——
    /// 在 preset 選單裡按 Tab 切分類時，畫面不該跟著跳走。
    /// 不在新分類裡才溶解到新清單的第一個。web 版的 `setCat` 也是這樣。
    pub fn set_category(&mut self, c: Category) {
        let cur = self.current_item();
        self.cat = c;
        if !self.ready() {
            return;
        }
        let milk = self.milk.take();
        self.reshuffle();
        match (cur, milk) {
            (Some(i), Some(m)) if self.queue.contains(&i) => {
                let at = self.queue.iter().position(|q| *q == i).unwrap_or(0);
                self.queue.swap(0, at);
                self.milk = Some(m);
            }
            (_, m) => {
                // 舊的那顆留著當溶解的來源
                self.milk = m;
                self.step(0);
            }
        }
    }

    /// 目前在跑的那顆在 `items` 裡的索引。
    pub fn current_item(&self) -> Option<usize> {
        self.milk.as_ref()?;
        self.queue.get(self.qpos).copied()
    }

    pub fn item_name(&self, i: usize) -> &str {
        self.items.get(i).map(|e| e.name.as_str()).unwrap_or("")
    }

    /// 目前分類裡的全部 preset，照名稱排好（不分大小寫）。選單用。
    ///
    /// 跟輪播佇列不同：佇列是洗過的，而且「我的最愛」是空的時候會退回
    /// 全部；選單要的是穩定的順序，空就是空。
    pub fn pool(&self) -> Vec<usize> {
        let mut v: Vec<usize> =
            (0..self.items.len()).filter(|i| self.matches(&self.items[*i])).collect();
        v.sort_by_cached_key(|i| self.items[*i].name.to_lowercase());
        v
    }

    /// 直接換到某一顆。`blend = false` 是選單預覽用的 —— 上下鍵移動時
    /// 要立刻看到，慢慢溶解反而看不出差別。
    pub fn show(&mut self, item: usize, blend: bool) {
        if item >= self.items.len() || self.current_item() == Some(item) {
            return;
        }
        match self.queue.iter().position(|q| *q == item) {
            Some(at) => self.qpos = at,
            None => {
                // 不在目前的輪播裡（例如最愛是空的、退回全部的時候）
                let at = (self.qpos + 1).min(self.queue.len());
                self.queue.insert(at, item);
                self.qpos = at;
            }
        }
        if blend {
            self.prev = self.milk.take();
            self.blend = if self.prev.is_some() { 0.0 } else { 1.0 };
            self.pattern = self.pick_pattern();
        } else {
            self.milk = None;
            self.prev = None;
            self.blend = 1.0;
        }
        self.since_switch = 0.0;
    }

    pub fn toggle_favourite(&mut self) -> bool {
        let name = self.current_name().to_string();
        self.toggle_favourite_of(&name)
    }

    /// 選單裡每一列都有星號，點的不一定是正在跑的那顆。
    pub fn toggle_favourite_of(&mut self, name: &str) -> bool {
        let name = name.to_string();
        if name.is_empty() {
            return false;
        }
        if let Some(i) = self.favourites.iter().position(|f| f == &name) {
            self.favourites.remove(i);
            self.message = format!("已移出我的最愛：{name}");
            false
        } else {
            self.favourites.push(name.clone());
            self.message = format!("已加入我的最愛：{name}");
            true
        }
    }

    pub fn is_favourite(&self) -> bool {
        self.is_favourite_name(self.current_name())
    }

    pub fn is_favourite_name(&self, n: &str) -> bool {
        !n.is_empty() && self.favourites.iter().any(|f| f == n)
    }

    /// 換用不用 preset 自帶的著色器。現有的那顆要丟掉重編才會生效。
    pub fn set_shaders(&mut self, on: bool) {
        if self.preset_shaders != on {
            self.preset_shaders = on;
            self.milk = None;
        }
    }

    /// 換影格上限。0 = 不限。
    pub fn set_cap(&mut self, fps: u32) {
        self.cap = fps as f64;
        self.accum = 0.0;
    }

    pub fn step(&mut self, d: i32) {
        if self.queue.is_empty() {
            return;
        }
        let n = self.queue.len() as i32;
        self.qpos = (((self.qpos as i32 + d) % n + n) % n) as usize;
        // 現在這顆留著當溶解的來源，新的那顆等 `ensure_milk` 生
        self.prev = self.milk.take();
        self.blend = if self.prev.is_some() { 0.0 } else { 1.0 };
        self.pattern = self.pick_pattern();
        self.since_switch = 0.0;
    }

    /// 隨機挑一種溶解圖樣。回傳 `(cosθ, sinθ, 1/寬度, 模式)`，
    /// 直接就是 `blend.wgsl` 要的那個 uniform。
    fn pick_pattern(&mut self) -> [f32; 4] {
        // 診斷用：固定成某一種，不然隨機挑的圖樣很難確認有沒有真的在動
        match std::env::var("VITRO_BLEND_PAT").ok().as_deref() {
            Some("dir") => return [1.0, 0.0, 1.0 / 0.25, 0.0],
            Some("in") => return [0.0, 0.0, 1.0 / 0.25, 1.0],
            Some("out") => return [0.0, 0.0, 1.0 / 0.25, 2.0],
            _ => {}
        }
        let r0 = (self.rand() >> 11) as f32 / (1u64 << 53) as f32;
        let r1 = (self.rand() >> 11) as f32 / (1u64 << 53) as f32;
        let r2 = (self.rand() >> 11) as f32 / (1u64 << 53) as f32;
        if r0 < 0.5 {
            // 方向抹除。寬度 0.1～0.3，角度隨便。
            let ang = r1 * std::f32::consts::TAU;
            let w = 0.1 + 0.2 * r2;
            [ang.cos(), ang.sin(), 1.0 / w, 0.0]
        } else {
            // 放射。原版的寬度是兩個亂數疊出來的，偏小值比較多。
            let w = 0.02 + 0.14 * r1 + 0.34 * r2;
            let inward = self.rand() & 1 == 0;
            [0.0, 0.0, 1.0 / w, if inward { 1.0 } else { 2.0 }]
        }
    }

    fn ensure_milk(&mut self, gpu: &Gpu, w: u32, h: u32) {
        if self.milk.is_some() || self.queue.is_empty() {
            return;
        }
        let assets = self
            .assets
            .get_or_insert_with(|| std::sync::Arc::new(crate::milkasset::MilkAssets::new(gpu)))
            .clone();
        // 換 preset 時偶爾會遇到編不起來的，往下找，不要卡住
        for _ in 0..self.queue.len().min(16) {
            let idx = self.queue[self.qpos];
            let e = &self.items[idx];
            match Milk::new(
                gpu,
                assets.clone(),
                e.name.clone(),
                e.raw.clone(),
                w,
                h,
                self.preset_shaders,
            ) {
                Ok(m) => {
                    self.milk = Some(m);
                    return;
                }
                Err(err) => {
                    crate::warn!("preset「{}」編不起來：{err}", e.name);
                    let n = self.queue.len();
                    self.qpos = (self.qpos + 1) % n;
                }
            }
        }
    }

    /// 算一幀。`out` 會被整個蓋掉。
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        out: &Offscreen,
        spec: &audio::Shared,
        dt: f64,
    ) -> bool {
        self.take_load();
        self.ensure_milk(gpu, out.width, out.height);

        let (wave, alive) = match spec.lock() {
            Ok(s) => {
                self.sound.update(&s);
                (s.wave, s.alive)
            }
            Err(_) => ([0.0f32; audio::WAVE], false),
        };
        // 沒訊號時給一點底噪，畫面才不會完全靜止 —— 原版也是這樣處理的
        let _ = alive;

        self.since_switch += dt;
        if self.since_switch > DWELL && !self.locked {
            self.step(1);
            self.ensure_milk(gpu, out.width, out.height);
        }

        if self.milk.is_none() {
            return false;
        }

        // 中繼畫布跟著視窗大小走
        let stale = self
            .frame_tex
            .as_ref()
            .is_none_or(|t| t.width != out.width || t.height != out.height);
        if stale {
            self.frame_tex = Some(Offscreen::new(&gpu.device, out.width, out.height));
            // 尺寸換了就立刻補畫一格，不然會先貼出一張空白
            self.accum = f64::MAX;
        }
        if self.blit.is_none() {
            self.blit = Some(crate::gfx::Blit::new(gpu, crate::gfx::OFFSCREEN_FORMAT));
        }
        // 溶解中的話，舊的那顆也要有自己的畫布
        if self.prev.is_some() {
            let stale = self
                .prev_tex
                .as_ref()
                .is_none_or(|t| t.width != out.width || t.height != out.height);
            if stale {
                self.prev_tex = Some(Offscreen::new(&gpu.device, out.width, out.height));
            }
            if self.blend_pipe.is_none() {
                self.blend_pipe = Some(BlendPipe::new(gpu));
            }
        }

        let cap_dt = if self.cap > 0.0 { 1.0 / self.cap } else { 0.0 };
        self.accum += dt;
        if cap_dt <= 0.0 || self.accum >= cap_dt {
            // **推進的時間用固定步長**，不是真實 dt —— 限幀的目的就是
            // 讓 preset 的速度跟螢幕更新率脫鉤。
            let step = if cap_dt > 0.0 { cap_dt } else { dt };
            self.accum = if cap_dt > 0.0 { (self.accum - cap_dt).min(cap_dt) } else { 0.0 };
            let ft = self.frame_tex.as_ref().unwrap();
            self.milk.as_mut().unwrap().render(gpu, enc, ft, &self.sound, &wave, step);
            if let (Some(p), Some(pt)) = (self.prev.as_mut(), self.prev_tex.as_ref()) {
                p.render(gpu, enc, pt, &self.sound, &wave, step);
            }
        }

        // 溶解的進度走真實時間，跟限幀無關 —— 不然低幀率時會變慢動作
        if self.prev.is_some() {
            self.blend = (self.blend + (dt / BLEND_S) as f32).min(1.0);
            if self.blend >= 1.0 {
                self.prev = None;
                self.prev_tex = None;
            }
        }

        let ft = self.frame_tex.as_ref().unwrap();
        let blit = self.blit.as_ref().unwrap();
        let mut pass = crate::gfx::pass(enc, &out.view, "viz-blit", None);
        match (self.prev_tex.as_ref(), self.blend_pipe.as_ref()) {
            // 先把舊的整張貼上去，新的再照圖樣溶上來
            (Some(pt), Some(bp)) => {
                blit.draw(gpu, &mut pass, &pt.view);
                let (w, h) = (out.width as f32, out.height as f32);
                let (ax, ay) = if w > h { (1.0, h / w) } else { (w / h, 1.0) };
                gpu.queue.write_buffer(
                    &bp.uni,
                    0,
                    bytemuck::cast_slice(&[
                        self.pattern[0],
                        self.pattern[1],
                        self.pattern[2],
                        self.pattern[3],
                        self.blend,
                        ax,
                        ay,
                        0.0,
                    ]),
                );
                let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &bp.tex_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&ft.view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&bp.sampler),
                        },
                    ],
                });
                pass.set_pipeline(&bp.pipeline);
                pass.set_bind_group(0, &bp.uni_bind, &[]);
                pass.set_bind_group(1, &bind, &[]);
                pass.draw(0..3, 0..1);
            }
            _ => blit.draw(gpu, &mut pass, &ft.view),
        }
        true
    }
}
