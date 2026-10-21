//! Open-Unmix（umxhq）的人聲模型，只用來算「這一刻有沒有人在唱」。
//!
//! 權重是官方發在 Zenodo 的 `vocals-b62c91ce.pth`（MIT），第一次用時下載、驗 md5。
//! 它是舊式 torch 存檔（pickle + 原始資料），不解 pickle：檔案固定，每個張量在第幾個位元組
//! 是離線量好的（`AlignLab\umx_offsets.py`），照表讀 f32 就好。
//!
//! 跟原版的差別都是為了省事，離線對 35 首驗過結果不變（`AlignLab\proto_rs.py`）：
//! - 不重取樣：48 kHz 直接用 4458 點的 STFT，頻率格距 10.767 Hz 跟 44.1 kHz／4096 點一樣；
//! - 不還原波形：只要人聲幅度在 [`BAND`] 的能量，最後一層也只算這些頻率格。

use std::path::Path;

use anyhow::{Context, bail};

pub const URL: &str = "https://zenodo.org/records/3370489/files/vocals-b62c91ce.pth";
pub const MD5: &str = "d918985fad0fedf6d9ce89e279aa7218";
pub const SIZE: u64 = 35_637_796;

/// 48 kHz 下等效 44.1 kHz／4096 點的視窗與 1024 的步距
pub const NFFT: usize = 4458;
pub const HOP: usize = 1115;
const BINS: usize = 2049;
const MAX_BIN: usize = 1487;
const H: usize = 512;
const HH: usize = 256;
/// 算能量的頻率格（150～5000 Hz）
pub const BAND: (usize, usize) = (13, 465);
const NB: usize = BAND.1 - BAND.0;

/// 一層單向 LSTM。`b` 是 bias_ih + bias_hh
struct Lstm {
    wih: Vec<f32>,
    whh: Vec<f32>,
    b: Vec<f32>,
}

pub struct Umx {
    in_mean: Vec<f32>,
    in_scale: Vec<f32>,
    fc1: Vec<f32>,
    /// BatchNorm 折成 y = x·s + t
    bn1: (Vec<f32>, Vec<f32>),
    /// [層][0 正向／1 反向]
    lstm: Vec<[Lstm; 2]>,
    fc2: Vec<f32>,
    bn2: (Vec<f32>, Vec<f32>),
    /// 只留 [`BAND`] 的列：兩個聲道 × NB
    fc3: Vec<f32>,
    bn3: (Vec<f32>, Vec<f32>),
    out_scale: Vec<f32>,
    out_mean: Vec<f32>,
}

/// (名稱, 位元組位置, 元素數)
const TABLE: &[(&str, usize, usize)] = &[
    ("input_mean", 27216756, 1487),
    ("input_scale", 27222712, 1487),
    ("output_scale", 27228668, 2049),
    ("output_mean", 27236872, 2049),
    ("fc1.weight", 21060380, 512 * 2974),
    ("bn1.weight", 21031616, 512),
    ("bn1.bias", 6632, 512),
    ("bn1.running_mean", 21050068, 512),
    ("bn1.running_var", 5263904, 512),
    ("lstm.weight_ih_l0", 10529392, 1024 * 512),
    ("lstm.weight_hh_l0", 17881768, 1024 * 256),
    ("lstm.bias_ih_l0", 18930352, 1024),
    ("lstm.bias_hh_l0", 13679240, 1024),
    ("lstm.weight_ih_l0_reverse", 13683344, 1024 * 512),
    ("lstm.weight_hh_l0_reverse", 12626552, 1024 * 256),
    ("lstm.bias_ih_l0_reverse", 13675136, 1024),
    ("lstm.bias_hh_l0_reverse", 1061376, 1024),
    ("lstm.weight_ih_l1", 1065480, 1024 * 512),
    ("lstm.weight_hh_l1", 8688, 1024 * 256),
    ("lstm.bias_ih_l1", 1057272, 1024),
    ("lstm.bias_hh_l1", 3162640, 1024),
    ("lstm.weight_ih_l1_reverse", 3166744, 1024 * 512),
    ("lstm.weight_hh_l1_reverse", 7367224, 1024 * 256),
    ("lstm.bias_ih_l1_reverse", 8415808, 1024),
    ("lstm.bias_hh_l1_reverse", 5265960, 1024),
    ("lstm.weight_ih_l2", 5270064, 1024 * 512),
    ("lstm.weight_hh_l2", 9476704, 1024 * 256),
    ("lstm.bias_ih_l2", 10525288, 1024),
    ("lstm.bias_hh_l2", 15780504, 1024),
    ("lstm.weight_ih_l2_reverse", 15784608, 1024 * 512),
    ("lstm.weight_hh_l2_reverse", 8419912, 1024 * 256),
    ("lstm.bias_ih_l2_reverse", 9468496, 1024),
    ("lstm.bias_hh_l2_reverse", 9472600, 1024),
    ("fc2.weight", 18934456, 512 * 1024),
    ("bn2.weight", 21056268, 512),
    ("bn2.bias", 21052156, 512),
    ("bn2.running_mean", 21058324, 512),
    ("bn2.running_var", 21054212, 512),
    ("fc3.weight", 27245092, 4098 * 512),
    ("bn3.weight", 27151140, 4098),
    ("bn3.bias", 27167540, 4098),
    ("bn3.running_mean", 27183940, 4098),
    ("bn3.running_var", 27200340, 4098),
];

pub fn md5_hex(bytes: &[u8]) -> String {
    use md5::Digest;
    md5::Md5::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// BatchNorm（推論）折成乘一個數再加一個數
fn fold_bn(w: &[f32], b: &[f32], mean: &[f32], var: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let s: Vec<f32> = w.iter().zip(var).map(|(w, v)| w / (v + 1e-5).sqrt()).collect();
    let t = b.iter().zip(mean).zip(&s).map(|((b, m), s)| b - m * s).collect();
    (s, t)
}

/// C (m×n) = A (m×k，列優先) · Wᵀ，W 是 torch 的 (n×k)
fn matmul_t(a: &[f32], w: &[f32], m: usize, k: usize, n: usize) -> Vec<f32> {
    let mut c = vec![0f32; m * n];
    // SAFETY: 三塊記憶體的長度都照 m、k、n 配好，步距描述的就是它們的列優先排法
    unsafe {
        matrixmultiply::sgemm(
            m, k, n, 1.0,
            a.as_ptr(), k as isize, 1,
            w.as_ptr(), 1, k as isize,
            0.0,
            c.as_mut_ptr(), n as isize, 1,
        );
    }
    c
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

impl Umx {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read(path).with_context(|| format!("讀不到 {}", path.display()))?;
        if raw.len() as u64 != SIZE || md5_hex(&raw) != MD5 {
            bail!("模型檔不對（大小或 md5 不符）");
        }
        Ok(Self::from_bytes(&raw))
    }

    fn from_bytes(raw: &[u8]) -> Self {
        let t = |name: &str| -> Vec<f32> {
            let &(_, off, n) = TABLE.iter().find(|e| e.0 == name).expect("表裡沒有這個張量");
            raw[off..off + n * 4].chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
        };
        let bn = |p: &str| fold_bn(&t(&format!("{p}.weight")), &t(&format!("{p}.bias")), &t(&format!("{p}.running_mean")), &t(&format!("{p}.running_var")));
        let cell = |l: usize, rev: bool| {
            let sfx = if rev { "_reverse" } else { "" };
            let bi = t(&format!("lstm.bias_ih_l{l}{sfx}"));
            let bh = t(&format!("lstm.bias_hh_l{l}{sfx}"));
            Lstm {
                wih: t(&format!("lstm.weight_ih_l{l}{sfx}")),
                whh: t(&format!("lstm.weight_hh_l{l}{sfx}")),
                b: bi.iter().zip(&bh).map(|(a, b)| a + b).collect(),
            }
        };
        // 最後一層只留要的頻率格：輸出第 c·2049 + k 列
        let rows: Vec<usize> = (0..2).flat_map(|c| (BAND.0..BAND.1).map(move |k| c * BINS + k)).collect();
        let fc3_all = t("fc3.weight");
        let (s3, t3) = bn("bn3");
        Self {
            in_mean: t("input_mean"),
            in_scale: t("input_scale"),
            fc1: t("fc1.weight"),
            bn1: bn("bn1"),
            lstm: (0..3).map(|l| [cell(l, false), cell(l, true)]).collect(),
            fc2: t("fc2.weight"),
            bn2: bn("bn2"),
            fc3: rows.iter().flat_map(|&r| fc3_all[r * H..(r + 1) * H].iter().copied()).collect(),
            bn3: (rows.iter().map(|&r| s3[r]).collect(), rows.iter().map(|&r| t3[r]).collect()),
            out_scale: t("output_scale"),
            out_mean: t("output_mean"),
        }
    }

    /// `mag`：每格 2 聲道 × 2049 的幅度（[`spectrogram`] 的輸出）。回傳每格人聲在 [`BAND`] 的能量
    pub fn vocal_energy(&self, mag: &[f32]) -> Vec<f32> {
        let n = mag.len() / (2 * BINS);
        if n == 0 {
            return Vec::new();
        }
        // 輸入：只取前 MAX_BIN 格，正規化
        let mut x = vec![0f32; n * 2 * MAX_BIN];
        for f in 0..n {
            for c in 0..2 {
                let src = &mag[(f * 2 + c) * BINS..][..MAX_BIN];
                let dst = &mut x[(f * 2 + c) * MAX_BIN..][..MAX_BIN];
                for k in 0..MAX_BIN {
                    dst[k] = (src[k] + self.in_mean[k]) * self.in_scale[k];
                }
            }
        }
        let mut h1 = matmul_t(&x, &self.fc1, n, 2 * MAX_BIN, H);
        for row in h1.chunks_exact_mut(H) {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (*v * self.bn1.0[j] + self.bn1.1[j]).tanh();
            }
        }
        // 三層雙向 LSTM
        let mut cur = h1.clone();
        for layer in &self.lstm {
            let mut out = vec![0f32; n * H];
            for (d, cell) in layer.iter().enumerate() {
                let g = matmul_t(&cur, &cell.wih, n, H, 4 * HH);
                let (mut h, mut c) = (vec![0f32; HH], vec![0f32; HH]);
                let mut gates = vec![0f32; 4 * HH];
                for step in 0..n {
                    let f = if d == 0 { step } else { n - 1 - step };
                    let gi = &g[f * 4 * HH..][..4 * HH];
                    for (r, gv) in gates.iter_mut().enumerate() {
                        let w = &cell.whh[r * HH..][..HH];
                        let mut s = 0f32;
                        for k in 0..HH {
                            s += w[k] * h[k];
                        }
                        *gv = gi[r] + cell.b[r] + s;
                    }
                    // torch 的閘門順序：i, f, g, o
                    for j in 0..HH {
                        let i = sigmoid(gates[j]);
                        let fg = sigmoid(gates[HH + j]);
                        let gg = gates[2 * HH + j].tanh();
                        let o = sigmoid(gates[3 * HH + j]);
                        c[j] = fg * c[j] + i * gg;
                        h[j] = o * c[j].tanh();
                    }
                    out[f * H + d * HH..][..HH].copy_from_slice(&h);
                }
            }
            cur = out;
        }
        // 跳接：[h1, lstm] → fc2
        let mut cat = vec![0f32; n * 2 * H];
        for f in 0..n {
            cat[f * 2 * H..][..H].copy_from_slice(&h1[f * H..][..H]);
            cat[f * 2 * H + H..][..H].copy_from_slice(&cur[f * H..][..H]);
        }
        let mut h2 = matmul_t(&cat, &self.fc2, n, 2 * H, H);
        for row in h2.chunks_exact_mut(H) {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (*v * self.bn2.0[j] + self.bn2.1[j]).max(0.0);
            }
        }
        let y = matmul_t(&h2, &self.fc3, n, H, 2 * NB);
        (0..n)
            .map(|f| {
                let mut e = 0f32;
                for c in 0..2 {
                    for i in 0..NB {
                        let r = c * NB + i;
                        let k = BAND.0 + i;
                        let m = (y[f * 2 * NB + r] * self.bn3.0[r] + self.bn3.1[r]) * self.out_scale[k] + self.out_mean[k];
                        let v = m.max(0.0) * mag[(f * 2 + c) * BINS + k];
                        e += v * v;
                    }
                }
                e
            })
            .collect()
    }
}

/// 48 kHz 立體聲交錯取樣 → 每格 2 聲道 × 2049 的幅度。第 j 格涵蓋取樣 [j·HOP, j·HOP + NFFT)
pub fn spectrogram(stereo: &[f32]) -> Vec<f32> {
    let frames = stereo.len() / 2;
    if frames < NFFT {
        return Vec::new();
    }
    let n = (frames - NFFT) / HOP + 1;
    let mut planner = realfft::RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(NFFT);
    let mut buf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    // 週期 Hann（torch.hann_window 預設），幅度換算回 4096 點的大小
    let win: Vec<f32> = (0..NFFT).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / NFFT as f32).cos()).collect();
    let scale = 4096.0 / NFFT as f32;
    let mut out = vec![0f32; n * 2 * BINS];
    for f in 0..n {
        for c in 0..2 {
            for (i, b) in buf.iter_mut().enumerate() {
                *b = stereo[(f * HOP + i) * 2 + c] * win[i];
            }
            if fft.process_with_scratch(&mut buf, &mut spec, &mut scratch).is_err() {
                continue;
            }
            let dst = &mut out[(f * 2 + c) * BINS..][..BINS];
            for (d, s) in dst.iter_mut().zip(&spec) {
                *d = s.norm() * scale;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_f32(p: &std::path::Path) -> Vec<f32> {
        std::fs::read(p).unwrap().chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    }

    /// 跟 torch 對答案：先跑 AlignLab\umx_ref.py，`VITRO_UMX_REF` 指到它的輸出資料夾，
    /// `VITRO_UMX_MODEL` 指到 vocals-b62c91ce.pth。沒設就跳過
    #[test]
    fn 跟_torch_算的一樣() {
        let (Some(dir), Some(model)) = (std::env::var_os("VITRO_UMX_REF"), std::env::var_os("VITRO_UMX_MODEL")) else { return };
        let dir = std::path::PathBuf::from(dir);
        let x = read_f32(&dir.join("umx_ref_in.f32"));
        let want_spec = read_f32(&dir.join("umx_ref_spec.f32"));
        let want = read_f32(&dir.join("umx_ref_out.f32"));
        let spec = spectrogram(&x);
        assert_eq!(spec.len(), want_spec.len());
        let worst = spec.iter().zip(&want_spec).map(|(a, b)| (a - b).abs() / (b.abs() + 1.0)).fold(0f32, f32::max);
        assert!(worst < 1e-3, "頻譜差 {worst}");
        let m = Umx::load(std::path::Path::new(&model)).unwrap();
        let t = std::time::Instant::now();
        let got = m.vocal_energy(&spec);
        eprintln!("{} 格（{:.1} 秒的聲音）算了 {:?}", got.len(), x.len() as f32 / 96000.0, t.elapsed());
        assert_eq!(got.len(), want.len());
        // 能量差在 dB 上比
        let db = |v: f32| 10.0 * (v + 1e-6).log10();
        let worst = got.iter().zip(&want).map(|(a, b)| (db(*a) - db(*b)).abs()).fold(0f32, f32::max);
        assert!(worst < 0.1, "能量差 {worst} dB");
    }
}
