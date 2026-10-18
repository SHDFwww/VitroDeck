//! 按鈕圖示 —— 直接把 Python 版 `index.html` 裡的 SVG 點陣化。
//!
//! 原本用 SDF 方塊與字型符號（⤭ ⟲ ⚙）拼，形狀跟 Python 版對不起來，
//! 而且字型符號的粗細、基線每台電腦不一樣。這裡的路徑是照抄的，
//! 筆畫規則照 CSS 的 `.barbtn svg`：viewBox 24、筆寬 1.9、圓頭圓角；
//! 播放、上一首、下一首是實心（`fill: currentColor; stroke-width: 1.7`）。
//!
//! 點陣化成白色遮罩，顏色交給矩形的 `color` 乘上去（`quad.wgsl` 的貼圖是「材質 × color」），
//! 所以同一張可以畫成灰、白、綠、深色。照實際像素大小點陣化，不靠縮放，邊緣才是銳利的。

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Svg {
    Shuffle,
    Prev,
    Play,
    Pause,
    Next,
    Repeat,
    Heart,
    HeartFilled,
    Device,
    /// 音量 ≥ 50：兩道聲波
    Vol,
    /// 音量 1–49：一道聲波（`.js-vol-btn.low`）
    VolLow,
    /// 靜音：打叉（`.js-vol-btn.mute`）
    VolMute,
    Gear,
    // 以下是設定視窗分頁的圖示，畫在彩色方塊裡，筆畫粗一點
    /// 顯示：跟 `Device` 同一個螢幕，筆畫粗
    Monitor,
    /// 帳號：人像
    Person,
    /// 歌詞：麥克風
    Mic,
    /// 唱片
    Disc,
    /// 迷你視窗：子母畫面
    Pip,
    /// 視覺效果：頻譜長條
    Bars,
    /// 關於：圓圈裡的 i
    Info,
    // 以下是設定視窗清單用的
    /// 返回
    ChevronLeft,
    /// 選中
    Check,
}

const SPEAKER: &str = r##"<path fill="#fff" d="M4 9.5h3.2L12 5.5v13l-4.8-4H4z"/>"##;
const WAVE1: &str = r##"<path d="M15.5 9.2a4 4 0 0 1 0 5.6"/>"##;
const WAVE2: &str = r##"<path d="M18.3 6.6a7.6 7.6 0 0 1 0 10.8"/>"##;
const HEART: &str = "M12 20.3s-7.3-4.4-9-9.1C1.9 8 3.9 4.6 7.3 4.6c2 0 3.6 1.1 4.7 2.7 1.1-1.6 2.7-2.7 4.7-2.7 3.4 0 5.4 3.4 4.3 6.6-1.7 4.7-9 9.1-9 9.1z";

impl Svg {
    /// (路徑, 是否實心, 筆寬)
    fn body(self) -> (String, bool, f32) {
        let p = |d: &str| format!(r##"<path d="{d}"/>"##);
        match self {
            Svg::Shuffle => (p("M16 3.5 20 7l-4 3.5M16 13.5 20 17l-4 3.5M20 7h-3.2a5 5 0 0 0-4.1 2.2l-3.4 5.6A5 5 0 0 1 5.2 17H3M20 17h-3.2a5 5 0 0 1-4.1-2.2M3 7h2.2a5 5 0 0 1 4.1 2.2"), false, 1.9),
            Svg::Prev => (p("M7 6v12M19 6.5v11a1 1 0 0 1-1.6.8l-7.3-5.5a1 1 0 0 1 0-1.6l7.3-5.5A1 1 0 0 1 19 6.5z"), true, 1.7),
            Svg::Play => (p("M7 4.5v15a1 1 0 0 0 1.5.87l12-7.5a1 1 0 0 0 0-1.74l-12-7.5A1 1 0 0 0 7 4.5z"), true, 1.7),
            Svg::Pause => (p("M7 4h3.4v16H7zM13.6 4H17v16h-3.4z"), true, 1.7),
            Svg::Next => (p("M17 6v12M5 6.5v11a1 1 0 0 0 1.6.8l7.3-5.5a1 1 0 0 0 0-1.6L6.6 5.7A1 1 0 0 0 5 6.5z"), true, 1.7),
            Svg::Repeat => (p("M17 2.5 21 6l-4 3.5M21 6H7a4 4 0 0 0-4 4v1M7 21.5 3 18l4-3.5M3 18h14a4 4 0 0 0 4-4v-1"), false, 1.9),
            Svg::Heart => (p(HEART), false, 1.9),
            Svg::HeartFilled => (p(HEART), true, 1.9),
            Svg::Device => (p("M4 4.5h16v11H4zM9 20h6M12 15.5V20"), false, 1.9),
            Svg::Vol => (format!("{SPEAKER}{WAVE1}{WAVE2}"), false, 1.9),
            Svg::VolLow => (format!("{SPEAKER}{WAVE1}"), false, 1.9),
            Svg::VolMute => (format!(r##"{SPEAKER}<path d="M16 9.5l5 5M21 9.5l-5 5"/>"##), false, 1.9),
            Svg::Gear => (
                p("M12 15.2a3.2 3.2 0 1 0 0-6.4 3.2 3.2 0 0 0 0 6.4z")
                    + &p("M19.3 13.9a1.5 1.5 0 0 0 .3 1.65l.06.06a1.8 1.8 0 1 1-2.55 2.55l-.06-.06a1.5 1.5 0 0 0-1.65-.3 1.5 1.5 0 0 0-.91 1.37v.17a1.8 1.8 0 1 1-3.6 0v-.09a1.5 1.5 0 0 0-.98-1.37 1.5 1.5 0 0 0-1.65.3l-.06.06a1.8 1.8 0 1 1-2.55-2.55l.06-.06a1.5 1.5 0 0 0 .3-1.65 1.5 1.5 0 0 0-1.37-.91h-.17a1.8 1.8 0 1 1 0-3.6h.09a1.5 1.5 0 0 0 1.37-.98 1.5 1.5 0 0 0-.3-1.65l-.06-.06A1.8 1.8 0 1 1 8.1 4.32l.06.06a1.5 1.5 0 0 0 1.65.3h.07a1.5 1.5 0 0 0 .91-1.37v-.17a1.8 1.8 0 1 1 3.6 0v.09a1.5 1.5 0 0 0 .91 1.37 1.5 1.5 0 0 0 1.65-.3l.06-.06a1.8 1.8 0 1 1 2.55 2.55l-.06.06a1.5 1.5 0 0 0-.3 1.65v.07a1.5 1.5 0 0 0 1.37.91h.17a1.8 1.8 0 1 1 0 3.6h-.09a1.5 1.5 0 0 0-1.37.91z"),
                false,
                1.9,
            ),
            Svg::Monitor => (p("M4 4.5h16v11H4zM9 20h6M12 15.5V20"), false, 2.3),
            Svg::Person => (p("M12 11.5a3.8 3.8 0 1 0 0-7.6 3.8 3.8 0 0 0 0 7.6zM4.5 20.5a7.5 7.5 0 0 1 15 0"), false, 2.3),
            Svg::Mic => (p("M12 3.5a3 3 0 0 0-3 3v5a3 3 0 0 0 6 0v-5a3 3 0 0 0-3-3zM6 11.5a6 6 0 0 0 12 0M12 17.5v3"), false, 2.3),
            Svg::Disc => (
                p("M12 20.5a8.5 8.5 0 1 0 0-17 8.5 8.5 0 0 0 0 17z") + &p("M12 14.2a2.2 2.2 0 1 0 0-4.4 2.2 2.2 0 0 0 0 4.4z"),
                false,
                2.3,
            ),
            Svg::Pip => (
                p("M3.5 5h17v14h-17z") + r##"<path fill="#fff" stroke="none" d="M11.5 11.5h7v5.5h-7z"/>"##,
                false,
                2.1,
            ),
            Svg::Bars => (p("M4.5 10v4M8.25 7v10M12 4v16M15.75 8v8M19.5 10.5v3"), false, 2.4),
            Svg::Info => (
                p("M12 20.5a8.5 8.5 0 1 0 0-17 8.5 8.5 0 0 0 0 17zM12 11v5.5") + r##"<circle fill="#fff" stroke="none" cx="12" cy="7.9" r="1.3"/>"##,
                false,
                2.2,
            ),
            Svg::ChevronLeft => (p("M14.5 5.5 8 12l6.5 6.5"), false, 2.3),
            Svg::Check => (p("M5 12.5l4.5 4.5L19 7.5"), false, 2.4),
        }
    }
}

/// 點陣化成 `px`×`px` 的 RGBA（白色，透明度就是形狀）。直通 alpha，不是預乘。
#[allow(clippy::chunks_exact_to_as_chunks)] // as_chunks 改寫後借用檢查過不了，維持 chunks_exact
pub fn rasterize(icon: Svg, px: u32) -> Vec<u8> {
    let (body, filled, stroke) = icon.body();
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><g fill="{}" stroke="#fff" stroke-width="{stroke}" stroke-linecap="round" stroke-linejoin="round">{body}</g></svg>"##,
        if filled { "#fff" } else { "none" },
    );
    let px = px.max(1);
    let mut out = vec![0u8; (px * px * 4) as usize];
    let Ok(tree) = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()) else {
        return out;
    };
    let Some(mut pm) = resvg::tiny_skia::Pixmap::new(px, px) else {
        return out;
    };
    let s = px as f32 / 24.0;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(s, s), &mut pm.as_mut());
    // 全白的形狀，預乘與否只差在 rgb；直接寫成白色＋原本的 alpha
    for (o, p) in out.chunks_exact_mut(4).zip(pm.pixels()) {
        o.copy_from_slice(&[255, 255, 255, p.alpha()]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每一個都要真的畫得出東西 —— 路徑抄錯一個字母，usvg 會整條丟掉而不報錯
    #[test]
    #[allow(clippy::chunks_exact_to_as_chunks)] // as_chunks 改寫後借用檢查過不了，維持 chunks_exact
    fn 每個圖示都有畫出東西() {
        for ic in [
            Svg::Shuffle, Svg::Prev, Svg::Play, Svg::Pause, Svg::Next, Svg::Repeat, Svg::Heart,
            Svg::HeartFilled, Svg::Device, Svg::Vol, Svg::VolLow, Svg::VolMute, Svg::Gear, Svg::Monitor, Svg::Person,
            Svg::Mic, Svg::Disc, Svg::Pip, Svg::Bars, Svg::Info, Svg::ChevronLeft, Svg::Check,
        ] {
            let px = rasterize(ic, 19);
            let ink: u32 = px.chunks_exact(4).map(|p| p[3] as u32).sum();
            assert!(ink > 255 * 20, "{ic:?} 幾乎是空的（{ink}）");
        }
    }

    #[test]
    #[allow(clippy::chunks_exact_to_as_chunks)] // as_chunks 改寫後借用檢查過不了，維持 chunks_exact
    fn 實心愛心比空心的墨多() {
        let ink = |ic| rasterize(ic, 19).chunks_exact(4).map(|p| p[3] as u32).sum::<u32>();
        assert!(ink(Svg::HeartFilled) > ink(Svg::Heart) * 3 / 2);
    }
}
