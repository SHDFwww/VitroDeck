//! 斷詞：長句只准在詞（日文是文節）的邊界切，不會把一個詞切成兩半。
//!
//! 日文用 TinySegmenter（約 25 KB 的小模型），切完把助詞、助動詞、送假名黏回前一個詞，
//! 變成「君の」「聞こえた」這種文節。中文用 jieba，繁體先轉簡體再切（詞典是簡體），
//! 再照詞性把詞併成類似文節的詞組（[`zh_words`]）：「的、了、著」、方位詞黏回前一個詞，
//! 副詞、介詞、助動詞、數量詞黏到後一個詞。

use std::sync::OnceLock;

use jieba_rs::{Jieba, Tag};

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

/// 中文：jieba 切詞＋詞性，照 [`zh_level`] 併成詞組。
/// 只看 jieba 的詞邊界時，「不｜屬於」「被窩｜裡」「最｜熟悉」跟真正的詞組邊界一樣好切，長句常從這些地方斷開
fn zh_words(chars: &[char]) -> Option<Vec<u8>> {
    let s: String = chars.iter().collect();
    // 詞典是簡體。轉了字數要一樣才用（逐字對齊），不一樣就拿原文切
    let simp = crate::zh::tw2s(&s);
    let text = if simp.chars().count() == chars.len() { simp } else { s };
    let words = jieba().tag(&text, true);
    let blank = |w: &Tag| w.word.chars().all(char::is_whitespace);
    let mut out = vec![0u8; chars.len()];
    for (k, w) in words.iter().enumerate() {
        if w.start >= out.len() {
            continue;
        }
        out[w.start] = if k == 0 || blank(w) || blank(&words[k - 1]) {
            2
        } else {
            let pp = k.checked_sub(2).map(|j| &words[j]).filter(|x| !blank(x));
            let last = words.get(k + 1).is_none_or(blank);
            zh_level(pp, &words[k - 1], w, last)
        };
    }
    Some(out)
}

/// 在 `w` 前面切開好不好，`p` 是前一個詞、`pp` 再前一個，`last` 是 `w` 為這串最後一個詞。
/// 0 = 當成同一個詞（的、了、著），1 = 同一個詞組，2 = 詞組的開頭
fn zh_level(pp: Option<&Tag>, p: &Tag, w: &Tag, last: bool) -> u8 {
    let len = |x: &Tag| x.word.chars().count();
    let noun = |x: &Tag| x.tag.starts_with('n') || x.tag == "r";
    // 介詞。「和、跟」jieba 標連詞，後面接人或東西時當介詞（和你、跟我）
    let prep = |x: &Tag, next: &Tag| {
        (x.tag == "p" && next.tag != "p") || (["和", "跟", "与", "同"].contains(&x.word) && noun(next))
    };
    // 語助詞、結構助詞
    if (len(w) == 1 && "的了着过吗呢吧啊呀哦喔啦嘛地得们么".contains(w.word)) || w.tag.starts_with('u') || w.tag == "y" {
        return 0;
    }
    let left =
        // 方位詞：被窩裡、失憶中、道別之前
        (w.tag == "f" && len(w) <= 2)
        // 「的」後面的單字名詞（心愛的人），或句尾的短名詞（荒唐的玩笑、自焚的牢籠），不要單獨落一行
        || (p.word.ends_with('的') && ((len(w) == 1 && w.tag.starts_with('n')) || (last && len(w) <= 2)))
        // 兩字動詞後面的單字補語：準備好
        || (len(p) >= 2 && p.tag.starts_with('v') && len(w) == 1 && w.tag == "a")
        // 名詞接名詞：jieba 拆開的複合詞
        || (p.tag.starts_with('n') && w.tag.starts_with('n'))
        // 介詞＋受詞＋動詞：對自己苛責、在那守護、和你道別
        || pp.is_some_and(|pp| prep(pp, p) && noun(p) && (w.tag.starts_with('v') || w.tag.starts_with('a') || w.tag.starts_with('n')));
    let right =
        // 副詞：不、最、也、都、就、才、還
        (p.tag == "d" && len(p) <= 2)
        || prep(p, w)
        // 數量詞接名詞或量詞：無數強心針、那場雨
        || (matches!(p.tag, "m" | "mq" | "q") && (w.tag.starts_with('n') || w.tag == "q"))
        // 這、那開頭的代詞：這一切、那種命
        || (p.tag == "r" && (p.word.starts_with('这') || p.word.starts_with('那')))
        // 單字形容詞接名詞或副詞：重感情、早已經
        || (len(p) == 1 && p.tag == "a" && (w.tag.starts_with('n') || w.tag == "d"))
        // 助動詞、使役：能發生、會記得、讓人、想你
        || ["能", "会", "要", "该", "想", "可以", "能够", "不会", "怎能", "无法", "没", "没有", "让", "被"].contains(&p.word);
    if left || right { 1 } else { 2 }
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
    fn 中文照詞性併成詞組() {
        // 實際歌詞裡被切開過的：副詞、介詞往後黏，方位詞、的後面的單字往前黏
        for (line, keep) in [
            ("最怕回到被窩裡的夢境振作", "被窩裡的"),
            ("成為最熟悉的陌生人", "最熟悉的"),
            ("何必留戀在不屬於我的地方", "在不屬於"),
            ("放不下心愛的人匆匆離去", "心愛的人"),
            ("還沒準備好去面對沒有你的世界", "還沒準備好"),
            ("在數不清的失憶中照亮黑夜裡的星", "黑夜裡的星"),
            ("就算是重感情的廢物希望你回來", "重感情的"),
        ] {
            let s = show(line).replace('.', "");
            assert!(s.split('|').any(|p| p.contains(keep)), "{keep} 被切開了：{s}");
        }
        // 「的、了、著」跟前一個詞算同一個詞，詞組裡也不從這裡切
        let c: Vec<char> = "我試著鬆開手".chars().collect();
        assert_eq!(starts(&c)[2], 0);
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
