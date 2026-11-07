//! 只錄 Spotify 自己的聲音，給比對歌詞內容（`verify`）用。
//!
//! 一般的 loopback 是整台電腦的混音：邊聽歌邊開實況時，whisper 寫下來的是實況主說的話，
//! 每個來源都對不上（2026-11-06 實機碰到）。WASAPI 的 process loopback 可以指定一個程序樹，
//! 其他程式的聲音進不來。錄不到時（Windows 太舊、Spotify 沒開在這台）照舊用混音。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};

static WANT: AtomicBool = AtomicBool::new(false);
/// 真的收到過 Spotify 的聲音（這次擷取）
static LIVE: AtomicBool = AtomicBool::new(false);

/// 要不要錄。工作執行緒每輪更新
pub fn set_want(on: bool) {
    WANT.store(on, Ordering::Relaxed);
}

/// 正在收到 Spotify 的聲音
pub fn live() -> bool {
    LIVE.load(Ordering::Relaxed)
}

pub fn start() {
    let _ = std::thread::Builder::new().name("spotcap".into()).spawn(|| {
        let _ = wasapi::initialize_mta();
        let mut warned = false;
        loop {
            if !WANT.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(300));
                continue;
            }
            if let Err(e) = capture()
                && !warned
            {
                warned = true;
                crate::warn!("錄不到 Spotify 自己的聲音，比對改用混音：{e}");
            }
            LIVE.store(false, Ordering::Relaxed);
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

fn capture() -> anyhow::Result<()> {
    let pid = spotify_pid().ok_or_else(|| anyhow::anyhow!("找不到 Spotify.exe"))?;
    let fmt = WaveFormat::new(32, 32, &SampleType::Float, 48_000, 2, None);
    let mut client = AudioClient::new_application_loopback_client(pid, true)?;
    client.initialize_client(&fmt, &Direction::Capture, &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 1_000_000 })?;
    let event = client.set_get_eventhandle()?;
    let cap = client.get_audiocaptureclient()?;
    client.start_stream()?;
    let mut raw: VecDeque<u8> = VecDeque::with_capacity(1 << 16);
    while WANT.load(Ordering::Relaxed) {
        // 沒在播時不給事件，那不是錯誤
        if event.wait_for_event(500).is_err() {
            continue;
        }
        let q = crate::alignrec::qpc_hns();
        loop {
            let before = raw.len();
            let info = cap.read_from_device_to_deque(&mut raw)?;
            if raw.len() == before {
                break;
            }
            if info.flags.silent {
                raw.clear();
            }
        }
        if raw.len() < 8 {
            continue;
        }
        let v: Vec<f32> = raw.make_contiguous().chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        raw.clear();
        LIVE.store(true, Ordering::Relaxed);
        super::spot_audio(q, v);
    }
    let _ = client.stop_stream();
    Ok(())
}

/// Spotify 的主程序：名字是 Spotify.exe，而且父程序不是。音訊在子程序裡，所以要錄整棵樹
fn spotify_pid() -> Option<u32> {
    let mut all: Vec<(u32, u32, String)> = Vec::new();
    // SAFETY: 快照用完就關，結構的 dwSize 依規定先填好
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let n = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
            all.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..n])));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    root_of(&all, "spotify.exe")
}

fn root_of(all: &[(u32, u32, String)], exe: &str) -> Option<u32> {
    let is = |n: &String| n.eq_ignore_ascii_case(exe);
    let parent_is = |ppid: u32| all.iter().any(|(p, _, n)| *p == ppid && is(n));
    all.iter().find(|(_, ppid, n)| is(n) && !parent_is(*ppid)).map(|x| x.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 主程序是父程序不是自己的那支() {
        let all = [
            (10, 1, "explorer.exe".to_string()),
            (21, 20, "Spotify.exe".to_string()),
            (20, 10, "Spotify.exe".to_string()),
            (22, 20, "Spotify.exe".to_string()),
        ];
        assert_eq!(root_of(&all, "spotify.exe"), Some(20));
        assert_eq!(root_of(&all, "nothing.exe"), None);
    }
}
