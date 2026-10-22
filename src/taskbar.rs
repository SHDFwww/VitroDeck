//! 主視窗工作列按鈕上的播放進度（`ITaskbarList3`），跟瀏覽器下載檔案時那條一樣：
//! 播放中是一般的綠色，暫停是黃色，沒在播或設定關掉就拿掉。
//! 按鈕上的字就是視窗標題，設定開著時換成「歌手 - 歌名」（[`Taskbar::title`]）。
//!
//! 按鈕上的進度不會自己走，要一直送。主視窗縮小時停畫，但事件迴圈還是每 `PARK_POLL`
//! 醒一次（`about_to_wait`），所以縮著的時候進度照樣會動。

use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::UI::Shell::{ITaskbarList3, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED, TaskbarList};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

/// 只有位置在走的時候多久送一次。按鈕才幾十 px 寬，一首歌一秒動不到一格；
/// Explorer 重開、或從系統列叫回來時按鈕是新的、進度被清掉，也靠這個補回來
const EVERY: Duration = Duration::from_secs(1);

/// 按鈕上要顯示什麼
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Show {
    Off,
    /// 播放中（綠色）。位置與長度都是毫秒
    Play { pos: u64, len: u64 },
    /// 暫停（黃色）
    Pause { pos: u64, len: u64 },
}

impl Show {
    /// `pos_ms` 是內插過的播放位置。長度不明（廣告、剛換歌還沒拿到）就不顯示
    pub fn of(on: bool, playing: bool, pos_ms: i64, len_ms: i64) -> Self {
        if !on || len_ms <= 0 {
            return Show::Off;
        }
        let (pos, len) = (pos_ms.clamp(0, len_ms) as u64, len_ms as u64);
        if playing { Show::Play { pos, len } } else { Show::Pause { pos, len } }
    }
}

/// 該不該送：顏色（播放／暫停／拿掉）變了馬上送，沒變的話一秒送一次；拿掉之後不再重送
fn due(sent: Option<(Show, Instant)>, show: Show, now: Instant) -> bool {
    match sent {
        None => show != Show::Off,
        Some((last, at)) => {
            std::mem::discriminant(&last) != std::mem::discriminant(&show) || (show != Show::Off && now - at >= EVERY)
        }
    }
}

#[derive(Default)]
pub struct Taskbar {
    list: Option<ITaskbarList3>,
    /// 建不起來（Explorer 沒在跑之類）就不再試
    broken: bool,
    /// 上次送出的與送的時間
    sent: Option<(Show, Instant)>,
    /// 上次換標題時的（設定、簡轉繁、歌手、歌名）。設定關著時一律記成 (false, false, "", "")
    titled: Option<(bool, bool, String, String)>,
}

impl Taskbar {
    /// 每次事件迴圈醒來都可以叫，要不要真的送由 [`due`] 決定
    pub fn update(&mut self, window: &Window, show: Show) {
        let now = Instant::now();
        if !due(self.sent, show, now) {
            return;
        }
        let Some(hwnd) = hwnd_of(window) else { return };
        if self.list.is_none() && !self.broken {
            self.list = create();
            self.broken = self.list.is_none();
        }
        let Some(list) = &self.list else { return };
        // 按鈕還沒建好、或視窗縮在系統列時會失敗，不用管：一秒後再送一次
        unsafe {
            match show {
                Show::Off => {
                    let _ = list.SetProgressState(hwnd, TBPF_NOPROGRESS);
                }
                // 先給值再給顏色：沒有進度的按鈕收到值會變成綠色，暫停的黃色要在它後面設
                Show::Play { pos, len } => {
                    let _ = list.SetProgressValue(hwnd, pos, len);
                    let _ = list.SetProgressState(hwnd, TBPF_NORMAL);
                }
                Show::Pause { pos, len } => {
                    let _ = list.SetProgressValue(hwnd, pos, len);
                    let _ = list.SetProgressState(hwnd, TBPF_PAUSED);
                }
            }
        }
        self.sent = Some((show, now));
    }

    /// 視窗標題要換成什麼，跟上次一樣就 `None`。每幀都會叫，所以先比原文，
    /// 有變才轉繁體、組字串（轉繁體要查字典）。設定關著時換歌不算變
    pub fn title(&mut self, on: bool, zh: bool, artist: &str, title: &str) -> Option<String> {
        let key = if on { (true, zh, artist, title) } else { (false, false, "", "") };
        if self.titled.as_ref().is_some_and(|(o, z, a, t)| (*o, *z, a.as_str(), t.as_str()) == key) {
            return None;
        }
        self.titled = Some((key.0, key.1, key.2.to_string(), key.3.to_string()));
        Some(label(on, zh, artist, title))
    }
}

/// 工作列按鈕上的字：設定開著、有歌時是「歌手 - 歌名」（跟畫面上一樣照 `zh` 轉繁體），其他時候是 VitroDeck
fn label(on: bool, zh: bool, artist: &str, title: &str) -> String {
    let (artist, title) = (artist.trim(), title.trim());
    if !on || title.is_empty() {
        return "VitroDeck".into();
    }
    let show = |s: &str| zh.then(|| crate::zh::to_trad(s)).flatten().unwrap_or_else(|| s.to_string());
    if artist.is_empty() { show(title) } else { format!("{} - {}", show(artist), show(title)) }
}

fn hwnd_of(window: &Window) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
        _ => None,
    }
}

fn create() -> Option<ITaskbarList3> {
    unsafe {
        // winit 開拖放時已經用 OleInitialize 把主執行緒設成 STA，再叫一次只會回 S_FALSE
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let list: ITaskbarList3 = CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER).ok()?;
        list.HrInit().ok()?;
        Some(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 長度不明或關掉就不顯示_位置夾在長度內() {
        assert_eq!(Show::of(false, true, 1000, 200_000), Show::Off);
        assert_eq!(Show::of(true, true, 1000, 0), Show::Off);
        assert_eq!(Show::of(true, true, 250_000, 200_000), Show::Play { pos: 200_000, len: 200_000 });
        assert_eq!(Show::of(true, false, -50, 200_000), Show::Pause { pos: 0, len: 200_000 });
    }

    #[test]
    fn 顏色變了馬上送_位置一秒送一次_拿掉不重送() {
        let t = Instant::now();
        let play = |pos| Show::Play { pos, len: 100_000 };
        assert!(!due(None, Show::Off, t), "一開始本來就沒有進度");
        assert!(due(None, play(0), t));
        let sent = Some((play(0), t));
        assert!(!due(sent, play(300), t + Duration::from_millis(300)));
        assert!(due(sent, play(1000), t + EVERY));
        assert!(due(sent, Show::Pause { pos: 300, len: 100_000 }, t + Duration::from_millis(300)), "暫停要馬上變黃");
        assert!(due(sent, Show::Off, t + Duration::from_millis(10)));
        let off = Some((Show::Off, t));
        assert!(!due(off, Show::Off, t + EVERY * 5));
    }

    #[test]
    fn 標題是歌手與歌名_關掉或沒歌是程式名() {
        assert_eq!(label(true, false, "周杰伦", "晴天"), "周杰伦 - 晴天");
        assert_eq!(label(true, true, "周杰伦", "晴天"), "周杰倫 - 晴天");
        // 沒有歌手（像某些 podcast）就只放歌名
        assert_eq!(label(true, true, " ", "第 12 集"), "第 12 集");
        assert_eq!(label(true, true, "A", ""), "VitroDeck");
        assert_eq!(label(false, true, "周杰伦", "晴天"), "VitroDeck");
    }

    #[test]
    fn 標題有變才換() {
        let mut tb = Taskbar::default();
        assert_eq!(tb.title(true, true, "A", "B").as_deref(), Some("A - B"));
        assert_eq!(tb.title(true, true, "A", "B"), None);
        assert_eq!(tb.title(true, true, "A", "C").as_deref(), Some("A - C"));
        assert_eq!(tb.title(false, true, "A", "C").as_deref(), Some("VitroDeck"));
        assert_eq!(tb.title(false, true, "A", "D"), None, "關著時換歌不用重設");
        assert_eq!(tb.title(true, true, "A", "D").as_deref(), Some("A - D"));
    }
}
