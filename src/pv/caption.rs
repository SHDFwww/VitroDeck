//! 文字PV_v2 的翻譯字幕：正在唱的那句的翻譯（設定「顯示翻譯」開著、這句有翻譯才有）。
//! 固定在畫面下方置中（讓開左欄、電影風格的黑邊），不跟鏡頭；換句時新舊交叉淡化，新的微微往上浮。

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use super::style::Style;
use super::{cam, ease_out, glyph_inst, sdf, smooth01, with_a, GIn, Inst, LEAD, LINGER};
use crate::gfx::Gpu;
use crate::lyrics::Lyrics;
use crate::text::Text;

/// 淡進、淡出各多久（毫秒）
const FADE: f32 = 320.0;

/// 第 `i` 句的翻譯。空的、網易雲拿來佔位的 `//`、跟原文一樣的都不算
pub fn trans_of(ly: &Lyrics, i: usize) -> Option<&str> {
    let l = ly.synced.get(i)?;
    l.trans.as_deref().map(str::trim).filter(|s| !s.is_empty() && *s != "//" && *s != l.text.trim())
}

/// 第 `i` 句的翻譯什麼時候出來、什麼時候開始退（歌詞時間）：跟著那句開唱出來，
/// 下一句來了、或唱完 [`LINGER`] 還沒有下一句就退
fn span(ly: &Lyrics, i: usize) -> (i64, i64) {
    let l = &ly.synced[i];
    let next = ly.synced.get(i + 1).map_or(i64::MAX, |n| n.t);
    let end = l.words.as_ref().and_then(|w| w.last()).map_or(l.t + 3000, |w| w.t + w.d);
    (cam::first_t(l) - LEAD, next.min(end + LINGER))
}

/// 排過版的翻譯（句索引 → 字），只留正在唱的附近幾句。換一份歌詞要 [`Captions::clear`]
#[derive(Default)]
pub struct Captions {
    shaped: HashMap<usize, Vec<GIn>>,
}

impl Captions {
    pub fn clear(&mut self) {
        self.shaped.clear();
    }

    fn shape(&mut self, text: &mut Text, ly: &Lyrics, i: usize) -> Option<&[GIn]> {
        if let Entry::Vacant(e) = self.shaped.entry(i) {
            let s = trans_of(ly, i)?;
            let l = crate::lyrics::Line { t: 0, text: s.to_string(), ..Default::default() };
            e.insert(super::glyphs(text, &l, 1000));
        }
        self.shaped.get(&i).map(Vec::as_slice)
    }

    /// 畫正在唱的 `cur` 那句與還在淡出的上一句。`(cx, y)` 是字幕中心，`size` 字級，太寬就縮到 `max_w`
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw(
        &mut self,
        out: &mut Vec<Inst>,
        atlas: &mut sdf::Atlas,
        gpu: &Gpu,
        text: &mut Text,
        ly: &Lyrics,
        cur: Option<usize>,
        ms: i64,
        st: &Style,
        (cx, y): (f32, f32),
        size: f32,
        max_w: f32,
        px: f32,
    ) {
        let Some(c) = cur else {
            self.shaped.clear();
            return;
        };
        self.shaped.retain(|&i, _| i + 1 >= c && i <= c + 1);
        // 下一句先排好：換句那一幀才排版會頓一下
        let _ = self.shape(text, ly, c + 1);
        for i in [c.checked_sub(1), Some(c)].into_iter().flatten() {
            let (t0, t1) = span(ly, i);
            let up = smooth01((ms - t0) as f32 / FADE);
            let k = up * (1.0 - smooth01((ms - t1) as f32 / FADE));
            if k <= 0.003 {
                continue;
            }
            let Some(g) = self.shape(text, ly, i) else { continue };
            let em: f32 = g.iter().map(|x| x.adv).sum();
            let size = size.min(max_w / em.max(0.01));
            let mut x = cx - em * size * 0.5;
            let y = y + (1.0 - ease_out(up)) * size * 0.35;
            for gi in g {
                let adv = gi.adv * size;
                if let Some(gk) = gi.key
                    && let Some(e) = atlas.get(gpu, gk, |kk| text.raster(kk))
                {
                    out.push(glyph_inst(&e, gi.adv, x + adv * 0.5, y, size, 0.0, 0.0, with_a(st.ink, 0.82 * k), [0.0; 3], 0.0, 1.0, (0.0, 0.0), px).fixed());
                }
                x += adv;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::{Line, Word};

    #[test]
    fn 佔位與跟原文一樣的不算翻譯() {
        let line = |text: &str, trans: Option<&str>| Line { t: 0, text: text.into(), trans: trans.map(Into::into), ..Default::default() };
        let ly = Lyrics {
            synced: vec![line("Hello", Some("你好")), line("Hi", Some(" // ")), line("Yo", Some("  ")), line("一樣", Some("一樣")), line("沒有", None)],
            ..Default::default()
        };
        assert_eq!(trans_of(&ly, 0), Some("你好"));
        for i in 1..6 {
            assert_eq!(trans_of(&ly, i), None, "{i}");
        }
    }

    #[test]
    fn 下一句來了就退() {
        let w = |t: i64, d: i64| Some(vec![Word { t, d, text: "啊".into() }]);
        let ly = Lyrics {
            synced: vec![
                Line { t: 1000, text: "啊".into(), words: w(1000, 500), ..Default::default() },
                Line { t: 2000, text: "啊".into(), words: w(2000, 500), ..Default::default() },
                Line { t: 20000, text: "啊".into(), words: w(20000, 500), ..Default::default() },
            ],
            ..Default::default()
        };
        assert_eq!(span(&ly, 0), (1000 - LEAD, 2000));
        // 下一句很久以後才來：唱完 LINGER 就退
        assert_eq!(span(&ly, 1), (2000 - LEAD, 2500 + LINGER));
        assert_eq!(span(&ly, 2).1, 20500 + LINGER);
    }
}
