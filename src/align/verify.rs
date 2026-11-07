//! 比對歌詞內容：把錄到的一分鐘混音交給 whisper.cpp 聽寫，跟歌詞的字比，抓配錯歌。
//!
//! 能量相關度只看「什麼時候在唱」，節奏像的別首騙得過，節奏特別的對歌反而會被判錯
//! （align.log：判配錯的 140 首有 67 首第二個來源也過不了）。這裡看的是唱什麼。
//!
//! 2026-11-06 用 whisper-cli b5454 small 離線對 35 首驗過（筆記.md「比對歌詞內容」）：
//! 34 首對的通過 33、不確定 1；1122 組配錯沒有一組通過。門檻是看同一批資料定的，
//! 那批沒有日韓文、英文也少。混音跟分離人聲一樣好，所以不用還原波形；base 會把對的判錯，不能用。

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::config;
use crate::lyrics::Line;

/// 錄多長：比這短的分不開（3 句、1 句都有對的歌得 0 分）
pub const CLIP_S: usize = 60;
/// 歌太短或快播完時，至少要這麼長才聽
const CLIP_MIN_S: usize = 30;
const ASR_RATE: usize = 16_000;
/// 辨識到的字只跟這段時間內的歌詞比。不限時間時配錯組最高 0.27，限了 0.06
const WINDOW_MS: f64 = 8000.0;
/// 辨識出的字組比這少就是沒聽出東西（整段幻覺、純音樂）
const MIN_GRAMS: usize = 20;
const PASS_RATIO: f32 = 0.15;
const MIN_HITS: usize = 6;
/// 對的歌最低 0.23，配錯最高 0.06
const FAIL_RATIO: f32 = 0.08;
/// 整段的平均功率低於這個（約 -70 dBFS）是音量 0，聽不出東西，也不是這首歌的資訊
const SILENT_POW: f32 = 1e-7;
const THREADS: &str = "4";
/// 60 秒的聲音正常 8 秒，語言猜錯重跑一次也不到 1 分鐘
const RUN_LIMIT: Duration = Duration::from_secs(180);

const ENGINE_URL: &str = "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip";
const ENGINE_SHA256: &str = "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin";
const MODEL_SHA256: &str = "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b";
const MODEL_FILE: &str = "ggml-small.bin";
const EXE: &str = "whisper-cli.exe";

/// 壓縮檔裡只拿這些，其他工具和 llama.dll 用不到
fn wanted(name: &str) -> bool {
    name == EXE || name == "whisper.dll" || (name.starts_with("ggml") && name.ends_with(".dll"))
}

/// 聽到的一段。時間是歌曲毫秒
#[derive(Debug, Clone, PartialEq)]
pub struct Seg {
    pub t: f64,
    pub e: f64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    /// 聽不出東西，或落在兩個門檻中間：不動作，也不封來源
    Unsure,
    Mismatch,
}

/// 轉繁體後的字詞：英文數字一個詞一個，其他一個字一個
fn tokens(s: &str) -> Vec<String> {
    let s: String = crate::zh::s2tw(s).nfkc().flat_map(char::to_lowercase).collect();
    let mut out = Vec::new();
    let mut word = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            word.push(c);
            continue;
        }
        if !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
        if c.is_alphabetic() {
            out.push(c.to_string());
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

type Gram = (String, String);

/// 辨識到的不重複字組有幾組對得上歌詞：回傳（對上的、全部）。
/// 字組不重複算，幻覺常是同一句重複幾十次；兩個字母一組的話「th」「in」到處都有，所以英文按詞比
pub fn score(heard: &[Seg], lines: &[Line]) -> (usize, usize) {
    let lines: Vec<(f64, HashSet<Gram>)> = lines
        .iter()
        .filter(|l| !l.text.trim().is_empty())
        .map(|l| {
            let t = tokens(&l.text);
            (l.t as f64, t.windows(2).map(|w| (w[0].clone(), w[1].clone())).collect())
        })
        .collect();
    let (mut all, mut hit) = (HashSet::new(), HashSet::new());
    for s in heard {
        let t = tokens(&s.text);
        let dur = (s.e - s.t).max(100.0);
        // whisper 一段常有十幾秒，時間照字數攤
        for (i, w) in t.windows(2).enumerate() {
            let at = s.t + dur * i as f64 / (t.len() - 1) as f64;
            let g = (w[0].clone(), w[1].clone());
            if lines.iter().any(|(lt, set)| (lt - at).abs() <= WINDOW_MS && set.contains(&g)) {
                hit.insert(g.clone());
            }
            all.insert(g);
        }
    }
    (hit.len(), all.len())
}

pub fn judge(hit: usize, all: usize) -> Verdict {
    if all < MIN_GRAMS {
        return Verdict::Unsure;
    }
    let r = hit as f32 / all as f32;
    if r >= PASS_RATIO && hit >= MIN_HITS {
        Verdict::Pass
    } else if r < FAIL_RATIO {
        Verdict::Mismatch
    } else {
        Verdict::Unsure
    }
}

/// whisper 猜的語言不可信時（台語被猜成緬甸語）改用的語言：看歌詞用什麼字
pub fn lang_of(lines: &[Line]) -> &'static str {
    let (mut kana, mut hangul, mut han, mut all) = (0, 0, 0, 0);
    for c in lines.iter().flat_map(|l| l.text.chars()).filter(|c| c.is_alphabetic()) {
        all += 1;
        match c {
            '\u{3040}'..='\u{30ff}' => kana += 1,
            '\u{ac00}'..='\u{d7a3}' => hangul += 1,
            '\u{3400}'..='\u{9fff}' => han += 1,
            _ => {}
        }
    }
    if kana > 0 {
        "ja"
    } else if hangul * 5 > all {
        "ko"
    } else if han * 5 > all {
        "zh"
    } else {
        "en"
    }
}

/// 錄下來要給 whisper 的聲音：16 kHz 單聲道，可以是不連續的幾段（拖過進度條）
#[derive(Default)]
pub struct Clip {
    pcm: Vec<f32>,
    /// 每段在 `pcm` 的起點與對應的歌曲毫秒
    parts: Vec<(usize, f64)>,
    end_ms: f64,
    sum: f32,
    count: u8,
}

impl Clip {
    /// `stereo` 是 48 kHz 交錯立體聲，從歌曲的 `ms` 開始。三點平均降到 16 kHz，離線驗證也是這樣
    pub fn push(&mut self, ms: f64, stereo: &[f32]) {
        if self.full() {
            return;
        }
        // 位置只要準到比對用的幾秒內，封包時間的抖動不算斷開
        if self.parts.is_empty() || (ms - self.end_ms).abs() > 250.0 {
            self.parts.push((self.pcm.len(), ms));
            self.sum = 0.0;
            self.count = 0;
        }
        self.end_ms = ms + (stereo.len() / 2) as f64 / 48.0;
        for f in stereo.chunks_exact(2) {
            self.sum += (f[0] + f[1]) / 2.0;
            self.count += 1;
            if self.count == 3 {
                self.pcm.push(self.sum / 3.0);
                self.sum = 0.0;
                self.count = 0;
            }
        }
        self.pcm.truncate(CLIP_S * ASR_RATE);
    }

    pub fn full(&self) -> bool {
        self.pcm.len() >= CLIP_S * ASR_RATE
    }

    pub fn usable(&self) -> bool {
        self.pcm.len() >= CLIP_MIN_S * ASR_RATE
    }

    pub fn silent(&self) -> bool {
        self.pcm.iter().map(|x| x * x).sum::<f32>() / self.pcm.len().max(1) as f32 <= SILENT_POW
    }

    /// `pcm` 裡第 `ms` 毫秒對應的歌曲時間
    fn song_ms(&self, ms: f64) -> f64 {
        let at = ms * ASR_RATE as f64 / 1000.0;
        let p = self.parts.iter().rev().find(|p| p.0 as f64 <= at).or(self.parts.first());
        p.map_or(ms, |&(i, start)| start + ms - i as f64 * 1000.0 / ASR_RATE as f64)
    }

    /// 響度拉到滿，音量開得小也一樣聽得到
    fn wav(&self) -> Vec<u8> {
        let peak = self.pcm.iter().fold(1e-6f32, |p, x| p.max(x.abs()));
        let len = self.pcm.len() as u32 * 2;
        let mut w = Vec::with_capacity(44 + len as usize);
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + len).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&(ASR_RATE as u32).to_le_bytes());
        w.extend_from_slice(&(ASR_RATE as u32 * 2).to_le_bytes());
        w.extend_from_slice(&2u16.to_le_bytes());
        w.extend_from_slice(&16u16.to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&len.to_le_bytes());
        for x in &self.pcm {
            w.extend_from_slice(&((x / peak * 0.9 * 32767.0) as i16).to_le_bytes());
        }
        w
    }
}

// ---------------------------- whisper-cli ----------------------------

pub fn ready(dir: &Path) -> bool {
    dir.join(EXE).exists() && dir.join(MODEL_FILE).exists()
}

#[derive(Deserialize)]
struct CliOut {
    #[serde(default)]
    result: CliResult,
    #[serde(default)]
    transcription: Vec<CliSeg>,
}

#[derive(Deserialize, Default)]
struct CliResult {
    #[serde(default)]
    language: String,
}

#[derive(Deserialize)]
struct CliSeg {
    offsets: CliOffsets,
    text: String,
}

#[derive(Deserialize)]
struct CliOffsets {
    from: f64,
    to: f64,
}

/// 跑一次 whisper-cli。回傳的時間是 `clip` 裡的毫秒
fn run_cli(dir: &Path, wav: &Path, lang: &str) -> anyhow::Result<CliOut> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
    let base = wav.with_extension("");
    let json = wav.with_extension("json");
    let _ = std::fs::remove_file(&json);
    let mut child = std::process::Command::new(dir.join(EXE))
        .arg("-m")
        .arg(dir.join(MODEL_FILE))
        .arg("-f")
        .arg(wav)
        .args(["-l", lang, "-t", THREADS, "-oj", "-np", "-nt", "-of"])
        .arg(&base)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS)
        .spawn()
        .context("開不起來")?;
    let t = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if t.elapsed() > RUN_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("跑太久");
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    if !status.success() {
        bail!("結束代碼 {:?}", status.code());
    }
    // 中日文可能被切在位元組中間，不是合法的 UTF-8
    let text = String::from_utf8_lossy(&std::fs::read(&json).context("沒有輸出")?).into_owned();
    let _ = std::fs::remove_file(&json);
    serde_json::from_str(&text).context("輸出讀不懂")
}

/// 聽寫。語言自動判斷，判成這幾種以外就用 `lang` 重跑
pub fn listen(dir: &Path, tmp: &Path, clip: &Clip, lang: &str) -> anyhow::Result<Vec<Seg>> {
    std::fs::create_dir_all(tmp)?;
    let wav = tmp.join("asr.wav");
    std::fs::write(&wav, clip.wav())?;
    let got = (|| {
        let mut r = run_cli(dir, &wav, "auto")?;
        if !matches!(r.result.language.as_str(), "zh" | "en" | "ja" | "ko") {
            r = run_cli(dir, &wav, lang)?;
        }
        anyhow::Ok(r)
    })();
    let _ = std::fs::remove_file(&wav);
    Ok(got?
        .transcription
        .into_iter()
        .map(|s| Seg { t: clip.song_ms(s.offsets.from), e: clip.song_ms(s.offsets.to), text: s.text })
        .collect())
}

// ---------------------------- 安裝 ----------------------------

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 邊下載邊算雜湊，500 MB 不進記憶體
fn download(url: &str, sha256: &str, dst: &Path) -> anyhow::Result<()> {
    let http = reqwest::blocking::Client::builder().user_agent(config::user_agent()).timeout(Duration::from_secs(3 * 3600)).build()?;
    let mut r = http.get(url).send()?.error_for_status()?;
    let tmp = dst.with_extension("part");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        f.write_all(&buf[..n])?;
    }
    f.flush()?;
    drop(f);
    if hex(&h.finalize()) != sha256 {
        let _ = std::fs::remove_file(&tmp);
        bail!("下載的檔案不對");
    }
    std::fs::rename(&tmp, dst)?;
    Ok(())
}

/// 從 zip 取出 `wanted` 的檔案（不分資料夾）。項目不多，不值得多一個相依
fn unzip(zip: &[u8], dst: &Path) -> anyhow::Result<usize> {
    let u16le = |i: usize| zip.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32le = |i: usize| zip.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let bad = || anyhow!("zip 壞了");
    let end = (0..zip.len().saturating_sub(21)).rev().find(|&i| zip[i..].starts_with(b"PK\x05\x06")).ok_or_else(bad)?;
    let count = u16le(end + 10).ok_or_else(bad)?;
    let mut p = u32le(end + 16).ok_or_else(bad)?;
    let mut done = 0;
    for _ in 0..count {
        if !zip.get(p..).is_some_and(|z| z.starts_with(b"PK\x01\x02")) {
            return Err(bad());
        }
        let method = u16le(p + 10).ok_or_else(bad)?;
        let size = u32le(p + 20).ok_or_else(bad)?;
        let (n, x, c) = (u16le(p + 28).ok_or_else(bad)?, u16le(p + 30).ok_or_else(bad)?, u16le(p + 32).ok_or_else(bad)?);
        let local = u32le(p + 42).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(zip.get(p + 46..p + 46 + n).ok_or_else(bad)?).replace('\\', "/");
        p += 46 + n + x + c;
        let Some(file) = name.rsplit('/').next().filter(|f| wanted(f)) else { continue };
        let start = local + 30 + u16le(local + 26).ok_or_else(bad)? + u16le(local + 28).ok_or_else(bad)?;
        let raw = zip.get(start..start + size).ok_or_else(bad)?;
        let mut f = std::fs::File::create(dst.join(file))?;
        match method {
            0 => f.write_all(raw)?,
            8 => {
                std::io::copy(&mut flate2::read::DeflateDecoder::new(raw), &mut f)?;
            }
            m => bail!("zip 的壓縮法 {m}"),
        }
        done += 1;
    }
    Ok(done)
}

/// 下載引擎與模型。exe 不帶這些：只有打開這個功能的人才用得到，也不用把 C++ 編進來
pub fn install(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    if !dir.join(EXE).exists() {
        let zip = dir.join("engine.zip");
        download(ENGINE_URL, ENGINE_SHA256, &zip)?;
        let got = unzip(&std::fs::read(&zip)?, dir);
        let _ = std::fs::remove_file(&zip);
        got?;
        if !dir.join(EXE).exists() {
            bail!("壓縮檔裡沒有 {EXE}");
        }
    }
    let model: PathBuf = dir.join(MODEL_FILE);
    if !model.exists() {
        download(MODEL_URL, MODEL_SHA256, &model)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::Lyrics;

    fn line(t: i64, text: &str) -> Line {
        Line { t, text: text.into(), ..Default::default() }
    }

    #[test]
    fn 簡繁全半形大小寫都當同一個字() {
        assert_eq!(tokens("你說ＡＢＣ don't"), tokens("你说 abc Don't"));
        assert_eq!(tokens("I love you"), ["i", "love", "you"]);
    }

    #[test]
    fn 重複的幻覺只算一次() {
        let l = [line(1000, "天亮了")];
        let s = Seg { t: 1000.0, e: 2000.0, text: "天亮了天亮了天亮了天亮了".into() };
        assert_eq!(score(&[s], &l), (2, 3));
    }

    #[test]
    fn 時間差太多的字不算對上() {
        let l = [line(60_000, "我想你")];
        let s = |t| Seg { t, e: t + 1000.0, text: "我想你".into() };
        assert_eq!(score(&[s(60_000.0)], &l).0, 2);
        assert_eq!(score(&[s(30_000.0)], &l).0, 0);
    }

    #[test]
    fn 三種結果() {
        assert_eq!(judge(40, 100), Verdict::Pass);
        assert_eq!(judge(2, 100), Verdict::Mismatch);
        assert_eq!(judge(10, 100), Verdict::Unsure, "夾在中間");
        assert_eq!(judge(0, 19), Verdict::Unsure, "聽不出東西不算配錯");
        assert_eq!(judge(5, 25), Verdict::Unsure, "比例夠但命中太少");
    }

    #[test]
    fn 語言看歌詞用的字() {
        assert_eq!(lang_of(&[line(0, "夜に駆ける")]), "ja");
        assert_eq!(lang_of(&[line(0, "時間走了")]), "zh");
        assert_eq!(lang_of(&[line(0, "사랑해요")]), "ko");
        assert_eq!(lang_of(&[line(0, "you and me")]), "en");
    }

    #[test]
    fn 片段記得每段對應的歌曲時間() {
        let mut c = Clip::default();
        c.push(10_000.0, &vec![0.5; 96_000]); // 1 秒
        c.push(40_000.0, &vec![0.5; 96_000]);
        assert_eq!(c.pcm.len(), 32_000);
        assert_eq!(c.song_ms(500.0), 10_500.0);
        assert_eq!(c.song_ms(1500.0), 40_500.0);
        assert!(!c.silent());
        assert!(Clip { pcm: vec![0.0; 100], ..Default::default() }.silent());
    }

    #[test]
    fn 連續的封包接成一段而且滿了就停() {
        let mut c = Clip::default();
        for i in 0..7000 {
            c.push(i as f64 * 10.0, &[0.1; 960]);
        }
        assert_eq!(c.parts.len(), 1);
        assert!(c.full());
        assert_eq!(c.pcm.len(), CLIP_S * ASR_RATE);
    }

    #[test]
    fn wav_標頭與長度對得上() {
        let c = Clip { pcm: vec![0.5, -0.5], ..Default::default() };
        let w = c.wav();
        assert_eq!(w.len(), 48);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(i16::from_le_bytes([w[44], w[45]]), (0.9 * 32767.0) as i16);
    }

    #[test]
    fn zip_只取要的檔案() {
        // 只有存放（方法 0）的項目，壓縮的那條路徑由 install 實際下載時走到
        let body = b"hello";
        let name = b"Release/whisper-cli.exe";
        let skip = b"Release/whisper-bench.exe";
        let mut z = Vec::new();
        let entry = |z: &mut Vec<u8>, n: &[u8]| {
            let at = z.len() as u32;
            z.extend_from_slice(b"PK\x03\x04");
            z.extend_from_slice(&[0; 22]);
            z.extend_from_slice(&(n.len() as u16).to_le_bytes());
            z.extend_from_slice(&0u16.to_le_bytes());
            z.extend_from_slice(n);
            z.extend_from_slice(body);
            at
        };
        let (a, b) = (entry(&mut z, skip), entry(&mut z, name));
        let cd = z.len() as u32;
        for (n, at) in [(&skip[..], a), (&name[..], b)] {
            z.extend_from_slice(b"PK\x01\x02");
            z.extend_from_slice(&[0; 16]);
            z.extend_from_slice(&(body.len() as u32).to_le_bytes());
            z.extend_from_slice(&(body.len() as u32).to_le_bytes());
            z.extend_from_slice(&(n.len() as u16).to_le_bytes());
            z.extend_from_slice(&[0; 12]);
            z.extend_from_slice(&at.to_le_bytes());
            z.extend_from_slice(n);
        }
        let size = z.len() as u32 - cd;
        z.extend_from_slice(b"PK\x05\x06");
        z.extend_from_slice(&[0; 4]);
        z.extend_from_slice(&2u16.to_le_bytes());
        z.extend_from_slice(&2u16.to_le_bytes());
        z.extend_from_slice(&size.to_le_bytes());
        z.extend_from_slice(&cd.to_le_bytes());
        z.extend_from_slice(&[0; 2]);
        let dir = std::env::temp_dir().join(format!("vitro-unzip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unzip(&z, &dir).unwrap(), 1);
        assert_eq!(std::fs::read(dir.join(EXE)).unwrap(), body);
        assert!(!dir.join("whisper-bench.exe").exists());
        let _ = std::fs::remove_file(dir.join(EXE));
        let _ = std::fs::remove_dir(&dir);
    }

    /// 真的下載、解壓、跑 whisper-cli 一次，走 app 的整條路（48 kHz 封包 → `Clip` → `listen` → `score`）。
    /// `VITRO_ASR_INSTALL=<models\whisper> VITRO_ASR_TRACK=<tracks\id>（不含副檔名）
    /// cargo test --release 真的聽 -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn 真的聽一次() {
        let (Some(dir), Some(base)) = (std::env::var_os("VITRO_ASR_INSTALL"), std::env::var_os("VITRO_ASR_TRACK")) else { return };
        let (dir, base) = (PathBuf::from(dir), PathBuf::from(base));
        install(&dir).unwrap();
        assert!(ready(&dir));
        let m: serde_json::Value = serde_json::from_slice(&std::fs::read(base.with_extension("json")).unwrap()).unwrap();
        let l: Lyrics = serde_json::from_value(m["lyrics"]["lyrics"].clone()).unwrap();
        let raw = std::fs::read(base.with_extension("wav")).unwrap();
        let pcm: Vec<f32> = raw[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
        let first = l.synced[0].t;
        let mut clip = Clip::default();
        let mut ms = first;
        while !clip.full() {
            let s = &pcm[ms as usize * 96..(ms as usize + 10) * 96];
            clip.push(ms as f64, s);
            ms += 10;
        }
        let tmp = std::env::temp_dir().join(format!("vitro-asr-{}", std::process::id()));
        let t = Instant::now();
        let heard = listen(&dir, &tmp, &clip, lang_of(&l.synced)).unwrap();
        let _ = std::fs::remove_dir(&tmp);
        eprintln!("{:.1} 秒聽完，{} 段：{:?}", t.elapsed().as_secs_f32(), heard.len(), heard.first());
        let (h, n) = score(&heard, &l.synced);
        eprintln!("自己的歌詞 {h}/{n}");
        assert_eq!(judge(h, n), Verdict::Pass);
        let other = [Line { t: first, text: "完全不一樣的一段話跟這首歌沒有任何關係".into(), ..Default::default() }];
        let (h, n) = score(&heard, &other);
        eprintln!("別的歌詞 {h}/{n}");
        assert_eq!(judge(h, n), Verdict::Mismatch);
    }

    /// 跟離線結果對答案：`VITRO_ASR_DIR=<asr_cpp 資料夾> VITRO_ASR_TRACKS=<tracks 資料夾>
    /// cargo test --release 跟離線 -- --ignored --nocapture`。要是 33 通過、1 不確定、1122 組沒有一組通過
    #[test]
    #[ignore]
    fn 跟離線結果一致() {
        let (Some(asr), Some(tracks)) = (std::env::var_os("VITRO_ASR_DIR"), std::env::var_os("VITRO_ASR_TRACKS")) else { return };
        let (asr, tracks) = (PathBuf::from(asr), PathBuf::from(tracks));
        let seg = |name: &str, id: &str| -> (Vec<Seg>, String) {
            let v: serde_json::Value = serde_json::from_slice(&std::fs::read(asr.join(name).join(format!("{id}.json"))).unwrap()).unwrap();
            let s = v["seg"].as_array().unwrap().iter().map(|x| Seg { t: x["t"].as_f64().unwrap() * 1000.0, e: x["e"].as_f64().unwrap() * 1000.0, text: x["text"].as_str().unwrap().into() });
            (s.collect(), v["lang"].as_str().unwrap_or("").to_string())
        };
        struct T {
            id: String,
            title: String,
            lines: Vec<Line>,
            heard: Vec<Seg>,
        }
        let root = |t: &str| t.split(" - ").next().unwrap().split(" (").next().unwrap().trim().to_string();
        let mut all = Vec::new();
        for f in std::fs::read_dir(asr.join("small_mix")).unwrap() {
            let id = f.unwrap().path().file_stem().unwrap().to_string_lossy().into_owned();
            let m: serde_json::Value = serde_json::from_slice(&std::fs::read(tracks.join(format!("{id}.json"))).unwrap()).unwrap();
            let l: Lyrics = serde_json::from_value(m["lyrics"]["lyrics"].clone()).unwrap();
            let (mut heard, lang) = seg("small_mix", &id);
            if !matches!(lang.as_str(), "zh" | "en" | "ja" | "ko") {
                heard = seg("small_mix_zh", &id).0;
            }
            all.push(T { id, title: m["title"].as_str().unwrap().into(), lines: l.synced, heard });
        }
        const WRONG: &str = "k4629c1d6c6bcb40f4889";
        let (mut right, mut wrong) = ([0usize; 3], [0usize; 3]);
        let slot = |v| match v {
            Verdict::Pass => 0,
            Verdict::Unsure => 1,
            Verdict::Mismatch => 2,
        };
        for a in all.iter().filter(|a| a.id != WRONG) {
            let (h, n) = score(&a.heard, &a.lines);
            right[slot(judge(h, n))] += 1;
            for b in all.iter().filter(|b| b.id != a.id && b.id != WRONG && root(&b.title) != root(&a.title)) {
                let (h, n) = score(&a.heard, &b.lines);
                wrong[slot(judge(h, n))] += 1;
            }
        }
        let w = all.iter().find(|a| a.id == WRONG).unwrap();
        let (h, n) = score(&w.heard, &w.lines);
        eprintln!("對的歌 {right:?}，配錯 {wrong:?}，黎明 {h}/{n}");
        assert_eq!(right, [33, 1, 0]);
        assert_eq!(wrong[0], 0);
        assert_eq!(judge(h, n), Verdict::Mismatch);
    }
}
