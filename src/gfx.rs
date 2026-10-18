//! GPU 基礎層：裝置、離屏算圖、以及把結果讀回來存成 PNG。
//!
//! 那個 PNG 不是附加功能，是**這個里程碑的驗證手段** —— 原生視窗沒有
//! DevTools 也截不了圖，畫錯了只能靠肉眼。離屏算一張存檔就能直接看，
//! 而且可以進自動化比對。

use std::sync::Arc;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_name: String,
}

impl Gpu {
    /// 不需要視窗的裝置，離屏算圖用。
    pub fn headless() -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))?;
        let name = adapter.get_info().name;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        Ok(Self { device, queue, adapter_name: name })
    }
}

impl Gpu {
    /// 有視窗時用這條。要先有 surface 才能挑到能對它輸出的介面卡。
    pub fn for_surface(
        instance: &wgpu::Instance,
        surface: &wgpu::Surface<'static>,
    ) -> anyhow::Result<Self> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(surface),
            ..Default::default()
        }))?;
        let info = adapter.get_info();
        let name = format!("{} ({:?})", info.name, info.backend);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))?;
        Ok(Self { device, queue, adapter_name: name })
    }
}

/// 把離屏材質整張貼到畫面上。
///
/// 整個畫面都先畫進離屏再貼出來，是因為玻璃要把「底下已經畫好的東西」
/// 當材質取樣，而 surface 的材質通常沒有 `TEXTURE_BINDING`。
/// 代價是一次全螢幕複製，換來視窗與 `--shot` 共用同一條算圖路徑。
pub struct Blit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl Blit {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let device = &gpu.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
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
        let shader = device.create_shader_module(wgpu::include_wgsl!("blit.wgsl"));
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self { pipeline, layout, sampler }
    }

    pub fn draw(&self, gpu: &Gpu, pass: &mut wgpu::RenderPass<'_>, src: &wgpu::TextureView) {
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

thread_local! {
    /// 這一幀的 DPI 縮放（實體像素 ÷ 邏輯像素）。版面一律用邏輯像素算，
    /// 矩形、玻璃、文字的 renderer 在送進 GPU 前才換成實體像素。
    /// 迷你視窗自己乘縮放，畫它之前要設回 1.0
    static SCALE: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
}

pub fn set_scale(s: f32) {
    SCALE.with(|c| c.set(if s.is_finite() && s > 0.0 { s } else { 1.0 }));
}

pub fn scale() -> f32 {
    SCALE.with(|c| c.get())
}

/// 暫時換一個縮放，離開作用域就換回來（中途 `?` 返回也會換回來）
pub struct ScaleGuard(f32);

impl Drop for ScaleGuard {
    fn drop(&mut self) {
        set_scale(self.0);
    }
}

pub fn scoped_scale(s: f32) -> ScaleGuard {
    let prev = scale();
    set_scale(s);
    ScaleGuard(prev)
}

/// 使用者設的介面倍率（設定的「介面大小」），存成 f32 的位元。初值 1.0
static UI_ZOOM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);

pub fn set_ui_zoom(z: f32) {
    let z = if z.is_finite() { z.clamp(0.5, 3.0) } else { 1.0 };
    UI_ZOOM.store(z.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

pub fn ui_zoom() -> f32 {
    f32::from_bits(UI_ZOOM.load(std::sync::atomic::Ordering::Relaxed))
}

/// 主視窗與設定視窗的縮放：系統給的 DPI 縮放 × 介面倍率。
/// 迷你視窗不乘介面倍率，直接用系統的
pub fn zoomed(system: f64) -> f32 {
    system as f32 * ui_zoom()
}

/// 同 `zoomed`，但放大後的邏輯大小放不下 `min` 時，倍率降到剛好放得下（不低於 1）。
/// 視窗模式靠最小視窗大小撐開，用不到這個；全螢幕沒辦法再變大，就靠它。`phys` 是視窗的實體大小
pub fn zoomed_fit(system: f64, phys: (u32, u32), min: (f32, f32)) -> f32 {
    let (sys, z) = (system as f32, ui_zoom());
    if z <= 1.0 || phys.0 == 0 || phys.1 == 0 {
        return sys * z;
    }
    let fit = (phys.0 as f32 / (sys * min.0)).min(phys.1 as f32 / (sys * min.1));
    sys * z.min(fit.max(1.0))
}

/// 實體尺寸換成邏輯尺寸。縮放 1.0 時就是原值
pub fn logical(w: u32, h: u32) -> (f32, f32) {
    let s = scale();
    (w as f32 / s, h as f32 / s)
}

/// 一張可以被畫上去、也可以被讀回來的離屏材質。
pub struct Offscreen {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

/// 離屏一律用這個格式：帶 sRGB，顏色才會跟視窗上看到的一致。
/// **合成一律在 sRGB 數值上做，不轉線性。**
///
/// 瀏覽器就是這樣：`rgba(8,8,11,.95)` 蓋上去是直接對 0..255 的編碼值
/// 做線性內插。整份 CSS 的 alpha（.055 的玻璃底色、.045 的體積感、
/// 兩層紗的 .35/.95）都是照這個空間調出來的，改在線性空間混就全都不對 ——
/// 壓暗的紗會變淡、玻璃會偏亮，整體看起來就是「灰灰的、沒有對比」。
///
/// 物理上線性才正確，但這裡要的是跟 web 版一模一樣，不是物理正確。
/// MilkDrop 那邊也剛好是這個空間（WebGL 預設的 framebuffer 不做色彩管理）。
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl Offscreen {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Self { texture, view, width, height }
    }

    /// 把畫好的內容讀回主記憶體。
    ///
    /// GPU→CPU 的複製要求每列的位元組數對齊到 256，所以視窗寬度不是 64 的
    /// 倍數時會有 padding，必須一列一列搬回去，不能整塊當成連續的。
    pub fn read_rgba(&self, gpu: &Gpu) -> Vec<u8> {
        const ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let unpadded = self.width * 4;
        let padded = unpadded.div_ceil(ALIGN) * ALIGN;

        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * self.height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d { width: self.width, height: self.height, depth_or_array_layers: 1 },
        );
        gpu.queue.submit([enc.finish()]);

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().ok();

        let data = slice.get_mapped_range().expect("讀回緩衝區映射失敗");
        let mut out = Vec::with_capacity((unpadded * self.height) as usize);
        for row in 0..self.height {
            let start = (row * padded) as usize;
            out.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        buffer.unmap();
        out
    }

    pub fn save_png(&self, gpu: &Gpu, path: &std::path::Path) -> anyhow::Result<()> {
        let rgba = self.read_rgba(gpu);
        let img = image::RgbaImage::from_raw(self.width, self.height, rgba)
            .ok_or_else(|| anyhow::anyhow!("尺寸跟資料量對不上"))?;
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        img.save(path)?;
        Ok(())
    }
}

/// 開一趟只畫到 `view` 的 render pass。`clear` 決定要不要先清掉。
pub fn pass<'a>(
    enc: &'a mut wgpu::CommandEncoder,
    view: &'a wgpu::TextureView,
    label: &'a str,
    clear: Option<wgpu::Color>,
) -> wgpu::RenderPass<'a> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: match clear {
                    Some(c) => wgpu::LoadOp::Clear(c),
                    None => wgpu::LoadOp::Load,
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

/// 上傳一張 RGBA8 圖到 GPU。專輯封面、SMTC 縮圖都走這裡。
pub fn upload_rgba(
    gpu: &Gpu,
    width: u32,
    height: u32,
    rgba: &[u8],
    label: &str,
) -> Arc<wgpu::TextureView> {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // 封面是 sRGB 的圖檔，而整條管線就在 sRGB 數值上合成（見
        // OFFSCREEN_FORMAT），所以直接當 UNORM 上傳，不要讓取樣器解回線性。
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    Arc::new(texture.create_view(&Default::default()))
}

/// 線性空間的版本。雜訊貼圖是資料不是圖片，**不能**當 sRGB 解 ——
/// 那會讓 preset 拿到的數值整個偏掉。
pub fn upload_rgba_linear(
    gpu: &Gpu,
    width: u32,
    height: u32,
    rgba: &[u8],
    label: &str,
) -> Arc<wgpu::TextureView> {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    Arc::new(texture.create_view(&Default::default()))
}

/// 解碼 PNG／JPEG／WebP 再上傳。Spotify 的封面大多是 JPEG（少數 WebP），SMTC 的縮圖是 PNG。
pub fn upload_encoded(gpu: &Gpu, bytes: &[u8], label: &str) -> anyhow::Result<Arc<wgpu::TextureView>> {
    let img = image::load_from_memory(bytes)?.to_rgba8();
    let (w, h) = img.dimensions();
    Ok(upload_rgba(gpu, w, h, &img, label))
}
