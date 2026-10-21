//! 段落配色：每段把風格的整套顏色轉一個色相，同一個風格出現幾次都不一樣。
//!
//! 轉色在 OKLab 做：亮度不動、彩度只在轉出 sRGB 色域時才收，整套一起轉同一個角度，
//! 所以風格裡各個顏色之間的色相差、明暗關係都保留。轉完逐對檢查看不看得清楚
//! （字對底、描邊、色塊上的字是對比，關鍵字對一般字、硬陰影對字是色差）：
//! 夠用的門檻要過；原本的配色就不到門檻的那一對（紙本的朱紅字對米白底），容許掉一成（[`legible`]）。
//!
//! 角度由 [`super::vary::paint`] 照曲目挑，同一首每次播都一樣。

use super::style::Style;

/// 墊底後面的畫面大概多亮：歌詞頁先蓋一層 42% 的黑，後面是模糊的封面，抓中間偏暗
const BEHIND: [f32; 3] = [0.2, 0.2, 0.21];

/// 一律當 sRGB 編碼值處理（PV 畫在 Rgba8Unorm 上，數值直接就是螢幕上的顏色）
fn lin(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}
fn enc(c: f32) -> f32 {
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

pub fn lab(c: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = c.map(lin);
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab 轉回線性 RGB（可能在色域外）
fn unlab(p: [f32; 3]) -> [f32; 3] {
    let l = (p[0] + 0.396_337_78 * p[1] + 0.215_803_76 * p[2]).powi(3);
    let m = (p[0] - 0.105_561_346 * p[1] - 0.063_854_17 * p[2]).powi(3);
    let s = (p[0] - 0.089_484_18 * p[1] - 1.291_485_5 * p[2]).powi(3);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// 彩度（灰階是 0，鮮豔的大約 0.15～0.3）
pub fn chroma(c: [f32; 3]) -> f32 {
    let p = lab(c);
    p[1].hypot(p[2])
}

/// 色相（度，0..360）
pub fn hue(c: [f32; 3]) -> f32 {
    let p = lab(c);
    p[2].atan2(p[1]).to_degrees().rem_euclid(360.0)
}

/// 色相轉 `deg` 度，亮度不動；轉出色域就把彩度收到剛好放得下
pub fn spin3(c: [f32; 3], deg: f32) -> [f32; 3] {
    if deg.rem_euclid(360.0) == 0.0 {
        return c;
    }
    let p = lab(c);
    let ch = p[1].hypot(p[2]);
    if ch < 1e-4 {
        return c;
    }
    lch(p[0], ch, p[2].atan2(p[1]) + deg.to_radians())
}

/// 亮度、彩度、色相（弧度）組回 sRGB；放不進色域就收彩度
fn lch(l: f32, ch: f32, h: f32) -> [f32; 3] {
    let at = |k: f32| unlab([l, ch * k * h.cos(), ch * k * h.sin()]);
    let inside = |v: [f32; 3]| v.iter().all(|&x| (-1e-4..=1.0 + 1e-4).contains(&x));
    let mut v = at(1.0);
    if !inside(v) {
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..16 {
            let mid = (lo + hi) * 0.5;
            if inside(at(mid)) { lo = mid } else { hi = mid }
        }
        v = at(lo);
    }
    v.map(|x| enc(x.clamp(0.0, 1.0)))
}

pub fn spin(c: [f32; 4], deg: f32) -> [f32; 4] {
    let [r, g, b] = spin3([c[0], c[1], c[2]], deg);
    [r, g, b, c[3]]
}

fn rgb(c: [f32; 4]) -> [f32; 3] {
    [c[0], c[1], c[2]]
}

/// WCAG 對比（1..21）
pub fn contrast(a: [f32; 3], b: [f32; 3]) -> f32 {
    let y = |c: [f32; 3]| 0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2]);
    let (ya, yb) = (y(a), y(b));
    (ya.max(yb) + 0.05) / (ya.min(yb) + 0.05)
}

/// OKLab 色差（0.02 以下幾乎分不出來，0.15 以上一眼就是不同顏色）
pub fn diff(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (p, q) = (lab(a), lab(b));
    ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
}

/// 墊底疊在後面畫面上的實際顏色
pub fn ground(st: &Style) -> [f32; 3] {
    let a = st.bg[3];
    [0, 1, 2].map(|i| BEHIND[i] + (st.bg[i] - BEHIND[i]) * a)
}

/// 要看得清楚的每一對：(量到的值, 夠用的門檻)。對比和色差混在一起，靠順序對齊
fn pairs(st: &Style) -> Vec<(f32, f32)> {
    const DECO: f32 = 3.0;
    let bg = ground(st);
    // 有描邊的字（衝擊、普普）靠邊跟字分開，底是什麼色不要緊
    let under = st.stroke.map_or(bg, |(c, _)| c);
    let ink = rgb(st.ink);
    let mut v = vec![(contrast(ink, under), TEXT), (contrast(rgb(st.line), bg), DECO)];
    if let Some(k) = st.key_ink {
        v.push((contrast(rgb(k), under), TEXT));
        v.push((diff(rgb(k), ink), APART));
    }
    if let Some(b) = st.band {
        v.push((contrast(rgb(st.band_ink), rgb(b)), TEXT));
        v.push((contrast(rgb(b), bg), DECO));
    }
    if let Some((c, _, _)) = st.shadow {
        v.push((diff(rgb(c), ink), APART));
    }
    v
}

/// 原本就不到門檻的那一對，轉色以後最多掉幾成
const SLACK: f32 = 0.9;
const TEXT: f32 = 4.5;
const APART: f32 = 0.15;

/// 封面強調色轉這段的色相。強調色到畫的時候才知道，所以在這裡檢查：
/// 亮的強調色轉到藍、紫會褪成接近白（跟白字分不出來），那就壓暗一點把彩度找回來；
/// 怎麼調都跟字或底太近，這段就用原本的強調色
pub fn accent(a: [f32; 4], st: &Style) -> [f32; 4] {
    if st.hue.rem_euclid(360.0) == 0.0 {
        return a;
    }
    let (ink, bg, a3) = (rgb(st.ink), ground(st), rgb(a));
    let (d0, c0) = (diff(a3, ink), contrast(a3, bg));
    let ok = |c: [f32; 3]| diff(c, ink) >= APART.min(d0 * SLACK) - 1e-4 && contrast(c, bg) >= TEXT.min(c0 * SLACK) - 1e-4;
    let p = lab(a3);
    let (ch, h) = (p[1].hypot(p[2]), p[2].atan2(p[1]) + st.hue.to_radians());
    for step in 0..8 {
        let c = lch(p[0] - 0.035 * step as f32, ch, h).map(|x| x.clamp(0.0, 1.0));
        if ok(c) {
            return [c[0], c[1], c[2], a[3]];
        }
    }
    a
}

/// 轉過色的 `t` 跟原本的 `base` 比：每一對都過門檻；原本就不到門檻的，不比原本差一成以上
pub fn legible(base: &Style, t: &Style) -> bool {
    pairs(base).iter().zip(pairs(t)).all(|(&(b, need), (m, _))| m >= need.min(b * SLACK) - 1e-4)
}

/// 這段一眼看過去是什麼顏色：(是不是封面強調色那一族, 色相)。
/// 強調色要到播放時才知道，那一族只能比轉了幾度
pub fn signature(st: &Style) -> (bool, f32) {
    for c in [st.key_ink, st.band].into_iter().flatten() {
        if chroma(rgb(c)) > 0.05 {
            return (false, hue(rgb(c)));
        }
    }
    (true, st.hue.rem_euclid(360.0))
}

/// 兩個色相差幾度（0..180）
pub fn gap(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pv::style::LOOKS;

    #[test]
    fn 來回轉換不走樣() {
        for c in [[0.97, 0.97, 0.98], [0.78, 0.09, 0.12], [1.0, 0.84, 0.2], [0.05, 0.05, 0.07], [0.25, 0.9, 1.0]] {
            let p = unlab(lab(c)).map(enc);
            for i in 0..3 {
                assert!((p[i] - c[i]).abs() < 2e-3, "{c:?} → {p:?}");
            }
            // 亮度不動，轉一圈回到原色
            let s = spin3(c, 137.0);
            assert!((lab(s)[0] - lab(c)[0]).abs() < 2e-3, "{c:?} 轉完 {s:?}");
            let back = spin3(spin3(c, 120.0), 240.0);
            assert!(diff(back, c) < 0.08, "{c:?} 轉回來 {back:?}");
        }
    }

    #[test]
    fn 轉零度就是原本的配色() {
        for l in LOOKS {
            let b = Style::of(l);
            assert!(legible(&b, &b.tinted(0.0)), "{l:?}");
            assert_eq!(b.tinted(0.0).key_ink, b.key_ink);
        }
    }

    #[test]
    fn 每個風格都有好幾個角度可以用() {
        for l in LOOKS {
            let b = Style::of(l);
            let ok = (0..24).filter(|&i| legible(&b, &b.tinted(i as f32 * 15.0))).count();
            assert!(ok >= 6, "{l:?} 只有 {ok} 個角度看得清楚");
        }
    }

    #[test]
    fn 封面強調色轉完不會褪成白字() {
        let neon = Style::of(crate::pv::style::Look::Neon);
        let ink = rgb(neon.ink);
        for a in [[1.0, 0.86, 0.2, 1.0], [0.3, 0.85, 1.0, 1.0], [1.0, 0.35, 0.6, 1.0], [0.55, 0.95, 0.4, 1.0]] {
            for i in 0..24 {
                let st = neon.tinted(i as f32 * 15.0);
                let c = accent(a, &st);
                assert!(diff(rgb(c), ink) >= APART.min(diff(rgb(a), ink) * SLACK) - 1e-3, "{a:?} 轉 {} 度變 {c:?}", i * 15);
            }
        }
    }

    #[test]
    fn 字跟底轉成相近色會被擋下() {
        // 紙本的黑字換成跟米白底差不多的顏色
        let b = Style::of(crate::pv::style::Look::Paper);
        let mut t = b;
        t.ink = [0.85, 0.82, 0.78, 1.0];
        assert!(!legible(&b, &t));
        // 關鍵字跟一般字一樣
        let mut t = b;
        t.key_ink = Some(t.ink);
        assert!(!legible(&b, &t));
    }
}
