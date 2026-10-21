//! 每首歌的場景計畫：哪幾句是副歌、副歌前哪幾句在往上推、每段用什麼風格與場景、每句的構圖配方、換句時鏡頭怎麼移。
//!
//! 現在全部由規則產生：歌詞裡整段重複最多次的就是副歌。以後要接模型，也是產生同一份計畫。
//! 跟構圖一樣只看歌詞與曲目，同一首歌每次播都一樣。

use super::cam::{self, Step};
use super::layout::Recipe;
use super::scene::{self, Shown, Track};
use super::style::{self, Look, Segment, Style};
use super::vary;
use crate::lyrics::Lyrics;

/// 一句在歌裡的位置
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Part {
    /// 主歌、間奏、空行：收斂
    #[default]
    Verse,
    /// 副歌前面一兩句：往上推
    Pre,
    /// 副歌：放開
    Chorus,
}

impl Part {
    /// 結構給的強度（0..1）
    pub fn level(self) -> f32 {
        match self {
            Part::Verse => 0.25,
            Part::Pre => 0.55,
            Part::Chorus => 0.9,
        }
    }
}

#[derive(Default)]
pub struct Plan {
    /// 每句（含空行）在歌裡的位置，跟 `Lyrics::synced` 一一對應
    pub parts: Vec<Part>,
    pub segs: Vec<Segment>,
    /// 每句（含空行）換過來時鏡頭怎麼移，一樣一一對應
    pub steps: Vec<Step>,
    /// 每段的場景，跟 `segs` 一一對應
    pub tracks: Vec<Track>,
    /// 每句（含空行）的構圖配方，整首不重複
    pub recipes: Vec<Option<Recipe>>,
}

/// 還沒唱第一句（前奏）的強度
const INTRO: f32 = 0.2;
/// 換句時強度提早多久開始變、變多久（ms）
const RAMP_LEAD: i64 = 400;
const RAMP: f32 = 1000.0;
/// 副歌前面最多幾句算往上推
const PRE_LINES: usize = 2;
/// 比對用的字太少（「啊」「Oh」這種）不算重複，也不打斷副歌
const SHORT: usize = 3;

impl Plan {
    /// `force` 是使用者鎖定的風格（整首歌只用它）；`w`、`h` 是畫面大小（鏡頭移多遠）
    pub fn build(ly: &Lyrics, seed: u64, force: Option<Look>, w: f32, h: f32) -> Self {
        let parts = parts(ly);
        let mut segs = style::segments(ly, &parts);
        let feats = vary::feats(ly);
        vary::cast(&mut segs, ly, &feats, seed, force);
        vary::paint(&mut segs, seed);
        let steps = cam::steps(ly, &parts, &segs, seed, w, h);
        let tracks = scene::tracks(ly, &segs, seed, w, h);
        let recipes = vary::recipes(&segs, &parts, &feats, seed);
        Self { parts, segs, steps, tracks, recipes }
    }

    /// 第 `i` 句的風格（含副歌、場景的調整）
    pub fn style(&self, i: usize) -> Style {
        vary::style_of(&self.segs, &self.parts, i)
    }

    /// 第 `i` 句的構圖配方
    pub fn recipe(&self, i: usize) -> Option<&Recipe> {
        self.recipes.get(i)?.as_ref()
    }

    /// 第 `i` 句所在那段的場景
    pub fn track(&self, i: usize) -> Option<&Track> {
        self.tracks.get(style::segment_of(&self.segs, i))
    }

    /// 第 `i` 句這一刻被場景擺在哪；不歸場景管是 `None`（照換句運鏡）
    pub fn shown(&self, i: usize, ms: i64) -> Option<Shown> {
        self.track(i)?.shown(i, ms)
    }

    /// 正在唱第 `c` 句時，從第幾句起可能還在畫面上：一般是前兩句；
    /// 堆疊牆、分鏡、階梯整段都在，上一段的在退場時也還在
    pub fn keep_from(&self, c: usize, ms: i64) -> usize {
        let mut from = c.saturating_sub(2);
        let si = style::segment_of(&self.segs, c);
        for k in [si.checked_sub(1), Some(si)].into_iter().flatten() {
            if let (Some(s), Some(t)) = (self.segs.get(k), self.tracks.get(k))
                && t.scene.keep_segment()
                && ms < t.exit + 1500
            {
                from = from.min(s.first);
            }
        }
        from.max(c.saturating_sub(14))
    }

    pub fn part(&self, i: usize) -> Part {
        self.parts.get(i).copied().unwrap_or_default()
    }

    /// 這一刻結構給的強度：照正在唱的那句，換句時平滑地變過去
    pub fn level_at(&self, ly: &Lyrics, ms: i64) -> f32 {
        let Some(c) = crate::scene_now::current_line(ly, ms + RAMP_LEAD) else { return INTRO };
        let prev = if c == 0 { INTRO } else { self.part(c - 1).level() };
        let q = ((ms + RAMP_LEAD - ly.synced[c].t) as f32 / RAMP).clamp(0.0, 1.0);
        let q = q * q * (3.0 - 2.0 * q);
        prev + (self.part(c).level() - prev) * q
    }
}

/// 拿來比對的字：只留字母、數字、漢字、假名，英文不分大小寫
fn norm(s: &str) -> Vec<char> {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// 兩句算不算同一句：差不到兩成（編輯距離），短句要一模一樣
fn same(a: &[char], b: &[char]) -> bool {
    let m = a.len().max(b.len());
    if a.len().min(b.len()) < SHORT || a.len().abs_diff(b.len()) > m / 5 {
        return false;
    }
    lev(a, b) <= m / 5
}

fn lev(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let up = row[j + 1];
            row[j + 1] = (up + 1).min(row[j] + 1).min(diag + usize::from(ca != cb));
            diag = up;
        }
    }
    row[b.len()]
}

/// 每句在歌裡的位置。
///
/// 1. 每句數它在整首出現幾次（`c`）。
/// 2. 連續的重複句（中間沒有空行、沒有間奏）串成一段；互相有句子重複的段落歸成一家。
/// 3. 段落最多的那一家是副歌，一樣多就取最後出現的那家（歌通常結束在副歌）。
///    副歌那段前後出現次數明顯少的句子修掉（副歌前導常跟副歌連在一起，但只重複兩次，副歌三次）。
/// 4. 副歌前面緊接著的一兩句是往上推的部分。
pub fn parts(ly: &Lyrics) -> Vec<Part> {
    let n = ly.synced.len();
    let mut out = vec![Part::Verse; n];
    let idx: Vec<usize> = (0..n).filter(|&i| !ly.synced[i].text.trim().is_empty()).collect();
    let txt: Vec<Vec<char>> = idx.iter().map(|&i| norm(&ly.synced[i].text)).collect();
    let m = idx.len();
    // 0 是太短不算
    let c: Vec<usize> = (0..m)
        .map(|k| if txt[k].len() < SHORT { 0 } else { 1 + (0..m).filter(|&j| j != k && same(&txt[k], &txt[j])).count() })
        .collect();
    // 跟前一句（idx 裡的前一個）中間有沒有空行或間奏
    let joined = |k: usize| k > 0 && idx[k] == idx[k - 1] + 1 && ly.synced[idx[k]].t - ly.synced[idx[k - 1]].t < style::SEG_GAP;

    // 連續的重複句（太短的夾在中間不打斷，但不當頭尾）
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut k = 0;
    while k < m {
        if c[k] < 2 {
            k += 1;
            continue;
        }
        // 中間夾一句沒重複的（每次副歌最後一句換詞）也接下去
        let mut e = k + 1;
        while e < m && joined(e) && (c[e] != 1 || (e + 1 < m && joined(e + 1) && c[e + 1] >= 2)) {
            e += 1;
        }
        while e > k + 1 && c[e - 1] == 0 {
            e -= 1;
        }
        if (k..e).filter(|&j| c[j] >= 2).count() >= 2 {
            runs.push((k, e));
        }
        k = e;
    }
    if runs.is_empty() {
        return out;
    }

    // 歸家：兩段裡有任何一句重複就是同一家
    let r = runs.len();
    let mut fam: Vec<usize> = (0..r).collect();
    fn root(f: &mut [usize], mut a: usize) -> usize {
        while f[a] != a {
            f[a] = f[f[a]];
            a = f[a];
        }
        a
    }
    for a in 0..r {
        for b in a + 1..r {
            let hit = (runs[a].0..runs[a].1).any(|x| (runs[b].0..runs[b].1).any(|y| same(&txt[x], &txt[y])));
            if hit {
                let (ra, rb) = (root(&mut fam, a), root(&mut fam, b));
                fam[rb] = ra;
            }
        }
    }
    let fams: Vec<usize> = (0..r).map(|a| root(&mut fam, a)).collect();
    // 段落最多、一樣多取最後出現的
    let best = (0..r)
        .map(|a| fams[a])
        .max_by_key(|&f| (fams.iter().filter(|&&x| x == f).count(), (0..r).rev().find(|&a| fams[a] == f)))
        .unwrap_or(0);
    if fams.iter().filter(|&&x| x == best).count() < 2 {
        return out;
    }
    // 這家的句子平均出現幾次（中位數），頭尾明顯少的修掉
    let mut cs: Vec<usize> = (0..r).filter(|&a| fams[a] == best).flat_map(|a| runs[a].0..runs[a].1).map(|x| c[x]).filter(|&v| v > 0).collect();
    cs.sort_unstable();
    let med = cs[cs.len() / 2] as f32;
    let weak = |x: usize| (c[x] as f32) < med * 0.75;
    for a in (0..r).filter(|&a| fams[a] == best) {
        let (mut s, mut e) = runs[a];
        while s < e && weak(s) {
            s += 1;
        }
        while e > s && weak(e - 1) {
            e -= 1;
        }
        if e - s < 2 {
            continue;
        }
        for x in s..e {
            out[idx[x]] = Part::Chorus;
        }
        // 前面緊接著的一兩句往上推
        let mut x = s;
        for _ in 0..PRE_LINES {
            if x == 0 || !joined(x) || out[idx[x - 1]] == Part::Chorus {
                break;
            }
            x -= 1;
            out[idx[x]] = Part::Pre;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::Line;

    /// 每段一個字串陣列，段落之間隔一行空行；一句 4 秒
    fn song(sections: &[&[&str]]) -> Lyrics {
        let mut synced = Vec::new();
        let mut t = 0;
        for s in sections {
            for l in *s {
                synced.push(Line { t, text: (*l).into(), ..Default::default() });
                t += 4000;
            }
            synced.push(Line { t: t - 500, ..Default::default() });
        }
        Lyrics { synced, ..Default::default() }
    }

    const V1: &[&str] = &["窗外的雨下了一整夜", "我數著時鐘的聲音", "桌上那杯咖啡已經冷掉"];
    const V2: &[&str] = &["路燈把影子拉得好長", "公車開過空蕩的街口", "你留下的外套還掛在門後"];
    const PRE: &[&str] = &["如果明天醒來", "一切都不一樣"];
    const CHO: &[&str] = &["就讓我大聲唱出來", "把所有的心事都丟掉", "天亮以前不要停下", "我們會一直走下去"];
    const BR: &[&str] = &["安靜一下", "聽見遠方的海浪"];

    fn of(ly: &Lyrics, text: &str) -> Vec<Part> {
        ly.synced.iter().zip(parts(ly)).filter(|(l, _)| l.text == text).map(|(_, p)| p).collect()
    }

    #[test]
    fn 重複最多次的是副歌() {
        let ly = song(&[V1, PRE, CHO, V2, PRE, CHO, BR, CHO]);
        assert_eq!(of(&ly, "就讓我大聲唱出來"), vec![Part::Chorus; 3]);
        assert_eq!(of(&ly, "我們會一直走下去"), vec![Part::Chorus; 3]);
        // 副歌前導只重複兩次，而且隔著空行：不算副歌；它是副歌前一兩句以外的，也不算往上推
        assert!(of(&ly, "如果明天醒來").iter().all(|&p| p != Part::Chorus));
        assert!(of(&ly, "窗外的雨下了一整夜").iter().all(|&p| p == Part::Verse));
        assert!(of(&ly, "聽見遠方的海浪").iter().all(|&p| p == Part::Verse));
    }

    #[test]
    fn 副歌前導連在一起也修得掉() {
        // 前導跟副歌之間沒有空行，整段一起重複；但副歌多唱一次
        let pc: Vec<&str> = PRE.iter().chain(CHO).copied().collect();
        let ly = song(&[V1, &pc, V2, &pc, BR, CHO]);
        assert_eq!(of(&ly, "就讓我大聲唱出來"), vec![Part::Chorus; 3]);
        assert_eq!(of(&ly, "如果明天醒來"), vec![Part::Pre; 2]);
        assert_eq!(of(&ly, "一切都不一樣"), vec![Part::Pre; 2]);
        assert_eq!(of(&ly, "桌上那杯咖啡已經冷掉"), vec![Part::Verse]);
    }

    #[test]
    fn 有一兩個字不一樣也認得出來() {
        let cho2: &[&str] = &["就讓我大聲的唱出來", "把所有的心事都丟掉", "天亮之前不要停下", "我們會一直走下去"];
        let ly = song(&[V1, CHO, V2, cho2]);
        let p = parts(&ly);
        assert_eq!(p.iter().filter(|&&x| x == Part::Chorus).count(), 8);
    }

    #[test]
    fn 沒有重複就沒有副歌() {
        let ly = song(&[V1, V2, PRE, BR]);
        assert!(parts(&ly).iter().all(|&p| p == Part::Verse));
        // 單獨一句重複（口號）不夠成副歌
        let ly = song(&[V1, &["就讓我大聲唱出來"], V2, &["就讓我大聲唱出來"]]);
        assert!(parts(&ly).iter().all(|&p| p == Part::Verse));
    }

    #[test]
    fn 強度換句時平滑變過去() {
        let ly = song(&[V1, CHO, V2, CHO]);
        let plan = Plan::build(&ly, 1, None, 1920.0, 1080.0);
        let t = ly.synced.iter().find(|l| l.text == CHO[0]).unwrap().t;
        let before = plan.level_at(&ly, t - 2000);
        let mid = plan.level_at(&ly, t);
        let after = plan.level_at(&ly, t + 1500);
        assert!((before - Part::Verse.level()).abs() < 1e-3, "{before}");
        assert!(mid > before && mid < after, "{before} {mid} {after}");
        assert!((after - Part::Chorus.level()).abs() < 1e-3, "{after}");
        assert!((plan.level_at(&ly, -5000) - INTRO).abs() < 1e-3);
    }
}
