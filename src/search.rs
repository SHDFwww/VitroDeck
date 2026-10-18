//! 搜尋與字母跳轉。搜尋框和 preset 選單共用同一個輸入框。
//!
//! 行為照 web 版：
//!
//! - `/` 開搜尋框，打字即時篩選（專輯名或演出者，不分大小寫的「包含」）
//! - Esc 關掉並清掉篩選，回到開啟前中央那張
//! - Enter 關掉、保留篩選、播中央那張
//! - ↑↓ 關掉、保留篩選 —— 之後用 ←→ 在結果裡翻
//! - 封面流按字母：跳到下一張「演出者或專輯名以該字母開頭」的，繞一圈

use crate::spotify::Album;

/// 單行文字輸入。原生沒有 `<input>`，連退格都要自己做。
///
/// `preedit` 是輸入法還在組字、還沒送出的那一段（注音、倉頡的候選）。
/// **它不能算進查詢字串** —— 打「ㄓㄡ」的時候不該拿注音符號去篩選，
/// 但畫面上要看得到，不然使用者不知道自己打到哪。
#[derive(Default, Clone, Debug)]
pub struct Input {
    pub text: String,
    pub preedit: String,
}

impl Input {
    pub fn push_str(&mut self, s: &str) {
        // 控制字元（Enter 的 \r、Tab 的 \t、退格的 \u{8}）不是文字
        self.text.extend(s.chars().filter(|c| !c.is_control()));
    }

    /// 刪一個字元。以 char 為單位 —— 中文一個字是三個位元組，
    /// 用 `pop()` 以外的方式按位元組砍會切在字的中間。
    pub fn backspace(&mut self) {
        self.text.pop();
    }

    pub fn composing(&self) -> bool {
        !self.preedit.is_empty()
    }
}

/// 查詢字串正規化：去頭尾空白、轉小寫。空字串代表「不篩選」。
pub fn norm(q: &str) -> String {
    q.trim().to_lowercase()
}

/// 名稱或演出者包含查詢字串。`q` 要先過 [`norm`]。
pub fn album_matches(a: &Album, q: &str) -> bool {
    q.is_empty() || a.name.to_lowercase().contains(q) || a.artist.to_lowercase().contains(q)
}

/// 篩選。空查詢回傳 `None`，代表「沒在篩選」—— 跟「篩了但沒結果」
/// （`Some(空的)`）是兩回事，後者畫面上要說「找不到」。
pub fn filter(library: &[Album], q: &str) -> Option<Vec<Album>> {
    let q = norm(q);
    if q.is_empty() {
        return None;
    }
    Some(library.iter().filter(|a| album_matches(a, &q)).cloned().collect())
}

/// 從 `from` 的下一張開始往後找，繞一圈。找不到回傳 `None`。
///
/// 從「下一張」開始而不是從頭：連按同一個字母會在所有 D 開頭的專輯之間
/// 輪流跳，這是 web 版的手感。
pub fn jump_letter(list: &[Album], from: usize, ch: char) -> Option<usize> {
    let n = list.len();
    if n == 0 {
        return None;
    }
    let c: String = ch.to_lowercase().collect();
    (1..=n).map(|k| (from + k) % n).find(|&i| {
        let a = &list[i];
        a.artist.to_lowercase().starts_with(&c) || a.name.to_lowercase().starts_with(&c)
    })
}

/// preset 名稱的篩選，給選單用。跟專輯一樣是不分大小寫的「包含」。
pub fn name_matches(name: &str, q: &str) -> bool {
    q.is_empty() || name.to_lowercase().contains(q)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn al(name: &str, artist: &str) -> Album {
        Album { name: name.into(), artist: artist.into(), ..Default::default() }
    }

    fn lib() -> Vec<Album> {
        vec![
            al("Discovery", "Daft Punk"),
            al("范特西", "周杰倫"),
            al("OK Computer", "Radiohead"),
            al("Random Access Memories", "Daft Punk"),
            al("七里香", "周杰倫"),
        ]
    }

    #[test]
    fn 空查詢代表沒在篩選() {
        assert!(filter(&lib(), "").is_none());
        assert!(filter(&lib(), "   ").is_none());
    }

    #[test]
    fn 篩不到跟沒篩是兩回事() {
        let r = filter(&lib(), "zzz").unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn 名稱或演出者都算而且不分大小寫() {
        let r = filter(&lib(), "daft").unwrap();
        assert_eq!(r.len(), 2);
        let r = filter(&lib(), "COMPUTER").unwrap();
        assert_eq!(r[0].name, "OK Computer");
    }

    #[test]
    fn 中文查詢() {
        let r = filter(&lib(), "周杰倫").unwrap();
        assert_eq!(r.len(), 2);
        let r = filter(&lib(), " 七里 ").unwrap();
        assert_eq!(r[0].name, "七里香");
    }

    #[test]
    fn 字母跳轉從下一張開始而且會繞回來() {
        let l = lib();
        // 從 0（Discovery）按 d：下一個 D 開頭的是 3（Daft Punk 那張）
        assert_eq!(jump_letter(&l, 0, 'd'), Some(3));
        // 從 3 再按一次 d：繞回 0
        assert_eq!(jump_letter(&l, 3, 'D'), Some(0));
        // 演出者開頭也算
        assert_eq!(jump_letter(&l, 0, 'r'), Some(2));
        assert_eq!(jump_letter(&l, 0, 'q'), None);
        assert_eq!(jump_letter(&[], 0, 'a'), None);
    }

    #[test]
    fn 退格以字元為單位() {
        let mut i = Input::default();
        i.push_str("周杰");
        i.backspace();
        assert_eq!(i.text, "周");
        i.push_str("a\rb\t");
        assert_eq!(i.text, "周ab");
    }

    #[test]
    fn 組字中的不算進查詢但看得到() {
        let i = Input { text: "周".into(), preedit: "ㄐㄧㄝ".into() };
        assert!(i.composing());
        assert_eq!(norm(&i.text), "周");
    }
}
