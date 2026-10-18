//! 網易雲音樂 —— 免自架、免登入，華語／日韓曲庫廣，而且常帶人工翻譯。
//!
//! 只用不需加密的公開端點：`/api/search/get/web`（搜尋）與 `/api/song/lyric`
//! （歌詞，回 `lrc` 行級、`tlyric` 翻譯、`romalrc` 羅馬拼音）。
//! 這條路拿不到逐字（`yrc` 要走加密的 eapi），所以這裡永遠是行級；逐字交給 LyricsPlus。

use std::collections::HashMap;

use super::matching::{Candidate, Outcome, pick};
use super::{Line, Lyrics, parse_lrc};

const REFERER: &str = "https://music.163.com";
/// 網易雲的公開端點對沒帶瀏覽器 UA 的請求偶爾會回空，所以不用專案自己的 UA。
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
pub fn search(http: &reqwest::blocking::Client, artist: &str, title: &str) -> Result<Vec<Candidate>, ()> {
    let kw = format!("{title} {artist}");
    let resp = http
        .get("https://music.163.com/api/search/get/web")
        .header("User-Agent", UA)
        .header("Referer", REFERER)
        .query(&[("s", kw.trim()), ("type", "1"), ("limit", "8")])
        .send()
        .map_err(|_| ())?;
    if !resp.status().is_success() {
        return Err(());
    }
    let v: serde_json::Value = resp.json().map_err(|_| ())?;
    let songs = v["result"]["songs"].as_array().cloned().unwrap_or_default();
    Ok(songs
        .iter()
        .filter_map(|s| {
            Some(Candidate {
                id: s["id"].as_i64()?.to_string(),
                title: s["name"].as_str()?.to_string(),
                artist: s["artists"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(" / "))
                    .unwrap_or_default(),
                album: s["album"]["name"].as_str().unwrap_or("").to_string(),
                duration_ms: s["duration"].as_i64().unwrap_or(0),
            })
        })
        .collect())
}

/// 開頭那一串「作词 : xxx」「编曲 : xxx」不是歌詞，網易雲的行級歌詞幾乎都有。
/// 只吃**開頭連續的**幾行，歌詞中間出現的同形句子不動。
pub(super) fn strip_credits(lines: Vec<Line>) -> Vec<Line> {
    const KEYS: &[&str] = &[
        "作词", "作詞", "作曲", "编曲", "編曲", "制作", "製作", "监制", "監製", "混音", "母带", "母帶", "录音", "錄音",
        "和声", "和聲", "合声", "合聲", "人声", "人聲", "配唱", "企划", "企劃", "统筹", "統籌", "封面", "OP", "SP", "吉他", "贝斯", "貝斯", "鼓", "键盘", "鍵盤", "弦乐", "弦樂", "词", "詞", "曲", "演唱", "歌手",
        "出品", "发行", "發行", "OP", "SP", "Producer", "Lyrics", "Composer", "Arranger", "Mixed", "Mastered", "Recorded",
    ];
    let is_credit = |s: &str| {
        let s = s.trim();
        let Some(pos) = s.find([':', '：']) else { return false };
        let key = s[..pos].trim();
        // 英文的製作名單幾乎都是「Lyrics by」「Produced by」「Mixed by」這種寫法
        let by_credit = key.is_ascii() && key.to_ascii_lowercase().ends_with(" by");
        key.chars().count() <= 24
            && (by_credit || KEYS.iter().any(|k| key.eq_ignore_ascii_case(k) || (!k.is_ascii() && key.starts_with(k))))
    };
    let skip = lines.iter().take_while(|l| l.text.trim().is_empty() || is_credit(&l.text)).count();
    lines.into_iter().skip(skip).collect()
}

/// 把副軌（翻譯／拼音）依時間掛到主歌詞的每一行上。
/// 先找時間完全相同的，沒有再找 500ms 內最近的；空白行不算。
pub(super) fn attach(main: &mut [Line], side: Vec<Line>, set: impl Fn(&mut Line, String)) {
    let side: Vec<Line> = side.into_iter().filter(|l| !l.text.trim().is_empty()).collect();
    if side.is_empty() {
        return;
    }
    let exact: HashMap<i64, &str> = side.iter().map(|l| (l.t, l.text.as_str())).collect();
    for ln in main.iter_mut() {
        if let Some(t) = exact.get(&ln.t) {
            set(ln, (*t).to_string());
            continue;
        }
        let near = side.iter().min_by_key(|s| (s.t - ln.t).abs()).filter(|s| (s.t - ln.t).abs() <= 500);
        if let Some(s) = near {
            set(ln, s.text.clone());
        }
    }
}

fn lyric_text<'a>(v: &'a serde_json::Value, key: &str) -> &'a str {
    v[key]["lyric"].as_str().unwrap_or("")
}

pub fn parse(v: &serde_json::Value) -> Option<Lyrics> {
    if v["nolyric"].as_bool() == Some(true) {
        return Some(Lyrics { source: "instrumental".into(), provider: "Netease".into(), ..Default::default() });
    }
    let lrc = lyric_text(v, "lrc");
    if lrc.contains("纯音乐，请欣赏") || lrc.contains("純音樂，請欣賞") {
        return Some(Lyrics { source: "instrumental".into(), provider: "Netease".into(), ..Default::default() });
    }
    let mut synced = strip_credits(parse_lrc(lrc));
    if synced.is_empty() {
        return None;
    }
    attach(&mut synced, parse_lrc(lyric_text(v, "tlyric")), |l, s| l.trans = Some(s));
    attach(&mut synced, parse_lrc(lyric_text(v, "romalrc")), |l, s| l.romaji = Some(s));
    Some(Lyrics { synced, source: "synced".into(), provider: "Netease".into(), ..Default::default() })
}

pub fn fetch(
    http: &reqwest::blocking::Client,
    artist: &str,
    title: &str,
    duration_ms: i64,
) -> Outcome {
    let cands = match search(http, artist, title) {
        Ok(c) => c,
        Err(()) => return Outcome::Failed,
    };
    let mut id = pick(&cands, artist, title, duration_ms).map(|c| c.id.clone());
    // 歌手寫法不同（英文名 vs 中文名）時，帶歌手的關鍵字會被別人的翻唱淹沒，只用曲名再找一次
    if id.is_none() && !artist.trim().is_empty() {
        match search(http, "", title) {
            Ok(c) => id = pick(&c, artist, title, duration_ms).map(|c| c.id.clone()),
            Err(()) => return Outcome::Failed,
        }
    }
    let Some(id) = id else { return Outcome::Miss };
    let resp = http
        .get("https://music.163.com/api/song/lyric")
        .header("User-Agent", UA)
        .header("Referer", REFERER)
        .query(&[("id", id.as_str()), ("lv", "1"), ("tv", "1"), ("rv", "1")])
        .send();
    let Ok(resp) = resp else { return Outcome::Failed };
    if !resp.status().is_success() {
        return Outcome::Failed;
    }
    let Ok(v) = resp.json::<serde_json::Value>() else { return Outcome::Failed };
    match parse(&v) {
        Some(l) => Outcome::Hit(l),
        None => Outcome::Miss,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credits_are_stripped_and_translation_attached() {
        let v = serde_json::json!({
            "lrc": {"lyric": "[00:00.000] 作词 : 某人\n[00:01.000] 作曲 : 某人\n[00:10.500]Hello\n[00:14.000]World"},
            "tlyric": {"lyric": "[00:10.500]你好\n[00:14.200]世界"},
            "romalrc": {"lyric": ""}
        });
        let l = parse(&v).unwrap();
        assert_eq!(l.synced.len(), 2);
        assert_eq!(l.synced[0].text, "Hello");
        assert_eq!(l.synced[0].trans.as_deref(), Some("你好"));
        assert_eq!(l.synced[1].trans.as_deref(), Some("世界")); // 200ms 內視為同一句
        assert!(l.synced[0].romaji.is_none());
    }

    /// 連真的網易雲，手動跑：`cargo test live_netease -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_netease() {
        let http = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(12)).build().unwrap();
        for (artist, title, dur) in [
            ("周杰倫", "晴天", 269_000),
            ("Jay Chou", "七里香", 299_000),
            ("YOASOBI", "夜に駆ける", 261_000),
            ("Coldplay", "Fix You", 295_000),
            ("Taylor Swift", "Love Story", 235_000),
            ("deadman_not_exists", "zzzz", 200_000),
        ] {
            match fetch(&http, artist, title, dur) {
                Outcome::Hit(l) => {
                    let n_trans = l.synced.iter().filter(|x| x.trans.is_some()).count();
                    let n_roma = l.synced.iter().filter(|x| x.romaji.is_some()).count();
                    println!(
                        "{artist} - {title}: {} 行 翻譯 {n_trans} 拼音 {n_roma} 第一句 {:?} / {:?}",
                        l.synced.len(),
                        l.synced.first().map(|x| &x.text),
                        l.synced.first().and_then(|x| x.trans.as_ref())
                    );
                }
                Outcome::Miss => println!("{artist} - {title}: Miss"),
                Outcome::Failed => println!("{artist} - {title}: Failed"),
            }
        }
    }
}
