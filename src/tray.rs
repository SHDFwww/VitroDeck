//! 系統列圖示（右下角通知區域）—— Python 版 `tray.py` 的對應。
//!
//! Python 版借 .NET 的 NotifyIcon；這裡直接打 `Shell_NotifyIconW`。圖示與右鍵選單
//! 跑在自己的執行緒和訊息迴圈上，點下去的事一律交給回呼（主程式那邊會轉成
//! winit 的使用者事件），不在這條執行緒碰主視窗。
//!
//! 用一般的隱藏視窗而不是 message-only 視窗：explorer 重開時廣播的
//! `TaskbarCreated` 只送給頂層視窗，收到它才能把圖示加回去。
//!
//! 全域一份（跟 `ledfeed` 一樣）：poller 換歌時要改提示文字。

use std::cell::RefCell;
use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{w, PCWSTR};

/// 選單點了什麼、或左鍵點了圖示
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Open,
    Mini,
    MiniLock,
    /// 把迷你視窗移回主螢幕右下角
    MiniRecall,
    Quit,
}

/// 選單的一列。`None` 是分隔線；`checked` 是 `None` 就是一般項目。
pub type Item = Option<(String, Option<bool>, Cmd)>;

const WM_TRAY: u32 = WM_APP + 1;
const WM_TIP: u32 = WM_APP + 2;
const ID: u32 = 1;

struct State {
    on_cmd: Box<dyn Fn(Cmd)>,
    menu: Box<dyn Fn() -> Vec<Item>>,
    icon: HICON,
    taskbar_created: u32,
}

thread_local! {
    static ST: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// 執行緒外面看得到的：隱藏視窗的 handle（`HWND` 不能跨執行緒，存成數字）與提示文字
struct Handle {
    hwnd: isize,
    tip: String,
}

static TRAY: Mutex<Option<Handle>> = Mutex::new(None);

pub fn running() -> bool {
    TRAY.lock().unwrap().is_some()
}

/// 開圖示。已經開著就什麼都不做。圖示加不上去回傳 `false` ——
/// 那時呼叫端不能攔關閉鈕，不然視窗藏起來之後再也叫不回來。
pub fn start(
    on_cmd: impl Fn(Cmd) + Send + 'static,
    menu: impl Fn() -> Vec<Item> + Send + 'static,
) -> bool {
    if running() {
        return true;
    }
    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    let spawned = std::thread::Builder::new().name("tray".into()).spawn(move || unsafe {
        let ok = create(Box::new(on_cmd), Box::new(menu));
        let _ = tx.send(ok);
        if !ok {
            return;
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        ST.with(|s| s.borrow_mut().take());
    });
    spawned.is_ok() && rx.recv().unwrap_or(false)
}

/// 拿掉圖示並結束那條執行緒。
pub fn stop() {
    if let Some(h) = TRAY.lock().unwrap().take() {
        unsafe {
            let _ = PostMessageW(Some(HWND(h.hwnd as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// 改滑鼠停在圖示上時的提示（正在播的歌）。圖示沒開就忽略。
pub fn set_tip(text: &str) {
    let mut g = TRAY.lock().unwrap();
    let Some(h) = g.as_mut() else { return };
    if h.tip == text {
        return;
    }
    h.tip = text.to_string();
    unsafe {
        let _ = PostMessageW(Some(HWND(h.hwnd as _)), WM_TIP, WPARAM(0), LPARAM(0));
    }
}

unsafe fn create(on_cmd: Box<dyn Fn(Cmd)>, menu: Box<dyn Fn() -> Vec<Item>>) -> bool {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return false };
        let hinst = windows::Win32::Foundation::HINSTANCE(module.0);
        let class = w!("VitroDeck.Tray");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc); // 第二次開圖示時類別已經在了，失敗也沒關係
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0), class, w!("VitroDeck"), WS_OVERLAPPED,
            0, 0, 0, 0, None, None, Some(hinst), None,
        ) else {
            return false;
        };
        // exe 裡嵌的圖示（build.rs 的 winresource，資源編號 1），小圖示的尺寸
        let (cx, cy) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        let icon = LoadImageW(Some(hinst), PCWSTR(1 as _), IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR)
            .map(|h| HICON(h.0))
            .or_else(|_| LoadIconW(None, IDI_APPLICATION))
            .unwrap_or_default();
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        ST.with(|s| *s.borrow_mut() = Some(State { on_cmd, menu, icon, taskbar_created }));
        *TRAY.lock().unwrap() = Some(Handle { hwnd: hwnd.0 as isize, tip: "VitroDeck".into() });
        if !notify(hwnd, NIM_ADD) {
            *TRAY.lock().unwrap() = None;
            let _ = DestroyWindow(hwnd);
            return false;
        }
        true
    }
}

/// 加上或更新圖示。提示文字從全域那份拿。
unsafe fn notify(hwnd: HWND, op: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE) -> bool {
    let icon = ST.with(|s| s.borrow().as_ref().map(|s| s.icon)).unwrap_or_default();
    let tip = TRAY.lock().unwrap().as_ref().map(|h| h.tip.clone()).unwrap_or_default();
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    // 提示最多 127 個 UTF-16 字元；太長就截掉，別讓它整段被拒
    for (dst, c) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *dst = c;
    }
    unsafe { Shell_NotifyIconW(op, &nid).as_bool() }
}

unsafe fn show_menu(hwnd: HWND) {
    unsafe {
        // 先把選單內容拿出來就放掉借用：TrackPopupMenu 會跑自己的訊息迴圈，
        // 期間 wndproc 還會被呼叫，借用沒放掉會 panic
        let items = ST.with(|s| s.borrow().as_ref().map(|s| (s.menu)())).unwrap_or_default();
        let Ok(menu) = CreatePopupMenu() else { return };
        let mut cmds = Vec::new();
        for it in &items {
            match it {
                None => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                }
                Some((text, checked, cmd)) => {
                    cmds.push(*cmd);
                    let flags = MF_STRING | if *checked == Some(true) { MF_CHECKED } else { MF_UNCHECKED };
                    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
                    let _ = AppendMenuW(menu, flags, cmds.len(), PCWSTR(wide.as_ptr()));
                }
            }
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // 不先拿前景的話，點選單外面選單不會收起來（Windows 的老規矩）
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(
            menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, hwnd, None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        let i = picked.0 as usize;
        if i >= 1 && i <= cmds.len() {
            fire(cmds[i - 1]);
        }
    }
}

fn fire(cmd: Cmd) {
    ST.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            (s.on_cmd)(cmd);
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAY => {
                match (lp.0 & 0xffff) as u32 {
                    WM_LBUTTONUP => fire(Cmd::Open),
                    WM_RBUTTONUP => show_menu(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_TIP => {
                notify(hwnd, NIM_MODIFY);
                LRESULT(0)
            }
            WM_CLOSE => {
                notify(hwnd, NIM_DELETE);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => {
                // explorer 重開了：圖示要重新加
                let tc = ST.with(|s| s.borrow().as_ref().map(|s| s.taskbar_created)).unwrap_or(0);
                if tc != 0 && msg == tc {
                    notify(hwnd, NIM_ADD);
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
    }
}
