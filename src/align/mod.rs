//! 歌詞自動對齊：loopback 錄到的聲音 → 人聲能量（`umx`）→ 跟歌詞「該在唱」的時段互相關，
//! 得到整首偏移，順便抓「整份歌詞配錯歌」。
//!
//! 2026-10-20 離線對 35 首驗過（筆記.md「歌詞自動對齊」）：
//! - 首句後 45 秒的粗估跟整首差中位 20 ms，失準的兩首信心都 ≤ 0.01；
//! - 配錯的那首相關峰值 0.07～0.12，其他歌最低 0.32。
//!
//! 流程：播放器每輪送位置（[`poll`]），音訊執行緒送封包（[`audio`]），都帶 QPC 時刻。
//! 兩次輪詢之間的封包先暫存，下一輪確認時間軸沒跳（沒拖進度條、沒暫停）才照位置擺進這首的格子；
//! 每滿 30 秒跑一次模型。量的是對「畫面用的位置」（已扣 lead.rs），所以 X 就是歌詞本身差多少。
//!
//! 結果存在歌詞資料夾的 `align.json`，以歌詞的時間指紋為準：同一份歌詞量過就不再錄。
//! 手動的單曲微調（`offsets.json`）優先。逐句歌詞故意早約 0.3 秒讓句子先出現（使用者決定先保留），
//! 所以逐句只修大錯、而且修完仍保留這 0.3 秒。

pub mod umx;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::config::{self, Config, Paths};
use crate::lyrics::Lyrics;
use crate::player::{NowPlaying, Shared};

const RATE: f64 = 48_000.0;
/// 對齊的格子：10 ms
const CELL_MS: i64 = 10;
/// 搜尋範圍 ±4 秒
const SEARCH_CELLS: i64 = 400;
/// 一次丟模型的長度（離線驗證用的也是 30 秒）
const CHUNK_S: f64 = 30.0;
/// 比這短的連續段不算（模型的 LSTM 需要上下文，邊界也可能擺錯）
const MIN_RUN_S: f64 = 3.0;
/// 相鄰兩輪的「位置 − 時鐘」差超過這麼多就是跳了
const JUMP_MS: f64 = 50.0;

/// 最高相關低於這個就當配錯歌
const WRONG_PEAK: f32 = 0.18;
/// 粗估（首句後 45 秒）的信心門檻：離線失準的兩首都 ≤ 0.01
const ROUGH_CONF: f32 = 0.03;
const ROUGH_AFTER_FIRST_MS: i64 = 45_000;
/// 整首結束時至少要有這麼多有效的格子（60 秒）才估
const FINAL_MIN_CELLS: usize = 6000;
/// 錄到這麼多（佔曲長）算整首：整首的信心低多半是節拍造成的多峰，35 首信心 0.01～0.04 的都跟 ASR 差 ~100 ms 內
const FULL_COVER: f32 = 0.8;
const FINAL_CONF: f32 = 0.02;
/// 只錄到一部分（切走又回來、中途才開）：AlignLab\sim_final.py 切 75 秒以上的窗、信心 ≥ 0.05，
/// 跟整首差 > 200 ms 的 0／265；實機誤存的那首是 73 秒、信心 0.038
const PARTIAL_MIN_S: u32 = 75;
const PARTIAL_CONF: f32 = 0.05;
/// 切走的歌留著格子，切回來接著錄
const PARKED: usize = 4;
/// 「該在唱」又有錄到的格子至少要這麼多（15 秒）才判斷得了配錯
const MIN_SING_CELLS: usize = 1500;
/// 差多少才修。酷狗逐字本身多在 ±130 ms 內，修那個只是在追雜訊
const APPLY_WORD_MS: i64 = 250;
const APPLY_LINE_MS: i64 = 600;
/// 逐句歌詞量到的固定早（6 首 +33～+388），修大錯時保留
const LINE_LEAD_MS: i64 = 300;
/// 太靠搜尋邊界的峰不可信
const MAX_APPLY_MS: i64 = 3500;
/// 人聲能量最響的 5% 跟第 20 百分位差不到這麼多（dB），就是聽不出誰在唱（整段靜音之類），不估
const MIN_SPREAD_DB: f32 = 10.0;
/// 一格（10 ms）混音的平均功率低於這個（約 -70 dBFS）當沒錄到：音量 0、混音器靜音時
/// loopback 照樣給全零的封包，模型在那裡說的「沒人聲」不是這首歌的資訊
const SILENT_POW: f32 = 1e-7;

static WANT: AtomicBool = AtomicBool::new(false);
/// 設定裡關掉了（[`set_enabled`]）：不錄、不量，量過的偏移也不套
static OFF: AtomicBool = AtomicBool::new(false);
/// 備好還沒開執行緒的工人（設定關著啟動的）
static IDLE: Mutex<Option<Worker>> = Mutex::new(None);
static TX: OnceLock<SyncSender<Msg>> = OnceLock::new();

enum Msg {
    Audio(u64, Vec<f32>),
    Poll(Poll),
    Lyrics(String, Box<Lyrics>),
}

#[derive(Clone)]
struct Poll {
    q: u64,
    id: String,
    pos: i64,
    playing: bool,
    dur: i64,
    local: bool,
    /// 位置可信（`lead::Lead::sure`）。自然接歌但還沒學到要扣多少時是 false：
    /// 那時量到的 X 會多出 Spotify 的那半秒，存下來以後每次播都錯
    sure: bool,
}

/// 音訊執行緒問：要不要送封包過來
pub fn wants_audio() -> bool {
    enabled() && WANT.load(Ordering::Relaxed)
}

pub fn enabled() -> bool {
    !OFF.load(Ordering::Relaxed)
}

/// 設定視窗切換時呼叫。關著的時候歌詞執行緒沒送歌詞過來，打開時把正在播的那首補送。
/// 關著時：不開執行緒、不要 loopback、不修 SMTC 位置（`lead.rs`）、不套偏移、不管配錯判斷，跟沒有這個功能一樣
pub fn set_enabled(on: bool, now: Option<(&str, &Lyrics)>) {
    OFF.store(!on, Ordering::Relaxed);
    if !on {
        return;
    }
    spawn();
    if let Some((id, l)) = now {
        lyrics(id, l);
    }
}

/// 音訊執行緒每個封包呼叫（`wants_audio` 時）。`bytes` 是 48 kHz 立體聲 f32 交錯，`q` 是封包的 QPC（100 ns），
/// `silent` 是 WASAPI 標的「這包是靜音」（內容不能用）
pub fn audio(q: u64, bytes: impl Iterator<Item = u8>, silent: bool) {
    let Some(tx) = TX.get() else { return };
    // 靜音的包不送：中間缺一段，前後自然切成兩段，那段的格子留 NaN（沒錄到）
    if silent {
        return;
    }
    let mut v = Vec::with_capacity(1024);
    let mut b = [0u8; 4];
    for (i, x) in bytes.enumerate() {
        b[i % 4] = x;
        if i % 4 == 3 {
            v.push(f32::from_le_bytes(b));
        }
    }
    // 滿了（模型正在算、或卡住）就丟：少一段而已，不能讓音訊執行緒等
    let _ = tx.try_send(Msg::Audio(q, v));
}

/// 播放器每輪呼叫。`sure` 見 [`Poll::sure`]
pub fn poll(np: Option<&NowPlaying>, sure: bool) {
    let (Some(tx), Some(t), true) = (TX.get(), np, enabled()) else { return };
    let pos = t.progress_ms + if t.playing { t.at.elapsed().as_millis() as i64 } else { 0 };
    let _ = tx.try_send(Msg::Poll(Poll {
        q: crate::alignrec::qpc_hns(),
        id: t.id.clone(),
        pos,
        playing: t.playing,
        dur: t.duration_ms,
        local: t.source == "smtc",
        sure,
    }));
}

/// 歌詞執行緒抓到歌詞時呼叫
pub fn lyrics(id: &str, l: &Lyrics) {
    if let (Some(tx), true) = (TX.get(), enabled()) {
        let _ = tx.try_send(Msg::Lyrics(id.to_string(), Box::new(l.clone())));
    }
}

// ---------------------------- 存檔 ----------------------------

const STORE_FILE: &str = "align.json";

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    /// 量的是哪一份歌詞（[`fingerprint`]）
    pub fp: u64,
    /// 人聲實際 − 歌詞（ms）、最高相關、信心、用了幾秒的聲音
    pub x: i64,
    pub peak: f32,
    pub conf: f32,
    pub secs: u32,
    /// 錄到的佔曲長多少（舊紀錄沒有，當 0）
    pub cover: f32,
    /// 要套用的單曲偏移（`lyrics::shift` 的單位），0 是不修
    pub apply: i64,
    /// 判斷為配錯歌的來源（`Lyrics::provider`），重抓時跳過
    pub bad: Vec<String>,
}

fn read_store(dir: &Path) -> BTreeMap<String, Entry> {
    config::read_json(&dir.join(STORE_FILE))
}

pub fn entry(dir: &Path, key: &str) -> Option<Entry> {
    read_store(dir).remove(key)
}

fn save_entry(dir: &Path, key: &str, e: Entry) {
    let mut m = read_store(dir);
    m.insert(key.to_string(), e);
    let _ = config::write_json(&dir.join(STORE_FILE), &m);
}

/// 歌詞的時間指紋：只看時間不看字，簡繁切換不會讓它變
pub fn fingerprint(l: &Lyrics) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |v: i64| {
        for b in v.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for ln in &l.synced {
        eat(ln.t);
        for w in ln.words.iter().flatten() {
            eat(w.t);
            eat(w.d);
        }
    }
    h
}

/// 整首的結果可不可信：幾乎整首都錄到就照信心決定修不修；只錄到一部分要更多證據
fn trusted(secs: u32, cover: f32, conf: f32) -> bool {
    cover >= FULL_COVER || (secs >= PARTIAL_MIN_S && conf >= PARTIAL_CONF)
}

impl Entry {
    /// 這份歌詞量過、結果可信、而且不是配錯的。舊版存的證據不足的紀錄在這裡被當成沒量過，會重量
    fn measured(&self, l: &Lyrics) -> bool {
        self.fp == fingerprint(l) && !self.bad.contains(&l.provider) && trusted(self.secs, self.cover, self.conf)
    }
}

/// 套用的偏移：這份歌詞量過、而且不是配錯的
pub fn auto_shift(dir: &Path, key: &str, l: &Lyrics) -> i64 {
    if !enabled() {
        return 0;
    }
    entry(dir, key).filter(|e| e.measured(l)).map_or(0, |e| e.apply)
}

// ---------------------------- 估計 ----------------------------

#[derive(Debug, Clone, Copy)]
pub struct Est {
    pub x: i64,
    pub peak: f32,
    pub conf: f32,
}

/// 歌詞說「這裡在唱」的格子。逐字用每個字的區間；逐句用到下一句開頭，最長 8 秒、字數 × 0.4 秒
fn expected_mask(l: &Lyrics, n: usize) -> Vec<f32> {
    let mut m = vec![0f32; n];
    let mut set = |a: i64, b: i64| {
        let (a, b) = ((a / CELL_MS).clamp(0, n as i64) as usize, (b / CELL_MS).clamp(0, n as i64) as usize);
        if a < b {
            m[a..b].fill(1.0);
        }
    };
    let lines: Vec<_> = l.synced.iter().filter(|x| !x.text.trim().is_empty()).collect();
    for (i, ln) in lines.iter().enumerate() {
        match ln.words.as_ref().filter(|w| !w.is_empty()) {
            Some(ws) => {
                for w in ws {
                    set(w.t, w.t + w.d.max(80));
                }
            }
            None => {
                let next = lines.get(i + 1).map_or(ln.t + 5000, |x| x.t);
                let est = ln.text.chars().filter(|c| !c.is_whitespace()).count() as i64 * 400;
                set(ln.t, (next - 150).min(ln.t + est.clamp(1000, 8000)));
            }
        }
    }
    m
}

/// `db`：每格人聲能量（dB），NaN 是沒錄到。回傳 X（人聲實際 − 歌詞，ms）、最高相關、
/// 信心（最高峰減掉離它 300 ms 外的第二高峰）。有效格子不到 10 秒、或能量沒有起伏回 `None`
pub fn estimate(db: &[f32], mask: &[f32]) -> Option<Est> {
    let n = db.len().min(mask.len());
    let mut valid: Vec<f32> = db[..n].iter().copied().filter(|v| !v.is_nan()).collect();
    if valid.len() < 1000 {
        return None;
    }
    let pct = |v: &mut Vec<f32>, p: f32| {
        let k = ((v.len() as f32 * p) as usize).min(v.len() - 1);
        *v.select_nth_unstable_by(k, |a, b| a.total_cmp(b)).1
    };
    let top = pct(&mut valid, 0.95);
    // 沒有起伏時每個位移的相關都是 -1，最高峰也是 -1，會被當成配錯歌
    if top - pct(&mut valid, 0.2) < MIN_SPREAD_DB {
        return None;
    }
    // 比最響的 5% 低 30 dB 以上當沒在唱，中間線性
    let f: Vec<f32> = db[..n].iter().map(|&v| if v.is_nan() { f32::NAN } else { ((v - (top - 30.0)) / 30.0).clamp(0.0, 1.0) }).collect();
    let mut scores = Vec::with_capacity((2 * SEARCH_CELLS + 1) as usize);
    for lag in -SEARCH_CELLS..=SEARCH_CELLS {
        // 歌詞往後挪 lag 格：mask 的第 i 格對到 f 的第 i+lag 格
        let (mut c, mut sa, mut sb, mut saa, mut sbb, mut sab) = (0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
        for (i, &b) in mask[..n].iter().enumerate() {
            let j = i as i64 + lag;
            if j < 0 || j >= n as i64 {
                continue;
            }
            let a = f[j as usize];
            if a.is_nan() {
                continue;
            }
            let (a, b) = (a as f64, b as f64);
            c += 1.0;
            sa += a;
            sb += b;
            saa += a * a;
            sbb += b * b;
            sab += a * b;
        }
        let r = if c < 1000.0 {
            -1.0
        } else {
            let va = saa - sa * sa / c;
            let vb = sbb - sb * sb / c;
            if va <= 1e-9 || vb <= 1e-9 { -1.0 } else { (sab - sa * sb / c) / (va * vb).sqrt() }
        };
        scores.push(r as f32);
    }
    let best = scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i)?;
    let second = scores
        .iter()
        .enumerate()
        .filter(|(i, _)| (*i as i64 - best as i64).abs() > 300 / CELL_MS)
        .map(|(_, &s)| s)
        .fold(-1f32, f32::max);
    Some(Est { x: (best as i64 - SEARCH_CELLS) * CELL_MS, peak: scores[best], conf: scores[best] - second })
}

/// 量到的 X 要不要修、修多少（`lyrics::shift` 的單位）
fn decide(e: &Est, word: bool) -> i64 {
    if e.peak < WRONG_PEAK || e.x.abs() > MAX_APPLY_MS {
        return 0;
    }
    if word {
        if e.x.abs() >= APPLY_WORD_MS { -e.x } else { 0 }
    } else if (e.x - LINE_LEAD_MS).abs() >= APPLY_LINE_MS {
        -(e.x - LINE_LEAD_MS)
    } else {
        0
    }
}

/// 混音本身幾乎沒聲音的格子改回 NaN（[`SILENT_POW`]）。`run` 是交錯立體聲，從歌曲的 `start_ms` 開始
fn mute_silent(db: &mut [f32], run: &[f32], start_ms: f64) {
    let frames = run.len() / 2;
    let per_cell = (RATE * CELL_MS as f64 / 1000.0) as usize;
    let first = (start_ms / CELL_MS as f64).ceil().max(0.0) as usize;
    for (c, v) in db.iter_mut().enumerate().skip(first) {
        let f0 = ((c as f64 * CELL_MS as f64 - start_ms) * RATE / 1000.0).round().max(0.0) as usize;
        if f0 >= frames {
            break;
        }
        let f1 = (f0 + per_cell).min(frames);
        let pow = run[f0 * 2..f1 * 2].iter().map(|x| x * x).sum::<f32>() / ((f1 - f0) * 2) as f32;
        if pow < SILENT_POW {
            *v = f32::NAN;
        }
    }
}

// ---------------------------- 工作執行緒 ----------------------------

struct Song {
    id: String,
    dur: i64,
    local: bool,
    lyrics: Option<Lyrics>,
    /// 每格人聲能量（dB），NaN 是沒錄到
    db: Vec<f32>,
    /// 正在累積的連續段：起點（歌曲毫秒）與交錯立體聲取樣
    run_ms: f64,
    run: Vec<f32>,
    /// 上一輪之後收到、還沒確認的封包
    pending: Vec<(u64, Vec<f32>)>,
    rough_done: bool,
    /// 這份歌詞已經量過（存檔裡有），不用再錄
    done: bool,
    /// 快播完時已經試過收尾（沒存成的話不要每輪重算）
    ended: bool,
    /// 這次播放判成配錯的來源（第二份也配不上時撤回用）
    banned: Vec<String>,
    /// 這次播放錄到的聲音不可信（連兩份歌詞都配不上），這次不再量也不再判斷
    deaf: bool,
    /// 位置不可信、這首先不錄，已經寫過紀錄
    unsure_logged: bool,
}

impl Song {
    fn new(p: &Poll) -> Self {
        Self {
            id: p.id.clone(),
            dur: p.dur,
            local: p.local,
            lyrics: None,
            db: vec![f32::NAN; (p.dur.max(0) / CELL_MS) as usize],
            run_ms: 0.0,
            run: Vec::new(),
            pending: Vec::new(),
            rough_done: false,
            done: false,
            ended: false,
            banned: Vec::new(),
            deaf: false,
            unsure_logged: false,
        }
    }

    fn synced(&self) -> Option<&Lyrics> {
        self.lyrics.as_ref().filter(|l| !l.synced.is_empty() && matches!(l.source.as_str(), "word" | "synced"))
    }

    fn first_line_ms(&self) -> i64 {
        self.synced().and_then(|l| l.synced.first()).map_or(0, |l| l.t.max(0))
    }
}

struct Worker {
    dir: PathBuf,
    model_path: PathBuf,
    log: PathBuf,
    shared: Arc<Shared>,
    refetch: Sender<NowPlaying>,
    model: Option<umx::Umx>,
    /// 下載失敗後多久再試
    retry_at: Option<Instant>,
    song: Option<Song>,
    /// 沒量完就切走的歌（新的在後）
    parked: Vec<Song>,
    last: Option<Poll>,
    /// 比這首的第一輪輪詢先到的歌詞（設定裡剛打開時補送的那份）
    stash: Option<(String, Lyrics)>,
}

/// 設定關著時只把工人備好、不開執行緒，在設定裡打開時（[`set_enabled`]）才開
pub fn start(cfg: &Config, paths: &Paths, shared: Arc<Shared>, refetch: Sender<NowPlaying>) {
    OFF.store(!cfg.lyrics_autoalign, Ordering::Relaxed);
    if std::env::var_os("VITRO_NO_ALIGN").is_some() {
        return;
    }
    *IDLE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Worker {
        dir: paths.lyrics.clone(),
        model_path: paths.data.join("models").join("umx-vocals-b62c91ce.pth"),
        log: paths.cache.join("align.log"),
        shared,
        refetch,
        model: None,
        retry_at: None,
        song: None,
        parked: Vec::new(),
        last: None,
        stash: None,
    });
    if enabled() {
        spawn();
    }
}

/// 開工人的執行緒。只開一次，之後關掉只是不送東西過去
fn spawn() {
    let Some(mut w) = IDLE.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    // 30 秒的封包約 3000 個；模型一次算 1～2 秒，留足夠的空間
    let (tx, rx) = std::sync::mpsc::sync_channel(8192);
    if TX.set(tx).is_err() {
        return;
    }
    let _ = std::thread::Builder::new().name("align".into()).spawn(move || {
        lower_priority();
        w.run(rx);
    });
}

fn lower_priority() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
    // SAFETY: 只改自己這條執行緒的優先權
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

impl Worker {
    fn run(&mut self, rx: Receiver<Msg>) {
        while let Ok(m) = rx.recv() {
            self.handle(m);
        }
    }

    fn handle(&mut self, m: Msg) {
        // 剛關掉時佇列裡還沒處理的就丟掉
        if !enabled() {
            return;
        }
        match m {
            Msg::Audio(q, v) => {
                if let Some(s) = self.song.as_mut()
                    && !s.done
                {
                    s.pending.push((q, v));
                }
            }
            Msg::Poll(p) => self.on_poll(p),
            Msg::Lyrics(id, l) => self.on_lyrics(&id, *l),
        }
        self.update_want();
    }

    fn log(&self, msg: &str) {
        use std::io::Write;
        // 一首十來行；超過 1 MB 就從頭寫
        let big = std::fs::metadata(&self.log).is_ok_and(|m| m.len() > 1 << 20);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).write(true).append(!big).truncate(big).open(&self.log) {
            let _ = writeln!(f, "{} {msg}", config::now_secs() as u64);
        }
    }

    fn update_want(&mut self) {
        let want = self.model.is_some()
            && self.song.as_ref().is_some_and(|s| s.local && !s.done && s.synced().is_some())
            && self.last.as_ref().is_some_and(|p| p.playing);
        WANT.store(want, Ordering::Relaxed);
    }

    /// 要用模型了：沒有就下載（只在有歌詞要量的時候）
    fn ensure_model(&mut self) {
        if self.model.is_some() || self.retry_at.is_some_and(|t| Instant::now() < t) {
            return;
        }
        if !self.model_path.exists() {
            self.shared.toast("下載歌詞對齊模型（36 MB）…");
            if let Err(e) = download(&self.model_path) {
                crate::warn!("歌詞對齊模型下載失敗：{e}");
                self.shared.toast("歌詞對齊模型下載失敗，30 分鐘後再試");
                self.retry_at = Some(Instant::now() + Duration::from_secs(1800));
                return;
            }
        }
        match umx::Umx::load(&self.model_path) {
            Ok(m) => self.model = Some(m),
            Err(e) => {
                crate::warn!("歌詞對齊模型讀不起來：{e}");
                // 壞掉的檔案刪掉，下次重新下載
                let _ = std::fs::remove_file(&self.model_path);
                self.retry_at = Some(Instant::now() + Duration::from_secs(1800));
            }
        }
    }

    fn on_lyrics(&mut self, id: &str, l: Lyrics) {
        let Some(s) = self.song.as_mut().filter(|s| s.id == id) else {
            self.stash = Some((id.to_string(), l));
            return;
        };
        let done = entry(&self.dir, id).is_some_and(|e| e.measured(&l));
        let msg = format!("歌詞 {id} {}/{} 量過={done}", l.source, l.provider);
        s.lyrics = Some(l);
        s.done = done || s.deaf;
        self.log(&msg);
        let Some(s) = self.song.as_mut() else { return };
        // 換了一份歌詞（例如配錯後重抓）：手上的聲音拿新的歌詞重算
        s.rough_done = false;
        if s.synced().is_some() && !s.done {
            self.ensure_model();
            self.check_rough();
        }
    }

    fn on_poll(&mut self, p: Poll) {
        let prev = self.last.replace(p.clone());
        if self.song.as_ref().is_none_or(|s| s.id != p.id) {
            if let Some(old) = self.song.take() {
                self.finish(old);
            }
            self.song = self.resume(&p).or_else(|| (p.dur > 0).then(|| Song::new(&p)));
            if let Some((id, l)) = self.stash.take().filter(|(id, _)| *id == p.id) {
                self.on_lyrics(&id, l);
            }
            return;
        }
        let Some(prev) = prev.filter(|q| q.id == p.id) else { return };
        let clock = |x: &Poll| x.pos as f64 - x.q as f64 / 1e4;
        let ok = prev.playing && p.playing && prev.sure && p.sure && (clock(&p) - clock(&prev)).abs() <= JUMP_MS;
        let Some(s) = self.song.as_mut() else { return };
        let pending = std::mem::take(&mut s.pending);
        if !p.sure && !s.unsure_logged && !s.done {
            s.unsure_logged = true;
            let msg = format!("位置不準 {}：自然接歌、還沒學到 Spotify 早多少，先不錄", s.id);
            self.log(&msg);
        }
        if !ok {
            // 跳了（拖進度條、Spotify 更正、暫停）：這段之前的照常算，暫存的不要
            self.flush_run();
            return;
        }
        for (q, v) in pending {
            let ms = prev.pos as f64 + (q as f64 - prev.q as f64) / 1e4;
            let Some(s) = self.song.as_mut() else { return };
            let end = s.run_ms + (s.run.len() / 2) as f64 * 1000.0 / RATE;
            if s.run.is_empty() || (ms - end).abs() > 15.0 {
                // 中間缺了一段：前面那段收掉，從這裡重新開始
                self.flush_run();
                let s = self.song.as_mut().unwrap();
                s.run_ms = ms;
            }
            let s = self.song.as_mut().unwrap();
            s.run.extend_from_slice(&v);
            if s.run.len() as f64 >= CHUNK_S * RATE * 2.0 {
                self.flush_run();
                // 下一段接在後面
                let s = self.song.as_mut().unwrap();
                s.run_ms = ms + (v.len() / 2) as f64 * 1000.0 / RATE;
            }
        }
        // 快播完了就先收尾：換歌那一刻 Spotify 的位置會先歸零，等不到
        let Some(s) = self.song.as_mut() else { return };
        if p.pos < s.dur - 2500 {
            s.ended = false;
        } else if !s.done && !s.ended {
            s.ended = true;
            self.flush_run();
            if self.final_estimate()
                && let Some(s) = self.song.as_mut()
            {
                s.done = true;
            }
        }
    }

    /// 把累積的連續段丟進模型，結果寫進這首的格子
    fn flush_run(&mut self) {
        let Some(s) = self.song.as_mut() else { return };
        let run = std::mem::take(&mut s.run);
        let start = s.run_ms;
        if (run.len() / 2) as f64 / RATE < MIN_RUN_S || s.done {
            return;
        }
        let Some(m) = self.model.as_ref() else { return };
        let t = Instant::now();
        let e = m.vocal_energy(&umx::spectrogram(&run));
        self.log(&format!("段 {:.1}～{:.1} 秒，算了 {:.1} 秒", start / 1000.0, start / 1000.0 + (run.len() / 2) as f64 / RATE, t.elapsed().as_secs_f32()));
        let s = self.song.as_mut().unwrap();
        // 每格的中心在 j·HOP + NFFT/2；中間的格子線性內插
        let center = |j: usize| start + (j * umx::HOP + umx::NFFT / 2) as f64 * 1000.0 / RATE;
        let db: Vec<f32> = e.iter().map(|v| 10.0 * (v + 1e-10).log10()).collect();
        for j in 0..db.len().saturating_sub(1) {
            let (t0, t1) = (center(j), center(j + 1));
            let a = (t0 / CELL_MS as f64).ceil().max(0.0) as usize;
            let b = ((t1 / CELL_MS as f64).ceil().max(0.0) as usize).min(s.db.len());
            for c in a..b {
                let w = ((c as f64 * CELL_MS as f64 - t0) / (t1 - t0)) as f32;
                s.db[c] = db[j] * (1.0 - w) + db[j + 1] * w;
            }
        }
        mute_silent(&mut s.db, &run, start);
        self.check_rough();
    }

    /// 首句後 45 秒：先粗修，或判斷配錯
    fn check_rough(&mut self) {
        let Some(s) = self.song.as_ref() else { return };
        if s.rough_done || s.done {
            return;
        }
        let Some(l) = s.synced() else { return };
        let until = ((s.first_line_ms() + ROUGH_AFTER_FIRST_MS) / CELL_MS) as usize;
        let have = s.db.iter().take(until).filter(|v| !v.is_nan()).count();
        // 那段有錄到 30 秒以上才算
        if have < 3000 {
            return;
        }
        let mask = expected_mask(l, s.db.len());
        let db: Vec<f32> = s.db.iter().enumerate().map(|(i, &v)| if i < until { v } else { f32::NAN }).collect();
        let sing = db.iter().zip(&mask).filter(|(v, m)| !v.is_nan() && **m > 0.0).count();
        let Some(e) = estimate(&db, &mask) else { return };
        let word = l.has_words();
        let provider = l.provider.clone();
        let id = s.id.clone();
        self.log(&format!("粗估 {id} {provider} X={} 峰={:.2} 信={:.2} 唱={sing}", e.x, e.peak, e.conf));
        self.song.as_mut().unwrap().rough_done = true;
        if e.peak < WRONG_PEAK && sing >= MIN_SING_CELLS {
            self.wrong(&id, &provider);
            return;
        }
        if e.conf >= ROUGH_CONF {
            self.apply(&id, decide(&e, word));
        }
    }

    /// 整首收尾（快播完或換歌時）：存結果。有估出東西回 true
    fn final_estimate(&mut self) -> bool {
        let Some(s) = self.song.as_ref() else { return false };
        let Some(l) = s.synced() else { return false };
        let cells = s.db.iter().filter(|v| !v.is_nan()).count();
        if cells < FINAL_MIN_CELLS {
            return false;
        }
        let mask = expected_mask(l, s.db.len());
        let sing = s.db.iter().zip(&mask).filter(|(v, m)| !v.is_nan() && **m > 0.0).count();
        let Some(e) = estimate(&s.db, &mask) else { return false };
        let (id, provider, fp, word) = (s.id.clone(), l.provider.clone(), fingerprint(l), l.has_words());
        let (secs, cover) = ((cells / 100) as u32, cells as f32 / s.db.len().max(1) as f32);
        self.log(&format!("整首 {id} {provider} X={} 峰={:.2} 信={:.2} 秒={secs} 涵蓋={:.0}% 唱={sing}", e.x, e.peak, e.conf, cover * 100.0));
        if e.peak < WRONG_PEAK && sing >= MIN_SING_CELLS {
            self.wrong(&id, &provider);
            return true;
        }
        if !trusted(secs, cover, e.conf) {
            // 粗估套上的留著（只到這次播完），下次播再量
            self.log(&format!("不存 {id}：只錄到一部分、證據不夠"));
            return false;
        }
        let apply = if e.conf >= FINAL_CONF { decide(&e, word) } else { 0 };
        let mut ent = entry(&self.dir, &id).unwrap_or_default();
        ent.fp = fp;
        ent.x = e.x;
        ent.peak = e.peak;
        ent.conf = e.conf;
        ent.secs = secs;
        ent.cover = cover;
        ent.apply = apply;
        save_entry(&self.dir, &id, ent);
        self.apply(&id, apply);
        true
    }

    fn finish(&mut self, s: Song) {
        self.song = Some(s);
        self.flush_run();
        if self.song.as_ref().is_some_and(|s| !s.done && !s.ended) && self.final_estimate() {
            self.song = None;
            return;
        }
        // 沒量完：格子留著，切回來接著錄（格子是聲音，跟哪份歌詞無關）
        let Some(mut s) = self.song.take() else { return };
        if s.done || s.db.iter().all(|v| v.is_nan()) {
            return;
        }
        s.pending.clear();
        self.parked.retain(|x| x.id != s.id);
        self.parked.push(s);
        if self.parked.len() > PARKED {
            self.parked.remove(0);
        }
    }

    /// 切回沒量完的歌
    fn resume(&mut self, p: &Poll) -> Option<Song> {
        let i = self.parked.iter().position(|s| s.id == p.id && s.dur == p.dur)?;
        let s = self.parked.remove(i);
        let have = s.db.iter().filter(|v| !v.is_nan()).count();
        self.log(&format!("接著錄 {}：已有 {} 秒", s.id, have / 100));
        Some(s)
    }

    /// 套用（如果這首還在放、歌詞還是同一份）
    fn apply(&self, id: &str, ms: i64) {
        let still = self.shared.now.lock().ok().is_some_and(|g| g.as_ref().is_some_and(|n| n.id == id));
        if !still {
            return;
        }
        let Ok(mut g) = self.shared.lyrics.lock() else { return };
        let Some(l) = g.as_mut() else { return };
        if (l.auto_ms - ms).abs() >= 100 && l.offset_ms == 0 && ms != 0 {
            self.shared.toast(format!("歌詞時間自動校正 {:+.1} 秒", ms as f64 / 1000.0));
        }
        l.auto_ms = ms;
    }

    /// 配錯歌：記下這個來源，叫歌詞執行緒重抓（會跳過它）。
    ///
    /// 同一次播放換來的下一份也配不上，多半不是兩份歌詞都錯，而是錄到的聲音不對
    /// （Spotify 輸出到別的裝置、混進別的聲音）：撤回這次記下的配錯、丟掉重抓蓋掉的快取，
    /// 換回原本的歌詞，這次播放不再量。不然同一批聲音會一份接一份判下去，整條來源鏈都被封掉
    fn wrong(&mut self, id: &str, provider: &str) {
        if provider == "本機" {
            return;
        }
        let now = self.shared.now.lock().ok().and_then(|g| g.clone()).filter(|t| t.id == id);
        let earlier = self.song.as_ref().filter(|s| s.id == id).map(|s| s.banned.clone()).unwrap_or_default();
        let mut ent = entry(&self.dir, id).unwrap_or_default();
        if !earlier.is_empty() {
            ent.bad.retain(|b| !earlier.contains(b));
            save_entry(&self.dir, id, ent);
            crate::lyrics::forget(&self.dir, id);
            self.log(&format!("聽不準 {id}：{provider} 也配不上，撤回 {}", earlier.join("、")));
            if let Some(s) = self.song.as_mut() {
                s.banned.clear();
                s.deaf = true;
                s.done = true;
                s.lyrics = None;
            }
            if let Some(t) = now {
                self.shared.toast("這首的聲音對不上歌詞，換回原本的歌詞");
                let _ = self.refetch.send(t);
            }
            return;
        }
        if !ent.bad.iter().any(|b| b == provider) {
            ent.bad.push(provider.to_string());
        }
        save_entry(&self.dir, id, ent);
        self.log(&format!("配錯 {id} {provider}"));
        if let Some(t) = now {
            self.shared.toast("這份歌詞好像不是這首歌，換別的來源");
            let _ = self.refetch.send(t);
        }
        if let Some(s) = self.song.as_mut() {
            s.banned.push(provider.to_string());
            s.lyrics = None;
        }
    }
}

fn download(dst: &Path) -> anyhow::Result<()> {
    let http = reqwest::blocking::Client::builder()
        .user_agent(config::user_agent())
        // Zenodo 實測只有 285 KB/s，36 MB 要兩分多鐘
        .timeout(Duration::from_secs(1200))
        .build()?;
    let r = http.get(umx::URL).send()?.error_for_status()?;
    let bytes = r.bytes()?;
    if bytes.len() as u64 != umx::SIZE || umx::md5_hex(&bytes) != umx::MD5 {
        anyhow::bail!("下載的檔案不對（{} 位元組）", bytes.len());
    }
    if let Some(d) = dst.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = dst.with_extension("part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::{Line, Word};

    fn word_lyrics(lines: &[(i64, &[(i64, i64)])]) -> Lyrics {
        Lyrics {
            synced: lines
                .iter()
                .map(|(t, ws)| Line {
                    t: *t,
                    text: "啦".repeat(ws.len()),
                    words: Some(ws.iter().map(|&(t, d)| Word { t, d, text: "啦".into() }).collect()),
                    ..Default::default()
                })
                .collect(),
            source: "word".into(),
            provider: "KuGou".into(),
            ..Default::default()
        }
    }

    /// 造一首 60 秒的歌：歌詞說的每個字晚 `x` ms 真的唱出來
    fn fake(x: i64) -> (Lyrics, Vec<f32>) {
        let mut lines = vec![];
        let mut t = 5000;
        let mut words_store: Vec<Vec<(i64, i64)>> = vec![];
        while t < 55_000 {
            let n = 4 + (t / 1000 % 5) as usize;
            let ws: Vec<(i64, i64)> = (0..n).map(|i| (t + i as i64 * 350, 300)).collect();
            words_store.push(ws);
            t += 3000 + (t / 700 % 7) * 250;
        }
        for ws in &words_store {
            lines.push((ws[0].0, ws.as_slice()));
        }
        let l = word_lyrics(&lines);
        let mask = expected_mask(&l, 6000);
        let db: Vec<f32> = (0..6000)
            .map(|i| {
                let j = i as i64 - x / CELL_MS;
                let sing = j >= 0 && j < 6000 && mask[j as usize] > 0.0;
                if sing { -10.0 } else { -45.0 }
            })
            .collect();
        (l, db)
    }

    #[test]
    fn 找得到偏移() {
        for x in [-2000, -300, 0, 480, 1500] {
            let (l, db) = fake(x);
            let e = estimate(&db, &expected_mask(&l, db.len())).unwrap();
            assert_eq!(e.x, x, "{e:?}");
            assert!(e.peak > 0.9);
        }
    }

    #[test]
    fn 沒錄到的格子不算() {
        let (l, mut db) = fake(700);
        for v in db.iter_mut().skip(3000) {
            *v = f32::NAN;
        }
        let e = estimate(&db, &expected_mask(&l, db.len())).unwrap();
        assert_eq!(e.x, 700);
        for v in db.iter_mut().skip(900) {
            *v = f32::NAN;
        }
        assert!(estimate(&db, &expected_mask(&l, db.len())).is_none(), "不到 10 秒不估");
    }

    #[test]
    fn 什麼時候修() {
        let e = |x, peak| Est { x, peak, conf: 0.1 };
        assert_eq!(decide(&e(120, 0.6), true), 0, "逐字差一點不修");
        assert_eq!(decide(&e(-2000, 0.6), true), 2000);
        assert_eq!(decide(&e(330, 0.6), false), 0, "逐句的固定早保留");
        assert_eq!(decide(&e(2300, 0.6), false), -2000, "逐句大錯修到剩 0.3 秒");
        assert_eq!(decide(&e(-2000, 0.1), true), 0, "配錯不修");
        assert_eq!(decide(&e(3900, 0.6), true), 0, "太靠邊界不修");
    }

    /// 拿 AlignData 切好的一首（`tools\align\slice_tracks.py` 的輸出）照播放的節奏重播整個流程：
    /// 每 10 ms 一個封包、每 500 ms 一輪輪詢，沒錄到的地方不送。手動跑：
    /// `VITRO_ALIGN_REPLAY=<tracks>\<id>`（不含副檔名）`VITRO_UMX_MODEL=<pth>`
    /// `cargo test --release replay -- --ignored --nocapture`。〈黎明〉（k4629c1d6c6bcb40f4889）要判成配錯
    #[test]
    #[ignore]
    fn replay() {
        let (Some(base), Some(model)) = (std::env::var_os("VITRO_ALIGN_REPLAY"), std::env::var_os("VITRO_UMX_MODEL")) else { return };
        let base = PathBuf::from(base);
        let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(base.with_extension("json")).unwrap()).unwrap();
        let id = meta["id"].as_str().unwrap().to_string();
        let dur = meta["dur"].as_i64().unwrap();
        let ly: Lyrics = serde_json::from_value(meta["lyrics"]["lyrics"].clone()).unwrap();
        // 切出來的區間在毫秒邊界上有 1 ms 的縫，實際播放時是連續的，20 ms 以內的縫合起來
        let mut cover: Vec<(i64, i64)> = vec![];
        for s in meta["cover_ms"].as_array().unwrap() {
            let (a, b) = (s[0].as_i64().unwrap(), s[1].as_i64().unwrap());
            match cover.last_mut() {
                Some(l) if a - l.1 <= 20 => l.1 = b,
                _ => cover.push((a, b)),
            }
        }
        let raw = std::fs::read(base.with_extension("wav")).unwrap();
        let pcm: Vec<f32> = raw[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();

        let dir = std::env::temp_dir().join(format!("vitro-align-replay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shared = Arc::new(Shared::default());
        *shared.now.lock().unwrap() = Some(NowPlaying {
            playing: true,
            id: id.clone(),
            title: meta["title"].as_str().unwrap_or("").into(),
            artist: String::new(),
            album: String::new(),
            album_uri: String::new(),
            art: crate::player::Art::SmtcThumb,
            duration_ms: dur,
            progress_ms: 0,
            device: String::new(),
            shuffle: false,
            repeat: crate::smtc::Repeat::Off,
            smtc_stamp: None,
            at: Instant::now(),
            source: "smtc",
            track_uri: String::new(),
            volume: None,
            supports_volume: true,
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let mut w = Worker {
            dir: dir.clone(),
            model_path: PathBuf::from(model),
            log: dir.join("align.log"),
            shared: shared.clone(),
            refetch: tx,
            model: None,
            retry_at: None,
            song: None,
            parked: Vec::new(),
            last: None,
            stash: None,
        };
        w.ensure_model();
        assert!(w.model.is_some());
        let poll = |pos: i64, id: &str| Msg::Poll(Poll { q: pos as u64 * 10_000, id: id.into(), pos, playing: true, dur, local: true, sure: true });
        w.handle(poll(0, &id));
        w.handle(Msg::Lyrics(id.clone(), Box::new(ly)));
        let t = Instant::now();
        let mut ms = 0;
        while ms + 10 <= dur && (ms as usize + 10) * 96 <= pcm.len() {
            if cover.iter().any(|&(a, b)| a <= ms && ms + 10 <= b) {
                let s = &pcm[ms as usize * 96..(ms as usize + 10) * 96];
                w.handle(Msg::Audio(ms as u64 * 10_000, s.to_vec()));
            }
            ms += 10;
            if ms % 500 == 0 {
                w.handle(poll(ms, &id));
            }
        }
        w.handle(poll(0, "next"));
        eprintln!("重播 {:.1} 秒的歌算了 {:?}", dur as f64 / 1000.0, t.elapsed());
        eprintln!("{}", std::fs::read_to_string(dir.join("align.log")).unwrap_or_default());
        eprintln!("align.json：{}", std::fs::read_to_string(dir.join(STORE_FILE)).unwrap_or_default());
        eprintln!("通知：{:?}；叫重抓：{}", shared.toast.lock().unwrap().take(), rx.try_recv().is_ok());
        for n in ["align.log", STORE_FILE] {
            let _ = std::fs::remove_file(dir.join(n));
        }
    }

    #[test]
    fn 只錄到一部分要更多證據() {
        assert!(!trusted(73, 0.44, 0.038), "實機誤存的那首");
        assert!(trusted(90, 0.5, 0.06));
        assert!(trusted(205, 0.96, 0.01), "整首都錄到：信心低照存（不修）");
        // 舊紀錄沒有涵蓋率
        let ok = Entry { secs: 133, conf: 0.068, ..Default::default() };
        let bad = Entry { secs: 73, conf: 0.038, apply: -550, ..Default::default() };
        let (l, _) = fake(0);
        let fp = fingerprint(&l);
        assert!(Entry { fp, ..ok }.measured(&l));
        assert!(!Entry { fp, ..bad }.measured(&l), "證據不足的舊紀錄當沒量過，重量");
    }

    #[test]
    fn 切回來接著錄() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let dir = std::env::temp_dir().join(format!("vitro-align-park-{}", std::process::id()));
        let mut w = Worker {
            dir: dir.clone(),
            model_path: PathBuf::new(),
            log: dir.join("align.log"),
            shared: Arc::new(Shared::default()),
            refetch: tx,
            model: None,
            retry_at: None,
            song: None,
            parked: Vec::new(),
            last: None,
            stash: None,
        };
        let poll = |id: &str, pos: i64| Msg::Poll(Poll { q: pos as u64 * 10_000, id: id.into(), pos, playing: true, dur: 100_000, local: true, sure: true });
        w.handle(poll("a", 1000));
        w.song.as_mut().unwrap().db[100..6100].fill(-20.0);
        w.handle(poll("b", 0));
        assert_eq!(w.parked.len(), 1);
        w.handle(poll("a", 70_000));
        let s = w.song.as_ref().unwrap();
        assert_eq!((s.id.as_str(), s.db.iter().filter(|v| !v.is_nan()).count()), ("a", 6000), "前面錄的還在");
        assert!(w.parked.is_empty());
        // 沒錄到東西的不留
        w.handle(poll("b", 0));
        w.handle(poll("c", 0));
        assert_eq!(w.parked.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["a"]);
        let _ = std::fs::remove_file(dir.join("align.log"));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn 靜音不估_不會被當成配錯() {
        let (l, _) = fake(0);
        // 音量 0：模型給的能量全是 0，換成 dB 每格一樣
        let db = vec![-100.0f32; 6000];
        assert!(estimate(&db, &expected_mask(&l, db.len())).is_none());
    }

    #[test]
    fn 混音沒聲音的格子當沒錄到() {
        // 從歌曲 1 秒開始的 3 秒：中間那 1 秒（歌曲 2～3 秒）全零
        let frames = 3 * RATE as usize;
        let mut run = vec![0.1f32; frames * 2];
        run[RATE as usize * 2..RATE as usize * 4].fill(0.0);
        let mut db = vec![-20.0f32; 500];
        mute_silent(&mut db, &run, 1000.0);
        assert!(db[..200].iter().all(|v| !v.is_nan()), "錄到之前的、有聲音的不動");
        assert!(db[200..300].iter().all(|v| v.is_nan()));
        assert!(db[300..].iter().all(|v| !v.is_nan()), "有聲音的、錄到之後的不動");
    }

    fn worker(name: &str) -> (Worker, PathBuf, Receiver<NowPlaying>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let dir = std::env::temp_dir().join(format!("vitro-align-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let w = Worker {
            dir: dir.clone(),
            model_path: PathBuf::new(),
            log: dir.join("align.log"),
            shared: Arc::new(Shared::default()),
            refetch: tx,
            model: None,
            retry_at: None,
            song: None,
            parked: Vec::new(),
            last: None,
            stash: None,
        };
        (w, dir, rx)
    }

    /// 設定裡剛打開時補送的歌詞會比這首的第一輪輪詢先到，要留著等它
    #[test]
    fn 歌詞比輪詢先到也接得上() {
        let (mut w, dir, _rx) = worker("stash");
        w.handle(Msg::Lyrics("a".into(), Box::new(Lyrics { source: "plain".into(), ..Default::default() })));
        w.handle(Msg::Poll(Poll { q: 10_000_000, id: "a".into(), pos: 1000, playing: true, dur: 100_000, local: true, sure: true }));
        assert_eq!(w.song.as_ref().unwrap().lyrics.as_ref().map(|l| l.source.as_str()), Some("plain"));
        // 別首的不會被接上
        w.handle(Msg::Lyrics("x".into(), Box::default()));
        w.handle(Msg::Poll(Poll { q: 20_000_000, id: "b".into(), pos: 1000, playing: true, dur: 100_000, local: true, sure: true }));
        assert!(w.song.as_ref().unwrap().lyrics.is_none());
        let _ = std::fs::remove_file(dir.join("align.log"));
    }

    #[test]
    fn 連兩份都配不上就撤回() {
        let (mut w, dir, _rx) = worker("deaf");
        let poll = |pos: i64| Msg::Poll(Poll { q: pos as u64 * 10_000, id: "a".into(), pos, playing: true, dur: 100_000, local: true, sure: true });
        w.handle(poll(1000));
        w.wrong("a", "QQ");
        assert_eq!(entry(&dir, "a").unwrap().bad, ["QQ"]);
        // 重抓回來的那份蓋掉了快取
        let cache = dir.join("a.json");
        std::fs::write(&cache, "{}").unwrap();
        w.wrong("a", "Netease");
        assert!(entry(&dir, "a").unwrap().bad.is_empty(), "這次記下的都撤回");
        assert!(!cache.exists(), "快取丟掉，下次照來源鏈重抓");
        // 換回來的歌詞到了也不再量
        let (l, _) = fake(0);
        w.handle(Msg::Lyrics("a".into(), Box::new(l)));
        assert!(w.song.as_ref().unwrap().done);
        // 之前的播放就判定過的不撤
        let (mut w, dir2, _rx) = worker("deaf2");
        crate::config::write_json(&dir2.join(STORE_FILE), &BTreeMap::from([("a".to_string(), Entry { bad: vec!["KuGou".into()], ..Default::default() })])).unwrap();
        w.handle(poll(1000));
        w.wrong("a", "QQ");
        w.wrong("a", "Netease");
        assert_eq!(entry(&dir2, "a").unwrap().bad, ["KuGou"]);
        for d in [dir, dir2] {
            for n in ["align.log", STORE_FILE] {
                let _ = std::fs::remove_file(d.join(n));
            }
            let _ = std::fs::remove_dir(&d);
        }
    }

    #[test]
    fn 位置不準時不錄() {
        let (mut w, dir, _rx) = worker("unsure");
        let poll = |pos: i64, sure: bool| Msg::Poll(Poll { q: pos as u64 * 10_000, id: "a".into(), pos, playing: true, dur: 100_000, local: true, sure });
        let pkt = |pos: i64| Msg::Audio(pos as u64 * 10_000, vec![0.1; 960]);
        w.handle(poll(0, false));
        for t in (0..500).step_by(10) {
            w.handle(pkt(t));
        }
        w.handle(poll(500, false));
        assert!(w.song.as_ref().unwrap().run.is_empty());
        // 準了以後的第一段也不收（前一輪還不準），再下一段才收
        for t in (500..1000).step_by(10) {
            w.handle(pkt(t));
        }
        w.handle(poll(1000, true));
        assert!(w.song.as_ref().unwrap().run.is_empty());
        for t in (1000..1500).step_by(10) {
            w.handle(pkt(t));
        }
        w.handle(poll(1500, true));
        assert_eq!(w.song.as_ref().unwrap().run.len(), 50 * 960);
        let _ = std::fs::remove_file(dir.join("align.log"));
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn 指紋只看時間() {
        let (mut l, _) = fake(0);
        let a = fingerprint(&l);
        l.synced[0].text = "別的字".into();
        assert_eq!(fingerprint(&l), a);
        l.synced[0].t += 10;
        assert_ne!(fingerprint(&l), a);
    }
}
