//! 歌詞自動對齊的第 0 步：收資料，離線比較對齊方法用。
//!
//! 設了 `VITRO_ALIGN_REC=<資料夾>` 才會動，沒設時每個入口都只是讀一次 `OnceLock` 就回去。
//! 開著時 loopback 一直擷取，存三樣東西：
//!
//! - `audio\<unix 秒>.wav`：loopback 原樣（48 kHz 立體聲，轉 16 位元），滿 20 分鐘換一個檔。
//!   **不照歌切**：播放器知道換歌會晚一輪輪詢，照它切會把下一首的開頭錄進上一首。
//!   旁邊的 `.sync.jsonl` 記每個封包落在第幾個 frame、WASAPI 給的 QPC 時刻與旗標，
//!   沒聲音時 loopback 不給封包，wav 裡沒有那段，要靠它還原時間。
//! - `timeline.jsonl`：播放器每輪輪詢一筆（QPC 時刻、曲目、推算的播放位置），
//!   加上換歌與手動調偏移的事件。
//! - `lyrics\<曲目>.json`：抓到的歌詞整份，連同當時的單曲偏移。
//!
//! 三樣的共同時鐘是 QPC（100 ns 單位）：WASAPI 封包的時刻本來就是這個，
//! 播放器這邊用 `QueryPerformanceCounter` 換成同一個單位。正式功能不讀這些檔。

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::lyrics::Lyrics;
use crate::player::NowPlaying;

/// 一個 wav 最多錄幾個 frame（20 分鐘）
const CHUNK_FRAMES: u64 = 20 * 60 * crate::audio::SAMPLE_RATE as u64;
/// 隔多少 frame 回頭補一次 wav 檔頭的長度（約 5 秒）。程式被砍掉時檔頭最多差這麼多
const PATCH_FRAMES: u64 = 5 * crate::audio::SAMPLE_RATE as u64;

static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
static TIMELINE: Mutex<Option<Timeline>> = Mutex::new(None);

struct Timeline {
    w: BufWriter<File>,
    last_id: String,
}

fn dir() -> Option<&'static PathBuf> {
    DIR.get_or_init(|| {
        let d = PathBuf::from(std::env::var_os("VITRO_ALIGN_REC")?);
        if let Err(e) = std::fs::create_dir_all(d.join("audio")).and_then(|_| std::fs::create_dir_all(d.join("lyrics"))) {
            crate::warn!("對齊資料夾開不了，不錄：{e}");
            return None;
        }
        Some(d)
    })
    .as_ref()
}

/// 有沒有在收資料。`audio::needed` 看它決定 loopback 要不要開
pub fn on() -> bool {
    dir().is_some()
}

/// 現在的 QPC，100 ns 單位（跟 WASAPI 封包時刻同一個鐘）
pub fn qpc_hns() -> u64 {
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    let (mut c, mut f) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
        let _ = QueryPerformanceFrequency(&mut f);
    }
    if f <= 0 {
        return 0;
    }
    (c as i128 * 10_000_000 / f as i128) as u64
}

fn unix_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn line(v: serde_json::Value) {
    let Some(d) = dir() else { return };
    let Ok(mut g) = TIMELINE.lock() else { return };
    if g.is_none() {
        match std::fs::OpenOptions::new().create(true).append(true).open(d.join("timeline.jsonl")) {
            Ok(f) => *g = Some(Timeline { w: BufWriter::new(f), last_id: String::new() }),
            Err(e) => {
                crate::warn!("timeline.jsonl 開不了：{e}");
                return;
            }
        }
    }
    if let Some(t) = g.as_mut() {
        let _ = writeln!(t.w, "{v}");
        let _ = t.w.flush();
    }
}

/// 播放器每輪輪詢呼叫一次。位置照畫面端的算法推到「現在」；`lead` 是 `lead.rs` 這次扣掉的毫秒
/// （`pos` 已經扣過，原始的是 `pos + lead`）
pub fn poll(np: Option<&NowPlaying>, lead: i64) {
    if !on() {
        return;
    }
    let Some(t) = np else { return };
    let q = qpc_hns();
    let changed = TIMELINE.lock().ok().is_some_and(|g| g.as_ref().is_none_or(|tl| tl.last_id != t.id));
    if changed {
        line(json!({
            "q": q, "ev": "track", "id": t.id, "title": t.title, "artist": t.artist,
            "album": t.album, "dur": t.duration_ms, "unix": unix_s(),
        }));
        if let Ok(mut g) = TIMELINE.lock()
            && let Some(tl) = g.as_mut()
        {
            tl.last_id = t.id.clone();
        }
    }
    let pos = t.progress_ms + if t.playing { t.at.elapsed().as_millis() as i64 } else { 0 };
    let st = t.smtc_stamp;
    line(json!({
        "q": q, "id": t.id, "pos": pos, "lead": lead, "playing": t.playing, "src": t.source,
        "sp": st.map(|s| s.0), "su": st.map(|s| s.1), "sn": st.map(|s| s.2),
    }));
}

/// 手動調了偏移（`[` `]` `0`）。`track` 是單曲偏移，`None` 是當時沒歌詞、調的是全域
pub fn offset(track: Option<i64>, global: i64) {
    if on() {
        line(json!({ "q": qpc_hns(), "ev": "offset", "track": track, "global": global }));
    }
}

/// 歌詞抓到了：整份存下來（`offset_ms` 不在序列化裡，另外記）
pub fn lyrics(t: &NowPlaying, l: &Lyrics) {
    let Some(d) = dir() else { return };
    let name: String = t.id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let v = json!({
        "q": qpc_hns(), "id": t.id, "title": t.title, "artist": t.artist, "dur": t.duration_ms,
        "offset_ms": l.offset_ms, "lyrics": l,
    });
    if let Err(e) = std::fs::write(d.join("lyrics").join(format!("{name}.json")), v.to_string()) {
        crate::warn!("對齊資料的歌詞存不了：{e}");
    }
}

struct Chunk {
    wav: BufWriter<File>,
    sync: BufWriter<File>,
    frames: u64,
    patched: u64,
}

impl Chunk {
    fn open() -> Option<Self> {
        let d = dir()?.join("audio");
        // 同一秒內換檔（裝置重開）就往後找一個沒用過的名字
        let mut stem = unix_s();
        while d.join(format!("{stem}.wav")).exists() {
            stem += 1;
        }
        let r = (|| -> std::io::Result<Self> {
            let mut wav = BufWriter::with_capacity(1 << 16, File::create(d.join(format!("{stem}.wav")))?);
            write_header(&mut wav, 0)?;
            let sync = BufWriter::new(File::create(d.join(format!("{stem}.sync.jsonl")))?);
            Ok(Self { wav, sync, frames: 0, patched: 0 })
        })();
        r.map_err(|e| crate::warn!("對齊錄音檔開不了：{e}")).ok()
    }

    /// 把檔頭的長度補成現在寫到的位置，再回到檔尾
    fn patch(&mut self) {
        let _ = self.wav.flush();
        let f = self.wav.get_mut();
        let _ = f.seek(SeekFrom::Start(0)).and_then(|_| write_header(f, self.frames)).and_then(|_| f.seek(SeekFrom::End(0)));
        let _ = self.sync.flush();
        self.patched = self.frames;
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        self.patch();
    }
}

fn write_header(w: &mut impl Write, frames: u64) -> std::io::Result<()> {
    let rate = crate::audio::SAMPLE_RATE as u32;
    let data = (frames * 4).min(u32::MAX as u64 - 36) as u32;
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&2u16.to_le_bytes())?;
    w.write_all(&rate.to_le_bytes())?;
    w.write_all(&(rate * 4).to_le_bytes())?;
    w.write_all(&4u16.to_le_bytes())?;
    w.write_all(&16u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data.to_le_bytes())
}

/// loopback 擷取那條執行緒持有一個。裝置換掉時 `capture_once` 結束、它跟著收檔
#[derive(Default)]
pub struct Rec {
    cur: Option<Chunk>,
}

impl Rec {
    /// 一個封包：`bytes` 是 f32 立體聲交錯，`ts` 是 WASAPI 給的第一個 frame 的 QPC 時刻
    pub fn push(&mut self, bytes: impl Iterator<Item = u8>, ts: u64, dev: u64, silent: bool, discont: bool) {
        if !on() {
            return;
        }
        if self.cur.as_ref().is_some_and(|c| c.frames >= CHUNK_FRAMES) {
            self.cur = None;
        }
        if self.cur.is_none() {
            self.cur = Chunk::open();
        }
        let Some(c) = self.cur.as_mut() else { return };
        let start = c.frames;
        let mut buf = [0u8; 4];
        let mut i = 0;
        let mut n = 0u64;
        for b in bytes {
            buf[i] = b;
            i += 1;
            if i == 4 {
                i = 0;
                // 標成靜音的封包，內容不保證是零
                let v = if silent { 0.0 } else { f32::from_le_bytes(buf) };
                let s = (v.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                let _ = c.wav.write_all(&s.to_le_bytes());
                n += 1;
            }
        }
        let frames = n / 2;
        c.frames += frames;
        let _ = writeln!(
            c.sync,
            "{}",
            json!({ "f": start, "n": frames, "q": ts, "dev": dev, "silent": silent, "discont": discont })
        );
        if c.frames - c.patched >= PATCH_FRAMES {
            c.patch();
        }
    }
}
