//! 斷詞：長句只准在詞（日文是文節）的邊界切，不會把一個詞切成兩半。
//!
//! 日文用 TinySegmenter（約 25 KB 的小模型），切完把助詞、助動詞、送假名黏回前一個詞，
//! 變成「君の」「聞こえた」這種文節。中文用 jieba，繁體先轉簡體再切（詞典是簡體），
//! 「的、了、著」這類單字黏回前一個詞。

use std::sync::OnceLock;

use jieba_rs::Jieba;

fn jieba() -> &'static Jieba {
    static J: OnceLock<Jieba> = OnceLock::new();
    J.get_or_init(Jieba::new)
}

/// 先把詞典建好。第一次建要幾百毫秒，不要卡在畫面第一句
pub fn warm() {
    std::thread::spawn(|| {
        jieba();
    });
}

fn is_kana(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F)
}

fn is_hira(c: char) -> bool {
    matches!(c as u32, 0x3041..=0x309F)
}

fn is_han(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F) || c == '々'
}

/// 每個字前面切開好不好：2 = 詞組的開頭（日文的文節、中文的詞），1 = 文節裡面另一個詞的開頭，
/// 0 = 詞的中間。第 0 個字是 2。沒有漢字也沒有假名的整串都是 2（交給空白與標點切）
pub fn starts(chars: &[char]) -> Vec<u8> {
    let n = chars.len();
    if n == 0 {
        return Vec::new();
    }
    let mut out = if chars.iter().any(|&c| is_kana(c)) {
        bunsetsu(chars)
    } else if chars.iter().any(|&c| is_han(c)) {
        zh_words(chars)
    } else {
        None
    }
    .unwrap_or_else(|| vec![2; n]);
    out[0] = 2;
    out
}

/// 日文：TinySegmenter 切成詞，再照附屬語併成文節
fn bunsetsu(chars: &[char]) -> Option<Vec<u8>> {
    let s: String = chars.iter().collect();
    let mut words: Vec<Vec<char>> = Vec::new();
    for w in tinysegmenter::tokenize(&s) {
        let w: Vec<char> = w.chars().collect();
        // 格助詞有時跟後面的字黏成一個詞（そば／にい／て），先拆開
        if w.len() >= 2
            && w.iter().all(|&c| is_hira(c))
            && "にをがへ".contains(w[0])
            && !JA_FUNC.contains(&String::from_iter(&w).as_str())
            && words.last().is_some_and(|p| !is_func(p))
        {
            words.push(vec![w[0]]);
            words.push(w[1..].to_vec());
        } else {
            words.push(w);
        }
    }
    if words.iter().map(Vec::len).sum::<usize>() != chars.len() {
        return None;
    }
    let mut out = vec![0u8; chars.len()];
    let mut k = 0;
    let mut prev: Option<&Vec<char>> = None;
    for w in &words {
        out[k] = if prev.is_some_and(|p| attaches(p, w)) { 1 } else { 2 };
        k += w.len();
        prev = Some(w);
    }
    Some(out)
}

/// 日文的附屬語：接在前一個詞後面、自己不能單獨成一個文節的
const JA_FUNC: &[&str] = &[
    "は", "が", "を", "に", "へ", "で", "と", "も", "の", "や", "か", "ね", "よ", "な", "さ", "ぞ", "ぜ", "わ", "て", "た", "だ", "ば", "し", "ず", "ぬ", "う", "よう",
    "から", "まで", "より", "だけ", "しか", "ほど", "くらい", "ぐらい", "など", "って", "けど", "けれど", "のに", "ので", "ながら", "たり", "だり", "ても", "でも", "ては", "では", "には", "とは", "にも",
    "ない", "なく", "なかっ", "たい", "たく", "たかっ", "ます", "まし", "ませ", "です", "でし", "だろ", "だっ", "でしょ", "れる", "れ", "られ", "られる", "せる", "させ", "せ", "そう", "らしい",
    "てる", "てた", "ちゃう", "ちゃっ", "じゃ", "いる", "い", "いた", "いく", "いっ", "くれ", "くれる", "あげ", "もらっ", "しまっ", "ちゃ", "てく", "ん", "かな", "かも", "よね", "なら", "たら", "だら",
];

/// 接在它後面的一定是新的文節（格助詞、接續助詞）
const JA_BREAK: &[&str] = &[
    "は", "が", "を", "に", "へ", "で", "と", "も", "の", "から", "まで", "より", "には", "では", "とは", "にも", "でも", "って", "けど", "のに", "ので", "ながら", "ば", "ても",
];

fn hira_str(w: &[char]) -> Option<String> {
    w.iter().all(|&c| is_hira(c)).then(|| w.iter().collect())
}

fn is_func(w: &[char]) -> bool {
    hira_str(w).is_some_and(|s| JA_FUNC.contains(&s.as_str()))
}

fn attaches(prev: &[char], w: &[char]) -> bool {
    let Some(s) = hira_str(w) else { return false };
    if prev.iter().all(|c| c.is_whitespace()) || hira_str(prev).is_some_and(|p| JA_BREAK.contains(&p.as_str())) {
        return false;
    }
    // 附屬語，或附屬語後面的短平假名（し／たん／だ）。送假名 TinySegmenter 本來就留在詞裡（聞こえ、笑っ）
    JA_FUNC.contains(&s.as_str()) || (w.len() <= 2 && is_func(prev))
}

/// 中文：jieba 切詞，語助詞黏回前一個詞
fn zh_words(chars: &[char]) -> Option<Vec<u8>> {
    let s: String = chars.iter().collect();
    // 詞典是簡體。轉了字數要一樣才用（逐字對齊），不一樣就拿原文切
    let simp = crate::zh::tw2s(&s);
    let text = if simp.chars().count() == chars.len() { simp } else { s };
    let mut out = vec![0u8; chars.len()];
    let mut prev_space = true;
    for t in jieba().cut(&text, true) {
        let func = t.end - t.start == 1 && "的了着过吗呢吧啊呀哦喔啦嘛地得们么".contains(t.word);
        if t.start < out.len() && !(func && !prev_space) {
            out[t.start] = 2;
        }
        prev_space = t.word.chars().all(char::is_whitespace);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(s: &str) -> String {
        let c: Vec<char> = s.chars().collect();
        let b = starts(&c);
        let mut o = String::new();
        for (k, ch) in c.iter().enumerate() {
            if k > 0 && b[k] > 0 {
                o.push(if b[k] == 2 { '|' } else { '.' });
            }
            o.push(*ch);
        }
        o
    }

    #[test]
    #[ignore]
    fn 看切出來的樣子() {
        for s in [
            "君の声が聞こえた気がしたんだ",
            "夜明けまで走り続けていたい",
            "忘れられない夏の終わりに",
            "優しい嘘をついてしまった",
            "ずっとそばにいてくれるなら",
            "花びらが舞い散る空の下で",
            "もう一度だけ笑ってほしい",
            "今日もう一度サヨナラを言わないで",
            "我想要和你一起走到最後",
            "為什麼月亮走得那麼慢",
            "那是今晚最後一班列車",
            "風吹過了城市的街道",
            "你說的話我都記得",
        ] {
            println!("{}", show(s));
        }
    }
}
