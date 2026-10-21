//! 簡轉繁。歌詞在抓回來時轉（見 `lyrics::convert_result`），畫面上其他的字
//! （曲名、歌手、專輯、播放清單、裝置、通知）在排版時轉（見 `Text::shape`）。
//!
//! 只改「畫出來的樣子」，資料本身維持原文：抓歌詞的搜尋、跟收藏比對、
//! 跟 `/me/player` 對曲名都拿原文，換成繁體反而可能搜不到。

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use ferrous_opencc::OpenCC;
use ferrous_opencc::config::BuiltinConfig;

/// 設定的 `lyrics_traditional`。排版那一層每次都看它，改了下一幀就生效
static ON: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// ferrous-opencc（Apache-2.0）。原本用的 zhconv 是 GPL，編進去整個 exe 就得改用 GPL。
/// 字典在編譯時就包進 exe，第一次用才建；建不起來就原文照回，字不該因此出不來。
fn run(cfg: BuiltinConfig, cell: &'static OnceLock<Option<OpenCC>>, text: &str) -> String {
    match cell.get_or_init(|| OpenCC::from_config(cfg).ok()) {
        Some(cc) => cc.convert(text),
        None => text.to_string(),
    }
}

/// 用 s2tw（純字元對映）而不是 s2twp（含台灣用詞）。用詞替換會整個詞換掉，
/// 例如「方便面」→「泡麵」是 3 字變 2 字；而逐字歌詞是一個字一個時間戳，
/// 字數一變時間軸就整行錯位。s2tw 天生 1:1，逐字轉換絕對安全。
///
/// 這是不看原文的硬轉，已經是繁體的字也會被改（「干擾」→「幹擾」），
/// 要顯示的字用 [`to_trad`]
pub fn s2tw(text: &str) -> String {
    static CC: OnceLock<Option<OpenCC>> = OnceLock::new();
    run(BuiltinConfig::S2tw, &CC, text)
}

pub fn tw2s(text: &str) -> String {
    static CC: OnceLock<Option<OpenCC>> = OnceLock::new();
    run(BuiltinConfig::Tw2s, &CC, text)
}

/// 有沒有漢字。沒有的話（英文、韓文、數字）不用轉，也不用進快取
pub fn has_han(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F))
}

/// 有假名就是日文。日文的新字體有不少跟簡體同形（国、体、会、声），
/// 照簡體轉會變成「國境の南」這種中日混搭，所以整串不轉
fn has_kana(s: &str) -> bool {
    s.chars().any(|c| matches!(c as u32, 0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F))
}

/// s2tw 會改、但在台灣的繁體裡也是正常用字的字（從 Big5 裡 s2tw 會改的 138 個字挑的）。
/// 繁體字串裡出現這些字不算簡體的證據：「干擾」的「干」、「游過」的「游」、
/// 「這台裝置」的「台」，s2tw 一律硬轉成幹、遊、臺
const BOTH: &str = "几于干丑凶斗占台伙划吁后并朴余里岩岳征杰采范准游恒裏夸污涌雇愿霉晒挂咸郁筑荐帘泄肴咨喫脣祕樑檐癡纔嫺尸佣云丰仆扑";

/// 要顯示的繁體版本。`None` = 原文照用：沒有漢字、是日文、本來就是繁體，或轉了也一樣。
///
/// 只轉簡體字串。判斷：沒有任何繁體專用字（tw2s 會改的字），或者有 s2tw 會改、
/// 又不在 [`BOTH`] 裡的字。混著寫的（「打擾一下乐团」）照轉，繁體的字 s2tw 不會動。
/// 「台」轉完留著，不改成「臺」：台灣慣用「台」，介面上的字也都是「台」。
///
/// 轉完字數或位元組數變了也當作不轉：逐字歌詞是一個字一個時間戳，逐字掃光
/// （`Text::lit_runs`）拿的是原文的位元組位置。s2tw 幾乎都是 3 位元組對 3 位元組，這條只是保險
pub fn to_trad(s: &str) -> Option<String> {
    if !has_han(s) || has_kana(s) {
        return None;
    }
    let t = s2tw(s);
    if t == s || t.len() != s.len() || t.chars().count() != s.chars().count() {
        return None;
    }
    let simplified = s.chars().zip(t.chars()).any(|(a, b)| a != b && !BOTH.contains(a));
    if !simplified && tw2s(s) != s {
        return None;
    }
    let out: String = s.chars().zip(t.chars()).map(|(a, b)| if a == '台' { a } else { b }).collect();
    (out != s).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::to_trad;

    #[test]
    fn 簡體轉繁體() {
        assert_eq!(to_trad("预言").as_deref(), Some("預言"));
        assert_eq!(to_trad("我还能找到吗").as_deref(), Some("我還能找到嗎"));
        // 只有兩岸都用的字＋共用字的簡體，沒有繁體專用字，照轉
        assert_eq!(to_trad("最后").as_deref(), Some("最後"));
        // 混著寫的：只轉簡體那幾個
        assert_eq!(to_trad("打擾一下乐团").as_deref(), Some("打擾一下樂團"));
    }

    #[test]
    fn 台不改成臺() {
        assert_eq!(to_trad("台湾").as_deref(), Some("台灣"));
        assert_eq!(to_trad("这台装置").as_deref(), Some("這台裝置"));
    }

    #[test]
    fn 繁體不動() {
        // s2tw 硬轉會變成「這臺裝置」「幹擾」「遊過」
        for s in ["這台裝置", "干擾", "游過", "打擾一下樂團", "我還能找到嗎"] {
            assert_eq!(to_trad(s), None, "{s}");
        }
    }

    #[test]
    fn 不用轉的照原文() {
        assert_eq!(to_trad("EXIT"), None);
        assert_eq!(to_trad("봄날"), None);
        // 有假名：「国」不能變成「國」
        assert_eq!(to_trad("国境の南"), None);
    }
}
