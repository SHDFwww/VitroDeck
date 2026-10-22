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

/// 「同一首歌、不同錄音」的版本註記，一組算一類（比對前轉繁體、小寫；英文要整個字）。
/// 只放錄音本身不一樣的：Remastered、Explicit、Album Version、feat. 都還是同一個錄音
const RECORDINGS: &[&[&str]] = &[
    &["live", "concert", "session", "sessions", "現場", "演唱會", "音樂會", "純享"],
    &["acoustic", "unplugged", "piano", "不插電", "鋼琴", "吉他"],
    &["instrumental", "inst", "karaoke", "off vocal", "伴奏", "純音樂", "卡拉ok", "ktv", "消音"],
    &["remix", "rmx", "sped up", "speed up", "slowed", "nightcore", "reverb", "混音", "重混", "dj版", "抖音", "加速", "降速", "慢速", "倍速"],
    &["cover", "翻唱"],
    &["demo"],
    &["orchestral", "orchestra", "symphonic", "交響"],
    &["medley", "mashup", "串燒"],
    &["spanglish"],
    &["spanish"],
    &["english", "英文版"],
    &["japanese", "日文版", "日語版"],
    &["korean", "韓文版", "韓語版"],
    &["chinese", "mandarin", "中文版", "國語版"],
    &["cantonese", "粵語版"],
    &["女聲版"],
    &["男聲版"],
    &["童聲版"],
];

/// 曲名裡的版本註記：括號裡的、` - ` 後面的（`core_title` 丟掉的那些），轉繁體、小寫
fn annotation(s: &str) -> String {
    let (mut head, mut inner) = (String::new(), String::new());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '(' | '（' | '[' | '【' | '「' => {
                depth += 1;
                inner.push(' ');
            }
            ')' | '）' | ']' | '】' | '」' => {
                depth = depth.saturating_sub(1);
                inner.push(' ');
            }
            _ if depth == 0 => head.push(c),
            _ => inner.push(c),
        }
    }
    if let Some((_, tail)) = head.split_once(" - ") {
        inner.push(' ');
        inner.push_str(tail);
    }
    to_traditional(&inner).to_lowercase()
}

/// `w` 在 `hay` 裡是不是一個完整的英文字（live 不算 alive、olive 裡的那個）
fn has_word(hay: &str, w: &str) -> bool {
    hay.match_indices(w).any(|(i, _)| {
        let before = hay[..i].chars().next_back();
        let after = hay[i + w.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric()) && !after.is_some_and(|c| c.is_ascii_alphanumeric())
    })
}

/// 曲名的版本註記落在 `RECORDINGS` 的哪幾類（位元）
fn recordings(title: &str) -> u32 {
    let a = annotation(title);
    RECORDINGS.iter().enumerate().fold(0, |bits, (i, marks)| {
        let hit = marks.iter().any(|m| if m.is_ascii() { has_word(&a, m) } else { a.contains(m) });
        if hit { bits | 1 << i } else { bits }
    })
}

/// 候選是不是同一首歌的別的錄音（現場、混音、伴奏、別的語言…）：它的註記有、我們的曲名沒有。
/// 長度常常只差一兩秒（QQ 音樂的〈Be My Baby〉回 2014 演唱會版、〈The Way〉回 Spanglish 版），光看長度擋不掉
pub fn other_recording(cand: &str, ours: &str) -> bool {
    recordings(cand) & !recordings(ours) != 0
}

/// 同一張專輯、長度差在 `STRICT_DELTA_MS` 內就是同一個錄音，曲名的版本註記只是兩邊標法不同
/// （QQ 音樂《Greatest Hits》的〈Killing Me Softly With His Song (Live from MCM)〉4:50，
/// Spotify 同一張專輯那首沒標 Live、一樣長，逐句時間也對得上）。QQ 的長度捨去到整秒，同一個音檔差不到 1 秒
pub fn same_release(cand_album: &str, cand_ms: i64, album: &str, duration_ms: i64) -> bool {
    let a = norm(album);
    !a.is_empty() && norm(cand_album) == a && cand_ms > 0 && duration_ms > 0 && (cand_ms - duration_ms).abs() <= STRICT_DELTA_MS
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
    let cands: Vec<&Candidate> = cands.iter().filter(|c| !other_recording(&c.title, title)).collect();

    // 第一層：曲名與歌手都對得上，長度差在容許範圍內，取最貼的
    let tier1 = cands
        .iter()
        .copied()
        .filter(|c| title_ok(c) && artist_matches(artist, &c.artist) && delta(c) <= MAX_DELTA_MS)
        .min_by_key(|c| delta(c));
    if let Some(c) = tier1 {
        return Some(c);
    }
    // 第二層：歌手寫法不同（英文名 vs 中文名）。曲名要完全相同、長度要更貼
    cands
        .into_iter()
        .filter(|c| core_title(&c.title) == want && !want.is_empty() && delta(c) <= STRICT_DELTA_MS && duration_ms > 0)
        .min_by_key(|c| delta(c))
}

/// 來源自己挑好、只回一首的（LyricsPlus），像不像同一首歌。
/// 長度不明就只認曲名與歌手。比 `pick` 多收一種：歌手對得上、長度差在 1.5 秒內，
/// 曲名寫法不同也收 —— 日文歌在 Spotify 常是英文或羅馬拼音曲名，來源那邊是原文。
/// QQ 音樂的長度捨去到整秒，同一首也會差到將近 1 秒，所以不能再緊
pub fn plausible(c: &Candidate, artist: &str, title: &str, duration_ms: i64) -> bool {
    if other_recording(&c.title, title) {
        return false;
    }
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

    #[test]
    fn 同一首的別的錄音不收() {
        // QQ 音樂實際回過的，長度都只差一兩秒
        let live = c(1, "Be My Baby (完整版|2014 iHeartradio演唱会)", "Ariana Grande", 219_000);
        assert!(!plausible(&live, "Ariana Grande", "Be My Baby", 217_053));
        let spanglish = c(2, "The Way (Spanglish Version)", "Ariana Grande", 226_000);
        assert!(!plausible(&spanglish, "Ariana Grande", "The Way", 227_026));
        // 這些還是同一個錄音
        for t in ["I'm The Man (Explicit)", "We Trying To Stay Alive (Album Version)", "My First Kiss(feat. Ke$ha)", "Let's Get It Started (Spike Mix)", "Fix You - Remastered 2011"] {
            assert!(!other_recording(t, "x"), "{t}");
        }
        // 我們的曲名也有同一類註記就收（寫法不同也算）
        let sped = c(3, "Collide (Sped Up Remix|Explicit)", "Justine Skye", 221_000);
        assert!(plausible(&sped, "Justine Skye", "Collide (Sped Up Remix)", 221_168));
        let xianchang = c(4, "晴天 (现场)", "周杰伦", 269_000);
        assert!(plausible(&xianchang, "周杰倫", "晴天 - Live", 269_400));
        // 曲名本身的字不算註記，alive 也不是 live
        assert!(other_recording("Live Forever (Live)", "Live Forever"));
        assert!(!other_recording("Stayin' Alive (Remastered)", "Stayin' Alive"));
        // 搜尋結果有現場版也有錄音室版：就算現場版長度比較貼，也挑錄音室版
        let cands = [c(5, "晴天 (Live)", "周杰伦", 269_300), c(6, "晴天", "周杰伦", 270_500)];
        assert_eq!(pid(&cands, "周杰倫", "晴天", 269_400), Some(6));
    }

    #[test]
    fn 同一張專輯一樣長就是同一個錄音() {
        // 都是 LyricsPlus 實際回的：QQ 的長度、專輯 vs Spotify 的
        assert!(same_release("Greatest Hits", 290_000, "Greatest Hits", 290_293));
        // 同一張但長了快 2 秒，不是同一個音檔
        assert!(!same_release("Yours Truly", 219_000, "Yours Truly", 217_053));
        // 十週年版是另一張（Spanglish 版只在那張）
        assert!(!same_release("Yours Truly (Tenth Anniversary Edition)", 226_000, "Yours Truly", 227_026));
        // 不知道專輯或長度就不算
        assert!(!same_release("", 200_000, "", 200_000));
        assert!(!same_release("A", 0, "A", 200_000));
    }
}
