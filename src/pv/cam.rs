//! 換句的運鏡：每句擺在一張比螢幕大的畫布上，換句時鏡頭移過去，舊句被帶出畫面、新句被帶進來。
//!
//! 一句擺在哪，是相對上一句走一步（[`Step`]）：平移、帶一點旋轉的甩鏡、往深處推進去，或者不動。
//! 主歌多半不動、偶爾平移；副歌常常移、移得遠。換段（已經有刷色轉場）、隔了空行或間奏、
//! 換句太快、打字風格、句子位置由段落場景管的（[`super::scene`]）都不動。跟構圖一樣只看歌詞與曲目；只有落點會對齊聽到的拍點（[`snap`]）。

use super::layout::Rng;
use super::plan::Part;
use super::style::{segment_of, Look, Segment};
use crate::lyrics::{Line, Lyrics};

/// 畫面上的相似變換：繞畫面中心縮放 `s`、轉 `a`（弧度），再平移 (x, y)（邏輯 px）
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub s: f32,
    pub a: f32,
    pub x: f32,
    pub y: f32,
}

impl Pose {
    pub const ID: Pose = Pose { s: 1.0, a: 0.0, x: 0.0, y: 0.0 };

    /// 先做 `b` 再做自己
    pub fn mul(self, b: Pose) -> Pose {
        let (sn, cs) = self.a.sin_cos();
        Pose {
            s: self.s * b.s,
            a: self.a + b.a,
            x: self.s * (b.x * cs - b.y * sn) + self.x,
            y: self.s * (b.x * sn + b.y * cs) + self.y,
        }
    }

    pub fn inv(self) -> Pose {
        let (sn, cs) = (-self.a).sin_cos();
        let k = -1.0 / self.s;
        Pose { s: 1.0 / self.s, a: -self.a, x: k * (self.x * cs - self.y * sn), y: k * (self.x * sn + self.y * cs) }
    }

    /// 從不動走到自己的 `u`（0..1）：縮放照比例走，推進的速度才均勻
    pub fn part(self, u: f32) -> Pose {
        Pose { s: self.s.powf(u), a: self.a * u, x: self.x * u, y: self.y * u }
    }

    /// 畫面上的一點（`c` 是畫面中心）
    pub fn map(self, x: f32, y: f32, c: (f32, f32)) -> (f32, f32) {
        let (sn, cs) = self.a.sin_cos();
        let (qx, qy) = (x - c.0, y - c.1);
        (c.0 + self.s * (qx * cs - qy * sn) + self.x, c.1 + self.s * (qx * sn + qy * cs) + self.y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    /// 不動，新句直接在原地出來
    Hold,
    /// 平移：新句從旁邊被帶進來
    Pan,
    /// 甩鏡：平移帶一點旋轉
    Swing,
    /// 推進：新句在深處，鏡頭往前推，舊句放大衝出畫面
    Dive,
}

#[derive(Clone, Copy, Debug)]
pub struct Step {
    pub mv: Move,
    /// 這句相對上一句擺在哪
    pub d: Pose,
    /// 鏡頭移多久（ms）
    pub dur: f32,
    /// 鏡頭什麼時候到（這句第一個字出來的時候，ms）
    pub land: i64,
}

/// 鏡頭在落點之前就走了幾成時間：新句的第一個字出來時鏡頭剛好走到一半，
/// 舊句先被帶走一段，新句後半段是被鏡頭帶進來的
const PRE: f32 = 0.5;
/// 兩句的落點隔多久之內才移（太快的換句來不及看，太久的中間早就沒字了）
const MIN_GAP: i64 = 900;
const MAX_GAP: i64 = 6000;
/// 鏡頭移的時間最多佔兩句間隔的幾成（下一次移之前要停穩）
const MAX_SHARE: f32 = 0.5;

/// 這句第一個字出來的時間
pub fn first_t(l: &Line) -> i64 {
    l.words.as_ref().and_then(|w| w.first()).map_or(l.t, |w| w.t.min(l.t))
}

/// 整首歌每句（含空行）的一步，跟 `Lyrics::synced` 一一對應
pub fn steps(ly: &Lyrics, parts: &[Part], segs: &[Segment], seed: u64, w: f32, h: f32) -> Vec<Step> {
    let mut rng = Rng::new(seed ^ 0xCA3E_0000);
    let mut out = Vec::with_capacity(ly.synced.len());
    let mut prev: Option<i64> = None;
    let mut blank = false;
    for (i, l) in ly.synced.iter().enumerate() {
        let land = first_t(l);
        let hold = Step { mv: Move::Hold, d: Pose::ID, dur: 0.0, land };
        if l.text.trim().is_empty() {
            blank = true;
            out.push(hold);
            continue;
        }
        // 每句抽一樣多次，前面哪句改成不動也不會牽動後面
        let r = [rng.f(), rng.f(), rng.f(), rng.f()];
        let part = parts.get(i).copied().unwrap_or_default();
        let seg = segs.get(segment_of(segs, i));
        let look = seg.map_or(Look::Neon, |s| s.look);
        let gap = prev.map_or(i64::MAX, |p| land - p);
        let still = prev.is_none()
            || blank
            || seg.is_some_and(|s| s.first == i || s.scene.persist())
            || !(MIN_GAP..=MAX_GAP).contains(&gap);
        prev = Some(land);
        blank = false;
        let mv = if still { Move::Hold } else { choose(part, look, r[0]) };
        if mv == Move::Hold {
            out.push(hold);
            continue;
        }
        let (lo, hi) = match part {
            Part::Verse => (0.45, 0.6),
            Part::Pre => (0.6, 0.8),
            Part::Chorus => (0.8, 1.05),
        };
        let dist = if look == Look::Cinema { 0.22 + 0.1 * r[2] } else { lo + (hi - lo) * r[2] };
        let phi = r[1] * std::f32::consts::TAU;
        let (dx, dy) = (phi.cos() * w * dist, phi.sin() * h * dist);
        let (d, base): (Pose, f32) = match mv {
            Move::Pan => (Pose { s: 1.0, a: 0.0, x: dx, y: dy }, 720.0),
            Move::Swing => {
                let a = if r[3] < 0.5 { -1.0 } else { 1.0 } * (0.22 + 0.2 * r[2]);
                (Pose { s: 1.0, a, x: dx * 0.55, y: dy * 0.55 }, 860.0)
            }
            _ => (Pose { s: 0.42 + 0.1 * r[2], a: 0.0, x: (r[1] - 0.5) * 0.3 * w, y: (r[3] - 0.5) * 0.2 * h }, 820.0),
        };
        let pace = match part {
            Part::Verse => 1.15,
            Part::Pre => 1.0,
            Part::Chorus => 0.9,
        } * if look == Look::Cinema { 1.4 } else { 1.0 };
        let dur = (base * pace).min(gap as f32 * MAX_SHARE);
        out.push(Step { mv, d, dur, land });
    }
    out
}

/// 照這句在歌裡的位置抽怎麼移：主歌多半不動，副歌常移
fn choose(part: Part, look: Look, r: f32) -> Move {
    let (hold, pan, swing) = match part {
        Part::Verse => (0.5, 0.35, 0.1),
        Part::Pre => (0.25, 0.4, 0.2),
        Part::Chorus => (0.1, 0.4, 0.25),
    };
    let mv = if r < hold {
        Move::Hold
    } else if r < hold + pan {
        Move::Pan
    } else if r < hold + pan + swing {
        Move::Swing
    } else {
        Move::Dive
    };
    match (look, mv) {
        // 打字稿一直寫在同一張紙上；電影只慢慢平移
        (Look::Type, _) => Move::Hold,
        (Look::Cinema, Move::Swing | Move::Dive) => Move::Pan,
        // 安靜的風格不往前衝
        (Look::Paper | Look::Drift, Move::Dive) => Move::Swing,
        _ => mv,
    }
}

/// 落點對齊拍點：離最近的拍不到四分之一拍（也不超過 150 ms）就移過去。
/// `since` 是現在離上一拍幾秒，`period` 一拍幾秒；不知道就照原本的落點
pub fn snap(land: i64, ms: i64, since: Option<f32>, period: f32) -> i64 {
    let Some(since) = since else { return land };
    if period <= 0.0 {
        return land;
    }
    let p = period * 1000.0;
    let base = ms as f32 - since * 1000.0;
    let beat = base + ((land as f32 - base) / p).round() * p;
    if (beat - land as f32).abs() <= (p * 0.25).min(150.0) { beat.round() as i64 } else { land }
}

/// 這一刻的鏡頭：拿它問每句該擺在畫面的哪裡
pub struct View {
    /// 算相對位置的那一句（正在移過去的那句的上一句）
    base: usize,
    cam: Pose,
}

/// 鏡頭移動的時間曲線：起訖都停穩，中間快（像手甩過去再穩住）
pub fn glide(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    u * u * u * (u * (u * 6.0 - 15.0) + 10.0)
}

impl View {
    /// `cur` 是正在唱的那句，`land(j)` 是第 j 句對過拍點的落點
    pub fn at(steps: &[Step], cur: Option<usize>, ms: i64, land: impl Fn(usize) -> i64) -> View {
        let start = |j: usize| steps.get(j).map(|s| land(j) as f32 - PRE * s.dur);
        // 下一句的鏡頭已經開始移了，就算它的
        let b = match cur {
            Some(c) if start(c + 1).is_some_and(|t| ms as f32 >= t) => c + 1,
            Some(c) => c,
            None if start(0).is_some_and(|t| ms as f32 >= t) => 0,
            None => return View { base: 0, cam: Pose::ID },
        };
        let Some(s) = steps.get(b) else { return View { base: b, cam: Pose::ID } };
        let u = if s.dur > 0.0 { (ms as f32 - (land(b) as f32 - PRE * s.dur)) / s.dur } else { 1.0 };
        View { base: b.saturating_sub(1), cam: s.d.part(glide(u)) }
    }

    /// 第 j 句這一刻在畫面上要套的變換
    pub fn of(&self, steps: &[Step], j: usize) -> Pose {
        let d = |k: usize| steps.get(k).map_or(Pose::ID, |s| s.d);
        let rel = if j > self.base {
            (self.base + 1..=j).fold(Pose::ID, |r, k| r.mul(d(k)))
        } else {
            (j + 1..=self.base).fold(Pose::ID, |r, k| r.mul(d(k))).inv()
        };
        self.cam.inv().mul(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::Line;

    fn near(a: Pose, b: Pose) -> bool {
        (a.s - b.s).abs() < 1e-4 && (a.a - b.a).abs() < 1e-4 && (a.x - b.x).abs() < 1e-2 && (a.y - b.y).abs() < 1e-2
    }

    #[test]
    fn 變換與反變換抵消() {
        let p = Pose { s: 0.5, a: 0.4, x: 120.0, y: -40.0 };
        assert!(near(p.mul(p.inv()), Pose::ID));
        assert!(near(p.inv().mul(p), Pose::ID));
        let (x, y) = p.inv().map(p.map(10.0, 20.0, (50.0, 50.0)).0, p.map(10.0, 20.0, (50.0, 50.0)).1, (50.0, 50.0));
        assert!((x - 10.0).abs() < 1e-3 && (y - 20.0).abs() < 1e-3);
    }

    fn step(mv: Move, d: Pose, land: i64) -> Step {
        Step { mv, d, dur: 1000.0, land }
    }

    #[test]
    fn 鏡頭移完新句在原位舊句被帶走() {
        let d = Pose { s: 1.0, a: 0.0, x: 800.0, y: 0.0 };
        let steps = [step(Move::Hold, Pose::ID, 0), step(Move::Pan, d, 3000)];
        let land = |j: usize| steps[j].land;
        // 還沒開始移：舊句在原位，新句在右邊外面
        let v = View::at(&steps, Some(0), 1000, land);
        assert!(near(v.of(&steps, 0), Pose::ID));
        assert!(near(v.of(&steps, 1), d));
        // 移完：新句在原位，舊句往左邊出去
        let v = View::at(&steps, Some(1), 4000, land);
        assert!(near(v.of(&steps, 1), Pose::ID));
        assert!(near(v.of(&steps, 0), d.inv()));
        // 移到一半：兩句一起往左走，中間沒有跳
        let v = View::at(&steps, Some(0), 2900, land);
        let x0 = v.of(&steps, 0).x;
        assert!(x0 < 0.0 && x0 > -800.0);
        assert!((v.of(&steps, 1).x - x0 - 800.0).abs() < 1e-2);
    }

    #[test]
    fn 落點對齊拍點() {
        // 現在 10000 ms，上一拍在 0.1 秒前，一拍 0.5 秒：拍點在 9900、10400、10900...
        assert_eq!(snap(10430, 10000, Some(0.1), 0.5), 10400);
        // 離最近的拍太遠就不動
        assert_eq!(snap(10650, 10000, Some(0.1), 0.5), 10650);
        assert_eq!(snap(10430, 10000, None, 0.5), 10430);
    }

    #[test]
    fn 主歌多半不動副歌常移() {
        let ly = Lyrics {
            synced: (0..80).map(|i| Line { t: i * 2500, text: format!("第{i}句 唱著歌"), ..Default::default() }).collect(),
            ..Default::default()
        };
        let moves = |part: Part| {
            let parts = vec![part; ly.synced.len()];
            steps(&ly, &parts, &[], 7, 1920.0, 1080.0).iter().filter(|s| s.mv != Move::Hold).count()
        };
        let (v, c) = (moves(Part::Verse), moves(Part::Chorus));
        assert!(v < c, "主歌 {v} 副歌 {c}");
        // 第一句一定不動
        let parts = vec![Part::Chorus; ly.synced.len()];
        assert_eq!(steps(&ly, &parts, &[], 7, 1920.0, 1080.0)[0].mv, Move::Hold);
    }
}
