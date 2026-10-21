//! 一句歌詞的構圖：每個字擺哪、多大、轉幾度、怎麼進場，配什麼裝飾。
//!
//! 文字 PV 的版面不是排版，是構圖：一句拆成幾個詞組，詞組之間大小、方向都不一樣，
//! 拖得長的（唱得久的）那段當關鍵字放大。這裡只決定「停好之後的樣子」與進退場的種類，
//! 動起來的部分在 `mod.rs` 照時間算。
//!
//! 亂數的種子是曲目與句子索引：同一首歌每次播，同一句的構圖都一樣。

use cosmic_text::CacheKey;

use super::style::Style;

/// 構圖要的每個字。`adv` 是以字級 1 為單位的寬度（em）
#[derive(Clone, Copy, Debug)]
pub struct GIn {
    pub ch: char,
    pub adv: f32,
    /// 空白是 `None`
    pub key: Option<CacheKey>,
    /// 這個字開始唱的時間與長度（ms，歌詞時間軸）
    pub t: i64,
    pub d: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Enter {
    /// 從 0 放大，衝過頭再彈回來
    Pop,
    /// 從上面掉下來，落地彈一下
    Drop,
    /// 轉著放大
    Spin,
    /// 從側邊滑進來，帶殘影與傾斜（-1 從左、1 從右）
    Slide(f32),
    /// 從很大蓋下來，蓋到的那一下畫面震
    Stamp,
    /// 從下面慢慢浮上來、淡入
    Rise,
    /// 一閃一閃、左右跳著出現（故障）
    Flicker,
    /// 一下就打上去，只輕輕壓一下（打字）
    Type,
    /// 像燈管通電：閃兩下再亮穩，不跳位置（燈管）
    Ignite,
    /// 原地從失焦慢慢對焦、淡入（電影）
    Focus,
    /// 彈簧：彈出來衝過頭，來回晃兩三下才停
    Spring,
    /// 預備動作：先往外退一點再甩進來（-1 從左、1 從右）
    Whip(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Exit {
    /// 往外飛散、邊飛邊轉
    Scatter,
    /// 往下掉
    Fall,
    /// 衝向鏡頭放大消失
    Zoom,
    /// 往上飄著淡掉
    Fade,
    /// 一個一個閃掉，橫向錯位（故障）
    Blink,
    /// 倒退鍵：從最後一個字往回一個一個刪掉（打字）
    Erase,
    /// 斷電：暗下去途中閃一下，不移動（燈管）
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    /// 詞組一行一個，大小不一、左右錯開，整塊微斜
    Stack,
    /// 直書：詞組一欄一個，由右往左
    Vertical,
    /// 整句沿一條斜線
    Diagonal,
    /// 詞組沿 Z 字散開
    Zigzag,
    /// 短句：一行超大字置中，一個一個蓋下來
    Slam,
    /// 字幕：一樣大的字、寬字距，一兩行置中
    Caption,
    /// 打字稿：一樣大的字靠左，一行寫不下就換行
    Typed,
}

/// 擺好的一個字（螢幕座標，邏輯 px）
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    /// 在 `GIn` 裡的索引
    pub i: usize,
    /// 字身框的中心
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub rot: f32,
    /// 關鍵字（放大、強調色、跟拍子跳）
    pub key: bool,
    /// 只描邊不填滿
    pub hollow: bool,
    /// 墊在色塊上（字改成深色）
    pub on_band: bool,
    pub enter: Enter,
    /// 每個字自己的亂數（-1..1），進場角度、飛散方向用
    pub jitter: f32,
    pub t: i64,
    pub d: i64,
}

#[derive(Clone, Copy, Debug)]
pub enum Deco {
    /// 關鍵字後面的色塊，從一端刷出來
    Band { cx: f32, cy: f32, w: f32, h: f32, rot: f32, t: i64 },
    /// 框住整句的四個角
    Corners { cx: f32, cy: f32, w: f32, h: f32, rot: f32, t: i64 },
    /// 墊在最後面的超大淡字（關鍵字的第一個字）
    Ghost { i: usize, cx: f32, cy: f32, size: f32, rot: f32 },
}

/// 一句的構圖配方：構圖裡看得出差別的那幾樣。整首歌由 [`super::vary`] 配，同一首不重複
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Recipe {
    pub template: Template,
    /// 同一個範本的變化（[`sides`]）：疊排齊左／齊右、直書階梯往哪邊、斜線與 Z 字往哪邊、字幕與打字稿擺上中下
    pub side: u8,
    pub key_enter: Enter,
    pub exit: Exit,
    pub band: bool,
    pub corners: bool,
}

/// 範本有幾種變化（0 是原本的樣子）
pub fn sides(t: Template) -> u8 {
    match t {
        Template::Stack | Template::Vertical | Template::Caption | Template::Typed => 3,
        Template::Diagonal | Template::Zigzag => 2,
        Template::Slam => 1,
    }
}

/// 配方要看的一句的樣子：要畫的字數、詞組數、是不是全是中日文
#[derive(Clone, Copy, Debug)]
pub struct Feat {
    pub n: usize,
    pub phrases: usize,
    pub all_cjk: bool,
}

impl Feat {
    pub fn of(g: &[GIn]) -> Self {
        let ph = phrases(g);
        Self {
            n: ph.iter().map(Vec::len).sum(),
            phrases: ph.len(),
            all_cjk: ph.iter().flatten().all(|&i| is_cjk(g[i].ch) || is_keep_punct(g[i].ch)),
        }
    }

    /// 這句擺不擺得進這個範本
    pub fn fits(&self, t: Template) -> bool {
        match t {
            Template::Slam => self.n <= 6,
            Template::Vertical => self.all_cjk && self.n <= 14 && self.phrases <= 4,
            Template::Zigzag => self.phrases >= 2,
            Template::Diagonal => self.n <= 18,
            Template::Stack | Template::Caption | Template::Typed => true,
        }
    }
}

/// 規劃用的字：沒排版，只有字、時間（寬度當 1 em），拿來量 [`Feat`]
pub fn probe(ch: char, t: i64, d: i64) -> GIn {
    let key = CacheKey::new(
        cosmic_text::fontdb::ID::dummy(),
        0,
        1.0,
        (0.0, 0.0),
        cosmic_text::Weight::BOLD,
        cosmic_text::CacheKeyFlags::empty(),
    )
    .0;
    GIn { ch, adv: 1.0, key: (!ch.is_whitespace()).then_some(key), t, d }
}

/// 這個風格下一句可以用的配方，各帶一個偏好（風格清單裡重複列的範本、進場算偏好）
pub fn options(st: &Style, f: &Feat) -> Vec<(Recipe, f32)> {
    fn uniq<T: Copy + PartialEq>(xs: &[T]) -> Vec<(T, usize)> {
        let mut out: Vec<(T, usize)> = Vec::new();
        for &x in xs {
            match out.iter_mut().find(|o| o.0 == x) {
                Some(o) => o.1 += 1,
                None => out.push((x, 1)),
            }
        }
        out
    }
    let mut out = Vec::new();
    let keys = uniq(st.key_enters);
    let exits = uniq(st.exits);
    for (t, tm) in uniq(st.templates).into_iter().filter(|(t, _)| f.fits(*t)) {
        let slam = t == Template::Slam;
        // 置中大字一律用第一種重點進場、不墊色塊也不框
        let ks: Vec<(Enter, usize)> = if slam { vec![(st.key_enters.first().copied().unwrap_or(Enter::Pop), 1)] } else { keys.clone() };
        let bands: &[bool] = if slam || st.band_p <= 0.0 { &[false] } else { &[false, true] };
        let corners: &[bool] = if slam || st.corners_p <= 0.0 { &[false] } else { &[false, true] };
        for side in 0..sides(t) {
            for &(key_enter, km) in &ks {
                for &(exit, _) in &exits {
                    for &band in bands {
                        for &c in corners {
                            let pref = (tm - 1) as f32 + (km - 1) as f32 * 0.5;
                            out.push((Recipe { template: t, side, key_enter, exit, band, corners: c }, pref));
                        }
                    }
                }
            }
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub glyphs: Vec<Placed>,
    pub decos: Vec<Deco>,
    pub template: Template,
    pub exit: Exit,
    /// 整句的中心與旋轉（退場飛散的中心、鏡頭微轉）
    pub cx: f32,
    pub cy: f32,
    pub tilt: f32,
    /// 最後一個字唱完的時間
    pub sung_end: i64,
}

/// splitmix64：夠亂、可重現、不用另外的 crate
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// 0..1
    pub fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
    pub fn chance(&mut self, p: f32) -> bool {
        self.f() < p
    }
    pub fn sign(&mut self) -> f32 {
        if self.chance(0.5) { 1.0 } else { -1.0 }
    }
}

pub fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x2E80..=0x9FFF | 0xAC00..=0xD7FF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

/// 斷詞組而且不畫出來的標點
fn is_drop_punct(c: char) -> bool {
    matches!(c, '，' | '。' | '、' | ',' | '.' | '；' | ';' | '：' | ':' | '「' | '」' | '『' | '』' | '（' | '）' | '(' | ')' | '"' | '“' | '”')
}

/// 照畫、但畫完就斷詞組的標點
fn is_keep_punct(c: char) -> bool {
    matches!(c, '！' | '？' | '!' | '?' | '…' | '～' | '~')
}

/// 中文詞組最長幾個字，超過就從唱的空檔切開
const MAX_CJK: usize = 6;
/// 英文詞組最多幾個字母（合併短字用）
const MAX_LATIN: usize = 13;

/// 一句拆成詞組，每組是 `GIn` 的索引。空白與不畫的標點不在任何一組裡
pub fn phrases(g: &[GIn]) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut cur = Vec::new();
    for (i, x) in g.iter().enumerate() {
        if x.key.is_none() || x.ch.is_whitespace() || is_drop_punct(x.ch) {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        // 中文接英文（或反過來）也切開，大小可以不一樣
        if let Some(&p) = cur.last()
            && is_cjk(g[p].ch) != is_cjk(x.ch)
            && !is_keep_punct(x.ch)
        {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(i);
        if is_keep_punct(x.ch) {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    // 太長的中文、日文詞組從詞的邊界切
    let chars: Vec<char> = g.iter().map(|x| x.ch).collect();
    let lv = super::seg::starts(&chars);
    let mut split = Vec::new();
    for p in out {
        split_long(g, &lv, p, &mut split);
    }
    // 英文：一個字一組太碎，短的併起來
    let mut merged: Vec<Vec<usize>> = Vec::new();
    for p in split {
        let latin = |p: &[usize]| p.iter().all(|&i| !is_cjk(g[i].ch));
        if let Some(last) = merged.last_mut()
            && latin(last)
            && latin(&p)
            && last.len() + p.len() < MAX_LATIN
        {
            last.extend(p);
            continue;
        }
        merged.push(p);
    }
    merged
}

/// `lv` 是 [`super::seg::starts`]：每個字前面切開好不好
fn split_long(g: &[GIn], lv: &[u8], p: Vec<usize>, out: &mut Vec<Vec<usize>>) {
    if p.len() <= MAX_CJK || !p.iter().all(|&i| is_cjk(g[i].ch)) {
        out.push(p);
        return;
    }
    let n = p.len();
    // 在第 k 個字前面切：先看是不是詞的邊界（不把詞切開），再看唱的空檔越長越好、越靠中間越好。
    // 切出單獨一個字的扣分，但比切在詞中間好
    let best = (1..n)
        .max_by_key(|&k| {
            let (a, b) = (&g[p[k - 1]], &g[p[k]]);
            let gap = (b.t - (a.t + a.d)).clamp(0, 2000);
            let lone = if k == 1 || k == n - 1 { 12_000 } else { 0 };
            lv[p[k]] as i64 * 20_000 - lone + gap * 4 - (k as i64 - n as i64 / 2).abs() * 60
        })
        .unwrap_or(n / 2);
    let (a, b) = p.split_at(best);
    split_long(g, lv, a.to_vec(), out);
    split_long(g, lv, b.to_vec(), out);
}

/// 詞組排成一行：回傳每個字的中心 x（從 0 起算，em × size）與整行寬
fn row(g: &[GIn], p: &[usize], size: f32) -> (Vec<f32>, f32) {
    let mut xs = Vec::with_capacity(p.len());
    let mut x = 0.0;
    for (k, &i) in p.iter().enumerate() {
        // 原句裡中間隔了空白（英文併起來的字）
        if k > 0 && i > p[k - 1] + 1 {
            x += 0.3 * size;
        }
        let w = g[i].adv * size;
        xs.push(x + w * 0.5);
        x += w;
    }
    (xs, x)
}

/// 構圖一句。`w`、`h` 是畫面大小（邏輯 px），`seed` 換一句就要換；範本、進退場、裝飾照 `st` 的偏好。
/// 有 `rec`（場景計畫配好的）就照它，這句擺不進它的範本才自己挑
pub fn compose(g: &[GIn], seed: u64, idx: usize, w: f32, h: f32, st: &Style, rec: Option<&Recipe>) -> Layout {
    let mut rng = Rng::new(seed.wrapping_mul(31).wrapping_add(idx as u64));
    let ph = phrases(g);
    let feat = Feat::of(g);
    let rec = rec.filter(|r| feat.fits(r.template));
    let side = rec.map(|r| r.side);
    let sung_end = g.iter().map(|x| x.t + x.d).max().unwrap_or(0);

    // 關鍵詞組：平均每個字唱最久的，最後一組加分（歌詞的重音常在句尾）
    let key_ph = (0..ph.len())
        .max_by(|&a, &b| {
            let score = |k: usize, r: f32| {
                let p = &ph[k];
                let mean = p.iter().map(|&i| g[i].d).sum::<i64>() as f32 / p.len() as f32;
                mean + if k + 1 == ph.len() { 250.0 } else { 0.0 } + r
            };
            let (ra, rb) = (rng.f() * 120.0, rng.f() * 120.0);
            score(a, ra).total_cmp(&score(b, rb))
        })
        .unwrap_or(0);

    // 範本：照風格給的順序輪，每句往後走（步長跟長度互質，連續兩句不會落在同一格），不適合這句的往後找
    let order = st.templates;
    let m = order.len().max(1);
    let step = if m % 2 == 1 { 2 } else { 1 };
    let start = (seed % m as u64) as usize + idx * step;
    // 這格不適合就往後找，但先跳過前後句會用的那兩格（不然跟隔壁句撞成一樣）
    let near = |o: usize| o == step % m || o == (m - step % m) % m;
    let offs = std::iter::once(0).chain((1..m).filter(|&o| !near(o))).chain((1..m).filter(|&o| near(o)));
    let template = rec
        .map(|r| r.template)
        .unwrap_or_else(|| offs.map(|o| order[(start + o) % m]).find(|t| feat.fits(*t)).unwrap_or(Template::Stack));
    let pick = |rng: &mut Rng, xs: &[Enter]| xs.get((rng.f() * xs.len() as f32) as usize).copied().unwrap_or(Enter::Pop);
    // 字幕、打字稿與「一樣大」的風格不分大小字
    let even = st.even || matches!(template, Template::Slam | Template::Caption | Template::Typed);

    // 每個詞組的字級（em）、空心、進場方式
    let mut sizes = vec![1.0f32; ph.len()];
    let mut hollow = vec![false; ph.len()];
    let mut enters = vec![Enter::Pop; ph.len()];
    for k in 0..ph.len() {
        if k == key_ph {
            // 亂數的抽法跟以前一樣（置中大字不抽），舊風格的構圖才不會變
            sizes[k] = if template == Template::Slam {
                1.0
            } else {
                let s = rng.range(1.45, 1.75);
                if even { 1.0 } else { s }
            };
            // 亂數照抽，後面的才跟沒配方時一樣
            enters[k] = pick(&mut rng, st.key_enters);
            if let Some(r) = rec {
                enters[k] = r.key_enter;
            }
        } else {
            let small = rng.chance(0.3);
            sizes[k] = if small && !even { 0.68 } else { 1.0 };
            hollow[k] = ph.len() > 1 && rng.chance(st.hollow);
            enters[k] = pick(&mut rng, st.enters);
        }
        match template {
            Template::Slam => enters[k] = st.key_enters[0],
            Template::Vertical if k != key_ph && st.enters.contains(&Enter::Drop) => enters[k] = Enter::Drop,
            _ => {}
        }
    }
    let band = template != Template::Slam && rng.chance(st.band_p);
    let band = rec.map_or(band, |r| r.band && template != Template::Slam);

    // ---- 在 em 空間擺好（字級 1 = 1 單位） ----
    // (詞組, 字索引, x, y, 字級, 旋轉)
    let mut pts: Vec<(usize, usize, f32, f32, f32, f32)> = Vec::new();
    let mut tilt = 0.0f32;
    match template {
        Template::Stack => {
            // 詞組太多就兩兩併成一行
            let rows: Vec<Vec<usize>> = if ph.len() > 4 {
                (0..ph.len()).collect::<Vec<_>>().chunks(2).map(<[usize]>::to_vec).collect()
            } else {
                (0..ph.len()).map(|k| vec![k]).collect()
            };
            let widths: Vec<f32> = rows
                .iter()
                .map(|r| r.iter().map(|&k| row(g, &ph[k], sizes[k]).1).sum::<f32>() + 0.35 * (r.len() as f32 - 1.0))
                .collect();
            let maxw = widths.iter().copied().fold(0.0, f32::max);
            let mut y = 0.0;
            for (ri, r) in rows.iter().enumerate() {
                let rh = r.iter().map(|&k| sizes[k]).fold(0.0, f32::max);
                y += rh * 0.56;
                let slack = maxw - widths[ri];
                let lean = match (rng.f() * 3.0) as u32 {
                    0 => 0.0,
                    1 => slack,
                    _ => slack * 0.5,
                };
                // 變化 1、2：整塊齊左、齊右（0 是每行各自錯開）
                let lean = match side {
                    Some(1) => 0.0,
                    Some(2) => slack,
                    _ => lean,
                };
                let mut x = lean + rng.range(-0.4, 0.4);
                for &k in r {
                    let (xs, wk) = row(g, &ph[k], sizes[k]);
                    // 小字的行靠上一點，跟大字錯開
                    let dy = (rh - sizes[k]) * 0.5 * if ri % 2 == 0 { 1.0 } else { -1.0 };
                    for (j, &i) in ph[k].iter().enumerate() {
                        pts.push((k, i, x + xs[j], y + dy, sizes[k], 0.0));
                    }
                    x += wk + 0.35;
                }
                y += rh * 0.56 + 0.06;
            }
            tilt = rng.range(-0.1, 0.1);
        }
        Template::Vertical => {
            let mut x = 0.0;
            let cols = ph.len().max(2) as f32 - 1.0;
            for (k, p) in ph.iter().enumerate() {
                let s = sizes[k];
                let top = rng.range(0.0, 1.4);
                // 變化 1、2：由右往左一欄比一欄低、一欄比一欄高（0 是各欄亂錯開）
                let top = match side {
                    Some(1) => k as f32 / cols * 1.6 + top * 0.15,
                    Some(2) => (1.0 - k as f32 / cols) * 1.6 + top * 0.15,
                    _ => top,
                };
                x -= s * 0.6;
                for (j, &i) in p.iter().enumerate() {
                    pts.push((k, i, x, top + (j as f32 + 0.5) * s * 1.02, s, 0.0));
                }
                x -= s * 0.6 + 0.25;
            }
            tilt = rng.range(-0.05, 0.05);
        }
        Template::Diagonal => {
            let mut x = 0.0;
            let mut line_k = Vec::new();
            for (k, p) in ph.iter().enumerate() {
                let (xs, wk) = row(g, p, sizes[k]);
                for (j, &i) in p.iter().enumerate() {
                    line_k.push((k, i, x + xs[j], sizes[k]));
                }
                x += wk + 0.4;
            }
            // 太長折成兩條平行的斜線
            let total = x - 0.4;
            let two = total > 11.0;
            let half = total * 0.5;
            for (k, i, gx, s) in line_k {
                let (gx, gy) = if two && gx > half { (gx - half + 1.2, 1.9) } else { (gx, 0.0) };
                // 大字跟小字底部對齊（同一條基線）
                pts.push((k, i, gx, gy - (s - 1.0) * 0.45, s, 0.0));
            }
            let sign = rng.sign();
            let sign = match side {
                Some(0) => 1.0,
                Some(_) => -1.0,
                None => sign,
            };
            tilt = sign * rng.range(0.16, 0.3);
        }
        Template::Zigzag => {
            let dir = rng.sign();
            let dir = match side {
                Some(0) => 1.0,
                Some(_) => -1.0,
                None => dir,
            };
            let mut cy = 0.0;
            for (k, p) in ph.iter().enumerate() {
                let (xs, wk) = row(g, p, sizes[k]);
                let side = if k % 2 == 0 { -1.0 } else { 1.0 } * dir;
                let x0 = side * rng.range(0.6, 1.8) - wk * 0.5;
                let r = side * rng.range(0.06, 0.16);
                for (j, &i) in p.iter().enumerate() {
                    pts.push((k, i, x0 + xs[j], cy, sizes[k], r));
                }
                cy += sizes[k] * 0.62 + ph.get(k + 1).map_or(0.0, |_| sizes[k + 1] * 0.62) + 0.1;
            }
        }
        Template::Slam => {
            let mut x = 0.0;
            for (k, p) in ph.iter().enumerate() {
                let (xs, wk) = row(g, p, 1.0);
                for (j, &i) in p.iter().enumerate() {
                    pts.push((k, i, x + xs[j], 0.0, 1.0, 0.0));
                }
                x += wk + 0.3;
            }
        }
        Template::Caption | Template::Typed => {
            let caption = template == Template::Caption;
            // 字距：字幕拉寬一點比較有電影感
            let track = if caption { 0.16 } else { 0.06 };
            let (gap, max_w, lead) = if caption { (0.6, 13.0, 1.55) } else { (0.5, 9.0, 1.4) };
            let runs: Vec<(Vec<f32>, f32)> = ph
                .iter()
                .map(|p| {
                    let (xs, wk) = row(g, p, 1.0);
                    let xs = xs.iter().enumerate().map(|(j, x)| x + j as f32 * track).collect();
                    (xs, wk + (p.len() as f32 - 1.0) * track)
                })
                .collect();
            // 一行放不下就換行（詞組不拆開）
            let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
            let mut used = 0.0;
            for (k, run) in runs.iter().enumerate() {
                let cur = rows.last_mut().expect("至少一行");
                if !cur.is_empty() && used + gap + run.1 > max_w {
                    rows.push(Vec::new());
                    used = 0.0;
                }
                let cur = rows.last_mut().expect("至少一行");
                if !cur.is_empty() {
                    used += gap;
                }
                used += run.1;
                cur.push(k);
            }
            for (ri, r) in rows.iter().enumerate() {
                let width = r.iter().map(|&k| runs[k].1).sum::<f32>() + gap * (r.len() as f32 - 1.0);
                let mut x = if caption { -width * 0.5 } else { 0.0 };
                for &k in r {
                    for (j, &i) in ph[k].iter().enumerate() {
                        pts.push((k, i, x + runs[k].0[j], ri as f32 * lead, 1.0, 0.0));
                    }
                    x += runs[k].1 + gap;
                }
            }
        }
    }
    // 每個字自己的小歪斜（直書、置中大字、字幕、打字稿不歪）
    let wobble = !matches!(template, Template::Vertical | Template::Slam | Template::Caption | Template::Typed);
    for p in pts.iter_mut() {
        if wobble {
            p.5 += rng.range(-0.05, 0.05);
        }
    }

    // ---- 整塊旋轉 tilt，量外框，縮放到螢幕 ----
    let (sn, cs) = tilt.sin_cos();
    let rot = |x: f32, y: f32| (x * cs - y * sn, x * sn + y * cs);
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(_, _, x, y, s, _) in &pts {
        let (rx, ry) = rot(x, y);
        let r = s * 0.62;
        (x0, y0, x1, y1) = (x0.min(rx - r), y0.min(ry - r), x1.max(rx + r), y1.max(ry + r));
    }
    if pts.is_empty() {
        (x0, y0, x1, y1) = (0.0, 0.0, 1.0, 1.0);
    }
    let (bw, bh) = ((x1 - x0).max(0.5), (y1 - y0).max(0.5));
    let max_em = pts.iter().map(|p| p.4).fold(1.0, f32::max);
    let cap = if template == Template::Slam { st.cap + 0.1 } else { st.cap };
    let k = (w * 0.8 * st.fill / bw).min(h * 0.64 * st.fill / bh).min(h * cap / max_em);
    let (bcx, bcy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    let cx = w * 0.5 + rng.range(-0.05, 0.05) * w;
    let cy = h * 0.5 + rng.range(-0.04, 0.04) * h;
    // 字幕正中間；打字稿從左邊一成的地方開始寫。變化 1、2 擺到上面、下面三分之一（不超出畫面）
    let (cx, cy) = match template {
        Template::Caption => (w * 0.5, h * 0.5),
        Template::Typed => (w * 0.1 + bw * k * 0.5, cy),
        _ => (cx, cy),
    };
    let cy = match (template, side) {
        (Template::Caption | Template::Typed, Some(s @ (1 | 2))) => {
            let want = if s == 1 { h * 0.32 } else { h * 0.68 };
            let half = bh * k * 0.5 + h * 0.06;
            want.clamp(half.min(h * 0.5), (h - half).max(h * 0.5))
        }
        _ => cy,
    };
    let to_screen = |x: f32, y: f32| {
        let (rx, ry) = rot(x, y);
        (cx + (rx - bcx) * k, cy + (ry - bcy) * k)
    };

    let mut glyphs = Vec::with_capacity(pts.len());
    for &(ph_k, i, x, y, s, r) in &pts {
        let (sx, sy) = to_screen(x, y);
        let held = g[i].d > 700;
        glyphs.push(Placed {
            i,
            x: sx,
            y: sy,
            size: s * k,
            rot: r + tilt,
            key: ph_k == key_ph || held,
            hollow: hollow[ph_k],
            on_band: band && ph_k == key_ph,
            enter: enters[ph_k],
            jitter: rng.range(-1.0, 1.0),
            t: g[i].t,
            d: g[i].d,
        });
    }

    // ---- 裝飾 ----
    let mut decos = Vec::new();
    let t0 = g.iter().filter(|x| x.key.is_some()).map(|x| x.t).min().unwrap_or(0);
    // 墊底的超大淡字
    if let Some(&first) = ph.get(key_ph).and_then(|p| p.first())
        && st.ghost
        && is_cjk(g[first].ch)
    {
        decos.push(Deco::Ghost {
            i: first,
            cx: w * rng.range(0.25, 0.75),
            cy: h * rng.range(0.4, 0.6),
            size: h * rng.range(0.8, 1.1),
            rot: rng.range(-0.25, 0.25),
        });
    }
    // 關鍵字的色塊：在 em 空間量那一組的範圍再轉到螢幕
    if band {
        let kp: Vec<_> = pts.iter().filter(|p| p.0 == key_ph).collect();
        if !kp.is_empty() {
            let s = sizes[key_ph];
            let (mut a0, mut b0, mut a1, mut b1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for p in &kp {
                (a0, b0, a1, b1) = (a0.min(p.2), b0.min(p.3), a1.max(p.2), b1.max(p.3));
            }
            let pad = 0.32 * s;
            let (bx, by) = to_screen((a0 + a1) * 0.5, (b0 + b1) * 0.5);
            let t = kp.iter().map(|p| g[p.1].t).min().unwrap_or(t0);
            decos.push(Deco::Band {
                cx: bx,
                cy: by,
                w: (a1 - a0 + s + 2.0 * pad) * k,
                h: (b1 - b0 + s * 1.05 + pad * 0.6) * k,
                rot: tilt + kp[0].5,
                t: t - 120,
            });
        }
    }
    // 這裡本來有貼著整塊上下緣、橫過畫面的兩條線，看起來多餘拿掉了。
    // 亂數照舊抽掉，後面的框角與退場才跟以前一樣
    for _ in 0..(rng.f() * 3.0) as u32 {
        rng.chance(0.5);
    }
    let corners = rng.chance(st.corners_p);
    if rec.map_or(corners, |r| r.corners) && template != Template::Slam {
        decos.push(Deco::Corners { cx, cy, w: (bw + 0.7) * k, h: (bh + 0.5) * k, rot: tilt, t: t0 - 100 });
    }

    let exit = st.exits.get((rng.f() * st.exits.len() as f32) as usize).copied().unwrap_or(Exit::Fade);
    let exit = rec.map_or(exit, |r| r.exit);
    Layout { glyphs, decos, template, exit, cx, cy, tilt, sung_end }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pv::style::LOOKS;

    fn line(s: &str, t: i64, per: i64) -> Vec<GIn> {
        s.chars()
            .enumerate()
            .map(|(k, ch)| GIn {
                ch,
                adv: if is_cjk(ch) { 1.0 } else { 0.55 },
                key: if ch.is_whitespace() { None } else { Some(fake_key(k)) },
                t: t + k as i64 * per,
                d: per,
            })
            .collect()
    }

    fn fake_key(k: usize) -> CacheKey {
        CacheKey::new(cosmic_text::fontdb::ID::dummy(), k as u16, 80.0, (0.0, 0.0), cosmic_text::Weight::BOLD, cosmic_text::CacheKeyFlags::empty()).0
    }

    #[test]
    fn 詞組照空白與標點切() {
        let g = line("你在左邊 我在右邊", 0, 200);
        let p = phrases(&g);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0], vec![0, 1, 2, 3]);
        let g = line("不要走，好嗎？", 0, 200);
        let p = phrases(&g);
        assert_eq!(p, vec![vec![0, 1, 2], vec![4, 5, 6]]);
    }

    #[test]
    fn 長句從空檔切開() {
        let mut g = line("為什麼月亮走得那麼慢", 0, 200);
        // 「月」前面停很久
        for x in g.iter_mut().skip(3) {
            x.t += 900;
        }
        let p = phrases(&g);
        assert_eq!(p[0], vec![0, 1, 2]);
        assert!(p.iter().all(|p| p.len() <= MAX_CJK));
    }

    #[test]
    fn 長句不把詞切開() {
        // 「最」和「後」之間停很久，以前會從這裡切
        let mut g = line("我想要和你一起走到最後", 0, 200);
        for x in g.iter_mut().skip(10) {
            x.t += 900;
        }
        let p = phrases(&g);
        assert!(p.iter().any(|p| p.contains(&9) && p.contains(&10)), "{p:?}");
        // 日文：文節不拆（聞こえた、気が）
        let g = line("君の声が聞こえた気がしたんだ", 0, 200);
        let p = phrases(&g);
        let s: Vec<String> = p.iter().map(|p| p.iter().map(|&i| g[i].ch).collect()).collect();
        assert!(s.iter().all(|w| w.len() <= MAX_CJK * 3), "{s:?}");
        let joined = s.join("|");
        for w in ["聞こえた", "気が", "したんだ"] {
            assert!(s.iter().any(|x| x.contains(w)), "{w} 被切開了：{joined}");
        }
    }

    #[test]
    fn 英文短字併起來() {
        let g = line("That's just how the night goes", 0, 150);
        let p = phrases(&g);
        assert!(p.len() <= 3, "{p:?}");
    }

    #[test]
    fn 構圖在畫面內而且可重現() {
        let g = line("那是今晚最後一班列車", 1000, 250);
        for idx in 0..12 {
            let st = Style::of(LOOKS[idx % LOOKS.len()]);
            let a = compose(&g, 42, idx, 1280.0, 720.0, &st, None);
            let b = compose(&g, 42, idx, 1280.0, 720.0, &st, None);
            assert_eq!(a.template, b.template);
            for (p, q) in a.glyphs.iter().zip(&b.glyphs) {
                assert_eq!((p.x, p.y, p.size), (q.x, q.y, q.size));
            }
            for p in &a.glyphs {
                assert!(p.x > 0.0 && p.x < 1280.0 && p.y > 0.0 && p.y < 720.0, "{:?} 跑出畫面 {p:?}", a.template);
            }
            // 每個要畫的字都有擺
            assert_eq!(a.glyphs.len(), 10);
        }
    }

    #[test]
    fn 連續兩句範本不一樣() {
        let g = line("你在左邊 我在右邊", 0, 200);
        for look in LOOKS {
            let st = Style::of(look);
            let ts: Vec<_> = (0..10).map(|i| compose(&g, 7, i, 1280.0, 720.0, &st, None).template).collect();
            for w in ts.windows(2) {
                assert_ne!(w[0], w[1], "{look:?} {ts:?}");
            }
        }
    }
}
