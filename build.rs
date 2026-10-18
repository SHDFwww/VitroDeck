//! 把圖示與版本資訊嵌進 exe（檔案總管、工作列、內容 → 詳細資料看到的那些）。
//!
//! 需要 Windows SDK 的 rc.exe，winresource 會自己從登錄檔找到它。

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
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
