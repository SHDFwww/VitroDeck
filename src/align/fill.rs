//! 逐句歌詞補成逐字：第一次播只有逐句的歌時錄下 Spotify 的聲音，播完在背景用 whisper.cpp 聽寫，
//! 把聽到的字（帶 DTW 字級時間）對回歌詞，每個字的開始時間存起來；下次播這首，歌詞就有逐字。
//!
//! 逐字時間是疊在歌詞上的一層（`wordfill.json`，不寫進歌詞快取）：歌詞重抓、換來源都不會被蓋掉，
//! 句子時間對不上存檔時（`lines_fp`）就不套。對得上的歌詞 `Lyrics::filled` 為 true，
//! 自動對齊量偏移時要當成逐句看（`align/mod.rs` 的 `fingerprint`、`expected_mask`）。
//!
//! 2026-11-08 用 whisper.cpp b5454 small 離線對 16 首（6263 個字）驗過（筆記.md「逐句補逐字的離線實驗」）：
//! 酷狗逐字當答案、只給句首時間，字時間誤差中位 96 ms、≤200 ms 的字佔 75%；平均分配到下一句是 279 ms、41%。
//! - DTW 的時間整首偏晚 0.5～0.9 秒（每首不同），用「句首字有對到的句子」的中位數扣掉，只用逐句資訊。
//! - 句中的字再多晚約 75 ms，所以再扣 50 ms。
//! - 給 whisper 歌詞當提示沒幫助（略差），所以不給。

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::bail;
use serde::{Deserialize, Serialize};

use super::verify::{self, Tok};
use crate::config;
use crate::lyrics::{Line, Lyrics, Word};

const RATE: usize = 16_000;
/// 涵蓋度的格子：100 ms
const CELL_SAMPLES: usize = RATE / 10;
/// 一次聽的長度上限（whisper 一個視窗 30 秒）
const MAX_CHUNK_S: f64 = 28.0;
/// 一段聲音有這麼多比例錄到才聽
const MIN_COVER: f64 = 0.9;
const MIN_CHUNK_MS: f64 = 1500.0;
/// 最後一個對到的字之後，每個字給多久
const PACE_MS: f64 = 250.0;
/// 句中的字比句首的字再扣一點（離線量到的系統性偏晚）
const LATER_MS: f64 = 50.0;
/// 至少要這麼多句的句首字對到，才估得出整首的偏晚量
const MIN_BIAS_LINES: usize = 3;
/// 歌詞的字至少要對到這麼多比例，不然聽到的多半不是這首歌（純音樂、靜音、別的聲音）
const MIN_MATCH: f32 = 0.2;
/// 偏晚量超過這個不可信（離線 -1345～+902）
const MAX_BIAS_MS: f64 = 3000.0;
/// 錄到的聲音太少（秒）就不聽
const MIN_REC_S: f64 = 30.0;
/// 同一首最多做幾次（沒補完的會再錄，但不能無止境）
pub const MAX_TRIES: u8 = 3;
/// 這麼多比例的句子補到了就算補完
const ENOUGH: f32 = 0.9;
/// 一格（100 ms）平均功率低於這個（約 -70 dBFS）算沒錄到
const SILENT_POW: f32 = 1e-7;

// ---------------------------- 錄音 ----------------------------

/// 整首的 Spotify 聲音：16 kHz 單聲道，樣本位置就是歌曲時間（第 n 個樣本 = n / 16 ms）
pub struct Rec {
    pcm: Vec<f32>,
    /// 每 100 ms 有沒有錄到
    have: Vec<bool>,
    next: usize,
    started: bool,
    sum: f32,
    count: u8,
}

impl Rec {
    pub fn new(dur_ms: i64) -> Self {
        let n = dur_ms.max(0) as usize * RATE / 1000 + RATE;
        Self { pcm: vec![0.0; n], have: vec![false; n / CELL_SAMPLES + 1], next: 0, started: false, sum: 0.0, count: 0 }
    }

    /// `stereo` 是 48 kHz 交錯立體聲，從歌曲的 `ms` 開始。三點平均降到 16 kHz（跟 `verify::Clip` 一樣）。
    /// 位置跟接著寫的地方差超過 120 ms（拖進度條、暫停）就從新的位置接著寫
    pub fn push(&mut self, ms: f64, stereo: &[f32]) {
        let want = (ms.max(0.0) * RATE as f64 / 1000.0) as usize;
        if !self.started || (want as f64 - self.next as f64).abs() > 0.12 * RATE as f64 {
            self.next = want;
            self.started = true;
            self.sum = 0.0;
            self.count = 0;
        }
        for f in stereo.chunks_exact(2) {
            self.sum += (f[0] + f[1]) / 2.0;
            self.count += 1;
            if self.count == 3 {
                if let Some(x) = self.pcm.get_mut(self.next) {
                    *x = self.sum / 3.0;
                    self.have[self.next / CELL_SAMPLES] = true;
                }
                self.next += 1;
                self.sum = 0.0;
                self.count = 0;
            }
        }
    }

    pub fn len_ms(&self) -> f64 {
        self.pcm.len() as f64 * 1000.0 / RATE as f64
    }

    /// 錄到的總長（毫秒）
    pub fn covered_ms(&self) -> f64 {
        self.have.iter().filter(|h| **h).count() as f64 * 100.0
    }

    /// `[a, b)` 毫秒裡有多少比例錄到
    fn covered(&self, a: f64, b: f64) -> f64 {
        let (i, j) = ((a / 100.0).floor().max(0.0) as usize, (b / 100.0).ceil().max(0.0) as usize);
        let j = j.min(self.have.len());
        if j <= i {
            return 0.0;
        }
        self.have[i..j].iter().filter(|h| **h).count() as f64 / (j - i) as f64
    }

    fn slice(&self, a: f64, b: f64) -> &[f32] {
        let at = |ms: f64| ((ms.max(0.0) * RATE as f64 / 1000.0) as usize).min(self.pcm.len());
        &self.pcm[at(a)..at(b)]
    }
}

// ---------------------------- 字與詞 ----------------------------

/// 一個字一個單位的字元：漢字、假名、諺文，以及其他非 ASCII 的字母（跟 `verify::tokens` 同一套切法）
fn is_solo(c: char) -> bool {
    c.is_alphabetic() && !c.is_ascii()
}

/// 把一句切成「一個一個要上色的詞」，接起來剛好等於原句（空白、標點接在前一個詞後面）：
/// 漢字、假名、諺文一個字一個，英文數字一個詞一個（don't 算一個）。
/// 句首的標點接在第一個詞前面
pub fn pieces(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut letter = false;
    let mut prev: Option<char> = None;
    for c in text.chars() {
        let solo = is_solo(c);
        let starts = solo || (c.is_ascii_alphanumeric() && prev.is_none_or(|p| !(p.is_ascii_alphanumeric() || p == '\'' || p == '’')));
        if starts && letter {
            out.push(std::mem::take(&mut cur));
            letter = false;
        }
        cur.push(c);
        if solo || c.is_ascii_alphanumeric() {
            letter = true;
        }
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 歌詞的句子時間指紋：只看句數與每句的開始時間，不看字（簡繁切換不會變）也不看逐字
pub fn lines_fp(lines: &[Line]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |v: i64| {
        for b in v.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(lines.len() as i64);
    for l in lines {
        eat(l.t);
    }
    h
}

struct LUnit {
    u: String,
    /// 在 `lines` 的第幾句、這句的第幾個詞
    line: usize,
    piece: usize,
    /// 是這個詞的第一個單位
    head: bool,
}

struct WUnit {
    u: String,
    /// 歌曲毫秒
    t: f64,
}

fn lyric_units(lines: &[(i64, String)], idx: &[usize]) -> Vec<LUnit> {
    let mut out = Vec::new();
    for &li in idx {
        for (pi, p) in pieces(&lines[li].1).iter().enumerate() {
            for (k, u) in verify::tokens(p).into_iter().enumerate() {
                out.push(LUnit { u, line: li, piece: pi, head: k == 0 });
            }
        }
    }
    out
}

/// whisper 的詞元換成單位：中日韓一字一個，英文一詞一個（沒有前導空白的英文詞元接在前一個後面）。
/// 一個詞元有好幾個字時，時間在它的範圍裡平均分。半個字（編碼被切斷）的詞元丟掉
fn whisper_units(toks: &[Tok], a: f64) -> Vec<WUnit> {
    let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric());
    let mut out: Vec<WUnit> = Vec::new();
    for tk in toks {
        if tk.text.starts_with("[_") || tk.text.contains('\u{fffd}') {
            continue;
        }
        let t = tk.dtw.unwrap_or(tk.from);
        let us = verify::tokens(&tk.text);
        if us.is_empty() {
            continue;
        }
        let span = (tk.to - tk.from).max(0.0);
        for (j, u) in us.iter().enumerate() {
            if j == 0 && !tk.text.starts_with(' ') && word(u) && out.last().is_some_and(|l| word(&l.u)) {
                out.last_mut().unwrap().u.push_str(u);
                continue;
            }
            out.push(WUnit { u: u.clone(), t: a + t + span * j as f64 / us.len() as f64 });
        }
    }
    out
}

/// 最長共同子序列：歌詞的單位對到聽到的哪一個。回傳 (歌詞索引, 聽到的索引)
fn lcs(a: &[LUnit], b: &[WUnit]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return Vec::new();
    }
    let w = m + 1;
    let mut dp = vec![0u16; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let hit = if a[i].u == b[j].u { dp[(i + 1) * w + j + 1] + 1 } else { 0 };
            dp[i * w + j] = hit.max(dp[(i + 1) * w + j]).max(dp[i * w + j + 1]);
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < n && j < m {
        if a[i].u == b[j].u && dp[i * w + j] == dp[(i + 1) * w + j + 1] + 1 {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if dp[(i + 1) * w + j] >= dp[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// 一句裡 `n` 個詞的開始時間。第 0 個固定在句子時間；`anchors` 是對到的詞（索引 > 0）的時間。
/// 錨點之間線性內插；最後一個錨點之後每個詞給 `pace`，但不會超過句尾
fn interp(n: usize, line_t: f64, end_t: f64, anchors: &BTreeMap<usize, f64>, pace: f64) -> Vec<f64> {
    let mut clean: Vec<(usize, f64)> = vec![(0, line_t)];
    for (&k, &t) in anchors {
        // 單調：比前一個早（或差不到 40 ms）的、落在句尾之後的不要
        if k > 0 && k < n && t > clean.last().unwrap().1 + 40.0 && t < end_t {
            clean.push((k, t));
        }
    }
    let mut est = vec![0.0; n];
    for w in clean.windows(2) {
        let ((x, tx), (y, ty)) = (w[0], w[1]);
        for (i, e) in est.iter_mut().enumerate().take(y + 1).skip(x) {
            *e = tx + (ty - tx) * (i - x) as f64 / (y - x) as f64;
        }
    }
    let (last, tl) = *clean.last().unwrap();
    est[last] = tl;
    let rest = n - 1 - last;
    if rest > 0 {
        let p = pace.min((end_t - tl) / (rest + 1) as f64).max(20.0);
        for (i, e) in est.iter_mut().enumerate().skip(last + 1) {
            *e = tl + p * (i - last) as f64;
        }
    }
    est
}

// ---------------------------- 規劃與求解 ----------------------------

/// 一次聽的一段：歌詞的第幾句到第幾句，聲音從 `a` 到 `b`（歌曲毫秒）
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub lines: Vec<usize>,
    pub a: f64,
    pub b: f64,
}

fn ends(lines: &[(i64, String)]) -> Vec<f64> {
    (0..lines.len()).map(|i| lines.get(i + 1).map_or(lines[i].0 + 4000, |l| l.0) as f64).collect()
}

fn nonblank(lines: &[(i64, String)]) -> Vec<usize> {
    (0..lines.len()).filter(|&i| !lines[i].1.trim().is_empty()).collect()
}

/// 照句子切成 ≤28 秒的段，只留錄到 90% 以上的
fn plan(lines: &[(i64, String)], dur_ms: f64, cover: impl Fn(f64, f64) -> f64) -> Vec<Chunk> {
    let end = ends(lines);
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in nonblank(lines) {
        if let Some(cur) = groups.last_mut()
            && (end[i] - lines[cur[0]].0 as f64) / 1000.0 + 1.0 <= MAX_CHUNK_S
        {
            cur.push(i);
            continue;
        }
        groups.push(vec![i]);
    }
    groups
        .into_iter()
        .filter_map(|g| {
            let (first, last) = (g[0], *g.last().unwrap());
            let a = (lines[first].0 as f64 - 500.0).max(0.0);
            let b = (end[last].min(lines[last].0 as f64 + 8000.0) + 500.0).min(dur_ms);
            (b - a >= MIN_CHUNK_MS && cover(a, b) >= MIN_COVER).then_some(Chunk { lines: g, a, b })
        })
        .collect()
}

/// 一段聽寫的結果
pub struct Heard {
    pub a: f64,
    pub toks: Vec<Tok>,
    pub lines: Vec<usize>,
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// 每句每個詞的 (開始, 長度)；沒聽的段是空的
    pub lines: Vec<Vec<[i64; 2]>>,
    /// 整首 DTW 時間偏晚多少（ms）
    pub bias: i64,
    pub units: usize,
    pub matched: usize,
    /// 句首字對到的句數
    pub heads: usize,
    /// 聽成的段數、規劃的段數（差的是靜音或 whisper 失敗的）
    pub chunks: [usize; 2],
}

/// 把聽到的字對回歌詞，算出每個詞的開始時間
pub fn solve(lines: &[(i64, String)], heard: &[Heard]) -> anyhow::Result<Outcome> {
    let end = ends(lines);
    // 每段各自對：重複的副歌不會把後面的聲音配到前面去
    let mut matched: Vec<(Vec<LUnit>, Vec<Option<f64>>)> = Vec::new();
    let (mut units, mut hits) = (0usize, 0usize);
    let mut head_diff: Vec<f64> = Vec::new();
    for h in heard {
        let lu = lyric_units(lines, &h.lines);
        let wu = whisper_units(&h.toks, h.a);
        let mut at: Vec<Option<f64>> = vec![None; lu.len()];
        for (i, j) in lcs(&lu, &wu) {
            at[i] = Some(wu[j].t);
        }
        units += lu.len();
        hits += at.iter().flatten().count();
        for (u, t) in lu.iter().zip(&at) {
            if let (true, 0, Some(t)) = (u.head, u.piece, t) {
                head_diff.push(t - lines[u.line].0 as f64);
            }
        }
        matched.push((lu, at));
    }
    if head_diff.len() < MIN_BIAS_LINES {
        bail!("句首的字只對到 {} 句", head_diff.len());
    }
    if (hits as f32) < units as f32 * MIN_MATCH {
        bail!("字只對到 {hits}/{units}");
    }
    head_diff.sort_by(|a, b| a.total_cmp(b));
    let bias = head_diff[head_diff.len() / 2];
    if bias.abs() > MAX_BIAS_MS {
        bail!("偏晚量 {bias:.0} ms 不合理");
    }
    let mut out = Outcome { lines: vec![Vec::new(); lines.len()], bias: bias.round() as i64, units, matched: hits, heads: head_diff.len(), chunks: [heard.len(), heard.len()] };
    for (h, (lu, at)) in heard.iter().zip(&matched) {
        for &li in &h.lines {
            let ps = pieces(&lines[li].1);
            let (line_t, end_t) = (lines[li].0 as f64, end[li]);
            let mut anchors: BTreeMap<usize, f64> = BTreeMap::new();
            for (u, t) in lu.iter().zip(at) {
                let Some(t) = t else { continue };
                if u.line != li || u.piece == 0 {
                    continue;
                }
                let t = t - bias - LATER_MS;
                if line_t - 1500.0 < t && t < end_t + 1500.0 {
                    anchors.entry(u.piece).or_insert(t);
                }
            }
            let est = interp(ps.len(), line_t, end_t, &anchors, PACE_MS);
            out.lines[li] = (0..ps.len())
                .map(|k| {
                    let d = est.get(k + 1).map_or((end_t - est[k]).clamp(120.0, 600.0), |n| (n - est[k]).max(1.0));
                    [est[k].round() as i64, d.round() as i64]
                })
                .collect();
        }
    }
    Ok(out)
}

// ---------------------------- 跑 ----------------------------

pub struct Job {
    pub id: String,
    /// (句子時間, 字)
    pub lines: Vec<(i64, String)>,
    pub fp: u64,
    pub rec: Rec,
    pub lang: &'static str,
}

impl Job {
    /// 錄到的夠不夠值得聽
    pub fn worth(&self) -> bool {
        self.rec.covered_ms() >= MIN_REC_S * 1000.0
            && !plan(&self.lines, self.rec.len_ms(), |a, b| self.rec.covered(a, b)).is_empty()
    }
}

/// 一段一段聽（whisper 會佔 4 條執行緒），最後求解。中間有段失敗就跳過，全失敗才算失敗
pub fn run(dir: &Path, tmp: &Path, job: &Job) -> anyhow::Result<Outcome> {
    let chunks = plan(&job.lines, job.rec.len_ms(), |a, b| job.rec.covered(a, b));
    if chunks.is_empty() {
        bail!("錄到的不夠");
    }
    let (mut heard, mut err) = (Vec::new(), None);
    for c in &chunks {
        if !super::fill_on() {
            bail!("中途關掉了");
        }
        let pcm = job.rec.slice(c.a, c.b);
        if pcm.iter().map(|x| x * x).sum::<f32>() / pcm.len().max(1) as f32 <= SILENT_POW {
            continue;
        }
        match verify::listen_words(dir, tmp, &verify::wav_bytes(pcm), job.lang) {
            Ok(toks) => heard.push(Heard { a: c.a, toks, lines: c.lines.clone() }),
            Err(e) => err = Some(e),
        }
    }
    if heard.is_empty() {
        return Err(err.unwrap_or_else(|| anyhow::anyhow!("每段都是靜音")));
    }
    let mut out = solve(&job.lines, &heard)?;
    out.chunks = [heard.len(), chunks.len()];
    Ok(out)
}

// ---------------------------- 存檔與套用 ----------------------------

const FILE: &str = "wordfill.json";

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored {
    /// 補的是哪一份歌詞的句子時間（`lines_fp`）
    pub fp: u64,
    /// 做過幾次
    pub tries: u8,
    pub bias: i64,
    /// 每句每個詞的 (開始, 長度)，空的是沒補
    pub lines: Vec<Vec<[i64; 2]>>,
}

fn read_all(dir: &Path) -> BTreeMap<String, Stored> {
    config::read_json(&dir.join(FILE))
}

pub fn load(dir: &Path, key: &str) -> Option<Stored> {
    read_all(dir).remove(key)
}

pub fn save(dir: &Path, key: &str, st: Stored) {
    let mut m = read_all(dir);
    m.insert(key.to_string(), st);
    let _ = config::write_json(&dir.join(FILE), &m);
}

/// 新結果併進舊的：已經補好的句子不動，只補還空著的。`out` 是 `None` 代表這次沒成功，只記一次嘗試
pub fn merge(old: Option<Stored>, fp: u64, n: usize, out: Option<Outcome>) -> Stored {
    let mut st = old.filter(|o| o.fp == fp && o.lines.len() == n).unwrap_or(Stored { fp, tries: 0, bias: 0, lines: vec![Vec::new(); n] });
    st.tries = st.tries.saturating_add(1);
    if let Some(o) = out {
        let before = st.lines.iter().filter(|l| !l.is_empty()).count();
        st.bias = o.bias;
        for (dst, src) in st.lines.iter_mut().zip(o.lines) {
            if dst.is_empty() {
                *dst = src;
            }
        }
        // 補過了還是剩下補不到的句子（歌詞尾巴有亂七八糟的假句子、整段是純音樂），再錄也不會多：到此為止
        if before > 0 && st.lines.iter().filter(|l| !l.is_empty()).count() == before {
            st.tries = MAX_TRIES;
        }
    }
    st
}

/// 這首還要不要再補：補完了、或做太多次就不用
pub fn wanted(st: Option<&Stored>, lines: &[Line]) -> bool {
    let Some(st) = st.filter(|s| s.fp == lines_fp(lines) && s.lines.len() == lines.len()) else { return true };
    if st.tries >= MAX_TRIES {
        return false;
    }
    let all = lines.iter().filter(|l| !l.text.trim().is_empty()).count();
    let done = lines.iter().zip(&st.lines).filter(|(l, w)| !l.text.trim().is_empty() && !w.is_empty()).count();
    (done as f32) < all as f32 * ENOUGH
}

/// 把存的逐字時間套到歌詞上。句子時間對不上（換了一份歌詞）就不套；
/// 詞數對不上的句子（簡繁轉換改了字數）略過。回傳有沒有套到
pub fn apply(l: &mut Lyrics, st: &Stored) -> bool {
    if st.fp != lines_fp(&l.synced) || st.lines.len() != l.synced.len() {
        return false;
    }
    let mut any = false;
    for (ln, w) in l.synced.iter_mut().zip(&st.lines) {
        if w.is_empty() || ln.words.is_some() {
            continue;
        }
        let ps = pieces(&ln.text);
        if ps.len() != w.len() {
            continue;
        }
        ln.words = Some(ps.into_iter().zip(w).map(|(text, [t, d])| Word { t: *t, d: *d, text }).collect());
        any = true;
    }
    l.filled = any;
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 逐字答案：句子時間 → (句子的字, [(第幾個非空白字元, 那個字的開始時間)])
    type Truth = BTreeMap<i64, (String, Vec<(usize, f64)>)>;

    /// 補出來的詞（第 2 個起）的開始時間跟答案差多少。`base` 是平均分到下一句的對照
    fn score(out: &Outcome, lines: &[(i64, String)], truth: &Truth, errs: &mut Vec<f64>, base: &mut Vec<f64>) {
        for (li, ws) in out.lines.iter().enumerate() {
            let Some((text, at)) = truth.get(&lines[li].0) else { continue };
            if ws.is_empty() {
                continue;
            }
            let ps = pieces(text);
            let (mut off, n) = (0, ws.len());
            for (k, p) in ps.iter().enumerate() {
                let first = off;
                off += p.chars().filter(|c| !c.is_whitespace()).count();
                if k == 0 {
                    continue;
                }
                let Some(&(_, tt)) = at.iter().find(|(o, _)| *o == first) else { continue };
                errs.push((ws[k][0] as f64 - tt).abs());
                let end = lines.get(li + 1).map_or(lines[li].0 + 4000, |l| l.0) as f64;
                base.push((lines[li].0 as f64 + (end - lines[li].0 as f64) * k as f64 / n as f64 - tt).abs());
            }
        }
    }

    fn stat(name: &str, v: &mut [f64]) {
        v.sort_by(|a, b| a.total_cmp(b));
        let pct = |p: f64| v[((v.len() as f64 * p) as usize).min(v.len() - 1)];
        let le = |x: f64| v.iter().filter(|e| **e <= x).count() as f64 * 100.0 / v.len() as f64;
        println!("{name}：n={} 中位 {:.0} p90 {:.0} ≤100 {:.1}% ≤200 {:.1}% ≤300 {:.1}%", v.len(), pct(0.5), pct(0.9), le(100.0), le(200.0), le(300.0));
    }

    fn tk(text: &str, from: f64, to: f64, dtw: Option<f64>) -> Tok {
        Tok { text: text.into(), from, to, dtw }
    }

    #[test]
    fn 切詞接起來等於原句() {
        for s in ["", "走過河岸", "we keep walking home tonight", "Don't stop, 不要停 (oh)", "（前奏）", "  Hello  世界!  ", "café au lait", "1 2 3"] {
            assert_eq!(pieces(s).concat(), s, "{s:?}");
        }
        assert_eq!(pieces("走過河岸"), ["走", "過", "河", "岸"]);
        assert_eq!(pieces("I can't stop"), ["I ", "can't ", "stop"]);
        assert_eq!(pieces("好 good, 吧"), ["好 ", "good, ", "吧"]);
        assert_eq!(pieces("（愛"), ["（愛"], "句首的標點接在第一個詞前面");
        assert!(pieces("   ").len() <= 1);
    }

    #[test]
    fn 單位對得上歌詞與聽寫() {
        let lines = vec![(1000, "Don't stop 不要停".to_string())];
        let lu = lyric_units(&lines, &[0]);
        let us: Vec<_> = lu.iter().map(|u| u.u.as_str()).collect();
        assert_eq!(us, ["don", "t", "stop", "不", "要", "停"]);
        // whisper：don + 't 沒有前導空白會接在一起，所以 don't 對不上 don、t —— 已知的小損失
        let toks = [tk(" Don", 0.0, 100.0, None), tk(" stop", 100.0, 300.0, None), tk(" 不要", 300.0, 500.0, None), tk("停", 500.0, 600.0, None)];
        let wu = whisper_units(&toks, 1000.0);
        let us: Vec<_> = wu.iter().map(|u| (u.u.as_str(), u.t)).collect();
        assert_eq!(us, [("don", 1000.0), ("stop", 1100.0), ("不", 1300.0), ("要", 1400.0), ("停", 1500.0)]);
        // 沒有前導空白的英文詞元接在前一個後面
        let wu = whisper_units(&[tk(" hel", 0.0, 10.0, None), tk("lo", 10.0, 20.0, None)], 0.0);
        assert_eq!(wu.len(), 1);
        assert_eq!(wu[0].u, "hello");
        // DTW 時間優先，特殊詞元與半個字略過
        let wu = whisper_units(&[tk("[_BEG_]", 0.0, 0.0, None), tk("愛", 100.0, 200.0, Some(150.0)), tk("\u{fffd}", 0.0, 0.0, None)], 0.0);
        assert_eq!(wu.len(), 1);
        assert_eq!(wu[0].t, 150.0);
    }

    #[test]
    fn 共同子序列() {
        let l = |s: &str| LUnit { u: s.into(), line: 0, piece: 0, head: true };
        let w = |s: &str| WUnit { u: s.into(), t: 0.0 };
        let a: Vec<_> = ["a", "b", "c", "d", "e"].into_iter().map(l).collect();
        let b: Vec<_> = ["x", "a", "c", "y", "d", "e"].into_iter().map(w).collect();
        assert_eq!(lcs(&a, &b), [(0, 1), (2, 2), (3, 4), (4, 5)]);
        assert!(lcs(&a, &[]).is_empty());
    }

    #[test]
    fn 內插() {
        // 沒有錨點：句子時間開始，每個詞 250 ms
        let e = interp(4, 1000.0, 9000.0, &BTreeMap::new(), 250.0);
        assert_eq!(e, [1000.0, 1250.0, 1500.0, 1750.0]);
        // 句子太短放不下 250 ms：均分到句尾
        let e = interp(4, 1000.0, 1400.0, &BTreeMap::new(), 250.0);
        assert!((e[3] - 1300.0).abs() < 1e-9, "{e:?}");
        // 錨點之間線性內插
        let mut a = BTreeMap::new();
        a.insert(2, 1600.0);
        a.insert(4, 2000.0);
        let e = interp(5, 1000.0, 5000.0, &a, 250.0);
        assert_eq!(e, [1000.0, 1300.0, 1600.0, 1800.0, 2000.0]);
        // 倒退、超出句尾的錨點不要
        let mut a = BTreeMap::new();
        a.insert(1, 900.0);
        a.insert(2, 7000.0);
        a.insert(3, 1600.0);
        let e = interp(4, 1000.0, 5000.0, &a, 250.0);
        assert_eq!(e, [1000.0, 1200.0, 1400.0, 1600.0]);
    }

    #[test]
    fn 錄音按歌曲時間擺() {
        let mut r = Rec::new(10_000);
        // 1 秒的 48 kHz 立體聲，值 0.5，從 2.0 秒開始
        r.push(2000.0, &vec![0.5; 96_000]);
        assert!((r.covered(2000.0, 3000.0) - 1.0).abs() < 1e-9);
        assert_eq!(r.covered(0.0, 2000.0), 0.0);
        assert!((r.covered_ms() - 1000.0).abs() <= 100.0);
        let s = r.slice(2000.0, 3000.0);
        assert_eq!(s.len(), 16_000);
        assert!(s.iter().all(|x| (x - 0.5).abs() < 1e-6));
        // 接著下一包：連續就接在後面；跳到別處就從那裡寫
        r.push(3000.0, &vec![0.25; 9600]);
        assert!((r.slice(3000.0, 3100.0)[0] - 0.25).abs() < 1e-6);
        r.push(7000.0, &vec![0.75; 9600]);
        assert!((r.slice(7000.0, 7100.0)[0] - 0.75).abs() < 1e-6);
        assert!(r.covered(5000.0, 6000.0) == 0.0);
    }

    #[test]
    fn 規劃只留錄滿的段() {
        let lines: Vec<(i64, String)> = (0..10).map(|i| (5000 + i * 5000, format!("第{i}句"))).collect();
        let all = plan(&lines, 60_000.0, |_, _| 1.0);
        // 每段 ≤ 28 秒：5 句一段（5 句 × 5 秒 + 1 秒）放不下，4 句一段
        assert!(all.len() >= 2 && all.iter().all(|c| (c.b - c.a) <= 28_000.0 + 1500.0), "{all:?}");
        assert_eq!(all.iter().flat_map(|c| c.lines.clone()).collect::<Vec<_>>(), (0..10).collect::<Vec<_>>());
        let some = plan(&lines, 60_000.0, |a, _| if a < 20_000.0 { 1.0 } else { 0.5 });
        assert!(some.len() < all.len() && !some.is_empty());
    }

    #[test]
    fn 補出來的字時間() {
        // 三句，每句 5 個字，真的唱的比句子時間晚 700 ms（DTW 偏晚），字距 300 ms
        let lines: Vec<(i64, String)> = vec![(1000, "一二三四五".into()), (4000, "六七八九十".into()), (7000, "甲乙丙丁戊".into())];
        let mut toks = Vec::new();
        for (li, (t, text)) in lines.iter().enumerate() {
            for (k, c) in text.chars().enumerate() {
                let at = *t as f64 + 700.0 + 300.0 * k as f64;
                toks.push(tk(&c.to_string(), at, at + 300.0, Some(at)));
                let _ = li;
            }
        }
        let h = Heard { a: 0.0, toks, lines: vec![0, 1, 2] };
        let o = solve(&lines, &[h]).unwrap();
        assert_eq!(o.bias, 700);
        assert_eq!(o.heads, 3);
        // 第 k 個字在句子時間 + 300k - 50（LATER）；第一個字就是句子時間
        let l0 = &o.lines[0];
        assert_eq!(l0.len(), 5);
        assert_eq!(l0[0][0], 1000);
        for (k, w) in l0.iter().enumerate().skip(1) {
            assert!((w[0] - (1000 + 300 * k as i64 - 50)).abs() <= 1, "{k}: {w:?}");
        }
    }

    #[test]
    fn 聽不出東西就不補() {
        let lines: Vec<(i64, String)> = vec![(1000, "一二三四五".into()), (4000, "六七八九十".into())];
        let junk = Heard { a: 0.0, toks: vec![tk(" thank", 0.0, 500.0, None), tk(" you", 500.0, 900.0, None)], lines: vec![0, 1] };
        assert!(solve(&lines, &[junk]).is_err());
        // 只有一句的句首對到
        let few = Heard { a: 0.0, toks: vec![tk("一", 1700.0, 2000.0, Some(1700.0)), tk("二", 2000.0, 2300.0, Some(2000.0))], lines: vec![0, 1] };
        assert!(solve(&lines, &[few]).is_err());
    }

    #[test]
    fn 存檔併進舊的() {
        let first = Outcome { lines: vec![vec![[1, 2]], vec![]], bias: 700, ..Default::default() };
        let st = merge(None, 7, 2, Some(first));
        assert_eq!((st.tries, st.bias), (1, 700));
        assert_eq!(st.lines, [vec![[1, 2]], vec![]]);
        // 第二次只補空著的，已經補好的不動
        let second = Outcome { lines: vec![vec![[9, 9]], vec![[3, 4]]], bias: 650, ..Default::default() };
        let st = merge(Some(st), 7, 2, Some(second));
        assert_eq!((st.tries, st.bias), (2, 650));
        assert_eq!(st.lines, [vec![[1, 2]], vec![[3, 4]]]);
        // 再補一次沒有多補到任何一句：不會再錄了
        let third = Outcome { lines: vec![vec![[5, 5]], vec![[6, 6]]], bias: 600, ..Default::default() };
        assert_eq!(merge(Some(st.clone()), 7, 2, Some(third)).tries, MAX_TRIES);
        // 歌詞換了（指紋不同）：重來
        let st = merge(Some(st), 8, 2, None);
        assert_eq!((st.tries, st.fp), (1, 8));
        assert!(st.lines.iter().all(Vec::is_empty));
    }

    fn lyrics(texts: &[&str]) -> Lyrics {
        Lyrics {
            synced: texts.iter().enumerate().map(|(i, t)| Line { t: 1000 * (i as i64 + 1), text: t.to_string(), ..Default::default() }).collect(),
            source: "synced".into(),
            provider: "LRCLIB".into(),
            ..Default::default()
        }
    }

    #[test]
    fn 套到歌詞上() {
        let mut l = lyrics(&["一二三", "", "Go now"]);
        let st = Stored { fp: lines_fp(&l.synced), tries: 1, bias: 0, lines: vec![vec![[1000, 100], [1100, 100], [1200, 100]], vec![], vec![[3000, 200], [3300, 300]]] };
        assert!(apply(&mut l, &st));
        assert!(l.filled && l.has_words());
        let w = l.synced[2].words.as_ref().unwrap();
        assert_eq!(w.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["Go ", "now"]);
        assert!(l.synced[1].words.is_none());
        // 句子時間對不上就不套
        let mut other = lyrics(&["一二三", "", "Go now", "多一句"]);
        assert!(!apply(&mut other, &st));
        assert!(!other.filled && !other.has_words());
        // 詞數對不上的句子略過
        let mut l = lyrics(&["一二三四", "", "Go now"]);
        let st = Stored { fp: lines_fp(&l.synced), ..st };
        assert!(apply(&mut l, &st));
        assert!(l.synced[0].words.is_none() && l.synced[2].words.is_some());
    }

    #[test]
    fn 補完了就不再錄() {
        let l = lyrics(&["一二三", "", "Go now", "四五六"]);
        assert!(wanted(None, &l.synced));
        let mut st = Stored { fp: lines_fp(&l.synced), tries: 1, bias: 0, lines: vec![vec![[0, 1]], vec![], vec![], vec![]] };
        assert!(wanted(Some(&st), &l.synced), "只補到 1/3");
        st.lines[2] = vec![[0, 1]];
        st.lines[3] = vec![[0, 1]];
        assert!(!wanted(Some(&st), &l.synced), "補完了");
        st.lines[3] = vec![];
        st.tries = MAX_TRIES;
        assert!(!wanted(Some(&st), &l.synced), "做太多次了");
        st.fp += 1;
        assert!(wanted(Some(&st), &l.synced), "歌詞換了");
    }

    /// 拿離線實驗（`AlignLab\word_exp`，Python 版切段與聽寫的輸出）的聽寫結果跑 `solve`，
    /// 跟酷狗逐字比：確認 Rust 的對齊跟 Python（difflib）的結果差不多。
    /// 手動跑：`cargo test --release 離線對酷狗逐字 -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn 離線對酷狗逐字() {
        use serde_json::Value;
        let root = std::path::PathBuf::from(std::env::var("VITRO_FILL_EXP").expect("VITRO_FILL_EXP=離線實驗資料夾"));
        let idx: Vec<Value> = serde_json::from_slice(&std::fs::read(root.join("chunks").join("index.json")).unwrap()).unwrap();
        let mut songs: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
        for c in &idx {
            songs.entry(c["id"].as_str().unwrap().to_string()).or_default().push(c);
        }
        let (mut errs, mut base) = (Vec::<f64>::new(), Vec::<f64>::new());
        let (mut ok_songs, mut hit, mut all) = (0, 0usize, 0usize);
        for (id, chunks) in &songs {
            // 這首所有句子（時間 → 逐字答案）
            let mut truth: Truth = BTreeMap::new();
            for c in chunks {
                for l in c["lines"].as_array().unwrap() {
                    let mut at = Vec::new();
                    let mut off = 0;
                    for w in l["words"].as_array().unwrap() {
                        let t = w["t"].as_f64().unwrap();
                        for ch in w["text"].as_str().unwrap().chars().filter(|c| !c.is_whitespace()) {
                            let _ = ch;
                            at.push((off, t));
                            off += 1;
                        }
                    }
                    truth.insert(l["t"].as_i64().unwrap(), (l["text"].as_str().unwrap().to_string(), at));
                }
            }
            let times: Vec<i64> = truth.keys().copied().collect();
            let lines: Vec<(i64, String)> = truth.iter().map(|(t, (s, _))| (*t, s.clone())).collect();
            let heard: Vec<Heard> = chunks
                .iter()
                .map(|c| {
                    let name = c["name"].as_str().unwrap();
                    let raw = std::fs::read(root.join("out").join("plain").join(format!("{name}.json"))).unwrap();
                    let toks = verify::parse_tokens(&String::from_utf8_lossy(&raw)).unwrap();
                    let ls = c["lines"].as_array().unwrap().iter().map(|l| times.binary_search(&l["t"].as_i64().unwrap()).unwrap()).collect();
                    Heard { a: c["a"].as_f64().unwrap(), toks, lines: ls }
                })
                .collect();
            let out = match solve(&lines, &heard) {
                Ok(o) => o,
                Err(e) => {
                    println!("{id}：補不成：{e}");
                    continue;
                }
            };
            ok_songs += 1;
            hit += out.matched;
            all += out.units;
            score(&out, &lines, &truth, &mut errs, &mut base);
            println!("{id}：偏晚 {} ms，字對到 {}/{}，句首 {} 句", out.bias, out.matched, out.units, out.heads);
        }
        println!("補成 {ok_songs}/{} 首，字對到 {hit}/{all}", songs.len());
        stat("Rust 錨點法", &mut errs);
        stat("平均分到下一句", &mut base);
        // 離線 Python（difflib）：中位 96、≤200 74.8%；平均分配中位 279、41.1%
        let mut sorted = errs.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        assert!(sorted[sorted.len() / 2] < 150.0, "中位數跟離線差太多");
    }

    /// 讀 AlignData 的 48 kHz 16-bit 立體聲 WAV
    fn read_wav(path: &std::path::Path) -> Vec<i16> {
        let b = std::fs::read(path).unwrap();
        let mut at = 12;
        while at + 8 <= b.len() {
            let len = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap()) as usize;
            if &b[at..at + 4] == b"data" {
                let end = (at + 8 + len).min(b.len());
                return b[at + 8..end].chunks_exact(2).map(|x| i16::from_le_bytes([x[0], x[1]])).collect();
            }
            at += 8 + len + (len & 1);
        }
        panic!("沒有 data");
    }

    /// 整條走一遍：AlignData 的整首錄音照 10 ms 一包擺進 `Rec`，歌詞降成只有句子時間，
    /// 真的呼叫 whisper-cli 聽寫並求解；有酷狗逐字的歌拿來跟答案比，只有逐句的歌印出結果。
    /// 手動跑：`VITRO_FILL_IDS=id1,id2 cargo test --release 整首錄音跑一遍 -- --ignored --nocapture`
    /// （引擎目錄 `VITRO_WHISPER_DIR`、錄音與歌詞資料夾 `VITRO_FILL_TRACKS` 都要自己指）
    #[test]
    #[ignore]
    fn 整首錄音跑一遍() {
        use serde_json::Value;
        let dir = std::path::PathBuf::from(std::env::var("VITRO_WHISPER_DIR").expect("VITRO_WHISPER_DIR=whisper 引擎資料夾"));
        let tracks = std::path::PathBuf::from(std::env::var("VITRO_FILL_TRACKS").expect("VITRO_FILL_TRACKS=錄音資料夾"));
        let tmp = std::env::temp_dir().join("vitro-fill-test");
        let ids = std::env::var("VITRO_FILL_IDS").expect("VITRO_FILL_IDS=id1,id2");
        crate::align::set_wordfill(true);
        let (mut errs, mut base) = (Vec::<f64>::new(), Vec::<f64>::new());
        for id in ids.split(',') {
            let meta: Value = serde_json::from_slice(&std::fs::read(tracks.join(format!("{id}.json"))).unwrap()).unwrap();
            let ly = &meta["lyrics"]["lyrics"];
            let synced = ly["synced"].as_array().unwrap();
            let lines: Vec<(i64, String)> = synced.iter().map(|l| (l["t"].as_i64().unwrap(), l["text"].as_str().unwrap().to_string())).collect();
            let mut truth: Truth = BTreeMap::new();
            if ly["source"] == "word" {
                for l in synced {
                    let mut at = Vec::new();
                    let mut off = 0;
                    for w in l["words"].as_array().into_iter().flatten() {
                        for _ in w["text"].as_str().unwrap().chars().filter(|c| !c.is_whitespace()) {
                            at.push((off, w["t"].as_f64().unwrap()));
                            off += 1;
                        }
                    }
                    truth.insert(l["t"].as_i64().unwrap(), (l["text"].as_str().unwrap().to_string(), at));
                }
            }
            let pcm = read_wav(&tracks.join(format!("{id}.wav")));
            let mut rec = Rec::new(meta["dur"].as_i64().unwrap());
            for r in meta["cover_ms"].as_array().unwrap() {
                let (a, b) = (r[0].as_i64().unwrap(), r[1].as_i64().unwrap());
                let mut ms = a;
                while ms < b {
                    let (i, j) = ((ms as usize * 48) * 2, (((ms + 10).min(b)) as usize * 48) * 2);
                    let f: Vec<f32> = pcm[i.min(pcm.len())..j.min(pcm.len())].iter().map(|x| *x as f32 / 32768.0).collect();
                    rec.push(ms as f64, &f);
                    ms += 10;
                }
            }
            let lang = verify::lang_of(&synced.iter().map(|l| Line { t: 0, text: l["text"].as_str().unwrap().into(), ..Default::default() }).collect::<Vec<_>>());
            let job = Job { id: id.into(), fp: 0, lines: lines.clone(), rec, lang };
            println!("{id}：{} 句，錄到 {:.0} 秒，worth={}", lines.len(), job.rec.covered_ms() / 1000.0, job.worth());
            let t = std::time::Instant::now();
            match run(&dir, &tmp, &job) {
                Ok(o) => {
                    let done = o.lines.iter().filter(|l| !l.is_empty()).count();
                    println!("  {:.0} 秒：補 {done} 句，聽了 {}/{} 段，字對到 {}/{}，句首 {} 句，偏晚 {} ms", t.elapsed().as_secs_f32(), o.chunks[0], o.chunks[1], o.matched, o.units, o.heads, o.bias);
                    if !truth.is_empty() {
                        let (mut e, mut b) = (Vec::new(), Vec::new());
                        score(&o, &lines, &truth, &mut e, &mut b);
                        stat("  Rust 整條", &mut e);
                        errs.extend(e);
                        base.extend(b);
                    }
                }
                Err(e) => println!("  補不成：{e:#}"),
            }
        }
        if !errs.is_empty() {
            stat("全部 Rust 整條", &mut errs);
            stat("全部 平均分到下一句", &mut base);
        }
    }
}
