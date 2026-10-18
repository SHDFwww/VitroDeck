//! OAuth 的回呼接收端。
//!
//! Python 版是拿本來就要跑的靜態檔 server 順便收 `/callback`。原生版沒有
//! 那個 server（前端資源直接編進二進位），所以這裡只在登入那幾十秒開一個
//! 臨時的監聽器，收到就關掉。
//!
//! 埠號不能隨便挑：Spotify 只認註冊時填的那一個 redirect URI，而且從
//! 2025-04 起只放行 `http://127.0.0.1:PORT` 字面位址，`localhost` 會被拒。

use std::time::{Duration, Instant};

const PAGE_OK: &str = r#"<!doctype html><meta charset="utf-8">
<title>VitroDeck</title>
<style>body{background:#111;color:#eee;font:16px system-ui;display:grid;
place-items:center;height:100vh;margin:0}div{text-align:center}</style>
<div><h2>授權完成</h2><p>可以關掉這個分頁，回到 VitroDeck。</p></div>"#;

const PAGE_BAD: &str = r#"<!doctype html><meta charset="utf-8">
<title>VitroDeck</title>
<style>body{background:#111;color:#eee;font:16px system-ui;display:grid;
place-items:center;height:100vh;margin:0}div{text-align:center}</style>
<div><h2>授權沒有完成</h2><p>回到 VitroDeck 再試一次。</p></div>"#;

pub struct Callback {
    pub code: String,
    pub state: String,
}

/// 擋在這裡等使用者在瀏覽器上按下同意。逾時或使用者拒絕都回 `Err`。
pub fn wait_for_callback(port: u16, timeout: Duration) -> anyhow::Result<Callback> {
    let server = tiny_http::Server::http(("127.0.0.1", port))
        .map_err(|e| anyhow::anyhow!("埠 {port} 開不起來：{e}"))?;
    let deadline = Instant::now() + timeout;

    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            anyhow::bail!("等太久了，沒等到授權回呼");
        }
        let Some(req) = server.recv_timeout(left)? else { continue };

        // 只理 /callback，其他路徑一律 404 —— 這個監聽器不供任何檔案。
        let url = req.url().to_string();
        if !url.starts_with("/callback") {
            let _ = req.respond(tiny_http::Response::empty(404));
            continue;
        }

        let mut code = None;
        let mut state = None;
        let mut error = None;
        if let Some(q) = url.split_once('?').map(|(_, q)| q) {
            for pair in q.split('&') {
                let Some((k, v)) = pair.split_once('=') else { continue };
                let v = urlencoding::decode(v).unwrap_or_default().into_owned();
                match k {
                    "code" => code = Some(v),
                    "state" => state = Some(v),
                    "error" => error = Some(v),
                    _ => {}
                }
            }
        }

        let ok = code.is_some() && state.is_some() && error.is_none();
        let body = if ok { PAGE_OK } else { PAGE_BAD };
        let resp = tiny_http::Response::from_string(body).with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                .unwrap(),
        );
        let _ = req.respond(resp);

        if let Some(e) = error {
            anyhow::bail!("使用者或 Spotify 拒絕了授權：{e}");
        }
        if let (Some(code), Some(state)) = (code, state) {
            return Ok(Callback { code, state });
        }
        anyhow::bail!("回呼缺少 code 或 state");
    }
}

/// 用系統預設瀏覽器開授權頁。
///
/// 兩個坑都要擋，少一個網址就是壞的:
///
/// 1. `start` 的第一個引數會被當成**視窗標題**，所以要先塞一個空字串。
/// 2. **`&` 是 cmd 的命令分隔符。** 授權網址每個參數之間都是 `&`，
///    不加引號的話 cmd 只會把第一段當網址，後面的 `response_type=code`
///    被當成另一條命令去執行 —— 瀏覽器開得起來，但 Spotify 會回
///    「response_type must be code」。
///
/// Rust 只有在引數含空白時才會自動加引號，`&` 不算，所以這裡得自己拼
/// 整條命令列（`raw_arg` 不做任何跳脫）。網址是 base64 與百分號編碼，
/// 不可能出現 `"`，所以直接包起來是安全的。
pub fn open_in_browser(url: &str) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("cmd")
        .raw_arg(format!("/C start \"\" \"{url}\""))
        .spawn()
        .map(|_| ())
}
