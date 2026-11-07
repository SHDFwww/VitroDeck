//! 燈條追光：文字 PV 演著、視窗在前景時，把畫面四邊的顏色送給燈條（SkydimoBloom）。
//!
//! 整幀先在 GPU 上縮成 [`GW`]×[`GH`] 格（`ledglow.wgsl`），讀回來只有 8 KB，
//! 再取四邊的那幾圈格子平均，經 [`crate::ledfeed`] 送出去。燈條那頭自己對應到 LED。
//! 讀回照 `Glass::probe` 的做法：錄複製、送出去之後才映射，不等，下一幀再收。
//!
//! 第一次要送才建（`Render::glow`），沒人訂閱就零開銷。

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::gfx::{Gpu, Offscreen};

/// 格數，要跟 `ledglow.wgsl` 一樣。寬 64 對 41 顆的長邊、高 32 對 15 顆的短邊都夠
pub const GW: usize = 64;
pub const GH: usize = 32;
/// 上下邊取幾列格子（32 列的 3 列 ≈ 畫面高度的 9%）。左右照實際像素換成差不多的深度
const DEPTH_ROWS: usize = 3;
/// 送的間隔下限：燈條那頭 60 FPS，180 Hz 的螢幕不必每幀都讀
const GAP: Duration = Duration::from_millis(15);
const SLOTS: usize = 3;

const IDLE: u8 = 0;
const COPIED: u8 = 1;
const MAPPING: u8 = 2;
const READY: u8 = 3;
const FAILED: u8 = 4;

struct Slot {
    buf: wgpu::Buffer,
    state: Arc<AtomicU8>,
    /// 拍的那一幀的寬高比，算左右邊的深度用
    aspect: f32,
}

pub struct Glow {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    grid: Offscreen,
    slots: Vec<Slot>,
    last: Option<Instant>,
    /// 送過邊緣色、還沒送停止
    live: bool,
}

impl Glow {
    pub fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ledglow"),
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
        let shader = device.create_shader_module(wgpu::include_wgsl!("ledglow.wgsl"));
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ledglow"),
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
                targets: &[Some(crate::gfx::OFFSCREEN_FORMAT.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ledglow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let grid = Offscreen::new(device, GW as u32, GH as u32);
        Self { pipeline, layout, sampler, grid, slots: Vec::new(), last: None, live: false }
    }

    /// 把 `scene` 縮成格子、錄一趟複製到讀回緩衝。錄在 present 那個 encoder 裡，送出去之後叫 [`Self::after_submit`]
    pub fn record(&mut self, gpu: &Gpu, enc: &mut wgpu::CommandEncoder, scene: &Offscreen) {
        if self.last.is_some_and(|t| t.elapsed() < GAP) {
            return;
        }
        let k = match self.slots.iter().position(|s| s.state.load(Ordering::Acquire) == IDLE) {
            Some(k) => k,
            None if self.slots.len() < SLOTS => {
                self.slots.push(Slot {
                    buf: gpu.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("ledglow"),
                        size: (GW * 4 * GH) as u64,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                    state: Arc::new(AtomicU8::new(IDLE)),
                    aspect: 1.0,
                });
                self.slots.len() - 1
            }
            // 全都還在路上：這次不拍
            None => return,
        };
        self.last = Some(Instant::now());
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&scene.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        {
            let mut pass = crate::gfx::pass(enc, &self.grid.view, "ledglow", None);
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        let s = &mut self.slots[k];
        // 一列 64×4 = 256 bytes，剛好是複製要的對齊，不必補
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.grid.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &s.buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some((GW * 4) as u32),
                    rows_per_image: Some(GH as u32),
                },
            },
            wgpu::Extent3d { width: GW as u32, height: GH as u32, depth_or_array_layers: 1 },
        );
        s.aspect = scene.width as f32 / scene.height.max(1) as f32;
        s.state.store(COPIED, Ordering::Release);
    }

    /// encoder 送出去之後叫：剛錄的開始映射，之前映射好的收下來送給燈條
    pub fn after_submit(&mut self, gpu: &Gpu) {
        if let Some(p) = self.take(gpu, wgpu::PollType::Poll) {
            crate::ledfeed::send_glow(&p);
            self.live = true;
        }
    }

    /// 開始映射剛錄的、收下映射好的，回傳最後收到的那包
    fn take(&mut self, gpu: &Gpu, poll: wgpu::PollType) -> Option<Vec<u8>> {
        for s in &self.slots {
            if s.state.load(Ordering::Acquire) == COPIED {
                s.state.store(MAPPING, Ordering::Release);
                let st = s.state.clone();
                s.buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
                    st.store(if r.is_ok() { READY } else { FAILED }, Ordering::Release);
                });
            }
        }
        let _ = gpu.device.poll(poll);
        // 同時好幾格都好了的話只送其中一格就夠，哪格都行：間隔只有 15 ms
        let mut out = None;
        for s in &self.slots {
            match s.state.load(Ordering::Acquire) {
                READY => {
                    if let Ok(data) = s.buf.slice(..).get_mapped_range() {
                        out = Some(packet(&data, s.aspect));
                    }
                    s.buf.unmap();
                    s.state.store(IDLE, Ordering::Release);
                }
                FAILED => s.state.store(IDLE, Ordering::Release),
                _ => {}
            }
        }
        out
    }

    /// 不演了：告訴燈條馬上退回自己的模式，不必等逾時
    pub fn stop(&mut self) {
        if std::mem::take(&mut self.live) {
            crate::ledfeed::glow_stop();
        }
    }
}

/// 格子（`GW`×`GH` 個 RGBA，一列接一列）→ 封包：
///
/// ```text
/// b"VDE1" GW GH  上[GW] 下[GW] 左[GH] 右[GH]     每個 RGB 三個 byte
/// ```
///
/// 上下邊由左到右、左右邊由上到下，都是看著螢幕的方向。`aspect` 是畫面寬高比：
/// 左右邊取的欄數換成跟上下邊差不多的實際深度
pub fn packet(px: &[u8], aspect: f32) -> Vec<u8> {
    let at = |x: usize, y: usize| &px[(y * GW + x) * 4..(y * GW + x) * 4 + 3];
    let cols = ((DEPTH_ROWS as f32 * GW as f32 / GH as f32 / aspect.max(0.1)).round() as usize).clamp(1, GW / 2);
    let avg = |cells: &mut dyn Iterator<Item = (usize, usize)>| {
        let (mut sum, mut n) = ([0u32; 3], 0u32);
        for (x, y) in cells {
            for (s, v) in sum.iter_mut().zip(at(x, y)) {
                *s += *v as u32;
            }
            n += 1;
        }
        sum.map(|s| ((s + n / 2) / n.max(1)) as u8)
    };
    let mut p = Vec::with_capacity(6 + (GW + GH) * 2 * 3);
    p.extend_from_slice(b"VDE1");
    p.extend_from_slice(&[GW as u8, GH as u8]);
    for x in 0..GW {
        p.extend_from_slice(&avg(&mut (0..DEPTH_ROWS).map(|y| (x, y))));
    }
    for x in 0..GW {
        p.extend_from_slice(&avg(&mut (GH - DEPTH_ROWS..GH).map(|y| (x, y))));
    }
    for y in 0..GH {
        p.extend_from_slice(&avg(&mut (0..cols).map(|x| (x, y))));
    }
    for y in 0..GH {
        p.extend_from_slice(&avg(&mut (GW - cols..GW).map(|x| (x, y))));
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 四邊各取各的方向() {
        // 左上紅、右下藍，其餘黑
        let mut px = vec![0u8; GW * GH * 4];
        let mut put = |x: usize, y: usize, c: [u8; 3]| px[(y * GW + x) * 4..(y * GW + x) * 4 + 3].copy_from_slice(&c);
        for y in 0..DEPTH_ROWS {
            put(0, y, [255, 0, 0]);
            put(GW - 1, GH - 1 - y, [0, 0, 255]);
        }
        let p = packet(&px, 2.0);
        assert_eq!(&p[..6], &[b'V', b'D', b'E', b'1', GW as u8, GH as u8]);
        assert_eq!(p.len(), 6 + (GW + GH) * 2 * 3);
        let rgb = |i: usize| &p[6 + i * 3..6 + i * 3 + 3];
        // 上邊第一格：三列都是紅
        assert_eq!(rgb(0), &[255, 0, 0]);
        assert_eq!(rgb(1), &[0, 0, 0]);
        // 下邊最後一格：三列都是藍
        assert_eq!(rgb(GW + GW - 1), &[0, 0, 255]);
        // 寬高比 2 → 左右取 3 欄；左邊第一格是 3 欄裡 1 欄紅
        assert_eq!(rgb(GW * 2), &[85, 0, 0]);
        assert_eq!(rgb(GW * 2 + GH * 2 - 1), &[0, 0, 85]);
    }

    /// 著色器真的跑一次：整張塗滿一個顏色，四邊讀回來都是它。
    /// 要 GPU，所以 ignore：`cargo test --bin vitro -- --ignored ledglow`
    #[test]
    #[ignore]
    fn 縮圖讀回是原本的顏色() {
        let gpu = Gpu::headless().unwrap();
        let scene = Offscreen::new(&gpu.device, 3440, 1440);
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        drop(crate::gfx::pass(&mut enc, &scene.view, "fill", Some(wgpu::Color { r: 0.2, g: 0.4, b: 0.6, a: 1.0 })));
        let mut g = Glow::new(&gpu);
        g.record(&gpu, &mut enc, &scene);
        gpu.queue.submit([enc.finish()]);
        let p = g.take(&gpu, wgpu::PollType::wait_indefinitely()).expect("沒讀回來");
        assert_eq!(p.len(), 6 + (GW + GH) * 2 * 3);
        for c in p[6..].chunks_exact(3) {
            assert_eq!(c, &[51, 102, 153]);
        }
    }
}
