//! 段落場景：整段一起演的大場面，疊在每句的構圖與換句運鏡上。
//!
//! - 長鏡頭飛越（[`Scene::Flight`]）：句子沿一條路擺在大畫布上，鏡頭一路不停地往前（大致往右）：
//!   唱的時候只慢慢漂，換句時才滑一整個畫面過去；舊句不退場、自己滑出畫面，背景的光塵照遠近有視差。
//! - 透視縱深（[`Scene::Depth`]）：句子一句比一句深，鏡頭往前飛；新句在遠處小小地出來、慢慢變大，
//!   舊句放大、失焦、淡掉，從鏡頭旁邊過去。背景是一層層往外擴的框與星塵。
//! - 大字跑馬燈帶（[`Scene::Marquee`]）：兩條斜的帶子墊在歌詞後面，反方向捲著剛唱到的關鍵字。
//! - 歌詞堆疊牆（[`Scene::Wall`]）：下一句來的時候，舊句縮小貼到牆上、變暗留著，整段唱完一起退場。
//! - 轉盤（[`Scene::Carousel`]）：句子刻在畫面外一個大圓盤的邊上，換句時盤轉一格，舊句順著弧線斜到旁邊。
//! - 倒影（[`Scene::Mirror`]）：句子收到上半部，下半部是水面，字的倒影跟著水波晃，關鍵字唱到時水面漾開。
//! - 分鏡（[`Scene::Split`]）：畫面四格斜的漫畫框，唱完的句子縮進自己那格留著，第五句來時才讓位。
//! - 鐘擺（[`Scene::Pendulum`]）：句子掛在畫面上方的長擺臂上，左右輪流甩進中間，下一句來時往來的方向甩回去。
//! - 階梯（[`Scene::Cascade`]）：每來一句，舊句往斜上方退一階、縮小變暗，疊成一座文字階梯。
//! - 螺旋（[`Scene::Vortex`]）：像縱深一樣往前飛，但句子排在螺旋上，從內圈轉出來、對焦時轉正。
//! - 舞台追光（[`Scene::Spotlight`]）：壓暗，兩盞燈慢慢擺、光束裡的微塵亮起來，關鍵字唱到時正上方淡淡打一道光。
//! - 殘響（[`Scene::Echo`]）：關鍵字唱到後，空心的字從句子中心一層層往外擴散淡掉，底下一排對稱細柱跟拍子起伏。
//!
//! 每段配哪個場景由 [`super::vary::cast`] 決定：副歌每次換、優先特殊場景，主歌稍微偏一般的（[`Scene::Plain`]），
//! 相鄰兩段不同。[`Scene::persist`] 的那幾個場景，句子位置由這裡管（換句運鏡不動），
//! 跟構圖一樣只看歌詞與曲目，是時間的純函數。`VITRO_PV_SCENE=depth` 這樣可以整首鎖定一個場景（實測用）。

use std::collections::HashMap;
use std::f32::consts::PI;
use std::sync::OnceLock;

use super::cam::{self, Pose};
use super::layout::{Placed, Rng};
use super::style::{segment_of, style_at, Look, Segment, Style};
use super::{ease_out, glyph_inst, mix, noise, sdf, smooth01, with_a, Beat, Inst, Line as PvLine, LINGER, WIPE_LEAD};
use crate::gfx::Gpu;
use crate::lyrics::{Line, Lyrics};
use crate::text::Text;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Scene {
    /// 一般：每句自己構圖、換句時照運鏡移
    #[default]
    Plain,
    Flight,
    Depth,
    Marquee,
    Wall,
    Carousel,
    Mirror,
    Split,
    Pendulum,
    Cascade,
    Vortex,
    Spotlight,
    Echo,
}

pub const SCENES: [Scene; 13] = [
    Scene::Plain,
    Scene::Flight,
    Scene::Depth,
    Scene::Marquee,
    Scene::Wall,
    Scene::Carousel,
    Scene::Mirror,
    Scene::Split,
    Scene::Pendulum,
    Scene::Cascade,
    Scene::Vortex,
    Scene::Spotlight,
    Scene::Echo,
];

impl Scene {
    /// 句子留到段落結束才退場（不在下一句來的時候退），位置由場景管
    pub fn persist(self) -> bool {
        matches!(
            self,
            Scene::Flight | Scene::Depth | Scene::Wall | Scene::Carousel | Scene::Split | Scene::Pendulum | Scene::Cascade | Scene::Vortex
        )
    }

    /// 整段的句子都可能還在畫面上（貼在牆上、收在分鏡格裡、疊在階梯上），不是只有最近兩句
    pub fn keep_segment(self) -> bool {
        matches!(self, Scene::Wall | Scene::Split | Scene::Cascade)
    }

    /// 這個風格演不演這個場景：打字稿一直寫在同一張紙上，電影與紙本安靜，不往前衝也不墊大字帶
    pub fn fits(self, look: Look) -> bool {
        match self {
            Scene::Plain | Scene::Wall => true,
            Scene::Flight => look != Look::Type,
            Scene::Depth => !matches!(look, Look::Type | Look::Cinema | Look::Paper),
            Scene::Marquee => !matches!(look, Look::Type | Look::Cinema),
            Scene::Carousel => !matches!(look, Look::Type | Look::Cinema),
            // 倒影要乾淨的字面：粗黑邊、硬陰影的倒過來很髒
            Scene::Mirror => !matches!(look, Look::Type | Look::Pop | Look::Impact),
            Scene::Split => !matches!(look, Look::Type | Look::Cinema | Look::Drift),
            Scene::Pendulum | Scene::Vortex => !matches!(look, Look::Type | Look::Cinema | Look::Paper),
            Scene::Cascade => look != Look::Type,
            Scene::Spotlight => !matches!(look, Look::Paper | Look::Pop),
            Scene::Echo => !matches!(look, Look::Type | Look::Paper | Look::Cinema),
        }
    }
}

/// `VITRO_PV_SCENE` 鎖定的場景（只讀一次）
pub fn forced() -> Option<Scene> {
    static F: OnceLock<Option<Scene>> = OnceLock::new();
    *F.get_or_init(|| {
        let v = std::env::var("VITRO_PV_SCENE").ok()?;
        SCENES.iter().copied().find(|s| format!("{s:?}").eq_ignore_ascii_case(v.trim()))
    })
}

/// 一句在場景裡的位置
#[derive(Clone, Copy, Debug)]
struct Spot {
    line: usize,
    /// 飛越：在畫布上的位置。縱深：x、y 是側向位移（在對焦的深度量，邏輯 px）、a 是自己的歪斜。
    /// 堆疊牆：牆上的格子（繞畫面中心縮小、轉，再搬過去）
    pose: Pose,
    /// 縱深：多深（對焦的距離是 1）
    z: f32,
    /// 堆疊牆、分鏡：什麼時候收到牆上／格子裡（下一句的落點，最後一句是 `i64::MAX`）、收多久。
    /// 鐘擺、階梯：這句開始唱的時間
    t: i64,
    dur: f32,
    /// 分鏡：格子被後面的句子佔走、這句淡掉的時間。鐘擺：甩出去的時間。用不到是 `i64::MAX`
    gone: i64,
}

/// 鏡頭的關鍵影格：時間、參數、斜率（每 ms）。飛越是 (x, y, ln 縮放, 旋轉)，縱深是 (深度, 0, 0, 0)。
/// 飛越每句兩格：唱的時候從句子前一點漂到後一點，兩句之間才大段移動
#[derive(Clone, Copy, Debug)]
struct Key {
    t: f32,
    p: [f32; 4],
    m: [f32; 4],
}

/// 一段的場景
pub struct Track {
    pub scene: Scene,
    spots: Vec<Spot>,
    keys: Vec<Key>,
    /// 整段一起退場的時間，與下一段開始的時間（倒退鍵要在那之前刪完）
    pub exit: i64,
    pub next: i64,
    /// 這段開始的時間（跑馬燈從這時候滑進來）
    pub t: i64,
    /// 縱深：越深轉越多（弧度／單位深度），像轉著飛進隧道。轉盤、螺旋：往哪邊轉（±1）。
    /// 階梯：往哪邊疊（-1..1）
    spin: f32,
    /// 跑馬燈：兩條帶子上下對調。轉盤：圓心在上面。分鏡、鐘擺：左右對調
    flip: bool,
    /// 分鏡：第 k 句用第 `order[k % 4]` 格
    order: [usize; 4],
    w: f32,
    h: f32,
}

/// 這一刻一句在畫面上的樣子：搬到哪、透明度倍率、另外加的失焦（邏輯 px）
#[derive(Clone, Copy, Debug)]
pub struct Shown {
    pub pose: Pose,
    pub alpha: f32,
    pub blur: f32,
}

/// 縱深：相鄰兩句差多深（對焦距離的幾倍）
const DZ: f32 = 0.5;
/// 句子收到牆上最多花多久（ms）
const WALL_MS: f32 = 700.0;

/// 這句最後一個字唱完的時間（跟構圖算的一樣：有逐字照逐字，沒有就用到句長的八成）
fn sung_end(l: &Line, next: i64) -> i64 {
    l.words.as_ref().and_then(|w| w.last()).map_or(l.t + (next - l.t) * 8 / 10, |w| w.t + w.d)
}

fn next_t(ly: &Lyrics, i: usize) -> i64 {
    ly.synced.get(i + 1).map_or(ly.synced[i].t + 5000, |n| n.t)
}

/// 鏡頭正對這句的時間：唱到一半的時候
fn mid(ly: &Lyrics, i: usize) -> f32 {
    let l = &ly.synced[i];
    (cam::first_t(l) + sung_end(l, next_t(ly, i)).min(next_t(ly, i))) as f32 * 0.5
}

/// 每段的場景，跟 `segs` 一一對應
pub fn tracks(ly: &Lyrics, segs: &[Segment], seed: u64, w: f32, h: f32) -> Vec<Track> {
    segs.iter()
        .enumerate()
        .map(|(si, s)| {
            let end = segs.get(si + 1).map_or(ly.synced.len(), |n| n.first);
            let lines: Vec<usize> = (s.first..end).filter(|&i| !ly.synced[i].text.trim().is_empty()).collect();
            // 整段跟著最後一句退場：唱完待一陣子，或下一句（下一段）來了
            let (exit, next) = lines.last().map_or((s.t, s.t), |&l| {
                let n = next_t(ly, l);
                (n.min(sung_end(&ly.synced[l], n) + LINGER), n)
            });
            let mut rng = Rng::new(seed ^ 0x5CE7_1000 ^ (si as u64).wrapping_mul(0x9E37_79B9));
            let mut tr =
                Track {
                scene: s.scene,
                spots: Vec::new(),
                keys: Vec::new(),
                exit,
                next,
                t: s.t,
                spin: 0.0,
                flip: rng.chance(0.5),
                order: [0, 1, 2, 3],
                w,
                h,
            };
            match s.scene {
                Scene::Flight => flight(&mut tr, ly, &lines, &mut rng),
                Scene::Depth => depth(&mut tr, ly, &lines, &mut rng),
                Scene::Wall => wall(&mut tr, ly, &lines, &mut rng),
                Scene::Carousel => carousel(&mut tr, ly, &lines, &mut rng),
                Scene::Split => split(&mut tr, ly, &lines, &mut rng),
                Scene::Pendulum => pendulum(&mut tr, ly, &lines),
                Scene::Cascade => cascade(&mut tr, ly, &lines, &mut rng),
                Scene::Vortex => vortex(&mut tr, ly, &lines, &mut rng),
                Scene::Plain | Scene::Marquee | Scene::Mirror | Scene::Spotlight | Scene::Echo => {}
            }
            tr
        })
        .collect()
}

/// 飛越：每句相對上一句往前走將近一個畫面（不跟上一句疊在一起），方向慢慢轉（大致往右）。
/// 唱的時候鏡頭從句子前一點漂到後一點，句子一直在畫面裡；唱完那段（多半是拖長的尾音）才滑到下一句。
/// 唱得快的句子走短一點，鏡頭不會太趕
fn flight(tr: &mut Track, ly: &Lyrics, lines: &[usize], rng: &mut Rng) {
    let (w, h) = (tr.w, tr.h);
    let mut p = [0.0f32; 4];
    let mut head = rng.range(-0.3, 0.3);
    let mut last = f32::NEG_INFINITY;
    for (k, &i) in lines.iter().enumerate() {
        let r = [rng.f(), rng.f(), rng.f(), rng.f()];
        let land = cam::first_t(&ly.synced[i]) as f32;
        let next = lines.get(k + 1).map_or(next_t(ly, i), |&n| cam::first_t(&ly.synced[n])) as f32;
        if k > 0 {
            head = (head + (r[0] - 0.5)).clamp(-0.55, 0.55);
            let d = (0.82 + 0.1 * r[1]) * ((land - last) / 3000.0).clamp(0.75, 1.0);
            p[0] += head.cos() * w * d;
            p[1] += head.sin() * h * d * 1.2;
            p[2] = (r[2] - 0.6) * 0.2;
            p[3] = (r[3] - 0.5) * 0.14;
        }
        let (dx, dy) = (head.cos() * w * 0.05, head.sin() * h * 0.05);
        let ta = land.max(last + 300.0);
        let tb = (land + 0.6 * (next - land)).max(ta + 300.0);
        last = tb;
        tr.keys.push(Key { t: ta, p: [p[0] - dx, p[1] - dy, p[2], p[3]], m: [0.0; 4] });
        tr.keys.push(Key { t: tb, p: [p[0] + dx, p[1] + dy, p[2], p[3]], m: [0.0; 4] });
        tr.spots.push(Spot { line: i, pose: Pose { s: p[2].exp(), a: p[3], x: p[0], y: p[1] }, z: 0.0, t: 0, dur: 0.0, gone: i64::MAX });
    }
    tangents(&mut tr.keys, [w * 0.55 / 4000.0, 0.0, 0.0, 0.0]);
}

/// 縱深：一句比一句深，左右錯開（鏡頭從句子旁邊飛過去，不是穿過字）
fn depth(tr: &mut Track, ly: &Lyrics, lines: &[usize], rng: &mut Rng) {
    let (w, h) = (tr.w, tr.h);
    tr.spin = if rng.chance(0.5) { 0.14 } else { -0.14 };
    let side0 = if tr.flip { -1.0 } else { 1.0 };
    let mut z = 0.0;
    let mut prev: Option<f32> = None;
    for (k, &i) in lines.iter().enumerate() {
        let r = [rng.f(), rng.f(), rng.f()];
        let mut t = mid(ly, i);
        if let Some(pt) = prev {
            t = t.max(pt + 300.0);
            z += DZ * ((t - pt) / 3500.0).clamp(0.6, 1.0);
        }
        prev = Some(t);
        let side = if k % 2 == 0 { side0 } else { -side0 };
        let pose = Pose { s: 1.0, a: (r[2] - 0.5) * 0.1, x: side * w * (0.02 + 0.05 * r[0]), y: (r[1] - 0.5) * 0.08 * h };
        tr.keys.push(Key { t, p: [z - 1.0, 0.0, 0.0, 0.0], m: [0.0; 4] });
        tr.spots.push(Spot { line: i, pose, z, t: 0, dur: 0.0, gone: i64::MAX });
    }
    tangents(&mut tr.keys, [DZ / 4000.0, 0.0, 0.0, 0.0]);
}

/// 堆疊牆：畫面切成 3 × 4 格，舊句照亂數的順序貼上去；正中間那兩格在新句後面，最後才用
fn wall(tr: &mut Track, ly: &Lyrics, lines: &[usize], rng: &mut Rng) {
    let (w, h) = (tr.w, tr.h);
    let mut cells: Vec<(usize, usize)> = (0..4).flat_map(|r| (0..3).map(move |c| (c, r))).collect();
    for k in (1..cells.len()).rev() {
        let j = ((rng.f() * (k + 1) as f32) as usize).min(k);
        cells.swap(k, j);
    }
    cells.sort_by_key(|&(c, r)| c == 1 && (r == 1 || r == 2));
    for (k, &i) in lines.iter().enumerate() {
        let (c, r) = cells[k % cells.len()];
        // 左右兩欄往內收一點，整句才不會超出畫面
        let cx = (0.5 + (c as f32 - 1.0) * 0.31 + rng.range(-0.025, 0.025)) * w;
        let cy = ((r as f32 + 0.5) / 4.0 + rng.range(-0.02, 0.02)) * h;
        let pose = Pose { s: rng.range(0.28, 0.33), a: rng.range(-0.08, 0.08), x: cx - w * 0.5, y: cy - h * 0.5 };
        let land = cam::first_t(&ly.synced[i]);
        let (t, dur) = match lines.get(k + 1) {
            Some(&n) => {
                let t = cam::first_t(&ly.synced[n]);
                (t, WALL_MS.min((t - land) as f32 * 0.5).max(1.0))
            }
            None => (i64::MAX, WALL_MS),
        };
        tr.spots.push(Spot { line: i, pose, z: 0.0, t, dur, gone: i64::MAX });
    }
}

/// 轉盤：相鄰兩句在盤上差幾弧度、盤的半徑（畫面高的幾倍）
const ARC: f32 = 0.72;
const DISC_R: f32 = 1.35;
/// 鐘擺：擺長（畫面高的幾倍；支點在畫面上方外面）
const ARM: f32 = 1.15;
/// 螺旋：相鄰兩句差多深
const VZ: f32 = 0.55;
/// 倒影的水面在畫面高的哪裡
pub const WATER: f32 = 0.7;

/// 轉盤：句子刻在一個大圓盤的邊上，圓心在畫面下面外側（或上面）。唱的時候盤只慢慢轉，
/// 換句時才轉一格把新句轉到正中間；前一兩句順著弧線斜斜地待在旁邊，轉遠了就淡掉
fn carousel(tr: &mut Track, ly: &Lyrics, lines: &[usize], rng: &mut Rng) {
    // 往哪邊轉（新句從右邊還是左邊轉上來）；`flip` 決定圓心在下面還是上面
    let dir = if rng.chance(0.5) { 1.0 } else { -1.0 };
    tr.spin = dir;
    tr.flip = rng.chance(0.3);
    let mut last = f32::NEG_INFINITY;
    for (k, &i) in lines.iter().enumerate() {
        let th = k as f32 * ARC * dir;
        let land = cam::first_t(&ly.synced[i]) as f32;
        let next = lines.get(k + 1).map_or(next_t(ly, i), |&n| cam::first_t(&ly.synced[n])) as f32;
        // 唱完才轉走（慢歌一句拖得長，照句長的比例轉的話字還沒唱完就被轉到旁邊），但至少留半秒轉過去
        let sung = sung_end(&ly.synced[i], next as i64) as f32 + 150.0;
        let ta = land.max(last + 300.0);
        let tb = sung.min(next - 500.0).max(land + 0.5 * (next - land)).max(ta + 300.0);
        last = tb;
        tr.keys.push(Key { t: ta, p: [th - 0.04 * dir, 0.0, 0.0, 0.0], m: [0.0; 4] });
        tr.keys.push(Key { t: tb, p: [th + 0.04 * dir, 0.0, 0.0, 0.0], m: [0.0; 4] });
        tr.spots.push(Spot { line: i, pose: Pose::ID, z: th, t: 0, dur: 0.0, gone: i64::MAX });
    }
    tangents(&mut tr.keys, [ARC * dir / 4000.0, 0.0, 0.0, 0.0]);
}

/// 分鏡格：畫面分成四格（斜的），格子的順序亂排。(中心 x、y 佔畫面的幾成, 歪多少)
fn panels(flip: bool) -> [(f32, f32, f32); 4] {
    let f = if flip { -1.0 } else { 1.0 };
    [(-0.25 * f, -0.235, -0.05 * f), (0.25 * f, -0.215, 0.035 * f), (-0.245 * f, 0.235, 0.03 * f), (0.25 * f, 0.225, -0.045 * f)]
}

/// 分鏡格的大小（佔畫面寬、高的幾成）
const PANEL: (f32, f32) = (0.47, 0.42);

/// 分鏡：正在唱的句子在中間（稍微偏向它的格子）；下一句來的時候縮進自己那格留著，
/// 四格輪著用，格子被第五句佔走時舊的那句才淡掉
fn split(tr: &mut Track, ly: &Lyrics, lines: &[usize], rng: &mut Rng) {
    let (w, h) = (tr.w, tr.h);
    let mut order = [0usize, 1, 2, 3];
    for k in (1..4).rev() {
        let j = ((rng.f() * (k + 1) as f32) as usize).min(k);
        order.swap(k, j);
    }
    // 存的是格子編號，背景畫格子要用
    tr.order = order;
    let cells = panels(tr.flip);
    for (k, &i) in lines.iter().enumerate() {
        let (px, py, pa) = cells[order[k % 4]];
        let pose = Pose { s: 0.4, a: pa, x: px * w, y: py * h };
        let land = cam::first_t(&ly.synced[i]);
        let (t, dur) = match lines.get(k + 1) {
            Some(&n) => {
                let t = cam::first_t(&ly.synced[n]);
                (t, 600f32.min((t - land) as f32 * 0.5).max(1.0))
            }
            None => (i64::MAX, 600.0),
        };
        tr.spots.push(Spot { line: i, pose, z: 0.0, t, dur, gone: i64::MAX });
    }
    // 第 k+4 句收進同一格的那一刻，第 k 句讓位
    for k in 0..tr.spots.len() {
        tr.spots[k].gone = tr.spots.get(k + 4).map_or(i64::MAX, |s| s.t);
    }
}

/// 鐘擺：句子掛在畫面上方的長擺臂上，一句從左、一句從右甩進中間，唱的時候輕輕晃；
/// 下一句從另一邊甩進來時，這句往自己來的那邊甩回去（不跟新句交叉）
fn pendulum(tr: &mut Track, ly: &Lyrics, lines: &[usize]) {
    let side0 = if tr.flip { -1.0 } else { 1.0 };
    for (k, &i) in lines.iter().enumerate() {
        let side = if k % 2 == 0 { side0 } else { -side0 };
        let land = cam::first_t(&ly.synced[i]);
        let gone = lines.get(k + 1).map_or(tr.exit, |&n| cam::first_t(&ly.synced[n]));
        tr.spots.push(Spot { line: i, pose: Pose::ID, z: side, t: land, dur: 0.0, gone });
    }
}

/// 階梯：新句在前面偏下，每來一句，舊句就往斜上方退一階、縮小、變暗、失焦，最多留四五階
fn cascade(tr: &mut Track, ly: &Lyrics, lines: &[usize], rng: &mut Rng) {
    tr.spin = [-1.0, -0.55, 0.55, 1.0][((rng.f() * 4.0) as usize).min(3)];
    for &i in lines {
        tr.spots.push(Spot { line: i, pose: Pose::ID, z: 0.0, t: cam::first_t(&ly.synced[i]), dur: 0.0, gone: i64::MAX });
    }
}

/// 螺旋：跟縱深一樣一句比一句深、鏡頭往前飛，但句子排在一條螺旋上（每句轉將近 80°），
/// 遠的新句從螺旋內圈斜斜地轉出來、到正中間時剛好轉正，舊句順著外圈轉出去
fn vortex(tr: &mut Track, ly: &Lyrics, lines: &[usize], rng: &mut Rng) {
    tr.spin = if rng.chance(0.5) { 1.0 } else { -1.0 };
    let mut z = 0.0;
    let mut prev: Option<f32> = None;
    for (k, &i) in lines.iter().enumerate() {
        let mut t = mid(ly, i);
        if let Some(pt) = prev {
            t = t.max(pt + 300.0);
            z += VZ * ((t - pt) / 3500.0).clamp(0.6, 1.0);
        }
        prev = Some(t);
        let pose = Pose { s: 1.0, a: k as f32 * 1.35 * tr.spin, x: 0.0, y: 0.0 };
        tr.keys.push(Key { t, p: [z - 1.0, 0.0, 0.0, 0.0], m: [0.0; 4] });
        tr.spots.push(Spot { line: i, pose, z, t: 0, dur: 0.0, gone: i64::MAX });
    }
    tangents(&mut tr.keys, [VZ / 4000.0, 0.0, 0.0, 0.0]);
}

/// 兩個樣子之間的 `u`（0..1），縮放照比例走
fn lerp_pose(a: Pose, b: Pose, u: f32) -> Pose {
    Pose { s: a.s * (b.s / a.s).powf(u), a: mix1(a.a, b.a, u), x: mix1(a.x, b.x, u), y: mix1(a.y, b.y, u) }
}

fn mix1(a: f32, b: f32, u: f32) -> f32 {
    a + (b - a) * u
}

/// 每個關鍵影格的斜率：兩邊斜率的調和平均（Fritsch–Carlson），速度連續、不會衝過頭往回走
/// （漂的那段很慢、滑的那段很快，用一般的 Catmull-Rom 會在漂的那段倒退）。頭尾用單邊的斜率，只有一格就用 `v`
fn tangents(keys: &mut [Key], v: [f32; 4]) {
    let n = keys.len();
    let slope = |a: &Key, b: &Key, c: usize| (b.p[c] - a.p[c]) / (b.t - a.t);
    for k in 0..n {
        keys[k].m = match (k.checked_sub(1), (k + 1 < n).then_some(k + 1)) {
            (None, None) => v,
            (Some(a), None) => std::array::from_fn(|c| slope(&keys[a], &keys[k], c)),
            (None, Some(b)) => std::array::from_fn(|c| slope(&keys[k], &keys[b], c)),
            (Some(a), Some(b)) => std::array::from_fn(|c| {
                let (l, r) = (slope(&keys[a], &keys[k], c), slope(&keys[k], &keys[b], c));
                if l * r > 0.0 { 2.0 * l * r / (l + r) } else { 0.0 }
            }),
        };
    }
}

/// 鏡頭在 `t` 的參數。頭尾外面照最後的速度直直走（只走位置，縮放與旋轉停住）
fn eval(keys: &[Key], t: f32) -> [f32; 4] {
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else { return [0.0; 4] };
    let lin = |k: &Key, dt: f32| [k.p[0] + k.m[0] * dt, k.p[1] + k.m[1] * dt, k.p[2], k.p[3]];
    if t <= first.t {
        return lin(first, t - first.t);
    }
    if t >= last.t {
        return lin(last, t - last.t);
    }
    let k = keys.partition_point(|x| x.t <= t) - 1;
    let (a, b) = (keys[k], keys[k + 1]);
    let dt = b.t - a.t;
    let u = (t - a.t) / dt;
    let (u2, u3) = (u * u, u * u * u);
    let (h00, h10, h01, h11) = (2.0 * u3 - 3.0 * u2 + 1.0, u3 - 2.0 * u2 + u, -2.0 * u3 + 3.0 * u2, u3 - u2);
    std::array::from_fn(|c| h00 * a.p[c] + h10 * dt * a.m[c] + h01 * b.p[c] + h11 * dt * b.m[c])
}

impl Track {
    fn spot(&self, i: usize) -> Option<&Spot> {
        self.spots.binary_search_by_key(&i, |s| s.line).ok().map(|k| &self.spots[k])
    }

    /// 飛越的鏡頭（畫布上框住哪裡）
    fn flight_cam(&self, ms: i64) -> Pose {
        let p = eval(&self.keys, ms as f32);
        Pose { s: p[2].exp(), a: p[3], x: p[0], y: p[1] }
    }

    /// 縱深的鏡頭在多深
    fn depth_z(&self, ms: i64) -> f32 {
        eval(&self.keys, ms as f32)[0]
    }

    /// 第 `i` 句這一刻在哪；不歸場景管（一般、跑馬燈、空行）是 `None`
    pub fn shown(&self, i: usize, ms: i64) -> Option<Shown> {
        let sp = self.spot(i)?;
        let (w, h) = (self.w, self.h);
        Some(match self.scene {
            Scene::Flight => {
                let pose = self.flight_cam(ms).inv().mul(sp.pose);
                // 離畫面中心越遠越淡一點（正在唱的那句比較顯眼），出了畫面就不畫
                let d = (pose.x / w).hypot(pose.y / h);
                let alpha = if d > 1.5 { 0.0 } else { 1.0 - 0.3 * smooth01((d - 0.3) / 0.5) };
                Shown { pose, alpha, blur: 0.0 }
            }
            Scene::Depth => {
                let dz = sp.z - self.depth_z(ms);
                if dz <= 0.25 {
                    return Some(Shown { pose: Pose::ID, alpha: 0.0, blur: 0.0 });
                }
                let s = 1.0 / dz;
                let a = sp.pose.a + self.spin * (dz - 1.0);
                let (sn, cs) = a.sin_cos();
                let (x, y) = ((sp.pose.x * cs - sp.pose.y * sn) * s, (sp.pose.x * sn + sp.pose.y * cs) * s);
                // 太近：失焦、淡掉；太遠：還看不清楚就先淡著
                // 下一句出來的時候（大約 1.33 倍）開始淡，不跟新句疊太久
                let alpha = (1.0 - smooth01((s - 1.3) / 0.55)) * smooth01((s - 0.2) / 0.2);
                Shown { pose: Pose { s, a, x, y }, alpha, blur: (s - 1.2).max(0.0) * 7.0 }
            }
            Scene::Wall => {
                if sp.t == i64::MAX {
                    return Some(Shown { pose: Pose::ID, alpha: 1.0, blur: 0.0 });
                }
                let u = cam::glide((ms as f32 - (sp.t as f32 - sp.dur * 0.5)) / sp.dur);
                Shown { pose: sp.pose.part(u), alpha: 1.0 - 0.66 * u, blur: 2.0 * u }
            }
            Scene::Carousel => {
                let d = sp.z - self.disc_angle(ms);
                let ad = d.abs();
                if ad > 1.6 {
                    return Some(Shown { pose: Pose::ID, alpha: 0.0, blur: 0.0 });
                }
                let (sg, r) = (self.disc_sign(), h * DISC_R);
                let pose = Pose { s: 1.0 - 0.3 * smooth01(ad), a: sg * d, x: r * d.sin(), y: sg * r * (1.0 - d.cos()) };
                let alpha = (1.0 - 0.6 * smooth01((ad - 0.15) / 0.6)) * (1.0 - smooth01((ad - 1.05) / 0.45));
                Shown { pose, alpha, blur: 3.5 * smooth01((ad - 0.2) / 0.8) }
            }
            Scene::Split => {
                // 唱的時候在中間、稍微偏向自己的格子
                let a = Pose { s: 0.88, a: sp.pose.a * 0.3, x: sp.pose.x * 0.15, y: sp.pose.y * 0.15 };
                let u = if sp.t == i64::MAX { 0.0 } else { cam::glide((ms as f32 - (sp.t as f32 - sp.dur * 0.5)) / sp.dur) };
                let out = 1.0 - smooth01((ms - (sp.gone - 200)) as f32 / 400.0);
                Shown { pose: lerp_pose(a, sp.pose, u), alpha: (1.0 - 0.42 * u) * out, blur: 1.2 * u }
            }
            Scene::Pendulum => {
                let (th, alpha, blur) = self.swing(sp, ms);
                let l = h * ARM;
                Shown { pose: Pose { s: 1.0, a: -th * 0.9, x: l * th.sin(), y: l * th.cos() - l }, alpha, blur }
            }
            Scene::Cascade => {
                let d = self.step_depth(i, ms);
                if d > 3.4 {
                    return Some(Shown { pose: Pose::ID, alpha: 0.0, blur: 0.0 });
                }
                let wave = (ms as f32 / 240.0 - d * 0.9).sin() * 5.0 * d.min(1.0);
                // 一階的高度要比一句（直書的也算）高，舊句才不會壓在新句上
                let pose = Pose { s: 0.75f32.powf(d), a: self.spin * 0.02 * d, x: self.spin * w * 0.07 * d, y: h * 0.2 - h * 0.22 * d + wave };
                let alpha = (1.0 - 0.2 * d).max(0.0) * (1.0 - smooth01((d - 2.6) / 0.8));
                Shown { pose, alpha, blur: d * 1.5 }
            }
            Scene::Vortex => {
                let dz = sp.z - self.depth_z(ms);
                if dz <= 0.25 {
                    return Some(Shown { pose: Pose::ID, alpha: 0.0, blur: 0.0 });
                }
                let s = 1.0 / dz;
                // 對焦的那一刻（dz = 1）剛好在正中間、轉正；遠的在內圈、近的往外圈甩
                let turn = (dz - 1.0) * 1.1 * self.spin;
                let r = (dz - 1.0) * h * 0.3 * s;
                let ang = sp.pose.a + turn;
                let alpha = (1.0 - smooth01((s - 1.3) / 0.6)) * smooth01((s - 0.25) / 0.25);
                Shown { pose: Pose { s, a: turn * 0.5, x: r * ang.cos(), y: r * ang.sin() }, alpha, blur: (s - 1.2).max(0.0) * 6.0 }
            }
            Scene::Plain | Scene::Marquee | Scene::Mirror | Scene::Spotlight | Scene::Echo => return None,
        })
    }

    /// 不歸場景管的句子，照換句運鏡擺好之後再整個搬一次：倒影把句子往上收，留下半部當水面
    pub fn lift(&self) -> Pose {
        match self.scene {
            Scene::Mirror => Pose { s: 0.86, a: 0.0, x: 0.0, y: -self.h * 0.13 },
            _ => Pose::ID,
        }
    }

    /// 轉盤轉到哪（弧度）
    fn disc_angle(&self, ms: i64) -> f32 {
        eval(&self.keys, ms as f32)[0]
    }

    /// 轉盤的圓心在下面是 1、上面是 -1
    fn disc_sign(&self) -> f32 {
        if self.flip { -1.0 } else { 1.0 }
    }

    /// 鐘擺這一刻的擺角（往右為正）、透明度、失焦。甩進來的時候帶一點衰減的來回，
    /// 唱的時候輕輕晃，甩出去的時候加速、淡掉
    fn swing(&self, sp: &Spot, ms: i64) -> (f32, f32, f32) {
        let side = sp.z;
        let u = ((ms - (sp.t - 150)) as f32 / 800.0).clamp(0.0, 1.0);
        let mut th = side * 0.62 * (-4.5 * u).exp() * (u * 2.6 * PI).cos();
        th += side * 0.025 * ((ms - sp.t) as f32 / 520.0).sin() * smooth01(u * 1.5);
        let o = ((ms - (sp.gone - 250)) as f32 / 650.0).clamp(0.0, 1.0);
        if o <= 0.0 {
            return (th, 1.0, 0.0);
        }
        (th * (1.0 - o) + side * 0.75 * o * o * o, 1.0 - smooth01(o * 1.25), 7.0 * o)
    }

    /// 階梯：第 `i` 句退了幾階（後面每一句開始唱就退一階，中間平滑地過去）
    fn step_depth(&self, i: usize, ms: i64) -> f32 {
        let Ok(k) = self.spots.binary_search_by_key(&i, |s| s.line) else { return 0.0 };
        self.spots[k + 1..]
            .iter()
            .take_while(|o| ms >= o.t - 250)
            .map(|o| cam::glide((ms - (o.t - 250)) as f32 / 520.0))
            .sum()
    }
}

/// 場景的背景（飛越的光塵、縱深的框與星塵、跑馬燈帶），`k` 是整組的濃淡（換段時交叉淡化）
#[allow(clippy::too_many_arguments)]
pub(super) fn back(
    out: &mut Vec<Inst>,
    atlas: &mut sdf::Atlas,
    gpu: &Gpu,
    text: &mut Text,
    tr: &Track,
    segs: &[Segment],
    si: usize,
    lines: &HashMap<usize, PvLine>,
    k: f32,
    ms: i64,
    beat: &Beat,
    accent: [f32; 4],
    px: f32,
) {
    if k <= 0.003 {
        return;
    }
    let st = style_at(segs, si);
    let accent = st.accent(accent);
    match tr.scene {
        Scene::Flight => dust(out, tr, k, ms, beat, accent),
        Scene::Depth => tunnel(out, tr, &st, k, ms, beat, accent),
        Scene::Marquee => marquee(out, atlas, gpu, text, tr, &st, segs, si, lines, k, ms, beat, accent, px),
        Scene::Carousel => astrolabe(out, tr, &st, k, ms, beat),
        Scene::Mirror => water(out, tr, segs, si, lines, k, ms, beat, accent),
        Scene::Split => frames(out, tr, &st, k, ms, accent),
        Scene::Pendulum => rig(out, tr, &st, k, ms, beat),
        Scene::Cascade => trays(out, tr, &st, lines, k, ms),
        Scene::Vortex => arms(out, tr, k, ms, beat, accent),
        Scene::Spotlight => stage(out, tr, segs, si, lines, k, ms, accent),
        Scene::Echo => meter(out, tr, &st, k, ms, beat),
        Scene::Plain | Scene::Wall => {}
    }
}

/// 轉盤：文字帶上下兩道細細的弧（圓的大部分在畫面外），中間一圈跟著盤轉的刻度。都很淡，只是給個軌道
fn astrolabe(out: &mut Vec<Inst>, tr: &Track, st: &Style, k: f32, ms: i64, beat: &Beat) {
    let (w, h) = (tr.w, tr.h);
    let (sg, r) = (tr.disc_sign(), h * DISC_R);
    let (cx, cy) = (w * 0.5, h * 0.5 + sg * r);
    let col = st.line;
    for (dr, a) in [(-0.3, 0.1), (-0.335, 0.07), (0.3, 0.07)] {
        out.push(Inst::ring(cx, cy, r + dr * h, 1.2, with_a(col, a * k)));
    }
    // 刻度：在內側兩道弧中間，每 0.06 弧度一格、每五格長一點；離畫面中間越遠越淡
    let cam_a = tr.disc_angle(ms);
    let rr = r - 0.3175 * h;
    let step = 0.06;
    let (j0, j1) = (((cam_a - 1.0) / step).ceil() as i64, ((cam_a + 1.0) / step).floor() as i64);
    for j in j0..=j1 {
        let phi = j as f32 * step - cam_a;
        let fade = 1.0 - smooth01((phi.abs() - 0.55) / 0.45);
        if fade <= 0.0 {
            continue;
        }
        let len = h * if j.rem_euclid(5) == 0 { 0.026 } else { 0.012 } * (1.0 + 0.15 * beat.env);
        let (x, y) = (cx + rr * phi.sin(), cy - sg * rr * phi.cos());
        out.push(Inst::rect(x, y, 1.5, len, sg * phi, with_a(col, 0.16 * fade * k)));
    }
}

/// 倒影：水面一道細光、水面下壓暗，幾條很淡的水紋慢慢漂；關鍵字唱到時水面從那個字底下往兩邊漾開。
/// 字的倒影本身在 `mod.rs` 畫（要照字這一刻的樣子翻過來）
#[allow(clippy::too_many_arguments)]
fn water(
    out: &mut Vec<Inst>,
    tr: &Track,
    segs: &[Segment],
    si: usize,
    lines: &HashMap<usize, PvLine>,
    k: f32,
    ms: i64,
    beat: &Beat,
    accent: [f32; 4],
) {
    let (w, h) = (tr.w, tr.h);
    let wy = h * WATER;
    let big = 2.0 * (w + h);
    out.push(Inst::grad(w * 0.5, wy + h * 0.08, big, PI * 0.5, [0.0; 4], [0.0, 0.0, 0.0, 0.24 * k], h * 0.12));
    let shine = mix(accent, [1.0; 4], 0.55);
    out.push(Inst::bar(w * 0.5, wy, w * 1.3, 16.0, 0.0, with_a(shine, 0.07 * k), 0.0));
    out.push(Inst::bar(w * 0.5, wy, w * 1.3, 3.0, 0.0, with_a(shine, 0.32 * k), 0.0));
    let t = ms as f32 / 1000.0;
    for j in 0..9u64 {
        let f = noise(j, 61);
        let y = wy + h * (0.03 + 0.26 * f * f);
        let len = w * (0.12 + 0.18 * noise(j, 62));
        let x = (noise(j, 63) * w * 1.4 + t * w * (0.006 + 0.012 * noise(j, 64))).rem_euclid(w * 1.4) - w * 0.2;
        out.push(Inst::bar(x, y, len, 2.0, 0.0, with_a(shine, k * (0.05 + 0.03 * beat.env) * (1.0 - f)), 0.0));
    }
    // 關鍵字唱到：水面上一道往兩邊拉開的光（位置照倒影搬過的句子）
    let lift = tr.lift();
    let c = (w * 0.5, h * 0.5);
    for (_, l) in lines.iter().filter(|(i, _)| segment_of(segs, **i) == si) {
        for p in l.lay.glyphs.iter().filter(|p| p.key && ms >= p.t && ms < p.t + 1300) {
            let q = (ms - p.t) as f32 / 1300.0;
            let (x, _) = lift.map(p.x, p.y, c);
            let len = p.size * (1.0 + 7.0 * ease_out(q));
            out.push(Inst::bar(x, wy + 5.0, len, 3.0, 0.0, with_a(shine, 0.3 * (1.0 - q).powi(2) * k), 0.0));
        }
    }
}

/// 分鏡：四格斜的框，段落開頭一格一格畫出來；剛收進一句的那格底下亮一下強調色
fn frames(out: &mut Vec<Inst>, tr: &Track, st: &Style, k: f32, ms: i64, accent: [f32; 4]) {
    let (w, h) = (tr.w, tr.h);
    let cells = panels(tr.flip);
    for (o, &(px, py, pa)) in cells.iter().enumerate() {
        let order = tr.order.iter().position(|&c| c == o).unwrap_or(o);
        let show = smooth01((ms - (tr.t - WIPE_LEAD) - order as i64 * 120) as f32 / 500.0);
        if show <= 0.0 {
            continue;
        }
        let (cx, cy) = (w * 0.5 + px * w, h * 0.5 + py * h);
        let (pw, ph) = (w * PANEL.0, h * PANEL.1);
        // 這格現在收著的句子（最近收進來的那句）
        let held = tr.spots.iter().enumerate().filter(|(n, s)| tr.order[n % 4] == o && s.t <= ms && ms < s.gone).map(|(_, s)| s.t).max();
        if let Some(t) = held {
            let flash = 1.0 - smooth01((ms - t) as f32 / 900.0);
            out.push(Inst::rect(cx, cy, pw, ph, pa, with_a(accent, (0.035 + 0.1 * flash) * show * k)));
        }
        let mut f = Inst::rect(cx, cy, pw * (0.96 + 0.04 * show), ph * (0.96 + 0.04 * show), pa, with_a(st.line, 0.3 * show * k));
        f.fx[2] = 0.0;
        f.line = [st.line[0], st.line[1], st.line[2], 3.0];
        out.push(f);
    }
}

/// 鐘擺：從畫面上方外面的支點拉到句子上方的細擺臂（不穿過字），末端一個小軸承
fn rig(out: &mut Vec<Inst>, tr: &Track, st: &Style, k: f32, ms: i64, beat: &Beat) {
    let (w, h) = (tr.w, tr.h);
    let l = h * ARM;
    let (px, py) = (w * 0.5, h * 0.5 - l);
    let len = l - h * 0.24;
    for sp in &tr.spots {
        if ms < sp.t - 300 || ms > sp.gone + 500 {
            continue;
        }
        let (th, alpha, _) = tr.swing(sp, ms);
        if alpha <= 0.003 {
            continue;
        }
        let (dx, dy) = (th.sin(), th.cos());
        let (ex, ey) = (px + dx * len, py + dy * len);
        out.push(Inst::bar(px + dx * len * 0.5, py + dy * len * 0.5, len, 2.0, dy.atan2(dx), with_a(st.line, 0.3 * alpha * k), 0.0));
        out.push(Inst::ring(ex, ey, 7.0 * (1.0 + 0.15 * beat.env), 1.5, with_a(st.line, 0.4 * alpha * k)));
    }
}

/// 階梯：每一階後面一個很淡的空心托盤，亮度從前往後慢慢傳過去
fn trays(out: &mut Vec<Inst>, tr: &Track, st: &Style, lines: &HashMap<usize, PvLine>, k: f32, ms: i64) {
    let (w, h) = (tr.w, tr.h);
    let c = (w * 0.5, h * 0.5);
    for sp in &tr.spots {
        let Some(l) = lines.get(&sp.line) else { continue };
        if ms < sp.t - 300 {
            continue;
        }
        let Some(s) = tr.shown(sp.line, ms) else { continue };
        if s.alpha <= 0.003 {
            continue;
        }
        let d = tr.step_depth(sp.line, ms);
        let (x, y) = s.pose.map(l.lay.cx, l.lay.cy, c);
        let glow = 0.5 + 0.5 * (ms as f32 / 400.0 - d * 1.2).sin();
        let mut f = Inst::rect(x, y, w * 0.66 * s.pose.s, h * 0.22 * s.pose.s, s.pose.a, with_a(st.line, (0.06 + 0.08 * glow) * s.alpha * k));
        f.fx[2] = 0.0;
        f.line = [st.line[0], st.line[1], st.line[2], 1.2];
        out.push(f);
    }
}

/// 螺旋：三條旋臂上的彗星光條，跟著鏡頭往外流、繞中心轉
fn arms(out: &mut Vec<Inst>, tr: &Track, k: f32, ms: i64, beat: &Beat, accent: [f32; 4]) {
    let (w, h) = (tr.w, tr.h);
    let zc = tr.depth_z(ms);
    let col = mix(accent, [1.0; 4], 0.4);
    for j in 0..48u64 {
        let arm = (j % 3) as f32;
        let u = (noise(j, 71) + zc * 0.35).rem_euclid(1.0);
        let r = h * (0.06 + 0.85 * u * u);
        let phi = arm * 2.0 * PI / 3.0 + tr.spin * (2.4 * (r / h * 4.0 + 1.0).ln() + zc * 0.8) + (noise(j, 72) - 0.5) * 0.25;
        let a = k * 0.24 * smooth01(u / 0.15) * (1.0 - smooth01((u - 0.75) / 0.25)) * (0.8 + 0.3 * beat.env);
        if a <= 0.003 {
            continue;
        }
        let rot = phi + tr.spin * (PI * 0.5 - 0.35);
        out.push(Inst::bar(w * 0.5 + r * phi.cos(), h * 0.5 + r * phi.sin(), 20.0 + 140.0 * u, 1.5 + 2.5 * u, rot, with_a(col, a), 1.8));
    }
}

/// 舞台追光：壓暗，左右上方兩盞燈慢慢擺，地上一團光斑，光束裡的微塵亮起來；
/// 關鍵字唱到時正上方一道強調色的光淡淡地打下來（不跟著每拍閃）
#[allow(clippy::too_many_arguments)]
fn stage(out: &mut Vec<Inst>, tr: &Track, segs: &[Segment], si: usize, lines: &HashMap<usize, PvLine>, k: f32, ms: i64, accent: [f32; 4]) {
    let (w, h) = (tr.w, tr.h);
    out.push(Inst::rect(w * 0.5, h * 0.5, w + 40.0, h + 40.0, 0.0, [0.0, 0.0, 0.0, 0.22 * k]).fixed());
    let t = ms as f32 / 1000.0;
    let warm = mix([1.0, 0.97, 0.9, 1.0], accent, 0.25);
    let lamps = [
        (w * 0.16, PI * 0.5 - 0.38 + 0.16 * (t * 0.55).sin()),
        (w * 0.84, PI * 0.5 + 0.38 - 0.16 * (t * 0.47 + 1.3).sin()),
    ];
    let oy = -h * 0.08;
    let floor = h * 0.86;
    let lb = h * 1.35;
    for &(ox, ang) in &lamps {
        // 三條往外張開一點的光疊成一個錐形：靠燈的那頭疊在一起、遠的那頭散開
        for f in [-1.0f32, 0.0, 1.0] {
            let a = ang + f * 0.07;
            let thick = w * if f == 0.0 { 0.09 } else { 0.07 };
            let alpha = if f == 0.0 { 0.07 } else { 0.04 };
            out.push(Inst::bar(ox + a.cos() * lb * 0.5, oy + a.sin() * lb * 0.5, lb, thick, a, with_a(warm, alpha * k), 0.0));
        }
        let d = (floor - oy) / ang.sin().max(0.2);
        let r = h * 0.13;
        out.push(Inst::disk(ox + ang.cos() * d, floor, r, with_a(warm, 0.08 * k), (0.0, 0.0)).blur(r * 0.6));
    }
    // 最近一個關鍵字唱到多久了
    let key_t = lines
        .iter()
        .filter(|(i, _)| segment_of(segs, **i) == si)
        .flat_map(|(_, l)| l.lay.glyphs.iter().filter(|p| p.key && p.t <= ms).map(|p| p.t))
        .max();
    if let Some(kt) = key_t {
        let q = (ms - kt) as f32 / 1100.0;
        if q < 1.0 {
            let a = 0.12 * smooth01(q * 6.0) * (1.0 - q).powi(2);
            out.push(Inst::bar(w * 0.5, h * 0.42, h * 1.25, w * 0.14, PI * 0.5, with_a(accent, a * k), 0.0));
        }
    }
    // 微塵：在光束裡的才亮
    for j in 0..30u64 {
        let x = noise(j, 51) * w;
        let y = (noise(j, 52) * h * 1.1 - t * (8.0 + 10.0 * noise(j, 53))).rem_euclid(h * 1.1) - h * 0.05;
        let lit = lamps.iter().any(|&(ox, ang)| {
            let d = ((y - oy).atan2(x - ox) - ang).abs();
            d < 0.12
        });
        let rr = 1.2 + 1.4 * noise(j, 54);
        let a = k * if lit { 0.3 } else { 0.04 };
        out.push(Inst::disk(x, y, rr, with_a(warm, a), (rr * 2.0, 0.5)));
    }
}

/// 殘響：畫面底部一排對稱的細柱，跟著拍子起伏（很淡，拍子之間平滑地變）。字的殘響在 `mod.rs` 畫
fn meter(out: &mut Vec<Inst>, tr: &Track, st: &Style, k: f32, ms: i64, beat: &Beat) {
    let (w, h) = (tr.w, tr.h);
    let step = ms as f32 / 220.0;
    let (s0, u) = (step.floor() as u64, smooth01(step.fract()));
    let base = h * 0.95;
    for j in 0..16u64 {
        let v = mix1(noise(j, s0), noise(j, s0 + 1), u);
        let bh = h * (0.015 + 0.06 * v * (0.5 + 0.7 * beat.env));
        for side in [-1.0f32, 1.0] {
            let x = w * 0.5 + side * (j as f32 + 0.5) * w * 0.022;
            out.push(Inst::rect(x, base - bh * 0.5, w * 0.008, bh, 0.0, with_a(st.line, 0.16 * k * (1.0 - j as f32 / 18.0))));
        }
    }
}

/// 飛越：光塵照遠近有視差（近的跟著鏡頭跑得快、大而糊，遠的慢、小）
fn dust(out: &mut Vec<Inst>, tr: &Track, k: f32, ms: i64, beat: &Beat, accent: [f32; 4]) {
    let (w, h) = (tr.w, tr.h);
    let c = tr.flight_cam(ms);
    let col = mix(accent, [1.0; 4], 0.5);
    let (tw, th) = (w * 1.4, h * 1.4);
    for j in 0..40u64 {
        let f = 0.25 + 0.6 * noise(j, 81);
        let x = (noise(j, 82) * tw - c.x * f).rem_euclid(tw) - (tw - w) * 0.5;
        let y = (noise(j, 83) * th - c.y * f).rem_euclid(th) - (th - h) * 0.5;
        let r = 1.5 + 6.0 * f * f;
        let a = k * (0.06 + 0.2 * f) * (0.8 + 0.4 * beat.env);
        out.push(Inst::disk(x, y, r, with_a(col, a), (r * 2.0, 0.5)).blur(r * 0.8 * f));
    }
}

/// 縱深：一層層往外擴的框（越深轉越多）與從中間往外流的星塵
fn tunnel(out: &mut Vec<Inst>, tr: &Track, st: &Style, k: f32, ms: i64, beat: &Beat, accent: [f32; 4]) {
    let (w, h) = (tr.w, tr.h);
    let zc = tr.depth_z(ms);
    let col = st.line;
    let step = DZ * 0.5;
    let n0 = ((zc + 0.3) / step).ceil() as i64;
    for n in n0..n0 + 10 {
        let dz = n as f32 * step - zc;
        if dz <= 0.3 {
            continue;
        }
        let s = 1.0 / dz;
        let a = k * 0.14 * smooth01((s - 0.18) / 0.3) * (1.0 - smooth01((s - 1.0) / 0.5)) * (1.0 + 0.3 * beat.env);
        if a <= 0.003 {
            continue;
        }
        let mut f = Inst::rect(w * 0.5, h * 0.5, w * 0.94 * s, h * 0.94 * s, tr.spin * (dz - 1.0), with_a(col, a));
        f.fx[2] = 0.0;
        f.line = [col[0], col[1], col[2], (1.6 * s.sqrt()).max(1.0)];
        out.push(f);
    }
    let star = mix(accent, [1.0; 4], 0.6);
    let span = 4.0;
    for j in 0..60u64 {
        let dz = (noise(j, 91) * span - zc).rem_euclid(span) + 0.12;
        let s = 1.0 / dz;
        let x = w * 0.5 + (noise(j, 92) - 0.5) * w * 0.9 * s;
        let y = h * 0.5 + (noise(j, 93) - 0.5) * h * 0.9 * s;
        if x < -20.0 || x > w + 20.0 || y < -20.0 || y > h + 20.0 {
            continue;
        }
        let r = (0.8 + 1.4 * noise(j, 94)) * s.sqrt();
        let a = k * 0.45 * smooth01((s - 0.3) / 0.8) * (0.7 + 0.5 * beat.env);
        out.push(Inst::disk(x, y, r, with_a(star, a), (r * 2.0, 0.5)));
    }
}

/// 跑馬燈：(在畫面高的哪裡、斜多少、字級佔畫面高幾成、實心帶子、往哪捲、每秒捲畫面寬的幾成)
/// 實心那條貼著上緣或下緣，不墊在歌詞正後面（衝擊的關鍵字跟帶子同色，疊上去就看不到）
fn bands(flip: bool) -> [(f32, f32, f32, bool, f32, f32); 2] {
    let (a, b) = if flip { (0.12, 0.86) } else { (0.88, 0.14) };
    [(a, -0.06, 0.11, true, -1.0, 0.045), (b, 0.05, 0.2, false, 1.0, 0.03)]
}

/// 跑馬燈帶：一條實心帶子捲著深色大字、一條只有空心大字，反方向捲。字是這段最近唱到的關鍵字，
/// 換句時新的淡進、舊的淡出；帶子在段落開頭從旁邊滑進來
#[allow(clippy::too_many_arguments)]
fn marquee(
    out: &mut Vec<Inst>,
    atlas: &mut sdf::Atlas,
    gpu: &Gpu,
    text: &mut Text,
    tr: &Track,
    st: &Style,
    segs: &[Segment],
    si: usize,
    lines: &HashMap<usize, PvLine>,
    k: f32,
    ms: i64,
    beat: &Beat,
    accent: [f32; 4],
    px: f32,
) {
    let (w, h) = (tr.w, tr.h);
    // (關鍵字第一個字唱到的時間, 句子)
    let mut sung: Vec<(i64, usize)> = lines
        .iter()
        .filter(|(i, _)| segment_of(segs, **i) == si)
        .filter_map(|(&i, l)| {
            let tk = l.lay.glyphs.iter().filter(|p| p.key).map(|p| p.t).min()?;
            (tk <= ms).then_some((tk, i))
        })
        .collect();
    sung.sort_unstable();
    let n = sung.len();
    let len = (w * w + h * h).sqrt() * 1.3;
    let slide = ease_out(((ms - (tr.t - WIPE_LEAD)) as f32 / 1100.0).clamp(0.0, 1.0));
    for (yf, rot, size_f, solid, dir, speed) in bands(tr.flip) {
        let (sn, cs) = rot.sin_cos();
        // 從它捲過來的那一邊滑進來
        let off = (1.0 - slide) * len * -dir;
        let (cx, cy) = (w * 0.5 + cs * off, h * yf + sn * off);
        let size = h * size_f;
        if solid {
            out.push(Inst::rect(cx, cy, len, size * 1.35 * (1.0 + 0.04 * beat.env), rot, with_a(st.band.unwrap_or(accent), 0.88 * k)));
        }
        for (j, &(tk, i)) in sung.iter().enumerate().skip(n.saturating_sub(2)) {
            let a = if j + 1 == n { smooth01((ms - tk) as f32 / 400.0) } else { 1.0 - smooth01((ms - sung[n - 1].0) as f32 / 400.0) };
            let Some(line) = lines.get(&i) else { continue };
            if a <= 0.003 {
                continue;
            }
            let mut ks: Vec<&Placed> = line.lay.glyphs.iter().filter(|p| p.key).collect();
            ks.sort_by_key(|p| p.i);
            // 關鍵字排成一行（em）：中間隔了空白的（英文）空一點
            let mut run: Vec<(usize, f32)> = Vec::with_capacity(ks.len());
            let mut x = 0.0;
            for (o, p) in ks.iter().enumerate() {
                if o > 0 && p.i > ks[o - 1].i + 1 {
                    x += 0.3;
                }
                let adv = line.g[p.i].adv;
                run.push((p.i, x + adv * 0.5));
                x += adv;
            }
            let period = (x + 1.0) * size;
            let shift = (ms as f32 / 1000.0 * speed * w).rem_euclid(period) * dir;
            let reps = (len * 0.5 / period).ceil() as i32 + 1;
            let (col, lc, lw, fa) = if solid {
                (with_a(st.band_ink, a * k), [0.0; 3], 0.0, 1.0)
            } else {
                (with_a(st.ink, 0.3 * a * k), [st.ink[0], st.ink[1], st.ink[2]], (size * 0.022).max(1.5), 0.0)
            };
            for r in -reps..=reps {
                let base = r as f32 * period + shift - x * size * 0.5;
                for &(gi, xc) in &run {
                    let s = base + xc * size;
                    if s.abs() > len * 0.5 {
                        continue;
                    }
                    let g = &line.g[gi];
                    let Some(gk) = g.key else { continue };
                    let Some(e) = atlas.get(gpu, gk, |kk| text.raster(kk)) else { continue };
                    out.push(glyph_inst(&e, g.adv, cx + cs * s, cy + sn * s, size, rot, 0.0, col, lc, lw, fa, (0.0, 0.0), px));
                }
                // 兩次之間一個圓點
                let s = base + (x + 0.5) * size;
                if s.abs() < len * 0.5 {
                    let dot = Inst::disk(cx + cs * s, cy + sn * s, size * 0.07, col, (0.0, 0.0));
                    out.push(dot);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::Word;

    /// 每句 4 秒、每字 300 ms 的逐字
    fn song(n: usize) -> Lyrics {
        let synced = (0..n)
            .map(|k| {
                let t = 1000 + k as i64 * 4000;
                let words = (0..6).map(|j| Word { t: t + j * 300, d: 300, text: "字".into() }).collect();
                Line { t, text: "一二三四五六".into(), words: Some(words), ..Default::default() }
            })
            .collect();
        Lyrics { synced, ..Default::default() }
    }

    fn seg(scene: Scene) -> Vec<Segment> {
        vec![Segment { first: 0, t: 1000, look: Look::Neon, chorus: true, scene, hue: 0.0 }]
    }

    #[test]
    fn 飛越鏡頭一直在動而且唱的時候句子在中間() {
        let ly = song(5);
        let tr = &tracks(&ly, &seg(Scene::Flight), 3, 1920.0, 1080.0)[0];
        // 唱的時候只在中間附近漂
        let land = cam::first_t(&ly.synced[2]);
        for t in [land, land + 1200, land + 2400] {
            let p = tr.shown(2, t).expect("飛越歸場景管").pose;
            assert!(p.x.abs() < 150.0 && p.y.abs() < 150.0, "{t} {p:?}");
        }
        // 下一句來的時候，這句已經離開畫面中間（不疊在一起）
        let p = tr.shown(2, cam::first_t(&ly.synced[3])).unwrap().pose;
        assert!(p.x.hypot(p.y) > 1000.0, "{p:?}");
        // 每 100 ms 都在動（長鏡頭不停）
        let mut last = tr.flight_cam(1000);
        for t in (1100..20_000).step_by(100) {
            let c = tr.flight_cam(t);
            assert!((c.x - last.x).hypot(c.y - last.y) > 0.5, "{t} ms 停住了");
            last = c;
        }
    }

    #[test]
    fn 縱深新句由小變大舊句飛過去淡掉() {
        let ly = song(4);
        let tr = &tracks(&ly, &seg(Scene::Depth), 3, 1920.0, 1080.0)[0];
        let land = cam::first_t(&ly.synced[2]);
        let new = tr.shown(2, land).unwrap();
        assert!(new.pose.s < 0.95 && new.alpha > 0.5, "{new:?}");
        let later = tr.shown(2, land + 2500).unwrap();
        assert!(later.pose.s > new.pose.s);
        // 兩句以後，這句已經飛過去了
        let gone = tr.shown(2, cam::first_t(&ly.synced[3]) + 2500).unwrap();
        assert!(gone.alpha < 0.05, "{gone:?}");
    }

    #[test]
    fn 堆疊牆舊句收到格子裡最後一句留在原位() {
        let ly = song(4);
        let tr = &tracks(&ly, &seg(Scene::Wall), 3, 1920.0, 1080.0)[0];
        let after = cam::first_t(&ly.synced[3]) + 1000;
        for i in 0..3 {
            let s = tr.shown(i, after).unwrap();
            assert!(s.pose.s < 0.4 && s.alpha < 0.5, "{i} {s:?}");
        }
        let last = tr.shown(3, after).unwrap();
        assert_eq!(last.pose, Pose::ID);
        // 整段一起退場：最後一句唱完待一陣子（沒有下一段）
        assert_eq!(tr.exit, (13_000 + 1800 + LINGER).min(13_000 + 5000));
    }

    #[test]
    fn 轉盤唱的時候句子在正中間舊句轉到旁邊() {
        let ly = song(5);
        for seed in 0..8 {
            let tr = &tracks(&ly, &seg(Scene::Carousel), seed, 1920.0, 1080.0)[0];
            let land = cam::first_t(&ly.synced[2]);
            for t in [land, land + 1000, land + 2000] {
                let s = tr.shown(2, t).unwrap();
                assert!(s.pose.x.abs() < 80.0 && s.pose.y.abs() < 20.0 && s.pose.a.abs() < 0.06 && s.alpha > 0.9, "seed {seed} {t} {s:?}");
            }
            // 上一句轉到旁邊、斜的、淡一點
            let old = tr.shown(1, land + 1000).unwrap();
            assert!(old.pose.x.abs() > 500.0 && old.pose.a.abs() > 0.4 && old.alpha < 0.8, "seed {seed} {old:?}");
        }
    }

    #[test]
    fn 分鏡舊句收進格子第五句來時讓位() {
        let ly = song(6);
        let tr = &tracks(&ly, &seg(Scene::Split), 3, 1920.0, 1080.0)[0];
        let at = |i: usize| cam::first_t(&ly.synced[i]);
        let s = tr.shown(0, at(0) + 500).unwrap();
        assert!(s.pose.s > 0.8, "{s:?}");
        let s = tr.shown(0, at(2)).unwrap();
        assert!(s.pose.s < 0.45 && s.alpha > 0.4, "{s:?}");
        // 第 4 句（k = 4）收進第 0 句那格的時候，第 0 句淡掉
        assert!(tr.shown(0, at(5) + 400).unwrap().alpha < 0.01);
        assert!(tr.shown(1, at(5) + 400).unwrap().alpha > 0.4);
        keep_in_view(tr, &ly);
    }

    #[test]
    fn 鐘擺甩進來停在中間再往來的方向甩出去() {
        let ly = song(4);
        let tr = &tracks(&ly, &seg(Scene::Pendulum), 3, 1920.0, 1080.0)[0];
        let at = |i: usize| cam::first_t(&ly.synced[i]);
        let a = tr.shown(1, at(1) - 100).unwrap().pose;
        let b = tr.shown(1, at(1) + 1500).unwrap().pose;
        let c = tr.shown(1, at(2) + 380).unwrap();
        assert!(a.x.abs() > 300.0, "{a:?}");
        assert!(b.x.abs() < 40.0 && b.a.abs() < 0.05, "{b:?}");
        assert!(a.x.signum() == c.pose.x.signum() && c.alpha < 0.8, "{a:?} {c:?}");
        // 新句從另一邊進來
        let n = tr.shown(2, at(2) - 100).unwrap().pose;
        assert!(n.x.signum() != a.x.signum(), "{n:?}");
    }

    #[test]
    fn 階梯舊句一階一階往上退() {
        let ly = song(7);
        let tr = &tracks(&ly, &seg(Scene::Cascade), 3, 1920.0, 1080.0)[0];
        let t = cam::first_t(&ly.synced[6]) + 800;
        let ys: Vec<f32> = (4..7).map(|i| tr.shown(i, t).unwrap().pose.y).collect();
        assert!(ys.windows(2).all(|w| w[0] < w[1]), "{ys:?}");
        assert!(tr.shown(0, t).unwrap().alpha < 0.01);
        assert!(tr.shown(6, t).unwrap().pose.s > 0.99);
        keep_in_view(tr, &ly);
    }

    #[test]
    fn 螺旋對焦時在正中間() {
        let ly = song(5);
        let tr = &tracks(&ly, &seg(Scene::Vortex), 3, 1920.0, 1080.0)[0];
        // 對焦的時刻（唱到一半）在正中間、轉正
        let i = 2;
        let s = tr.shown(i, mid(&ly, i) as i64).unwrap();
        assert!(s.pose.x.hypot(s.pose.y) < 30.0 && (s.pose.s - 1.0).abs() < 0.08 && s.pose.a.abs() < 0.05, "{s:?}");
        let gone = tr.shown(i, cam::first_t(&ly.synced[i + 1]) + 2500).unwrap();
        assert!(gone.alpha < 0.05, "{gone:?}");
    }

    /// 看得見的句子，中心都還在畫面裡（場景不把正在顯示的句子擺出去）
    fn keep_in_view(tr: &Track, ly: &Lyrics) {
        for t in (1000..ly.synced.last().unwrap().t + 4000).step_by(100) {
            for i in 0..ly.synced.len() {
                if let Some(s) = tr.shown(i, t)
                    && s.alpha > 0.3
                {
                    assert!(s.pose.x.abs() < 960.0 && s.pose.y.abs() < 540.0, "{i} {t} {s:?}");
                }
            }
        }
    }
}
