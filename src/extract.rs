//! `--extract-presets <vendor 目錄> <輸出目錄>`
//!
//! 把 butterchurn 的 preset 包從 webpack bundle 裡挖成純 JSON。一次性工具。
//!
//! 為什麼不直接解析：每個 preset 是一個獨立的 webpack 模組
//! （`function(a,e){a.exports={baseVals:{…}}}`），名稱對應表藏在模組圖裡，
//! 要拿到「名稱 → preset」得把整個模組系統重跑一遍。**執行它比解析它可靠。**
//!
//! 作法是起一個臨時 HTTP 伺服器，同源供應一個小網頁加那五個 bundle；
//! 網頁把 UMD 掛上去的全域物件 `JSON.stringify` 之後 POST 回來，Rust 這邊寫檔。
//! 這樣不需要 node，也不必把 3 MB 的資料從瀏覽器主控台複製出來。


use std::path::Path;

const PACKS: &[(&str, &str)] = &[
    ("base", "butterchurnPresets"),
    ("extra", "butterchurnPresetsExtra"),
    ("extra2", "butterchurnPresetsExtra2"),
    ("md1", "butterchurnPresetsMD1"),
    ("nonminimal", "butterchurnPresetsNonMinimal"),
];

fn file_of(pack: &str) -> String {
    match pack {
        "base" => "butterchurnPresets.min.js".into(),
        "extra" => "butterchurnPresetsExtra.min.js".into(),
        "extra2" => "butterchurnPresetsExtra2.min.js".into(),
        "md1" => "butterchurnPresetsMD1.min.js".into(),
        _ => "butterchurnPresetsNonMinimal.min.js".into(),
    }
}

fn page() -> String {
    let mut scripts = String::new();
    let mut calls = String::new();
    for (pack, global) in PACKS {
        scripts.push_str(&format!("<script src=\"/js/{pack}\"></script>\n"));
        calls.push_str(&format!(
            "await dump('{pack}', window.{global} || window['{global}']);\n"
        ));
    }
    format!(
        r#"<!doctype html><meta charset="utf-8"><title>preset 萃取</title>
<style>body{{background:#111;color:#ddd;font:15px system-ui;padding:24px}}
pre{{white-space:pre-wrap}}</style>
<h3>正在萃取 butterchurn preset…</h3><pre id="log"></pre>
{scripts}
<script>
const log = (s) => {{ document.getElementById('log').textContent += s + "\n"; }};
async function dump(pack, obj) {{
  if (!obj) {{ log(pack + "：找不到全域物件"); return; }}
  // UMD 有時候會包一層 default
  // UMD 匯出的是一個函式，preset 要透過 getPresets() 拿 —— 不是直接的物件。
  const api = obj.default || obj;
  if (typeof api.getPresets !== 'function') {{ log(pack + "：沒有 getPresets"); return; }}
  const data = api.getPresets();
  const n = Object.keys(data).length;
  const body = JSON.stringify(data);
  const r = await fetch('/save/' + pack, {{ method: 'POST', body }});
  log(pack + "：" + n + " 個 preset，" + body.length + " bytes → " + (r.ok ? "已寫入" : "失敗"));
}}
(async () => {{
  {calls}
  log("完成。可以關掉了。");
  await fetch('/done', {{ method: 'POST', body: 'ok' }});
}})();
</script>"#
    )
}

pub fn run(vendor: &Path, out: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(out)?;
    let server = tiny_http::Server::http("127.0.0.1:8899")
        .map_err(|e| anyhow::anyhow!("開不了 127.0.0.1:8899：{e}"))?;
    println!("萃取伺服器起來了：http://127.0.0.1:8899/");
    println!("用瀏覽器打開它，抓完會自己結束。");

    let mut saved = 0usize;
    loop {
        let mut req: tiny_http::Request = match server.recv() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("recv: {e}");
                continue;
            }
        };
        let url = req.url().to_string();
        let method = req.method().as_str().to_string();

        if url == "/" {
            let body = page();
            let resp = tiny_http::Response::from_string(body).with_header(
                tiny_http::Header::from_bytes(
                    &b"Content-Type"[..],
                    &b"text/html; charset=utf-8"[..],
                )
                .unwrap(),
            );
            let _ = req.respond(resp);
            continue;
        }

        if let Some(pack) = url.strip_prefix("/js/") {
            let path = vendor.join(file_of(pack));
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let resp = tiny_http::Response::from_data(bytes).with_header(
                        tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"application/javascript"[..],
                        )
                        .unwrap(),
                    );
                    let _ = req.respond(resp);
                }
                Err(e) => {
                    eprintln!("讀不到 {}：{e}", path.display());
                    let _ = req.respond(tiny_http::Response::empty(404));
                }
            }
            continue;
        }

        if method == "POST" {
            if let Some(pack) = url.strip_prefix("/save/") {
                let pack = pack.to_string();
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let dest = out.join(format!("{pack}.json"));
                match std::fs::write(&dest, body.as_bytes()) {
                    Ok(()) => {
                        saved += 1;
                        let n = count_presets(&body);
                        println!(
                            "  {pack:<12} {n:>4} 個 preset  {:>9} bytes → {}",
                            body.len(),
                            dest.display()
                        );
                    }
                    Err(e) => eprintln!("寫不進 {}：{e}", dest.display()),
                }
                let _ = req.respond(tiny_http::Response::from_string("ok"));
                continue;
            }
            if url == "/done" {
                let _ = req.respond(tiny_http::Response::from_string("bye"));
                println!("\n全部完成，共 {saved} 包。");
                return Ok(());
            }
        }

        let _ = req.respond(tiny_http::Response::empty(404));
    }
}

/// 只數頂層的 key，不做完整剖析 —— 只是給人看的進度數字。
fn count_presets(json: &str) -> usize {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.as_object().map(|o| o.len()))
        .unwrap_or(0)
}
