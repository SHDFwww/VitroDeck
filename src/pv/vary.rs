//! 整首歌不重複：每段的風格＋場景、每句的構圖配方都在這裡一次排好。
//!
//! - 段落：風格與場景一起挑。同一首裡「風格＋場景」的組合不重複；風格十種用完以前不重複；
//!   相鄰兩段風格、場景都不一樣；副歌每次都換，而且副歌之間場景也不重複（特殊場景輪完才回頭）。
//!   配方太少的風格（浮遊、打字、電影）不給句子多的段落，不然那段的句子一定撞。
//! - 句子：這句在這個風格下能用的配方（[`layout::options`]）裡，挑整首還沒用過的；
//!   同一段裡範本、變化、進退場盡量錯開，色塊、框角出現的比例照風格。
//!
//! 都是貪心地一段一段、一句一句挑，挑的時候帶一點亂數；段落多試幾次取最好的一組。
//! 種子是曲目，同一首每次播都一樣。

use super::layout::{self, Feat, Recipe, Rng, Template};
use super::plan::Part;
use super::scene::{self, Scene, SCENES};
use super::style::{self, Look, Segment, Style, LOOKS};
use super::tint;
use crate::lyrics::Lyrics;

/// 段落的排法試幾次
const TRIES: u64 = 48;
/// 配方要比句子多幾成，那段才給這個風格（還要留給「不跟上一句同範本」的餘地）
const ROOM: f32 = 1.3;

/// 每句（含空行）的樣子，空行與沒有字的是 `None`
pub fn feats(ly: &Lyrics) -> Vec<Option<Feat>> {
    ly.synced
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if l.text.trim().is_empty() {
                return None;
            }
            let next = ly.synced.get(i + 1).map_or(l.t + 5000, |n| n.t);
            let times = super::char_times(l, next);
            let g: Vec<_> = l
                .text
                .chars()
                .enumerate()
                .map(|(k, ch)| {
                    let (t, d) = times.get(k).copied().unwrap_or((l.t, 200));
                    layout::probe(ch, t, d)
                })
                .collect();
            let f = Feat::of(&g);
            (f.n > 0).then_some(f)
        })
        .collect()
}

/// 第 `k` 段有字的句子
fn lines_of<'a>(segs: &[Segment], feats: &'a [Option<Feat>], k: usize) -> impl Iterator<Item = (usize, Feat)> + 'a {
    let end = segs.get(k + 1).map_or(feats.len(), |s| s.first);
    (segs[k].first..end).filter_map(move |i| feats[i].map(|f| (i, f)))
}

/// 這段給這個風格的話，總共有幾種配方可以用
fn capacity(look: Look, segs: &[Segment], feats: &[Option<Feat>], k: usize) -> usize {
    let st = Style::of(look);
    let mut all: Vec<Recipe> = Vec::new();
    for (_, f) in lines_of(segs, feats, k) {
        for (r, _) in layout::options(&st, &f) {
            if !all.contains(&r) {
                all.push(r);
            }
        }
    }
    all.len()
}

/// 給每段配風格與場景。`force` 是使用者鎖定的風格；`VITRO_PV_SCENE` 鎖定場景
pub fn cast(segs: &mut [Segment], ly: &Lyrics, feats: &[Option<Feat>], seed: u64, force: Option<Look>) {
    if segs.is_empty() {
        return;
    }
    let lock = scene::forced();
    let looks: Vec<Look> = force.map_or(LOOKS.to_vec(), |l| vec![l]);
    let scenes: Vec<Scene> = lock.map_or(SCENES.to_vec(), |s| vec![s]);
    // 每段：句數、唱多快、每個風格配方夠不夠
    let need: Vec<usize> = (0..segs.len()).map(|k| lines_of(segs, feats, k).count()).collect();
    let fast: Vec<f32> = (0..segs.len()).map(|k| style::fast(ly, segs, k)).collect();
    let cap: Vec<Vec<usize>> = (0..segs.len()).map(|k| looks.iter().map(|&l| capacity(l, segs, feats, k)).collect()).collect();
    // 副歌能不能用這個風格（照那段唱多快）
    let for_chorus = |l: Look, k: usize| force.is_some() || style::chorus_weight(l, fast[k]) > 0.0;

    let mut best: Option<(f32, Vec<(Look, Scene)>)> = None;
    for attempt in 0..TRIES {
        let mut rng = Rng::new(seed ^ 0xCA57_0000 ^ attempt.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut picks: Vec<(Look, Scene)> = Vec::with_capacity(segs.len());
        let mut total = 0.0;
        for (k, s) in segs.iter().enumerate() {
            let prev = picks.last().copied();
            let mut pick: Option<(f32, f32, Look, Scene)> = None;
            for (li, &l) in looks.iter().enumerate() {
                // 副歌不用安靜的風格（鎖定時例外）
                let pref = if s.chorus { style::chorus_weight(l, fast[k]) } else { style::weight(l, fast[k]) };
                if pref <= 0.0 && force.is_none() {
                    continue;
                }
                let look_used = picks.iter().filter(|p| p.0 == l).count() as f32;
                // 同一個風格的配方整首共用：前面用掉的句數也要算進去
                let taken: usize = picks.iter().enumerate().filter(|(_, p)| p.0 == l).map(|(j, _)| need[j]).sum();
                let roomy = cap[k][li] as f32 >= (need[k] + taken) as f32 * ROOM;
                // 挑了它以後，後面的段落還分不分得到沒用過的風格（副歌只能用放得開的）
                let starve = force.is_none() && look_used == 0.0 && {
                    let rest = k + 1..segs.len();
                    let rc = rest.clone().filter(|&j| segs[j].chorus).count();
                    let rv = rest.len() - rc;
                    let unused = looks.iter().filter(|&&x| x != l && !picks.iter().any(|p| p.0 == x));
                    let uc = unused.clone().filter(|&&x| rest.clone().any(|j| segs[j].chorus && for_chorus(x, j))).count();
                    let uv = unused.count() - uc;
                    let fits_after = |uc: usize, uv: usize| rc <= uc && rv <= uc - rc + uv;
                    // 本來就排不下（段落比風格多）就不算這次挑的錯
                    let before = {
                        let uc0 = uc + usize::from(rest.clone().any(|j| segs[j].chorus && for_chorus(l, j)));
                        fits_after(uc0, uv + usize::from(uc0 == uc))
                    };
                    before && !fits_after(uc, uv)
                };
                for &sc in &scenes {
                    if !sc.fits(l) && lock.is_none() {
                        continue;
                    }
                    let mut c = 0.0;
                    if picks.contains(&(l, sc)) {
                        c += 1000.0;
                    }
                    c += 300.0 * look_used;
                    if let Some((pl, ps)) = prev {
                        if pl == l {
                            c += 600.0;
                        }
                        if ps == sc {
                            c += 250.0;
                        }
                    }
                    // 配方不夠：已經是重複用的風格更不該再擠
                    if !roomy {
                        c += if look_used > 0.0 { 400.0 } else { 200.0 };
                    }
                    if starve {
                        c += 250.0;
                    }
                    let scene_used = picks.iter().filter(|p| p.1 == sc).count() as f32;
                    c += 40.0 * scene_used;
                    if s.chorus {
                        // 副歌之間場景不重複，也少用一般的
                        let in_chorus = picks.iter().zip(segs.iter()).filter(|(p, x)| x.chorus && p.1 == sc).count() as f32;
                        c += 90.0 * in_chorus;
                        if sc == Scene::Plain {
                            c += 60.0;
                        }
                    } else if sc == Scene::Plain {
                        // 主歌收斂，一般的多用一點
                        c -= 25.0;
                    }
                    // 唱的快慢給的偏好
                    c -= 20.0 * pref.min(2.0);
                    let noisy = c + rng.f() * 30.0;
                    if pick.is_none_or(|p| noisy < p.0) {
                        pick = Some((noisy, c, l, sc));
                    }
                }
            }
            let (_, c, l, sc) = pick.unwrap_or((0.0, 0.0, looks[0], scenes[0]));
            total += c;
            picks.push((l, sc));
        }
        if best.as_ref().is_none_or(|b| total < b.0) {
            best = Some((total, picks));
        }
    }
    if let Some((_, picks)) = best {
        for (s, (l, sc)) in segs.iter_mut().zip(picks) {
            s.look = l;
            s.scene = sc;
        }
    }
}

/// 第 `i` 句的風格（加上這句在歌裡的位置與場景的調整），構圖與配方都照這個
pub fn style_of(segs: &[Segment], parts: &[Part], i: usize) -> Style {
    let s = segs.get(style::segment_of(segs, i));
    s.map_or(Style::of(Look::Neon), Segment::style)
        .tuned(parts.get(i).copied().unwrap_or_default())
        .scened(s.map_or(Scene::Plain, |s| s.scene))
}

/// 色相轉的角度從這幾個裡挑（每 15 度一個，0 是原本的配色）
const HUES: usize = 24;
/// 相鄰兩段一眼看去的主色至少差幾度；同一個風格再出現時，跟它最近兩次至少差幾度
const NEXT_APART: f32 = 45.0;
const SAME_APART: f32 = 60.0;
const SAME_RECENT: usize = 2;

/// 給每段配色：在風格原本的配色上轉一個色相。
/// 字跟底、關鍵字跟一般字轉完會變相近的角度不用（[`tint::legible`]）；
/// 相鄰兩段主色要分得開，同一個風格第二次出現也要換個顏色。都排不開時先放掉後兩條，最後退回原色
pub fn paint(segs: &mut [Segment], seed: u64) {
    for k in 0..segs.len() {
        let base = Style::of(segs[k].look);
        let mut rng = Rng::new(seed ^ 0x7155_0000 ^ (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut cand: Vec<f32> = (0..HUES).map(|i| i as f32 * 360.0 / HUES as f32).collect();
        for i in (1..cand.len()).rev() {
            cand.swap(i, (rng.next() % (i as u64 + 1)) as usize);
        }
        let ok: Vec<f32> = cand.into_iter().filter(|&d| tint::legible(&base, &base.tinted(d))).collect();
        let prev = k.checked_sub(1).map(|j| tint::signature(&segs[j].style()));
        let apart = |d: f32| {
            let me = tint::signature(&base.tinted(d));
            prev.is_none_or(|p| p.0 != me.0 || tint::gap(p.1, me.1) >= NEXT_APART)
        };
        let fresh = |d: f32| segs[..k].iter().rev().filter(|s| s.look == segs[k].look).take(SAME_RECENT).all(|s| tint::gap(s.hue, d) >= SAME_APART);
        let pick = ok.iter().copied().find(|&d| apart(d) && fresh(d)).or_else(|| ok.iter().copied().find(|&d| apart(d))).or_else(|| ok.first().copied());
        segs[k].hue = pick.unwrap_or(0.0);
    }
}

/// 每句（含空行）的構圖配方，跟 `feats` 一一對應
pub fn recipes(segs: &[Segment], parts: &[Part], feats: &[Option<Feat>], seed: u64) -> Vec<Option<Recipe>> {
    let mut out: Vec<Option<Recipe>> = vec![None; feats.len()];
    let mut used: Vec<(Look, Recipe)> = Vec::new();
    let mut prev_t: Option<Template> = None;
    let mut rng = Rng::new(seed ^ 0x2EC1_0000);
    for k in 0..segs.len() {
        let look = segs[k].look;
        // 這段到目前用過的
        let mut here: Vec<Recipe> = Vec::new();
        for (i, f) in lines_of(segs, feats, k) {
            let st = style_of(segs, parts, i);
            let opts = layout::options(&st, &f);
            let n = here.len() as f32;
            let ratio = |hit: usize, on: bool, p: f32| ((hit + usize::from(on)) as f32 / (n + 1.0) - p).abs();
            let bands = here.iter().filter(|r| r.band).count();
            let corners = here.iter().filter(|r| r.corners).count();
            let mut pick: Option<(f32, Recipe)> = None;
            for (r, pref) in opts {
                let mut c = 0.0;
                c += 1000.0 * used.iter().filter(|u| u.0 == look && u.1 == r).count() as f32;
                // 相鄰兩句同範本比整首撞配方更顯眼
                if prev_t == Some(r.template) {
                    c += 1500.0;
                }
                c += 30.0 * here.iter().filter(|h| h.template == r.template).count() as f32;
                c += 15.0 * here.iter().filter(|h| h.template == r.template && h.side == r.side).count() as f32;
                c += 8.0 * used.iter().filter(|u| u.0 == look && u.1.template == r.template && u.1.side == r.side).count() as f32;
                c += 6.0 * here.iter().filter(|h| h.key_enter == r.key_enter).count() as f32;
                c += 6.0 * here.iter().filter(|h| h.exit == r.exit).count() as f32;
                c += 15.0 * ratio(bands, r.band, st.band_p);
                c += 15.0 * ratio(corners, r.corners, st.corners_p);
                c -= 6.0 * pref;
                c += rng.f() * 3.0;
                if pick.is_none_or(|p| c < p.0) {
                    pick = Some((c, r));
                }
            }
            let Some((_, r)) = pick else { continue };
            out[i] = Some(r);
            used.push((look, r));
            here.push(r);
            prev_t = Some(r.template);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::{Line, Word};

    /// 每段一組句子、段落之間隔空行；一句 4 秒、每字 300 ms 的逐字
    fn song(sections: &[&[&str]]) -> Lyrics {
        let mut synced = Vec::new();
        let mut t = 1000;
        for s in sections {
            for l in *s {
                let words = l.chars().enumerate().map(|(j, c)| Word { t: t + j as i64 * 300, d: 300, text: c.to_string() }).collect();
                synced.push(Line { t, text: (*l).into(), words: Some(words), ..Default::default() });
                t += 4000;
            }
            synced.push(Line { t: t - 500, ..Default::default() });
        }
        Lyrics { synced, ..Default::default() }
    }

    const V1: &[&str] = &["窗外的雨下了一整夜", "我數著時鐘的聲音", "桌上那杯咖啡已經冷掉", "你說過的話還在耳邊"];
    const V2: &[&str] = &["路燈把影子拉得好長", "公車開過空蕩的街口", "你留下的外套還掛在門後", "我還是習慣留一盞燈"];
    const PRE: &[&str] = &["如果明天醒來", "一切都不一樣"];
    const CHO: &[&str] = &["就讓我大聲唱出來", "把所有的心事都丟掉", "天亮以前不要停下", "我們會一直走下去"];
    const BR: &[&str] = &["安靜一下", "聽見遠方的海浪", "風吹過我們的屋頂", "星星一顆一顆亮起來"];

    fn plan(ly: &Lyrics, seed: u64, force: Option<Look>) -> super::super::plan::Plan {
        super::super::plan::Plan::build(ly, seed, force, 1920.0, 1080.0, false)
    }

    #[test]
    fn 段落的風格與場景整首不重複() {
        let ly = song(&[V1, PRE, CHO, V2, PRE, CHO, BR, CHO, CHO]);
        for seed in 0..30 {
            let p = plan(&ly, seed, None);
            let s = &p.segs;
            assert!(s.len() >= 8, "{s:?}");
            assert!(s.len() <= LOOKS.len());
            for (a, x) in s.iter().enumerate() {
                assert!(x.scene.fits(x.look), "{x:?}");
                for y in &s[a + 1..] {
                    assert_ne!(x.look, y.look, "seed {seed} 風格重複：{s:?}");
                }
            }
            for w in s.windows(2) {
                assert_ne!(w[0].scene, w[1].scene, "seed {seed} 相鄰場景一樣：{s:?}");
            }
            // 副歌每次都換，場景也不重複
            let ch: Vec<&Segment> = s.iter().filter(|x| x.chorus).collect();
            assert!(ch.len() >= 3);
            for (a, x) in ch.iter().enumerate() {
                assert!(!matches!(x.look, Look::Type | Look::Cinema));
                for y in &ch[a + 1..] {
                    assert_ne!(x.scene, y.scene, "seed {seed} 副歌場景重複：{s:?}");
                }
            }
        }
    }

    #[test]
    fn 段落比風格多也不撞組合() {
        let secs: Vec<&[&str]> = vec![V1, V2, BR, V1, V2, BR, V1, V2, BR, V1, V2, BR, V1, V2];
        let ly = song(&secs);
        let p = plan(&ly, 5, None);
        let s = &p.segs;
        assert!(s.len() > LOOKS.len());
        for (a, x) in s.iter().enumerate() {
            for y in &s[a + 1..] {
                assert!((x.look, x.scene) != (y.look, y.scene), "{s:?}");
            }
        }
        // 十種都用到才回頭
        let distinct: std::collections::HashSet<_> = s.iter().map(|x| x.look as u8).collect();
        assert_eq!(distinct.len(), LOOKS.len());
    }

    #[test]
    fn 每段配色看得清楚而且會換() {
        let secs: Vec<&[&str]> = vec![V1, V2, BR, V1, V2, BR, V1, V2, BR, V1, V2, BR, V1, V2];
        let ly = song(&secs);
        for seed in 0..30 {
            for force in [None, Some(Look::Paper), Some(Look::Neon)] {
                let s = plan(&ly, seed, force).segs;
                for x in &s {
                    assert!(tint::legible(&Style::of(x.look), &x.style()), "seed {seed} {x:?}");
                }
                // 鎖定風格時，同一個風格連著出現也要換顏色
                for w in s.windows(2) {
                    if w[0].look == w[1].look {
                        assert!(tint::gap(w[0].hue, w[1].hue) >= NEXT_APART, "seed {seed} {w:?}");
                    }
                }
                // 整首不是全部同一個顏色
                let distinct: std::collections::HashSet<_> = s.iter().map(|x| x.hue as u32).collect();
                assert!(distinct.len() >= 4, "seed {seed} {force:?} {distinct:?}");
            }
        }
    }

    #[test]
    fn 每句構圖整首不重複而且相鄰範本不同() {
        let ly = song(&[V1, PRE, CHO, V2, PRE, CHO, BR, CHO]);
        for seed in 0..30 {
            let p = plan(&ly, seed, None);
            let mut seen: Vec<(Look, Recipe)> = Vec::new();
            let mut prev: Option<Template> = None;
            for (i, r) in p.recipes.iter().enumerate() {
                let Some(r) = *r else { continue };
                let look = p.segs[style::segment_of(&p.segs, i)].look;
                assert!(!seen.contains(&(look, r)), "seed {seed} 第 {i} 句配方重複 {r:?}");
                assert_ne!(prev, Some(r.template), "seed {seed} 第 {i} 句跟上一句同範本");
                seen.push((look, r));
                prev = Some(r.template);
            }
            // 每句都配到了
            assert_eq!(seen.len(), 28);
        }
    }

    #[test]
    fn 鎖定風格整首都用它() {
        let ly = song(&[V1, CHO, V2, CHO]);
        let p = plan(&ly, 3, Some(Look::Paper));
        assert!(p.segs.iter().all(|x| x.look == Look::Paper));
        assert!(p.segs.iter().all(|x| x.scene.fits(Look::Paper)));
    }

    #[test]
    fn 同一首每次一樣() {
        let ly = song(&[V1, CHO, V2, CHO, BR]);
        let (a, b) = (plan(&ly, 9, None), plan(&ly, 9, None));
        assert_eq!(a.recipes, b.recipes);
        assert!(a.segs.iter().zip(&b.segs).all(|(x, y)| x.look == y.look && x.scene == y.scene));
    }
}
