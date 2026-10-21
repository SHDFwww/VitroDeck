//! 歌詞。
//!
//! 來源優先序：本機自訂 → 自架 LyricsPlus（逐字）→ 網易雲（行級＋翻譯）→ LRCLIB（行級）→ 無。
//! 任何一層出狀況都往下掉，不會讓整個歌詞功能掛掉。

mod kugou;
mod matching;
mod netease;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{self, Config};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Word {
    pub t: i64,
    pub d: i64,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Line {
    pub t: i64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<Word>>,
    /// 這句的翻譯（網易雲的 tlyric）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trans: Option<String>,
    /// 這句的羅馬拼音／假名讀音（網易雲的 romalrc）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub romaji: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Lyrics {
    pub synced: Vec<Line>,
    pub plain: String,
    /// word / synced / plain / instrumental / none / error
    pub source: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 這首歌專屬的時間微調（毫秒）。**不存在歌詞快取裡** —— 重抓或手動換歌詞時
    /// 快取會被整份覆寫，微調不該跟著消失；它存在 `offsets.json`，抓完才填進來。
    #[serde(skip)]
    pub offset_ms: i64,
    /// 自動對齊量出來的單曲偏移（`align.json`）。手動的 `offset_ms` 不是 0 時不用
    #[serde(skip)]
    pub auto_ms: i64,
    /// 產生這份快取的來源鏈版本。比現在的 `CHAIN_VERSION` 舊的，換到這首時會重抓一次
    /// （新加了來源，舊的行級結果值得試著升級），重抓過就不再問。
    pub chain: u8,
}

/// 來源鏈每加一種來源就 +1。0 是 Python 版與更早的 Rust 版寫的。
/// 2：LyricsPlus 的結果開始核對是不是同一首（`unverified`）
const CHAIN_VERSION: u8 = 2;

/// 2 之前 LyricsPlus 給的逐字沒核對過是不是同一首（曾把美秀集團〈小老婆〉配成〈卷菸〉），
/// 要重驗一次，重抓空手也不能沿用。酷狗一直有走 `matching::pick`，不用重驗
fn unverified(c: &Lyrics) -> bool {
    c.source == "word" && c.provider != "KuGou" && c.chain < 2
}

/// 比對歌詞要用的總偏移 = 全域設定 + 這首的偏移（手動優先，沒有才用自動對齊的）。
pub fn shift(cfg: &Config, l: Option<&Lyrics>) -> i64 {
    cfg.lyrics_offset_ms + l.map_or(0, Lyrics::track_shift)
}

const OFFSETS_FILE: &str = "offsets.json";

fn read_offsets(dir: &Path) -> std::collections::BTreeMap<String, i64> {
    config::read_json(&dir.join(OFFSETS_FILE))
}

/// 記下（或清掉）某首歌的專屬偏移。0 代表清除，免得檔案越長越大。
pub fn set_track_offset(dir: &Path, key: &str, ms: i64) {
    let mut m = read_offsets(dir);
    if ms == 0 {
        m.remove(key);
    } else {
        m.insert(key.to_string(), ms);
    }
    let _ = config::write_json(&dir.join(OFFSETS_FILE), &m);
}

fn cache_file(dir: &Path, key: &str) -> PathBuf {
    let safe: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    dir.join(format!("{safe}.json"))
}

/// 丟掉這首的歌詞快取，下次換到它從頭照來源鏈抓。
/// 自動對齊撤回誤判時用：誤判那時重抓的結果已經蓋掉原本那份
pub fn forget(dir: &Path, key: &str) {
    let _ = std::fs::remove_file(cache_file(dir, key));
}

/// 檔名不能有的字元換成底線
fn safe_name(s: &str) -> String {
    s.chars().map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c }).collect()
}

impl Lyrics {
    fn has_content(&self) -> bool {
        !self.synced.is_empty() || !self.plain.is_empty() || self.source == "instrumental"
    }

    /// 這首的偏移：手動微調優先，沒有才用自動對齊的
    pub fn track_shift(&self) -> i64 {
        if self.offset_ms != 0 { self.offset_ms } else { self.auto_ms }
    }

    /// 有沒有逐字時間（只要有一句有就算）
    pub fn has_words(&self) -> bool {
        self.synced.iter().any(|l| l.words.as_ref().is_some_and(|w| !w.is_empty()))
    }
}

// ---------------------------- LRC 解析 ----------------------------

/// `[mm:ss.xx]` 或 `[mm:ss:xx]`，一行可以有好幾個（同一句配多個時間點）。
fn find_stamps(raw: &str) -> Vec<(usize, usize, i64)> {
    let b = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'[' {
            i += 1;
            continue;
        }
        let start = i;
        let Some(close) = raw[i..].find(']').map(|p| i + p) else { break };
        let inner = &raw[i + 1..close];
        if let Some(ms) = parse_stamp(inner) {
            out.push((start, close + 1, ms));
        }
        i = close + 1;
    }
    out
}

fn parse_stamp(s: &str) -> Option<i64> {
    let (mm, rest) = s.split_once(':')?;
    let mm: i64 = mm.trim().parse().ok()?;
    let (ss, frac) = match rest.split_once(['.', ':']) {
        Some((a, b)) => (a, b),
        None => (rest, "0"),
    };
    let ss: i64 = ss.trim().parse().ok()?;
    let frac: String = frac.chars().filter(|c| c.is_ascii_digit()).collect();
    if frac.is_empty() {
        return Some(mm * 60_000 + ss * 1000);
    }
    // 兩位數是百分秒、三位數是毫秒
    let frac_ms: i64 = if frac.len() >= 3 {
        frac[..3].parse().ok()?
    } else {
        frac.parse::<i64>().ok()? * 10
    };
    Some(mm * 60_000 + ss * 1000 + frac_ms)
}

/// Enhanced LRC（A2）會在句中插入 `<mm:ss.xx>` 逐字標記。LRCLIB 抽樣看到的
/// 都是行級，但它是社群投稿的，難保沒有；不濾掉會把標記當歌詞顯示出來。
fn strip_word_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

pub fn parse_lrc(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let stamps = find_stamps(raw);
        if stamps.is_empty() {
            continue;
        }
        let body_start = stamps.last().unwrap().1;
        let body = strip_word_tags(&raw[body_start..]).trim().to_string();
        for (_, _, t) in &stamps {
            lines.push(Line { t: *t, text: body.clone(), ..Default::default() });
        }
    }
    lines.sort_by_key(|l| l.t);
    lines
}

// ---------------------------- 簡繁轉換 ----------------------------

/// 為什麼是 s2tw 見 [`crate::zh::s2tw`]
fn to_traditional(text: &str) -> String {
    crate::zh::s2tw(text)
}

/// 給 `--probe` 用，驗這張表是不是真的 1:1。
pub fn to_traditional_probe(text: &str) -> String {
    to_traditional(text)
}

/// 把整份歌詞轉成繁體。一句一句判斷，本來就是繁體的句子不動（見 [`crate::zh::to_trad`]）。
///
/// 逐字的部分不要一個字一個字轉 —— 字典裡有同長度的詞條，有上下文才轉得對
/// （「荡秋千」整串轉是「盪鞦韆」，拆開逐字轉只會得到「蕩秋千」）。
/// 所以先把整串轉好，再依原本各字的長度切回去；`to_trad` 保證字數不變，切得回來。
fn convert_result(mut r: Lyrics, cfg: &Config) -> Lyrics {
    if !cfg.lyrics_traditional {
        return r;
    }
    let conv = |s: &mut String| {
        if let Some(t) = crate::zh::to_trad(s) {
            *s = t;
        }
    };
    for ln in r.synced.iter_mut() {
        conv(&mut ln.text);
        if let Some(t) = ln.trans.as_mut() {
            conv(t);
        }
        let Some(words) = ln.words.as_mut() else { continue };
        let joined: String = words.iter().map(|w| w.text.as_str()).collect();
        let Some(t) = crate::zh::to_trad(&joined) else { continue };
        let chars: Vec<char> = t.chars().collect();
        let mut pos = 0usize;
        for w in words.iter_mut() {
            let n = w.text.chars().count();
            w.text = chars[pos..pos + n].iter().collect();
            pos += n;
        }
    }
    conv(&mut r.plain);
    r
}

// ---------------------------- 抓取 ----------------------------

/// 把 `src`（網易雲）的翻譯依時間掛到 `dst`（逐字來源）上。兩邊的分句不一定一樣，
/// 掛得上的不到六成就整份不要 —— 寧可沒有翻譯，也不要一句配到別句。
fn merge_trans(dst: &mut Lyrics, src: &Lyrics) {
    let side: Vec<Line> = src
        .synced
        .iter()
        .filter_map(|l| l.trans.as_ref().map(|t| Line { t: l.t, text: t.clone(), ..Default::default() }))
        .collect();
    if side.is_empty() {
        return;
    }
    let mut lines = dst.synced.clone();
    netease::attach(&mut lines, side.clone(), |l, s| l.trans = Some(s));
    let matched = lines.iter().filter(|l| l.trans.is_some()).count();
    if matched * 10 >= side.len() * 6 {
        dst.synced = lines;
    }
}

/// LyricsPlus 照曲名搜到的那一首，是不是我們要的這首。它搜不到原曲時會回同歌手的別首
/// （〈小老婆〉回了〈卷菸〉），所以要拿回應的 `metadata` 再核對一次。
/// 回應沒附曲名就無從核對，照舊收下
fn lyricsplus_same_song(meta: &serde_json::Value, artist: &str, title: &str, duration_ms: i64) -> bool {
    let got = meta["title"].as_str().unwrap_or("").trim();
    if got.is_empty() {
        return true;
    }
    let c = matching::Candidate {
        id: String::new(),
        title: got.to_string(),
        artist: meta["artist"].as_str().unwrap_or("").to_string(),
        album: meta["album"].as_str().unwrap_or("").to_string(),
        duration_ms: meta["totalDuration"].as_str().and_then(parse_clock).unwrap_or(0),
    };
    matching::plausible(&c, artist, title, duration_ms)
}

/// `3:52.000`、`232.5` 這類長度轉毫秒
fn parse_clock(s: &str) -> Option<i64> {
    let s = s.trim();
    let secs = match s.split_once(':') {
        Some((m, rest)) => m.trim().parse::<f64>().ok()? * 60.0 + rest.trim().parse::<f64>().ok()?,
        None => s.parse::<f64>().ok()?,
    };
    (secs > 0.0).then(|| (secs * 1000.0).round() as i64)
}

pub struct Fetcher {
    pub(crate) http: reqwest::blocking::Client,
    dir: PathBuf,
    /// 這次執行期間問過但沒有的曲目。**不寫進磁碟** —— 伺服器暫時沒開
    /// 就被永久記成「沒有逐字」的話，之後永遠不會再試。
    misses: HashSet<String>,
}

impl Fetcher {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            http: reqwest::blocking::Client::builder()
                .user_agent(config::user_agent())
                .timeout(std::time::Duration::from_secs(12))
                .build()
                .expect("reqwest client 建不起來"),
            dir,
            misses: HashSet::new(),
        }
    }

    fn cache_path(&self, key: &str) -> PathBuf {
        cache_file(&self.dir, key)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn fetch(
        &mut self,
        cfg: &Config,
        key: &str,
        artist: &str,
        title: &str,
        album: &str,
        duration_ms: i64,
        platform_id: Option<&str>,
    ) -> Lyrics {
        // 自動對齊判斷為配錯歌的來源，這首不再用它們（設定關掉時不管）
        let bad = crate::align::entry(&self.dir, key).filter(|_| crate::align::enabled()).map(|e| e.bad).unwrap_or_default();
        let mut got = match self.local(cfg, key, artist, title) {
            Some(l) => l,
            None => self.fetch_cached(cfg, key, artist, title, album, duration_ms, platform_id, &bad),
        };
        got.offset_ms = read_offsets(&self.dir).get(key).copied().unwrap_or(0);
        got.auto_ms = crate::align::auto_shift(&self.dir, key, &got);
        got
    }

    /// 本機自訂歌詞：`<歌詞快取>/custom/<key>.lrc` 或 `<歌手> - <曲名>.lrc`。
    /// 優先於快取與所有線上來源，每次換歌都檢查（檔案放進去馬上生效）。
    fn local(&self, cfg: &Config, key: &str, artist: &str, title: &str) -> Option<Lyrics> {
        let dir = self.dir.join("custom");
        let by_name = format!("{} - {}.lrc", safe_name(artist), safe_name(title));
        let by_key = format!("{}.lrc", safe_name(key));
        let text = [by_key, by_name].iter().find_map(|n| {
            let bytes = std::fs::read(dir.join(n)).ok()?;
            Some(String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}').to_string())
        })?;
        let synced = parse_lrc(&text);
        let r = if synced.is_empty() {
            if text.trim().is_empty() {
                return None;
            }
            Lyrics { plain: text, source: "plain".into(), provider: "本機".into(), ..Default::default() }
        } else {
            Lyrics { synced, source: "synced".into(), provider: "本機".into(), ..Default::default() }
        };
        Some(convert_result(r, cfg))
    }

    #[allow(clippy::too_many_arguments)]
    fn fetch_cached(
        &mut self,
        cfg: &Config,
        key: &str,
        artist: &str,
        title: &str,
        album: &str,
        duration_ms: i64,
        platform_id: Option<&str>,
        bad: &[String],
    ) -> Lyrics {
        let path = self.cache_path(key);
        let cached: Option<Lyrics> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            // 配錯歌的那份不能再拿出來用
            .filter(|c: &Lyrics| !bad.contains(&c.provider));

        if let Some(ref c) = cached
            && !self.stale(cfg, key, c) {
                return c.clone();
            }

        let fresh = self
            .fetch_fresh(cfg, artist, title, album, duration_ms, platform_id, bad)
            .map(|mut f| {
                f.chain = CHAIN_VERSION;
                f
            });

        // 重抓失敗、或抓到的比手上這份還空，就沿用舊的，並記著這次別再試。
        match (fresh, cached) {
            (Some(f), Some(mut c)) if !f.has_content() && c.has_content() && !unverified(&c) => {
                self.misses.insert(key.to_string());
                // 問過了、沒有更好的：蓋上版本章，下次就不再問
                c.chain = CHAIN_VERSION;
                let _ = config::write_json(&path, &c);
                c
            }
            (Some(f), _) => {
                let _ = config::write_json(&path, &f);
                f
            }
            (None, Some(c)) => {
                self.misses.insert(key.to_string());
                c
            }
            (None, None) => Lyrics {
                source: "error".into(),
                error: Some("lyrics fetch failed".into()),
                ..Default::default()
            },
        }
    }

    /// 這份快取值不值得再抓一次。
    fn stale(&self, cfg: &Config, key: &str, cached: &Lyrics) -> bool {
        if self.misses.contains(key) {
            return false; // 這次執行期間已經試過了
        }
        if cached.source == "word" && !unverified(cached) {
            return false;
        }
        // 存的是行級，而現在有 LyricsPlus 可以問，值得升級成逐字；
        // 或是這份快取比現在的來源鏈舊（之後才加了酷狗、網易雲），升級一次
        !cfg.lyricsplus_url.trim().is_empty() || cached.chain < CHAIN_VERSION
    }

    #[allow(clippy::too_many_arguments)]
    fn fetch_fresh(
        &self,
        cfg: &Config,
        artist: &str,
        title: &str,
        album: &str,
        duration_ms: i64,
        platform_id: Option<&str>,
        bad: &[String],
    ) -> Option<Lyrics> {
        let skip = |p: &str| bad.iter().any(|b| b == p);
        // 逐字來源：自架 LyricsPlus、酷狗。命中後若沒有翻譯，再向網易雲借。
        let mut degraded = false;
        let mut word = self.lyricsplus(cfg, artist, title, album, duration_ms, platform_id).filter(|l| !skip(&l.provider));
        if word.is_none() && cfg.lyrics_kugou && !skip("KuGou") {
            match kugou::fetch(&self.http, artist, title, duration_ms) {
                matching::Outcome::Hit(l) => word = Some(l),
                matching::Outcome::Miss => {}
                matching::Outcome::Failed => degraded = true,
            }
        }
        if let Some(mut l) = word {
            if cfg.lyrics_netease && !l.synced.iter().any(|x| x.trans.is_some())
                && let matching::Outcome::Hit(n) = netease::fetch(&self.http, artist, title, duration_ms) {
                    merge_trans(&mut l, &n);
                }
            return Some(convert_result(l, cfg));
        }

        // 行級來源。連不上時不能把「沒有」當成定論：後面的 LRCLIB 若也空手，
        // 就不寫快取，下次換到這首還會再問一次。
        if cfg.lyrics_netease && !skip("Netease") {
            match netease::fetch(&self.http, artist, title, duration_ms) {
                matching::Outcome::Hit(l) => return Some(convert_result(l, cfg)),
                matching::Outcome::Miss => {}
                matching::Outcome::Failed => degraded = true,
            }
        }
        if skip("LRCLIB") {
            return Some(Lyrics { source: "none".into(), ..Default::default() });
        }
        let r = self.lrclib(artist, title, album, duration_ms).map(|r| convert_result(r, cfg));
        match r {
            Some(l) if degraded && !l.has_content() => None,
            other => other,
        }
    }

    /// 來源一：自架的 LyricsPlus（逐字）。沒設 `lyricsplus_url` 就跳過。
    fn lyricsplus(
        &self,
        cfg: &Config,
        artist: &str,
        title: &str,
        album: &str,
        duration_ms: i64,
        platform_id: Option<&str>,
    ) -> Option<Lyrics> {
        let base = cfg.lyricsplus_url.trim().trim_end_matches('/');
        if base.is_empty() {
            return None;
        }
        let dur = format!("{:.2}", duration_ms as f64 / 1000.0);
        let sources = cfg.lyricsplus_sources.trim();

        let attempts: Vec<Vec<(&str, &str)>> = vec![
            platform_id.map(|p| vec![("platformId", p)]).unwrap_or_default(),
            vec![("title", title), ("artist", artist), ("album", album), ("duration", &dur)],
            vec![("title", title), ("artist", artist)],
        ];

        for (n, attempt) in attempts.into_iter().enumerate() {
            // 第一次是拿平台 ID 查，查到的就是這首；後兩次是伺服器照曲名搜的，可能搜到同歌手的別首
            let by_id = n == 0;
            let mut params: Vec<(&str, &str)> =
                attempt.into_iter().filter(|(_, v)| !v.is_empty()).collect();
            if params.is_empty() {
                continue;
            }
            if !sources.is_empty() {
                params.push(("source", sources));
            }
            let url = format!("{base}/v2/lyrics/get");
            // 伺服器連不上就整個放棄，交給 LRCLIB。
            let mut req = self.http.get(&url).query(&params);
            for (k, v) in &cfg.lyricsplus_headers {
                req = req.header(k.as_str(), v.as_str());
            }
            let resp = req.send().ok()?;
            if !resp.status().is_success() {
                continue;
            }
            let Ok(data) = resp.json::<serde_json::Value>() else { continue };
            let Some(raw_lines) = data["lyrics"].as_array() else { continue };
            if raw_lines.is_empty() {
                continue;
            }
            let is_word = data["type"].as_str() == Some("Word");
            if !by_id && !lyricsplus_same_song(&data["metadata"], artist, title, duration_ms) {
                continue;
            }

            let mut out = Vec::new();
            for ln in raw_lines {
                let mut words = Vec::new();
                if let Some(syl) = ln["syllabus"].as_array() {
                    for sy in syl {
                        let wt = sy["text"].as_str().unwrap_or("");
                        if wt.is_empty() {
                            continue;
                        }
                        words.push(Word {
                            t: sy["time"].as_i64().unwrap_or(0),
                            d: sy["duration"].as_i64().unwrap_or(0),
                            text: wt.to_string(),
                        });
                    }
                }
                let mut text = ln["text"].as_str().unwrap_or("").trim().to_string();
                if text.is_empty() && !words.is_empty() {
                    text = words.iter().map(|w| w.text.as_str()).collect::<String>();
                    text = text.trim().to_string();
                }
                if text.is_empty() && words.is_empty() {
                    continue;
                }
                out.push(Line {
                    t: ln["time"].as_i64().unwrap_or(0),
                    text,
                    words: if is_word && !words.is_empty() { Some(words) } else { None },
                    ..Default::default()
                });
            }
            if out.is_empty() {
                continue;
            }
            out.sort_by_key(|l| l.t);
            let has_words = out.iter().any(|l| l.words.is_some());
            // LyricsPlus 會說是哪個站給的（QQ Music / Apple Music / …），
            // 拿來顯示在狀態列，使用者才知道逐字是從哪來的。
            let prov = data["metadata"]["source"].as_str().unwrap_or("").trim().to_string();
            return Some(Lyrics {
                synced: out,
                plain: String::new(),
                source: if has_words { "word".into() } else { "synced".into() },
                provider: if prov.is_empty() { "LyricsPlus".into() } else { prov },
                error: None,
                ..Default::default()
            });
        }
        None
    }

    /// 來源二：LRCLIB（行級）。三段式，一段比一段寬鬆。
    fn lrclib(&self, artist: &str, title: &str, album: &str, duration_ms: i64) -> Option<Lyrics> {
        let dur = ((duration_ms as f64) / 1000.0).round() as i64;
        let dur_s = dur.to_string();

        let take = |payload: &serde_json::Value| -> Option<Lyrics> {
            if payload["instrumental"].as_bool() == Some(true) {
                return Some(Lyrics {
                    source: "instrumental".into(),
                    provider: "LRCLIB".into(),
                    ..Default::default()
                });
            }
            let synced = parse_lrc(payload["syncedLyrics"].as_str().unwrap_or(""));
            let plain = payload["plainLyrics"].as_str().unwrap_or("").to_string();
            if !synced.is_empty() {
                return Some(Lyrics {
                    synced,
                    plain,
                    source: "synced".into(),
                    provider: "LRCLIB".into(),
                    error: None,
                    ..Default::default()
                });
            }
            if !plain.is_empty() {
                return Some(Lyrics {
                    synced: Vec::new(),
                    plain,
                    source: "plain".into(),
                    provider: "LRCLIB".into(),
                    error: None,
                    ..Default::default()
                });
            }
            None
        };

        // 1) 精準比對（帶專輯與長度）
        let r = self
            .http
            .get("https://lrclib.net/api/get")
            .query(&[
                ("artist_name", artist),
                ("track_name", title),
                ("album_name", album),
                ("duration", &dur_s),
            ])
            .send()
            .ok()?;
        if r.status().is_success()
            && let Ok(v) = r.json::<serde_json::Value>()
                && let Some(l) = take(&v) {
                    return Some(l);
                }

        // 2) 放寬：不帶專輯
        if let Ok(r) = self
            .http
            .get("https://lrclib.net/api/get")
            .query(&[("artist_name", artist), ("track_name", title), ("duration", &dur_s)])
            .send()
            && r.status().is_success()
                && let Ok(v) = r.json::<serde_json::Value>()
                    && let Some(l) = take(&v) {
                        return Some(l);
                    }

        // 3) 模糊搜尋，挑有同步歌詞且長度最接近的
        if let Ok(r) = self
            .http
            .get("https://lrclib.net/api/search")
            .query(&[("track_name", title), ("artist_name", artist)])
            .send()
            && r.status().is_success()
                && let Ok(v) = r.json::<serde_json::Value>() {
                    let mut cands = v.as_array().cloned().unwrap_or_default();
                    cands.sort_by_key(|c| {
                        let has_synced = c["syncedLyrics"].as_str().is_some_and(|s| !s.is_empty());
                        let delta = (c["duration"].as_f64().unwrap_or(0.0).round() as i64 - dur).abs();
                        (if has_synced { 0 } else { 1 }, delta)
                    });
                    for c in cands.iter().take(5) {
                        if let Some(l) = take(c) {
                            return Some(l);
                        }
                    }
                }

        // 連上了但真的沒有 —— 這跟「網路壞掉」不一樣，要回 Some 讓它被快取，
        // 否則每次換到這首都會再打三次 API。
        Some(Lyrics { source: "none".into(), ..Default::default() })
    }
}

#[cfg(test)]
mod tests {
    use super::{Fetcher, set_track_offset, shift, to_traditional};
    use crate::config::Config;

    /// 整條來源鏈連真的網路，手動跑：`cargo test live_chain -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_chain() {
        let dir = std::env::temp_dir().join(format!("vitro-chain-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = Fetcher::new(dir.clone());
        let cfg = Config::default();
        for (i, (artist, title, album, dur)) in [
            ("周杰倫", "晴天", "葉惠美", 269_000),
            ("米津玄師", "Lemon", "Lemon", 255_000),
            ("Ed Sheeran", "Shape of You", "÷", 233_000),
            ("五月天", "倔強", "神的孩子都在跳舞", 259_000),
            ("陳奕迅", "十年", "黑·白·灰", 205_000),
            ("deadman_not_exists", "zzzz", "", 200_000),
        ]
        .into_iter()
        .enumerate()
        {
            let t = std::time::Instant::now();
            let l = f.fetch(&cfg, &format!("k{i}"), artist, title, album, dur, None);
            let words = l.synced.iter().filter(|x| x.words.is_some()).count();
            let trans = l.synced.iter().filter(|x| x.trans.is_some()).count();
            println!(
                "{artist} - {title}: {} / {} 行 {} 逐字 {words} 翻譯 {trans} 耗時 {} ms",
                l.provider,
                l.source,
                l.synced.len(),
                t.elapsed().as_millis()
            );
        }
    }

    /// 本機自訂歌詞優先，而且專屬偏移會填進回傳值、不碰網路
    #[test]
    fn local_override_and_per_track_offset() {
        let dir = std::env::temp_dir().join(format!("vitro-lyrics-test-{}", std::process::id()));
        let custom = dir.join("custom");
        std::fs::create_dir_all(&custom).unwrap();
        let file = custom.join("A_B - 曲_名.lrc");
        std::fs::write(&file, "\u{feff}[00:01.00]第一句\n[00:03.50]第二句\n").unwrap();
        let mut f = Fetcher::new(dir.clone());
        let cfg = Config { lyrics_offset_ms: 100, lyrics_traditional: false, ..Default::default() };

        let l = f.fetch(&cfg, "k1", "A:B", "曲:名", "", 200_000, None);
        assert_eq!(l.provider, "本機");
        assert_eq!(l.synced.len(), 2);
        assert_eq!(l.synced[1].t, 3500);
        assert_eq!(shift(&cfg, Some(&l)), 100);

        set_track_offset(&dir, "k1", -300);
        let l = f.fetch(&cfg, "k1", "A:B", "曲:名", "", 200_000, None);
        assert_eq!(l.offset_ms, -300);
        assert_eq!(shift(&cfg, Some(&l)), -200);

        set_track_offset(&dir, "k1", 0);
        assert_eq!(f.fetch(&cfg, "k1", "A:B", "曲:名", "", 200_000, None).offset_ms, 0);
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(dir.join("offsets.json"));
    }

    /// 自動對齊判斷配錯的來源：快取裡那份不再拿出來，重抓也跳過（全跳過時不碰網路，回「沒有」）
    #[test]
    fn 配錯的來源不再用() {
        use super::{Line, Lyrics};
        let dir = std::env::temp_dir().join(format!("vitro-lyrics-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wrong = Lyrics {
            synced: vec![Line { t: 1000, text: "別首歌".into(), ..Default::default() }],
            source: "synced".into(),
            provider: "KuGou".into(),
            chain: 2,
            ..Default::default()
        };
        crate::config::write_json(&dir.join("k1.json"), &wrong).unwrap();
        let cfg = Config { lyrics_netease: false, lyrics_traditional: false, ..Default::default() };
        let mut f = Fetcher::new(dir.clone());
        assert_eq!(f.fetch(&cfg, "k1", "A", "B", "", 200_000, None).provider, "KuGou", "還沒判斷前照用快取");

        let e = crate::align::Entry { bad: vec!["KuGou".into(), "LRCLIB".into()], ..Default::default() };
        crate::config::write_json(&dir.join("align.json"), &std::collections::BTreeMap::from([("k1".to_string(), e)])).unwrap();
        let l = f.fetch(&cfg, "k1", "A", "B", "", 200_000, None);
        assert_eq!(l.source, "none", "{} {}", l.provider, l.source);
        for n in ["k1.json", "align.json"] {
            let _ = std::fs::remove_file(dir.join(n));
        }
    }

    /// 期望值是 Python 版用的 opencc-python-reimplemented `s2tw` 跑出來的，兩版要一致
    #[test]
    fn s2tw_matches_python_opencc() {
        let src = "后来我总算学会了如何去爱 可惜你早已远去消失在人海 头发 干净 乾隆 皇后 只是 一只 着急 着落 台湾 云朵 系统 联系 里面 面条 钟表 一出戏 冲突 范围";
        let want = "後來我總算學會了如何去愛 可惜你早已遠去消失在人海 頭髮 乾淨 乾隆 皇后 只是 一隻 著急 著落 臺灣 雲朵 系統 聯繫 裡面 麵條 鐘錶 一齣戲 衝突 範圍";
        assert_eq!(to_traditional(src), want);
    }
}
