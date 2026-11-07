//! Discord 個人狀態上的「正在聽」（Rich Presence）。
//!
//! 走 Discord 用戶端自己開的本機管道 `\\.\pipe\discord-ipc-N`，不經過任何 VitroDeck 的伺服器：
//! 送出去的只有歌名、歌手、專輯、封面網址與播放時間，收的是使用者自己的 Discord。
//! 封面用 Spotify 給的公開網址（i.scdn.co），Discord 會自己代抓；拿不到就用開發者後台上傳的 [`ICON`]。
//!
//! 設定關著時不開執行緒、不碰管道。程式結束時管道一斷，Discord 會自己把狀態拿掉。
//!
//! Discord 同時有好幾個程式在送狀態時只顯示一個。掛著狀態的期間開著具名 mutex [`MARKER`]，
//! 別的 Rich Presence 程式查得到就可以讓位；程式結束或當掉時系統會自己收掉它。

use std::fs::File;
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Discord 開發者後台的應用程式 ID。狀態上「正在聽 VitroDeck」的名字就是那邊設的
const APP_ID: &str = "1564640592980476037";
/// 後台 Rich Presence → Art Assets 上傳的圖（assets/icon_1024.png）
const ICON: &str = "icon";
/// Discord 限制每 20 秒最多 5 次，平均一次 4 秒。連按下一首時只送停下來那首
const GAP: Duration = Duration::from_secs(4);
/// Discord 沒開：多久再找一次管道
const RETRY: Duration = Duration::from_secs(15);
/// 連著的時候多久看一次管道還在不在。Discord 關掉重開，要靠這個把狀態補回去
const ALIVE: Duration = Duration::from_secs(15);
/// 換歌後最多等封面網址多久（曲目資訊要等 `META_SETTLE` 才問），等不到就先用圖示
const ART_WAIT: Duration = Duration::from_secs(4);
/// 推算出的開始時間跟上次送的差多少才重送（跳轉、播放位置校正）
const DRIFT_MS: i64 = 2000;
/// 掛著狀態的期間存在。只看有沒有，不拿來鎖東西
const MARKER: windows::core::PCWSTR = windows::core::w!(r"Local\VitroDeckDiscordPresence");

/// 正在播的那首
pub struct Track<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    /// 封面的公開網址
    pub art: Option<&'a str>,
    /// 封面網址還在問（換歌後幾秒內）
    pub art_pending: bool,
    pub playing: bool,
    pub pos_ms: i64,
    pub len_ms: i64,
    /// 跟畫面一樣轉繁體（`lyrics_traditional`）。真的要送時才轉，不是每幀
    pub zh: bool,
}

/// 送到 Discord 的那一筆
#[derive(Clone, Debug, PartialEq)]
struct Activity {
    title: String,
    artist: String,
    album: String,
    art: Option<String>,
    /// 播放中：（開始, 結束）的 Unix 毫秒，Discord 拿來畫進度條。暫停時沒有
    span: Option<(i64, i64)>,
}

/// 上次交給執行緒的，拿來判斷要不要重送（不用每幀配置字串）
struct Sent {
    id: String,
    playing: bool,
    art: Option<String>,
    start: i64,
    zh: bool,
}

pub struct Presence {
    tx: Sender<Option<Activity>>,
    /// `None` = 送過「沒在播」或還沒送過
    sent: Option<Sent>,
    /// 換歌但還在等封面：（那首的 id, 開始等的時刻）
    waiting: Option<(String, Instant)>,
    cleared: bool,
}

impl Presence {
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new().name("discord".into()).spawn(move || run(rx));
        Self { tx, sent: None, waiting: None, cleared: true }
    }

    /// 每次事件迴圈醒來都可以叫，沒變就什麼都不做
    pub fn update(&mut self, track: Option<Track>) {
        let Some(t) = track.filter(|t| !t.title.trim().is_empty()) else {
            self.sent = None;
            self.waiting = None;
            if !self.cleared {
                self.cleared = true;
                let _ = self.tx.send(None);
            }
            return;
        };
        let start = unix_ms() - t.pos_ms.clamp(0, t.len_ms.max(0));
        if let Some(s) = &self.sent
            && s.id == t.id
            && s.playing == t.playing
            && s.art.as_deref() == t.art
            && s.zh == t.zh
            && (!t.playing || (s.start - start).abs() < DRIFT_MS)
        {
            return;
        }
        // 新的一首、封面還在問：先等一下，免得先送圖示、隔幾秒又換成封面（多吃一次配額還會閃）
        let new_track = self.sent.as_ref().is_none_or(|s| s.id != t.id);
        if new_track && t.art.is_none() && t.art_pending {
            match &self.waiting {
                Some((id, at)) if id == t.id => {
                    if at.elapsed() < ART_WAIT {
                        return;
                    }
                }
                _ => {
                    self.waiting = Some((t.id.to_string(), Instant::now()));
                    return;
                }
            }
        }
        self.waiting = None;
        self.sent = Some(Sent { id: t.id.to_string(), playing: t.playing, art: t.art.map(str::to_string), start, zh: t.zh });
        self.cleared = false;
        let span = (t.playing && t.len_ms > 0).then_some((start, start + t.len_ms));
        let show = |s: &str| t.zh.then(|| crate::zh::to_trad(s)).flatten().unwrap_or_else(|| s.to_string());
        let _ = self.tx.send(Some(Activity {
            title: show(t.title),
            artist: show(t.artist),
            album: show(t.album),
            art: t.art.map(str::to_string),
            span,
        }));
    }
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// 背景執行緒：連管道、照節奏送、Discord 重開時補送。`Presence` 被丟掉（設定關掉）就清空狀態離開
fn run(rx: Receiver<Option<Activity>>) {
    let mut conn: Option<Conn> = None;
    let mut want: Option<Activity> = None;
    let mut dirty = false;
    let mut last_send: Option<Instant> = None;
    let mut next_try = Instant::now();
    let mut next_alive = Instant::now() + ALIVE;
    let mut marker = Marker(None);
    loop {
        let now = Instant::now();
        let wake = if !dirty {
            next_alive
        } else if conn.is_none() {
            next_try
        } else {
            last_send.map_or(now, |t| t + GAP)
        };
        match rx.recv_timeout(wake.saturating_duration_since(now)) {
            Ok(a) => {
                want = a;
                while let Ok(a) = rx.try_recv() {
                    want = a;
                }
                dirty = true;
                // 先豎旗子再送：讓位的那邊每幾秒才看一次，越早豎越早換過來
                marker.set(want.is_some());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(mut c) = conn {
                    let _ = c.set(None);
                }
                return;
            }
        }
        let now = Instant::now();
        if let Some(c) = &conn
            && now >= next_alive
        {
            next_alive = now + ALIVE;
            if !c.alive() {
                conn = None;
                dirty = true;
            }
        }
        if !dirty {
            continue;
        }
        if conn.is_none() {
            if now < next_try {
                continue;
            }
            conn = Conn::open();
            if conn.is_none() {
                next_try = now + RETRY;
                continue;
            }
            next_alive = now + ALIVE;
        }
        if last_send.is_some_and(|t| now - t < GAP) {
            continue;
        }
        let Some(c) = conn.as_mut() else { continue };
        last_send = Some(now);
        match c.set(want.as_ref()) {
            Ok(()) => dirty = false,
            // 管道斷了：重連後再送
            Err(e) => {
                crate::warn!("Discord 狀態送不出去：{e}");
                conn = None;
                next_try = now;
            }
        }
    }
}

/// [`MARKER`] 的 handle。丟掉就關
struct Marker(Option<windows::Win32::Foundation::HANDLE>);

impl Marker {
    fn set(&mut self, on: bool) {
        use windows::Win32::System::Threading::CreateMutexW;
        match (on, self.0) {
            (true, None) => self.0 = unsafe { CreateMutexW(None, false, MARKER) }.ok(),
            (false, Some(_)) => *self = Marker(None),
            _ => {}
        }
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(h) };
        }
    }
}

struct Conn {
    pipe: File,
    nonce: u64,
}

impl Conn {
    /// Discord 開著的話管道是 0 號；同時開了測試版之類才會往後排
    fn open() -> Option<Self> {
        for i in 0..10 {
            let Ok(pipe) = std::fs::OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{i}"))
            else {
                continue;
            };
            let mut c = Self { pipe, nonce: 0 };
            match c.handshake() {
                Ok(()) => return Some(c),
                Err(e) => crate::warn!("Discord 握手失敗（discord-ipc-{i}）：{e}"),
            }
        }
        None
    }

    fn handshake(&mut self) -> anyhow::Result<()> {
        self.write(0, &json!({ "v": 1, "client_id": APP_ID }))?;
        let (op, v) = self.read()?;
        anyhow::ensure!(op == 1 && v["evt"] == "READY", "{v}");
        Ok(())
    }

    fn set(&mut self, a: Option<&Activity>) -> anyhow::Result<()> {
        self.nonce += 1;
        let nonce = self.nonce.to_string();
        self.write(
            1,
            &json!({
                "cmd": "SET_ACTIVITY",
                "args": { "pid": std::process::id(), "activity": a.map(activity_json) },
                "nonce": nonce,
            }),
        )?;
        // 回應一定會回來；中間夾著別的訊息就跳過
        loop {
            let (op, v) = self.read()?;
            anyhow::ensure!(op != 2, "Discord 關閉了連線：{v}");
            if v["nonce"] == nonce.as_str() {
                if v["evt"] == "ERROR" {
                    // 欄位被拒（例如字太長）不是連線問題，記下來就好，不重連
                    crate::warn!("Discord 不收這筆狀態：{}", v["data"]);
                }
                return Ok(());
            }
        }
    }

    /// 管道另一頭關了沒（Discord 結束）。不會讀走任何東西
    fn alive(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        let h = windows::Win32::Foundation::HANDLE(self.pipe.as_raw_handle());
        unsafe { windows::Win32::System::Pipes::PeekNamedPipe(h, None, 0, None, None, None).is_ok() }
    }

    fn write(&mut self, op: u32, v: &Value) -> std::io::Result<()> {
        let body = v.to_string();
        let mut buf = Vec::with_capacity(8 + body.len());
        buf.extend_from_slice(&op.to_le_bytes());
        buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
        buf.extend_from_slice(body.as_bytes());
        self.pipe.write_all(&buf)
    }

    fn read(&mut self) -> anyhow::Result<(u32, Value)> {
        let mut head = [0u8; 8];
        self.pipe.read_exact(&mut head)?;
        let op = u32::from_le_bytes(head[..4].try_into()?);
        let len = u32::from_le_bytes(head[4..].try_into()?) as usize;
        anyhow::ensure!(len <= 1 << 20, "回應太大（{len} bytes）");
        let mut body = vec![0u8; len];
        self.pipe.read_exact(&mut body)?;
        Ok((op, serde_json::from_slice(&body)?))
    }
}

/// 狀態的欄位。Listening 的版面：第一行歌名、第二行歌手（暫停時前面標 Paused）、
/// 大圖封面、滑過大圖是專輯名，播放中有進度條
fn activity_json(a: &Activity) -> Value {
    let state = if a.span.is_some() || a.artist.trim().is_empty() {
        a.artist.clone()
    } else {
        format!("Paused · {}", a.artist)
    };
    let state = if state.trim().is_empty() { "Paused".to_string() } else { state };
    let mut assets = json!({ "large_image": a.art.as_deref().unwrap_or(ICON) });
    if let Some(album) = text(&a.album) {
        assets["large_text"] = album.into();
    }
    let mut v = json!({
        "type": 2,
        "details": text(&a.title).unwrap_or_else(|| "VitroDeck".into()),
        "assets": assets,
        "buttons": [{ "label": "VitroDeck", "url": crate::config::GUIDE_URL }],
        "instance": false,
    });
    if let Some(s) = text(&state) {
        v["state"] = s.into();
    }
    if let Some((start, end)) = a.span {
        v["timestamps"] = json!({ "start": start, "end": end });
    }
    v
}

/// Discord 的字串欄位要 2～128 字，超過整筆會被拒。一個字的歌名補一個空白點字（會被當成字，不會被修剪掉）
fn text(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut out: String = if s.chars().count() > 128 {
        s.chars().take(127).chain(['…']).collect()
    } else {
        s.to_string()
    };
    if out.chars().count() < 2 {
        out.push('\u{2800}');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(playing: bool) -> Activity {
        Activity {
            title: "曲".into(),
            artist: "歌手".into(),
            album: "".into(),
            art: None,
            span: playing.then_some((1000, 5000)),
        }
    }

    #[test]
    fn 暫停時標上_paused_也沒有進度條() {
        let v = activity_json(&act(false));
        assert_eq!(v["state"], "Paused · 歌手");
        assert!(v.get("timestamps").is_none());
        let v = activity_json(&act(true));
        assert_eq!(v["state"], "歌手");
        assert_eq!(v["timestamps"]["end"], 5000);
        assert_eq!(v["type"], 2);
    }

    #[test]
    fn 沒封面用圖示_沒專輯不放滑過文字() {
        let v = activity_json(&act(true));
        assert_eq!(v["assets"]["large_image"], ICON);
        assert!(v["assets"].get("large_text").is_none());
    }

    /// 會真的改到 Discord 狀態：`cargo test --release discord_實機 -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn discord_實機() {
        let mut c = Conn::open().expect("Discord 沒開");
        // 讓別的 Rich Presence 程式看得到、讓位
        let mut marker = Marker(None);
        marker.set(true);
        let now = unix_ms();
        let cases = [
            Activity {
                title: "測試歌名".into(),
                artist: "測試歌手".into(),
                album: "測試專輯".into(),
                art: Some("https://i.scdn.co/image/ab67616d0000b2736f91aef5dc9be016c559f106".into()),
                span: Some((now - 30_000, now + 150_000)),
            },
            Activity { title: "測試歌名".into(), artist: "測試歌手".into(), album: "".into(), art: None, span: None },
        ];
        for (i, a) in cases.iter().enumerate() {
            c.nonce += 1;
            c.write(1, &json!({ "cmd": "SET_ACTIVITY", "args": { "pid": std::process::id(), "activity": activity_json(a) }, "nonce": c.nonce.to_string() }))
                .unwrap();
            let (_, v) = c.read().unwrap();
            println!("第 {i} 筆回應：{v}");
            assert_ne!(v["evt"], "ERROR");
            assert_eq!(v["data"]["type"], 2, "Discord 沒收 Listening");
            std::thread::sleep(Duration::from_secs(if i == 0 { 20 } else { 15 }));
        }
        c.set(None).unwrap();
    }

    #[test]
    fn 標記開關() {
        use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
        let exists = || unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, MARKER) }.map(|h| unsafe { windows::Win32::Foundation::CloseHandle(h) }).is_ok();
        let mut m = Marker(None);
        assert!(!exists());
        m.set(true);
        m.set(true);
        assert!(exists());
        m.set(false);
        assert!(!exists());
        m.set(true);
        drop(m);
        assert!(!exists());
    }

    #[test]
    fn 字數限制() {
        assert_eq!(text("A").unwrap().chars().count(), 2);
        assert_eq!(text("  "), None);
        let long = "字".repeat(300);
        let t = text(&long).unwrap();
        assert_eq!(t.chars().count(), 128);
        assert!(t.ends_with('…'));
        assert_eq!(text(&"字".repeat(128)).unwrap(), "字".repeat(128));
    }

    fn track<'a>(id: &'a str, playing: bool, pos: i64, art: Option<&'a str>, pending: bool) -> Track<'a> {
        Track { id, title: "歌", artist: "人", album: "專輯", art, art_pending: pending, playing, pos_ms: pos, len_ms: 200_000, zh: false }
    }

    #[test]
    fn 沒變不重送_換歌等封面() {
        let (tx, rx) = mpsc::channel();
        let mut p = Presence { tx, sent: None, waiting: None, cleared: true };
        p.update(Some(track("a", true, 1000, Some("https://x/a"), false)));
        assert!(rx.try_recv().is_ok());
        // 位置照常往前走：推算出的開始時間不變
        p.update(Some(track("a", true, 1000, Some("https://x/a"), false)));
        assert!(rx.try_recv().is_err());
        // 暫停
        p.update(Some(track("a", false, 1000, Some("https://x/a"), false)));
        assert!(rx.try_recv().unwrap().unwrap().span.is_none());
        // 換歌、封面還在問：先不送
        p.update(Some(track("b", true, 0, None, true)));
        assert!(rx.try_recv().is_err());
        // 封面到了
        p.update(Some(track("b", true, 0, Some("https://x/b"), false)));
        assert_eq!(rx.try_recv().unwrap().unwrap().art.as_deref(), Some("https://x/b"));
        // 停播
        p.update(None);
        assert!(rx.try_recv().unwrap().is_none());
        p.update(None);
        assert!(rx.try_recv().is_err());
    }
}
