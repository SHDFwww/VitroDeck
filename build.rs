//! 把圖示與版本資訊嵌進 exe（檔案總管、工作列、內容 → 詳細資料看到的那些），
//! 並把這一版的 release notes 整理好嵌進去（首次啟動新版時彈一次，見 `src/scene_notes.rs`）。
//!
//! 需要 Windows SDK 的 rc.exe，winresource 會自己從登錄檔找到它。

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    notes();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico")
        .set("FileDescription", "VitroDeck（原生版）")
        .set("ProductName", "VitroDeck");
    if let Err(e) = res.compile() {
        // 沒有 rc.exe 也要編得過，只是 exe 沒有圖示
        println!("cargo:warning=圖示沒有嵌進去：{e}");
    }
}

/// 讀 `RELEASE_NOTES_v<版本>.md`，只留條列（去掉標題、`<details>` 以後的校驗碼與聲明），
/// 一行一條寫到 `OUT_DIR/release_notes.txt`。沒有那份檔案就寫空的（程式不彈窗）。
/// 內容沒變就不重寫，免得每次都把主程式重編一遍。
fn notes() {
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let src = std::path::PathBuf::from(format!("RELEASE_NOTES_v{ver}.md"));
    println!("cargo:rerun-if-changed={}", src.display());
    let body = std::fs::read_to_string(&src).unwrap_or_default();
    let mut out = String::new();
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("<details") {
            break;
        }
        let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) else { continue };
        out.push_str(&item.replace("**", "").replace('`', ""));
        out.push('\n');
    }
    let dst = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default()).join("release_notes.txt");
    if std::fs::read_to_string(&dst).ok().as_deref() != Some(out.as_str()) {
        let _ = std::fs::write(&dst, out);
    }
}
