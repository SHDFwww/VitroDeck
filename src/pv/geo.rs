//! 文字PV_v2 的背景幾何（設定「背景幾何圖形」）：十幾個很淡的圓、方、三角、叉
//! 散在畫面上，各自慢慢漂、轉一圈，大小跟著一個頻段起伏 —— 圓跟低音、方跟中低、三角跟中、叉跟高音。
//!
//! 起伏的換算：頻段次方壓縮、過一個彈簧、映到 0.95～1.45 倍（[`scale_of`]）。
//! 拿錄音實際算過，主要是跟著音量大小（低音的圓大多在 1.2～1.4 倍），拍點只再多幾個百分比，不會一直跳。
//! 擺法照曲目的種子，同一首每次一樣；顏色是夜色底同色相、亮一點的灰藍，疊上去只比底亮一點點。不跟鏡頭動。

use std::f32::consts::TAU;
use std::time::Instant;

use super::layout::Rng;
use super::style::Style;
use super::{tint, Inst};

/// 最多幾個：16:9 的畫面放 15 個，寬螢幕照比例多放，密度一樣
const MAX: usize = 24;
/// 各頻段的次方壓縮：低音、中低、中、人聲、高音
const BOOST: [f32; 5] = [1.8, 2.0, 2.0, 1.5, 2.0];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Circle,
    Square,
    Triangle,
    Cross,
}

impl Kind {
    /// 跟哪個頻段（`audio::PV_BANDS` 的順序）
    fn band(self) -> usize {
        match self {
            Kind::Circle => 0,
            Kind::Square => 1,
            Kind::Triangle => 2,
            Kind::Cross => 4,
        }
    }
}

#[derive(Debug, PartialEq)]
struct Shape {
    kind: Kind,
    /// 中心在自己那一格的哪裡（0..1）、邊長（畫面高的幾成）。畫面照數量切成差不多正方的格子、
    /// 一格放一個（[`grid`]），整個畫面隨便撒會擠成一團或空一大塊
    x: f32,
    y: f32,
    size: f32,
    /// 漂一個來回、轉一圈要幾秒，從這一圈的哪裡開始（0..1）
    period: f32,
    phase: f32,
    alpha: f32,
    /// 往哪邊漂（±1）、一開始轉了多少（弧度）
    dir: f32,
    rot: f32,
    /// 圓、方有三成是實心，其他是細線框；三角、叉都是實心
    filled: bool,
}

/// 排好的形狀與各頻段的彈簧
#[derive(Default)]
pub struct Geo {
    seed: Option<u64>,
    shapes: Vec<Shape>,
    /// 彈簧現在的值、速度，上一幀的時刻（剛度 300、阻尼 30）
    pos: [f32; 5],
    vel: [f32; 5],
    at: Option<Instant>,
}

/// 彈簧之後的值換成大小倍率：0..255 的 10..200 映到 0.95..1.45，超過就停在兩頭
fn scale_of(v: f32) -> f32 {
    0.95 + 0.5 * ((v * 255.0 - 10.0) / 190.0).clamp(0.0, 1.0)
}

/// `n` 個放進寬高比 `aspect` 的畫面：(幾欄, 幾列)，格子差不多正方、放得下
fn grid(n: usize, aspect: f32) -> (usize, usize) {
    let cols = ((n as f32 * aspect).sqrt().round() as usize).clamp(1, n.max(1));
    (cols, n.div_ceil(cols))
}

impl Geo {
    fn lay_out(&mut self, seed: u64) {
        let mut r = Rng::new(seed ^ 0x6E0_5EED);
        self.shapes = (0..MAX)
            .map(|_| {
                let kind = [Kind::Circle, Kind::Square, Kind::Triangle, Kind::Cross][(r.f() * 4.0) as usize % 4];
                let fill = r.chance(0.3);
                Shape {
                    kind,
                    x: r.range(0.1, 0.9),
                    y: r.range(0.1, 0.9),
                    size: r.range(0.035, 0.13),
                    period: r.range(30.0, 60.0),
                    phase: r.f(),
                    alpha: r.range(0.11, 0.19),
                    dir: r.sign(),
                    rot: r.range(0.0, TAU),
                    filled: fill || matches!(kind, Kind::Triangle | Kind::Cross),
                }
            })
            .collect();
        self.seed = Some(seed);
    }

    /// 各頻段過完彈簧的值（0..1，已次方壓縮）。第一幀、隔太久沒畫就直接到位
    fn spring(&mut self, bands: &[f32; 5], now: Instant) -> [f32; 5] {
        let want: [f32; 5] = std::array::from_fn(|i| bands[i].clamp(0.0, 1.0).powf(BOOST[i]));
        let dt = self.at.map(|t| now.saturating_duration_since(t).as_secs_f32());
        self.at = Some(now);
        match dt {
            Some(dt) if dt <= 0.25 => {
                // 半隱式歐拉，一步不超過 8 ms 才穩
                let n = (dt / 0.008).ceil().max(1.0);
                let h = dt / n;
                for _ in 0..n as usize {
                    for ((p, v), w) in self.pos.iter_mut().zip(&mut self.vel).zip(want) {
                        *v += (300.0 * (w - *p) - 30.0 * *v) * h;
                        *p += *v * h;
                    }
                }
            }
            _ => {
                self.pos = want;
                self.vel = [0.0; 5];
            }
        }
        self.pos
    }

    /// 畫進 `out`（墊底上面、流光底下）。`seed` 是曲目的種子，`bands` 是 `audio::Spectrum::bands`，
    /// `ms` 是歌詞時間（漂與轉跟著它，暫停就停）
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw(&mut self, out: &mut Vec<Inst>, seed: u64, bands: &[f32; 5], now: Instant, ms: i64, st: &Style, w: f32, h: f32) {
        if self.seed != Some(seed) {
            self.lay_out(seed);
        }
        let lv = self.spring(bands, now);
        // 夜色底的色相、亮到 0.5，彩度比底多一點。疊上去跟底差 0.035～0.06 的亮度（OKLab），
        // 取 0.04 上下，寧可淡一點
        let p = tint::lab([st.bg[0], st.bg[1], st.bg[2]]);
        let [r, g, b] = tint::lch(0.5, (p[1].hypot(p[2]) * 1.3).min(0.07), p[2].atan2(p[1]));
        let n = ((w / h) / (16.0 / 9.0) * 15.0).round().clamp(8.0, MAX as f32) as usize;
        let (cols, rows) = grid(n, w / h);
        let t = ms as f32 / 1000.0;
        // 位移與線寬以 1080 高為準換過來
        let px = h / 1080.0;
        for (i, s) in self.shapes.iter().take(n).enumerate() {
            let q = (t / s.period + s.phase).rem_euclid(1.0);
            // 斜斜地漂過去再漂回來（±15、∓30 px），同時轉一圈
            let u = 1.0 - (2.0 * q - 1.0).abs();
            let cx = ((i % cols) as f32 + s.x) / cols as f32 * w + s.dir * (30.0 * u - 15.0) * px;
            let cy = ((i / cols) as f32 + s.y) / rows as f32 * h + s.dir * (30.0 - 60.0 * u) * px;
            let rot = s.rot + TAU * q;
            let size = s.size * h * scale_of(lv[s.kind.band()]);
            let col = [r, g, b, s.alpha];
            let line = px.max(1.0);
            let inst = match (s.kind, s.filled) {
                (Kind::Circle, true) => Inst::disk(cx, cy, size * 0.5, col, (0.0, 0.0)),
                (Kind::Circle, false) => Inst::ring(cx, cy, size * 0.5, line, col),
                (Kind::Square, true) => Inst::rect(cx, cy, size, size, rot, col),
                (Kind::Square, false) => {
                    // 空心：不填色、只描邊
                    let mut i = Inst::rect(cx, cy, size, size, rot, col);
                    i.fx[2] = 0.0;
                    i.line = [r, g, b, line];
                    i
                }
                (Kind::Triangle, _) => Inst::mark(cx, cy, size, rot, 9.0, col),
                (Kind::Cross, _) => Inst::mark(cx, cy, size, rot, 10.0, col),
            };
            out.push(inst.fixed());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn 同一首擺法一樣_換一首就換() {
        let (mut a, mut b) = (Geo::default(), Geo::default());
        a.lay_out(7);
        b.lay_out(7);
        assert_eq!(a.shapes, b.shapes);
        b.lay_out(8);
        assert_ne!(a.shapes, b.shapes);
        // 四種都有，三角、叉一定是實心
        for k in [Kind::Circle, Kind::Square, Kind::Triangle, Kind::Cross] {
            assert!(a.shapes.iter().any(|s| s.kind == k), "{k:?}");
        }
        assert!(a.shapes.iter().all(|s| s.filled || matches!(s.kind, Kind::Circle | Kind::Square)));
    }

    #[test]
    fn 格子差不多正方而且放得下() {
        for (n, aspect, want) in [(15, 16.0 / 9.0, (5, 3)), (20, 21.0 / 9.0, (7, 3)), (11, 4.0 / 3.0, (4, 3)), (1, 1.0, (1, 1))] {
            let (c, r) = grid(n, aspect);
            assert_eq!((c, r), want, "{n} {aspect}");
            assert!(c * r >= n);
        }
    }

    #[test]
    fn 大小照頻段換算() {
        assert_eq!(scale_of(0.0), 0.95);
        assert_eq!(scale_of(10.0 / 255.0), 0.95);
        assert!((scale_of(105.0 / 255.0) - 1.2).abs() < 1e-4);
        assert_eq!(scale_of(1.0), 1.45);
    }

    #[test]
    fn 彈簧第一幀到位_之後跟得上又不太彈() {
        let mut g = Geo::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert_eq!(g.spring(&[0.0; 5], at(0))[0], 0.0);
        // 低音一下子到 1：0.3 秒內跟到九成以上，過衝不超過 5%
        let mut top = 0.0f32;
        let mut v = 0.0;
        for f in 1..=60 {
            v = g.spring(&[1.0, 0.0, 0.0, 0.0, 0.0], at(f * 16))[0];
            top = top.max(v);
            if f == 19 {
                assert!(v > 0.9, "{v}");
            }
        }
        assert!(top < 1.05 && (v - 1.0).abs() < 0.01, "{top} {v}");
        // 隔太久沒畫：直接到位
        assert_eq!(g.spring(&[0.0; 5], at(5000))[0], 0.0);
    }
}
