//! 專輯比對 —— 對應 Python 版 `app.py` 的「專輯比對」一節。
//!
//! SMTC 不給任何 Spotify ID，所以要用「專輯名 + 演出者」回頭比對收藏清單。
//! 實測 17/17 完全命中且唯一，加上 `(Deluxe)`、`- Remastered`、`（豪華版）`、
//! `[Live]` 這類後綴也都還能對上。

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::config;
use crate::spotify::{Album, ApiError, Spotify};

/// 這份快取的比對演算法改過幾次，用版本號控制什麼時候該作廢重查。
/// v2 的退路只用專輯名查，同名的翻唱版可能被挑走（七里香 → 趙小泉）。
/// 哪幾筆錯了無從分辨，整批丟掉重查 —— 只有二十幾張，成本可以忽略。
const RESOLVE_CACHE_V: u32 = 3;
/// 換歌後停留多久才去搜尋專輯
const SEARCH_SETTLE: std::time::Duration = std::time::Duration::from_millis(1500);

const SUFFIXES: &[&str] = &[
    "deluxe",
    "remaster",
    "remastered",
    "single",
    "ep",
    "live",
    "expanded",
    "anniversary",
    "edition",
    "version",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Resolved {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub uri: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub art: String,
}

/// 去掉括號內容與版本後綴，NFKC 正規化，砍掉所有空白。
pub fn norm_name(s: &str) -> String {
    let s: String = s.nfkc().collect::<String>().to_lowercase();
    let s = s.trim();

    // 括號類（半形、全形、方括號、黑括號）整段拿掉
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '(' | '（' | '[' | '【' => depth += 1,
            ')' | '）' | ']' | '】' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }

    // 「 - remastered …」這類後綴：找最後一個破折號，看後面第一個詞是不是版本字
    if let Some(pos) = out.rfind(['-', '–']) {
        let tail = out[pos + 1..].trim_start().to_lowercase();
        let first_word: String =
            tail.chars().take_while(|c| c.is_alphanumeric()).collect();
        if SUFFIXES.contains(&first_word.as_str()) {
            out.truncate(pos);
        }
    }

    out.chars().filter(|c| !c.is_whitespace()).collect()
}

pub struct Matcher {
    /// 正規化專輯名 -> 收藏裡的專輯
    index: HashMap<String, Vec<Album>>,
    /// "正規化專輯名|正規化演出者" -> 搜尋結果（空的代表查過而且真的沒有）
    resolved: HashMap<String, Resolved>,
    /// 搜尋出錯（非限流）的專輯，在這個時間之前不再查。resolve 每個 tick 都會被叫，
    /// 沒有冷卻的話持續出錯的那首歌會一秒打兩次 /search。只存記憶體，不寫檔。
    fail_until: HashMap<String, f64>,
    /// 正在等待的搜尋：(key, 第一次看到的時間)。換歌後要停留夠久才真的去搜，
    /// 連續切歌時前一首的搜尋就不會發生。
    pending_since: Option<(String, std::time::Instant)>,
    path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct Match {
    pub uri: String,
    pub art: String,
    /// 是不是在使用者的收藏清單裡（決定封面流要不要跳到那張）
    #[allow(dead_code)] // 解析時有填，目前沒有人讀
    pub in_library: bool,
}

#[derive(Deserialize, Serialize, Default)]
struct ResolvedFile {
    #[serde(rename = "__v", default)]
    v: u32,
    #[serde(flatten)]
    entries: HashMap<String, Resolved>,
}

impl Matcher {
    pub fn load(path: PathBuf) -> Self {
        let file: ResolvedFile = config::read_json(&path);
        let entries = if file.v == RESOLVE_CACHE_V { file.entries } else { HashMap::new() };
        Self { index: HashMap::new(), resolved: entries, fail_until: HashMap::new(), pending_since: None, path }
    }

    pub fn rebuild_index(&mut self, albums: &[Album]) {
        self.index.clear();
        for a in albums {
            self.index.entry(norm_name(&a.name)).or_default().push(a.clone());
        }
    }

    /// 中斷連結時清空。檔案由呼叫端刪，這裡只清記憶體裡的 —— 不清的話下次比對又會寫回去
    pub fn forget_all(&mut self) {
        self.index.clear();
        self.resolved.clear();
    }

    fn save(&self) {
        let file = ResolvedFile { v: RESOLVE_CACHE_V, entries: self.resolved.clone() };
        let _ = config::write_json(&self.path, &file);
    }

    /// 把 SMTC 的專輯名對回 Spotify 專輯。
    ///
    /// 順序刻意由便宜到貴：收藏比對（0 次請求）→ 磁碟快取（0 次）→ 搜尋（1 次）。
    /// 搜尋結果永久存檔，所以每張專輯一輩子最多花一次配額。
    pub fn resolve(&mut self, sp: &mut Spotify, album_name: &str, artist: &str) -> Option<Match> {
        if album_name.is_empty() {
            return None;
        }
        let key = norm_name(album_name);
        let na = norm_name(artist);

        if let Some(cands) = self.index.get(&key) {
            if !na.is_empty() {
                for a in cands {
                    if norm_name(&a.artist).contains(&na) {
                        return Some(Match {
                            uri: a.uri.clone(),
                            art: a.art.clone(),
                            in_library: true,
                        });
                    }
                }
            }
            // 只有一張同名，演出者對不上也認了
            if cands.len() == 1 {
                let a = &cands[0];
                return Some(Match { uri: a.uri.clone(), art: a.art.clone(), in_library: true });
            }
        }

        let ck = format!("{key}|{na}");
        if let Some(hit) = self.resolved.get(&ck) {
            return if hit.uri.is_empty() {
                None
            } else {
                Some(Match { uri: hit.uri.clone(), art: hit.art.clone(), in_library: false })
            };
        }

        // 被限流時直接放棄，而且**不要**寫負向快取 —— 那次搜尋根本沒發生，
        // 記成「找不到」的話這張專輯即使等限流解除也永遠不會再查一次。
        if config::now_secs() < sp.rate_limited_until() {
            return None;
        }

        if self.fail_until.get(&ck).is_some_and(|t| config::now_secs() < *t) {
            return None;
        }

        // 換歌後先用 SMTC 的縮圖頂著，停留夠久才花配額去搜 640px 的。
        // 搜尋結果會永久存檔，所以常聽的歌只會等這一次。
        let since = match &self.pending_since {
            Some((k, t)) if *k == ck => *t,
            _ => {
                let now = std::time::Instant::now();
                self.pending_since = Some((ck.clone(), now));
                now
            }
        };
        if since.elapsed() < SEARCH_SETTLE {
            return None;
        }

        match search_album(sp, album_name, artist) {
            Ok(Some(it)) => {
                let art = it["images"]
                    .as_array()
                    .and_then(|v| v.first())
                    .and_then(|i| i["url"].as_str())
                    .unwrap_or("")
                    .to_string();
                let found =
                    Resolved { uri: it["uri"].as_str().unwrap_or("").to_string(), art };
                self.resolved.insert(ck, found.clone());
                self.save();
                Some(Match { uri: found.uri, art: found.art, in_library: false })
            }
            Ok(None) => {
                // 真的查過而且沒有，才記下來省配額。
                self.resolved.insert(ck, Resolved::default());
                self.save();
                None
            }
            // 限流或未授權：沒查成就不要記。限流以外的錯誤要冷卻，不然下個 tick 又打一次。
            Err(e) => {
                if !matches!(e, ApiError::RateLimited { .. } | ApiError::NotAuthed) {
                    self.fail_until.insert(ck, config::now_secs() + 120.0);
                }
                None
            }
        }
    }
}

/// 找出對應的 Spotify 專輯。找不到回 `Ok(None)`。
///
/// Spotify 桌面版顯示的是在地化的演出者名稱，跟目錄裡的正式名稱常常不一樣
/// （宇宙人 / Cosmos People、周杰倫 / Jay Chou）。所以：
///
/// 1. 先用 `album:` + `artist:` 精準查。名字本來就一致的一次就中。
/// 2. 沒中就改用「專輯名 + 演出者」的自由文字查。搜尋引擎的排序認得在地化
///    名稱，只有 `artist:` 這個欄位過濾器不認 —— 實測「七里香 周杰倫」第一筆
///    就是 Jay Chou 的那張，而只用 `album:"七里香"` 會挑到同名的翻唱版。
///
/// 兩段都只認「專輯名完全相同」的結果，避免把『超級右腦』當成『右腦』。
fn search_album(
    sp: &mut Spotify,
    album_name: &str,
    artist: &str,
) -> Result<Option<serde_json::Value>, ApiError> {
    let key = norm_name(album_name);

    let q = format!("album:\"{album_name}\" artist:\"{artist}\"");
    let data = sp.get(
        "/search",
        &[("q", q), ("type", "album".into()), ("limit", "1".into())],
    )?;
    if let Some(first) = data
        .as_ref()
        .and_then(|d| d["albums"]["items"].as_array())
        .and_then(|v| v.first())
    {
        return Ok(Some(first.clone()));
    }

    if artist.is_empty() {
        return Ok(None);
    }
    let data = sp.get(
        "/search",
        &[
            ("q", format!("{album_name} {artist}")),
            ("type", "album".into()),
            ("limit", "10".into()),
        ],
    )?;
    let items = data
        .as_ref()
        .and_then(|d| d["albums"]["items"].as_array())
        .cloned()
        .unwrap_or_default();
    let exact: Vec<&serde_json::Value> = items
        .iter()
        .filter(|it| norm_name(it["name"].as_str().unwrap_or("")) == key)
        .collect();
    if exact.is_empty() {
        return Ok(None);
    }

    // 同名不只一張時，優先挑演出者也對得上的（英文名的情況）；
    // 對不上就用搜尋排序的第一名，那已經把演出者當關鍵字算進去了。
    let na = norm_name(artist);
    if exact.len() > 1 && !na.is_empty() {
        for it in &exact {
            let hit = it["artists"].as_array().is_some_and(|v| {
                v.iter().any(|a| {
                    let n = norm_name(a["name"].as_str().unwrap_or(""));
                    !n.is_empty() && (n.contains(&na) || na.contains(&n))
                })
            });
            if hit {
                return Ok(Some((*it).clone()));
            }
        }
    }
    Ok(Some(exact[0].clone()))
}
