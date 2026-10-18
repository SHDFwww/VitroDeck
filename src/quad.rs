//! 圓角矩形渲染器 —— 整套原生 UI 的主力。
//!
//! CSS 那 35 處 `border-radius`、16 處 `box-shadow`、還有外框，在原生全部
//! 要自己算。這裡用 SDF 一次解決形狀、抗鋸齒與陰影，因為三者本質上都是
//! 「離邊界多遠」的函數。
//!
//! 同一張材質的矩形會併成一個 draw call，所以整面封面流也只是幾次呼叫。

use std::sync::Arc;

use crate::gfx::Gpu;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Quad {
    pub rect: [f32; 4],
    pub radius: [f32; 4],
    pub color: [f32; 4],
    pub uv: [f32; 4],
    pub shadow: [f32; 4],
    pub shadow_color: [f32; 4],
    pub border: [f32; 4],
    pub border_color: [f32; 4],
}

impl Default for Quad {
    fn default() -> Self {
        Self {
            rect: [0.0; 4],
            radius: [0.0; 4],
            color: [1.0, 1.0, 1.0, 1.0],
            // u1 <= u0 代表不貼圖
            uv: [0.0, 0.0, -1.0, -1.0],
            shadow: [0.0, 0.0, 0.0, 0.0],
            shadow_color: [0.0, 0.0, 0.0, 0.0],
            border: [0.0; 4],
            border_color: [0.0; 4],
        }
    }
}

impl Quad {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { rect: [x, y, w, h], ..Default::default() }
    }
    pub fn color(mut self, c: [f32; 4]) -> Self {
        self.color = c;
        self
    }
    pub fn radius(mut self, r: f32) -> Self {
        self.radius = [r; 4];
        self
    }
    pub fn radius4(mut self, tl: f32, tr: f32, br: f32, bl: f32) -> Self {
        self.radius = [tl, tr, br, bl];
        self
    }
    pub fn shadow(mut self, dx: f32, dy: f32, blur: f32, spread: f32, c: [f32; 4]) -> Self {
        self.shadow = [dx, dy, blur, spread];
        self.shadow_color = c;
        self
    }
    /// 套一組陰影 token。`alpha_mul` 是淡入淡出的倍率（不淡就給 1.0）
    pub fn shadow_of(self, s: &crate::theme::Shadow, alpha_mul: f32) -> Self {
        self.shadow(s.dx, s.dy, s.blur, s.spread, s.color(alpha_mul))
    }
    /// 同一個形狀的影子，但顏色是 `theme::halo`：墊在字底下的光暈，淺色模式是白霧
    pub fn halo_of(self, s: &crate::theme::Shadow, alpha_mul: f32) -> Self {
        self.shadow(s.dx, s.dy, s.blur, s.spread, crate::theme::halo(s.alpha * alpha_mul))
    }
    /// 只動寬度那一格：`border` 的其他三格是形狀、飽和度、旋轉，不能被洗掉
    pub fn border(mut self, w: f32, c: [f32; 4]) -> Self {
        self.border[0] = w;
        self.border_color = c;
        self
    }

    /// 繞矩形中心轉 `rad` 弧度（正值順時針，畫面 y 朝下 —— 跟 CSS 的 rotate 一樣）。
    /// 貼圖、圓角、外框、陰影全部跟著轉。
    pub fn rotate(mut self, rad: f32) -> Self {
        self.border[3] = rad;
        self
    }

    /// 黑膠唱片：圓、溝紋、標籤外緣與外圈的亮線、固定方向的反光。
    /// 矩形要是正方形，`radius` 設成邊長一半（陰影才會是圓的）。
    pub fn vinyl(mut self) -> Self {
        self.border[1] = 4.0;
        self
    }

    /// 調整貼圖的飽和度（1.0 = 原樣）。只對有貼圖的有效。
    pub fn saturate(mut self, s: f32) -> Self {
        self.border[2] = s;
        self
    }

    /// 換成三角形。`right` 決定指向哪邊，`round` 是尖角的收斂量。
    ///
    /// 播放鍵這種東西不用字型畫 —— `▶` 在 Windows 會被 Segoe UI Emoji
    /// 接走變成彩色表情符號，跟灰階的介面完全不搭，而且沒辦法控制。
    pub fn triangle(mut self, right: bool, round: f32) -> Self {
        self.border[1] = if right { 1.0 } else { 2.0 };
        self.radius = [round; 4];
        self
    }
    /// 整張材質鋪滿這個矩形。
    pub fn textured(mut self) -> Self {
        self.uv = [0.0, 0.0, 1.0, 1.0];
        self
    }
    /// 維持長寬比的裁切（等同 CSS 的 `object-fit: cover`）。
    pub fn cover(mut self, tex_w: f32, tex_h: f32) -> Self {
        let (rw, rh) = (self.rect[2], self.rect[3]);
        if rw <= 0.0 || rh <= 0.0 || tex_w <= 0.0 || tex_h <= 0.0 {
            return self.textured();
        }
        let scale = (rw / tex_w).max(rh / tex_h);
        let (vw, vh) = (rw / (tex_w * scale), rh / (tex_h * scale));
        let (u0, v0) = ((1.0 - vw) * 0.5, (1.0 - vh) * 0.5);
        self.uv = [u0, v0, u0 + vw, v0 + vh];
        self
    }
}

struct Batch {
    texture: Option<Arc<wgpu::TextureView>>,
    start: u32,
    count: u32,
}

pub struct QuadRenderer {
    pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)]
    globals_layout: wgpu::BindGroupLayout,
    tex_layout: wgpu::BindGroupLayout,
    globals: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    white: Arc<wgpu::TextureView>,
    instances: wgpu::Buffer,
    capacity: u64,
    quads: Vec<Quad>,
    batches: Vec<Batch>,
    /// 已經錄進 draw 的批次數，用來切出「這一趟要畫哪些」
    flushed: usize,
    /// 下一個 push 必須另開一批（剛 flush 過）
    break_batch: bool,
    /// 之後 push 的每一個都乘上這個透明度、往下移這麼多（轉場時一整組元素一起淡入／浮上來）。
    /// `begin` 會重設
    fx_alpha: f32,
    fx_dy: f32,
    /// 點陣化過的按鈕圖示（圖示, 邊長 px）。材質綁在這個 renderer 的 device 上，所以放這裡
    icons: std::collections::HashMap<(crate::icons::Svg, u32), Arc<wgpu::TextureView>>,
}

const INITIAL_CAPACITY: u64 = 4096;

impl QuadRenderer {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let device = &gpu.device;

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad texture"),
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

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("quad sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        // 不貼圖的矩形也要綁一張材質，用 1x1 白點頂著，shader 才不用分支。
        let white = crate::gfx::upload_rgba(gpu, 1, 1, &[255, 255, 255, 255], "white");

        let shader = device.create_shader_module(wgpu::include_wgsl!("quad.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&globals_layout), Some(&tex_layout)],
            immediate_size: 0,
        });

        // 8 個 vec4 的 instance 屬性，剛好在 16 個屬性的上限內。
        let attrs: Vec<wgpu::VertexAttribute> = (0..8)
            .map(|i| wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: i * 16,
                shader_location: i as u32,
            })
            .collect();

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Quad>() as u64,
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
                    // shader 輸出的是預乘 alpha，所以來源用 One 不是 SrcAlpha。
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

        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad instances"),
            size: INITIAL_CAPACITY * std::mem::size_of::<Quad>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            globals_layout,
            tex_layout,
            globals,
            globals_bind,
            sampler,
            white,
            instances,
            capacity: INITIAL_CAPACITY,
            quads: Vec::new(),
            batches: Vec::new(),
            flushed: 0,
            break_batch: false,
            fx_alpha: 1.0,
            fx_dy: 0.0,
            icons: Default::default(),
        }
    }

    /// 畫一個按鈕圖示，中心在 (cx, cy)、邊長 `px`、顏色 `col`。
    /// 位置對齊整數像素，圖示是照這個大小點陣化的，對齊了才不會被取樣糊掉。
    pub fn svg(&mut self, gpu: &Gpu, icon: crate::icons::Svg, px: f32, cx: f32, cy: f32, col: [f32; 4]) {
        // 照實體像素點陣化、對齊實體像素格，縮放之後才不會糊
        let k = crate::gfx::scale();
        let n = (px * k).round().max(1.0) as u32;
        let tex = self
            .icons
            .entry((icon, n))
            .or_insert_with(|| {
                crate::gfx::upload_rgba(gpu, n, n, &crate::icons::rasterize(icon, n), "icon")
            })
            .clone();
        let s = n as f32;
        let (x, y) = ((cx * k - s * 0.5).round() / k, (cy * k - s * 0.5).round() / k);
        let q = Quad::new(x, y, s / k, s / k).textured().color(col);
        self.push_tex(q, &tex);
    }

    /// 一幀的開始。容量只在這裡長大 —— 不能在 `push` 裡換掉 buffer，
    /// 因為前面的 pass 已經用 `set_vertex_buffer` 錄進舊的那個了。
    pub fn begin(&mut self, gpu: &Gpu) {
        if self.quads.len() as u64 > self.capacity {
            self.capacity = (self.quads.len() as u64).next_power_of_two();
            self.instances = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("quad instances"),
                size: self.capacity * std::mem::size_of::<Quad>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        self.quads.clear();
        self.batches.clear();
        self.flushed = 0;
        self.break_batch = false;
        self.fx_alpha = 1.0;
        self.fx_dy = 0.0;
    }

    /// 之後 push 的一律乘上 `alpha`、往下移 `dy`。畫完那一組記得設回 `(1, 0)`。
    pub fn set_fx(&mut self, alpha: f32, dy: f32) {
        self.fx_alpha = alpha;
        self.fx_dy = dy;
    }

    pub fn push(&mut self, q: Quad) {
        self.push_with(q, None);
    }

    pub fn push_tex(&mut self, q: Quad, tex: &Arc<wgpu::TextureView>) {
        self.push_with(q, Some(tex.clone()));
    }

    fn push_with(&mut self, mut q: Quad, tex: Option<Arc<wgpu::TextureView>>) {
        if self.fx_alpha != 1.0 || self.fx_dy != 0.0 {
            q.rect[1] += self.fx_dy;
            q.color[3] *= self.fx_alpha;
            q.shadow_color[3] *= self.fx_alpha;
            q.border_color[3] *= self.fx_alpha;
        }
        // 材質沒變就併進上一批，變了才開新的一批。畫的順序必須保持，
        // 不能為了少幾個 draw call 重排 —— 那會弄亂疊放次序。
        //
        // **flush 之後的第一個一定要另開一批。** 不然它會被併進「已經畫過」
        // 的那一批裡，而 `flush` 只畫 `flushed..` 的部分 —— 結果是這個矩形
        // 完全不會出現，而且沒有任何錯誤。設定面板的選取底色就是這樣消失的。
        let same = !self.break_batch
            && match (self.batches.last(), &tex) {
                (Some(b), None) => b.texture.is_none(),
                (Some(b), Some(t)) => b.texture.as_ref().is_some_and(|bt| Arc::ptr_eq(bt, t)),
                (None, _) => false,
            };
        self.break_batch = false;
        if same {
            self.batches.last_mut().unwrap().count += 1;
        } else {
            self.batches.push(Batch { texture: tex, start: self.quads.len() as u32, count: 1 });
        }
        self.quads.push(q);
    }

    /// 把上次 flush 之後累積的矩形畫進這個 pass。
    ///
    /// **這裡不寫任何 buffer。** `queue.write_buffer` 是在 submit 前一次套用的，
    /// 不是照錄製順序 —— 一幀內 flush 好幾次而每次都從頭寫，
    /// 只有最後一次的資料會留下來，前面幾趟畫到的會是別人的東西。
    /// 所有資料改成在 [`Self::upload`] 一次寫完，這裡只負責錄 draw。
    pub fn flush(&mut self, gpu: &Gpu, pass: &mut wgpu::RenderPass<'_>, _w: u32, _h: u32) {
        if self.flushed >= self.batches.len() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_bind, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));

        for b in &self.batches[self.flushed..] {
            let view = b.texture.as_ref().unwrap_or(&self.white);
            let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
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
            });
            pass.set_bind_group(1, &bind, &[]);
            pass.draw(0..6, b.start..b.start + b.count);
        }
        self.flushed = self.batches.len();
        self.break_batch = true;
    }

    /// 把整幀的 instance 資料一次寫上去。要在 `queue.submit` 之前呼叫，
    /// 錄製順序無所謂 —— buffer 寫入本來就是在命令執行前才套用的。
    pub fn upload(&self, gpu: &Gpu, width: u32, height: u32) {
        if !self.quads.is_empty() {
            gpu.queue
                .write_buffer(&self.instances, 0, bytemuck::cast_slice(&self.quads));
        }
        // 頂點座標是邏輯像素，所以這裡給邏輯解析度；fwidth 量的是實體像素，抗鋸齒仍是一個實體像素寬
        let (lw, lh) = crate::gfx::logical(width, height);
        gpu.queue.write_buffer(&self.globals, 0, bytemuck::cast_slice(&[lw, lh, 0.0, 0.0]));
    }

    #[allow(dead_code)]
    pub fn globals_layout(&self) -> &wgpu::BindGroupLayout {
        &self.globals_layout
    }
}
