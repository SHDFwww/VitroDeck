//! 從 `assets/icon.svg` 重新產生圖示：
//!
//! - `icon.ico`：exe、視窗、系統列用。每個尺寸各自從向量畫，不拿 256 縮小
//! - `icon.png`：256px，程式執行時載入（視窗圖示）
//! - `icon_1024.png`：上傳到 Discord 開發者後台當 Rich Presence 的預設大圖（不進 exe）
//!
//! 32px 以下拿掉倒影和光暈：只剩一兩個像素高，畫出來是糊的一團。
//!
//!     cargo run --example make_icon

use std::path::Path;

const ICO_SIZES: [u32; 10] = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];
const SMALL: u32 = 32;

fn main() -> anyhow::Result<()> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let full = std::fs::read_to_string(dir.join("icon.svg"))?;
    let small = strip_detail(&full)?;

    let mut frames = Vec::new();
    for px in ICO_SIZES {
        frames.push((px, png(if px <= SMALL { &small } else { &full }, px)?));
    }
    std::fs::write(dir.join("icon.ico"), ico(&frames))?;
    std::fs::write(dir.join("icon.png"), &frames.last().unwrap().1)?;
    std::fs::write(dir.join("icon_1024.png"), png(&full, 1024)?)?;
    println!("icon.ico（{} 個尺寸）、icon.png、icon_1024.png 已更新", frames.len());
    Ok(())
}

/// 拿掉倒影與光暈，卡片移回正中間（大圖往上挪了 8 讓位給倒影）
fn strip_detail(svg: &str) -> anyhow::Result<String> {
    let start = svg.find(r#"<g id="reflection">"#).ok_or_else(|| anyhow::anyhow!("找不到倒影群組"))?;
    let cards = r#"<g id="cards">"#;
    let end = svg.find(cards).filter(|&e| e > start).ok_or_else(|| anyhow::anyhow!("倒影後面要接卡片群組"))?;
    let out = format!("{}{}", &svg[..start], &svg[end..]);
    let glow = r#"<rect width="256" height="256" fill="url(#glow)"/>"#;
    anyhow::ensure!(out.contains(glow), "找不到光暈");
    Ok(out.replace(glow, "").replace(cards, r#"<g id="cards" transform="translate(0 8)">"#))
}

fn png(svg: &str, px: u32) -> anyhow::Result<Vec<u8>> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())?;
    let mut pm = resvg::tiny_skia::Pixmap::new(px, px).ok_or_else(|| anyhow::anyhow!("建不出 {px}px 的畫布"))?;
    let s = px as f32 / tree.size().width();
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pm.as_mut());
    // tiny-skia 是預乘 alpha，PNG 要直通
    let raw: Vec<u8> = pm
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    let img = image::RgbaImage::from_raw(px, px, raw).ok_or_else(|| anyhow::anyhow!("像素數不對"))?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}

/// 每一格都直接內嵌 PNG（Vista 起支援）。寬高欄位 0 代表 256
fn ico(frames: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * frames.len() as u32;
    for (px, data) in frames {
        let side = if *px >= 256 { 0 } else { *px as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in frames {
        out.extend_from_slice(data);
    }
    out
}
