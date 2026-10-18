//! 燈條（SkydimoBloom）的音量回報 —— Python 版 `audiofeed.py` 的 UDP 那一半。
//!
//! 協定只走 127.0.0.1:[`PORT`]，跟 Python 版逐 byte 相同：
//!
//! ```text
//! SkydimoBloom → 這裡   b"SKY1"                          每秒一次，當作訂閱
//! 這裡 → SkydimoBloom   pack("<4sBf", b"VDL1", playing, rms)
//!                       收到訂閱時回一包，擷取中每 1024 個取樣送一包
//! ```
//!
//! 超過 [`TTL`] 沒收到訂閱就當作退訂。拾音只看**本機**在播（SMTC）——
//! Spotify 放在別台裝置時 loopback 根本抓不到聲音。
//!
//! 全域一份：poller（播放狀態）、音訊擷取（RMS）、這裡的收訊執行緒三邊都要碰，
//! 用 `OnceLock` 比一路把 `Arc` 傳下去單純。

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 緊鄰 SkydimoBloom 的單一實例鎖 49573
pub const PORT: u16 = 49574;
const TTL: Duration = Duration::from_secs(3);

struct Led {
    sock: UdpSocket,
    /// 訂閱者與到期時刻
    sub: Mutex<Option<(SocketAddr, Instant)>>,
    playing: AtomicBool,
    last_rms: Mutex<f32>,
}

static LED: OnceLock<Led> = OnceLock::new();

/// `<4sBf`：標頭、播放中、RMS（little-endian f32），共 9 bytes
pub fn packet(playing: bool, rms: f32) -> [u8; 9] {
    let mut p = [0u8; 9];
    p[..4].copy_from_slice(b"VDL1");
    p[4] = playing as u8;
    p[5..].copy_from_slice(&rms.to_le_bytes());
    p
}

/// 綁埠並開收訊執行緒。綁不起來（另一份程式佔著）只影響燈條，回傳原因。
pub fn start() -> Result<(), String> {
    let sock = UdpSocket::bind(("127.0.0.1", PORT)).map_err(|e| e.to_string())?;
    let recv = sock.try_clone().map_err(|e| e.to_string())?;
    let led = Led { sock, sub: Mutex::new(None), playing: AtomicBool::new(false), last_rms: Mutex::new(0.0) };
    if LED.set(led).is_err() {
        return Err("已經啟動過".into());
    }
    std::thread::Builder::new()
        .name("ledfeed".into())
        .spawn(move || {
            let mut buf = [0u8; 64];
            loop {
                match recv.recv_from(&mut buf) {
                    Ok((n, addr)) if buf[..n].starts_with(b"SKY1") => {
                        let Some(led) = LED.get() else { continue };
                        *led.sub.lock().unwrap() = Some((addr, Instant::now() + TTL));
                        let playing = led.playing.load(Ordering::Relaxed);
                        let rms = if playing { *led.last_rms.lock().unwrap() } else { 0.0 };
                        let _ = led.sock.send_to(&packet(playing, rms), addr);
                    }
                    Ok(_) => {}
                    // 燈條那頭關掉後，Windows 會把 ICMP port unreachable 回報成
                    // 下一次 recv 的錯誤（WSAECONNRESET），忽略就好
                    Err(e) if e.raw_os_error() == Some(10054) => {}
                    Err(e) => {
                        crate::warn!("燈條收訊錯誤：{e}");
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn subscriber(led: &Led) -> Option<SocketAddr> {
    led.sub.lock().unwrap().filter(|(_, until)| Instant::now() < *until).map(|(a, _)| a)
}

/// 燈條要不要音量：有人訂閱而且本機正在播。音訊擷取靠這個決定要不要開。
pub fn wanted() -> bool {
    LED.get().is_some_and(|l| l.playing.load(Ordering::Relaxed) && subscriber(l).is_some())
}

/// 音訊擷取每收滿一塊就呼叫一次。
pub fn send_rms(rms: f32) {
    let Some(led) = LED.get() else { return };
    *led.last_rms.lock().unwrap() = rms;
    if let Some(addr) = subscriber(led) {
        let _ = led.sock.send_to(&packet(led.playing.load(Ordering::Relaxed), rms), addr);
    }
}

/// poller 告訴這裡本機有沒有在播。
pub fn set_playing(playing: bool) {
    let Some(led) = LED.get() else { return };
    if led.playing.swap(playing, Ordering::Relaxed) == playing {
        return;
    }
    if !playing {
        *led.last_rms.lock().unwrap() = 0.0;
        // 主動告訴燈條停了，它不必等逾時才退回綻放
        if let Some(addr) = subscriber(led) {
            let _ = led.sock.send_to(&packet(false, 0.0), addr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 封包跟_python_的_struct_pack_一樣() {
        // struct.pack("<4sBf", b"VDL1", 1, 0.25)
        assert_eq!(packet(true, 0.25), [b'V', b'D', b'L', b'1', 1, 0x00, 0x00, 0x80, 0x3e]);
    }
}
