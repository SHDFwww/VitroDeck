//! 演出風格：整套配色、構圖偏好、進退場、裝飾與鏡頭力道。
//!
//! 一首歌照段落換風格（[`segments`]）：間奏、空行或滿八句就切一段，
//! 每段挑一個跟前兩段都不一樣的風格，唱得快的段落偏向衝擊、故障、普普、復古，慢的偏向浮遊、紙本、電影。
//! 跟構圖一樣只看歌詞與曲目，同一首歌每次播換的地方、換成什麼都一樣。

use super::layout::{Enter, Exit, Rng, Template};
use crate::lyrics::Lyrics;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Look {
    /// 霓虹：白字＋強調色色塊、鼓點色差與閃光（最早那一版）
    Neon,
    /// 紙本：米白底、黑字、朱紅點綴，直書優先，安靜
    Paper,
    /// 衝擊：白字粗黑邊、黃色關鍵字、集中線，蓋章與大字
    Impact,
    /// 故障：常駐色差、橫向切片，字一閃一閃地出現
    Glitch,
    /// 浮遊：字慢慢浮上來、柔光、光點往上飄，鏡頭幾乎不晃
    Drift,
    /// 打字：深色底、暖白字一個一個打出來，游標跟著走，退場時倒退鍵刪回去
    Type,
    /// 普普：黃底、白字粗黑邊＋硬陰影、角落網點，彈跳進場
    Pop,
    /// 燈管：字只剩發光的輪廓，像霓虹招牌一個字一個字通電亮起來、退場時斷電
    Sign,
    /// 電影：上下黑邊、小字置中、失焦淡入，暖色漏光與較重的顆粒，幾乎不動
    Cinema,
    /// 復古：合成波（synthwave）的紫底、往前流的透視網格、粉紅字帶青色錯位陰影
    Retro,
}

pub const LOOKS: [Look; 10] =
    [Look::Neon, Look::Paper, Look::Impact, Look::Glitch, Look::Drift, Look::Type, Look::Pop, Look::Sign, Look::Cinema, Look::Retro];

impl Look {
    pub fn name(self) -> &'static str {
        match self {
            Look::Neon => "霓虹",
            Look::Paper => "紙本",
            Look::Impact => "衝擊",
            Look::Glitch => "故障",
            Look::Drift => "浮遊",
            Look::Type => "打字",
            Look::Pop => "普普",
            Look::Sign => "燈管",
            Look::Cinema => "電影",
            Look::Retro => "復古",
        }
    }
}

#[derive(Clone, Copy, Debug)]
/// 字型不歸風格管：一律用設定裡選的（使用者明講過）
pub struct Style {
    pub look: Look,
    /// 整片墊底的顏色（直通 alpha，0 是不墊）
    pub bg: [f32; 4],
    /// 一般字的填色
    pub ink: [f32; 4],
    /// 關鍵字的填色，`None` 用封面的強調色
    pub key_ink: Option<[f32; 4]>,
    /// 色塊的顏色（`None` 用強調色）與墊在上面的字色
    pub band: Option<[f32; 4]>,
    pub band_ink: [f32; 4],
    /// 實心字也描一圈邊（衝擊的黑邊）：(顏色, 字級的幾分之幾)
    pub stroke: Option<([f32; 3], f32)>,
    /// 線條、框角、圓環這類細裝飾的顏色
    pub line: [f32; 4],
    /// 構圖範本：照順序輪，排在前面的比較常出現
    pub templates: &'static [Template],
    pub enters: &'static [Enter],
    pub key_enters: &'static [Enter],
    pub exits: &'static [Exit],
    pub hollow: f32,
    pub band_p: f32,
    pub corners_p: f32,
    pub ghost: bool,
    /// 最大的字佔畫面高的幾成
    pub cap: f32,
    /// 鏡頭晃動、色差、閃光、跟拍縮放的倍率
    pub shake: f32,
    pub chroma: f32,
    pub flash: f32,
    pub pump: f32,
    /// 唱到的字發光多強
    pub glow: f32,
    /// 鼓點時畫面中心擴散的圓環
    pub ring: bool,
    /// 特有的裝飾
    pub rays: bool,
    pub slices: bool,
    pub motes: bool,
    /// 句尾蓋一枚朱印（紙本）
    pub seal: bool,
    /// 背景散景光斑的濃淡（0 是沒有）
    pub bokeh: f32,
    /// 四角暗角、底片顆粒有多重
    pub vignette: f32,
    pub grain: f32,
    /// 關鍵字被唱到時迸出的光粒、掃過字面的光澤（強度，0 是沒有）
    pub sparks: bool,
    pub shine: f32,
    /// 暗角的顏色（亮底的風格用深褐、橘，不用黑）
    pub vig: [f32; 3],
    /// 字沒在動的時候上下輕輕浮多少（字級的幾分之幾）
    pub float: f32,
    /// 每個詞組一樣大（不放大關鍵字、不混小字）
    pub even: bool,
    /// 硬陰影：(顏色, 往右, 往下)，位移是字級的幾分之幾
    pub shadow: Option<([f32; 4], f32, f32)>,
    /// 字畫成發光的燈管輪廓（燈管）
    pub tube: bool,
    /// 打字游標（打字）
    pub cursor: bool,
    /// 上下黑邊各佔畫面高的幾成（電影，0 是沒有）
    pub bars: f32,
    /// 鋪在背景、不管有沒有字都在的場景：角落網點、透視網格、漏光
    pub halftone: bool,
    pub grid: bool,
    pub leaks: bool,
}

const WHITE: [f32; 4] = [0.97, 0.97, 0.98, 1.0];
const NEAR_BLACK: [f32; 4] = [0.05, 0.05, 0.07, 1.0];

impl Style {
    pub fn of(look: Look) -> Self {
        use Enter::*;
        use Template::*;
        let neon = Style {
            look,
            bg: [0.0; 4],
            ink: WHITE,
            key_ink: None,
            band: None,
            band_ink: NEAR_BLACK,
            stroke: None,
            line: [1.0, 1.0, 1.0, 1.0],
            templates: &[Stack, Diagonal, Vertical, Zigzag, Slam],
            enters: &[Pop, Drop, Spin, Slide(-1.0), Slide(1.0)],
            key_enters: &[Stamp, Stamp, Stamp, Pop, Pop],
            exits: &[Exit::Scatter, Exit::Fall, Exit::Zoom],
            hollow: 0.25,
            band_p: 0.5,
            corners_p: 0.4,
            ghost: true,
            cap: 0.24,
            shake: 1.0,
            chroma: 1.0,
            flash: 1.0,
            pump: 1.0,
            glow: 0.55,
            ring: true,
            rays: false,
            slices: false,
            motes: false,
            seal: false,
            bokeh: 1.0,
            vignette: 0.42,
            grain: 0.035,
            sparks: true,
            shine: 0.6,
            vig: [0.0; 3],
            float: 0.018,
            even: false,
            shadow: None,
            tube: false,
            cursor: false,
            bars: 0.0,
            halftone: false,
            grid: false,
            leaks: false,
        };
        match look {
            Look::Neon => neon,
            Look::Paper => Style {
                bg: [0.93, 0.91, 0.86, 0.94],
                ink: [0.09, 0.08, 0.08, 1.0],
                key_ink: Some([0.78, 0.09, 0.12, 1.0]),
                band: Some([0.78, 0.09, 0.12, 1.0]),
                band_ink: [0.96, 0.94, 0.9, 1.0],
                line: [0.09, 0.08, 0.08, 1.0],
                templates: &[Vertical, Stack, Vertical, Diagonal],
                enters: &[Rise, Rise, Pop],
                key_enters: &[Rise, Stamp],
                exits: &[Exit::Fade],
                hollow: 0.0,
                band_p: 0.3,
                corners_p: 0.5,
                ghost: true,
                cap: 0.22,
                shake: 0.25,
                chroma: 0.0,
                flash: 0.0,
                pump: 0.5,
                glow: 0.0,
                ring: false,
                seal: true,
                bokeh: 0.0,
                vignette: 0.22,
                grain: 0.06,
                sparks: false,
                shine: 0.35,
                vig: [0.32, 0.22, 0.12],
                ..neon
            },
            Look::Impact => Style {
                bg: [0.02, 0.02, 0.03, 0.55],
                key_ink: Some([1.0, 0.86, 0.1, 1.0]),
                band: Some([1.0, 0.86, 0.1, 1.0]),
                stroke: Some(([0.0, 0.0, 0.0], 0.07)),
                templates: &[Slam, Zigzag, Diagonal, Slam, Stack],
                enters: &[Pop, Stamp, Slide(-1.0), Slide(1.0)],
                key_enters: &[Stamp],
                exits: &[Exit::Zoom, Exit::Scatter],
                hollow: 0.15,
                band_p: 0.25,
                corners_p: 0.0,
                ghost: false,
                cap: 0.3,
                shake: 1.7,
                chroma: 0.4,
                flash: 1.4,
                pump: 1.5,
                glow: 0.0,
                rays: true,
                bokeh: 0.0,
                vignette: 0.55,
                grain: 0.05,
                shine: 0.7,
                ..neon
            },
            Look::Glitch => Style {
                bg: [0.0, 0.02, 0.05, 0.6],
                key_ink: Some([0.35, 1.0, 0.9, 1.0]),
                band: Some([1.0, 0.2, 0.55, 1.0]),
                line: [0.35, 1.0, 0.9, 1.0],
                templates: &[Stack, Slam, Diagonal, Zigzag],
                enters: &[Flicker],
                key_enters: &[Flicker, Stamp],
                exits: &[Exit::Blink],
                hollow: 0.2,
                band_p: 0.35,
                corners_p: 0.6,
                ghost: false,
                shake: 0.8,
                chroma: 2.2,
                flash: 0.6,
                glow: 0.3,
                ring: false,
                slices: true,
                bokeh: 0.0,
                vignette: 0.5,
                grain: 0.06,
                sparks: false,
                shine: 0.4,
                ..neon
            },
            Look::Drift => Style {
                bg: [0.01, 0.02, 0.05, 0.35],
                templates: &[Vertical, Stack, Diagonal],
                enters: &[Rise],
                key_enters: &[Rise],
                exits: &[Exit::Fade],
                hollow: 0.3,
                band_p: 0.0,
                corners_p: 0.0,
                cap: 0.19,
                shake: 0.0,
                chroma: 0.0,
                flash: 0.0,
                pump: 0.5,
                glow: 0.9,
                ring: false,
                motes: true,
                bokeh: 1.3,
                vignette: 0.4,
                grain: 0.03,
                shine: 0.5,
                float: 0.04,
                ..neon
            },
            Look::Type => Style {
                bg: [0.06, 0.055, 0.05, 0.78],
                ink: [0.93, 0.9, 0.82, 1.0],
                key_ink: Some([1.0, 0.72, 0.3, 1.0]),
                line: [1.0, 0.72, 0.3, 1.0],
                templates: &[Typed, Caption],
                enters: &[Type],
                key_enters: &[Type],
                exits: &[Exit::Erase],
                hollow: 0.0,
                band_p: 0.0,
                corners_p: 0.0,
                ghost: false,
                cap: 0.11,
                shake: 0.0,
                chroma: 0.0,
                flash: 0.0,
                pump: 0.15,
                glow: 0.15,
                ring: false,
                bokeh: 0.0,
                vignette: 0.5,
                grain: 0.05,
                sparks: false,
                shine: 0.0,
                float: 0.0,
                even: true,
                cursor: true,
                ..neon
            },
            Look::Pop => Style {
                bg: [1.0, 0.84, 0.2, 0.93],
                key_ink: Some([1.0, 0.25, 0.5, 1.0]),
                band: Some([0.08, 0.07, 0.1, 1.0]),
                band_ink: [1.0, 0.84, 0.2, 1.0],
                stroke: Some(([0.08, 0.07, 0.1], 0.08)),
                line: [0.08, 0.07, 0.1, 1.0],
                templates: &[Zigzag, Stack, Diagonal, Slam],
                enters: &[Pop, Drop, Pop],
                key_enters: &[Pop, Stamp],
                exits: &[Exit::Fall, Exit::Scatter],
                hollow: 0.0,
                band_p: 0.35,
                corners_p: 0.0,
                ghost: false,
                cap: 0.27,
                shake: 0.6,
                chroma: 0.0,
                flash: 0.0,
                pump: 1.3,
                glow: 0.0,
                ring: false,
                bokeh: 0.0,
                vignette: 0.2,
                grain: 0.03,
                sparks: false,
                shine: 0.5,
                vig: [0.75, 0.3, 0.05],
                shadow: Some(([0.08, 0.07, 0.1, 1.0], 0.07, 0.08)),
                halftone: true,
                ..neon
            },
            Look::Sign => Style {
                bg: [0.03, 0.02, 0.05, 0.72],
                ink: [0.5, 0.92, 1.0, 1.0],
                key_ink: Some([1.0, 0.3, 0.72, 1.0]),
                line: [1.0, 0.3, 0.72, 1.0],
                templates: &[Stack, Diagonal, Zigzag, Slam],
                enters: &[Ignite],
                key_enters: &[Ignite],
                exits: &[Exit::Off],
                hollow: 0.0,
                band_p: 0.0,
                corners_p: 0.3,
                ghost: false,
                cap: 0.22,
                shake: 0.0,
                chroma: 0.0,
                flash: 0.0,
                pump: 0.6,
                glow: 0.0,
                ring: false,
                bokeh: 0.5,
                vignette: 0.5,
                grain: 0.04,
                sparks: false,
                shine: 0.0,
                tube: true,
                ..neon
            },
            Look::Cinema => Style {
                bg: [0.0, 0.0, 0.0, 0.55],
                ink: [0.95, 0.93, 0.88, 1.0],
                key_ink: Some([1.0, 0.8, 0.5, 1.0]),
                line: [1.0, 0.8, 0.5, 1.0],
                templates: &[Caption, Vertical],
                enters: &[Focus],
                key_enters: &[Focus],
                exits: &[Exit::Fade],
                hollow: 0.0,
                band_p: 0.0,
                corners_p: 0.0,
                ghost: false,
                cap: 0.085,
                shake: 0.0,
                chroma: 0.0,
                flash: 0.0,
                pump: 0.15,
                glow: 0.15,
                ring: false,
                bokeh: 0.0,
                vignette: 0.55,
                grain: 0.07,
                sparks: false,
                shine: 0.25,
                float: 0.006,
                even: true,
                bars: 0.12,
                leaks: true,
                ..neon
            },
            Look::Retro => Style {
                bg: [0.08, 0.02, 0.14, 0.78],
                key_ink: Some([1.0, 0.35, 0.75, 1.0]),
                band: Some([0.25, 0.9, 1.0, 1.0]),
                line: [0.3, 0.9, 1.0, 1.0],
                templates: &[Diagonal, Stack, Slam, Zigzag],
                enters: &[Slide(-1.0), Slide(1.0), Pop],
                key_enters: &[Pop, Stamp],
                exits: &[Exit::Zoom, Exit::Fade],
                hollow: 0.2,
                band_p: 0.25,
                corners_p: 0.0,
                ghost: false,
                cap: 0.25,
                shake: 0.5,
                chroma: 0.5,
                flash: 0.5,
                glow: 0.45,
                ring: false,
                bokeh: 0.0,
                vignette: 0.45,
                grain: 0.04,
                shine: 0.6,
                shadow: Some(([0.25, 0.85, 1.0, 0.9], 0.05, 0.06)),
                grid: true,
                ..neon
            },
        }
    }

    /// 字要描邊的話，邊有多粗（px）
    pub fn stroke_w(&self, size: f32) -> f32 {
        self.stroke.map_or(0.0, |(_, k)| (size * k).max(2.0))
    }
}

/// 一段：從第 `first` 句開始，`t` 是那句開始的時間
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub first: usize,
    pub t: i64,
    pub look: Look,
}

/// 一段最少、最多幾句；兩句開頭隔多久算間奏（ms）
const SEG_MIN: usize = 4;
const SEG_MAX: usize = 8;
const SEG_GAP: i64 = 9000;

/// 把歌詞切成段落並配好風格。`force` 是使用者鎖定的風格（整首歌只用它）
pub fn segments(ly: &Lyrics, seed: u64, force: Option<Look>) -> Vec<Segment> {
    // 先找段落的起點與每段的字數、時長
    let mut starts: Vec<usize> = Vec::new();
    let mut count = 0;
    let mut prev_t: Option<i64> = None;
    let mut blank = false;
    for (i, l) in ly.synced.iter().enumerate() {
        if l.text.trim().is_empty() {
            blank = true;
            continue;
        }
        let gap = prev_t.map_or(0, |p| l.t - p);
        if starts.is_empty() || count >= SEG_MAX || (count >= SEG_MIN && (blank || gap >= SEG_GAP)) {
            starts.push(i);
            count = 0;
        }
        count += 1;
        prev_t = Some(l.t);
        blank = false;
    }
    let mut rng = Rng::new(seed ^ 0x5EC7_0000);
    let mut out: Vec<Segment> = Vec::with_capacity(starts.len());
    for (k, &first) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(ly.synced.len());
        let lines = &ly.synced[first..end];
        let chars: usize = lines.iter().map(|l| l.text.chars().filter(|c| !c.is_whitespace()).count()).sum();
        let t_end = ly.synced.get(end).map_or(lines.last().map_or(0, |l| l.t + 4000), |l| l.t);
        let secs = ((t_end - ly.synced[first].t) as f32 / 1000.0).max(1.0);
        let cps = chars as f32 / secs;
        let recent: Vec<Look> = out.iter().rev().take(2).map(|s| s.look).collect();
        let look = force.unwrap_or_else(|| pick(&mut rng, cps, &recent));
        out.push(Segment { first, t: ly.synced[first].t, look });
    }
    out
}

/// 照這段唱得多快挑風格，不跟前兩段一樣（只避上一段的話，常常兩種來回換）
fn pick(rng: &mut Rng, cps: f32, recent: &[Look]) -> Look {
    // 每秒幾個字：慢歌大約 2～3，快歌 5 以上
    let fast = ((cps - 2.5) / 3.0).clamp(0.0, 1.0);
    let weight = |l: Look| match l {
        Look::Neon => 1.3,
        Look::Paper => 1.4 - 0.7 * fast,
        Look::Impact => 0.9 + 1.4 * fast,
        Look::Glitch => 0.9 + 1.0 * fast,
        Look::Drift => 1.5 - 1.2 * fast,
        Look::Type => 1.0 - 0.4 * fast,
        Look::Pop => 0.8 + 0.8 * fast,
        Look::Sign => 1.0,
        Look::Cinema => 1.2 - 0.9 * fast,
        Look::Retro => 0.8 + 0.8 * fast,
    };
    let cands: Vec<(Look, f32)> = LOOKS.iter().filter(|l| !recent.contains(l)).map(|&l| (l, weight(l))).collect();
    let total: f32 = cands.iter().map(|c| c.1).sum();
    let mut r = rng.f() * total;
    for &(l, w) in &cands {
        if r < w {
            return l;
        }
        r -= w;
    }
    cands.last().map_or(Look::Neon, |c| c.0)
}

/// 第 `i` 句屬於哪一段
pub fn segment_of(segs: &[Segment], i: usize) -> usize {
    segs.iter().rposition(|s| s.first <= i).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::Line;

    fn lyrics(times: &[i64]) -> Lyrics {
        Lyrics {
            synced: times.iter().map(|&t| Line { t, text: if t < 0 { String::new() } else { "我們一起唱歌".into() }, ..Default::default() }).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn 間奏切段而且相鄰三段風格都不同() {
        // 六句、隔 15 秒間奏、再六句
        let mut t: Vec<i64> = (0..6).map(|k| k * 3000).collect();
        t.extend((0..6).map(|k| 33_000 + k * 3000));
        let s = segments(&lyrics(&t), 7, None);
        assert_eq!(s.len(), 2, "{s:?}");
        assert_eq!(s[1].first, 6);
        for seed in 0..40 {
            let t: Vec<i64> = (0..40).map(|k| k * 2500).collect();
            let s = segments(&lyrics(&t), seed, None);
            assert!(s.len() >= 5);
            for w in s.windows(2) {
                assert_ne!(w[0].look, w[1].look);
                assert!(w[1].first - w[0].first <= SEG_MAX);
            }
            for w in s.windows(3) {
                assert_ne!(w[0].look, w[2].look);
            }
        }
    }

    #[test]
    fn 鎖定風格整首都用它() {
        let t: Vec<i64> = (0..20).map(|k| k * 2500).collect();
        let s = segments(&lyrics(&t), 3, Some(Look::Paper));
        assert!(s.iter().all(|x| x.look == Look::Paper));
        assert_eq!(segment_of(&s, 0), 0);
        assert_eq!(segment_of(&s, 19), s.len() - 1);
    }
}
