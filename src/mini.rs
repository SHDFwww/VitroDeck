//! 迷你視窗 —— Python 版 `web/mini.*` 與 `app.py` 的 `open_mini` 一帶。
//!
//! 封面、曲名、歌手、進度。未鎖定時：拖背景移動、拖右下角改大小、點封面叫出主視窗，
//! 滑鼠移進來右上角出現「鎖定」與「關閉」。鎖定＝最上層＋滑鼠穿透，只能從主視窗的
//! 設定面板或系統列解鎖。不佔工作列。
//!
//! 圓角兩條路並行（見 `layered.rs`）：Win11 交給 DWM，Win10 自己畫圓角與陰影、
//! 讀回 CPU 交給 `UpdateLayeredWindow`。
//!
//! GPU 裝置、quad 與文字渲染器都跟主視窗共用（文字渲染器每一份都要掃一次系統字型）。
//! 各自完整做完「begin → 推 → submit」就不會互相干擾。
//!
//! 位置記在 `mini_rect`，存的是**卡片本體**的螢幕矩形（實體像素），不含 Win10 那條
//! 為了畫陰影四周多留的空白 —— 換條路走的時候位置才不會偏。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowId, WindowLevel};

use crate::gfx::{self, Blit, Gpu, Offscreen};
use crate::layered::{Backend, Layered};
use crate::quad::{Quad, QuadRenderer};
use crate::text::Text;
use crate::theme;

/// 預設大小（邏輯像素）與最小值，照 Python 版
pub const DEFAULT: (f32, f32) = (380.0, 120.0);
const MIN: (f32, f32) = (200.0, 64.0);
const RADIUS: f32 = 16.0;
/// Win10 那條：陰影要畫得下，四周留白
const PAD: f32 = 28.0;
/// 多久重畫一次（進度條要走）。Python 版也是 250ms
const TICK: Duration = Duration::from_millis(250);

/// 系統列選單要顯示勾選狀態，從別的執行緒讀
pub static OPEN: AtomicBool = AtomicBool::new(false);
pub static LOCKED: AtomicBool = AtomicBool::new(false);
/// `VITRO_PROF`：主程式 `tick_mini` 整段（取色、封面上傳、讀歌詞、畫）花的微秒數，跟著上面那行一起記
pub static TICK_US: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// ---------------------------------------------------------------- 取色

/// 從封面取的三個顏色（0..1 的 RGB）。`mini.css` 的 --glow-rgb、--bg、--dim。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tint {
    pub glow: [f32; 3],
    pub bg: [f32; 3],
    pub dim: [f32; 3],
}

fn hex(c: u32) -> [f32; 3] {
    [((c >> 16) & 0xff) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0]
}

/// 取不到色（沒有封面、還沒下載到）
pub fn default_tint() -> Tint {
    Tint { glow: [142.0 / 255.0, 240.0 / 255.0, 207.0 / 255.0], bg: hex(0x07110f), dim: hex(0x8fa39d) }
}

/// 封面幾乎是灰階
fn grey_tint() -> Tint {
    Tint { glow: [225.0 / 255.0, 228.0 / 255.0, 235.0 / 255.0], bg: hex(0x0c0d10), dim: hex(0x9a9ca6) }
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let l = (mx + mn) / 2.0;
    if mx == mn {
        return (0.0, 0.0, l);
    }
    let d = mx - mn;
    let s = if l > 0.5 { d / (2.0 - mx - mn) } else { d / (mx + mn) };
    let h = if mx == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    let k = |n: f32| (n + h / 30.0) % 12.0;
    let a = s * l.min(1.0 - l);
    let f = |n: f32| l - a * (-1.0f32).max((k(n) - 3.0).min((9.0 - k(n)).min(1.0)));
    // 跟 JS 一樣四捨五入到 0..255，結果才對得上
    [f(0.0), f(8.0), f(4.0)].map(|v| (v * 255.0).round() / 255.0)
}

/// `mini.js` 的 `pickColor` ＋ `tint`：縮成 24×24，依「飽和 × 亮度」加權後按色相分
/// 12 桶，取分數最高那桶的加權平均。`px` 是 24×24 的 RGBA8。
pub fn tint_from_pixels(px: &[u8]) -> Tint {
    let Some((hue, sat)) = dominant(px) else { return grey_tint() };
    // 光點要亮、要有色，不管封面本身多暗多灰都拉到同一個區間
    let s = (sat + 0.15).clamp(0.55, 0.9);
    Tint {
        glow: hsl_to_rgb(hue, s, 0.72),
        bg: hsl_to_rgb(hue, s.min(0.6), 0.055),
        dim: hsl_to_rgb(hue, 0.16, 0.6),
    }
}

/// 封面的主色相（度）與那一桶的平均飽和度。幾乎是灰階就 `None`。
/// 迷你視窗的光暈與主視窗的強調色共用這一段
#[allow(clippy::chunks_exact_to_as_chunks)] // as_chunks 改寫後借用檢查過不了，維持 chunks_exact
fn dominant(px: &[u8]) -> Option<(f32, f32)> {
    #[derive(Default, Clone, Copy)]
    struct B {
        w: f32,
        x: f32,
        y: f32,
        s: f32,
    }
    let mut buckets = [B::default(); 12];
    for p in px.chunks_exact(4) {
        let (h, s, l) = rgb_to_hsl(p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
        if !(0.12..=0.92).contains(&l) {
            continue; // 近黑近白不算顏色
        }
        let w = s * s * (1.0 - (l - 0.5).abs() * 1.4);
        if w <= 0.0 {
            continue;
        }
        let b = &mut buckets[((h / 30.0) as usize) % 12];
        // 色相是環狀的，用向量平均才不會在紅色那裡（0°/360°）算歪
        b.w += w;
        b.x += h.to_radians().cos() * w;
        b.y += h.to_radians().sin() * w;
        b.s += s * w;
    }
    let best = buckets.iter().copied().fold(B::default(), |a, b| if b.w > a.w { b } else { a });
    if best.w < 2.0 {
        return None;
    }
    let hue = (best.y.atan2(best.x).to_degrees() + 360.0) % 360.0;
    Some((hue, best.s / best.w))
}

fn small_pixels(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    Some(img.resize_exact(24, 24, image::imageops::FilterType::Triangle).to_rgba8().into_raw())
}

/// 從編碼過的圖檔（jpg/png/webp）取色。解不開就 `None`。
pub fn tint_from_bytes(bytes: &[u8]) -> Option<Tint> {
    Some(tint_from_pixels(&small_pixels(bytes)?))
}

/// 主視窗的強調色（`theme::accent_from`）。外層 `None` = 圖解不開，
/// 內層 `None` = 封面幾乎是灰階，該退回 Spotify 綠
pub fn accent_from_bytes(bytes: &[u8]) -> Option<Option<[f32; 3]>> {
    Some(accent_from_pixels(&small_pixels(bytes)?))
}

/// `px` 是 24×24 的 RGBA8
pub fn accent_from_pixels(px: &[u8]) -> Option<[f32; 3]> {
    let (hue, sat) = dominant(px)?;
    Some(crate::theme::accent_from(hsl_to_rgb(hue, 0.7, 0.5), sat))
}

// ---------------------------------------------------------------- 版面

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Art,
    Lock,
    Close,
    Grip,
}

#[derive(Clone, Copy, Debug)]
struct R {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl R {
    fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// 歌詞模式的兩行：當前句與下一句，同一個字級（換句時整組往上推）
#[derive(Clone, Copy, Debug)]
struct LyricRows {
    y: f32,
    size: f32,
    /// 行高
    lh: f32,
    /// 放不放得下第二行（下一句）
    two: bool,
}

/// 版面，邏輯像素、卡片左上角為原點。數字照 `mini.css`（全部跟著視窗高度走）。
/// 歌詞模式是 Rust 版自己加的：歌手併到歌名那行後面，空出來的位置放兩行歌詞。
struct Layout {
    art: R,
    meta_x: f32,
    meta_w: f32,
    title_y: f32,
    title_size: f32,
    /// 歌手自己一行的位置。歌詞模式下是 `None`（併到歌名後面）
    artist_y: Option<f32>,
    artist_size: f32,
    lyric: Option<LyricRows>,
    track_y: f32,
    show_time: bool,
    /// 實際用的字級倍率（見 `layout`）
    k: f32,
    lock: R,
    close: R,
    grip: R,
}

/// `k` 是字級倍率：行高都乘它，放不下第二行歌詞就只留一行，再放不下就把倍率降到剛好放得下
/// （實際用的倍率在 `Layout::k`，畫字前要 `Text::cap_scale`）
fn layout(w: f32, h: f32, lyrics: bool, k: f32) -> Layout {
    let vh = h / 100.0;
    let pad = (13.0 * vh).clamp(8.0, 20.0);
    let art = h - 2.0 * pad;
    let meta_x = pad + art + pad * 0.95;
    let meta_w = (w - meta_x - pad).max(10.0);
    let title_size = (14.0 * vh).clamp(12.0, 18.0);
    let artist_size = (10.5 * vh).clamp(10.5, 13.5);
    let gap = (3.0 * vh).clamp(1.0, 5.0);
    let mt = (8.0 * vh).clamp(4.0, 14.0);
    let sh = 9.0;
    let size = (11.5 * vh).clamp(11.0, 15.0);
    // 字的部分（隨倍率放大的行高）與固定的部分（間距、進度條）分開算，放不下時才解得出倍率
    let (fixed_two, fixed_one) = (2.0 * gap + mt + sh, 2.0 * gap + mt + sh);
    let text_h = |n: f32| title_size * 1.3 + n * size * 1.3;
    let (k, two) = if lyrics {
        // 中間那一段：歌詞一到兩行（矮到放不下第二行就只留當前句）
        let fits = |n: f32, fixed: f32| ((h - 8.0 - fixed) / text_h(n)).max(0.0);
        if k <= fits(2.0, fixed_two) {
            (k, true)
        } else {
            (k.min(fits(1.0, fixed_one).max(1.0)), false)
        }
    } else {
        let (n, fixed) = if h >= 70.0 { (artist_size / size, 2.0 * gap + mt + sh) } else { (0.0, gap + mt + sh) };
        (k.min(((h - 8.0 - fixed) / text_h(n)).max(1.0)), false)
    };
    let (tl, al) = (title_size * 1.3 * k, artist_size * 1.3 * k);
    let (mid, show_artist, lyric) = if lyrics {
        let lh = size * 1.3;
        let n = if two { 2.0 } else { 1.0 };
        (gap + n * lh * k, false, Some(LyricRows { y: 0.0, size, lh, two }))
    } else if h >= 70.0 {
        (gap + al, true, None)
    } else {
        (0.0, false, None)
    };
    let total = tl + mid + gap + mt + sh;
    let top = (h - total) / 2.0;
    let artist_y = show_artist.then_some(top + tl + gap);
    let lyric = lyric.map(|r| LyricRows { y: top + tl + gap, ..r });
    let track_y = top + total - sh / 2.0;
    let close = R { x: w - 10.0 - 22.0, y: 8.0, w: 22.0, h: 22.0 };
    let lock = R { x: close.x - 2.0 - 22.0, ..close };
    Layout {
        art: R { x: pad, y: pad, w: art, h: art },
        meta_x,
        meta_w,
        title_y: top,
        title_size,
        artist_y,
        artist_size,
        lyric,
        track_y,
        show_time: w > 300.0,
        k,
        lock,
        close,
        grip: R { x: w - 5.0 - 12.0, y: h - 5.0 - 12.0, w: 12.0, h: 12.0 },
    }
}

fn hit(w: f32, h: f32, x: f32, y: f32, locked: bool) -> Option<Hit> {
    // 命中的東西（封面、工具、把手）不受歌詞模式影響
    let l = layout(w, h, false, 1.0);
    if !locked {
        if l.close.contains(x, y) {
            return Some(Hit::Close);
        }
        if l.lock.contains(x, y) {
            return Some(Hit::Lock);
        }
        if l.grip.contains(x, y) {
            return Some(Hit::Grip);
        }
    }
    l.art.contains(x, y).then_some(Hit::Art)
}

fn fmt(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    format!("{}:{:02}", s / 60, s % 60)
}

// ---------------------------------------------------------------- 視窗

enum Out {
    Layered(Layered),
    Dwm { surface: wgpu::Surface<'static>, config: wgpu::SurfaceConfiguration, blit: Blit },
}

/// 迷你視窗要主程式做的事
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    OpenMain,
    Lock,
    Close,
}

/// 這一幀要畫的內容
pub struct Content<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub pos_ms: i64,
    pub dur_ms: i64,
    pub has: bool,
    pub art: Option<Arc<wgpu::TextureView>>,
    /// 歌詞模式才有。關掉時主程式連歌詞的鎖都不碰
    pub lyric: Option<Lyric<'a>>,
}

/// 歌詞模式的兩行
pub struct Lyric<'a> {
    /// 哪一首（換歌時不播換句動畫）
    pub key: &'a str,
    /// 第幾句；-1 是第一句之前。沒有同步歌詞時是 `i64::MIN`，`cur` 放說明文字
    pub idx: i64,
    pub cur: &'a str,
    pub next: &'a str,
    /// 離下一句還有幾毫秒（播放中才有），時間到就醒來重畫，不用等下一次 250ms
    pub next_in_ms: Option<i64>,
}

/// 換句時往上推的時間
const SLIDE: f32 = theme::dur::MINI_SLIDE;
/// 推的途中多久畫一幀。分層那條每幀都要讀回 CPU，30fps 就夠順
const SLIDE_TICK: Duration = Duration::from_millis(33);

pub struct Mini {
    pub window: Arc<Window>,
    out: Out,
    target: Option<Offscreen>,
    pub locked: bool,
    /// 卡片的螢幕位置與大小（實體像素）
    card: (i32, i32, u32, u32),
    /// `card` 是照這個縮放量的，`card_l` 是卡片的邏輯大小。拖到縮放不同的螢幕時，
    /// 實體大小由邏輯大小重算 —— 拿實體大小乘比例的話，來回拖幾次就會因為四捨五入越拖越大
    scale: f32,
    card_l: (f32, f32),
    hover: bool,
    hot: Option<Hit>,
    /// 游標在卡片座標裡的位置（邏輯像素）
    cursor: (f32, f32),
    pressed: Option<Hit>,
    /// 拉右下角：起點的螢幕游標與當時的卡片大小
    resizing: Option<((i32, i32), (u32, u32))>,
    last_draw: Option<Instant>,
    pub dirty: bool,
    pub tint: Tint,
    /// 取色是對哪張封面取的。空字串＝還沒取到
    pub tint_key: String,
    tint_tried: Option<Instant>,
    /// 位置或大小變了、還沒存
    pub moved_at: Option<Instant>,
    /// 歌詞模式（設定的 `mini_lyrics`）
    pub lyrics: bool,
    /// 上一幀畫的是哪一首的第幾句、內容是什麼。用來判斷要不要播換句動畫
    ly_key: String,
    ly_idx: i64,
    ly_cur: String,
    /// 換句動畫：推出去的那句與開始時刻
    ly_slide: Option<(String, Instant)>,
    /// 下一句開始的時刻
    wake_at: Option<Instant>,
    /// 歌名、歌手放不下時來回捲動：對哪一首算的、從何時起算（換歌就從頭捲）
    roll_at: (String, Instant),
    /// 捲動的字下一次動的時刻（停在兩端時不用一直畫）
    roll_wake: Option<Instant>,
    /// `VITRO_PROF`：每 5 秒把畫了幾幀寫進 diag.log（起點、幀數、其中被 dirty 叫醒的）
    prof: Option<(Instant, u32, u32, Duration)>,
}

fn scale_of(w: &Window) -> f32 {
    w.scale_factor() as f32
}

/// 卡片中心落在某一台螢幕上（實體像素）
fn on_screen(el: &ActiveEventLoop, [x, y, w, h]: [f64; 4]) -> bool {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    el.available_monitors().any(|m| {
        let (p, s) = (m.position(), m.size());
        let (l, t) = (p.x as f64, p.y as f64);
        cx >= l && cx < l + s.width as f64 && cy >= t && cy < t + s.height as f64
    })
}

/// 預設位置：螢幕右下角、工作列上面一點。`w`、`h` 是卡片的實體像素
fn corner(m: &winit::monitor::MonitorHandle, w: u32, h: u32) -> (i32, i32) {
    let (p, s, k) = (m.position(), m.size(), m.scale_factor());
    (
        p.x + s.width as i32 - w as i32 - (24.0 * k) as i32,
        p.y + s.height as i32 - h as i32 - (72.0 * k) as i32,
    )
}

impl Mini {
    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    fn pad(&self) -> f32 {
        match self.out {
            Out::Layered(_) => PAD * scale_of(&self.window),
            Out::Dwm { .. } => 0.0,
        }
    }

    /// 開視窗。`rect` 是存下來的卡片矩形，`None` 或已經不在任何螢幕上就擺主螢幕右下角（大小照存的）。
    pub fn open(
        el: &ActiveEventLoop,
        gpu: &Gpu,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        rect: Option<[f64; 4]>,
        locked: bool,
        lyrics: bool,
    ) -> anyhow::Result<Self> {
        use winit::platform::windows::{CornerPreference, WindowAttributesExtWindows};
        let backend = crate::layered::pick_backend();
        let scale = el.primary_monitor().map_or(1.0, |m| m.scale_factor()) as f32;
        let (w, h) = match rect {
            Some([_, _, w, h]) => ((w as f32).max(MIN.0 * scale) as u32, (h as f32).max(MIN.1 * scale) as u32),
            None => ((DEFAULT.0 * scale) as u32, (DEFAULT.1 * scale) as u32),
        };
        // 存的位置可能來自別的螢幕配置（拔掉一台、換到別台電腦），整張落在螢幕外就看不到也拖不回來。
        // 這時只把位置改回預設，大小照舊
        let card = match rect.filter(|r| on_screen(el, *r)) {
            Some([x, y, _, _]) => (x as i32, y as i32, w, h),
            None => {
                let (x, y) = el.primary_monitor().map_or((100, 100), |m| corner(&m, w, h));
                (x, y, w, h)
            }
        };
        let pad = if backend == Backend::Layered { (PAD * scale).round() as i32 } else { 0 };
        let mut attrs = Window::default_attributes()
            .with_title("VitroDeck Mini")
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false)
            .with_skip_taskbar(true)
            .with_position(PhysicalPosition::new(card.0 - pad, card.1 - pad))
            .with_inner_size(PhysicalSize::new(card.2 + 2 * pad as u32, card.3 + 2 * pad as u32));
        if backend == Backend::Dwm {
            // 沒有標題列的視窗，DWM 預設不給陰影也不給圓角，兩個都要明講
            attrs = attrs.with_undecorated_shadow(true).with_corner_preference(CornerPreference::Round);
        }
        let window = Arc::new(el.create_window(attrs)?);
        let scale0 = scale_of(&window);
        // 開在縮放跟主螢幕不同的螢幕上時，winit 建窗途中就照 DPI 改過視窗大小了；DWM 那條的卡片＝視窗，對齊一次
        let mut card = card;
        if backend == Backend::Dwm {
            let sz = window.inner_size();
            if sz.width > 0 && sz.height > 0 {
                (card.2, card.3) = (sz.width, sz.height);
            }
        }
        let out = match backend {
            Backend::Layered => {
                let RawWindowHandle::Win32(h) = window.window_handle()?.as_raw() else {
                    anyhow::bail!("不是 Win32 視窗");
                };
                Out::Layered(Layered::new(h.hwnd.get()))
            }
            Backend::Dwm => {
                let surface = instance.create_surface(window.clone())?;
                let sz = window.inner_size();
                let mut config = surface
                    .get_default_config(adapter, sz.width.max(1), sz.height.max(1))
                    .ok_or_else(|| anyhow::anyhow!("surface 沒有可用設定"))?;
                // 跟主視窗同一個理由：離屏裡已經是編碼好的 sRGB，交換鏈不能再編一次
                let plain = config.format.remove_srgb_suffix();
                if surface.get_capabilities(adapter).formats.contains(&plain) {
                    config.format = plain;
                }
                surface.configure(&gpu.device, &config);
                let blit = Blit::new(gpu, config.format);
                Out::Dwm { surface, config, blit }
            }
        };
        let mut m = Self {
            window,
            out,
            target: None,
            locked: !locked,
            card,
            scale: scale0,
            card_l: (card.2 as f32 / scale0, card.3 as f32 / scale0),
            hover: false,
            hot: None,
            cursor: (0.0, 0.0),
            pressed: None,
            resizing: None,
            last_draw: None,
            dirty: true,
            tint: default_tint(),
            tint_key: String::new(),
            tint_tried: None,
            moved_at: None,
            lyrics,
            ly_key: String::new(),
            ly_idx: i64::MIN,
            ly_cur: String::new(),
            ly_slide: None,
            wake_at: None,
            roll_at: (String::new(), Instant::now()),
            roll_wake: None,
            prof: std::env::var_os("VITRO_PROF").map(|_| (Instant::now(), 0, 0, Duration::ZERO)),
        };
        m.set_locked(locked);
        OPEN.store(true, Ordering::Relaxed);
        Ok(m)
    }

    /// 鎖定＝最上層＋滑鼠穿透。
    pub fn set_locked(&mut self, locked: bool) {
        if self.locked == locked {
            return;
        }
        self.locked = locked;
        LOCKED.store(locked, Ordering::Relaxed);
        match &self.out {
            Out::Layered(l) => l.set_locked(locked),
            Out::Dwm { .. } => {
                // winit 的穿透是 WS_EX_LAYERED|WS_EX_TRANSPARENT，交換鏈照樣畫得出來
                let _ = self.window.set_cursor_hittest(!locked);
                self.window.set_window_level(if locked { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal });
            }
        }
        self.hover = false;
        self.hot = None;
        self.dirty = true;
    }

    /// 叫回來：移到主螢幕右下角，邏輯大小不變，鎖定狀態不動。
    /// 給找不到它的時候用（被拖到看不到的地方、螢幕配置變了）
    pub fn recall(&mut self) {
        let Some(m) = self.window.primary_monitor() else { return };
        let k = m.scale_factor() as f32;
        // 照主螢幕的縮放換算，從縮放不同的螢幕叫回來時才剛好貼齊角落
        let (w, h) = ((self.card_l.0 * k).round() as u32, (self.card_l.1 * k).round() as u32);
        let (x, y) = corner(&m, w, h);
        let pad = (PAD * k).round() as i32;
        let pad = if matches!(self.out, Out::Layered(_)) { pad } else { 0 };
        self.window.set_outer_position(PhysicalPosition::new(x - pad, y - pad));
        (self.card.0, self.card.1) = (x, y);
        self.moved_at = Some(Instant::now());
        self.dirty = true;
        if !self.locked {
            // 沒鎖定的不是最上層，可能壓在別的視窗後面
            self.window.focus_window();
        }
    }

    /// 存進 `mini_rect` 的值
    pub fn rect(&self) -> [f64; 4] {
        let (x, y, w, h) = self.card;
        [x as f64, y as f64, w as f64, h as f64]
    }

    /// 開關歌詞模式（版面整個換）
    pub fn set_lyrics(&mut self, on: bool) {
        if self.lyrics != on {
            self.lyrics = on;
            self.ly_key.clear();
            self.ly_idx = i64::MIN;
            self.ly_slide = None;
            self.wake_at = None;
            self.dirty = true;
        }
    }

    /// 換句動畫還沒畫到最後一幀。推完那一幀（p = 1）畫掉才清，不然會停在半路等下一個 250ms
    fn sliding(&self) -> bool {
        self.ly_slide.is_some()
    }

    /// 該不該重畫了
    pub fn due(&self) -> bool {
        self.dirty || Instant::now() >= self.next_due()
    }

    /// 下一次要醒來畫的時刻（主視窗縮在系統列、迴圈改成 Wait 的時候用）。
    /// 平常 250ms 一次；歌詞模式多兩個：下一句開始的那一刻、換句動畫途中 30fps。
    /// 歌名放不下在捲的時候也是 30fps，停在兩端那段等到要動了再醒
    pub fn next_due(&self) -> Instant {
        let Some(last) = self.last_draw else { return Instant::now() };
        let mut t = last + TICK;
        if let Some(w) = self.wake_at {
            t = t.min(w);
        }
        if self.sliding() {
            t = t.min(last + SLIDE_TICK);
        }
        if let Some(w) = self.roll_wake {
            t = t.min(w.max(last + SLIDE_TICK));
        }
        t
    }

    /// 該不該再試一次取色。封面還沒下載到的話一秒試一次就好
    pub fn want_tint(&mut self, key: &str) -> bool {
        if self.tint_key == key {
            return false;
        }
        if self.tint_tried.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return false;
        }
        self.tint_tried = Some(Instant::now());
        true
    }

    pub fn set_tint(&mut self, key: &str, t: Tint) {
        self.tint = t;
        self.tint_key = key.to_string();
        self.tint_tried = None;
        self.dirty = true;
    }

    /// 卡片的大小（邏輯像素）
    fn logical(&self) -> (f32, f32) {
        let s = scale_of(&self.window);
        (self.card.2 as f32 / s, self.card.3 as f32 / s)
    }

    pub fn event(&mut self, ev: &WindowEvent) -> Option<Action> {
        let s = scale_of(&self.window);
        match ev {
            WindowEvent::CursorMoved { position, .. } => {
                let pad = self.pad();
                self.cursor = ((position.x as f32 - pad) / s, (position.y as f32 - pad) / s);
                if let Some((start, size)) = self.resizing {
                    self.resize_to(start, size);
                }
                let (w, h) = self.logical();
                let hot = hit(w, h, self.cursor.0, self.cursor.1, self.locked);
                if !self.hover || hot != self.hot {
                    self.dirty = true;
                }
                self.hover = true;
                self.hot = hot;
                self.window.set_cursor(match hot {
                    Some(Hit::Grip) => winit::window::CursorIcon::NwseResize,
                    Some(Hit::Art) if !self.locked => winit::window::CursorIcon::Pointer,
                    Some(Hit::Lock | Hit::Close) => winit::window::CursorIcon::Pointer,
                    _ => winit::window::CursorIcon::Default,
                });
            }
            WindowEvent::CursorLeft { .. } if self.resizing.is_none() => {
                self.hover = false;
                self.hot = None;
                self.dirty = true;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if self.locked {
                    return None;
                }
                match self.hot {
                    Some(Hit::Grip) => {
                        self.resizing = Some((cursor_screen(), (self.card.2, self.card.3)));
                    }
                    Some(h) => self.pressed = Some(h),
                    // 背景：拖動視窗
                    None => {
                        let _ = self.window.drag_window();
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.resizing.take().is_some() {
                    return None;
                }
                let p = self.pressed.take();
                // 按下跟放開都在同一個東西上才算點
                if p.is_some() && p == self.hot {
                    return match p {
                        Some(Hit::Art) => Some(Action::OpenMain),
                        Some(Hit::Lock) => Some(Action::Lock),
                        Some(Hit::Close) => Some(Action::Close),
                        _ => None,
                    };
                }
            }
            WindowEvent::Moved(p) => {
                let pad = self.pad().round() as i32;
                let (x, y) = (p.x + pad, p.y + pad);
                if (x, y) != (self.card.0, self.card.1) {
                    self.card.0 = x;
                    self.card.1 = y;
                    self.moved_at = Some(Instant::now());
                }
            }
            // 拖到縮放不同的螢幕：邏輯大小不變、實體大小重算（DWM 那條 winit 建議的新大小也是這樣算）。
            // 沒重算的話版面照卡片高度排，內容維持原本的實體大小，只有陰影留白變大
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let s = *scale_factor as f32;
                if (s - self.scale).abs() > 1e-3 {
                    self.scale = s;
                    self.card.2 = (self.card_l.0 * s).round() as u32;
                    self.card.3 = (self.card_l.1 * s).round() as u32;
                    self.moved_at = Some(Instant::now());
                }
                self.dirty = true;
            }
            // DWM 那條的視窗大小歸 winit 管（換 DPI 時它會自己縮放視窗），卡片要跟著實際大小走，
            // 不然離屏照舊尺寸畫、blit 拉到新尺寸，整張糊掉。分層那條的視窗大小是每幀 present 決定的，不跟
            WindowEvent::Resized(sz) if matches!(self.out, Out::Dwm { .. }) && sz.width > 0 && sz.height > 0 => {
                if (sz.width, sz.height) != (self.card.2, self.card.3) {
                    self.card.2 = sz.width;
                    self.card.3 = sz.height;
                    self.card_l = (sz.width as f32 / s, sz.height as f32 / s);
                    self.moved_at = Some(Instant::now());
                }
                self.dirty = true;
            }
            WindowEvent::RedrawRequested => self.dirty = true,
            _ => {}
        }
        None
    }

    /// 拉右下角。用螢幕上的游標算位移：視窗一變大，視窗內的座標就跟著跑掉了
    fn resize_to(&mut self, start: (i32, i32), size: (u32, u32)) {
        let s = scale_of(&self.window);
        let now = cursor_screen();
        let w = (size.0 as i32 + now.0 - start.0).max((MIN.0 * s) as i32) as u32;
        let h = (size.1 as i32 + now.1 - start.1).max((MIN.1 * s) as i32) as u32;
        if (w, h) == (self.card.2, self.card.3) {
            return;
        }
        self.card.2 = w;
        self.card.3 = h;
        self.card_l = (w as f32 / s, h as f32 / s);
        if let Out::Dwm { .. } = self.out {
            let _ = self.window.request_inner_size(PhysicalSize::new(w, h));
        }
        self.moved_at = Some(Instant::now());
        self.dirty = true;
    }

    /// 看這一幀的歌詞跟上一幀比是不是往下一句走了，是的話開始（或繼續）換句動畫。
    /// 回傳推出去的那句與進度 0..1；沒在推就是 `None`。順便排好下一句開始時醒來。
    fn take_lyric(&mut self, c: &Content<'_>) -> Option<(String, f32)> {
        let now = Instant::now();
        let Some(ly) = c.lyric.as_ref() else {
            self.ly_key.clear();
            self.ly_idx = i64::MIN;
            self.ly_slide = None;
            self.wake_at = None;
            return None;
        };
        if ly.key != self.ly_key || ly.idx != self.ly_idx {
            // 同一首往下走一句才推；換歌、拖進度、歌詞剛到貨都直接換
            let step = ly.key == self.ly_key && self.ly_idx != i64::MIN && self.ly_idx.checked_add(1) == Some(ly.idx);
            self.ly_slide = step.then(|| (std::mem::take(&mut self.ly_cur), now));
            self.ly_key = ly.key.to_string();
            self.ly_idx = ly.idx;
        }
        if self.ly_cur != ly.cur {
            self.ly_cur = ly.cur.to_string();
        }
        // 多等 5ms，醒來時確定已經跨過去，不會差一點又排一次
        self.wake_at = ly.next_in_ms.filter(|ms| *ms >= 0).map(|ms| now + Duration::from_millis(ms as u64 + 5));
        let (prev, t0) = self.ly_slide.as_ref()?;
        let p = t0.elapsed().as_secs_f32() / SLIDE;
        if p >= 1.0 {
            self.ly_slide = None;
            return None;
        }
        Some((prev.clone(), p))
    }

    pub fn draw(&mut self, gpu: &Gpu, quads: &mut QuadRenderer, text: &mut Text, c: &Content<'_>) -> anyhow::Result<()> {
        let t_draw = Instant::now();
        let r = self.draw_inner(gpu, quads, text, c);
        if let Some((t0, n, d, spent)) = self.prof.as_mut() {
            *n += 1;
            *spent += t_draw.elapsed();
            if t0.elapsed() >= Duration::from_secs(5) {
                crate::config::diag(&format!(
                    "迷你視窗 {n} 幀／5 秒（dirty {d}）draw 共 {:.1} ms、tick 共 {:.1} ms 歌詞={}",
                    spent.as_secs_f64() * 1000.0,
                    TICK_US.swap(0, Ordering::Relaxed) as f64 / 1000.0,
                    self.lyrics
                ));
                *t0 = Instant::now();
                (*n, *d, *spent) = (0, 0, Duration::ZERO);
            }
        }
        r
    }

    fn draw_inner(&mut self, gpu: &Gpu, quads: &mut QuadRenderer, text: &mut Text, c: &Content<'_>) -> anyhow::Result<()> {
        if let Some((_, _, d, _)) = self.prof.as_mut() {
            *d += self.dirty as u32;
        }
        self.dirty = false;
        self.last_draw = Some(Instant::now());
        let slide = self.take_lyric(c);
        let key = format!("{}|{}", c.title, c.artist);
        if key != self.roll_at.0 {
            self.roll_at = (key, Instant::now());
        }
        let pad = self.pad();
        let (cw, ch) = (self.card.2 as f32, self.card.3 as f32);
        let (w, h) = ((cw + pad * 2.0).round() as u32, (ch + pad * 2.0).round() as u32);
        if self.target.as_ref().is_none_or(|t| t.width != w || t.height != h) {
            self.target = Some(Offscreen::new(&gpu.device, w, h));
        }
        let target = self.target.as_ref().unwrap();
        let look = Look {
            s: scale_of(&self.window),
            pad,
            card: (cw, ch),
            layered: matches!(self.out, Out::Layered(_)),
            show_tools: self.hover && !self.locked,
            hot: self.hot,
            tint: self.tint,
            lyrics: self.lyrics,
            roll: Some(self.roll_at.1.elapsed().as_secs_f32()),
        };
        let wait = paint(gpu, quads, text, target, &look, c, slide.as_ref().map(|(p, k)| (p.as_str(), *k)))?;
        self.roll_wake = wait.map(|w| Instant::now() + Duration::from_secs_f32(w.min(60.0)));

        match &mut self.out {
            Out::Layered(lay) => {
                let rgba = target.read_rgba(gpu);
                lay.present(&rgba, w, h, None)?;
            }
            Out::Dwm { surface, config, blit } => {
                use wgpu::CurrentSurfaceTexture as Cst;
                let sz = self.window.inner_size();
                if config.width != sz.width.max(1) || config.height != sz.height.max(1) {
                    config.width = sz.width.max(1);
                    config.height = sz.height.max(1);
                    surface.configure(&gpu.device, config);
                }
                let frame = match surface.get_current_texture() {
                    Cst::Success(f) | Cst::Suboptimal(f) => f,
                    Cst::Timeout | Cst::Occluded | Cst::Validation => return Ok(()),
                    Cst::Outdated | Cst::Lost => {
                        surface.configure(&gpu.device, config);
                        self.dirty = true;
                        return Ok(());
                    }
                };
                let view = frame.texture.create_view(&Default::default());
                let mut enc = gpu.device.create_command_encoder(&Default::default());
                {
                    let mut pass = gfx::pass(&mut enc, &view, "mini-blit", Some(wgpu::Color::BLACK));
                    blit.draw(gpu, &mut pass, &target.view);
                }
                gpu.queue.submit([enc.finish()]);
                gpu.queue.present(frame);
            }
        }
        if !self.window.is_visible().unwrap_or(true) {
            self.window.set_visible(true);
        }
        Ok(())
    }
}

/// 畫一張迷你視窗要的東西。跟視窗無關，`--shot mini` 也走這裡
pub struct Look {
    /// DPI 縮放
    pub s: f32,
    /// 卡片四周為了畫陰影多留的空白（實體像素；DWM 那條是 0）
    pub pad: f32,
    /// 卡片大小（實體像素）
    pub card: (f32, f32),
    /// 分層那條：卡片、圓角與陰影自己畫
    pub layered: bool,
    pub show_tools: bool,
    pub hot: Option<Hit>,
    pub tint: Tint,
    pub lyrics: bool,
    /// 歌名、歌手放不下時來回捲動的時鐘（秒）。`None` 停在開頭（截圖）
    pub roll: Option<f32>,
}

/// 把一幀畫進 `target`（大小＝卡片＋四周空白）。`slide` 是換句動畫：推出去的那句與進度。
/// 回傳歌名、歌手的捲動字多久後會再動（秒，見 `widgets::marquee_wait`）；`None` 是都放得下
pub fn paint(
    gpu: &Gpu,
    quads: &mut QuadRenderer,
    text: &mut Text,
    target: &Offscreen,
    look: &Look,
    c: &Content<'_>,
    slide: Option<(&str, f32)>,
) -> anyhow::Result<Option<f32>> {
    let (s, pad) = (look.s, look.pad);
    // 迷你視窗自己乘縮放、座標全是這張圖上的像素，renderer 不能再乘一次主視窗的縮放
    let _unscaled = crate::gfx::scoped_scale(1.0);
    // 迷你視窗不跟外觀模式：卡片永遠是依封面取色的深色（第 6 階段使用者決定）
    let _dark = theme::scoped_dark();
    let (cw, ch) = look.card;
    let (w, h) = (target.width, target.height);
    let (lw, lh) = (cw / s, ch / s);
    let l = layout(lw, lh, look.lyrics, crate::text::pref_scale());
    // 邏輯座標 → 這張圖上的像素
    let px = |x: f32| pad + x * s;
    let t = look.tint;
    let glow = |a: f32| [t.glow[0], t.glow[1], t.glow[2], a];
    let layered = look.layered;
    let show_tools = look.show_tools;

    quads.begin(gpu);
    if layered {
        // 卡片本體與陰影。DWM 那條的底色是清除色，圓角與陰影是系統畫的
        quads.push(
            Quad::new(pad, pad, cw, ch)
                .radius(RADIUS * s)
                .color([t.bg[0], t.bg[1], t.bg[2], 1.0])
                .shadow(0.0, 8.0 * s, 20.0 * s, 0.0, theme::scrim(0.5)),
        );
    }
    // 封面糊成的底色只透一點，這裡用取色的光暈代替：從封面那側淡淡地亮過來
    quads.push(
        Quad::new(px(l.art.x), px(l.art.y), l.art.w * s, l.art.h * s)
            .radius(10.0 * s)
            .color([0.0; 4])
            .shadow(0.0, 0.0, 60.0 * s, 10.0 * s, glow(0.10)),
    );
    // 沿著圓角的一圈細邊，讓卡片邊緣在深色桌面上也分得出來
    quads.push(
        Quad::new(pad + 1.0 * s, pad + 1.0 * s, cw - 2.0 * s, ch - 2.0 * s)
            .radius((RADIUS - 1.0) * s)
            .color([0.0; 4])
            .border(1.0 * s, theme::ink(0.07)),
    );
    // 封面：一圈柔光＋往下的影子
    let (ax, ay, aw) = (px(l.art.x), px(l.art.y), l.art.w * s);
    quads.push(Quad::new(ax, ay, aw, aw).radius(10.0 * s).color([0.0; 4]).shadow(0.0, 0.0, 18.0 * s, 0.0, glow(0.16)));
    quads.push(
        Quad::new(ax, ay, aw, aw)
            .radius(10.0 * s)
            .color(theme::ink(0.06))
            .shadow(0.0, 4.0 * s, 14.0 * s, 0.0, theme::scrim(0.55)),
    );
    if let Some(art) = &c.art {
        quads.push_tex(Quad::new(ax, ay, aw, aw).radius(10.0 * s).cover(1.0, 1.0), art);
    }

    // 進度條
    text.begin();
    text.cap_scale(l.k);
    let fg = [0.933, 0.953, 0.945, 1.0];
    let dim = [t.dim[0], t.dim[1], t.dim[2], 1.0];
    let time = if c.has { format!("{}/{}", fmt(c.pos_ms.min(c.dur_ms)), fmt(c.dur_ms)) } else { "0:00/0:00".into() };
    // 時間比歌手名亮：CSS 版用 --dim，在深底上太淡看不清（使用者回報）。往白色拉一半、字重 500
    let time_ink = [
        (t.dim[0] + fg[0]) * 0.5,
        (t.dim[1] + fg[1]) * 0.5,
        (t.dim[2] + fg[2]) * 0.5,
        0.95,
    ];
    let time_w = if l.show_time {
        text.measure_weight(&time, 10.5 * s, 14.0 * s, None, 500).0 / s
    } else {
        0.0
    };
    let track_w = (l.meta_w - if l.show_time { time_w + 10.0 } else { 0.0 }).max(10.0);
    let (tx, ty) = (px(l.meta_x), px(l.track_y) - 2.5 * s);
    quads.push(Quad::new(tx, ty, track_w * s, 5.0 * s).radius(3.0 * s).color(theme::ink(0.08)));
    let frac = if c.has && c.dur_ms > 0 { (c.pos_ms as f32 / c.dur_ms as f32).clamp(0.0, 1.0) } else { 0.0 };
    if frac > 0.0 {
        quads.push(
            Quad::new(tx, ty, track_w * s * frac, 5.0 * s)
                .radius(3.0 * s)
                .color(glow(0.85))
                .shadow(0.0, 0.0, 8.0 * s, 0.0, glow(0.45)),
        );
    }
    // 進度最前端的光點：25% 的取色混白
    let dot = [0.25 * t.glow[0] + 0.75, 0.25 * t.glow[1] + 0.75, 0.25 * t.glow[2] + 0.75, 1.0];
    let dx = tx + track_w * s * frac;
    quads.push(
        Quad::new(dx - 4.5 * s, px(l.track_y) - 4.5 * s, 9.0 * s, 9.0 * s)
            .radius(4.5 * s)
            .color(dot)
            .shadow(0.0, 0.0, 10.0 * s, 1.0 * s, glow(0.6)),
    );

    // 文字。歌名、歌手放不下的在框裡來回捲動，`wait` 收最快要再動的那個
    let mut wait: Option<f32> = None;
    let mut roll = |text: &mut Text, s: &str, at: (f32, f32, f32), font: (f32, f32, u16), color| {
        if let Some(w) = crate::widgets::roll_line(text, look.roll, look.s, s, at, font, color, None, None) {
            wait = Some(wait.map_or(w, |v: f32| v.min(w)));
        }
    };
    let title_max = l.meta_w - if show_tools { 48.0 } else { 0.0 };
    let title = if c.has { c.title } else { "沒有在播放" };
    let (ts, tlh) = (l.title_size * s, l.title_size * 1.3 * s);
    let (az, alh) = (l.artist_size * s, l.artist_size * 1.3 * s);
    if l.lyric.is_some() && c.has && !c.artist.is_empty() {
        // 歌詞模式：歌手併到歌名後面，小一號、暗色。
        // 都放得下就照原樣；放不下時歌手最多讓到四成，其餘給歌名
        let artist = format!("·  {}", c.artist);
        let gap = 6.0 * s;
        let avail = title_max * s;
        let tw = text.measure_weight(title, ts, tlh, None, 600).0;
        let aw = text.measure(&artist, az, alh, None).0;
        let a_budget = if tw + gap + aw <= avail { aw } else { aw.min((avail - gap - tw).max(avail * 0.4)) };
        let t_budget = (avail - gap - a_budget).max(10.0);
        let ty = px(l.title_y) + text.lift(tlh);
        roll(text, title, (tx, ty, t_budget), (ts, tlh, 600), fg);
        // 行高都是 1.3 倍，基線約在「行頂＋字級」，往下挪字級差就對齊歌名的基線
        let ay = px(l.title_y) + (ts - az) * l.k + text.lift(alh);
        roll(text, &artist, (tx + tw.min(t_budget) + gap, ay, a_budget.max(10.0)), (az, alh, 400), dim);
    } else {
        let ty = px(l.title_y) + text.lift(tlh);
        roll(text, title, (tx, ty, title_max * s), (ts, tlh, 600), fg);
    }
    if let (Some(y), true) = (l.artist_y, c.has) {
        let ay = px(y) + text.lift(alh);
        roll(text, c.artist, (tx, ay, l.meta_w * s), (az, alh, 400), dim);
    }
    // 歌詞兩行：當前句（亮）、下一句（暗）。換句時整組往上推一行，
    // 舊的那句往上淡出、新的下一句從底下淡入，超出這兩行的部分裁掉
    if let (Some(r), true, Some(ly)) = (l.lyric, c.has, c.lyric.as_ref()) {
        // `lhp` 是交給 Text 的行高（它自己乘字級倍率），`step` 是放大後真的一行多高
        let (z, lhp) = (r.size * s, r.lh * s);
        let step = lhp * l.k;
        let y0 = px(r.y);
        let rows = if r.two { 2.0 } else { 1.0 };
        let clip = glyphon::TextBounds {
            left: 0,
            top: y0.floor() as i32,
            right: w as i32,
            bottom: (y0 + rows * step).ceil() as i32,
        };
        let maxw = l.meta_w * s;
        let k = slide.map_or(1.0, |(_, p)| crate::anim::STD.at(p));
        let dy = (1.0 - k) * step;
        // 頂端對齊（見 `Text::lift`）
        let y0 = y0 + text.lift(lhp);
        let with_a = |c: [f32; 4], a: f32| [c[0], c[1], c[2], c[3] * a];
        if let Some((prev, _)) = slide {
            let f = text.fit_weight(prev, z, lhp, maxw, 500);
            text.push(&f, tx, y0 - k * step, z, lhp, with_a(fg, 1.0 - k), 500, None, Some(clip));
        }
        // 沒有同步歌詞時 cur 是說明文字，用暗色
        let ink = if ly.idx == i64::MIN { dim } else { fg };
        let ink = [0, 1, 2, 3].map(|i| dim[i] + (ink[i] - dim[i]) * k);
        let cur = if ly.cur.is_empty() { "♪" } else { ly.cur };
        let f = text.fit_weight(cur, z, lhp, maxw, 500);
        text.push(&f, tx, y0 + dy, z, lhp, ink, 500, None, Some(clip));
        if r.two && !ly.next.is_empty() {
            let f = text.fit(ly.next, z, lhp, maxw);
            text.push(&f, tx, y0 + step + dy, z, lhp, with_a(dim, k), 400, None, Some(clip));
        }
    }
    if l.show_time {
        text.push(&time, tx + (track_w + 10.0) * s, px(l.track_y) - 7.0 * s, 10.5 * s, 14.0 * s, time_ink, 500, None, None);
    }

    // 右上角工具與右下角把手：滑鼠移進來才出現，鎖定時整組拿掉
    if show_tools {
        for (r, which) in [(l.lock, Hit::Lock), (l.close, Hit::Close)] {
            let hot = look.hot == Some(which);
            quads.push(
                Quad::new(px(r.x), px(r.y), r.w * s, r.h * s)
                    .radius(6.0 * s)
                    .color(if hot { theme::ink(0.16) } else { theme::scrim(0.3) }),
            );
            let (cx, cy) = (px(r.x + r.w / 2.0), px(r.y + r.h / 2.0));
            let ink = theme::ink(0.9);
            match which {
                Hit::Lock => {
                    // 鎖：上半圈鎖環（只畫外框）＋鎖身
                    quads.push(
                        Quad::new(cx - 3.0 * s, cy - 6.0 * s, 6.0 * s, 9.0 * s)
                            .radius(3.0 * s)
                            .color([0.0; 4])
                            .border(1.6 * s, ink),
                    );
                    quads.push(Quad::new(cx - 5.0 * s, cy - 1.0 * s, 10.0 * s, 7.0 * s).radius(1.5 * s).color(ink));
                }
                _ => {
                    // 叉：兩條轉 ±45° 的圓頭線
                    for a in [45.0f32, -45.0] {
                        quads.push(
                            Quad::new(cx - 0.9 * s, cy - 6.0 * s, 1.8 * s, 12.0 * s)
                                .radius(0.9 * s)
                                .color(ink)
                                .rotate(a.to_radians()),
                        );
                    }
                }
            }
        }
        // 把手：兩道斜線
        let g = l.grip;
        for (len, off) in [(10.0f32, 0.0f32), (5.0, 3.5)] {
            let (cx, cy) = (px(g.x + 6.0 + off), px(g.y + 6.0 + off));
            quads.push(
                Quad::new(cx - 0.6 * s, cy - len / 2.0 * s, 1.2 * s, len * s)
                    .color(theme::ink(0.3))
                    .rotate(45f32.to_radians()),
            );
        }
    }
    text.prepare(gpu, w, h)?;

    // 分層：一定要清成全透明，分層視窗看的是 alpha，殘留的底色會變成一塊板子
    let clear = if layered {
        wgpu::Color::TRANSPARENT
    } else {
        wgpu::Color { r: t.bg[0] as f64, g: t.bg[1] as f64, b: t.bg[2] as f64, a: 1.0 }
    };
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    {
        let mut pass = gfx::pass(&mut enc, &target.view, "mini", Some(clear));
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    }
    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok(wait)
}

impl Drop for Mini {
    fn drop(&mut self) {
        OPEN.store(false, Ordering::Relaxed);
    }
}

fn cursor_screen() -> (i32, i32) {
    let mut p = windows::Win32::Foundation::POINT::default();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(r: u8, g: u8, b: u8) -> Vec<u8> {
        [r, g, b, 255].repeat(24 * 24)
    }

    #[test]
    fn webp_封面解得開() {
        // Spotify 有些封面是 WebP，image 沒開 webp 時這裡會是 None、強調色退回綠
        let mut webp = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut webp)
            .encode(&solid(200, 40, 40), 24, 24, image::ExtendedColorType::Rgba8)
            .unwrap();
        let t = tint_from_bytes(&webp).expect("WebP 解不開");
        assert!(t.glow[0] > t.glow[1] && t.glow[0] > t.glow[2], "{t:?}");
    }

    #[test]
    fn 紅色封面取到紅色而且灰階退回不帶色() {
        let t = tint_from_pixels(&solid(200, 40, 40));
        assert!(t.glow[0] > t.glow[1] && t.glow[0] > t.glow[2], "{t:?}");
        // 光點亮度固定在 0.72 附近
        let (_, _, l) = rgb_to_hsl(t.glow[0], t.glow[1], t.glow[2]);
        assert!((l - 0.72).abs() < 0.02, "{l}");
        assert_eq!(tint_from_pixels(&solid(128, 128, 128)), grey_tint());
    }

    #[test]
    fn 色相環在紅色附近不會算歪() {
        // 一半偏紫紅（350°）一半偏橘紅（10°），向量平均應該落在 0° 附近而不是 180°
        let mut px = Vec::new();
        for i in 0..24 * 24 {
            let (r, g, b) = if i % 2 == 0 { (220, 40, 75) } else { (220, 75, 40) };
            px.extend_from_slice(&[r, g, b, 255]);
        }
        let t = tint_from_pixels(&px);
        let (h, _, _) = rgb_to_hsl(t.glow[0], t.glow[1], t.glow[2]);
        assert!(!(30.0..330.0).contains(&h), "色相 {h}");
    }

    #[test]
    fn 版面塞得進最小尺寸() {
        let l = layout(MIN.0, MIN.1, false, 1.0);
        assert!(l.art.x + l.art.w < l.meta_x);
        assert!(l.artist_y.is_none(), "64 高時不放歌手");
        assert!(!l.show_time, "200 寬時不放時間");
        let l = layout(DEFAULT.0, DEFAULT.1, false, 1.0);
        assert!(l.title_y > 0.0 && l.track_y + 4.5 < DEFAULT.1, "{} {}", l.title_y, l.track_y);
        assert_eq!(hit(DEFAULT.0, DEFAULT.1, l.close.x + 5.0, l.close.y + 5.0, false), Some(Hit::Close));
        // 鎖定時工具不算
        assert_eq!(hit(DEFAULT.0, DEFAULT.1, l.close.x + 5.0, l.close.y + 5.0, true), None);
    }

    #[test]
    fn 歌詞模式_預設大小放兩行_最小只放一行_都不壓到進度條() {
        for (h, two) in [(DEFAULT.1, true), (MIN.1, false)] {
            let l = layout(DEFAULT.0, h, true, 1.0);
            let r = l.lyric.expect("歌詞模式要有歌詞列");
            assert_eq!(r.two, two, "高 {h}");
            assert!(l.artist_y.is_none(), "歌手併到歌名那行");
            assert!(l.title_y >= 0.0, "高 {h}：{}", l.title_y);
            let bottom = r.y + if r.two { 2.0 } else { 1.0 } * r.lh;
            assert!(bottom < l.track_y - 4.5, "高 {h}：歌詞底 {bottom} 進度條 {}", l.track_y);
            assert!(l.track_y + 4.5 <= h, "高 {h}");
        }
    }

    #[test]
    fn 字級放大時行高跟著放大_放不下就降倍率_不壓到進度條() {
        for (h, lyrics) in [(DEFAULT.1, true), (MIN.1, true), (DEFAULT.1, false)] {
            let l = layout(DEFAULT.0, h, lyrics, 1.5);
            assert!(l.k > 1.0 && l.k <= 1.5, "高 {h}：倍率 {}", l.k);
            assert!(l.title_y >= 0.0, "高 {h}：{}", l.title_y);
            if let Some(r) = l.lyric {
                let bottom = r.y + if r.two { 2.0 } else { 1.0 } * r.lh * l.k;
                assert!(bottom < l.track_y - 4.5, "高 {h}：歌詞底 {bottom} 進度條 {}", l.track_y);
            }
            assert!(l.track_y + 4.5 <= h, "高 {h}");
        }
        // 倍率 1 時跟原本一樣
        let a = layout(DEFAULT.0, DEFAULT.1, true, 1.0);
        assert_eq!(a.k, 1.0);
    }
}
