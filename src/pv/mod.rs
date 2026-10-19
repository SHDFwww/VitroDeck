//! 文字 PV（實驗中）：歌詞頁的全螢幕演出模式。
//!
//! 每句歌詞照 [`layout::compose`] 構圖，字在自己被唱到的時候彈進來，換句時整句飛散退場；
//! 節拍（[`beat::Pulse`]，loopback 抓的）讓鏡頭跟著鼓點縮放（不抖；鏡頭震只留給置中大字那句的第一下）。
//! 一首歌照段落換演出風格（[`style`]）：配色、構圖偏好、進退場、裝飾都跟著換，換段時一道斜向刷過去。
//! 字形畫法是 SDF（[`sdf`]），所以字可以任意放大、旋轉、傾斜、描邊、發光。
//!
//! **畫面是時間的純函數**：同一個歌詞時間、同一組節拍資訊畫出來一定一樣，
//! 所以 `--shot pv` 可以逐幀輸出、捲回去重播也不會亂。要記的狀態只有構圖快取。

pub mod beat;
pub mod layout;
pub mod sdf;
pub mod shot;
pub mod style;

use std::collections::HashMap;
use std::f32::consts::PI;
use std::time::Instant;

use crate::gfx::{self, Gpu, Offscreen};
use crate::lyrics::Lyrics;
use crate::text::Text;
use layout::{Deco, Enter, Exit, GIn, Layout, Placed, Rng, Template};
use style::{Look, Segment, Style};

/// 字的粗細。正黑體只有 300／400／700，要更粗沒有
const WEIGHT: u16 = 700;
/// 字比唱到早多少出現（進場動畫的前段要在唱之前跑完一點）
const LEAD: i64 = 70;
/// 退場多長、每個字錯開多少
const EXIT_MS: f32 = 430.0;
const EXIT_STAGGER: f32 = 16.0;
/// 唱完多久沒有下一句就先退場
const LINGER: i64 = 2600;
/// CJK 字身框中心在基線上方多少 em
const EM_MID: f32 = 0.38;
/// 換段的斜向轉場：在新段第一句開始前多久開刷、刷多久、新舊底色之間的漸層多寬（畫面寬的幾倍）
const WIPE_LEAD: i64 = 700;
const WIPE_MS: f32 = 1300.0;
const WIPE_SOFT: f32 = 0.6;
/// 演出層最底下先把歌詞頁的背景再壓暗多少。放在演出層裡（不是歌詞頁那邊另外蓋），
/// 進出的時候才會跟著遮罩一起漫過來
const DIM: f32 = 0.42;
/// 進出一般歌詞頁：遮罩邊界前後的漸變各多寬（畫面寬的幾倍）、字一開始放大多少
const MIX_SOFT: f32 = 0.3;
const MIX_ZOOM: f32 = 0.07;
/// 進出一般歌詞頁走多久（秒）
pub const MIX_S: f32 = 1.2;
/// 切歌之後至少在一般歌詞頁待多久（秒）才換回 PV：退場要走完，也讓人看到換了哪首
pub const HOLD_S: f32 = 2.0;
/// 前奏、間奏的流光：字跟字之間空多久（毫秒）才出來；沒字之後多久淡進、下一句前多久開始淡出
const IDLE_GAP: f32 = 4000.0;
const IDLE_IN: f32 = 1500.0;
const IDLE_OUT: f32 = 1800.0;
/// 關鍵字被唱到後：光粒散開多久、光澤掃過去多久（毫秒）
const SPARK_MS: f32 = 600.0;
const SHINE_MS: f32 = 520.0;
/// 倒退鍵刪一個字最多多久（毫秒）
const ERASE_STEP: f32 = 32.0;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Inst {
    pos: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
    line: [f32; 4],
    xf: [f32; 4],
    fx: [f32; 4],
    /// 失焦、光澤位置、光澤強度、由下往上露出的進度（見 pv.wgsl）
    ex: [f32; 4],
}

impl Inst {
    fn rect(cx: f32, cy: f32, w: f32, h: f32, rot: f32, color: [f32; 4]) -> Self {
        Self {
            pos: [cx, cy, w * 0.5, h * 0.5],
            uv: [0.0; 4],
            color,
            line: [0.0; 4],
            xf: [rot, 0.0, 0.0, 1.0],
            fx: [0.0, 0.0, 1.0, 0.0],
            ex: [0.0; 4],
        }
    }
    fn ring(cx: f32, cy: f32, r: f32, width: f32, color: [f32; 4]) -> Self {
        Self {
            pos: [cx, cy, r, r],
            uv: [0.0; 4],
            color,
            line: [0.0, 0.0, 0.0, width],
            xf: [0.0, 0.0, 0.0, 2.0],
            fx: [0.0, 0.0, 1.0, 0.0],
            ex: [0.0; 4],
        }
    }
    fn disk(cx: f32, cy: f32, r: f32, color: [f32; 4], glow: (f32, f32)) -> Self {
        Self {
            pos: [cx, cy, r, r],
            uv: [0.0; 4],
            color,
            line: [0.0; 4],
            xf: [0.0, 0.0, 0.0, 3.0],
            fx: [glow.0, glow.1, 1.0, 0.0],
            ex: [0.0; 4],
        }
    }
    /// 邊長 `size` 的漸層正方形：左邊 `left`、右邊 `right`，中間 ±`ramp` 那一段平滑過渡
    fn grad(cx: f32, cy: f32, size: f32, rot: f32, left: [f32; 4], right: [f32; 4], ramp: f32) -> Self {
        Self {
            pos: [cx, cy, size * 0.5, size * 0.5],
            uv: right,
            color: left,
            line: [0.0, 0.0, 0.0, ramp],
            xf: [rot, 0.0, 0.0, 4.0],
            fx: [0.0, 0.0, 1.0, 0.0],
            ex: [0.0; 4],
        }
    }
    /// 柔光條：橫切面高斯、兩頭淡掉。`tail` 是 0 就對稱，大於 0 是頭在 +x 的彗星（越大尾巴越短）
    fn bar(cx: f32, cy: f32, len: f32, thick: f32, rot: f32, color: [f32; 4], tail: f32) -> Self {
        Self {
            pos: [cx, cy, len * 0.5, thick * 0.5],
            uv: [0.0; 4],
            color,
            line: [tail, 0.0, 0.0, 0.0],
            xf: [rot, 0.0, 0.0, 5.0],
            fx: [0.0, 0.0, 1.0, 0.0],
            ex: [0.0; 4],
        }
    }
    /// 鋪滿畫面的暗角（形狀 6）或底片顆粒（形狀 7）。`seed` 換一組顆粒
    fn screen(w: f32, h: f32, shape: f32, color: [f32; 4], seed: f32) -> Self {
        Self {
            pos: [w * 0.5, h * 0.5, w * 0.5 + 20.0, h * 0.5 + 20.0],
            uv: [0.0; 4],
            color,
            line: [0.0, seed, 0.0, 0.0],
            xf: [0.0, 0.0, 0.0, shape],
            fx: [0.0, 0.0, 1.0, 1.0],
            ex: [0.0; 4],
        }
    }
    /// 一片圓形的網點（形狀 8）：中心的點最大、往外縮到沒有。`spacing` 是點距，
    /// `fill` 是中心的點佔格子幾成，`shift` 讓點陣慢慢平移（邏輯 px）
    fn dots(cx: f32, cy: f32, r: f32, spacing: f32, fill: f32, shift: f32, color: [f32; 4]) -> Self {
        Self {
            pos: [cx, cy, r, r],
            uv: [0.0; 4],
            color,
            line: [spacing, fill, shift, 0.0],
            xf: [0.0, 0.0, 0.0, 8.0],
            fx: [0.0, 0.0, 1.0, 1.0],
            ex: [0.0; 4],
        }
    }
    fn blur(mut self, px: f32) -> Self {
        self.ex[0] = px;
        self
    }
    /// 不跟鏡頭動（墊底、轉場、閃光）
    fn fixed(mut self) -> Self {
        self.fx[3] = 1.0;
        self
    }
}

/// 一句的快取：構圖要排版、量字，換句才算一次
struct Line {
    g: Vec<GIn>,
    lay: Layout,
    st: Style,
    /// 這句開始的時間（歌詞時間軸）與下一句開始的時間
    t0: i64,
    next: i64,
}

/// 畫一幀要的東西
pub struct Input<'a> {
    pub lyrics: &'a Lyrics,
    /// 曲目的識別（構圖的亂數種子、快取的鍵）
    pub track: &'a str,
    /// 歌詞時間（已加補償）
    pub ms: i64,
    /// 節拍：`now` 是這一幀的時刻，跟 `pulse` 裡的時刻同一個時鐘
    pub pulse: beat::Pulse,
    pub now: Instant,
    pub accent: [f32; 4],
    /// 整層的透明度（進出歌詞頁的轉場）
    pub alpha: f32,
    /// 左邊被控制項佔掉多寬（邏輯 px，已乘上控制項的淡入程度）：鏡頭往右讓、縮小一點
    pub inset: f32,
    /// 使用者鎖定的風格，`None` 是照段落自動換
    pub look: Option<Look>,
    /// 從一般歌詞頁換過來的進度（0..1，已經補間過）：整層從右邊柔柔地漫過來，字從稍大縮回原位。
    /// 退回一般歌詞頁時倒著走
    pub mix: f32,
}

pub struct Pv {
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    cam: wgpu::Buffer,
    instances: wgpu::Buffer,
    capacity: u64,
    atlas: sdf::Atlas,
    /// 構圖快取的鍵（曲目＋歌詞來源＋句數＋畫面大小），變了整個清掉
    cache_for: String,
    lines: HashMap<usize, Line>,
    segs: Vec<Segment>,
}

impl Pv {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let device = &gpu.device;
        let atlas = sdf::Atlas::new(gpu);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pv"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let cam = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pv cam"),
            size: 48,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pv sdf"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pv"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: cam.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("../pv.wgsl"));
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let attrs: Vec<wgpu::VertexAttribute> = (0..7)
            .map(|i| wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: i * 16, shader_location: i as u32 })
            .collect();
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pv"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Inst>() as u64,
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
        let capacity = 1024u64;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pv instances"),
            size: capacity * std::mem::size_of::<Inst>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, bind, cam, instances, capacity, atlas, cache_for: String::new(), lines: HashMap::new(), segs: Vec::new() }
    }

    /// 這一句的構圖，沒算過就算
    fn line(&mut self, text: &mut Text, ly: &Lyrics, i: usize, seed: u64, w: f32, h: f32) -> Option<&Line> {
        if let std::collections::hash_map::Entry::Vacant(slot) = self.lines.entry(i) {
            let l = ly.synced.get(i)?;
            if l.text.trim().is_empty() {
                return None;
            }
            let next = ly.synced.get(i + 1).map_or(l.t + 5000, |n| n.t);
            let g = glyphs(text, l, next);
            if g.iter().all(|x| x.key.is_none()) {
                return None;
            }
            let st = Style::of(self.segs.get(style::segment_of(&self.segs, i)).map_or(Look::Neon, |s| s.look));
            let lay = layout::compose(&g, seed, i, w, h, &st);
            slot.insert(Line { g, lay, st, t0: l.t, next });
        }
        self.lines.get(&i)
    }

    /// 畫進 `target`（疊在已經畫好的背景上）
    pub fn render(
        &mut self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        target: &Offscreen,
        text: &mut Text,
        inp: &Input<'_>,
    ) {
        let (w, h) = gfx::logical(target.width, target.height);
        let ly = inp.lyrics;
        let key = format!("{}|{}|{}|{}|{}x{}|{:?}", inp.track, ly.source, ly.provider, ly.synced.len(), w as u32, h as u32, inp.look);
        let seed = hash(inp.track);
        if key != self.cache_for {
            self.cache_for = key;
            self.lines.clear();
            self.segs = style::segments(ly, seed, inp.look);
        }
        self.atlas.begin();
        let ms = inp.ms;

        // 正在唱的那句與前兩句（可能還在退場）
        let cur = crate::scene_now::current_line(ly, ms);
        let mut show: Vec<usize> = Vec::new();
        if let Some(c) = cur {
            for i in c.saturating_sub(2)..=c {
                show.push(i);
            }
        }
        // 構圖只留附近幾句
        self.lines.retain(|k, _| show.contains(k) || cur.is_some_and(|c| *k == c + 1));
        for &i in &show {
            self.line(text, ly, i, seed, w, h);
        }
        // 下一句先排好：換句那一幀才排版會頓一下
        if let Some(c) = cur {
            self.line(text, ly, c + 1, seed, w, h);
        }

        let beat = Beat::at(&inp.pulse, inp.now);
        let accent = bright(inp.accent);
        let k = gfx::scale();
        // 現在這段的風格（鏡頭、墊底、全畫面的裝飾照它）；第一句之前算第一段。
        // 下一段的轉場已經開始刷了，就提早算新的那段
        let mut si = cur.map_or(0, |c| style::segment_of(&self.segs, c)).min(self.segs.len().saturating_sub(1));
        if self.segs.get(si + 1).is_some_and(|n| ms >= n.t - WIPE_LEAD) {
            si += 1;
        }
        let st = Style::of(self.segs.get(si).map_or(Look::Neon, |s| s.look));
        let mut back: Vec<Inst> = vec![Inst::rect(w * 0.5, h * 0.5, w + 40.0, h + 40.0, 0.0, [0.0, 0.0, 0.0, DIM]).fixed()];
        backdrop(&mut back, &self.segs, si, ms, w, h);
        let mut deco: Vec<Inst> = Vec::new();
        let mut body: Vec<Inst> = Vec::new();
        let mut shake = (0.0f32, 0.0f32);
        let mut flash = 0.0f32;
        let mut zoom_in = 0.0f32;
        let mut tilt = 0.0f32;
        // 畫面上最後一個字什麼時候退完（流光要等它）
        let mut last_end = f32::NEG_INFINITY;

        for &i in &show {
            let Some(line) = self.lines.get(&i) else { continue };
            let exit_t = exit_time(line);
            let exit_end = exit_t as f32 + EXIT_MS + EXIT_STAGGER * line.lay.glyphs.len() as f32;
            if !line.lay.glyphs.is_empty() {
                last_end = last_end.max(exit_end);
            }
            if ms as f32 > exit_end {
                continue;
            }
            // 整句的退場進度（裝飾用）
            let out_p = ((ms - exit_t) as f32 / EXIT_MS).clamp(0.0, 1.0);
            let first = line.lay.glyphs.iter().map(|p| p.t).min().unwrap_or(line.t0);
            let in_p = ((ms - (first - LEAD)) as f32 / 400.0).clamp(0.0, 1.0);
            // 這句唱到一半鏡頭慢慢推近
            if ms >= line.t0 && ms < exit_t {
                zoom_in = zoom_in.max(((ms - line.t0) as f32 / (exit_t - line.t0).max(1) as f32).clamp(0.0, 1.0));
                tilt = line.lay.tilt;
            }
            // 鏡頭震只給置中大字那種構圖，一句只震第一個蓋下來的字。
            // 每個關鍵字都震、每拍也震的話，看久了很煩
            let quake = (line.lay.template == Template::Slam)
                .then(|| line.lay.glyphs.iter().enumerate().filter(|(_, p)| p.enter == Enter::Stamp).min_by_key(|(_, p)| p.t).map(|(o, _)| o))
                .flatten();
            let ls = &line.st;
            draw_decos(&mut deco, &mut self.atlas, gpu, text, line, ms, in_p, out_p, accent, &beat, w, h, k);

            for (order, p) in line.lay.glyphs.iter().enumerate() {
                let Some(st) = state(p, &line.lay, ms, exit_t, order, &beat, h, ls) else { continue };
                let gi = &line.g[p.i];
                let Some(gk) = gi.key else { continue };
                let Some(e) = self.atlas.get(gpu, gk, |kk| text.raster(kk)) else { continue };
                // 鏡頭震：蓋章落地那一下
                if p.enter == Enter::Stamp {
                    let age = ms - (p.t - LEAD + enter_ms(Enter::Stamp) as i64);
                    if quake == Some(order) && (0..280).contains(&age) {
                        let a = (1.0 - age as f32 / 280.0).powi(2) * 12.0 * ls.shake;
                        let ph = age as f32 * 0.09 + p.jitter * 7.0;
                        shake.0 += a * ph.sin();
                        shake.1 += a * (ph * 1.3).cos();
                    }
                    if p.key && (0..120).contains(&age) {
                        flash = flash.max(0.07 * ls.flash * (1.0 - age as f32 / 120.0));
                    }
                    // 落地的那一圈
                    if (0..320).contains(&age) {
                        let q = age as f32 / 320.0;
                        let col = if p.key { ls.key_ink.unwrap_or(accent) } else { ls.line };
                        deco.push(Inst::ring(st.x, st.y, p.size * (0.45 + 0.9 * ease_out(q)), 3.0 * (1.0 - q) + 0.5, with_a(col, 0.55 * (1.0 - q) * st.alpha)));
                    }
                }
                // 關鍵字被唱到：幾顆光粒往外散開（蓋章的字已經有落地那一圈，不疊）
                if p.key && ls.sparks && p.enter != Enter::Stamp && ms >= p.t && ms < exit_t {
                    let q = (ms - p.t) as f32 / SPARK_MS;
                    if q < 1.0 {
                        let col = mix(ls.key_ink.unwrap_or(accent), [1.0; 4], 0.45);
                        let sd = seed_of(p.jitter);
                        for j in 0..7u64 {
                            let ang = (j as f32 + noise(sd, j) * 0.7) / 7.0 * 2.0 * PI;
                            let dist = p.size * (0.45 + (0.5 + 0.5 * noise(sd ^ 0x5A, j)) * 0.9 * ease_out(q));
                            let r = (1.6 + 2.0 * noise(sd ^ 0x6B, j)) * (1.0 - q * 0.6);
                            let a = 0.75 * (1.0 - q).powf(1.5) * st.alpha;
                            deco.push(Inst::disk(st.x + ang.cos() * dist, st.y + ang.sin() * dist, r, with_a(col, a), (r * 2.0, 0.7)));
                        }
                    }
                }
                let (fill, line_c, line_w, fill_a) = colors(p, ls, accent, st.sing);
                let mut push = |dx: f32, dy: f32, a: f32, col: [f32; 4], lc: [f32; 3], lw: f32, fa: f32, glow: (f32, f32)| {
                    let mut g = glyph_inst(&e, gi.adv, st.x + dx, st.y + dy, p.size * st.scale, st.rot, st.skew, with_a(col, a), lc, lw, fa, glow, k);
                    g.ex = [st.blur, 0.0, 0.0, st.reveal];
                    body.push(g);
                };
                // 硬陰影：整個字（含描邊）往右下錯開一份，墊在最下面
                if let Some((sc, ox, oy)) = ls.shadow
                    && !p.on_band
                {
                    let s = p.size * st.scale;
                    push(ox * s, oy * s, st.alpha * sc[3], sc, [sc[0], sc[1], sc[2]], line_w, fill_a, (0.0, 0.0));
                }
                // 殘影（滑進來的時候）
                for &(gx, gy, ga) in &st.trail {
                    push(gx, gy, st.alpha * ga, fill, line_c, line_w, fill_a.max(0.6), (0.0, 0.0));
                }
                // 色差：鼓點重的時候關鍵字裂成紅、青兩份；故障風格一直在裂
                let mut ca = beat.hit * if p.key { 1.0 } else { 0.25 } * ls.chroma;
                if ls.look == Look::Glitch {
                    ca = ca.max(0.12 + 0.5 * noise(seed_of(p.jitter), (ms / 90) as u64).powi(3));
                }
                if ca > 0.04 {
                    let off = 2.0 + 6.0 * ca;
                    push(-off, 0.0, st.alpha * 0.55 * ca, [1.0, 0.18, 0.35, 1.0], [1.0, 0.18, 0.35], line_w, fill_a.max(0.8), (0.0, 0.0));
                    push(off, 0.0, st.alpha * 0.55 * ca, [0.15, 0.9, 1.0, 1.0], [0.15, 0.9, 1.0], line_w, fill_a.max(0.8), (0.0, 0.0));
                }
                let gl = if ls.tube {
                    // 燈管一直亮著，唱到的那個更亮
                    0.55 + 0.35 * st.sing
                } else if ls.look == Look::Drift {
                    (ls.glow * st.sing).max(0.22)
                } else {
                    ls.glow * st.sing
                };
                let glow = if gl > 0.0 { (p.size * if ls.tube { 0.09 } else { 0.06 }, gl) } else { (0.0, 0.0) };
                push(0.0, 0.0, st.alpha, fill, line_c, line_w, fill_a, glow);
                // 光澤：關鍵字唱到之後，一道亮帶從左上掃到右下（只有本體，殘影與色差不掃）
                if p.key && ls.shine > 0.0 && ms >= p.t + 60 {
                    let q = (ms - p.t - 60) as f32 / SHINE_MS;
                    if q < 1.0
                        && let Some(g) = body.last_mut()
                    {
                        g.ex[1] = -1.5 + 3.0 * ease_in_out(q);
                        g.ex[2] = ls.shine * (q * PI).sin().sqrt();
                    }
                }
            }
        }

        // 鼓點：一圈從畫面中心擴散出去、閃一下
        if beat.hit > 0.02 && st.ring {
            let q = 1.0 - beat.hit;
            deco.push(Inst::ring(w * 0.5, h * 0.5, h * (0.18 + 0.55 * ease_out(q)), 2.0 + 4.0 * beat.hit, with_a(accent, 0.28 * beat.hit)));
        }
        flash = flash.max(0.05 * st.flash * beat.hit * beat.hit);
        // 前奏、間奏、尾奏沒有字的時候鋪一層流光：上一句退完才淡進，下一句的字出來前淡掉
        let next_in = ly
            .synced
            .iter()
            .skip(cur.unwrap_or(0))
            .filter(|l| !l.text.trim().is_empty())
            .map(|l| l.words.as_ref().and_then(|w| w.first()).map_or(l.t, |w| w.t.min(l.t)) - LEAD)
            .find(|&t| t > ms)
            .map_or(f32::INFINITY, |t| t as f32);
        let idle = if next_in - last_end < IDLE_GAP {
            0.0
        } else {
            let t = ms as f32;
            smooth01((t - last_end) / IDLE_IN) * smooth01((next_in - t) / IDLE_OUT)
        };
        // 風格特有、鋪滿畫面的裝飾（有句子在畫面上才出現）與流光。
        // 換段轉場中，舊段的淡出、新段的淡入，不在轉場開始那一幀整組跳過去
        let lines_on = show.iter().any(|i| self.lines.contains_key(i));
        let mut fade = |st: &Style, k: f32| {
            let from = back.len();
            scenery(&mut back, st, ms, &beat, w, h);
            if lines_on {
                ambient(&mut back, st, ms, &beat, accent, w, h);
            }
            flow(&mut back, st, ms, &beat, accent, idle, w, h);
            for x in &mut back[from..] {
                x.color[3] *= k;
            }
        };
        match wipe_at(&self.segs, si, ms, w) {
            Some(wp) => {
                fade(&Style::of(self.segs[si - 1].look), 1.0 - wp.q);
                fade(&st, wp.q);
            }
            None => fade(&st, 1.0),
        }
        let mut all = back;
        all.extend(deco);
        all.extend(body);
        // 暗角與底片顆粒蓋在最上面（字也一起），換段時一樣交叉淡化
        match wipe_at(&self.segs, si, ms, w) {
            Some(wp) => {
                screen(&mut all, &Style::of(self.segs[si - 1].look), ms, &beat, 1.0 - wp.q, w, h);
                screen(&mut all, &st, ms, &beat, wp.q, w, h);
            }
            None => screen(&mut all, &st, ms, &beat, 1.0, w, h),
        }
        if flash > 0.003 {
            all.push(Inst::rect(w * 0.5, h * 0.5, w + 40.0, h + 40.0, 0.0, [1.0, 1.0, 1.0, flash]).fixed());
        }
        wipe(&mut all, &self.segs, si, ms, accent, w, h);
        if inp.alpha < 1.0 {
            for x in &mut all {
                x.color[3] *= inp.alpha;
            }
        }

        // 鏡頭
        let room = 1.0 - (inp.inset / w).clamp(0.0, 0.6) * 0.6;
        let mix = inp.mix.clamp(0.0, 1.0);
        let zoom = (1.0 + (0.025 * beat.env + 0.04 * beat.hit) * st.pump + 0.035 * ease_in_out(zoom_in))
            * room
            * (1.0 + MIX_ZOOM * (1.0 - mix));
        let drift = ms as f32 / 1000.0;
        // 唱下去鏡頭慢慢轉正一點（斜的那句會被「扶起來」）
        // 鼓點只推鏡頭（上面的 zoom），不抖
        let cam_rot = 0.012 * (drift * 0.37).sin() - tilt * 0.25 * ease_in_out(zoom_in);
        let off = (
            inp.inset * 0.5 + shake.0 + 7.0 * (drift * 0.23).sin(),
            shake.1 + 5.0 * (drift * 0.31).cos(),
        );
        // 進出一般歌詞頁的遮罩：0 的時候邊界在畫面右邊外面，1 的時候在左邊外面
        let (edge, soft) = if mix < 1.0 {
            let soft = MIX_SOFT * w;
            ((w + soft) - mix * (w + 2.0 * soft), soft)
        } else {
            (0.0, 0.0)
        };
        let cam: [f32; 12] = [w, h, w * 0.5, h * 0.5, zoom, cam_rot, off.0, off.1, k, sdf::SPREAD, edge, soft];

        if all.len() as u64 > self.capacity {
            self.capacity = (all.len() as u64).next_power_of_two();
            self.instances = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pv instances"),
                size: self.capacity * std::mem::size_of::<Inst>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        gpu.queue.write_buffer(&self.cam, 0, bytemuck::cast_slice(&cam));
        gpu.queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&all));
        let mut pass = gfx::pass(enc, &target.view, "pv", None);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..6, 0..all.len() as u32);
    }
}

/// 這一幀的節拍強度：`env` 是推算的拍點（每拍一下、很快退掉），`hit` 是真的聽到的鼓點
#[derive(Clone, Copy, Debug, Default)]
struct Beat {
    env: f32,
    hit: f32,
}

impl Beat {
    fn at(p: &beat::Pulse, now: Instant) -> Self {
        let env = p.since_beat(now).map_or(0.0, |s| (-s / 0.11).exp() * p.conf.min(1.0));
        let hit = match p.hit_at {
            Some(t) if now >= t => p.hit * (-(now - t).as_secs_f32() / 0.13).exp(),
            _ => 0.0,
        };
        Self { env, hit: if hit < 0.01 { 0.0 } else { hit } }
    }
}

/// 一個字這一幀的樣子
struct State {
    x: f32,
    y: f32,
    scale: f32,
    rot: f32,
    skew: f32,
    alpha: f32,
    /// 正在唱它（0..1，唱完退掉）
    sing: f32,
    /// 殘影 (dx, dy, 透明度倍率)
    trail: Vec<(f32, f32, f32)>,
    /// 失焦多少（邏輯 px）：進場從模糊對焦，淡出、放大退場時又散焦
    blur: f32,
    /// 由下往上露出的進度（0 是不遮）
    reveal: f32,
}

fn enter_ms(e: Enter) -> f32 {
    match e {
        Enter::Stamp => 240.0,
        Enter::Drop => 430.0,
        Enter::Slide(_) => 360.0,
        Enter::Spin => 420.0,
        Enter::Pop => 340.0,
        Enter::Rise => 680.0,
        Enter::Flicker => 300.0,
        Enter::Type => 90.0,
        Enter::Ignite => 560.0,
        Enter::Focus => 650.0,
    }
}

/// 進場的位移與變形：(dx, dy, 放大, 旋轉, 傾斜, 透明度)，`q` 是 0..1
fn enter_at(p: &Placed, q: f32) -> (f32, f32, f32, f32, f32, f32) {
    let s = p.size;
    match p.enter {
        Enter::Pop => (0.0, 0.0, back_out(q, 2.4), (1.0 - ease_out(q)) * 0.6 * p.jitter, 0.0, (q * 4.0).min(1.0)),
        Enter::Drop => (0.0, -(1.0 - bounce_out(q)) * s * 1.5, 1.0, (1.0 - ease_out(q)) * 0.3 * p.jitter, 0.0, (q * 5.0).min(1.0)),
        Enter::Spin => (0.0, 0.0, ease_out(q), (1.0 - ease_out(q)) * PI * 0.9 * p.jitter.signum(), 0.0, (q * 3.0).min(1.0)),
        Enter::Slide(dir) => {
            let e = expo_out(q);
            (dir * (1.0 - e) * s * 3.5, 0.0, 1.0, 0.0, -dir * (1.0 - ease_out(q)) * 0.9, (q * 3.0).min(1.0))
        }
        Enter::Stamp => (0.0, 0.0, 1.0 + 2.2 * (1.0 - expo_out(q)), 0.0, 0.0, (q * 2.5).min(1.0)),
        Enter::Rise => (0.0, (1.0 - ease_out(q)) * s * 0.55, 1.0 + 0.06 * (1.0 - ease_out(q)), 0.0, 0.0, ease_in_out(q)),
        Enter::Flicker => {
            if q >= 1.0 {
                return (0.0, 0.0, 1.0, 0.0, 0.0, 1.0);
            }
            // 每 35 ms 換一次：越後面越常亮著、跳得越少
            let step = (q * 300.0 / 35.0) as u64;
            let n = noise(seed_of(p.jitter), step);
            let on = if n < 0.2 + 0.8 * q { 1.0 } else { 0.0 };
            let jump = (noise(seed_of(p.jitter) ^ 0xA5, step) - 0.5) * s * 0.5 * (1.0 - q);
            (jump, 0.0, 1.0, 0.0, 0.0, on)
        }
        // 一打上去就在，只有字輕輕壓下去再彈回原本大小
        Enter::Type => (0.0, 0.0, 1.0 + 0.1 * (1.0 - q), 0.0, 0.0, if q > 0.0 { 1.0 } else { 0.0 }),
        Enter::Ignite => {
            if q >= 1.0 {
                return (0.0, 0.0, 1.0, 0.0, 0.0, 1.0);
            }
            // 每 45 ms 換一次：先是暗暗的閃，越後面越常亮著；暗的時候也留一點（燈管不會全黑）
            let step = (q * 560.0 / 45.0) as u64;
            let on = noise(seed_of(p.jitter) ^ 0x1D, step) < 0.3 + 0.7 * q;
            (0.0, 0.0, 1.0, 0.0, 0.0, if on { 0.35 + 0.65 * q.sqrt() } else { 0.15 })
        }
        Enter::Focus => (0.0, 0.0, 1.0 + 0.05 * (1.0 - ease_out(q)), 0.0, 0.0, ease_out(q)),
    }
}

/// 這句什麼時候開始退場：唱完待一陣子，或下一句來了。
/// 倒退鍵要在下一句打第一個字之前刪完，不然新舊兩句會疊在一起
fn exit_time(line: &Line) -> i64 {
    let t = line.next.min(line.lay.sung_end + LINGER);
    if line.lay.exit != Exit::Erase {
        return t;
    }
    let total = erase_at(line.lay.glyphs.len(), 0, 0) as i64 + 40;
    t.min(line.next - LEAD - total).max(line.lay.sung_end)
}

/// 倒退鍵刪到第 `order` 個字的時間：從最後一個字往回刪，整句刪完不超過一般退場的長度
fn erase_at(n: usize, order: usize, exit_t: i64) -> f32 {
    let step = (EXIT_MS / n.max(1) as f32).min(ERASE_STEP);
    exit_t as f32 + n.saturating_sub(order + 1) as f32 * step
}

#[allow(clippy::too_many_arguments)]
fn state(p: &Placed, lay: &Layout, ms: i64, exit_t: i64, order: usize, beat: &Beat, h: f32, st: &Style) -> Option<State> {
    let start = p.t - LEAD;
    if ms < start {
        return None;
    }
    let dur = enter_ms(p.enter);
    let te = (ms - start) as f32;
    let q = (te / dur).clamp(0.0, 1.0);
    let (mut dx, mut dy, mut scale, mut rot, skew, mut alpha) = enter_at(p, q);
    // 進場從失焦對焦：蓋章那種放大砸下來的收得快一點；閃現的不糊（那是故障風格的一閃一閃）
    let mut blur = p.size
        * match p.enter {
            Enter::Flicker | Enter::Type | Enter::Ignite => 0.0,
            Enter::Stamp => (1.0 - expo_out(q)) * 0.09,
            Enter::Focus => (1.0 - ease_out(q)) * 0.3,
            _ => (1.0 - ease_out(q)) * 0.12,
        };
    // 浮上來的字像從一道縫裡升起來
    let reveal = if p.enter == Enter::Rise && q < 1.0 { ease_out(q).max(0.001) } else { 0.0 };

    // 殘影：滑進來的時候往回取三個時間點
    let mut trail = Vec::new();
    if matches!(p.enter, Enter::Slide(_)) && q < 1.0 {
        for (j, a) in [(1, 0.35), (2, 0.2), (3, 0.1)] {
            let qq = ((te - 28.0 * j as f32) / dur).clamp(0.0, 1.0);
            if qq > 0.0 {
                let (tx, ty, ..) = enter_at(p, qq);
                trail.push((tx - dx, ty - dy, a));
            }
        }
    }

    // 唱到它：放大一點、發光；拖長的音慢慢再大一點
    let sing = if ms >= p.t && ms < p.t + p.d.max(1) {
        1.0
    } else if ms >= p.t + p.d.max(1) {
        (1.0 - (ms - p.t - p.d.max(1)) as f32 / 260.0).max(0.0)
    } else {
        0.0
    };
    scale *= 1.0 + 0.07 * sing;
    if p.d > 700 && ms >= p.t {
        scale *= 1.0 + 0.1 * ((ms - p.t) as f32 / p.d as f32).min(1.0);
    }
    // 跟拍：關鍵字跳一下，其他字輕輕浮
    if p.key {
        scale *= 1.0 + (0.07 * beat.env + 0.06 * beat.hit) * st.pump;
    }
    dy += (ms as f32 / 720.0 + p.jitter * 3.0).sin() * p.size * st.float;
    // 故障：偶爾整個字往旁邊錯開一下
    if st.look == Look::Glitch && ms >= p.t {
        let step = (ms / 80) as u64;
        if noise(seed_of(p.jitter) ^ 0x3C, step) < 0.06 + 0.25 * beat.hit {
            dx += (noise(seed_of(p.jitter) ^ 0x7E, step) - 0.5) * p.size * 0.35;
        }
    }

    // 退場
    if ms >= exit_t {
        let tx = exit_t as f32 + order as f32 * EXIT_STAGGER;
        let r = ((ms as f32 - tx) / EXIT_MS).clamp(0.0, 1.0);
        if r >= 1.0 {
            return None;
        }
        let r2 = r * r;
        match lay.exit {
            Exit::Scatter => {
                let (vx, vy) = (p.x - lay.cx, p.y - lay.cy);
                let len = (vx * vx + vy * vy).sqrt().max(1.0);
                let a = p.jitter * 0.7;
                let (ux, uy) = (vx / len * a.cos() - vy / len * a.sin(), vx / len * a.sin() + vy / len * a.cos());
                dx += ux * r2 * p.size * 4.0;
                dy += uy * r2 * p.size * 4.0;
                rot += p.jitter * 2.6 * r;
                scale *= 1.0 - 0.3 * r;
                alpha *= 1.0 - r;
                blur += r * 0.05 * p.size;
            }
            Exit::Fall => {
                dy += r2 * h * 0.45;
                rot += p.jitter * 1.3 * r;
                alpha *= 1.0 - r.powf(1.5);
                blur += r * 0.05 * p.size;
            }
            Exit::Zoom => {
                scale *= 1.0 + 1.7 * r2;
                dx += (p.x - lay.cx) * 0.7 * r2;
                dy += (p.y - lay.cy) * 0.7 * r2;
                alpha *= (1.0 - r) * (1.0 - r);
                blur += r2 * 0.12 * p.size;
            }
            Exit::Fade => {
                dy -= ease_out(r) * p.size * 0.5;
                alpha *= 1.0 - ease_in_out(r);
                blur += ease_in_out(r) * 0.12 * p.size;
            }
            Exit::Blink => {
                let step = ((ms as f32 - tx) / 40.0).max(0.0) as u64;
                if noise(seed_of(p.jitter) ^ 0x51, step) < r * 1.3 {
                    alpha = 0.0;
                }
                dx += (noise(seed_of(p.jitter) ^ 0x19, step) - 0.5) * p.size * 0.6 * r;
            }
            Exit::Erase => {
                if ms as f32 >= erase_at(lay.glyphs.len(), order, exit_t) {
                    return None;
                }
            }
            Exit::Off => {
                // 暗下去的途中閃一兩下，像燈管斷電
                let step = ((ms as f32 - tx) / 50.0).max(0.0) as u64;
                let dip = if noise(seed_of(p.jitter) ^ 0x2F, step) < 0.3 { 0.35 } else { 1.0 };
                alpha *= (1.0 - r) * (1.0 - r) * dip;
            }
        }
    }
    Some(State { x: p.x + dx, y: p.y + dy, scale, rot: p.rot + rot, skew, alpha, sing, trail, blur, reveal })
}

/// (填色, 描邊色, 描邊寬, 填色倍率)
fn colors(p: &Placed, st: &Style, accent: [f32; 4], sing: f32) -> ([f32; 4], [f32; 3], f32, f32) {
    let ink = st.ink;
    let (sc, sw) = match st.stroke {
        Some((c, _)) => (c, st.stroke_w(p.size)),
        None => ([0.0; 3], 0.0),
    };
    if p.on_band {
        return (st.band_ink, [0.0; 3], 0.0, 1.0);
    }
    if st.tube {
        // 燈管：只畫輪廓，管子本身偏白、外面的光暈才是顏色（光暈拿填色那格的顏色）
        let c = if p.key { st.key_ink.unwrap_or(accent) } else { ink };
        let core = mix(c, [1.0; 4], 0.55);
        // 字裡面透一點同色的光，不然中間全黑，看起來像黑字描邊
        return (c, [core[0], core[1], core[2]], (p.size * 0.04).max(1.5), 0.1 + 0.1 * sing);
    }
    if p.hollow {
        // 空心字唱到的時候填起來
        let lw = (p.size * 0.035).max(1.5);
        return (ink, [ink[0], ink[1], ink[2]], lw, 0.85 * sing);
    }
    if p.key {
        let c = st.key_ink.unwrap_or(accent);
        // 霓虹那種會發光的，唱到時往白色亮一點
        let c = if st.look == Look::Neon { mix(c, [0.97, 0.97, 0.98, 1.0], 0.25 * sing) } else { c };
        return (c, sc, sw, 1.0);
    }
    (ink, sc, sw, 1.0)
}

#[allow(clippy::too_many_arguments)]
fn glyph_inst(
    e: &sdf::Entry,
    adv: f32,
    cx: f32,
    cy: f32,
    size: f32,
    rot: f32,
    skew: f32,
    color: [f32; 4],
    line_c: [f32; 3],
    line_w: f32,
    fill_a: f32,
    glow: (f32, f32),
    k: f32,
) -> Inst {
    let s = size / sdf::BASE;
    // 點陣圖中心相對於字身框中心的偏移（BASE px、y 往下）
    let ox = e.left + e.w * 0.5 - adv * sdf::BASE * 0.5;
    let oy = -e.top + e.h * 0.5 + EM_MID * sdf::BASE;
    let (sn, cs) = rot.sin_cos();
    let (rx, ry) = ((ox * cs - oy * sn) * s, (ox * sn + oy * cs) * s);
    Inst {
        pos: [cx + rx, cy + ry, e.w * s * 0.5, e.h * s * 0.5],
        uv: e.uv,
        color,
        line: [line_c[0], line_c[1], line_c[2], line_w],
        xf: [rot, skew, s * k, 0.0],
        fx: [glow.0, glow.1, fill_a, 0.0],
        ex: [0.0; 4],
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_decos(
    out: &mut Vec<Inst>,
    atlas: &mut sdf::Atlas,
    gpu: &Gpu,
    text: &mut Text,
    line: &Line,
    ms: i64,
    in_p: f32,
    out_p: f32,
    accent: [f32; 4],
    beat: &Beat,
    w: f32,
    h: f32,
    k: f32,
) {
    let fade = ease_out(in_p) * (1.0 - out_p);
    if fade <= 0.0 {
        return;
    }
    for d in &line.lay.decos {
        match *d {
            Deco::Ghost { i, cx, cy, size, rot } => {
                let gi = &line.g[i];
                let Some(gk) = gi.key else { continue };
                let Some(e) = atlas.get(gpu, gk, |kk| text.raster(kk)) else { continue };
                let t = ms as f32 / 1000.0;
                let s = size * (1.0 + 0.03 * (t * 0.6).sin() + 0.04 * beat.env) * (1.0 + 0.25 * out_p);
                let a = fade * (0.07 + 0.05 * beat.env);
                let c = line.st.ink;
                // 景深：墊在後面的大字對不到焦
                let g = glyph_inst(&e, gi.adv, cx, cy, s, rot + t * 0.02, 0.0, with_a(c, a), [c[0], c[1], c[2]], 2.5, 0.25, (0.0, 0.0), k);
                out.push(g.blur(s * 0.035));
            }
            Deco::Band { cx, cy, w: bw, h: bh, rot, t } => {
                let p = ease_out(((ms - t) as f32 / 220.0).clamp(0.0, 1.0));
                if p <= 0.0 {
                    continue;
                }
                // 從左端刷出來、退場時往右端收掉
                let (lo, hi) = (if out_p > 0.0 { ease_in_out(out_p) } else { 0.0 }, p);
                let len = (hi - lo) * bw;
                if len <= 0.5 {
                    continue;
                }
                let mid = (lo + hi) * 0.5 - 0.5;
                let (sn, cs) = rot.sin_cos();
                let (ox, oy) = (mid * bw * cs, mid * bw * sn);
                let a = 0.92 * (1.0 - 0.2 * out_p);
                out.push(Inst::rect(cx + ox, cy + oy, len, bh * (1.0 + 0.05 * beat.env), rot, with_a(line.st.band.unwrap_or(accent), a)));
            }
            Deco::Corners { cx, cy, w: cw, h: ch, rot, t } => {
                let p = ((ms - t) as f32 / 260.0).clamp(0.0, 1.0);
                if p <= 0.0 {
                    continue;
                }
                let grow = 1.0 + 0.25 * (1.0 - ease_out(p)) + 0.03 * beat.env;
                let (hw, hh) = (cw * 0.5 * grow, ch * 0.5 * grow);
                let arm = (cw.min(ch) * 0.14).clamp(14.0, 60.0);
                let (sn, cs) = rot.sin_cos();
                let at = |x: f32, y: f32| (cx + x * cs - y * sn, cy + x * sn + y * cs);
                let col = with_a(line.st.line, 0.75 * fade * ease_out(p));
                for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                    let (ax, ay) = at(sx * (hw - arm * 0.5), sy * hh);
                    out.push(Inst::rect(ax, ay, arm, 3.0, rot, col));
                    let (bx, by) = at(sx * hw, sy * (hh - arm * 0.5));
                    out.push(Inst::rect(bx, by, 3.0, arm, rot, col));
                }
            }
        }
    }
    // 紙本：最後一個字唱完時，在句尾蓋一枚方的朱印（刻著這句的關鍵字），像書法的落款。
    // 以前是一顆大紅圓擺在離字最遠的空位，跟字對不起來，看起來很突兀
    if line.st.seal {
        seal(out, atlas, gpu, text, line, ms, fade, accent, w, h, k);
    }
    if line.st.cursor {
        cursor(out, line, ms, in_p);
    }
}

/// 打字游標：停在最後一個打上去的字後面；打字中一直亮著，停下來才慢慢明暗（不是硬閃）。
/// 倒退鍵刪字的時候跟著退回去，整句刪完就不見
fn cursor(out: &mut Vec<Inst>, line: &Line, ms: i64, in_p: f32) {
    let gs = &line.lay.glyphs;
    let exit_t = exit_time(line);
    // 下一句開始打了，游標就交給它（畫面上只有一個游標）
    if ms >= line.next - LEAD {
        return;
    }
    let n = gs.len();
    let shown = |(o, p): &(usize, &Placed)| {
        ms >= p.t - LEAD && !(line.lay.exit == Exit::Erase && ms >= exit_t && ms as f32 >= erase_at(n, *o, exit_t))
    };
    let last = gs.iter().enumerate().filter(shown).max_by_key(|(o, p)| (p.t, *o)).map(|(_, p)| p);
    let (x, y, size, rot, typed_at) = match last {
        Some(p) => {
            let adv = line.g[p.i].adv * p.size;
            let (sn, cs) = p.rot.sin_cos();
            let off = adv * 0.5 + p.size * 0.2;
            (p.x + cs * off, p.y + sn * off, p.size, p.rot, Some(p.t))
        }
        // 還沒打第一個字：停在第一個字前面等
        None if ms < exit_t => match gs.iter().min_by_key(|p| p.t) {
            Some(p) => {
                let adv = line.g[p.i].adv * p.size;
                let (sn, cs) = p.rot.sin_cos();
                let off = -adv * 0.5;
                (p.x + cs * off, p.y + sn * off, p.size, p.rot, None)
            }
            None => return,
        },
        None => return,
    };
    // 還沒打字就當作停很久了（照時間慢慢明暗）
    let idle = typed_at.map_or(1000.0 + ms as f32, |t| (ms - t) as f32);
    let blink = if idle < 450.0 { 1.0 } else { 0.55 + 0.45 * ((idle - 450.0) / 1060.0 * 2.0 * PI).cos() };
    let a = ease_out(in_p) * blink * 0.9;
    if a > 0.003 {
        out.push(Inst::rect(x, y, (size * 0.07).max(2.0), size * 0.92, rot, with_a(line.st.line, a)));
    }
}

/// 句尾的朱印：直書蓋在最後一個字下面、橫書蓋在右邊；出了畫面就改蓋在旁邊
#[allow(clippy::too_many_arguments)]
fn seal(
    out: &mut Vec<Inst>,
    atlas: &mut sdf::Atlas,
    gpu: &Gpu,
    text: &mut Text,
    line: &Line,
    ms: i64,
    fade: f32,
    accent: [f32; 4],
    w: f32,
    h: f32,
    k: f32,
) {
    let gs = &line.lay.glyphs;
    let Some(last) = gs.iter().max_by_key(|g| g.t) else { return };
    let ts = last.t + last.d.clamp(120, 500);
    let p = expo_out(((ms - ts) as f32 / 260.0).clamp(0.0, 1.0));
    if p <= 0.0 {
        return;
    }
    let s = (last.size * 0.6).max(h * 0.055);
    let (sn, cs) = last.rot.sin_cos();
    // 書寫方向：直書往下、其他照字的角度往右
    let along = if line.lay.template == Template::Vertical { (-sn, cs) } else { (cs, sn) };
    let side = (-along.1, along.0);
    let gap = last.size * 0.5 + s * 0.5 + last.size * 0.2;
    let m = s * 0.6;
    let inside = |(x, y): (f32, f32)| x > m && x < w - m && y > m && y < h - m;
    let (x, y) = [(along.0, along.1), (side.0, side.1), (-side.0, -side.1)]
        .into_iter()
        .map(|(dx, dy)| (last.x + dx * gap, last.y + dy * gap))
        .find(|&c| inside(c))
        .unwrap_or((last.x.clamp(m, w - m), last.y.clamp(m, h - m)));
    let red = line.st.key_ink.unwrap_or(accent);
    let paper = [line.st.bg[0], line.st.bg[1], line.st.bg[2], 1.0];
    // 蓋下去：從大一點、透明壓下來
    let sc = 1.0 + 0.7 * (1.0 - p);
    let a = fade * (p * 3.0).min(1.0) * 0.9;
    let rot = last.rot - 0.06;
    let size = s * sc;
    out.push(Inst::rect(x, y, size, size, rot, with_a(red, a)));
    // 內框：紙色的細線
    let mut frame = Inst::rect(x, y, size * 0.8, size * 0.8, rot, with_a(paper, a));
    frame.fx[2] = 0.0;
    frame.line = [paper[0], paper[1], paper[2], (size * 0.035).max(1.2)];
    out.push(frame);
    // 刻的字：這句第一個關鍵字，沒有就第一個字
    let pick = gs.iter().filter(|g| g.key).min_by_key(|g| g.t).or_else(|| gs.iter().min_by_key(|g| g.t));
    if let Some(gi) = pick.map(|g| &line.g[g.i])
        && let Some(gk) = gi.key
        && let Some(e) = atlas.get(gpu, gk, |kk| text.raster(kk))
    {
        out.push(glyph_inst(&e, gi.adv, x, y, size * 0.58, rot, 0.0, with_a(paper, a), [0.0; 3], 0.0, 1.0, (0.0, 0.0), k));
    }
}

/// 墊底：這段風格的底色。換段時新的底色從左邊漸漸漫過來，新舊之間是一大段漸層
fn backdrop(out: &mut Vec<Inst>, segs: &[Segment], si: usize, ms: i64, w: f32, h: f32) {
    let bg_of = |i: usize| Style::of(segs.get(i).map_or(Look::Neon, |s| s.look)).bg;
    let cur = bg_of(si);
    match wipe_at(segs, si, ms, w) {
        Some(wp) => {
            let old = bg_of(si - 1);
            if cur[3] > 0.0 || old[3] > 0.0 {
                // 一整片轉過的大矩形，漸層的中線貼著刷到哪：左邊新的、右邊舊的。
                // 不拆成幾塊拼，拼縫會有一條細線
                let big = w.max(h) * 4.0;
                out.push(Inst::grad(wp.edge, h * 0.5, big, wp.rot, cur, old, WIPE_SOFT * w * 0.5).fixed());
            }
        }
        None if cur[3] > 0.0 => out.push(Inst::rect(w * 0.5, h * 0.5, w + 40.0, h + 40.0, 0.0, cur).fixed()),
        None => {}
    }
}

/// 換段轉場刷到哪
struct Wipe {
    /// 漸層中線在畫面中央那一列的 x
    edge: f32,
    rot: f32,
    /// 進度 0..1（已經緩動過）
    q: f32,
}

/// 換段轉場刷到哪；不在轉場中是 `None`
fn wipe_at(segs: &[Segment], si: usize, ms: i64, w: f32) -> Option<Wipe> {
    if si == 0 {
        return None;
    }
    let s = segs.get(si)?;
    let q = (ms - (s.t - WIPE_LEAD)) as f32 / WIPE_MS;
    if !(0.0..1.0).contains(&q) || segs[si - 1].look == s.look {
        return None;
    }
    // 斜的，所以兩端要多刷一點；再加上漸層的半寬，開頭結尾才整片是舊的／新的。
    // 緩動用 smoothstep：中段不衝，漸層在畫面上慢慢漫過去
    let q = q * q * (3.0 - 2.0 * q);
    let pad = 0.12 + WIPE_SOFT * 0.5;
    Some(Wipe { edge: (-pad + q * (1.0 + 2.0 * pad)) * w, rot: 0.32, q })
}

/// 轉場刷過去的那道光：細細一條強調色、往兩邊柔柔地暈開，頭尾淡進淡出
fn wipe(out: &mut Vec<Inst>, segs: &[Segment], si: usize, ms: i64, accent: [f32; 4], w: f32, h: f32) {
    let Some(wp) = wipe_at(segs, si, ms, w) else { return };
    let len = w.max(h) * 2.0;
    let a = (wp.q * PI).sin();
    let mut glow = Inst::rect(wp.edge, h * 0.5, 2.0, len, wp.rot, with_a(accent, 0.35 * a)).fixed();
    glow.fx[0] = w * 0.035;
    glow.fx[1] = 0.6;
    out.push(glow);
}

/// 流光：沒有字的時候（前奏、間奏）斜斜流過畫面的光。幾道很寬、很淡的光帶慢慢平移，
/// 上面有幾條細的光一條一條劃過去；跟拍子一起亮一下，但不閃、不動鏡頭。`k` 是整組的濃淡（0..1）
#[allow(clippy::too_many_arguments)]
fn flow(out: &mut Vec<Inst>, st: &Style, ms: i64, beat: &Beat, accent: [f32; 4], k: f32, w: f32, h: f32) {
    if k <= 0.003 {
        return;
    }
    let t = ms as f32 / 1000.0;
    // 紙本的底是亮的，白光看不見：改用朱紅、整體淡一點
    let paper = st.bg[3] > 0.5 && 0.2126 * st.bg[0] + 0.7152 * st.bg[1] + 0.0722 * st.bg[2] > 0.6;
    let (c0, c1, gain) = if paper {
        let c = st.key_ink.unwrap_or(accent);
        (c, c, 0.6)
    } else {
        (accent, mix(accent, [1.0; 4], 0.55), 1.0)
    };
    let pulse = 0.6 * beat.env + 0.4 * beat.hit;
    let rot = -0.36f32;
    let (sn, cs) = rot.sin_cos();
    let diag = (w * w + h * h).sqrt();
    // 沿著光的方向 s、垂直方向 n 放到畫面上
    let at = |s: f32, n: f32| (w * 0.5 + s * cs - n * sn, h * 0.5 + s * sn + n * cs);

    // 寬光帶：上中下各一道，慢慢上下飄
    for j in 0..3u64 {
        let n = ((j as f32 + 0.5) / 3.0 - 0.5) * h + (t * (0.05 + 0.05 * noise(j, 12)) + noise(j, 13) * 6.3).sin() * h * 0.16;
        let (x, y) = at(0.0, n);
        let a = k * gain * (0.09 + 0.05 * noise(j, 14) + 0.08 * pulse);
        out.push(Inst::bar(x, y, diag * 1.4, h * (0.22 + 0.16 * noise(j, 15)), rot, with_a(c0, a), 0.0).fixed());
    }
    // 細的流光：每條照自己的速度從左下劃到右上，每趟換一條軌道。頭尾那段淡進淡出，換趟不會跳
    for j in 0..8u64 {
        let len = w * (0.22 + 0.2 * noise(j, 21));
        let speed = w * (0.18 + 0.16 * noise(j, 22));
        let span = diag + len * 2.0;
        let run = t * speed + noise(j, 23) * span;
        let pass = (run / span).floor();
        let u = run / span - pass;
        let seed = j * 977 + pass as u64;
        let n = (noise(seed, 24) - 0.5) * h * 0.85;
        let (x, y) = at(-span * 0.5 + u * span, n);
        let life = (u * PI).sin();
        let a = k * gain * life * (0.3 + 0.15 * noise(seed, 25) + 0.35 * pulse);
        let thick = 2.5 + 3.5 * noise(seed, 26);
        // 一層光暈墊在底下，再一條亮的芯
        out.push(Inst::bar(x, y, len, thick * 8.0, rot, with_a(c0, a * 0.45), 1.6).fixed());
        out.push(Inst::bar(x, y, len, thick, rot, with_a(c1, a), 2.2).fixed());
    }
}

/// 暗角與底片顆粒。暗角跟著拍點微微鬆開一點（不閃）；顆粒大約 24 fps 換一組。`k` 是整組的濃淡
fn screen(out: &mut Vec<Inst>, st: &Style, ms: i64, beat: &Beat, k: f32, w: f32, h: f32) {
    // 電影的上下黑邊：疊在最上面，換段時跟著交叉淡化
    if st.bars > 0.0 && k > 0.003 {
        let bh = h * st.bars;
        for y in [bh * 0.5 - 10.0, h - bh * 0.5 + 10.0] {
            out.push(Inst::rect(w * 0.5, y, w + 40.0, bh + 20.0, 0.0, [0.0, 0.0, 0.0, k]).fixed());
        }
    }
    // 紙本的四角是舊紙泛黃那種褐色、普普是橘，其他是黑
    let vc = [st.vig[0], st.vig[1], st.vig[2], 1.0];
    let v = st.vignette * k * (1.0 - 0.12 * beat.env);
    if v > 0.003 {
        out.push(Inst::screen(w, h, 6.0, with_a(vc, v), 0.0));
    }
    let g = st.grain * k;
    if g > 0.003 {
        out.push(Inst::screen(w, h, 7.0, [1.0, 1.0, 1.0, g], (ms / 42) as f32));
    }
}

/// 風格自己的背景場景：不管有沒有字都在（前奏也在），換段時交叉淡化。都很慢、不閃
fn scenery(out: &mut Vec<Inst>, st: &Style, ms: i64, beat: &Beat, w: f32, h: f32) {
    let t = ms as f32 / 1000.0;
    if st.halftone {
        // 普普：右上、左下兩片網點，點陣慢慢斜著平移，拍點時點大一點
        let fill = 0.85 * (1.0 + 0.12 * beat.env);
        out.push(Inst::dots(w, 0.0, h * 0.95, 18.0, fill, t * 8.0, [1.0, 0.42, 0.15, 0.5]));
        out.push(Inst::dots(0.0, h, h * 0.75, 14.0, fill, -t * 6.0, [1.0, 0.98, 0.92, 0.55]));
    }
    if st.grid {
        // 復古：地平線以下是往鏡頭流過來的透視網格，地平線上一道粉紅的光
        let y0 = h * 0.66;
        let pink = [1.0, 0.3, 0.75, 1.0];
        let big = w.max(h) * 3.0;
        // 天空：往地平線漸漸透出粉紫
        out.push(Inst::grad(w * 0.5, y0 - h * 0.25, big, PI * 0.5, [0.3, 0.05, 0.4, 0.0], [0.6, 0.12, 0.5, 0.45], h * 0.3).fixed());
        // 地面壓暗
        out.push(Inst::rect(w * 0.5, (y0 + h) * 0.5 + 10.0, w + 40.0, h - y0 + 20.0, 0.0, [0.04, 0.0, 0.08, 0.8]).fixed());
        out.push(Inst::bar(w * 0.5, y0, w * 1.6, h * 0.05, 0.0, with_a(pink, 0.5 + 0.15 * beat.env), 0.0).fixed());
        // 橫線：越近間距越大，慢慢往下流；靠地平線的淡掉
        let n = 11;
        let phase = (t * 0.5).fract();
        for i in 0..n {
            let u = (i as f32 + phase) / n as f32;
            let y = y0 + (h - y0) * u * u;
            let a = 0.55 * u * (1.0 + 0.3 * beat.env);
            out.push(Inst::rect(w * 0.5, y, w + 40.0, 1.0 + 2.0 * u, 0.0, with_a(pink, a)).fixed());
        }
        // 直線：全部收到地平線中間那一點，頭在畫面底、往地平線淡掉
        let (vx, vy) = (w * 0.5, y0);
        for j in -9..=9 {
            let bx = vx + j as f32 * w * 0.13;
            let (dx, dy) = (bx - vx, h + 20.0 - vy);
            let len = (dx * dx + dy * dy).sqrt();
            let a = 0.5 * (1.0 + 0.3 * beat.env);
            out.push(Inst::bar(vx + dx * 0.5, vy + dy * 0.5, len, 3.0, dy.atan2(dx), with_a(pink, a), 1.2).fixed());
        }
    }
    if st.leaks {
        // 電影：畫面邊緣的暖色漏光，很慢地明暗、飄動
        for (j, (x, y, r, c)) in [
            (0.0, 0.3, 0.6, [1.0, 0.45, 0.15]),
            (1.0, 0.12, 0.45, [1.0, 0.72, 0.35]),
            (0.92, 1.0, 0.4, [0.9, 0.25, 0.15]),
        ]
        .into_iter()
        .enumerate()
        {
            let j = j as u64;
            let breathe = 0.5 + 0.5 * (t * (0.13 + 0.08 * noise(j, 61)) + noise(j, 62) * 6.3).sin();
            let r = h * r * (1.0 + 0.08 * breathe);
            let x = x * w + (t * 0.05 + noise(j, 63) * 6.3).sin() * w * 0.03;
            let a = 0.07 + 0.11 * breathe;
            out.push(Inst::disk(x, y * h, r, [c[0], c[1], c[2], a], (0.0, 0.0)).blur(r * 0.6).fixed());
        }
    }
}

/// 每種風格自己的、鋪滿畫面的動態裝飾
fn ambient(out: &mut Vec<Inst>, st: &Style, ms: i64, beat: &Beat, accent: [f32; 4], w: f32, h: f32) {
    let (cx, cy) = (w * 0.5, h * 0.5);
    if st.bokeh > 0.0 {
        // 散景：幾個很大、對不到焦的光斑在後面慢慢飄、慢慢明暗，拍點時亮一點
        let t = ms as f32 / 1000.0;
        let warm = mix(accent, [1.0; 4], 0.5);
        for j in 0..9u64 {
            let r = h * (0.035 + 0.08 * noise(j, 41));
            let x = (noise(j, 42) * 1.2 - 0.1) * w + (t * (0.03 + 0.04 * noise(j, 43)) + noise(j, 44) * 6.3).sin() * w * 0.06;
            let y = (noise(j, 45) * 1.1 - 0.05) * h + (t * (0.025 + 0.03 * noise(j, 46)) + noise(j, 47) * 6.3).cos() * h * 0.05;
            let tw = 0.5 + 0.5 * (t * (0.2 + 0.3 * noise(j, 48)) + noise(j, 49) * 6.3).sin();
            let a = st.bokeh * (0.035 + 0.05 * tw) * (1.0 + 0.6 * beat.env);
            let c = if j % 3 == 0 { warm } else { accent };
            out.push(Inst::disk(x, y, r, with_a(c, a), (0.0, 0.0)).blur(r * 0.25).fixed());
        }
    }
    if st.rays {
        // 集中線：從畫面外往中心收的細線，每 90 ms 換一組
        let step = (ms / 90) as u64;
        let n = 44u64;
        let r0 = h * (0.4 - 0.04 * beat.hit);
        let reach = (w * w + h * h).sqrt() * 0.6;
        for j in 0..n {
            let a = (j as f32 + noise(j, step) * 0.8) / n as f32 * 2.0 * PI;
            let len = reach - r0 * (0.75 + 0.5 * noise(j ^ 0x99, step));
            let mid = reach - len * 0.5;
            let wd = 1.5 + 4.0 * noise(j ^ 0x33, step);
            let al = (0.1 + 0.3 * beat.hit) * (0.4 + 0.6 * noise(j ^ 0x55, step));
            out.push(Inst::rect(cx + a.cos() * mid, cy + a.sin() * mid, len, wd, a, [1.0, 1.0, 1.0, al]).fixed());
        }
    }
    if st.slices {
        // 橫向切片：每 70 ms 換位置，鼓點重的時候多幾條
        let step = (ms / 70) as u64;
        let n = 3 + (beat.hit * 5.0) as u64;
        for j in 0..n {
            if noise(j ^ 0x11, step) < 0.35 {
                continue;
            }
            let y = noise(j ^ 0x22, step) * h;
            let bh = 2.0 + 16.0 * noise(j ^ 0x44, step).powi(2);
            let bw = w * (0.2 + 0.8 * noise(j ^ 0x66, step));
            let x = noise(j ^ 0x77, step) * w;
            let c = if j % 2 == 0 { st.key_ink.unwrap_or(accent) } else { st.band.unwrap_or(accent) };
            out.push(Inst::rect(x, y, bw, bh, 0.0, with_a(c, 0.12 + 0.3 * noise(j ^ 0x88, step))).fixed());
        }
        // 掃描線
        let mut y = 0.0;
        while y < h {
            out.push(Inst::rect(cx, y, w, 1.0, 0.0, [0.0, 0.0, 0.0, 0.18]).fixed());
            y += 4.0;
        }
    }
    if st.motes {
        // 光點：慢慢往上飄，跟拍子亮一下
        let t = ms as f32 / 1000.0;
        for j in 0..36u64 {
            let sp = 18.0 + 40.0 * noise(j, 2);
            let r = 1.2 + 3.2 * noise(j, 3).powi(2);
            let span = h + 60.0;
            let y = h + 30.0 - (t * sp + noise(j, 4) * span).rem_euclid(span);
            let x = noise(j, 1) * w + (t * 0.5 + j as f32).sin() * 24.0;
            let a = (0.18 + 0.3 * noise(j, 5)) * (0.7 + 0.5 * beat.env);
            out.push(Inst::disk(x, y, r, with_a(accent, a), (r * 2.5, 0.6)).fixed());
        }
    }
}

/// 0..1 的可重現亂數：同一組 (a, b) 一定一樣
fn noise(a: u64, b: u64) -> f32 {
    Rng::new(a.wrapping_mul(0x9E37_79B9).wrapping_add(b)).f()
}

/// 每個字的 `jitter` 轉成種子
fn seed_of(j: f32) -> u64 {
    ((j + 1.0) * 1.0e6) as u64
}

/// 一句的每個字：排版拿寬度與字形鍵，再對上逐字時間
fn glyphs(text: &mut Text, l: &crate::lyrics::Line, next: i64) -> Vec<GIn> {
    let (shown, run) = text.glyph_run(&l.text, sdf::BASE, WEIGHT);
    let times = char_times(l, next);
    let shown_chars: Vec<(usize, char)> = shown.char_indices().collect();
    let n_orig = l.text.chars().count().max(1);
    let n_shown = shown_chars.len().max(1);
    run.iter()
        .map(|&(start, w, key)| {
            let ci = shown_chars.iter().position(|&(b, _)| b >= start).unwrap_or(n_shown - 1);
            let ch = shown_chars.get(ci).map_or(' ', |c| c.1);
            // 簡轉繁字數通常一樣；不一樣就照比例對回原句
            let oi = if n_orig == n_shown { ci } else { ci * n_orig / n_shown };
            let (t, d) = times.get(oi).copied().unwrap_or((l.t, 200));
            GIn { ch, adv: w / sdf::BASE, key: (!ch.is_whitespace()).then_some(key), t, d }
        })
        .collect()
}

/// 原句每個字（char）開始唱的時間與長度。有逐字照逐字；沒有就把這句平均分給每個字
fn char_times(l: &crate::lyrics::Line, next: i64) -> Vec<(i64, i64)> {
    let chars: Vec<char> = l.text.chars().collect();
    let n = chars.len();
    // 逐字：把每個字詞的時間平均分給它裡面的字，跳過空白後依序對上原句
    if let Some(ws) = l.words.as_ref().filter(|w| !w.is_empty()) {
        let mut per: Vec<(i64, i64)> = Vec::new();
        for wd in ws {
            let cs: Vec<char> = wd.text.chars().filter(|c| !c.is_whitespace()).collect();
            let m = cs.len().max(1) as i64;
            for j in 0..cs.len() as i64 {
                per.push((wd.t + wd.d * j / m, (wd.d / m).max(1)));
            }
        }
        let mut out = Vec::with_capacity(n);
        let mut k = 0;
        let mut last = (l.t, 200);
        for c in &chars {
            if c.is_whitespace() {
                out.push((last.0 + last.1, 0));
                continue;
            }
            if let Some(&tm) = per.get(k) {
                last = tm;
            }
            out.push(last);
            k += 1;
        }
        if k == per.len() || per.len() > n / 2 {
            return out;
        }
    }
    // 整句時間：字平均分配，最多用到句長的八成、每字最多 320 ms
    let visible = chars.iter().filter(|c| !c.is_whitespace()).count().max(1) as i64;
    let span = ((next - l.t) * 8 / 10).min(visible * 320).max(visible * 60);
    let per = span / visible;
    let mut k = 0;
    chars
        .iter()
        .map(|c| {
            let t = l.t + k * per;
            if !c.is_whitespace() {
                k += 1;
            }
            (t, per)
        })
        .collect()
}

fn hash(s: &str) -> u64 {
    // FNV-1a：只拿來當種子，跨版本要穩定（std 的 Hasher 不保證）
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn with_a(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}

fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

/// 強調色在深色畫面上要夠亮：太暗的往白色拉
fn bright(c: [f32; 4]) -> [f32; 4] {
    let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let t = ((0.55 - l) / 0.55).clamp(0.0, 0.6);
    mix([c[0], c[1], c[2], 1.0], [1.0; 4], t)
}

/// 0..1 夾住再做 smoothstep
fn smooth01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}
fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) * 0.5 }
}
fn expo_out(t: f32) -> f32 {
    if t >= 1.0 { 1.0 } else { 1.0 - 2f32.powf(-10.0 * t) }
}
/// 衝過頭再回來，`s` 越大衝越多
fn back_out(t: f32, s: f32) -> f32 {
    let u = t - 1.0;
    1.0 + (s + 1.0) * u * u * u + s * u * u
}
fn bounce_out(t: f32) -> f32 {
    let (n, d) = (7.5625, 2.75);
    if t < 1.0 / d {
        n * t * t
    } else if t < 2.0 / d {
        let t = t - 1.5 / d;
        n * t * t + 0.75
    } else if t < 2.5 / d {
        let t = t - 2.25 / d;
        n * t * t + 0.9375
    } else {
        let t = t - 2.625 / d;
        n * t * t + 0.984375
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::{Line as LLine, Word};

    #[test]
    fn 逐字時間對上原句() {
        let l = LLine {
            t: 1000,
            text: "你好 嗎".into(),
            words: Some(vec![
                Word { t: 1000, d: 100, text: "你".into() },
                Word { t: 1100, d: 200, text: "好 ".into() },
                Word { t: 1300, d: 400, text: "嗎".into() },
            ]),
            ..Default::default()
        };
        let t = char_times(&l, 3000);
        assert_eq!(t.len(), 4);
        assert_eq!(t[0], (1000, 100));
        assert_eq!(t[1], (1100, 200));
        assert_eq!(t[3], (1300, 400));
    }

    #[test]
    fn 沒有逐字就平均分() {
        let l = LLine { t: 0, text: "一二三四".into(), ..Default::default() };
        let t = char_times(&l, 10_000);
        assert_eq!(t, vec![(0, 320), (320, 320), (640, 320), (960, 320)]);
        // 句子很短：用句長的八成
        let t = char_times(&l, 1000);
        assert_eq!(t[1], (200, 200));
    }

    #[test]
    fn 緩動的端點() {
        for f in [ease_out, ease_in_out, expo_out, bounce_out] {
            assert!(f(0.0).abs() < 1e-3);
            assert!((f(1.0) - 1.0).abs() < 1e-3);
        }
        assert!((back_out(1.0, 2.4) - 1.0).abs() < 1e-4);
        assert!(back_out(0.7, 2.4) > 1.0, "要衝過頭");
    }
}
