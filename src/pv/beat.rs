//! 從 loopback 的頻譜抓節拍。
//!
//! 三步：
//! 1. **起音強度**：每 [`HOP`] 個取樣算一次頻譜通量（對數頻段的正向變化量總和），
//!    低頻另外算一份給「鼓點」用。
//! 2. **速度**：最近幾秒的起音強度做自相關，在 60～200 BPM 裡挑最像週期的那個，
//!    用 120 BPM 附近的對數高斯當先驗（Ellis 2007），不然常常挑到兩倍或一半。
//! 3. **相位**：拿那個週期當梳子，在起音強度上滑，對最多的位置就是拍點。
//!
//! 畫面只拿 [`Pulse`]：一個已知的拍點加週期就能自己往後推，不用每幀問。
//! 鼓點（`hit_at`）另外給，是實際聽到的低頻起音，不是推算的。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 每幀起音強度隔幾個取樣（48 kHz 下約 10.7 ms）
pub const HOP: usize = 512;
const HOP_S: f32 = HOP as f32 / crate::audio::SAMPLE_RATE as f32;
/// 速度估計看多長的歷史（約 6 秒）
const HIST: usize = 560;
/// 多少幀重估一次速度（約 0.27 秒）
const EVERY: u32 = 25;
const BANDS: usize = 48;
/// 低頻（大鼓）算到第幾個頻段（約 150 Hz）
const LOW_BANDS: usize = 9;
/// 音量：平滑的時間常數（秒）、隔幾幀記一筆（約 0.26 秒）、記幾筆（約一分半）、記滿幾筆才算可信（約 10 秒）
const LOUD_TAU: f32 = 1.2;
const LOUD_EVERY: u32 = 24;
const LOUD_HIST: usize = 350;
const LOUD_TRUST: usize = 40;

/// 給畫面的節拍資訊
#[derive(Clone, Copy, Debug, Default)]
pub struct Pulse {
    /// 一拍幾秒，0 是還不知道
    pub period: f32,
    /// 某一個拍點的時刻（往後推 `period` 就是之後的拍）
    pub beat_at: Option<Instant>,
    /// 速度有多可信（0..1）。低的時候畫面不要跟著它跳
    pub conf: f32,
    /// 最近一次鼓點與它的強度（0..1）
    pub hit_at: Option<Instant>,
    pub hit: f32,
    /// 現在這一段有多響（0..1）：最近一秒多的音量，在最近一分半裡排第幾（百分位）。
    /// 比的是同一段時間裡的自己，所以跟音量開多大無關；副歌通常比主歌高
    pub energy: f32,
    /// `energy` 有多可信（0..1）：剛開始聽、或沒聲音的時候是 0
    pub energy_conf: f32,
}

impl Pulse {
    /// `now` 離最近的拍點過了幾秒；不知道就 `None`
    pub fn since_beat(&self, now: Instant) -> Option<f32> {
        let at = self.beat_at?;
        if self.period <= 0.0 || self.conf < 0.15 {
            return None;
        }
        let dt = if now >= at { (now - at).as_secs_f32() } else { -(at - now).as_secs_f32() };
        Some(dt.rem_euclid(self.period))
    }
}

pub struct Tracker {
    prev: [f32; BANDS],
    env: VecDeque<f32>,
    low: VecDeque<f32>,
    frame: u64,
    tick: u32,
    /// 週期（幀，可以有小數）與連續幾次估到「不一樣的」週期（換速度要連三次才換）
    period: f32,
    pending: (f32, u32),
    conf: f32,
    beat_frame: Option<f64>,
    last_hit: u64,
    /// 平滑過的音量與它的歷史（算百分位）
    loud: f32,
    louds: VecDeque<f32>,
    loud_tick: u32,
    /// 百分位每 0.26 秒才算一次；`pulse.energy` 每幀往它靠，畫面才不會一格一格跳
    energy: f32,
    pub pulse: Pulse,
}

impl Default for Tracker {
    fn default() -> Self {
        Self {
            prev: [0.0; BANDS],
            env: VecDeque::with_capacity(HIST + 1),
            low: VecDeque::with_capacity(HIST + 1),
            frame: 0,
            tick: 0,
            period: 0.0,
            pending: (0.0, 0),
            conf: 0.0,
            beat_frame: None,
            last_hit: 0,
            loud: 0.0,
            louds: VecDeque::with_capacity(LOUD_HIST + 1),
            loud_tick: 0,
            energy: 0.0,
            pulse: Pulse::default(),
        }
    }
}

/// 第 `i` 個頻段的 FFT bin 範圍（40 Hz～12 kHz 對數切）
fn band(i: usize, nbins: usize) -> std::ops::Range<usize> {
    let (lo, hi) = (40.0f32, 12_000.0f32);
    let hz = |t: f32| lo * (hi / lo).powf(t);
    let nyq = crate::audio::SAMPLE_RATE as f32 / 2.0;
    let bin = |f: f32| ((f / nyq) * nbins as f32) as usize;
    let a = bin(hz(i as f32 / BANDS as f32)).max(1);
    let b = bin(hz((i + 1) as f32 / BANDS as f32)).max(a + 1).min(nbins);
    a..b
}

impl Tracker {
    /// 餵一幀線性振幅頻譜（realfft 的輸出取絕對值）。`now` 是這一幀最後一個取樣的時刻
    pub fn push(&mut self, mags: &[f32], now: Instant) {
        let mut cur = [0.0f32; BANDS];
        for (i, c) in cur.iter_mut().enumerate() {
            let r = band(i, mags.len());
            let m = mags[r.clone()].iter().sum::<f32>() / r.len() as f32;
            // 對數壓縮：響的歌與小聲的歌通量差不多大
            *c = (1.0 + m).ln();
        }
        let (mut flux, mut low) = (0.0f32, 0.0f32);
        for (i, (c, p)) in cur.iter().zip(&self.prev).enumerate() {
            let d = (c - p).max(0.0);
            flux += d;
            if i < LOW_BANDS {
                low += d;
            }
        }
        self.prev = cur;
        self.loudness(cur.iter().sum::<f32>() / BANDS as f32);
        self.feed(flux, low, now);
    }

    /// 音量（各頻段對數強度的平均）平滑一秒多，在最近一分半裡排第幾
    fn loudness(&mut self, v: f32) {
        let a = 1.0 - (-HOP_S / LOUD_TAU).exp();
        self.loud += (v - self.loud) * a;
        self.loud_tick += 1;
        if self.loud_tick >= LOUD_EVERY {
            self.loud_tick = 0;
            if self.louds.len() == LOUD_HIST {
                self.louds.pop_front();
            }
            self.louds.push_back(self.loud);
            let n = self.louds.len();
            let below = self.louds.iter().filter(|&&x| x < self.loud).count();
            self.energy = below as f32 / (n.max(2) - 1) as f32;
            self.pulse.energy_conf = (n as f32 / LOUD_TRUST as f32).min(1.0);
        }
        self.pulse.energy += (self.energy - self.pulse.energy) * (1.0 - (-HOP_S / 0.5).exp());
    }

    /// 完全沒聲音（loopback 不給資料）：信心慢慢掉，不要讓畫面照舊的速度一直跳
    pub fn silence(&mut self) {
        self.conf *= 0.8;
        self.pulse.conf = self.conf;
        self.pulse.energy_conf *= 0.8;
        self.prev = [0.0; BANDS];
    }

    fn feed(&mut self, flux: f32, low: f32, now: Instant) {
        self.frame += 1;
        if self.env.len() == HIST {
            self.env.pop_front();
            self.low.pop_front();
        }
        self.env.push_back(flux);
        self.low.push_back(low);
        self.pick_hit(now);
        self.tick += 1;
        if self.tick >= EVERY && self.env.len() >= HIST / 2 {
            self.tick = 0;
            self.tempo();
            self.phase();
        }
        let at = |f: f64| {
            let back = (self.frame as f64 - f) * HOP_S as f64;
            if back >= 0.0 { now.checked_sub(Duration::from_secs_f64(back)) } else { Some(now + Duration::from_secs_f64(-back)) }
        };
        self.pulse.period = self.period * HOP_S;
        self.pulse.conf = self.conf;
        self.pulse.beat_at = self.beat_frame.and_then(at);
    }

    /// 低頻的起音：比前後三幀都大、超過最近半秒的平均加 1.5 個標準差、離上一個至少 120 ms
    fn pick_hit(&mut self, now: Instant) {
        let n = self.low.len();
        if n < 50 {
            return;
        }
        let c = n - 4;
        let v = self.low[c];
        if (c - 3..=c + 3).any(|j| j != c && self.low[j] > v) {
            return;
        }
        let win = self.low.range(n - 50..).copied();
        let mean = win.clone().sum::<f32>() / 50.0;
        let var = win.map(|x| (x - mean) * (x - mean)).sum::<f32>() / 50.0;
        let sd = var.sqrt().max(1e-4);
        let peak_frame = self.frame - 3;
        if v > mean + 1.5 * sd && peak_frame - self.last_hit > (0.12 / HOP_S) as u64 {
            self.last_hit = peak_frame;
            self.pulse.hit = ((v - mean) / (4.0 * sd)).clamp(0.2, 1.0);
            self.pulse.hit_at = now.checked_sub(Duration::from_secs_f32(3.0 * HOP_S));
        }
    }

    fn tempo(&mut self) {
        let n = self.env.len();
        let mean = self.env.iter().sum::<f32>() / n as f32;
        // 先抹平一點（前後兩幀）：週期不是整數幀時，脈衝間距在兩個整數之間跳，
        // 沒抹平的話正確週期的峰被打散，兩倍週期反而贏（150 BPM 會估成 75）
        const K: [f32; 5] = [1.0, 2.0, 3.0, 2.0, 1.0];
        let e: Vec<f32> = (0..n)
            .map(|t| {
                let (mut s, mut wsum) = (0.0, 0.0);
                for (j, kw) in K.iter().enumerate() {
                    if let Some(v) = (t + j).checked_sub(2).and_then(|i| self.env.get(i)) {
                        s += kw * v;
                        wsum += kw;
                    }
                }
                s / wsum - mean
            })
            .collect();
        let r0: f32 = e.iter().map(|x| x * x).sum::<f32>().max(1e-9);
        let lag_of = |bpm: f32| 60.0 / bpm / HOP_S;
        let (lo, hi) = (lag_of(200.0).floor() as usize, lag_of(60.0).ceil() as usize);
        let ac = |l: usize| -> f32 { (l..n).map(|t| e[t] * e[t - l]).sum::<f32>() / r0 };
        let l120 = lag_of(120.0);
        let mut best = (0usize, f32::MIN, 0.0f32);
        let mut acs = vec![0.0f32; hi + 2];
        for (l, a) in acs.iter_mut().enumerate().take(hi + 2).skip(lo.saturating_sub(1)) {
            *a = ac(l);
        }
        for (l, &a) in acs.iter().enumerate().take(hi + 1).skip(lo) {
            let w = (-0.5 * ((l as f32 / l120).log2() / 0.9).powi(2)).exp();
            let s = a * w;
            if s > best.1 {
                best = (l, s, a);
            }
        }
        let (l, _, raw) = best;
        if l == 0 {
            return;
        }
        // 拋物線內插出小數週期
        let (a, b, c) = (acs[l - 1], acs[l], acs[l + 1]);
        let den = a - 2.0 * b + c;
        let frac = if den.abs() > 1e-9 { (0.5 * (a - c) / den).clamp(-0.5, 0.5) } else { 0.0 };
        let p = l as f32 + frac;
        let conf = (raw * 2.5).clamp(0.0, 1.0);
        if self.period == 0.0 || (p - self.period).abs() / self.period < 0.06 {
            self.period = if self.period == 0.0 { p } else { self.period * 0.8 + p * 0.2 };
            self.pending = (0.0, 0);
        } else if (p - self.pending.0).abs() / p < 0.06 {
            self.pending.1 += 1;
            if self.pending.1 >= 3 {
                self.period = p;
                self.beat_frame = None;
                self.pending = (0.0, 0);
            }
        } else {
            self.pending = (p, 1);
        }
        self.conf = self.conf * 0.6 + conf * 0.4;
    }

    /// 拿週期當梳子在起音強度上滑，找最近的拍點
    fn phase(&mut self) {
        let p = self.period;
        if p < 2.0 {
            return;
        }
        let n = self.env.len();
        let pi = p.round() as usize;
        let teeth = ((n as f32 / p) as usize).clamp(1, 8);
        let mut best = (0usize, f32::MIN);
        for off in 0..pi {
            let mut s = 0.0;
            for k in 0..teeth {
                let at = off as f32 + k as f32 * p;
                let i = at.round() as usize;
                if i + 1 >= n {
                    break;
                }
                // 往回數：0 是最新的一幀。旁邊一格也算一點，抖動不要整個錯過
                let j = n - 1 - i;
                s += self.env[j] + 0.5 * self.env[j.saturating_sub(1)].max(self.env[(j + 1).min(n - 1)]);
            }
            if s > best.1 {
                best = (off, s);
            }
        }
        let found = self.frame as f64 - best.0 as f64;
        // 跟原本推算的差不多就慢慢拉過去，不要每次重估都跳
        self.beat_frame = Some(match self.beat_frame {
            Some(old) => {
                let k = ((found - old) / p as f64).round();
                let pred = old + k * p as f64;
                if (found - pred).abs() < p as f64 * 0.2 { pred + (found - pred) * 0.3 } else { found }
            }
            None => found,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成的頻譜：每拍低頻與全頻一下，中間只有一點雜音
    fn run(bpm: f32, secs: f32) -> (Tracker, Instant) {
        let mut t = Tracker::default();
        let t0 = Instant::now();
        let period = 60.0 / bpm;
        let frames = (secs / HOP_S) as usize;
        let mut seed = 12345u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 16) as f32 / 65536.0
        };
        for f in 0..frames {
            let time = f as f32 * HOP_S;
            let ph = (time / period).fract() * period;
            let hit = ph < HOP_S;
            let mags: Vec<f32> = (0..1025)
                .map(|b| {
                    let base = 0.2 + 0.1 * rnd();
                    if hit { base + if b < 10 { 80.0 } else { 6.0 } } else { base }
                })
                .collect();
            // 這一幀最後一個取樣的時刻
            t.push(&mags, t0 + Duration::from_secs_f32(time + HOP_S));
        }
        (t, t0)
    }

    #[test]
    fn 抓得到速度() {
        for bpm in [92.0, 128.0, 150.0] {
            let (t, _) = run(bpm, 12.0);
            let got = 60.0 / t.pulse.period;
            assert!((got - bpm).abs() / bpm < 0.03, "{bpm} BPM 估成 {got}");
            assert!(t.pulse.conf > 0.3, "信心 {}", t.pulse.conf);
        }
    }

    #[test]
    fn 響的段落能量高() {
        // 小聲 20 秒、大聲 8 秒：大聲那段排在最前面，而且可信
        let mut t = Tracker::default();
        let t0 = Instant::now();
        let frames = (28.0 / HOP_S) as usize;
        for f in 0..frames {
            let time = f as f32 * HOP_S;
            let m = if time < 20.0 { 0.5 } else { 4.0 };
            let mags = vec![m; 1025];
            t.push(&mags, t0 + Duration::from_secs_f32(time));
        }
        assert!(t.pulse.energy > 0.9, "大聲時 {}", t.pulse.energy);
        assert!(t.pulse.energy_conf > 0.99);
        t.silence();
        assert!(t.pulse.energy_conf < 0.9);
    }

    #[test]
    fn 拍點對得上() {
        let (t, t0) = run(120.0, 10.0);
        // 推算的拍點離真的拍點（0.5 秒的整數倍）不超過兩幀
        let at = (t.pulse.beat_at.unwrap() - t0).as_secs_f32();
        let off = at.rem_euclid(0.5);
        let err = off.min(0.5 - off);
        assert!(err < 2.5 * HOP_S, "拍點偏了 {err} 秒");
        // 鼓點也抓得到，時間一樣對得上
        let hit = (t.pulse.hit_at.unwrap() - t0).as_secs_f32().rem_euclid(0.5);
        assert!(hit.min(0.5 - hit) < 2.5 * HOP_S, "鼓點偏了 {hit} 秒");
    }
}
