//! 各來源共用的「這是不是同一首歌」比對。
//!
//! 寧可沒有，也不要配上別人的歌詞：搜尋結果裡混著翻唱、卡拉 OK、bootleg，
//! 只看長度或只看曲名都會抓錯。

use super::{Lyrics, to_traditional};

/// 長度差超過這個就不是同一個版本（現場版、加長版、混音版）。
const MAX_DELTA_MS: i64 = 3000;
/// 沒對上歌手時，曲名完全相同才肯收，而且長度要更貼。
const STRICT_DELTA_MS: i64 = 1500;

pub enum Outcome {
    Hit(Lyrics),
    /// 連上了但沒有這首（或沒有可信的候選）
    Miss,
    /// 網路或伺服器出問題，不代表真的沒有
    Failed,
}

#[allow(dead_code)] // album 留給之後的手動修正面板
pub struct Candidate {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: i64,
}

/// 比對用的正規化：先轉繁體（Spotify 與網易雲的簡繁不一定一致），
/// 再丟掉標點與空白、轉小寫。
pub fn norm(s: &str) -> String {
    to_traditional(s)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 去掉曲名裡的版本註記：`(Remastered)`、`【Live】`、` - Live` 之類。
pub fn core_title(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '(' | '（' | '[' | '【' | '「' => depth += 1,
            ')' | '）' | ']' | '】' | '」' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let head = out.split(" - ").next().unwrap_or("");
    norm(head)
}

fn contains_either(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && (a.contains(b) || b.contains(a))
}

/// 候選的歌手欄位對得上查詢嗎。查詢的歌手字串可能是「A, B」的合併寫法。
fn artist_matches(query: &str, cand: &str) -> bool {
    let q = norm(query);
    if q.is_empty() {
        return true;
    }
    let parts: Vec<String> = cand
        .split(['/', ',', '、', '&'])
        .map(norm)
        .filter(|p| p.chars().count() >= 2)
        .collect();
    parts.iter().any(|p| q.contains(p.as_str())) || contains_either(&q, &norm(cand))
}

/// 從搜尋結果挑最可信的一個；沒有就 `None`。寧可沒有，也不要配上別人的歌詞。
pub fn pick<'a>(cands: &'a [Candidate], artist: &str, title: &str, duration_ms: i64) -> Option<&'a Candidate> {
    let want = core_title(title);
    let delta = |c: &Candidate| if duration_ms > 0 { (c.duration_ms - duration_ms).abs() } else { 0 };
    let title_ok = |c: &Candidate| {
        let got = core_title(&c.title);
        got == want || (got.chars().count() >= 2 && contains_either(&got, &want) && artist_matches(artist, &c.artist))
    };

    // 第一層：曲名與歌手都對得上，長度差在容許範圍內，取最貼的
    let tier1 = cands
        .iter()
        .filter(|c| title_ok(c) && artist_matches(artist, &c.artist) && delta(c) <= MAX_DELTA_MS)
        .min_by_key(|c| delta(c));
    if let Some(c) = tier1 {
        return Some(c);
    }
    // 第二層：歌手寫法不同（英文名 vs 中文名）。曲名要完全相同、長度要更貼
    cands
        .iter()
        .filter(|c| core_title(&c.title) == want && !want.is_empty() && delta(c) <= STRICT_DELTA_MS && duration_ms > 0)
        .min_by_key(|c| delta(c))
}

/// 來源自己挑好、只回一首的（LyricsPlus），像不像同一首歌。
/// 長度不明就只認曲名與歌手。比 `pick` 多收一種：歌手對得上、長度差在 1.5 秒內，
/// 曲名寫法不同也收 —— 日文歌在 Spotify 常是英文或羅馬拼音曲名，來源那邊是原文。
/// QQ 音樂的長度捨去到整秒，同一首也會差到將近 1 秒，所以不能再緊
pub fn plausible(c: &Candidate, artist: &str, title: &str, duration_ms: i64) -> bool {
    let known = c.duration_ms > 0 && duration_ms > 0;
    if pick(std::slice::from_ref(c), artist, title, if known { duration_ms } else { 0 }).is_some() {
        return true;
    }
    known && (c.duration_ms - duration_ms).abs() <= STRICT_DELTA_MS && artist_matches(artist, &c.artist)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: i64, title: &str, artist: &str, dur: i64) -> Candidate {
        Candidate { id: id.to_string(), title: title.into(), artist: artist.into(), album: String::new(), duration_ms: dur }
    }

    fn pid(c: &[Candidate], artist: &str, title: &str, dur: i64) -> Option<i64> {
        pick(c, artist, title, dur).map(|c| c.id.parse().unwrap())
    }

    #[test]
    fn picks_the_original_not_the_karaoke_cover() {
        let cands = [
            c(1, "晴天 (原唱 周杰伦)", "RyaVocal", 270_738),
            c(2, "晴天", "Jay", 112_373),
            c(3, "晴天", "周杰伦", 269_000),
        ];
        assert_eq!(pid(&cands, "周杰倫", "晴天", 269_400), Some(3));
    }

    #[test]
    fn rejects_when_duration_is_off() {
        let cands = [c(3, "晴天", "周杰伦", 300_000)];
        assert_eq!(pid(&cands, "周杰倫", "晴天", 269_400), None);
    }

    #[test]
    fn version_suffix_is_ignored() {
        let cands = [c(7, "Fix You", "Coldplay", 295_000)];
        assert_eq!(pid(&cands, "Coldplay", "Fix You - Remastered 2011", 295_500), Some(7));
    }

    #[test]
    fn english_artist_vs_chinese_needs_exact_title_and_tight_duration() {
        let cands = [c(9, "七里香", "周杰伦", 299_000)];
        assert_eq!(pid(&cands, "Jay Chou", "七里香", 299_800), Some(9));
        assert_eq!(pid(&cands, "Jay Chou", "七里香", 302_000), None);
    }

    #[test]
    fn same_artist_other_song_is_rejected() {
        // LyricsPlus 照曲名搜〈小老婆〉，回的是同歌手的〈卷菸〉；公館青少年好幾首都回〈要不要跟我出去走一走〉
        let got = c(1, "卷菸", "美秀集团", 232_000);
        assert!(!plausible(&got, "美秀集團", "小老婆", 315_000));
        assert!(!plausible(&got, "美秀集團", "小老婆", 0));
        assert!(plausible(&got, "美秀集團", "捲菸", 232_400));
    }

    #[test]
    fn translated_title_passes_on_artist_and_tight_duration() {
        let got = c(1, "アイドル", "YOASOBI", 213_800);
        assert!(plausible(&got, "YOASOBI", "Idol", 213_500));
        assert!(!plausible(&got, "YOASOBI", "Idol", 216_000));
    }

}
