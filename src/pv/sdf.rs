//! 字形的 SDF 圖集。
//!
//! glyphon 的圖集是照「這個字級」點陣化的遮罩：字級每幀在變（彈入、放大）就每幀塞新的進去，
//! 以前就這樣把圖集塞滿過。文字 PV 的字要能任意放大、旋轉、傾斜、描邊、發光，
//! 所以另外存一份距離場：每個字只在 [`BASE`] 這個大小點陣化一次，存「離字的邊多遠」，
//! 放多大都是同一張，描邊與光暈也只是換個門檻。
//!
//! 距離場用 Felzenszwalb 的兩趟一維 EDT 算，一個字幾十微秒。

use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::CacheKey;

use crate::gfx::Gpu;

/// 點陣化的字級（px）。放到螢幕上幾百 px 時邊角會略圓，文字 PV 的大字看不出來
pub const BASE: f32 = 80.0;
/// 距離場往字外延伸幾 px。描邊與光暈最多只能到這麼寬（換算到螢幕要乘放大倍率）
pub const SPREAD: f32 = 10.0;
const SIZE: u32 = 2048;

/// 一個字在圖集裡的位置與它相對於筆位置的偏移（都是 [`BASE`] 下的 px，含 `SPREAD` 留白）
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub uv: [f32; 4],
    pub w: f32,
    pub h: f32,
    /// 點陣圖左緣離筆位置多遠（往右為正）
    pub left: f32,
    /// 點陣圖上緣離基線多遠（往上為正）
    pub top: f32,
}

pub struct Atlas {
    tex: wgpu::Texture,
    pub view: Arc<wgpu::TextureView>,
    /// `None` 是沒有形狀的字（空白）或點陣化失敗，不要每幀重試
    map: HashMap<CacheKey, Option<Entry>>,
    x: u32,
    y: u32,
    row_h: u32,
    /// 這一幀塞不下了：下一幀開始時整張清掉重來（不能在這一幀清，前面排進去的字還要用）
    full: bool,
}

impl Atlas {
    pub fn new(gpu: &Gpu) -> Self {
        let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pv sdf"),
            size: wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = Arc::new(tex.create_view(&Default::default()));
        Self { tex, view, map: HashMap::new(), x: 0, y: 0, row_h: 0, full: false }
    }

    /// 一幀的開始。上一幀塞滿了就整張重來
    pub fn begin(&mut self) {
        if self.full {
            self.map.clear();
            (self.x, self.y, self.row_h, self.full) = (0, 0, 0, false);
        }
    }

    /// 拿一個字。第一次要的時候才點陣化（`raster` 回傳 BASE 字級下的遮罩）
    pub fn get(
        &mut self,
        gpu: &Gpu,
        key: CacheKey,
        raster: impl FnOnce(CacheKey) -> Option<Mask>,
    ) -> Option<Entry> {
        if let Some(e) = self.map.get(&key) {
            return *e;
        }
        let e = raster(key).and_then(|m| self.insert(gpu, &m));
        // 塞不下的不記：清掉重來之後還要再試
        if e.is_some() || !self.full {
            self.map.insert(key, e);
        }
        e
    }

    fn insert(&mut self, gpu: &Gpu, m: &Mask) -> Option<Entry> {
        if m.w == 0 || m.h == 0 {
            return None;
        }
        let (field, w, h) = sdf(&m.data, m.w, m.h);
        let (w32, h32) = (w as u32, h as u32);
        if self.x + w32 > SIZE {
            (self.x, self.y, self.row_h) = (0, self.y + self.row_h + 1, 0);
        }
        if self.y + h32 > SIZE {
            self.full = true;
            return None;
        }
        let (x, y) = (self.x, self.y);
        self.x += w32 + 1;
        self.row_h = self.row_h.max(h32);
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &field,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w32), rows_per_image: Some(h32) },
            wgpu::Extent3d { width: w32, height: h32, depth_or_array_layers: 1 },
        );
        let s = SIZE as f32;
        let pad = SPREAD.ceil();
        Some(Entry {
            uv: [x as f32 / s, y as f32 / s, (x + w32) as f32 / s, (y + h32) as f32 / s],
            w: w as f32,
            h: h as f32,
            left: m.left as f32 - pad,
            top: m.top as f32 + pad,
        })
    }
}

/// 點陣化出來的覆蓋率遮罩（0..255），`left`／`top` 照 swash 的 placement
pub struct Mask {
    pub data: Vec<u8>,
    pub w: usize,
    pub h: usize,
    pub left: i32,
    pub top: i32,
}

/// 覆蓋率遮罩 → 有號距離場，四周各留 `SPREAD` px。0.5 是字的邊，往上是字裡面。
/// 回傳 (資料, 寬, 高)
pub fn sdf(mask: &[u8], w: usize, h: usize) -> (Vec<u8>, usize, usize) {
    let pad = SPREAD.ceil() as usize;
    let (fw, fh) = (w + 2 * pad, h + 2 * pad);
    let cov = |x: usize, y: usize| -> f32 {
        if x < pad || y < pad || x >= w + pad || y >= h + pad {
            0.0
        } else {
            mask[(y - pad) * w + (x - pad)] as f32 / 255.0
        }
    };
    const INF: f32 = 1e20;
    let mut outside = vec![0.0f32; fw * fh];
    let mut inside = vec![0.0f32; fw * fh];
    for y in 0..fh {
        for x in 0..fw {
            let a = cov(x, y);
            let i = y * fw + x;
            // 到「字裡面」的距離：字裡面的點是 0
            outside[i] = if a >= 0.5 { 0.0 } else { INF };
            inside[i] = if a >= 0.5 { INF } else { 0.0 };
        }
    }
    edt(&mut outside, fw, fh);
    edt(&mut inside, fw, fh);
    let mut out = vec![0u8; fw * fh];
    for y in 0..fh {
        for x in 0..fw {
            let i = y * fw + x;
            let a = cov(x, y);
            // 像素中心到邊界：外面的點離最近的字裡像素中心差半格
            let mut d = if a >= 0.5 { inside[i].sqrt() - 0.5 } else { -(outside[i].sqrt() - 0.5) };
            // 抗鋸齒的邊緣像素直接用覆蓋率，比整數格的距離準
            if a > 0.0 && a < 1.0 {
                d = a - 0.5;
            }
            out[i] = ((0.5 + d / (2.0 * SPREAD)).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    (out, fw, fh)
}

/// 二維平方距離轉換，就地。0 是特徵點，其他給很大的數
fn edt(grid: &mut [f32], w: usize, h: usize) {
    let n = w.max(h);
    let (mut f, mut d, mut v, mut z) = (vec![0.0f32; n], vec![0.0f32; n], vec![0usize; n], vec![0.0f32; n + 1]);
    for x in 0..w {
        for y in 0..h {
            f[y] = grid[y * w + x];
        }
        edt_1d(&f[..h], &mut d[..h], &mut v, &mut z);
        for y in 0..h {
            grid[y * w + x] = d[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&grid[y * w..y * w + w]);
        edt_1d(&f[..w], &mut d[..w], &mut v, &mut z);
        grid[y * w..y * w + w].copy_from_slice(&d[..w]);
    }
}

/// Felzenszwalb & Huttenlocher 的一維下包絡（拋物線）
fn edt_1d(f: &[f32], d: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    let cross = |q: usize, p: usize| ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
    for q in 1..n {
        let mut s = cross(q, v[k]);
        // z[0] 是負無限大，k 不會退到 0 以下
        while s <= z[k] {
            k -= 1;
            s = cross(q, v[k]);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f32::INFINITY;
    }
    k = 0;
    for (q, out) in d.iter_mut().enumerate().take(n) {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        let dq = q as f32 - p as f32;
        *out = dq * dq + f[p];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 方塊的距離場() {
        // 10x10 的實心方塊：中心最深，邊界剛好在 0.5，外面越遠越小
        let (w, h) = (10, 10);
        let mask = vec![255u8; w * h];
        let (f, fw, _) = sdf(&mask, w, h);
        let pad = SPREAD.ceil() as usize;
        let at = |x: usize, y: usize| f[y * fw + x] as f32 / 255.0;
        let c = at(pad + 5, pad + 5);
        assert!(c > 0.7, "中心 {c}");
        // 方塊內緣第一格：離邊半格 → 0.5 + 0.5/20
        let edge_in = at(pad, pad + 5);
        assert!((edge_in - 0.525).abs() < 0.01, "內緣 {edge_in}");
        let edge_out = at(pad - 1, pad + 5);
        assert!((edge_out - 0.475).abs() < 0.01, "外緣 {edge_out}");
        // 最外圈遠在 SPREAD 外 → 0
        assert!(at(0, 0) < 0.02);
    }

    #[test]
    fn 一維距離轉換() {
        let f = [1e20, 1e20, 0.0, 1e20, 1e20, 1e20, 0.0];
        let mut d = [0.0f32; 7];
        let (mut v, mut z) = ([0usize; 7], [0.0f32; 8]);
        edt_1d(&f, &mut d, &mut v, &mut z);
        assert_eq!(d, [4.0, 1.0, 0.0, 1.0, 4.0, 1.0, 0.0]);
    }
}
