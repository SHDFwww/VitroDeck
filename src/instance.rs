//! 單一實例 —— Python 版 `_single_instance` 的對應。
//!
//! mutex 名字跟 Python 版**同一個**（`Local\VitroDeck.SingleInstance`），兩版不能同時開
//! （使用者決定的：Rust 版要取代 Python 版，開發期間 Python 版保持關閉）。
//!
//! 第二份啟動時要把第一份叫出來。Python 版靠 HTTP `/activate`；Rust 版平常沒有
//! HTTP 伺服器，改用具名事件：第一份開一條執行緒等 `Local\VitroDeck.Activate`，
//! 第二份 `SetEvent` 一下就走。第一份是 Python 版的話事件不存在，退回打它的 `/activate`。
//! 不能靠綁埠失敗判斷 —— 筆記記過，Windows 上兩份綁同一個埠不一定會報錯。

use std::io::{Read, Write};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE, INFINITE,
};
use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};
use windows::core::w;

/// 已經有一份在跑就叫它出來。回傳 `true` 表示這份可以繼續開，`false` 就該直接結束。
pub fn claim(port: u16) -> bool {
    unsafe {
        let Ok(h) = CreateMutexW(None, true, w!("Local\\VitroDeck.SingleInstance")) else {
            return true; // 連 mutex 都開不了就別擋，照開
        };
        if GetLastError() != ERROR_ALREADY_EXISTS {
            // handle 故意不關：程式結束時系統會收，關了鎖就沒了
            return true;
        }
        // 前景權在我們手上（使用者剛點的），讓給舊的那份，它的 SetForegroundWindow 才有效
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        if let Ok(ev) = OpenEventW(EVENT_MODIFY_STATE, false, w!("Local\\VitroDeck.Activate")) {
            let _ = SetEvent(ev);
            let _ = CloseHandle(ev);
            return false;
        }
        if http_activate(port) {
            return false;
        }
        // 叫不動：多半是舊的那份正在結束，等它放掉鎖再自己開
        let r = WaitForSingleObject(h, 5000);
        if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED {
            return true;
        }
        crate::warn!("已經有一份 VitroDeck 在跑但沒有回應，不再另開");
        false
    }
}

/// 第一份是 Python 版的時候：打它的 `/activate`。
fn http_activate(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
    if s.write_all(b"GET /activate HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").is_err() {
        return false;
    }
    let mut head = [0u8; 16];
    let n = s.read(&mut head).unwrap_or(0);
    // "HTTP/1.0 200 OK"
    head[..n].windows(5).any(|w| w == b" 200 ")
}

/// 第一份：等第二份來敲門，敲了就呼叫 `on_activate`（通常是送一個使用者事件給 winit）。
pub fn listen(on_activate: impl Fn() + Send + 'static) {
    let _ = std::thread::Builder::new().name("instance".into()).spawn(move || unsafe {
        // 事件在這條執行緒裡開（HANDLE 不能跨執行緒傳）。自動重設：每敲一次醒一次
        let Ok(ev) = CreateEventW(None, false, false, w!("Local\\VitroDeck.Activate")) else {
            crate::warn!("單一實例的喚醒事件開不起來，第二份啟動時叫不出這份");
            return;
        };
        loop {
            if WaitForSingleObject(ev, INFINITE) == WAIT_OBJECT_0 {
                on_activate();
            }
        }
    });
}
