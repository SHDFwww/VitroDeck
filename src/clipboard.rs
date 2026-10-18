//! 剪貼簿的純文字讀寫。
//!
//! 原生視窗的輸入框連貼上都要自己做。設定精靈要貼 32 字元的 client_id
//! （手打不實際），還要把 redirect URI 複製出去給使用者貼進 Spotify Dashboard。
//!
//! 直接打 Win32（`CF_UNICODETEXT`），不為了這兩件事多背一個相依套件。

use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// 開著剪貼簿的期間持有它，離開作用域一定關掉 —— 忘了關的話
/// 其他程式都開不了剪貼簿，直到我們結束為止。
struct Open;

impl Open {
    fn new() -> Option<Self> {
        // 別的程式正開著時會失敗，稍等再試幾次
        for _ in 0..5 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Some(Open);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

pub fn get_text() -> Option<String> {
    let _open = Open::new()?;
    unsafe {
        let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
        let mem = HGLOBAL(h.0);
        let p = GlobalLock(mem) as *const u16;
        if p.is_null() {
            return None;
        }
        let mut n = 0usize;
        while *p.add(n) != 0 {
            n += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
        let _ = GlobalUnlock(mem);
        Some(s)
    }
}

pub fn set_text(s: &str) -> bool {
    let Some(_open) = Open::new() else { return false };
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        if EmptyClipboard().is_err() {
            return false;
        }
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) else { return false };
        let p = GlobalLock(mem) as *mut u16;
        if p.is_null() {
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
        let _ = GlobalUnlock(mem);
        // 成功之後這塊記憶體歸系統管，不能再釋放
        SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(mem.0))).is_ok()
    }
}

#[cfg(test)]
mod tests {
    /// 會動到真的剪貼簿，所以平常不跑：`cargo test -- --ignored clipboard`。
    /// 測完把原本的內容放回去。
    #[test]
    #[ignore]
    fn clipboard_來回() {
        let keep = super::get_text();
        let s = "VitroDeck 剪貼簿測試 ✓ 0123abcd";
        assert!(super::set_text(s));
        assert_eq!(super::get_text().as_deref(), Some(s));
        if let Some(k) = keep {
            super::set_text(&k);
        }
    }
}
