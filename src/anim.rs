//! 動畫的小工具：CSS 的 `cubic-bezier()`、延遲＋時長換成 0..1 的進度、補間。
//!
//! Python 版的動畫全是 CSS 與 Web Animations，曲線都寫成 `cubic-bezier(a,b,c,d)`。
//! 這裡照同一組數字算，兩邊的手感才對得起來。

use std::time::Instant;

/// CSS 的 `cubic-bezier(x1, y1, x2, y2)`。
#[derive(Clone, Copy, Debug)]
pub struct Ease(pub f32, pub f32, pub f32, pub f32);

/// `ease`
pub const EASE: Ease = Ease(0.25, 0.1, 0.25, 1.0);
/// `ease-in`
pub const EASE_IN: Ease = Ease(0.42, 0.0, 1.0, 1.0);
/// 視圖轉場的替身（app.js 的 `EASE`）
pub const VIEW: Ease = Ease(0.45, 0.02, 0.2, 1.0);
/// 浮上來、長出來（rise-in、disc-grow、swapFlow 進場）
pub const OUT: Ease = Ease(0.16, 0.84, 0.44, 1.0);
/// 歌詞捲動、KTV 換句、幽靈換圖
pub const SOFT: Ease = Ease(0.22, 0.61, 0.36, 1.0);
/// Material 的標準曲線（播放列收下去）
pub const STD: Ease = Ease(0.4, 0.0, 0.2, 1.0);
/// 會超過一點再回來的「液態」回彈（glass-in）
pub const SPRING: Ease = Ease(0.32, 1.18, 0.44, 1.0);
/// 選單從按鈕長出來：起步不衝，看得到長的過程，最後才微微回彈
pub const GROW: Ease = Ease(0.4, 0.3, 0.3, 1.1);

impl Ease {
    /// `t` 是時間進度 0..1，回傳曲線上的值（回彈曲線會短暫超過 1）。
    pub fn at(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        if t == 0.0 || t == 1.0 {
            return t;
        }
        let Ease(x1, y1, x2, y2) = self;
        let bez = |a: f32, b: f32, s: f32| {
            let u = 1.0 - s;
            3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
        };
        // 先用牛頓法從 x 反解參數 s，收不了再二分
        let mut s = t;
        for _ in 0..8 {
            let x = bez(x1, x2, s) - t;
            let u = 1.0 - s;
            let dx = 3.0 * u * u * x1 + 6.0 * u * s * (x2 - x1) + 3.0 * s * s * (1.0 - x2);
            if x.abs() < 1e-5 {
                return bez(y1, y2, s);
            }
            if dx.abs() < 1e-6 {
                break;
            }
            s = (s - x / dx).clamp(0.0, 1.0);
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..30 {
            s = (lo + hi) * 0.5;
            if bez(x1, x2, s) < t {
                lo = s;
            } else {
                hi = s;
            }
        }
        bez(y1, y2, s)
    }
}

/// `animation: X dur ease delay both` 在經過 `t` 秒時的進度（0..1，還沒開始是 0、結束後是 1）。
pub fn phase(t: f32, delay: f32, dur: f32) -> f32 {
    if dur <= 0.0 {
        return if t >= delay { 1.0 } else { 0.0 };
    }
    ((t - delay) / dur).clamp(0.0, 1.0)
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// 從目前的值補到新目標，花固定時間、走指定曲線。中途換目標就從**當下的值**重新出發，
/// 跟 CSS transition 被打斷時的行為一樣，不會跳。
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    dur: f32,
    ease: Ease,
}

impl Tween {
    pub fn new(v: f32, dur: f32, ease: Ease) -> Self {
        Self { from: v, to: v, start: Instant::now(), dur, ease }
    }

    pub fn value(&self) -> f32 {
        let t = self.start.elapsed().as_secs_f32();
        lerp(self.from, self.to, self.ease.at(phase(t, 0.0, self.dur)))
    }

    pub fn target(&self) -> f32 {
        self.to
    }

    pub fn set(&mut self, to: f32) {
        if to == self.to {
            return;
        }
        self.from = self.value();
        self.to = to;
        self.start = Instant::now();
    }

    /// 同 `set`，但換目標時順便換時長與曲線（例如打開慢、有回彈，收回去快、沒有回彈）
    pub fn set_eased(&mut self, to: f32, dur: f32, ease: Ease) {
        if to != self.to {
            self.set(to);
            self.dur = dur;
            self.ease = ease;
        }
    }

    /// 直接跳到 `v`，不補間
    pub fn jump(&mut self, v: f32) {
        self.from = v;
        self.to = v;
    }

    #[allow(dead_code)]
    pub fn moving(&self) -> bool {
        self.start.elapsed().as_secs_f32() < self.dur && self.from != self.to
    }
}

/// CSS 的 `glass-in`：0% 透明、縮 0.93 → 62% 不透明、1.018 → 100% 原大，每段各自套回彈曲線。
/// 回傳 (縮放, 透明度)。
pub fn glass_in(t: f32) -> (f32, f32) {
    let p = t / 0.42;
    if p >= 1.0 {
        return (1.0, 1.0);
    }
    if p < 0.62 {
        let k = SPRING.at(p / 0.62);
        (lerp(0.93, 1.018, k), k.clamp(0.0, 1.0))
    } else {
        (lerp(1.018, 1.0, SPRING.at((p - 0.62) / 0.38)), 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 曲線端點與單調() {
        for e in [EASE, EASE_IN, VIEW, OUT, SOFT, STD] {
            assert_eq!(e.at(0.0), 0.0);
            assert_eq!(e.at(1.0), 1.0);
            let mut last = 0.0;
            for i in 1..100 {
                let v = e.at(i as f32 / 100.0);
                assert!(v >= last - 1e-4, "{e:?} 在 {i} 倒退");
                last = v;
            }
        }
    }

    #[test]
    fn 對照瀏覽器的數值() {
        // 線性 bezier 就是恆等
        let lin = Ease(0.0, 0.0, 1.0, 1.0);
        assert!((lin.at(0.3) - 0.3).abs() < 1e-3);
        // ease 在 0.5 約 0.8024（Chrome 的實作值）
        assert!((EASE.at(0.5) - 0.8024).abs() < 2e-3, "{}", EASE.at(0.5));
        // 回彈曲線中段要超過 1
        assert!((0..100).any(|i| SPRING.at(i as f32 / 100.0) > 1.0));
    }

    #[test]
    fn 延遲與時長() {
        assert_eq!(phase(0.05, 0.1, 0.2), 0.0);
        assert!((phase(0.2, 0.1, 0.2) - 0.5).abs() < 1e-6);
        assert_eq!(phase(1.0, 0.1, 0.2), 1.0);
    }
}
