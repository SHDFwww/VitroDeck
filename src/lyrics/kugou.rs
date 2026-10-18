//! 酷狗音樂 —— 免登入的逐字歌詞（KRC），華語與日語曲庫很廣，內嵌的 `[language:…]` 還帶翻譯。
//!
//! 流程：歌曲搜尋拿 hash → 用 hash 問歌詞候選（id＋accesskey）→ 下載 KRC。
//! KRC 是 `krc1` 四個位元組開頭，後面用固定 16 位元組金鑰逐位元組 XOR，再 zlib 解壓。
//! 西洋歌常搜到別人的 bootleg 或混音版，所以長度與歌手一定要驗（見 `matching`）。

use std::io::Read;

use base64::Engine;

use super::matching::{Candidate, Outcome, core_title, norm, pick};
use super::netease::strip_credits;
use super::{Line, Lyrics, Word};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
const KEY: [u8; 16] = [0x40, 0x47, 0x61, 0x77, 0x5e, 0x32, 0x74, 0x47, 0x51, 0x36, 0x31, 0x2d, 0xce, 0xd2, 0x6e, 0x69];
/// 歌詞候選與曲目長度差超過這個就不是同一個版本
const MAX_DELTA_MS: i64 = 3000;

/// 解開 KRC 的外殼，回傳純文字。
pub fn decode_krc(raw: &[u8]) -> Option<String> {
    if raw.len() <= 4 || &raw[..4] != b"krc1" {
        return None;
    }
    let xored: Vec<u8> = raw[4..].iter().enumerate().map(|(i, b)| b ^ KEY[i % KEY.len()]).collect();
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(xored.as_slice()).read_to_end(&mut out).ok()?;
    let text = String::from_utf8(out).ok()?;
    Some(text.trim_start_matches('\u{feff}').to_string())
}

/// `[start,dur]<off,dur,0>字<off,dur,0>字…`。逐字的時間是相對這一行開頭的，這裡換成絕對時間。
fn parse_line(raw: &str) -> Option<Line> {
    let rest = raw.strip_prefix('[')?;
    let (head, body) = rest.split_once(']')?;
    let (start, _dur) = head.split_once(',')?;
    let start: i64 = start.trim().parse().ok()?;

    let mut words = Vec::new();
    let mut s = body;
    while let Some(open) = s.find('<') {
        let Some(close) = s[open..].find('>').map(|p| open + p) else { break };
        let mut nums = s[open + 1..close].split(',').map(|x| x.trim().parse::<i64>().ok());
        let (off, d) = (nums.next().flatten(), nums.next().flatten());
        let after = &s[close + 1..];
        let end = after.find('<').unwrap_or(after.len());
        if let (Some(off), Some(d)) = (off, d)
            && !after[..end].is_empty() {
                words.push(Word { t: start + off, d, text: after[..end].to_string() });
            }
        s = &after[end..];
    }
    let text: String = words.iter().map(|w| w.text.as_str()).collect::<String>().trim().to_string();
    Some(Line { t: start, text, words: if words.is_empty() { None } else { Some(words) }, ..Default::default() })
}

/// 內嵌的翻譯。`type == 1` 是翻譯、`type == 0` 是音譯（這裡不用）。
/// 每個元素對應 KRC 的同一行（連製作名單那幾行也算），所以行數對不上就整份不要。
fn embedded_translation(text: &str, n_lines: usize) -> Option<Vec<String>> {
    let tag = text.lines().find_map(|l| l.strip_prefix("[language:"))?;
    let b64 = tag.split_once(']')?.0;
    let json = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&json).ok()?;
    let part = v["content"].as_array()?.iter().find(|c| c["type"].as_i64() == Some(1))?;
    let rows: Vec<String> = part["lyricContent"]
        .as_array()?
        .iter()
        .map(|r| {
            r.as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "))
                .unwrap_or_default()
        })
        .collect();
    (rows.len() == n_lines).then_some(rows)
}

/// 第一行常是「曲名 - 歌手」的標題，不是歌詞。
fn drop_title_line(mut lines: Vec<Line>, title: &str) -> Vec<Line> {
    let want = core_title(title);
    if let Some(first) = lines.first()
        && !want.is_empty() && first.text.contains(" - ") && norm(&first.text).contains(&want) {
            lines.remove(0);
        }
    lines
}

pub fn parse(text: &str, title: &str) -> Option<Lyrics> {
    let mut lines: Vec<Line> = text.lines().filter_map(|l| parse_line(l.trim())).collect();
    if lines.is_empty() {
        return None;
    }
    if let Some(tr) = embedded_translation(text, lines.len()) {
        for (ln, t) in lines.iter_mut().zip(tr) {
            if !t.trim().is_empty() {
                ln.trans = Some(t.trim().to_string());
            }
        }
    }
    let mut synced = strip_credits(drop_title_line(lines, title));
    synced.retain(|l| !l.text.is_empty());
    if synced.is_empty() {
        return None;
    }
    synced.sort_by_key(|l| l.t);
    let has_words = synced.iter().any(|l| l.words.is_some());
    Some(Lyrics {
        synced,
        source: if has_words { "word".into() } else { "synced".into() },
        provider: "KuGou".into(),
        ..Default::default()
    })
}

fn search_songs(http: &reqwest::blocking::Client, keyword: &str) -> Result<Vec<Candidate>, ()> {
    // 這個搜尋端點只有 http 通（https 連不上），送出去的只有曲名與歌手
    let resp = http
        .get("http://mobilecdn.kugou.com/api/v3/search/song")
        .header("User-Agent", UA)
        .query(&[("format", "json"), ("keyword", keyword), ("page", "1"), ("pagesize", "8")])
        .send()
        .map_err(|_| ())?;
    if !resp.status().is_success() {
        return Err(());
    }
    let v: serde_json::Value = resp.json().map_err(|_| ())?;
    let list = v["data"]["info"].as_array().cloned().unwrap_or_default();
    Ok(list
        .iter()
        .filter_map(|s| {
            Some(Candidate {
                id: s["hash"].as_str().filter(|h| !h.is_empty())?.to_string(),
                title: s["songname"].as_str()?.to_string(),
                artist: s["singername"].as_str().unwrap_or("").to_string(),
                album: s["album_name"].as_str().unwrap_or("").to_string(),
                duration_ms: s["duration"].as_i64().unwrap_or(0) * 1000,
            })
        })
        .collect())
}

fn json_id(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(str::to_string).or_else(|| v.as_i64().map(|n| n.to_string()))
}

pub fn fetch(http: &reqwest::blocking::Client, artist: &str, title: &str, duration_ms: i64) -> Outcome {
    let kw = format!("{title} {artist}");
    let cands = match search_songs(http, kw.trim()) {
        Ok(c) => c,
        Err(()) => return Outcome::Failed,
    };
    let mut hash = pick(&cands, artist, title, duration_ms).map(|c| c.id.clone());
    if hash.is_none() && !artist.trim().is_empty() {
        match search_songs(http, title) {
            Ok(c) => hash = pick(&c, artist, title, duration_ms).map(|c| c.id.clone()),
            Err(()) => return Outcome::Failed,
        }
    }
    let Some(hash) = hash else { return Outcome::Miss };

    let dur_ms = duration_ms.to_string();
    let resp = http
        .get("https://lyrics.kugou.com/search")
        .header("User-Agent", UA)
        .query(&[("ver", "1"), ("man", "yes"), ("client", "pc"), ("keyword", title), ("duration", dur_ms.as_str()), ("hash", hash.as_str())])
        .send();
    let Ok(resp) = resp else { return Outcome::Failed };
    let Ok(v) = resp.json::<serde_json::Value>() else { return Outcome::Failed };
    // 伺服器照相關度排好了，取第一個長度對得上的
    let chosen = v["candidates"].as_array().and_then(|a| {
        a.iter().find(|c| {
            duration_ms <= 0 || c["duration"].as_i64().is_some_and(|d| (d - duration_ms).abs() <= MAX_DELTA_MS)
        })
    });
    let Some(c) = chosen else { return Outcome::Miss };
    let (Some(id), Some(key)) = (json_id(&c["id"]), c["accesskey"].as_str()) else { return Outcome::Miss };

    let resp = http
        .get("https://lyrics.kugou.com/download")
        .header("User-Agent", UA)
        .query(&[("ver", "1"), ("client", "pc"), ("id", id.as_str()), ("accesskey", key), ("fmt", "krc"), ("charset", "utf8")])
        .send();
    let Ok(resp) = resp else { return Outcome::Failed };
    let Ok(v) = resp.json::<serde_json::Value>() else { return Outcome::Failed };
    let Some(content) = v["content"].as_str() else { return Outcome::Miss };
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(content) else { return Outcome::Miss };
    match decode_krc(&raw).and_then(|t| parse(&t, title)) {
        Some(l) => Outcome::Hit(l),
        None => Outcome::Miss,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn encode(text: &str) -> Vec<u8> {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(text.as_bytes()).unwrap();
        let body = z.finish().unwrap();
        let mut out = b"krc1".to_vec();
        out.extend(body.iter().enumerate().map(|(i, b)| b ^ KEY[i % KEY.len()]));
        out
    }

    fn sample() -> String {
        let lang = serde_json::json!({"content": [
            {"language": 0, "type": 0, "lyricContent": [[""], [""], ["a", "b"], ["c"]]},
            {"language": 0, "type": 1, "lyricContent": [[" "], [" "], ["你好"], ["世界"]]},
        ], "version": 1});
        let b64 = base64::engine::general_purpose::STANDARD.encode(lang.to_string());
        format!(
            "[ti:晴天]\n[ar:周杰伦]\n[language:{b64}]\n\
             [0,2000]<0,160,0>晴<160,160,0>天<320,160,0> <480,160,0>-<640,160,0> <800,160,0>周<960,160,0>杰<1120,160,0>伦\n\
             [2000,2000]<0,450,0>词<450,450,0>：<900,450,0>某<1350,450,0>人\n\
             [10000,3000]<0,500,0>Hel<500,500,0>lo <1000,500,0>wor<1500,500,0>ld\n\
             [14000,2000]<0,1000,0>再<1000,1000,0>見\n"
        )
    }

    #[test]
    fn krc_roundtrip() {
        let raw = encode("[0,100]<0,100,0>字");
        assert_eq!(decode_krc(&raw).as_deref(), Some("[0,100]<0,100,0>字"));
        assert!(decode_krc(b"nope").is_none());
    }

    #[test]
    fn parses_words_translation_and_drops_title_and_credits() {
        let l = parse(&sample(), "晴天").unwrap();
        assert_eq!(l.source, "word");
        assert_eq!(l.synced.len(), 2, "標題行與製作名單要濾掉");
        assert_eq!(l.synced[0].text, "Hello world");
        // 逐字時間是相對行首的，要換成絕對時間
        let w = l.synced[0].words.as_ref().unwrap();
        assert_eq!((w[0].t, w[0].d, w[0].text.as_str()), (10_000, 500, "Hel"));
        assert_eq!(w[3].t, 11_500);
        // 翻譯照原本的行索引對（標題行與製作名單也佔位）
        assert_eq!(l.synced[0].trans.as_deref(), Some("你好"));
        assert_eq!(l.synced[1].trans.as_deref(), Some("世界"));
    }

    #[test]
    fn english_and_variant_credits_are_dropped_but_lyrics_with_colons_stay() {
        let line = |t: i64, s: &str| format!("[{t},1000]<0,1000,0>{s}\n");
        let text = [
            line(0, "合声：某人"),
            line(1000, "Lyrics by：Someone"),
            line(2000, "Produced by：Someone Else"),
            line(3000, "Hello: is it me"),
        ]
        .concat();
        let l = parse(&text, "x").unwrap();
        assert_eq!(l.synced.len(), 1);
        assert_eq!(l.synced[0].text, "Hello: is it me");
    }

    #[test]
    fn translation_with_wrong_line_count_is_ignored() {
        let text = sample().replace("[14000,2000]<0,1000,0>再<1000,1000,0>見\n", "");
        let l = parse(&text, "晴天").unwrap();
        assert!(l.synced[0].trans.is_none());
    }

    /// 連真的酷狗，手動跑：`cargo test live_kugou -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_kugou() {
        let http = reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(12)).build().unwrap();
        for (artist, title, dur) in [
            ("周杰倫", "晴天", 269_000),
            ("Jay Chou", "七里香", 299_000),
            ("YOASOBI", "夜に駆ける", 261_000),
            ("Taylor Swift", "Love Story", 235_000),
            ("Coldplay", "Fix You", 295_000),
            ("deadman_not_exists", "zzzz", 200_000),
        ] {
            match fetch(&http, artist, title, dur) {
                Outcome::Hit(l) => {
                    let nw = l.synced.iter().filter(|x| x.words.is_some()).count();
                    let nt = l.synced.iter().filter(|x| x.trans.is_some()).count();
                    println!("{artist} - {title}: {} 行 逐字 {nw} 翻譯 {nt} 第一句 {:?}", l.synced.len(), l.synced.first().map(|x| &x.text));
                }
                Outcome::Miss => println!("{artist} - {title}: Miss"),
                Outcome::Failed => println!("{artist} - {title}: Failed"),
            }
        }
    }
}
