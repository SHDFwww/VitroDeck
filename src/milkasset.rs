//! MilkDrop 著色器要的固定資源：雜訊貼圖與取樣器。
//!
//! preset 的著色器會取樣 `sampler_noise_lq`、`sampler_noisevol_hq` 這些名字。
//! MilkDrop 自己是程式產生這些貼圖的，不是外部檔案，所以這裡照做。
//!
//! 這些東西整個程式只需要一份，所以跟 `Milk` 拆開 —— 換 preset 時不用重建。

use std::sync::Arc;

use crate::gfx::Gpu;

/// 一個夠用的雜訊來源。不需要密碼學強度，只要空間上看起來夠亂。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 8) as u8
    }
}

pub struct MilkAssets {
    /// 13 張 2D：main 系列由 Milk 自己換掉，這裡只放雜訊那幾張
    pub noise_lq: Arc<wgpu::TextureView>,
    pub noise_lq_lite: Arc<wgpu::TextureView>,
    pub noise_mq: Arc<wgpu::TextureView>,
    pub noise_hq: Arc<wgpu::TextureView>,
    pub noisevol_lq: Arc<wgpu::TextureView>,
    pub noisevol_hq: Arc<wgpu::TextureView>,
    /// MilkDrop 的六種取樣器狀態。preset 的 `sampler_fw_main` 這類名字
    /// 指的都是**同一張主貼圖**，差別只在這裡：f=filtered／p=point、
    /// w=wrap／c=clamp。用錯的話銳化與雜訊查表整個走樣。
    pub smp_fw: wgpu::Sampler,
    pub smp_fc: wgpu::Sampler,
    pub smp_pw: wgpu::Sampler,
    pub smp_pc: wgpu::Sampler,
    pub smp_noise: wgpu::Sampler,
    pub smp_noise_pt: wgpu::Sampler,
    #[allow(dead_code)]
    pub sampler: wgpu::Sampler,
}

fn mk_sampler(
    device: &wgpu::Device,
    label: &str,
    linear: bool,
    repeat: bool,
) -> wgpu::Sampler {
    let f = if linear { wgpu::FilterMode::Linear } else { wgpu::FilterMode::Nearest };
    // 原版是 LINEAR_MIPMAP_LINEAR / NEAREST_MIPMAP_NEAREST，跟著 f 走
    let mf = if linear { wgpu::MipmapFilterMode::Linear } else { wgpu::MipmapFilterMode::Nearest };
    let a = if repeat { wgpu::AddressMode::Repeat } else { wgpu::AddressMode::ClampToEdge };
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        mag_filter: f,
        min_filter: f,
        mipmap_filter: mf,
        address_mode_u: a,
        address_mode_v: a,
        address_mode_w: a,
        ..Default::default()
    })
}

impl MilkAssets {
    pub fn new(gpu: &Gpu) -> Self {
        let mut rng = Rng(0x1234_5678_9ABC_DEF0);
        Self {
            // 低頻雜訊：每 4×4 一格再放大，所以看起來是團塊而不是雪花
            noise_lq: noise2d(gpu, 256, 4, &mut rng, "noise_lq"),
            noise_lq_lite: noise2d(gpu, 32, 1, &mut rng, "noise_lq_lite"),
            noise_mq: noise2d(gpu, 256, 2, &mut rng, "noise_mq"),
            noise_hq: noise2d(gpu, 256, 1, &mut rng, "noise_hq"),
            noisevol_lq: noise3d(gpu, 32, 2, &mut rng, "noisevol_lq"),
            noisevol_hq: noise3d(gpu, 32, 1, &mut rng, "noisevol_hq"),
            smp_fw: mk_sampler(&gpu.device, "fw", true, true),
            smp_fc: mk_sampler(&gpu.device, "fc", true, false),
            smp_pw: mk_sampler(&gpu.device, "pw", false, true),
            smp_pc: mk_sampler(&gpu.device, "pc", false, false),
            smp_noise: mk_sampler(&gpu.device, "noise", true, true),
            smp_noise_pt: mk_sampler(&gpu.device, "noise-pt", false, true),
            sampler: gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("milk noise"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                // 雜訊一定要能重複取樣，preset 常常用遠大於 1 的 uv
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                address_mode_w: wgpu::AddressMode::Repeat,
                ..Default::default()
            }),
        }
    }
}

/// `cell` 是每格幾個像素 —— 越大越低頻。
fn noise2d(gpu: &Gpu, size: u32, cell: u32, rng: &mut Rng, label: &str) -> Arc<wgpu::TextureView> {
    let cells = size / cell.max(1);
    let grid: Vec<[u8; 4]> = (0..cells * cells)
        .map(|_| [rng.byte(), rng.byte(), rng.byte(), rng.byte()])
        .collect();
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let c = grid[((y / cell.max(1)) * cells + (x / cell.max(1))) as usize];
            data.extend_from_slice(&c);
        }
    }
    crate::gfx::upload_rgba_linear(gpu, size, size, &data, label)
}

fn noise3d(gpu: &Gpu, size: u32, cell: u32, rng: &mut Rng, label: &str) -> Arc<wgpu::TextureView> {
    let cells = size / cell.max(1);
    let grid: Vec<[u8; 4]> = (0..cells * cells * cells)
        .map(|_| [rng.byte(), rng.byte(), rng.byte(), rng.byte()])
        .collect();
    let mut data = Vec::with_capacity((size * size * size * 4) as usize);
    for z in 0..size {
        for y in 0..size {
            for x in 0..size {
                let (cx, cy, cz) = (x / cell.max(1), y / cell.max(1), z / cell.max(1));
                let c = grid[((cz * cells + cy) * cells + cx) as usize];
                data.extend_from_slice(&c);
            }
        }
    }

    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: size },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
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
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size * 4),
            rows_per_image: Some(size),
        },
        wgpu::Extent3d { width: size, height: size, depth_or_array_layers: size },
    );
    Arc::new(texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D3),
        ..Default::default()
    }))
}
