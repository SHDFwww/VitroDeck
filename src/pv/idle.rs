//! 文字PV_v2 的前奏、間奏（沒有字、只有流光的那段）多加的四樣，都很淡、很慢：
//!
//! - **片頭曲名卡**（[`title_card`]）：前奏夠長時，歌名一個字一個字浮出來（失焦到清楚、微微往上），
//!   再來是歌手，底下一條強調色的細線從中間拉開；開唱前淡掉。間奏不出，尾奏夠長時同一張卡再出一次（片尾），歌結束前淡掉。
//! - **下一句預告＋倒數**（[`countdown`]）：空了一陣子之後的那句，提早三拍淡淡地出現在它的位置，
//!   底下三顆光點每過一拍熄一顆，熄完剛好開唱。預告的淡進在 `mod.rs` 跟著 v2 的前後句一起算。
//! - **頻譜光帶**（[`spectrum`]）：畫面下方一排很淡的柔光柱，低頻在中間、往兩邊是高頻，左右對稱。
//! - **關鍵字漂過**（[`drift`]）：間奏時剛唱過的關鍵字放大、失焦，很淡地從右往左漂過去，三層不同遠近。
//!
//! 濃淡都跟流光同一個 `idle`（沒字的那段才有，前後淡進淡出），只多幾十個方塊或字。

use std::collections::HashMap;
use std::f32::consts::PI;

use super::style::Style;
use super::{ease_out, glyph_inst, mix, sdf, smooth01, with_a, GIn, Inst, Line};
use crate::gfx::Gpu;
use crate::text::Text;

/// 給光帶的頻段數（從 `audio::Spectrum::level` 併過來；`bins` 播歌時中低頻幾乎一直頂在 1，不能用）
pub const SPEC: usize = 24;

/// 從 128 個對數頻段併成 [`SPEC`] 個（最低、最高的幾段幾乎沒東西，不要）
pub fn spec_from(bins: &[f32]) -> [f32; SPEC] {
    let (lo, hi) = (4usize, bins.len().saturating_sub(16).max(5));
    std::array::from_fn(|j| {
        let a = lo + (hi - lo) * j / SPEC;
        let b = (lo + (hi - lo) * (j + 1) / SPEC).max(a + 1).min(bins.len());
        bins[a..b].iter().copied().fold(0.0, f32::max)
    })
}

/// 曲名卡的字（排過版的歌名、歌手），換歌才重排
#[derive(Default)]
pub struct Card {
    key: String,
    title: Vec<GIn>,
    artist: Vec<GIn>,
}

impl Card {
    fn sync(&mut self, text: &mut Text, title: &str, artist: &str) {
        let key = format!("{title}\u{1}{artist}");
        if key == self.key {
            return;
        }
        let shape = |text: &mut Text, s: &str| {
            if s.trim().is_empty() {
                return Vec::new();
            }
            let l = crate::lyrics::Line { t: 0, text: s.to_string(), ..Default::default() };
            super::glyphs(text, &l, 1000)
        };
        self.title = shape(text, title);
        self.artist = shape(text, artist);
        self.key = key;
    }
}

/// 一排字從左到右一個一個浮出來。`at` 是第一個字開始的時間，`size` 是字級，整排置中在 (cx, cy)，
/// 太寬就縮到 `max_w`。回傳整排的寬
#[allow(clippy::too_many_arguments)]
fn write_out(
    out: &mut Vec<Inst>,
    atlas: &mut sdf::Atlas,
    gpu: &Gpu,
    text: &mut Text,
    g: &[GIn],
    cx: f32,
    cy: f32,
    size: f32,
    max_w: f32,
    at: f32,
    step: f32,
    col: [f32; 4],
    glow: f32,
    ms: f32,
    px: f32,
) -> f32 {
    let em: f32 = g.iter().map(|x| x.adv).sum();
    let size = size.min(max_w / em.max(0.01));
    let mut x = cx - em * size * 0.5;
    for (j, gi) in g.iter().enumerate() {
        let adv = gi.adv * size;
        let q = ((ms - (at + j as f32 * step)) / 700.0).clamp(0.0, 1.0);
        if let Some(gk) = gi.key
            && q > 0.0
            && let Some(e) = atlas.get(gpu, gk, |kk| text.raster(kk))
        {
            let u = ease_out(q);
            let y = cy + (1.0 - u) * size * 0.18;
            let mut inst = glyph_inst(&e, gi.adv, x + adv * 0.5, y, size, 0.0, 0.0, with_a(col, col[3] * u), [0.0; 3], 0.0, 1.0, (size * 0.06, glow), px);
            inst.ex[0] = (1.0 - u) * size * 0.08;
            out.push(inst.fixed());
        }
        x += adv;
    }
    em * size
}

/// 曲名卡：`span` 是 (開始寫的時間, 要淡完的時間)（歌詞時間），中間不到 3 秒就不出。
/// 片頭是 (0.5 秒, 第一句的字出來的時間)，片尾是 (最後一句退完, 歌結束)。
/// `cx` 是卡的水平中心、`wide` 是能用的寬（左欄出來時讓開）
#[allow(clippy::too_many_arguments)]
pub(super) fn title_card(
    out: &mut Vec<Inst>,
    card: &mut Card,
    atlas: &mut sdf::Atlas,
    gpu: &Gpu,
    text: &mut Text,
    title: &str,
    artist: &str,
    (start, until): (f32, f32),
    ms: i64,
    st: &Style,
    accent: [f32; 4],
    (cx, wide): (f32, f32),
    h: f32,
    px: f32,
) {
    let t = ms as f32;
    if until - start < 3000.0 || t > until || t < start - 500.0 {
        return;
    }
    // 結束前 1.5 秒淡掉，最後 0.4 秒什麼都不留
    let k = smooth01((until - 400.0 - t) / 1100.0);
    if k <= 0.003 {
        return;
    }
    card.sync(text, title, artist);
    if card.title.is_empty() {
        return;
    }
    // 空檔短的話寫快一點，整張卡至少要在淡掉前一秒寫完
    let room = (until - 1900.0 - start).max(800.0);
    let n = (card.title.len() + card.artist.len()).max(1) as f32;
    let step = (room * 0.6 / n).clamp(35.0, 110.0);
    let ink = with_a(st.ink, 0.92 * k);
    let tw = write_out(out, atlas, gpu, text, &card.title, cx, h * 0.45, h * 0.085, wide * 0.7, start, step, ink, 0.45 * k, t, px);
    let a_at = start + card.title.len() as f32 * step + 250.0;
    let sub = with_a(mix(st.ink, accent, 0.45), 0.7 * k);
    write_out(out, atlas, gpu, text, &card.artist, cx, h * 0.56, h * 0.038, wide * 0.55, a_at, step * 0.8, sub, 0.3 * k, t, px);
    // 歌名底下一條細線從中間拉開
    let u = ease_out(((t - a_at) / 900.0).clamp(0.0, 1.0));
    if u > 0.0 {
        let mut l = Inst::bar(cx, h * 0.505, tw * 0.55 * u, 1.6, 0.0, with_a(accent, 0.55 * k), 1.4).fixed();
        l.fx[0] = h * 0.01;
        l.fx[1] = 0.5;
        out.push(l);
    }
}

/// 倒數的三顆光點：擺在預告那句底下（`cx`、`y` 是那句的水平中心與下緣）。
/// 開唱前三拍全亮，之後每過一拍熄一顆，`t0` 熄完。`k` 是那句這一刻的濃淡
#[allow(clippy::too_many_arguments)]
pub(super) fn countdown(out: &mut Vec<Inst>, cx: f32, y: f32, t0: i64, beat_ms: f32, ms: i64, col: [f32; 4], k: f32, h: f32) {
    let t = (ms - t0) as f32;
    let start = -3.0 * beat_ms - 400.0;
    if t < start || t > 300.0 || k <= 0.003 {
        return;
    }
    let appear = smooth01((t - start) / 400.0);
    let gap = h * 0.032;
    let r = h * 0.0065;
    for j in 0..3 {
        // 第 j 顆在開唱前 (2 - j) 拍熄
        let off = t - (-(2 - j) as f32 * beat_ms);
        let fade = 1.0 - smooth01(off / 220.0);
        if fade <= 0.003 {
            continue;
        }
        // 熄之前那一拍輕輕亮一下
        let pre = (1.0 - (off + beat_ms).abs() / (beat_ms * 0.5)).clamp(0.0, 1.0);
        let x = cx + (j as f32 - 1.0) * gap;
        let s = 1.0 + 0.25 * pre - 0.4 * (1.0 - fade);
        out.push(Inst::disk(x, y + h * 0.05, r * s, with_a(col, (0.55 + 0.35 * pre) * fade * appear * k), (r * 2.5, 0.6)));
    }
}

/// 頻譜光帶：畫面下方一排柔光柱，跟著音樂起伏。`spec` 是 [`spec_from`] 的值（0..1）
pub(super) fn spectrum(out: &mut Vec<Inst>, spec: &[f32; SPEC], k: f32, ms: i64, accent: [f32; 4], w: f32, h: f32) {
    if k <= 0.003 {
        return;
    }
    let base = h * 0.8;
    let gap = w * 0.017;
    let t = ms as f32 / 1000.0;
    // 墊一道很寬很淡的橫光，柱子才不像是浮在空中
    let mut floor = Inst::bar(w * 0.5, base, w * 0.9, h * 0.06, 0.0, with_a(accent, 0.06 * k), 1.6).fixed();
    floor.ex[0] = h * 0.02;
    out.push(floor);
    for (j, &v) in spec.iter().enumerate() {
        // 沒聲音（或拍圖時）也留一點點起伏，不是一排死板的點
        let v = v.max(0.08 + 0.04 * (t * 1.3 + j as f32 * 0.7).sin());
        let len = h * (0.012 + 0.14 * v.powf(1.6));
        let a = k * (0.16 + 0.3 * v);
        let col = mix(accent, [1.0; 4], 0.3 * v);
        for side in [-1.0, 1.0] {
            let x = w * 0.5 + side * (j as f32 + 0.5) * gap;
            // 往上長，底下另有一截很淡的倒影
            let mut up = Inst::bar(x, base - len * 0.5, len, w * 0.005, PI * 0.5, with_a(col, a), 1.2).fixed();
            up.fx[0] = w * 0.006;
            up.fx[1] = 0.5;
            out.push(up);
            out.push(Inst::bar(x, base + len * 0.25, len * 0.5, w * 0.004, PI * 0.5, with_a(col, a * 0.3), 1.6).fixed());
        }
    }
}

/// 關鍵字漂過：間奏時剛唱過的句子（`before` 之前）的關鍵字，放大、失焦、很淡地從右往左漂。
/// `since` 是沒字那段開始的時間（流光淡進的起點），`k` 是整組的濃淡
#[allow(clippy::too_many_arguments)]
pub(super) fn drift(
    out: &mut Vec<Inst>,
    atlas: &mut sdf::Atlas,
    gpu: &Gpu,
    text: &mut Text,
    lines: &HashMap<usize, Line>,
    before: usize,
    since: f32,
    ms: i64,
    st: &Style,
    k: f32,
    w: f32,
    h: f32,
    px: f32,
) {
    if k <= 0.003 || !since.is_finite() {
        return;
    }
    // 最近唱過的兩句，各取關鍵字那一串
    let mut idx: Vec<usize> = lines.keys().copied().filter(|&i| i < before).collect();
    idx.sort_unstable();
    let words: Vec<(&Line, Vec<usize>)> = idx
        .iter()
        .rev()
        .take(3)
        .filter_map(|i| {
            let l = &lines[i];
            let mut ks: Vec<usize> = l.lay.glyphs.iter().filter(|p| p.key).map(|p| p.i).collect();
            ks.sort_unstable();
            (!ks.is_empty()).then_some((l, ks))
        })
        .collect();
    let t = (ms as f32 - since) / 1000.0;
    // 三層：遠的小、慢、更糊；近的大、快一點
    for (layer, (l, ks)) in words.iter().enumerate() {
        let depth = layer as f32;
        let size = h * (0.26 - 0.06 * depth);
        let speed = w * (0.045 - 0.01 * depth);
        let x0 = w * (1.05 + 0.25 * depth) - t * speed;
        let y = h * [0.34, 0.6, 0.2][layer % 3];
        let a = k * (0.075 - 0.015 * depth) * smooth01(t / 1.5);
        let mut x = x0;
        for &i in ks {
            let g = &l.g[i];
            let adv = g.adv * size;
            if x + adv > -w * 0.1
                && x < w * 1.1
                && let Some(gk) = g.key
                && let Some(e) = atlas.get(gpu, gk, |kk| text.raster(kk))
            {
                let mut inst = glyph_inst(&e, g.adv, x + adv * 0.5, y, size, 0.0, 0.0, with_a(st.ink, a), [0.0; 3], 0.0, 1.0, (0.0, 0.0), px);
                inst.ex[0] = 4.0 + 3.0 * depth;
                out.push(inst.fixed());
            }
            x += adv;
        }
    }
}
