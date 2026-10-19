//! VitroDeck-rs —— 全原生 Rust 重寫。
//!
//! 三個視圖用 Tab 循環：封面流 → 歌詞 → 視覺效果，跟 Python 版一致。
//!
//! 命令列：
//!   `--probe`             不開視窗，驗後端各層
//!   `--shot <檔> [場景]`   離屏算一張圖存檔（原生沒有 DevTools，這是驗證手段）

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod albums;
mod anim;
mod art;
mod audio;
mod config;
mod clipboard;
mod cover;
mod eel;
mod extract;
mod gfx;
mod icons;
mod instance;
mod layered;
mod ledfeed;
mod glass;
mod lyrics;
mod milk;
mod milkasset;
mod mini;
mod oauth;
mod player;
mod preset;
mod prefs;
mod pv;
mod quad;
mod scene_about;
mod scene_flow;
mod scene_help;
mod scene_now;
mod scene_search;
mod scene_settings;
mod scene_toast;
mod scene_devices;
mod scene_setup;
mod scene_viz;
mod search;
mod settings;
mod shader;
mod shot;
mod smtc;
mod spotify;
mod viz;
mod vinyl;
mod text;
mod theme;
mod tray;
mod update;
mod ui;
mod widgets;
mod zh;

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Browse,
    Now,
    Viz,
}

/// 背景換圖的交叉淡化（CSS 的 `background-image .7s ease`）。
#[derive(Default)]
struct BgFade {
    cur: Option<Arc<wgpu::TextureView>>,
    prev: Option<(Arc<wgpu::TextureView>, Instant)>,
}

const BG_FADE_S: f32 = theme::dur::BG_FADE;

impl BgFade {
    /// 這一幀的背景是 `now`。換了就把舊的留著淡出；回傳要墊在底下的上一張與它的權重。
    fn track(&mut self, now: Option<&Arc<wgpu::TextureView>>) -> Option<(Arc<wgpu::TextureView>, f32)> {
        let same = match (&self.cur, now) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            if let Some(old) = self.cur.take() {
                self.prev = Some((old, Instant::now()));
            }
            self.cur = now.cloned();
        }
        let (p, t0) = self.prev.as_ref()?;
        let t = t0.elapsed().as_secs_f32() / BG_FADE_S;
        if t >= 1.0 {
            self.prev = None;
            return None;
        }
        Some((p.clone(), 1.0 - anim::EASE.at(t)))
    }
}

/// 強調色跟著現在播的封面。換歌時跟背景一樣淡 0.7 秒過去。
/// `None` 的意思一律是「調色盤的綠」：沒在播、封面是灰階、設定關掉。
#[derive(Default)]
struct AccentFade {
    /// 已經取過色的封面（網址或 SMTC 的曲目 id）
    key: String,
    tried: Option<Instant>,
    from: Option<[f32; 3]>,
    to: Option<[f32; 3]>,
    t0: Option<Instant>,
}

impl AccentFade {
    /// 封面還沒下載好就一秒後再試，跟迷你視窗的 `want_tint` 一樣
    fn want(&mut self, key: &str) -> bool {
        if self.key == key || self.tried.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return false;
        }
        self.tried = Some(Instant::now());
        true
    }

    fn aim(&mut self, key: &str, c: Option<[f32; 3]>) {
        self.key = key.to_string();
        self.tried = None;
        if c != self.to {
            self.from = Some(self.now());
            self.to = c;
            self.t0 = Some(Instant::now());
        }
    }

    /// 這一幀的顏色。淡完之後回 `to` 本身，`None` 就原封不動地交回調色盤
    fn current(&mut self) -> Option<[f32; 3]> {
        let t = self.t0.map_or(1.0, |t0| t0.elapsed().as_secs_f32() / BG_FADE_S);
        if t >= 1.0 {
            self.t0 = None;
            return self.to;
        }
        let green = theme::palette().accent;
        let a = self.from.unwrap_or([green[0], green[1], green[2]]);
        let b = self.to.unwrap_or([green[0], green[1], green[2]]);
        let k = anim::EASE.at(t);
        Some([0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * k))
    }

    fn now(&mut self) -> [f32; 3] {
        let g = theme::palette().accent;
        self.current().unwrap_or([g[0], g[1], g[2]])
    }
}

/// 視圖轉場。封面流 ⇄ 歌詞頁是共享元素：中央那張封面由替身飛過去，
/// 其餘淡出淡入；進出視覺效果頁沒有共通的元素，單純淡入淡出。
/// 對應 Python 版的 `crossView`／`flyBetween` 與 style.css 的 `.v-in`／`.v-out`。
struct Trans {
    from: View,
    to: View,
    start: Instant,
    /// 有替身可飛（封面流 ⇄ 歌詞頁，而且封面流上有東西）
    shared: bool,
    /// 替身的起訖。第一幀才算（要版面與材質）
    fly: Option<Fly>,
    /// 這一幀的轉場時刻。一幀只取樣一次（`animate` 開頭）：
    /// 封面流、歌詞頁、替身各自去讀時鐘的話，中間隔著別頁的繪圖時間，
    /// 會出現「封面流還在藏中央那張、替身卻已經落地收掉」的那一幀 —— 中央整個空掉，看起來就是閃一下
    now: f32,
    /// 按下去那一刻的 chrome。離開歌詞頁的途中 PV 左欄照這個畫：到了封面流控制列會升起來，
    /// 跟著升的話留在原地淡出的唱片會一邊變大
    chrome0: f32,
}

struct Fly {
    from: ui::Rect,
    to: ui::Rect,
    /// 圓角：封面流的 6 → 唱片標籤的一半（變圓），或歌詞頁方形封面的 14
    r0: f32,
    r1: f32,
    /// 唱片模式要轉到唱片當下的角度，落地才接得上
    rot0: f32,
    rot1: f32,
    img: Arc<wgpu::TextureView>,
    /// 起點與終點是同一張圖：落地直接換掉；不同張才交錯 140ms
    same: bool,
}

/// 替身飛多久（app.js 的 `VIEW_ANIM_MS`）
const LAND_S: f32 = theme::dur::LAND;

impl Trans {
    fn t(&self) -> f32 {
        self.now
    }

    fn done(&self) -> bool {
        // 共享：飛完再加交錯；其他：淡完（.7 × .34）就好
        self.t() > if self.shared { LAND_S + 0.16 } else { 0.26 }
    }

    /// 封面流 ⇄ 歌詞頁（不論有沒有替身），控制列要收下去或升上來
    fn tucks(&self) -> bool {
        matches!((self.from, self.to), (View::Browse, View::Now) | (View::Now, View::Browse))
    }
}

/// 展開／收起曲目：先縮小淡出，**中途**才真的換內容，再從稍大淡入。
struct Swap {
    start: Instant,
    /// 還沒套用的內容變更；套用之後記下時刻，進場從那裡起算
    #[allow(clippy::type_complexity)]
    apply: Option<Box<dyn FnOnce(&mut App)>>,
    applied: Option<Instant>,
}

const SWAP_OUT_S: f32 = theme::dur::SWAP_OUT;
const SWAP_IN_S: f32 = theme::dur::SWAP_IN;

impl Swap {
    /// (縮放, 透明度)
    fn fx(&self) -> (f32, f32) {
        match self.applied {
            None => {
                let p = anim::EASE_IN.at(self.start.elapsed().as_secs_f32() / SWAP_OUT_S);
                (1.0 - 0.08 * p, 1.0 - p)
            }
            Some(t) => {
                let q = anim::OUT.at(t.elapsed().as_secs_f32() / SWAP_IN_S);
                (1.06 - 0.06 * q, q)
            }
        }
    }

    fn done(&self) -> bool {
        self.applied.is_some_and(|t| t.elapsed().as_secs_f32() >= SWAP_IN_S)
    }
}

/// KTV 提詞這一幀的內容：(目前這句, 下一句, 唱到哪 0..1)。Python 版 ktvTick 的計算：
/// 有逐字就照「已唱完幾個字」算（字內按比例補），只有行級時間戳就用到下一句的間隔內插。
fn ktv_view(l: &lyrics::Lyrics, i: i64, pos: i64, dur: i64) -> (String, String, f32) {
    let cur = (i >= 0).then(|| &l.synced[i as usize]);
    let next = l.synced.get((i + 1) as usize).map(|n| n.text.clone()).unwrap_or_default();
    let Some(line) = cur else {
        return (String::new(), next, 0.0);
    };
    // 空白的那句（間奏）顯示音符，不要整行空掉
    let text = if line.text.trim().is_empty() { "♪".to_string() } else { line.text.clone() };
    let pct = match line.words.as_ref().filter(|w| !w.is_empty()) {
        Some(ws) => {
            let (mut total, mut sung) = (0.0f32, 0.0f32);
            for w in ws {
                let n = w.text.chars().count().max(1) as f32;
                total += n;
                if pos >= w.t + w.d {
                    sung += n;
                } else if pos > w.t {
                    sung += n * (pos - w.t) as f32 / w.d.max(1) as f32;
                }
            }
            if total > 0.0 { sung / total } else { 0.0 }
        }
        None => {
            let t0 = line.t;
            let t1 = l.synced.get((i + 1) as usize).map_or(t0 + 2500.max(dur - t0), |n| n.t);
            if t1 > t0 { (pos - t0) as f32 / (t1 - t0) as f32 } else { 1.0 }
        }
    };
    (text, next, pct.clamp(0.0, 1.0))
}

/// 幽靈換圖的脈衝（CSS 的 `cover-swap` 關鍵影格，每一段各自套曲線）：(縮放, 透明度)
fn pulse_fx(t0: Instant) -> Option<(f32, f32)> {
    let t = t0.elapsed().as_secs_f32() / 0.46;
    if t >= 1.0 {
        return None;
    }
    Some(if t < 0.6 {
        let k = anim::SOFT.at(t / 0.6);
        (anim::lerp(0.88, 1.02, k), anim::lerp(0.2, 1.0, k))
    } else {
        let k = anim::SOFT.at((t - 0.6) / 0.4);
        (anim::lerp(1.02, 1.0, k), 1.0)
    })
}

/// 從別的執行緒丟進 winit 事件迴圈的事。
#[derive(Debug, Clone, Copy)]
enum UserEv {
    /// 第二份啟動了：把這份叫到前面
    Activate,
    /// 系統列圖示：左鍵或右鍵選單
    Tray(tray::Cmd),
    /// 更新裝好了、新版已經叫起來：這份結束
    UpdateReady,
}

/// F11 的三段：視窗、這個螢幕、跨所有螢幕。
///
/// 第三段不是 winit 的全螢幕 —— `Fullscreen::Borderless` 一次只吃得下
/// 一個螢幕。跨螢幕是「無邊框視窗擺滿整個虛擬桌面」。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScreenMode {
    Windowed,
    One,
    Span,
}

impl ScreenMode {
    fn key(self) -> &'static str {
        match self {
            ScreenMode::Windowed => "window",
            ScreenMode::One => "one",
            ScreenMode::Span => "span",
        }
    }

    /// F11 的下一段，只在「視窗 ＋ `f11_modes` 留下的」之間轉。
    /// 目前這段不在循環裡（從設定直接切過去的）就回視窗 —— 跟 Python 版一樣。
    fn next_in(self, modes: &[String]) -> Self {
        let seq: Vec<ScreenMode> = std::iter::once(ScreenMode::Windowed)
            .chain([ScreenMode::One, ScreenMode::Span].into_iter().filter(|m| modes.iter().any(|k| k == m.key())))
            .collect();
        match seq.iter().position(|m| *m == self) {
            Some(i) => seq[(i + 1) % seq.len()],
            None => ScreenMode::Windowed,
        }
    }
}

/// 介面放大後，主視窗的版面至少要有這麼大（邏輯像素）。快捷鍵、舊設定面板這些固定大小的面板
/// 是照 720 高設計的，再小就超出視窗
const MAIN_MIN: (f32, f32) = (960.0, 640.0);

/// 主視窗這一幀的縮放：系統縮放 × 介面倍率，全螢幕放不下 `MAIN_MIN` 時降到放得下（見 `gfx::zoomed_fit`）
fn main_scale(w: &Window) -> f32 {
    let sz = w.inner_size();
    gfx::zoomed_fit(w.scale_factor(), (sz.width, sz.height), MAIN_MIN)
}

/// 介面放大時，視窗模式的主視窗最小要能放下 `MAIN_MIN`：設成最小視窗大小，比它小的視窗
/// Windows 會自己撐大。全螢幕不設（不能比螢幕大），沒放大時也不設（維持原本可以縮很小）
fn sync_min_size(w: &Window, mode: ScreenMode) {
    let z = gfx::ui_zoom();
    let min = (z > 1.0 && mode == ScreenMode::Windowed)
        .then(|| winit::dpi::LogicalSize::new(MAIN_MIN.0 * z, MAIN_MIN.1 * z));
    w.set_min_inner_size(min);
    // 最小大小只擋之後的拖曳，現在已經比它小的視窗不會自己變大（實測），要主動撐開
    if let Some(m) = min {
        let now = w.inner_size().to_logical::<f32>(w.scale_factor());
        if now.width < m.width || now.height < m.height {
            let _ = w.request_inner_size(winit::dpi::LogicalSize::new(now.width.max(m.width), now.height.max(m.height)));
        }
    }
}

/// 所有螢幕的外接矩形（實體像素）。回傳 `(x, y, 寬, 高)`。
///
/// 螢幕可以擺在原點的左邊或上面，所以 x、y 會是負的 —— 用 0 當起點
/// 的話，主螢幕左邊那台就會被切掉。
fn span_rect(win: &Window) -> Option<(i32, i32, u32, u32)> {
    let mut it = win.available_monitors();
    let first = it.next()?;
    let (mut x0, mut y0) = (first.position().x, first.position().y);
    let (mut x1, mut y1) = (
        x0 + first.size().width as i32,
        y0 + first.size().height as i32,
    );
    for m in it {
        let (p, sz) = (m.position(), m.size());
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x + sz.width as i32);
        y1 = y1.max(p.y + sz.height as i32);
    }
    Some((x0, y0, (x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32))
}

/// 滑鼠靜止多久之後把控制列收起來。**只跟滑鼠移動綁定** ——
/// 鍵盤操作不該讓它重新出現，那是使用者明確要求過的行為。
const IDLE_HIDE: Duration = Duration::from_millis(3000);

/// 閒置（多久沒操作）或失焦時，主視窗最多每秒畫幾幀。實際是螢幕更新率的整數分之一，
/// 幀才會平均落在垂直同步上（180 Hz 每三格畫一次、144 Hz 每兩格）。
/// 量過：180 Hz 全速閒置時一顆核心的 25～34%，成本跟幀率成正比（第 7 階段）
const IDLE_FPS: f32 = 60.0;
const IDLE_AFTER: Duration = Duration::from_millis(3000);

/// 沒在播又沒人動多久之後降到幾 fps（從 `IDLE_AFTER` 的 60 接著往下），最後一段過了就停畫。
/// 分段是因為一下子停住的話，PV 的光粒、流光會直接凍在半空中，很突兀
const CALM: [(Duration, f32); 3] =
    [(Duration::from_secs(20), 60.0), (Duration::from_secs(40), 30.0), (Duration::from_secs(60), 10.0)];
/// 停畫時多久看一次該不該醒（播放恢復、換歌、poller 有話要說）。這些沒有事件可等
const PARK_POLL: Duration = Duration::from_millis(200);
/// 文字 PV 的節拍擷取在這首不能演之後還留多久：切歌時歌詞先變「載入中」，
/// 下一首有逐字的話馬上又要用，不必關掉重開、節拍重新抓
const PV_EAR_KEEP: Duration = Duration::from_secs(8);

/// 主視窗這一幀怎麼畫（見 `App::pace`）
#[derive(Clone, Copy, PartialEq, Debug)]
enum Pace {
    /// 照更新率上限畫，沒設上限就跟著垂直同步
    Full,
    /// 降到大約這個 fps
    Slow(f32),
    /// 不畫了，等有事再叫醒（`App::park`）
    Park,
}

/// 主視窗停畫的狀態。`hidden`：因為縮小（或被系統判定看不見）才停的，看得見就醒；
/// `sig`：停的那一刻的播放狀態、曲目、有沒有待顯示的提示，變了就醒
struct Parked {
    at: Instant,
    hidden: bool,
    sig: (bool, String, bool),
}

/// 歌詞頁用滾輪捲走、沒再碰歌詞多久之後，捲回正在唱的那句（滑鼠還指著歌詞就一直延後）
const LYRIC_HOLD: Duration = Duration::from_millis(4000);

/// 算不算使用者在操作（閒置降幀用）。滑鼠移動也算，跟控制列的 `last_mouse` 不同
fn is_input(ev: &WindowEvent) -> bool {
    matches!(
        ev,
        WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::KeyboardInput { .. }
    )
}

/// 神燈收起／展開的長度。照抄 web 版的 `HIDE_MS` / `SHOW_MS` ——
/// 收起來比展開慢一點，因為收的時候要看清楚那條帶子被吸進去。
const HIDE_S: f32 = theme::dur::HIDE;
const SHOW_S: f32 = theme::dur::SHOW;

/// 歌詞頁的控制列就是往下收，時間照 CSS 的 `--view-anim`。
const BAR_SLIDE_S: f32 = theme::dur::BAR_SLIDE;

/// 文字 PV 演的那份歌詞與唱到哪。切歌那一刻共用的歌詞就換成「載入中」了，
/// 退回一般歌詞頁的那一段要靠這份接著演（位置照本機時鐘往前推）
struct PvSnap {
    /// 曲目＋歌詞來源＋句數，變了才重存一份
    key: String,
    track: String,
    lyrics: lyrics::Lyrics,
    ms: i64,
    at: Instant,
    playing: bool,
}

impl PvSnap {
    fn ms_now(&self) -> i64 {
        self.ms + if self.playing { self.at.elapsed().as_millis() as i64 } else { 0 }
    }
}

struct Render {
    surface: wgpu::Surface<'static>,
    surf_config: wgpu::SurfaceConfiguration,
    gpu: gfx::Gpu,
    target: gfx::Offscreen,
    quads: quad::QuadRenderer,
    covers: cover::CoverRenderer,
    glass: glass::Glass,
    text: text::Text,
    blit: gfx::Blit,
    art: art::ArtCache,
    veil: scene_flow::Veil,
    /// 轉場時第二個視圖畫在這裡再疊上去。用到才建，轉場完就丟
    layer: Option<gfx::Offscreen>,
    /// 文字 PV（實驗中）。第一次打開才建，沒開過就零開銷
    pv: Option<pv::Pv>,
    /// 迷你視窗在 Win11 走交換鏈，要同一個 instance 與介面卡開 surface
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
}

/// preset 選單的狀態。畫的東西在 `scene_search`。
struct Picker {
    input: search::Input,
    /// 篩選後的清單（`Viz` 的 item 索引），照名稱排
    rows: Vec<usize>,
    /// 反白那一列在 `rows` 裡的位置
    at: usize,
    /// 捲到的第一列
    top: usize,
    /// 開之前那顆與鎖定狀態。Esc 要原樣放回去 —— 上下鍵是即時預覽，
    /// 不還原的話「取消」就沒有意義。
    was: Option<usize>,
    was_locked: bool,
    fit: scene_search::FitCache,
}

/// 展開中：封面流換成這張專輯／清單裡的曲目（web 版的 `S.drill`）。
struct Drill {
    src: spotify::Album,
    tracks: Vec<spotify::Album>,
    /// 展開前站在第幾格。退出時找不到原本那張（例如幽靈在這期間被拿掉）就回到這裡
    at: usize,
}

struct App {
    window: Option<Arc<Window>>,
    r: Option<Render>,
    player: player::Handle,
    spec: audio::Shared,
    paths: config::Paths,

    library: Vec<spotify::Album>,
    /// 上一次讀進 `library` 時的版本號，用來發現 poller 抓了新的清單
    albums_rev: u64,
    /// 已經自動定位過了。**只做一次** —— 每次換歌都跳位的話，
    /// 使用者正在翻的時候會被搶走控制權。
    positioned: bool,
    /// 使用者自己動過捲軸／點過封面。動過之後就不再自動跳位。
    user_touched: bool,
    /// 正在播、但不在收藏裡的那張專輯。不插的話使用者會看到
    /// 「明明在播卻找不到它在哪」。
    ghost: Option<spotify::Album>,
    /// 幽靈插在第幾個位置。**不是清單最後** —— 是使用者正在看的位置，
    /// 所以它會變成中央那張，其餘往兩側排開。
    ghost_at: usize,
    fake: Vec<scene_flow::Album>,
    drill: Option<Drill>,
    /// 專輯／清單的 uri → 曲目。本次執行內不重抓，省配額（web 版的 `drillTracks`）
    track_cache: std::collections::HashMap<String, Vec<spotify::Album>>,
    /// 正在抓哪一張的曲目。抓的期間再按不要重送
    drill_loading: Option<spotify::Album>,
    /// 上次看到的 `Shared::tracks_rev`
    tracks_seen: u64,
    /// 上次跟過的換歌（`Shared::generation`）。展開中換歌時，封面流跟著跳到那一首
    follow_gen: u64,
    /// 返回鍵上名字的寬度。畫的時候量，命中測試拿同一個數字
    back_w: f32,
    /// 正在拖音量滑桿
    vol_drag: bool,
    /// 正在拖進度條，拖到哪（0..1）。放開才送出
    seek_drag: Option<f32>,
    /// 給系統列圖示的回呼用：把點擊丟回 winit 的事件迴圈
    proxy: winit::event_loop::EventLoopProxy<UserEv>,
    /// 主視窗縮在系統列（藏起來、停畫）
    in_tray: bool,
    mini: Option<mini::Mini>,
    /// 上次告訴 poller 的「迷你視窗要歌詞」
    mini_ly_sent: bool,
    /// 下一次 `sync_mini` 要把迷你視窗叫回主螢幕右下角
    mini_recall: bool,
    /// 設定視窗。`prefs_want`：有人要開（齒輪、逗號鍵），在 about_to_wait 開或拉到前面
    prefs: Option<prefs::Prefs>,
    prefs_want: bool,
    updater: Arc<update::Updater>,
    /// 幽靈進退場的進度 0..1，退場中、換圖脈衝（Python 版的 ghostAnim／pulseGhost）
    ghost_anim: f32,
    ghost_removing: bool,
    ghost_pulse: Option<Instant>,
    /// 背景換圖的交叉淡化，封面流與歌詞頁各一份
    bg_flow: BgFade,
    bg_now: BgFade,
    accent: AccentFade,
    /// 展開／收起曲目的縮放淡出（Python 版的 swapFlow）
    swap: Option<Swap>,
    trans: Option<Trans>,
    /// KTV 提詞目前是第幾句、何時換的（換句往上推的動畫）
    ktv_idx: i64,
    ktv_at: Instant,
    /// 歌詞捲到第幾句（補間過的），以及它是對哪一份歌詞算的（換歌、歌詞到貨就直接跳）
    lyric_scroll: anim::Tween,
    lyric_for: String,
    /// 亮的那句（補間過的「正在唱第幾句」）。平常跟 `lyric_scroll` 一起走，用滾輪捲走時分開
    lyric_focus: anim::Tween,
    /// 用滾輪捲到第幾句、最後一次碰歌詞的時間。`None` 就是跟著正在唱的那句
    lyric_manual: Option<(f32, Instant)>,
    /// 自己捲的程度 0..1（遠處的句子跟著亮起來）
    lyric_browse: anim::Tween,
    /// 滑鼠停著的那句、剛離開的那句、換的時間（底色淡入淡出）
    lyric_hover: (Option<usize>, Option<usize>, Instant),
    /// 剛點的那句與時間
    lyric_flash: Option<(usize, Instant)>,
    /// 純文字歌詞捲到幾 px：補間過的與目標
    plain_scroll: anim::Tween,
    plain_target: f32,
    /// 上一幀每句畫在哪，點擊與滑鼠停留拿來比對
    lyric_hits: scene_now::LyricHits,
    /// 曲名、歌手放不下時來回捲動的時鐘：對哪一首、哪一頁算的，從何時起算（換歌、換頁就從頭捲）
    roll_at: (String, Instant),
    /// 歌詞頁的唱盤，以及上一幀在哪個視圖（判斷「剛進歌詞頁」用）
    vinyl: vinyl::Vinyl,
    vinyl_view: View,
    /// VITRO_TRANS_DUMP 自動走到第幾步
    dump_step: u8,
    /// 還沒送出去的音量。拖動時每次送都吃配額，所以節流成 200ms 一次
    vol_pending: Option<u8>,
    vol_sent: Instant,
    /// 靜音前的音量，取消靜音時放回去
    vol_before_mute: u8,
    /// 上次問音量的時間。滑鼠移到音量上、距離上次夠久才再問
    vol_synced: Option<Instant>,
    viz: viz::Viz,
    cfg: config::Config,

    view: View,
    centre: f32,
    target_centre: f32,
    screen_mode: ScreenMode,
    /// 進全螢幕之前的視窗位置與大小，退出時放回去。
    windowed: Option<(winit::dpi::PhysicalPosition<i32>, winit::dpi::PhysicalSize<u32>)>,

    /// 控制列的展開程度，1 = 完全展開，0 = 收進畫面中心。套過緩動的值。
    chrome: f32,
    /// 上面那個的線性版本。緩動不能直接套在自己身上，不然方向一換就會跳。
    chrome_t: f32,
    /// 文字 PV 鎖定的風格（O 鍵輪），`None` 照段落自動換。不存檔
    pv_look: Option<pv::style::Look>,
    /// 一般歌詞頁換成文字 PV 的進度（1 = PV）。目標是 `pv_want`
    pv_mix: anim::Tween,
    /// 現在該不該是 PV：開著、這首的歌詞有時間軸、切歌後待滿 `pv::HOLD_S`
    pv_want: bool,
    /// 上次看到的曲目與換到它的時刻
    pv_track: (String, Instant),
    /// PV 演的那份歌詞（見 [`PvSnap`]），退回一般歌詞頁之後清掉
    pv_snap: Option<PvSnap>,
    last_mouse: Instant,
    /// 游標位置（實體像素，跟畫面座標同一套）
    cursor: (f32, f32),
    /// 游標底下是什麼。每次移動重算一次，畫的時候拿來提亮按鈕。
    hover: Option<ui::Hit>,
    /// 游標在視窗裡、左鍵按著、視窗有焦點。玻璃的互動光、凝膠形變、失焦退後用
    cursor_in: bool,
    mouse_down: bool,
    focused: bool,
    /// 主視窗或設定視窗最後一次有輸入的時間；設定視窗有沒有焦點。閒置降幀用（`IDLE_FPS`）
    last_input: Instant,
    prefs_focused: bool,
    /// 畫面上最後一次有東西該動的時間：播放中、換歌、提示、視窗被叫回來。
    /// 跟 `last_input` 取較近的那個，算沒在播又沒人動多久（`CALM`）
    stir: Instant,
    /// 主視窗停畫中（縮小、或暫停太久）。迷你視窗與設定視窗照常
    parked: Option<Parked>,
    /// winit 說主視窗看不見（Windows 上是縮小）
    occluded: bool,
    /// 文字 PV 最後一次能演（設定開著、這首有逐字或正在退場）的時間，節拍擷取看它（`PV_EAR_KEEP`）
    pv_ear: Option<Instant>,
    /// 主視窗所在螢幕的更新率與查詢時間（每兩秒重查，拖到別的螢幕會變）
    refresh: Option<(f32, Instant)>,
    /// 設定面板開著沒。它是疊層不是視圖 —— 底下那頁照樣在動。
    settings_open: bool,
    settings_sel: usize,
    help_open: bool,
    /// 關於面板（I）。`about_confirm`：中斷連結按過一次，等第二次
    about_open: bool,
    about_confirm: bool,
    /// 裝置選單（D）。跟設定面板一樣是疊層，清單本身在 `Shared::devices`。
    devices_open: bool,
    devices_sel: usize,
    /// 裝置選單從裝置鍵裡長出來、收回去的進度（0 = 收著，1 = 開著）。
    /// 關掉之後還要畫到它縮回去為止，所以不跟 `devices_open` 綁在一起
    devices_t: anim::Tween,
    /// 底部控制列（封面流、視覺效果）的存在度。大面板蓋到它時退成 0：
    /// 玻璃不疊在玻璃上（Liquid Glass 的規則），面板收掉再凝回來
    bar_k: anim::Tween,
    /// 上次看到的 `Shared::no_device_rev`。變了代表播放因為沒裝置而失敗，要自動打開選單。
    no_device_seen: u64,
    /// 「滑到正在播的那張之後再換到歌詞頁」的排程：(目標位置, 開始時間)。
    /// 使用者中途自己捲了（`target_centre` 不再是目標）就作廢。
    lyrics_wait: Option<(f32, Instant)>,
    /// 首次設定精靈。沒有 client_id 時開程式就打開；Esc 可以先略過。
    setup: Option<scene_setup::Setup>,
    /// Ctrl／Shift 這些。貼上（Ctrl+V）要知道 Ctrl 有沒有按著
    mods: winit::keyboard::ModifiersState,
    /// 通知列：那句話與發出的時刻。一次只有一則，新的蓋掉舊的（web 版也是）。
    toast: Option<(String, Instant)>,

    /// 搜尋篩過的清單。`None` = 沒在篩選；`Some(空的)` = 篩了但沒結果，
    /// 兩者在畫面上不一樣（後者要說「找不到」）。
    filtered: Option<Vec<spotify::Album>>,
    /// 目前篩選用的查詢字串。框關掉之後還要靠它重篩（清單更新、換排序）。
    filter_q: String,
    /// 搜尋框開著。關掉之後篩選可以留著（Enter／上下鍵），
    /// 所以「框開著」跟「有篩選」是兩件事。
    search: Option<search::Input>,
    /// 搜尋框打開的時刻（出現時的回彈）
    search_at: Instant,
    /// 開搜尋框之前中央那張，Esc 要回到它
    search_keep: Option<String>,
    picker: Option<Picker>,
    /// 游標閃爍的時間基準。打字時重設 —— 打字中的游標要一直亮著，
    /// 停下來才開始閃，這是所有輸入框的手感。
    caret_t: Instant,

    last: Instant,
    /// 畫面更新率上限（0 = 不限，跟著 vsync）。
    ///
    /// WebView2 那版被瀏覽器的 rAF 綁在 60，原生版預設是螢幕的更新率
    /// （這台 165）—— 兩邊要比耗電量的話得先把這個拉平。
    fps_cap: f32,
    /// 這一幀有沒有真的送上畫面。沒有的話（算圖失敗、拿不到交換鏈）垂直同步就不會擋住迴圈，
    /// 要自己睡一下，不然會空轉吃滿一顆核心
    presented: bool,
    /// 程式開始跑的時間點，只拿來報第一幀
    boot: Instant,
    first_frame_reported: bool,
    /// 上一幀的間隔。viz 要用它推進 MilkDrop 的時間。
    dt: f64,
    frames: u32,
    fps_at: Instant,
    /// `VITRO_PROF`：這段 fps 統計期間 animate、draw 各花的總時間
    prof_cpu: (Duration, Duration),
    /// draw 裡等下一張畫布（垂直同步）的總時間。draw 的時間要扣掉它才是真的在做事
    prof_wait: Duration,
}

impl App {
    /// 封面流現在排的是哪一份：篩過的，或完整收藏。
    fn list(&self) -> &[spotify::Album] {
        self.filtered.as_deref().unwrap_or(self.base())
    }

    /// 沒篩選時封面流排的東西：展開中是曲目，否則是完整收藏。
    fn base(&self) -> &[spotify::Album] {
        match &self.drill {
            Some(d) => &d.tracks,
            None => &self.library,
        }
    }

    /// 幽靈只在沒篩選的時候插。篩選中插進去的話，它會混在搜尋結果裡，
    /// 而且位置是相對完整清單釘的，套在篩過的清單上會落在奇怪的地方。
    fn ghost_shown(&self) -> bool {
        self.ghost.is_some() && self.filtered.is_none() && self.drill.is_none()
    }

    fn album_count(&self) -> usize {
        if self.library.is_empty() {
            self.fake.len()
        } else {
            self.list().len() + usize::from(self.ghost_shown())
        }
    }

    /// 封面流第 `idx` 格是哪一張。**要把幽靈算進去** —— 幽靈插在中間，
    /// 它後面的每一張索引都差一。直接拿 `library[idx]` 的話，幽靈右邊
    /// 那一整半點下去播的都是隔壁那張。
    fn album_at(&self, idx: usize) -> Option<&spotify::Album> {
        let l = self.list();
        if self.ghost_shown() {
            let g = self.ghost_at.min(l.len());
            if idx == g {
                return self.ghost.as_ref();
            }
            if idx > g {
                return l.get(idx - 1);
            }
        }
        l.get(idx)
    }

    fn centre_index(&self) -> usize {
        self.target_centre.round().max(0.0) as usize
    }

    fn current_id(&self) -> Option<String> {
        self.album_at(self.centre_index()).map(|a| a.id.clone())
    }

    /// 立刻站到某一格，不要動畫。換清單（篩選、還原）時用 ——
    /// 從舊清單的位置滑到新清單的位置沒有意義，只會看到一排不相干的封面飛過去。
    fn jump_to(&mut self, idx: usize) {
        let last = (self.album_count() as f32 - 1.0).max(0.0);
        self.target_centre = (idx as f32).min(last);
        self.centre = self.target_centre;
    }

    /// 查詢字串來自開著的搜尋框；框關了但篩選還留著的話，用上次的結果重篩
    /// （收藏清單更新或重新排序時）。
    fn refilter(&mut self) {
        if let Some(q) = self.search.as_ref().map(|i| i.text.clone()) {
            self.filtered = search::filter(self.base(), &q);
        } else if self.filtered.is_some() {
            let q = self.filter_q.clone();
            self.filtered = search::filter(self.base(), &q);
        }
        if let Some(i) = self.search.as_ref() {
            self.filter_q = i.text.clone();
        }
    }

    // ---------------------------------------------------------------- 搜尋

    fn open_search(&mut self) {
        if self.library.is_empty() {
            return; // 示範資料沒什麼好搜
        }
        // 上一次留下的篩選先清掉，框裡跟畫面上才對得起來。
        // 站在眼前這張 —— 它就是這次搜尋的「原位」
        if self.filtered.is_some() {
            let here = self.current_id();
            self.clear_filter(here);
        }
        self.search_keep = self.album_at(self.centre_index()).map(|a| a.id.clone());
        self.search_at = Instant::now();
        self.search = Some(search::Input::default());
        self.caret_t = Instant::now();
        self.set_ime(true);
    }

    fn search_changed(&mut self) {
        self.caret_t = Instant::now();
        self.refilter();
        self.user_touched = true;
        if self.filtered.is_some() {
            self.jump_to(0);
        } else {
            // 把字刪光 = 回到沒在搜尋的樣子，就該站回開框之前那張。
            // web 版這裡是跳到第一張，但那樣刪完字就找不到剛剛看的東西了。
            let at = self
                .search_keep
                .as_ref()
                .and_then(|id| self.base().iter().position(|a| &a.id == id))
                .unwrap_or(0);
            let at = if self.ghost_shown() && at >= self.ghost_at { at + 1 } else { at };
            self.jump_to(at);
        }
    }

    /// `keep` = 關掉框但留著篩選（Enter、上下鍵）。
    fn close_search(&mut self, keep: bool) {
        self.search = None;
        self.set_ime(false);
        if !keep {
            let back = self.search_keep.clone();
            self.clear_filter(back);
        }
    }

    /// 取消篩選，站到 `keep` 那張（找不到就回最前面）。
    ///
    /// 兩種情況要站的地方不一樣：
    /// - 搜尋框裡按 Esc 是「算了」→ 回到開框之前那張
    /// - 結果留著、翻了一陣子再按 Esc 是「好，找到了」→ 留在眼前這張，
    ///   只是把左右兩邊換回完整收藏
    fn clear_filter(&mut self, keep: Option<String>) {
        if self.filtered.take().is_none() {
            return;
        }
        self.filter_q.clear();
        self.search_keep = None;
        let at = keep
            .and_then(|id| self.base().iter().position(|a| a.id == id))
            .unwrap_or(0);
        // 幽靈插在前面的話位置要往後挪一格
        let at = if self.ghost_shown() && at >= self.ghost_at { at + 1 } else { at };
        self.jump_to(at);
    }

    fn on_key_search(&mut self, key: &Key) {
        let composing = self.search.as_ref().is_some_and(|i| i.composing());
        // 組字中的按鍵都是輸入法的（Enter 是選字、Esc 是取消組字）
        if composing {
            return;
        }
        match key {
            Key::Named(NamedKey::Escape) => self.close_search(false),
            Key::Named(NamedKey::Enter) => {
                self.close_search(true);
                if self.album_count() > 0 {
                    self.act(ui::Hit::PlayCurrent);
                }
            }
            // 上下鍵：關掉框、留著結果，接下來用左右鍵在結果裡翻
            Key::Named(NamedKey::ArrowUp) | Key::Named(NamedKey::ArrowDown) => {
                self.close_search(true)
            }
            // 左右鍵照樣翻封面 —— 邊打邊看結果是搜尋最常見的用法
            Key::Named(NamedKey::ArrowLeft) => self.step(-1.0),
            Key::Named(NamedKey::ArrowRight) => self.step(1.0),
            // 框已經是空的就什麼都不做 —— 不然會觸發一次「查詢變了」，
            // 畫面跳到第一張，Esc 也回不到原本那張
            Key::Named(NamedKey::Backspace) => {
                let had = self.search.as_ref().is_some_and(|i| !i.text.is_empty());
                if had {
                    if let Some(i) = self.search.as_mut() {
                        i.backspace();
                    }
                    self.search_changed();
                }
            }
            Key::Named(NamedKey::Space) => {
                if let Some(i) = self.search.as_mut() {
                    i.push_str(" ");
                }
                self.search_changed();
            }
            Key::Character(c) => {
                if let Some(i) = self.search.as_mut() {
                    i.push_str(c.as_str());
                }
                self.search_changed();
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------- preset 選單

    fn open_picker(&mut self) {
        if !self.viz.ready() {
            return;
        }
        let mut p = Picker {
            input: search::Input::default(),
            rows: Vec::new(),
            at: 0,
            top: 0,
            was: self.viz.current_item(),
            was_locked: self.viz.locked,
            fit: Default::default(),
        };
        Self::picker_rebuild(&self.viz, &mut p, self.picker_rows());
        self.picker = Some(p);
        self.caret_t = Instant::now();
        self.set_ime(true);
    }

    /// 查詢字串或分類變了：重新篩、反白目前在跑的那顆、捲到正中間。
    fn picker_rebuild(viz: &viz::Viz, p: &mut Picker, rows: usize) {
        let q = search::norm(&p.input.text);
        p.rows = viz
            .pool()
            .into_iter()
            .filter(|i| search::name_matches(viz.item_name(*i), &q))
            .collect();
        let cur = viz.current_item();
        p.at = p.rows.iter().position(|i| Some(*i) == cur).unwrap_or(0);
        p.top = ui::picker_centre(p.at, rows, p.rows.len());
    }

    fn picker_move(&mut self, d: i32) {
        let rows = self.picker_rows();
        let Some(p) = self.picker.as_mut() else { return };
        if p.rows.is_empty() {
            return;
        }
        let n = p.rows.len() as i32;
        p.at = (p.at as i32 + d).clamp(0, n - 1) as usize;
        p.top = ui::picker_follow(p.top, p.at, rows, p.rows.len());
        let item = p.rows[p.at];
        // 即時預覽，不溶解 —— 要立刻看到才比得出差別
        self.viz.show(item, false);
    }

    /// `apply = false` 是取消：把開之前那顆與鎖定狀態原樣放回去。
    fn close_picker(&mut self, apply: bool) {
        let Some(p) = self.picker.take() else { return };
        self.set_ime(false);
        if apply {
            // 篩選完直接按 Enter 的話，反白的那列還沒套用過 ——
            // web 版在這裡會套用「原本那顆」，那不是使用者要的
            if let Some(&item) = p.rows.get(p.at) {
                self.viz.show(item, true);
            }
            // 明確挑過的，別讓它 22 秒後自己跑掉
            self.viz.locked = true;
        } else {
            if let Some(w) = p.was {
                self.viz.show(w, false);
            }
            self.viz.locked = p.was_locked;
        }
    }

    fn picker_rows(&self) -> usize {
        let (fw, fh) = self.frame_size();
        ui::picker(fw, fh).rows
    }

    fn frame_size(&self) -> (f32, f32) {
        self.r
            .as_ref()
            .map(|r| gfx::logical(r.target.width, r.target.height))
            .unwrap_or((1280.0, 800.0))
    }

    fn picker_set_category(&mut self, c: viz::Category) {
        self.viz.set_category(c);
        self.cfg.viz_category = c.key().to_string();
        self.save_cfg();
        let rows = self.picker_rows();
        if let Some(p) = self.picker.as_mut() {
            Self::picker_rebuild(&self.viz, p, rows);
        }
    }

    fn on_key_picker(&mut self, key: &Key) {
        let composing = self.picker.as_ref().is_some_and(|p| p.input.composing());
        if composing {
            return;
        }
        let mut edited = false;
        match key {
            Key::Named(NamedKey::ArrowDown) => self.picker_move(1),
            Key::Named(NamedKey::ArrowUp) => self.picker_move(-1),
            Key::Named(NamedKey::PageDown) => self.picker_move(10),
            Key::Named(NamedKey::PageUp) => self.picker_move(-10),
            Key::Named(NamedKey::Enter) => self.close_picker(true),
            Key::Named(NamedKey::Escape) => self.close_picker(false),
            Key::Named(NamedKey::Tab) => {
                let next = self.viz.cat.next();
                self.picker_set_category(next);
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(p) = self.picker.as_mut() {
                    p.input.backspace();
                }
                edited = true;
            }
            Key::Named(NamedKey::Space) => {
                if let Some(p) = self.picker.as_mut() {
                    p.input.push_str(" ");
                }
                edited = true;
            }
            Key::Character(c) => {
                if let Some(p) = self.picker.as_mut() {
                    p.input.push_str(c.as_str());
                }
                edited = true;
            }
            _ => {}
        }
        if edited {
            self.picker_edited();
        }
    }

    fn picker_edited(&mut self) {
        self.caret_t = Instant::now();
        let rows = self.picker_rows();
        if let Some(p) = self.picker.as_mut() {
            Self::picker_rebuild(&self.viz, p, rows);
        }
    }

    /// 輸入法開關。**只有打字框開著的時候才開** —— 平常開著的話，
    /// 中文輸入法會把 s、k、r 這些快捷鍵吃進組字框裡，按了沒反應。
    fn set_ime(&mut self, on: bool) {
        if let Some(w) = &self.window {
            w.set_ime_allowed(on);
        }
        if !on {
            if let Some(i) = self.search.as_mut() {
                i.preedit.clear();
            }
            if let Some(p) = self.picker.as_mut() {
                p.input.preedit.clear();
            }
        }
    }

    fn on_ime(&mut self, ime: winit::event::Ime) {
        use winit::event::Ime;
        let input = match (self.search.as_mut(), self.picker.as_mut()) {
            (Some(i), _) => i,
            (None, Some(p)) => &mut p.input,
            _ => return,
        };
        let committed = match ime {
            Ime::Preedit(s, _) => {
                input.preedit = s;
                false
            }
            Ime::Commit(s) => {
                input.preedit.clear();
                input.push_str(&s);
                true
            }
            // winit 每一次組字結束都會送 Disabled，不是「輸入法被關掉」
            Ime::Disabled => {
                input.preedit.clear();
                false
            }
            Ime::Enabled => false,
        };
        if committed {
            if self.search.is_some() {
                self.search_changed();
            } else {
                self.picker_edited();
            }
        }
        self.caret_t = Instant::now();
    }

    fn step(&mut self, d: f32) {
        self.user_touched = true;
        let n = self.album_count() as f32;
        if n <= 0.0 {
            return;
        }
        self.target_centre = (self.target_centre + d).clamp(0.0, n - 1.0);
    }

    /// 讓封面流跟上正在播的東西。對應 web 版的 `initialPosition` ＋ `updateGhost`。
    ///
    /// **兩段的順序不能換。** 先定位再插幽靈 —— 幽靈是插在「使用者正在看的
    /// 位置」，所以要先讓那個位置落在清單中間，插進去之後左右才都有封面
    /// 可以排開；反過來的話幽靈會插在索引 0，左邊是空的。
    ///
    /// 定位**只做一次**，而且使用者自己動過就不再做 —— 每次換歌都把畫面
    /// 拉走的話，正在翻收藏的人會覺得被搶走控制權。
    fn sync_playing(&mut self, now: &Option<player::NowPlaying>) {
        // 篩選中不定位也不插幽靈 —— 使用者正在看搜尋結果，
        // 換歌就把畫面拉走的話等於搜尋被打斷
        if self.library.is_empty() || self.filtered.is_some() || self.drill.is_some() {
            return;
        }
        let Some(n) = now else { return };
        if n.id.is_empty() || n.album_uri.is_empty() {
            return; // 還不知道在播什麼，等下一次輪詢
        }
        let found = self.library.iter().position(|a| a.uri == n.album_uri);

        // ---- 1：第一次定位 ----
        if !self.positioned && !self.user_touched {
            self.positioned = true;
            let i = match found {
                Some(i) => i,
                // 不在收藏裡 → 等一下會插幽靈，先站到中間
                None => self.library.len() / 2,
            };
            self.centre = i as f32;
            self.target_centre = i as f32;
        }

        // ---- 2：幽靈 ----
        // 收掉不是立刻拿掉：先播退場（縮小淡出、右邊的往回補位），播完才真的移除
        if found.is_some() {
            if self.ghost.is_some() {
                self.ghost_removing = true;
            }
            return;
        }
        if self.ghost.as_ref().is_some_and(|g| g.uri == n.album_uri) {
            // 同一張，位置不用動；正在退場又播回來就取消退場
            self.ghost_removing = false;
            return;
        }
        // 換一張幽靈的話位置留著，原本就站在上面，用脈衝帶過去就好
        let replacing = self.ghost.is_some() && !self.ghost_removing;
        self.ghost_removing = false;
        self.ghost = Some(spotify::Album {
            id: n.id.clone(),
            uri: n.album_uri.clone(),
            name: if n.album.is_empty() { n.title.clone() } else { n.album.clone() },
            artist: n.artist.clone(),
            art: match &n.art {
                player::Art::Url(u) => u.clone(),
                player::Art::SmtcThumb => String::new(),
            },
            ..Default::default()
        });
        if replacing {
            self.ghost_anim = 1.0;
            self.ghost_pulse = Some(Instant::now());
        } else {
            // 插在使用者正在看的位置：它就是中央那張，其餘往兩側排開
            self.ghost_at = (self.centre.round().max(0.0) as usize).min(self.library.len());
            self.centre = self.ghost_at as f32;
            self.target_centre = self.centre;
            self.ghost_anim = 0.0;
        }
    }

    /// 幽靈的進退場往目標收斂；退場播完才真的拿掉（Python 版的 finishGhostRemoval）。
    fn animate_ghost(&mut self, dt: f32) {
        if self.ghost.is_none() {
            self.ghost_anim = 1.0;
            return;
        }
        let target = if self.ghost_removing { 0.0 } else { 1.0 };
        // Python 版每幀 ×0.19（60fps），換成跟幀率無關的寫法
        let k = 1.0 - (1.0f32 - 0.19).powf(dt * 60.0);
        self.ghost_anim += (target - self.ghost_anim) * k;
        if (target - self.ghost_anim).abs() < 0.0025 {
            self.ghost_anim = target;
        }
        if self.ghost_anim == 0.0 && self.ghost_removing {
            let was = self.ghost_at;
            self.ghost = None;
            self.ghost_removing = false;
            self.ghost_anim = 1.0;
            // 抽掉一張之後索引會位移，把視線留在原地。退場最後一幀右邊的已經補位，
            // 所以這裡減一不會跳
            if self.drill.is_none() && self.filtered.is_none() && self.centre.round() as usize > was {
                self.centre -= 1.0;
                self.target_centre -= 1.0;
            }
            let last = (self.album_count() as f32 - 1.0).max(0.0);
            self.centre = self.centre.clamp(0.0, last);
            self.target_centre = self.target_centre.clamp(0.0, last);
        }
    }

    /// poller 抓到新的收藏清單就重讀。每幀比一個數字，沒變就什麼都不做。
    fn sync_library(&mut self) {
        let rev = self.player.shared.albums_rev.load(std::sync::atomic::Ordering::Relaxed);
        if rev == self.albums_rev {
            return;
        }
        self.albums_rev = rev;
        let list = load_library(&self.paths, &self.cfg.sort);
        println!("收藏清單更新：{} 項", list.len());
        self.library = list;
        // 清單換了，展開的那張可能已經不在；曲目也可能變了（web 版的 reloadLibrary）
        self.drill = None;
        self.drill_loading = None;
        self.track_cache.clear();
        self.refilter();
        // 清單換了，中央那張的索引可能已經超出範圍
        let last = (self.album_count() as f32 - 1.0).max(0.0);
        self.target_centre = self.target_centre.min(last);
        self.centre = self.centre.min(last);
    }

    // ---------------------------------------------------------------- 展開

    /// 中央那張按下去：專輯與自己的清單展開成曲目，追蹤的清單直接播；
    /// 曲目則以專輯／清單為 context 從那首開始，下一首會接著原本的順序。
    fn play_current(&mut self) {
        // 示範資料沒有 uri，播不了，那就當成播放／暫停
        let idx = self.centre.round().max(0.0) as usize;
        match self.album_at(idx).cloned() {
            Some(a) if a.can_drill() => self.open_drill(a),
            Some(a) if a.is_track() => {
                let Some(src) = self.drill.as_ref().map(|d| d.src.uri.clone()) else { return };
                let _ = self.player.tx.send(player::Cmd::PlayContext {
                    uri: src,
                    track_uri: a.uri,
                    pos: a.pos.unwrap_or(0),
                    name: a.name,
                });
            }
            // 幽靈專輯比對不到 Spotify 上的項目時，uri 是自己編的 `smtc:album:…`，
            // 送出去只會換到一個 400
            Some(a) if a.uri.starts_with("smtc:") => {
                self.toast("這張專輯還沒對應到 Spotify 上的項目");
            }
            Some(a) => {
                let _ = self.player.tx.send(player::Cmd::PlayAlbum(a.uri, a.name));
            }
            None => {
                let _ = self.player.tx.send(player::Cmd::Toggle);
            }
        }
    }

    fn open_drill(&mut self, src: spotify::Album) {
        if let Some(t) = self.track_cache.get(&src.uri).cloned() {
            self.enter_drill(src, t);
            return;
        }
        if self.drill_loading.is_some() {
            return;
        }
        self.toast(format!("讀取  {}…", src.name));
        let _ = self.player.tx.send(player::Cmd::LoadTracks(src.clone()));
        self.drill_loading = Some(src);
    }

    /// poller 抓完曲目了。**讀取期間使用者可能已經捲走或換頁**，那就只存快取不展開。
    fn on_tracks(&mut self, t: player::Tracks) {
        let Some(src) = self.drill_loading.take() else { return };
        if src.uri != t.src {
            return;
        }
        let mut tracks = match t.res {
            Ok(v) => v,
            Err(e) => {
                self.toast(e);
                return;
            }
        };
        // 專輯的曲目端點不帶封面與專輯名，用專輯那張補上
        if !src.is_playlist() {
            for tr in &mut tracks {
                tr.album = src.name.clone();
                tr.album_uri = src.uri.clone();
                tr.art = src.art.clone();
                tr.art_sm = if src.art_sm.is_empty() { src.art.clone() } else { src.art_sm.clone() };
            }
        }
        self.track_cache.insert(src.uri.clone(), tracks.clone());
        if tracks.is_empty() {
            self.toast(if src.is_playlist() { "這個播放清單是空的" } else { "這張專輯沒有曲目" });
            return;
        }
        let here = self.album_at(self.centre_index()).map(|a| a.uri.clone());
        if self.view == View::Browse && self.drill.is_none() && here.as_deref() == Some(&src.uri) {
            self.enter_drill(src, tracks);
        }
    }

    /// 換視圖一律走這裡，順便開始一段轉場。
    fn set_view(&mut self, v: View) {
        if v == self.view {
            return;
        }
        let from = self.view;
        let pair = matches!((from, v), (View::Browse, View::Now) | (View::Now, View::Browse));
        // 封面流上沒東西（搜尋篩到空的）就沒有替身可飛，退回單純淡入淡出
        let shared = pair && self.album_count() > 0;
        self.trans = Some(Trans { from, to: v, start: Instant::now(), shared, fly: None, now: 0.0, chrome0: self.chrome });
        self.view = v;
        if v == View::Viz {
            self.viz.enter();
        }
    }

    /// 封面流整片縮小淡出 → 換內容 → 從稍大淡入（Python 版的 swapFlow）。
    /// 上一段還沒換完就來了新的，先把上一段的內容換掉，再從這段的開頭播。
    fn swap_flow(&mut self, f: impl FnOnce(&mut App) + 'static) {
        if let Some(prev) = self.swap.take().and_then(|mut s| s.apply.take()) {
            prev(self);
        }
        self.swap = Some(Swap { start: Instant::now(), apply: Some(Box::new(f)), applied: None });
    }

    fn animate_swap(&mut self) {
        let due = self.swap.as_mut().and_then(|s| {
            if s.apply.is_some() && s.start.elapsed().as_secs_f32() >= SWAP_OUT_S { s.apply.take() } else { None }
        });
        if let Some(f) = due {
            f(self);
            if let Some(s) = self.swap.as_mut() {
                s.applied = Some(Instant::now());
            }
        }
        if self.swap.as_ref().is_some_and(|s| s.done()) {
            self.swap = None;
        }
    }

    fn enter_drill(&mut self, src: spotify::Album, tracks: Vec<spotify::Album>) {
        if tracks.is_empty() {
            return;
        }
        self.swap_flow(move |app| app.enter_drill_now(src, tracks));
    }

    fn enter_drill_now(&mut self, src: spotify::Album, tracks: Vec<spotify::Album>) {
        let at = self.centre_index();
        if self.search.is_some() {
            self.search = None;
            self.set_ime(false);
        }
        self.filtered = None;
        self.filter_q.clear();
        self.drill = Some(Drill { src, tracks, at });
        self.follow_gen = self.player.shared.generation.load(std::sync::atomic::Ordering::Relaxed);
        let i = self.drill_playing_index().unwrap_or(0);
        self.jump_to(i);
    }

    fn close_drill(&mut self) {
        if self.drill.is_some() {
            self.swap_flow(|app| app.close_drill_now());
        }
    }

    fn close_drill_now(&mut self) {
        let Some(d) = self.drill.take() else { return };
        if self.search.is_some() {
            self.search = None;
            self.set_ime(false);
        }
        self.filtered = None;
        self.filter_q.clear();
        let i = self.library.iter().position(|a| a.id == d.src.id);
        // 幽靈這時才會重新出現（下一幀的 sync_playing），先站好
        self.jump_to(i.unwrap_or(d.at));
    }

    /// 展開中，正在播的那首是第幾格。有 URI 就比 URI；SMTC 來源沒有，
    /// 只能比歌名＋第一位演出者（web 版的 `drillPlayingIndex`）。
    fn drill_playing_index(&self) -> Option<usize> {
        let d = self.drill.as_ref()?;
        let now = self.player.shared.now.lock().ok()?.clone()?;
        let meta = self.player.shared.meta.lock().ok()?.clone();
        let uri = if !now.track_uri.is_empty() {
            now.track_uri.clone()
        } else if meta.id == now.id {
            meta.uri
        } else {
            String::new()
        };
        if !uri.is_empty()
            && let Some(i) = d.tracks.iter().position(|t| t.uri == uri) {
                return Some(i);
            }
        let norm = |s: &str| -> String {
            use unicode_normalization::UnicodeNormalization;
            s.nfkc().collect::<String>().to_lowercase().chars().filter(|c| !c.is_whitespace()).collect()
        };
        let first = |s: &str| norm(s.split([',', '、', '&']).next().unwrap_or(""));
        let (t, a) = (norm(&now.title), first(&now.artist));
        d.tracks.iter().position(|x| norm(&x.name) == t && first(&x.artist) == a)
    }

    // ---------------------------------------------------------------- 愛心與音量

    fn tools(&self) -> ui::Tools {
        let sh = &self.player.shared;
        let id = self.now_flag(|n| n.id.clone());
        let liked = sh.meta.lock().ok().and_then(|m| {
            (!id.is_empty() && m.id == id && m.uri.starts_with("spotify:track:"))
                .then(|| m.liked.unwrap_or(false))
        });
        let v = sh.volume.lock().map(|g| *g).unwrap_or_default();
        ui::Tools { liked, volume: v.vol, vol_locked: !v.supported }
    }

    fn toggle_like(&mut self) {
        let id = self.now_flag(|n| n.id.clone());
        let send = {
            let Ok(mut m) = self.player.shared.meta.lock() else { return };
            if id.is_empty() || m.id != id || !m.uri.starts_with("spotify:track:") {
                None
            } else {
                // 先改畫面再送；失敗的話 poller 會改回去
                let want = !m.liked.unwrap_or(false);
                m.liked = Some(want);
                Some((m.uri.clone(), want))
            }
        };
        if let Some((uri, on)) = send {
            let _ = self.player.tx.send(player::Cmd::SetLiked { uri, on });
        }
    }

    /// 使用者改的音量。立刻反映在畫面上，送出交給節流（`now` = 放開滑鼠，馬上送）。
    fn set_volume(&mut self, v: i32, now: bool) {
        let v = v.clamp(0, 100) as u8;
        {
            let Ok(mut g) = self.player.shared.volume.lock() else { return };
            if !g.supported {
                return;
            }
            g.vol = Some(v);
        }
        if v > 0 {
            self.vol_before_mute = v;
        }
        self.vol_pending = Some(v);
        self.flush_volume(now);
    }

    fn flush_volume(&mut self, force: bool) {
        if let Some(v) = self.vol_pending
            && (force || self.vol_sent.elapsed() >= Duration::from_millis(200)) {
                let _ = self.player.tx.send(player::Cmd::SetVolume(v));
                self.vol_pending = None;
                self.vol_sent = Instant::now();
            }
    }

    fn toggle_mute(&mut self) {
        match self.tools().volume {
            Some(0) => self.set_volume(if self.vol_before_mute > 0 { self.vol_before_mute as i32 } else { 50 }, true),
            _ => self.set_volume(0, true),
        }
    }

    /// 音量滑桿在目前這頁的位置。拖曳中游標跑出滑桿也要算得出值，所以不能靠命中測試。
    /// 依設定開關迷你視窗、同步鎖定狀態。設定面板與系統列只改 `cfg`，
    /// 開視窗要 `ActiveEventLoop`，所以統一在這裡做。
    fn sync_mini(&mut self, el: &ActiveEventLoop) {
        match (&mut self.mini, self.cfg.mini_enabled) {
            (None, true) => {
                let Some(r) = self.r.as_ref() else { return };
                match mini::Mini::open(
                    el,
                    &r.gpu,
                    &r.instance,
                    &r.adapter,
                    self.cfg.mini_rect,
                    self.cfg.mini_locked,
                    self.cfg.mini_lyrics,
                ) {
                    Ok(m) => self.mini = Some(m),
                    Err(e) => {
                        // 開不起來就把開關關回去，不然每一輪都會再試一次
                        crate::warn!("迷你視窗開不起來：{e:#}");
                        self.cfg.mini_enabled = false;
                        self.save_cfg();
                        self.toast("迷你視窗開不起來");
                    }
                }
            }
            (Some(_), false) => {
                self.save_mini_rect();
                self.mini = None;
            }
            (Some(m), true) => {
                if m.locked != self.cfg.mini_locked {
                    m.set_locked(self.cfg.mini_locked);
                }
                m.set_lyrics(self.cfg.mini_lyrics);
            }
            _ => {}
        }
        // 叫回來要等視窗開好才移得動，所以排到這裡
        if std::mem::take(&mut self.mini_recall)
            && let Some(m) = self.mini.as_mut()
        {
            m.recall();
            self.save_mini_rect();
        }
        // 縮在系統列時 poller 不抓歌詞；迷你視窗要顯示歌詞的話要它照抓
        let want = self.mini.is_some() && self.cfg.mini_lyrics;
        if want != self.mini_ly_sent {
            self.mini_ly_sent = want;
            let _ = self.player.tx.send(player::Cmd::WantLyrics(want));
        }
    }

    /// 迷你視窗找不到的時候用：移到主螢幕右下角，沒開就順便打開。真正移動在 `sync_mini`
    fn recall_mini(&mut self) {
        self.mini_recall = true;
        if !self.cfg.mini_enabled {
            self.cfg.mini_enabled = true;
            self.save_cfg();
        }
    }

    /// 迷你視窗的位置：移動、縮放時一秒來幾十次，停下來半秒才寫檔
    fn save_mini_rect(&mut self) {
        let Some(m) = self.mini.as_mut() else { return };
        if m.moved_at.take().is_some() {
            self.cfg.mini_rect = Some(m.rect());
            self.save_cfg();
        }
    }

    fn tick_mini(&mut self) {
        if self.mini.as_ref().is_some_and(|m| m.moved_at.is_some_and(|t| t.elapsed() > Duration::from_millis(500))) {
            self.save_mini_rect();
        }
        if !self.mini.as_ref().is_some_and(|m| m.due()) {
            return;
        }
        let t_tick = Instant::now();
        let now = self.player.shared.now.lock().ok().and_then(|g| g.clone());
        let (Some(r), Some(m)) = (self.r.as_mut(), self.mini.as_mut()) else { return };
        // 主視窗縮在系統列時它的 draw 不會跑，封面的上傳要在這裡補
        r.art.pump(&r.gpu);
        let has = now.as_ref().is_some_and(|n| !n.id.is_empty());
        let (art, key) = match now.as_ref().filter(|_| has) {
            Some(n) => match &n.art {
                player::Art::Url(u) => (Some(r.art.get(u)), u.clone()),
                player::Art::SmtcThumb => {
                    if !r.art.has(&n.id)
                        && let Some(b) = self.player.shared.thumb.lock().ok().and_then(|g| g.clone()) {
                            r.art.put_bytes(&r.gpu, &n.id, &b);
                        }
                    (Some(r.art.get_cached(&n.id)), n.id.clone())
                }
            },
            None => (None, String::new()),
        };
        // 換封面就重新取色。只在換歌時解一張小圖，不必每張封面都多解一次
        if key.is_empty() {
            if !m.tint_key.is_empty() {
                m.set_tint("", mini::default_tint());
            }
        } else if m.want_tint(&key) {
            let bytes = match now.as_ref().map(|n| &n.art) {
                Some(player::Art::SmtcThumb) => self.player.shared.thumb.lock().ok().and_then(|g| g.clone()),
                _ => r.art.disk_bytes(&key),
            };
            if let Some(t) = bytes.as_deref().and_then(mini::tint_from_bytes) {
                m.set_tint(&key, t);
            }
        }
        let pos = now.as_ref().map_or(0, |n| {
            n.progress_ms + if n.playing { n.at.elapsed().as_millis() as i64 } else { 0 }
        });
        // 歌詞模式：當前句與下一句。關掉時整段跳過，連歌詞的鎖都不碰
        let lyric = match now.as_ref().filter(|_| has && m.lyrics) {
            Some(n) => {
                let base = pos;
                self.player.shared.lyrics.lock().ok().map(|g| match g.as_ref() {
                    Some(l) if !l.synced.is_empty() => {
                        let pos = base + lyrics::shift(&self.cfg, Some(l));
                        let i = scene_now::current_line(l, pos).map_or(-1, |i| i as i64);
                        let (cur, next, _) = ktv_view(l, i, pos, n.duration_ms);
                        let next_in = l.synced.get((i + 1) as usize).filter(|_| n.playing).map(|x| x.t - pos);
                        (i, cur, next, next_in)
                    }
                    // 沒有同步歌詞：當前句那行寫原因，載入中就留白
                    other => {
                        let why = match other.map(|l| (l.source.as_str(), l.plain.is_empty())) {
                            Some(("loading", _)) | None => "",
                            Some(("instrumental", _)) => "♪ 純音樂",
                            Some((_, false)) => "這首只有純文字歌詞",
                            _ => "找不到這首的歌詞",
                        };
                        (i64::MIN, why.to_string(), String::new(), None)
                    }
                })
            }
            None => None,
        };
        let c = mini::Content {
            title: now.as_ref().map_or("", |n| n.title.as_str()),
            artist: now.as_ref().map_or("", |n| n.artist.as_str()),
            pos_ms: pos,
            dur_ms: now.as_ref().map_or(0, |n| n.duration_ms),
            has,
            art,
            lyric: lyric.as_ref().map(|(idx, cur, next, next_in)| mini::Lyric {
                key: now.as_ref().map_or("", |n| n.id.as_str()),
                idx: *idx,
                cur,
                next,
                next_in_ms: *next_in,
            }),
        };
        if let Err(e) = m.draw(&r.gpu, &mut r.quads, &mut r.text, &c) {
            crate::warn!("迷你視窗畫不出來：{e:#}");
        }
        r.text.end_frame();
        mini::TICK_US.fetch_add(t_tick.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
    }

    fn mini_event(&mut self, el: &ActiveEventLoop, ev: &WindowEvent) {
        let Some(m) = self.mini.as_mut() else { return };
        match m.event(ev) {
            Some(mini::Action::OpenMain) => self.bring_front(el),
            Some(mini::Action::Lock) => {
                self.cfg.mini_locked = true;
                self.save_cfg();
            }
            Some(mini::Action::Close) => {
                self.cfg.mini_enabled = false;
                self.save_cfg();
            }
            None => {}
        }
    }

    /// 有人要設定視窗：沒開就開，開著就拉到前面
    /// 依設定（跟隨系統時再讀一次登錄檔）套用外觀，兩個有標題列的視窗也跟著換。
    /// 迷你視窗沒有標題列，畫面又固定深色，不用動
    /// 這一幀怎麼畫。操作中、視覺效果頁（MilkDrop 有自己的幀率設定，兩個時鐘疊在一起會抖）、
    /// 轉場中照上限；播放中的文字 PV 也是（含退回一般歌詞頁的那一段）。其他頁閒置降到 `IDLE_FPS`，
    /// 沒在播又沒人動就照 `CALM` 一段段往下，最後停畫。縮小了直接停
    fn pace(&self) -> Pace {
        if self.hidden() {
            return Pace::Park;
        }
        let active = (self.focused || (self.prefs_focused && self.prefs.is_some()))
            && self.last_input.elapsed() < IDLE_AFTER;
        if active || self.view == View::Viz || self.trans.is_some() {
            return Pace::Full;
        }
        let quiet = self.last_input.elapsed().min(self.stir.elapsed());
        if quiet < IDLE_AFTER {
            let pv = self.view == View::Now && (self.pv_want || self.pv_mix.value() > 0.0);
            return if pv { Pace::Full } else { Pace::Slow(IDLE_FPS) };
        }
        CALM.iter().find(|(t, _)| quiet < *t).map_or(Pace::Park, |&(_, fps)| Pace::Slow(fps))
    }

    /// 這一幀跟上一幀至少隔多久（秒），不用等就是 `None`
    fn frame_gap(&mut self, pace: Pace) -> Option<f32> {
        if self.refresh.is_none_or(|(_, t)| t.elapsed() > Duration::from_secs(2)) {
            let hz = self.window.as_ref().and_then(|w| w.current_monitor()).and_then(|m| m.refresh_rate_millihertz());
            self.refresh = Some((hz.map_or(60.0, |m| m as f32 / 1000.0).max(1.0), Instant::now()));
        }
        let hz = self.refresh.map_or(60.0, |(hz, _)| hz);
        // 睡滿整數格。交換鏈這時有空的緩衝，不會在 Fifo 那裡等，節奏完全由這裡決定
        // （少睡半格的話量出來是 71 fps，不是 60）
        let slow = match pace {
            Pace::Slow(fps) => Some((hz / fps).round().max(1.0) / hz),
            _ => None,
        };
        // 使用者設的上限要「不超過」，所以無條件進位（180 Hz 選 120 是每兩格一幀 = 90）。
        // `--fps` 是量測用的，照原本的不對齊
        let limit = if self.fps_cap > 0.0 {
            Some(1.0 / self.fps_cap)
        } else if self.cfg.fps_limit > 0 && self.view != View::Viz {
            Some((hz / self.cfg.fps_limit as f32 - 0.05).ceil().max(1.0) / hz)
        } else {
            None
        };
        match (slow, limit) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    fn hidden(&self) -> bool {
        self.occluded || self.window.as_ref().is_some_and(|w| w.is_minimized() == Some(true))
    }

    /// 停畫時拿來比對的狀態，見 [`Parked`]
    fn stir_sig(&self) -> (bool, String, bool) {
        let (playing, id) = self.player.shared.now.lock().ok()
            .and_then(|g| g.as_ref().map(|n| (n.playing, n.id.clone())))
            .unwrap_or_default();
        let toast = self.player.shared.toast.lock().is_ok_and(|g| g.is_some())
            || self.updater.toast.lock().is_ok_and(|g| g.is_some());
        (playing, id, toast)
    }

    /// 主視窗停畫：不排下一幀、關掉只給畫面用的擷取，迴圈改成定時醒來看一下（`about_to_wait`）
    fn park(&mut self, el: &ActiveEventLoop) {
        self.parked = Some(Parked { at: Instant::now(), hidden: self.hidden(), sig: self.stir_sig() });
        audio::set_pv(false);
        audio::set_viz(false);
        el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + PARK_POLL));
    }

    fn unpark(&mut self, el: &ActiveEventLoop) {
        if self.parked.take().is_none() {
            return;
        }
        self.stir = Instant::now();
        el.set_control_flow(ControlFlow::Poll);
        // 停了多久都不能算進動畫的 dt 與 fps 統計
        self.last = Instant::now();
        self.frames = 0;
        self.fps_at = Instant::now();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn sync_appearance(&mut self) {
        let a = theme::Appearance::parse(&self.cfg.appearance);
        let m = a.resolve();
        if m != theme::mode() {
            theme::set_mode(m);
            // 強調色的亮度深淺色不同，重新取一次
            self.accent.key.clear();
            if let Some(p) = self.prefs.as_mut() {
                p.dirty = true;
            }
        }
        // winit 0.30 的 `set_theme` 只換當下的標題列、不記住偏好：建窗時沒指定主題
        // 的話，系統每廣播一次設定變更它就改回系統的那套。所以每次都重套
        for w in [self.window.as_deref(), self.prefs.as_ref().map(|p| &*p.window)].into_iter().flatten() {
            w.set_theme(a.title_bar());
        }
    }

    /// 字型或字級倍率改了：每個 `Text` 下一次 `begin` 跟上；迷你視窗不常重畫，要叫它
    fn sync_font(&mut self) {
        text::set_font(&self.cfg.font_latin, &self.cfg.font_cjk, self.cfg.font_scale);
        if let Some(m) = self.mini.as_mut() {
            m.dirty = true;
        }
        if let Some(p) = self.prefs.as_mut() {
            p.dirty = true;
        }
    }

    /// 介面倍率改了：主視窗下一幀就用新的縮放（實體大小不變，邏輯大小變），
    /// 設定視窗連視窗大小一起等比例縮放
    fn sync_zoom(&mut self) {
        let old = gfx::ui_zoom();
        gfx::set_ui_zoom(self.cfg.ui_scale);
        let new = gfx::ui_zoom();
        if old == new {
            return;
        }
        if let Some(p) = self.prefs.as_mut() {
            p.rezoom(old, new);
            p.dirty = true;
        }
        if let Some(w) = &self.window {
            sync_min_size(w, self.screen_mode);
            w.request_redraw();
        }
    }

    fn sync_prefs(&mut self, el: &ActiveEventLoop) {
        if !std::mem::take(&mut self.prefs_want) {
            return;
        }
        if let Some(p) = &self.prefs {
            p.raise();
            return;
        }
        let Some(r) = self.r.as_ref() else { return };
        match prefs::Prefs::open(el, &r.gpu, &r.instance, &r.adapter, self.window.as_deref(), window_icon()) {
            Ok(p) => {
                p.window.set_theme(theme::Appearance::parse(&self.cfg.appearance).title_bar());
                self.prefs = Some(p);
            }
            Err(e) => {
                crate::warn!("設定視窗開不起來：{e:#}");
                self.toast("設定視窗開不起來");
            }
        }
    }

    fn screens(&self) -> usize {
        self.window.as_ref().map_or(1, |w| w.available_monitors().count().max(1))
    }

    /// 設定視窗那幾列要的狀態（登入、更新），跟舊面板讀的是同一份
    fn prefs_status(&self) -> prefs::Status {
        use std::sync::atomic::Ordering::Relaxed;
        use widgets::Kind;
        let sh = &self.player.shared;
        let authed = sh.authed.load(Relaxed);
        let reauth = sh.need_reauth.load(Relaxed);
        let msg = sh.login_msg.lock().map(|m| m.clone()).unwrap_or_default();
        // 按鈕的動作跟 set_item 的 SpotifyStatus 一致：沒 client_id 開精靈、缺權限重新授權、登入中照樣可以再按
        let (hint, btn) = if !self.cfg.client_id_looks_valid() {
            ("還沒設定 client_id".to_string(), prefs::Btn::new("設定精靈…", Kind::Primary))
        } else if authed && reauth {
            ("需要重新授權".to_string(), prefs::Btn::new("重新授權", Kind::Primary))
        } else if authed {
            ("已登入".to_string(), prefs::Btn::new("登出", Kind::Normal))
        } else {
            ("還沒登入".to_string(), prefs::Btn::new("登入", Kind::Primary))
        };
        let hint = if msg.is_empty() { hint } else { msg };
        let v = config::VERSION;
        let (uhint, ubtn) = match self.updater.stage() {
            update::Stage::Idle => (format!("VitroDeck {v}"), prefs::Btn::new("檢查更新", Kind::Normal)),
            update::Stage::Latest => (self.updater.label(), prefs::Btn::new("檢查更新", Kind::Normal)),
            update::Stage::Failed(_) => (self.updater.label(), prefs::Btn::new("再試一次", Kind::Normal)),
            update::Stage::Available { version, installable: true } => {
                (format!("有新版 {version}，目前是 {v}"), prefs::Btn::new("更新", Kind::Primary))
            }
            update::Stage::Available { version, .. } => {
                (format!("有新版 {version}，目前是 {v}"), prefs::Btn::new("開下載頁", Kind::Primary))
            }
            update::Stage::Checking => (self.updater.label(), prefs::Btn::off("檢查更新")),
            update::Stage::Downloading { .. } | update::Stage::Ready => (self.updater.label(), prefs::Btn::off("更新")),
        };
        // 全螢幕在小螢幕上放不下時，介面倍率實際上被降低了：說明那行寫出來
        let zoom_note = self.window.as_deref().map_or(String::new(), |w| {
            let real = main_scale(w) / w.scale_factor() as f32;
            if real + 0.004 < gfx::ui_zoom() {
                format!("螢幕放不下，目前實際 {:.0}%", real * 100.0)
            } else {
                String::new()
            }
        });
        prefs::Status {
            screens: self.screens(),
            zoom_note,
            spotify_hint: hint,
            spotify_btn: btn,
            update_hint: uhint,
            update_btn: ubtn,
            data_dir: self.paths.data.display().to_string(),
        }
    }

    fn tick_prefs(&mut self) {
        if let Some(p) = &self.prefs {
            p.ensure_shown();
        }
        if !self.prefs.as_ref().is_some_and(|p| p.due()) {
            return;
        }
        let ctx = prefs::Ctx { st: self.prefs_status(), cfg: &self.cfg };
        let (Some(r), Some(p)) = (self.r.as_mut(), self.prefs.as_mut()) else { return };
        if let Err(e) = p.draw(&r.gpu, &mut r.quads, &mut r.text, &ctx) {
            crate::warn!("設定視窗畫不出來：{e:#}");
        }
        r.text.end_frame();
    }

    fn prefs_event(&mut self, el: &ActiveEventLoop, ev: &WindowEvent) {
        let screens = self.screens();
        // 跟主視窗同一個理由（見 `window_event`）
        if matches!(ev, WindowEvent::ThemeChanged(_) | WindowEvent::Focused(true)) {
            self.sync_appearance();
        }
        // 在設定視窗操作時主視窗失焦，但改的東西要在主視窗上順順地動，不算閒置
        if let WindowEvent::Focused(f) = ev {
            self.prefs_focused = *f;
        }
        if is_input(ev) {
            self.last_input = Instant::now();
        }
        let Some(p) = self.prefs.as_mut() else { return };
        match p.event(ev, &self.cfg, screens) {
            Some(prefs::Action::Close) => self.prefs = None,
            // 裝置選單、關於、設定精靈是主視窗上的面板：先把主視窗叫到前面，不然開在看不到的地方
            Some(prefs::Action::Edit(it, e))
                if matches!(it, settings::Item::Device | settings::Item::About)
                    || (it == settings::Item::SpotifyStatus && !self.cfg.client_id_looks_valid()) =>
            {
                self.bring_front(el);
                self.set_item(it, e);
            }
            Some(prefs::Action::Edit(it, e)) => self.set_item(it, e),
            // 中文字型選微軟正黑體就存成空字串（＝預設），之後預設換了也跟著換
            Some(prefs::Action::Font(slot, key)) => {
                match slot {
                    prefs::FontSlot::Cjk => {
                        self.cfg.font_cjk = if key == text::DEFAULT_FONT { String::new() } else { key }
                    }
                    prefs::FontSlot::Latin => self.cfg.font_latin = key,
                }
                self.sync_font();
                self.save_cfg();
            }
            Some(prefs::Action::DragEnd(settings::Item::UiScale)) => self.sync_zoom(),
            Some(prefs::Action::DragEnd(_)) | None => {}
        }
    }

    /// 結束程式。q、系統列的「結束」、沒開系統列時的關閉鈕都走這裡。
    fn quit(&mut self, el: &ActiveEventLoop) {
        self.save_mini_rect();
        self.mini = None;
        let _ = self.player.tx.send(player::Cmd::Quit);
        tray::stop();
        el.exit();
    }

    /// 依 `close_to_tray` 開或拿掉系統列圖示（Python 版的 `sync_tray`）。
    /// 縮在系統列的時候不拿掉 —— 拿掉就再也叫不回來了。
    fn sync_tray(&mut self) {
        if self.cfg.close_to_tray && !tray::running() {
            let px = self.proxy.clone();
            let ok = tray::start(
                move |c| {
                    let _ = px.send_event(UserEv::Tray(c));
                },
                || {
                    use std::sync::atomic::Ordering::Relaxed;
                    let open = mini::OPEN.load(Relaxed);
                    vec![
                        Some(("開啟 VitroDeck".to_string(), None, tray::Cmd::Open)),
                        None,
                        Some(("迷你視窗".to_string(), Some(open), tray::Cmd::Mini)),
                        Some(("鎖定迷你視窗".to_string(), Some(open && mini::LOCKED.load(Relaxed)), tray::Cmd::MiniLock)),
                        Some(("叫回迷你視窗".to_string(), None, tray::Cmd::MiniRecall)),
                        None,
                        Some(("結束".to_string(), None, tray::Cmd::Quit)),
                    ]
                },
            );
            if !ok {
                crate::warn!("系統列圖示加不上去，關閉鈕維持直接結束");
            }
        } else if !self.cfg.close_to_tray && !self.in_tray {
            tray::stop();
        }
    }

    /// 關閉鈕：藏起來縮到系統列。畫面停畫（重繪迴圈在 RedrawRequested 那邊斷掉）、
    /// poller 不抓歌詞與曲目資訊，只剩播放狀態與燈條。
    fn hide_to_tray(&mut self, el: &ActiveEventLoop) {
        if self.in_tray {
            return;
        }
        self.in_tray = true;
        // 系統列那套自己管迴圈，停畫的狀態用不到了
        self.parked = None;
        // 迴圈平常是 Poll，靠重繪時的限幀睡眠降速；停畫之後 Poll 會空轉吃滿一顆核心。
        // 縮著的時候改成有事件才醒（系統列點擊、第二份敲門都是事件）
        el.set_control_flow(ControlFlow::Wait);
        self.seek_drag = None;
        self.vol_drag = false;
        let _ = self.player.tx.send(player::Cmd::Tray(true));
        if let Some(w) = &self.window {
            w.set_visible(false);
        }
    }

    /// 把主視窗叫到最前面：縮小的還原、藏起來的顯示。縮在系統列的話先恢復。
    fn bring_front(&mut self, el: &ActiveEventLoop) {
        self.unpark(el);
        if self.in_tray {
            self.in_tray = false;
            el.set_control_flow(ControlFlow::Poll);
            let _ = self.player.tx.send(player::Cmd::Tray(false));
            // 重繪迴圈在縮起來時斷了，要重新點一次火
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            self.last = Instant::now();
        }
        if let Some(w) = &self.window {
            w.set_visible(true);
            w.set_minimized(false);
            w.focus_window();
        }
    }

    /// 跳到 `t`（0..1）。
    fn seek_to(&mut self, t: f32) {
        let dur = self.now_flag(|n| n.duration_ms);
        if dur <= 0 {
            return;
        }
        self.seek_ms((t as f64 * dur as f64) as i64);
    }

    /// 跳到第 `ms` 毫秒。
    fn seek_ms(&mut self, ms: i64) {
        let dur = self.now_flag(|n| n.duration_ms);
        if dur <= 0 {
            return;
        }
        let ms = ms.clamp(0, dur);
        let _ = self.player.tx.send(player::Cmd::Seek(ms));
        // **先在本地跳過去**，不等下一次輪詢（最慢要 1.5 秒）——
        // 不然進度條會先彈回原位、過一下才跳到點的地方，
        // 看起來像是沒點到。web 版的 `posBase` 也是這樣。
        if let Ok(mut g) = self.player.shared.now.lock()
            && let Some(n) = g.as_mut() {
                n.progress_ms = ms;
                n.at = Instant::now();
            }
    }

    /// 歌詞頁的滾輪。`d` 是格數，往下捲為正；一格一句
    fn scroll_lyrics(&mut self, d: f32) {
        let Some((n, plain)) = self.player.shared.lyrics.lock().ok()
            .and_then(|g| g.as_ref().map(|l| (l.synced.len(), !l.plain.is_empty())))
        else { return };
        // 文字 PV 把清單換掉了，捲了也看不到（捲回正在唱的那句之前還會擋到點句跳轉）
        if n > 0 && self.pv_want {
            return;
        }
        if n == 0 {
            // 純文字歌詞照像素捲，一格 3 行
            if plain {
                self.plain_target = (self.plain_target + d * 90.0).clamp(0.0, self.lyric_hits.plain_max);
                self.plain_scroll.set(self.plain_target);
            }
            return;
        }
        // 從畫面現在要停的地方接著捲；第一次捲是從正在唱的那句開始
        let from = self.lyric_manual.map_or(self.lyric_scroll.target(), |(at, _)| at);
        let at = (from + d).clamp(0.0, (n - 1) as f32);
        self.lyric_manual = Some((at, Instant::now()));
    }

    /// 點到第 `i` 句：跳到它開始唱的地方，畫面捲回去跟著唱
    fn jump_to_line(&mut self, i: usize) {
        let Some(ms) = self.player.shared.lyrics.lock().ok().and_then(|g| {
            let l = g.as_ref()?;
            // 比對歌詞用的是「播放位置＋補償」，所以要扣回去。多給 30 ms：
            // 剛好落在句首的話，回報的位置稍微少一點就會算成上一句，亮的那句會先退一格再跳回來
            Some(l.synced.get(i)?.t - lyrics::shift(&self.cfg, Some(l)) + 30)
        }) else { return };
        self.seek_ms(ms);
        self.lyric_manual = None;
        self.lyric_flash = Some((i, Instant::now()));
    }

    /// 目前這一頁可以拖的進度條。視覺效果頁沒有。
    fn progress_rect(&self) -> Option<ui::Rect> {
        let r = self.r.as_ref()?;
        let (fw, fh) = gfx::logical(r.target.width, r.target.height);
        match self.view {
            View::Browse => Some(ui::flow(fw, fh, self.cfg.cover_scale).progress),
            View::Now => Some(ui::now(fw, fh).progress),
            View::Viz => None,
        }
    }

    fn volume_rect(&self) -> Option<ui::Rect> {
        let r = self.r.as_ref()?;
        let (fw, fh) = gfx::logical(r.target.width, r.target.height);
        match self.view {
            View::Browse => Some(ui::flow(fw, fh, self.cfg.cover_scale).volume),
            View::Now => Some(ui::now(fw, fh).volume),
            View::Viz => None,
        }
    }

    /// 正在播的那張在封面流上的位置（幽靈也算）。
    fn playing_index(&self) -> Option<usize> {
        let uri = self.now_flag(|n| n.album_uri.clone());
        if uri.is_empty() {
            return None;
        }
        (0..self.album_count()).find(|&i| self.album_at(i).is_some_and(|a| a.uri == uri))
    }

    /// 換到歌詞頁。中央不是正在播的那張的話，**先滑過去再換** ——
    /// 不然歌詞頁的封面跟剛剛眼前那張不一樣，會以為跳錯了。web 版的 `openLyrics()`。
    fn open_lyrics(&mut self) {
        match self.playing_index() {
            Some(i) if i != self.centre_index() => {
                self.target_centre = i as f32;
                self.lyrics_wait = Some((i as f32, Instant::now()));
            }
            _ => {
                self.lyrics_wait = None;
                self.set_view(View::Now);
            }
        }
    }

    /// 每幀往目標值收斂。指數衰減而不是線性 —— 停下來時才不會有突兀的煞車。
    fn animate(&mut self, dt: f32) {
        self.sync_library();
        self.animate_ghost(dt);
        self.animate_swap();
        if let Some(tr) = self.trans.as_mut() {
            tr.now = tr.start.elapsed().as_secs_f32();
        }
        if self.trans.as_ref().is_some_and(|t| t.done()) {
            self.trans = None;
        }
        let k = 1.0 - (-dt * 14.0).exp();
        self.centre += (self.target_centre - self.centre) * k;
        if (self.target_centre - self.centre).abs() < 0.001 {
            self.centre = self.target_centre;
        }
        if let Some((to, t0)) = self.lyrics_wait {
            if self.view != View::Browse || self.target_centre != to {
                // 使用者自己動了封面流（或已經不在封面流），排程作廢
                self.lyrics_wait = None;
            } else if (self.centre - to).abs() < 0.45 || t0.elapsed() > Duration::from_millis(900) {
                // 0.45：換頁那一刻中央那張一定要是目標。900ms 是逾時，web 版同一個數字
                self.lyrics_wait = None;
                self.set_view(View::Now);
            }
        }

        // 封面流那頁的控制列不自動收，只有歌詞與視覺效果會。
        // 設定面板開著的時候也不收 —— 底下的東西不該在使用者操作時消失。
        let want = if self.view == View::Browse
            || self.settings_open
            || self.devices_open
            || self.setup.is_some()
            || self.picker.is_some()
            || self.last_mouse.elapsed() < IDLE_HIDE
        {
            1.0f32
        } else {
            0.0
        };
        // **只有視覺效果頁是神燈**，歌詞頁就是整條往下收。
        // 兩者的時間長度差很多（1.25s vs 0.34s），曲線也不一樣。
        let genie = self.view == View::Viz;
        let dur = if !genie {
            BAR_SLIDE_S
        } else if want > self.chrome_t {
            SHOW_S
        } else {
            HIDE_S
        };
        let step = dt / dur;
        self.chrome_t = if want > self.chrome_t {
            (self.chrome_t + step).min(1.0)
        } else {
            (self.chrome_t - step).max(0.0)
        };
        self.chrome = if !genie {
            // CSS 的 cubic-bezier(.4,0,.2,1)，用 smoothstep 近似就夠了
            let t = self.chrome_t;
            t * t * (3.0 - 2.0 * t)
        } else if want > 0.5 {
            // 神燈：出來是甩出去之後慢慢歸位，收起來是加速捲進中心。
            // 方向不同曲線就不同，這點不能省。
            1.0 - (1.0 - self.chrome_t).powf(2.6)
        } else {
            self.chrome_t.powf(2.2)
        };
    }

    /// 設定面板開著時的鍵盤。**它會把按鍵吃掉** —— 不然在面板裡按方向鍵
    /// 會連帶捲動底下的封面流，兩件事同時動很難用。回傳有沒有處理掉。
    /// 打開裝置選單並重抓清單。清單每次都重抓 —— 裝置是會上線下線的，
    /// 沿用上次的結果很可能選到一台已經不在的。
    fn open_about(&mut self) {
        self.about_open = true;
        self.about_confirm = false;
        self.settings_open = false;
        self.help_open = false;
        self.devices_open = false;
    }

    fn close_about(&mut self) {
        self.about_open = false;
        self.about_confirm = false;
    }

    fn open_devices(&mut self) {
        if let Ok(mut g) = self.player.shared.devices.lock() {
            *g = None;
        }
        let _ = self.player.tx.send(player::Cmd::Devices);
        self.devices_open = true;
        self.devices_sel = 0;
        self.settings_open = false;
        self.help_open = false;
    }

    fn device_count(&self) -> usize {
        match self.player.shared.devices.lock().ok().as_deref() {
            Some(Some(Ok(v))) => v.len(),
            _ => 0,
        }
    }

    fn pick_device(&mut self, i: usize) {
        let id = match self.player.shared.devices.lock().ok().as_deref() {
            Some(Some(Ok(v))) => match v.get(i) {
                Some(d) => d.id.clone(),
                None => return,
            },
            _ => return,
        };
        match id {
            Some(id) => {
                let _ = self.player.tx.send(player::Cmd::Transfer(id));
                self.devices_open = false;
            }
            // 私人工作階段、受限裝置沒有 id，Web API 轉不過去
            None => self.toast("這個裝置不能從這裡切換"),
        }
    }

    /// 裝置選單開著時它吃掉全部的按鍵，跟設定面板同一個理由。
    fn on_key_devices(&mut self, key: &Key) {
        let n = self.device_count();
        match key {
            Key::Named(NamedKey::Escape) => self.devices_open = false,
            Key::Character(c) if c.as_str().eq_ignore_ascii_case("d") => self.devices_open = false,
            Key::Named(NamedKey::ArrowUp) if n > 0 => self.devices_sel = (self.devices_sel + n - 1) % n,
            Key::Named(NamedKey::ArrowDown) if n > 0 => self.devices_sel = (self.devices_sel + 1) % n,
            Key::Named(NamedKey::Enter) => self.pick_device(self.devices_sel),
            Key::Character(c) => {
                if let Some(d) = c.chars().next().and_then(|ch| ch.to_digit(10))
                    && d >= 1 {
                        self.pick_device(d as usize - 1);
                    }
            }
            _ => {}
        }
    }

    fn on_key_settings(&mut self, key: &Key) -> bool {
        let n = settings::ITEMS.len();
        match key {
            Key::Named(NamedKey::Escape) => self.settings_open = false,
            Key::Named(NamedKey::ArrowUp) => {
                self.settings_sel = (self.settings_sel + n - 1) % n;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.settings_sel = (self.settings_sel + 1) % n;
            }
            Key::Named(NamedKey::ArrowLeft) => self.change_setting(self.settings_sel, -1),
            Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::Enter) => {
                self.change_setting(self.settings_sel, 1)
            }
            Key::Character(c) if c.as_str() == "," => self.settings_open = false,
            _ => return false,
        }
        true
    }

    /// 設定精靈開著時它吃掉全部的按鍵。
    fn on_key_setup(&mut self, key: &Key) {
        let Some(st) = self.setup.as_mut() else { return };
        let ctrl = self.mods.control_key();
        match key {
            Key::Named(NamedKey::Escape) => self.setup = None,
            Key::Named(NamedKey::Enter) => self.act(ui::Hit::SetupSave),
            Key::Named(NamedKey::Backspace) if ctrl => st.input.text.clear(),
            Key::Named(NamedKey::Backspace) => st.input.backspace(),
            Key::Character(c) if ctrl && c.as_str().eq_ignore_ascii_case("v") => {
                if let Some(t) = clipboard::get_text() {
                    st.input.push_str(t.trim());
                }
            }
            // 其他 Ctrl 組合不是打字（Ctrl+A 之類的會送控制字元進來）
            Key::Character(_) if ctrl => {}
            Key::Character(c) => st.input.push_str(c.as_str()),
            _ => return,
        }
        if let Some(st) = self.setup.as_mut() {
            st.err.clear();
        }
        self.caret_t = Instant::now();
    }

    /// 搜尋框與 preset 選單的 Ctrl+V。回傳有沒有處理掉。
    fn paste_into_input(&mut self) -> bool {
        let Some(t) = clipboard::get_text() else { return true };
        // 單行輸入：換行換成空白
        let t = t.replace(['\r', '\n'], " ");
        if let Some(i) = self.search.as_mut() {
            i.push_str(&t);
            self.search_changed();
        } else if let Some(p) = self.picker.as_mut() {
            p.input.push_str(&t);
            self.picker_edited();
        } else {
            return false;
        }
        self.caret_t = Instant::now();
        true
    }

    /// 精靈的「儲存」。格式不對就把原因寫在框下面，對了就存檔、交給 poller、直接開始登入。
    fn setup_save(&mut self) {
        let Some(st) = self.setup.as_mut() else { return };
        match scene_setup::validate(&st.input.text) {
            Err(e) => st.err = e,
            Ok(id) => {
                self.cfg.client_id = id.clone();
                self.save_cfg();
                let _ = self.player.tx.send(player::Cmd::SetClientId(id));
                let _ = self.player.tx.send(player::Cmd::Login);
                self.setup = None;
                self.toast("Client ID 已儲存，接著在瀏覽器裡授權");
            }
        }
    }

    fn on_key(&mut self, el: &ActiveEventLoop, key: &Key) {
        if self.setup.is_some() {
            self.on_key_setup(key);
            return;
        }
        let paste = self.mods.control_key()
            && matches!(key, Key::Character(c) if c.as_str().eq_ignore_ascii_case("v"));
        if paste && self.paste_into_input() {
            return;
        }
        // 打字框開著的時候它吃掉全部的按鍵 —— 不然打「s」會同時切排序
        if self.search.is_some() {
            self.on_key_search(key);
            return;
        }
        if self.picker.is_some() {
            self.on_key_picker(key);
            return;
        }
        if self.devices_open {
            self.on_key_devices(key);
            return;
        }
        if self.settings_open && self.on_key_settings(key) {
            return;
        }
        if self.about_open {
            if matches!(key, Key::Named(NamedKey::Escape))
                || matches!(key, Key::Character(c) if c.as_str().eq_ignore_ascii_case("i"))
            {
                self.close_about();
            }
            return;
        }
        // 說明開著的時候按什麼都先把它關掉，不然它會一直擋在前面
        if self.help_open {
            self.help_open = false;
            if matches!(key, Key::Named(NamedKey::Escape))
                || matches!(key, Key::Character(c) if c.as_str() == "?")
            {
                return;
            }
        }
        let browse = self.view == View::Browse;
        match key {
            // 說明只有「?」。h 在視覺效果頁是收藏，在封面流是跳字母（Python 版的對應）
            Key::Character(c) if c.as_str() == "?" => {
                self.help_open = true;
                self.settings_open = false;
            }
            Key::Character(c) if c.as_str().eq_ignore_ascii_case("i") => self.open_about(),
            // Esc 只做「退一步」，**不關程式**（關程式是 q）：
            // 歌詞／視覺效果 → 封面流；篩選 → 清掉；展開 → 收起；全螢幕 → 視窗
            Key::Named(NamedKey::Escape) if !browse => self.set_view(View::Browse),
            Key::Named(NamedKey::Escape) if self.filtered.is_some() => {
                let here = self.current_id();
                self.clear_filter(here);
            }
            Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Backspace)
                if self.drill.is_some() && browse =>
            {
                self.close_drill()
            }
            Key::Named(NamedKey::Escape) if self.screen_mode != ScreenMode::Windowed => {
                self.set_screen_mode(ScreenMode::Windowed)
            }
            // 封面流 → 歌詞 → 視覺效果（關掉的話跳過）→ 封面流
            Key::Named(NamedKey::Tab) => match self.view {
                View::Browse => self.open_lyrics(),
                View::Now if self.cfg.viz_enabled => self.set_view(View::Viz),
                _ => self.set_view(View::Browse),
            },
            // 視覺效果頁的左右鍵換 preset（web 版的 viz.js 就是左右），
            // 封面流在那一頁根本看不到，捲它沒有意義
            Key::Named(NamedKey::ArrowRight) if self.view == View::Viz => self.viz.step(1),
            Key::Named(NamedKey::ArrowLeft) if self.view == View::Viz => self.viz.step(-1),
            Key::Named(NamedKey::ArrowRight) => self.step(1.0),
            Key::Named(NamedKey::ArrowLeft) => self.step(-1.0),
            Key::Named(NamedKey::PageDown) => self.step(10.0),
            Key::Named(NamedKey::PageUp) => self.step(-10.0),
            Key::Named(NamedKey::Home) => {
                self.user_touched = true;
                self.target_centre = 0.0;
            }
            Key::Named(NamedKey::End) => {
                self.user_touched = true;
                self.target_centre = (self.album_count() as f32 - 1.0).max(0.0);
            }
            Key::Named(NamedKey::Space) => {
                let _ = self.player.tx.send(player::Cmd::Toggle);
            }
            // 跟點中央那張封面同一件事
            Key::Named(NamedKey::Enter) if self.view == View::Browse => {
                self.act(ui::Hit::PlayCurrent)
            }
            // 視覺效果頁的 Enter 是 preset 選單（web 版也是）
            Key::Named(NamedKey::Enter) if self.view == View::Viz => self.open_picker(),
            Key::Character(c) if c.as_str() == "/" => match self.view {
                View::Browse => self.open_search(),
                View::Viz => self.open_picker(),
                View::Now => {}
            },
            // 視覺效果頁的 p 是鎖定，不是上一首 —— 跟 web 版一樣
            Key::Character(c) if c.as_str() == "p" && self.view == View::Viz => {
                self.viz.locked = !self.viz.locked;
                self.toast(if self.viz.locked { "已鎖定這個 preset" } else { "解除鎖定" });
            }
            // 三段循環：視窗 → 這個螢幕 → 跨所有螢幕 → 視窗。設定可以拿掉後兩段其中之一
            Key::Named(NamedKey::F11) => {
                let next = self.screen_mode.next_in(&self.cfg.f11_modes);
                self.set_screen_mode(next);
                self.toast(match next {
                    ScreenMode::Windowed => "視窗",
                    ScreenMode::One => "全螢幕",
                    ScreenMode::Span => "跨所有螢幕",
                });
            }
            // 上下鍵在每一頁都是上一首／下一首（視覺效果頁也是，換 preset 用左右），
            // 跟 web 版一樣
            Key::Named(NamedKey::ArrowUp) => {
                let _ = self.player.tx.send(player::Cmd::Prev);
            }
            Key::Named(NamedKey::ArrowDown) => {
                let _ = self.player.tx.send(player::Cmd::Next);
            }
            // 歌詞時間：一次 100ms，0 歸零。跟設定面板那列同一個欄位、同一個範圍
            Key::Character(c) if c.as_str() == "[" => self.bump_offset(-100),
            Key::Character(c) if c.as_str() == "]" => self.bump_offset(100),
            Key::Character(c) if c.as_str() == "0" => self.bump_offset(i64::MIN),
            // l：在封面流就去歌詞頁、在歌詞頁回封面流、在視覺效果回歌詞頁（web 版的對應）
            Key::Character(c) if c.as_str().eq_ignore_ascii_case("l") => match self.view {
                View::Browse => self.open_lyrics(),
                View::Now => self.set_view(View::Browse),
                View::Viz => self.set_view(View::Now),
            },
            Key::Character(c) if c.as_str().eq_ignore_ascii_case("q") => self.quit(el),
            Key::Character(c) if c.as_str() == "c" && self.view == View::Viz => {
                self.act(ui::Hit::Category)
            }
            Key::Character(c) if c.as_str().eq_ignore_ascii_case("h") && self.view == View::Viz => {
                self.act(ui::Hit::Favourite)
            }
            Key::Character(c) if c.as_str().eq_ignore_ascii_case("f") && self.view == View::Viz => {
                self.change_item(settings::Item::VizFps, 1);
                let v = settings::Item::VizFps.value(&self.cfg, false, "", "", "");
                self.toast(format!("影格上限：{v}"));
            }
            // 以下這幾個鍵是照 web 版的對應表來的，讓兩邊的手感一致。
            Key::Character(c) if c.as_str().eq_ignore_ascii_case("d") => self.open_devices(),
            Key::Character(c) if c.as_str() == "," => self.prefs_want = true,
            // 舊的設定面板：新視窗跑穩之前留著對照，Shift+逗號
            Key::Character(c) if c.as_str() == "<" => {
                self.settings_open = true;
                self.settings_sel = 0;
            }
            Key::Character(c) if c.as_str() == "s" && self.view == View::Browse => {
                const ORDER: [&str; 4] = ["added", "name", "artist", "year"];
                let i = ORDER.iter().position(|v| *v == self.cfg.sort).unwrap_or(0);
                self.cfg.sort = ORDER[(i + 1) % ORDER.len()].to_string();
                let i = settings::ITEMS.iter().position(|it| *it == settings::Item::Sort);
                // 走設定面板同一條路，副作用（重排、保持中央那張）才不會漏
                if let Some(i) = i {
                    self.change_setting(i, 0);
                    let v = settings::Item::Sort.value(&self.cfg, false, "", "", "");
                    self.toast(format!("排序：{v}"));
                }
            }
            // 封面流的字母鍵是跳字母，V 只在歌詞頁有作用（Python 版也是）
            Key::Character(c) if c.as_str().eq_ignore_ascii_case("v") && self.view == View::Now => {
                self.change_item(settings::Item::Vinyl, 1);
                self.toast(if self.cfg.vinyl { "唱片模式：開" } else { "唱片模式：關" });
            }
            // 文字 PV（實驗中）。封面流的字母鍵是跳字母，P 只在歌詞頁有作用（視覺效果頁的 p 是 preset 選單）
            Key::Character(c) if c.as_str().eq_ignore_ascii_case("p") && self.view == View::Now => {
                // 跟設定視窗那個開關走同一條路，開著的設定視窗才會跟著更新
                self.set_item(settings::Item::LyricsPv, settings::Edit::Press);
                let words = self.player.shared.lyrics.lock().ok().is_some_and(|g| g.as_ref().is_some_and(|l| l.has_words()));
                self.toast(match (self.cfg.lyrics_pv, words) {
                    (true, true) => "文字 PV（實驗中）：開",
                    (true, false) => "文字 PV（實驗中）：開，這首沒有逐字歌詞，先維持一般歌詞頁",
                    (false, _) => "文字 PV（實驗中）：關",
                });
            }
            // 文字 PV 的風格：自動 → 霓虹 → 紙本 → 衝擊 → 故障 → 浮遊 → 自動
            Key::Character(c) if c.as_str().eq_ignore_ascii_case("o") && self.view == View::Now && self.cfg.lyrics_pv => {
                use pv::style::LOOKS;
                self.pv_look = match self.pv_look {
                    None => Some(LOOKS[0]),
                    Some(l) => LOOKS.iter().position(|&x| x == l).and_then(|i| LOOKS.get(i + 1)).copied(),
                };
                self.toast(match self.pv_look {
                    Some(l) => format!("PV 風格：{}", l.name()),
                    None => "PV 風格：自動（照段落換）".to_string(),
                });
            }
            // 封面流的字母鍵是跳字母，T 只在歌詞頁有作用
            Key::Character(c) if c.as_str().eq_ignore_ascii_case("t") && self.view == View::Now => {
                self.cfg.lyrics_trans = !self.cfg.lyrics_trans;
                self.save_cfg();
                self.toast(if self.cfg.lyrics_trans { "歌詞翻譯：開" } else { "歌詞翻譯：關" });
            }
            Key::Character(c) if c.as_str() == "k" => {
                self.cfg.ktv = !self.cfg.ktv;
                self.save_cfg();
                self.toast(if self.cfg.ktv { "KTV 提詞：開" } else { "KTV 提詞：關" });
            }
            Key::Character(c) if c.as_str() == "r" => {
                let _ = self.player.tx.send(player::Cmd::RefreshAlbums);
                self.toast("重抓收藏清單中…");
            }
            Key::Character(c) if c.as_str() == "x" => self.act(ui::Hit::Shuffle),
            Key::Character(c) if c.as_str() == "e" => self.act(ui::Hit::Repeat),
            // 封面大小。Python 版也是這三顆，而且存在同一份 config.json
            Key::Character(c) if c.as_str() == "-" => self.zoom_covers(-0.1),
            Key::Character(c) if c.as_str() == "=" || c.as_str() == "+" => self.zoom_covers(0.1),
            Key::Character(c) if c.as_str() == "\\" => self.set_cover_scale(1.0),
            // 沒被上面吃掉的字母（n、p 也是：Python 版沒有這兩顆，上下鍵就是換歌）：跳到下一張以它開頭的專輯
            Key::Character(c) if self.view == View::Browse => {
                let mut it = c.chars();
                if let (Some(ch), None) = (it.next(), it.next())
                    && ch.is_ascii_alphabetic() {
                        self.jump_letter(ch);
                    }
            }
            _ => {}
        }
    }

    fn jump_letter(&mut self, ch: char) {
        // 把幽靈也算進去排成畫面上的順序，索引才對得上
        let shown: Vec<spotify::Album> =
            (0..self.album_count()).filter_map(|i| self.album_at(i).cloned()).collect();
        if let Some(i) = search::jump_letter(&shown, self.centre_index(), ch) {
            self.user_touched = true;
            self.target_centre = i as f32;
        }
    }

    /// 開著的大面板有沒有蓋到底部控制列。視窗夠大時面板都停在控制列上方，
    /// 只有矮視窗（例如 800x600）才會蓋到
    fn panel_over_bar(&self, fw: f32, fh: f32) -> bool {
        let bar = match self.view {
            View::Browse => ui::flow(fw, fh, self.cfg.cover_scale).bar,
            View::Viz => ui::viz(fw, fh).bar,
            View::Now => return false,
        };
        let mut panels = Vec::new();
        if self.settings_open {
            panels.push(ui::settings(fw, fh, settings::ITEMS.len()).panel);
        }
        if self.devices_open {
            panels.push(ui::devices(fw, fh, self.device_count()).panel);
        }
        if self.about_open {
            panels.push(ui::about(fw, fh).panel);
        }
        if self.help_open {
            panels.push(scene_help::panel_rect(fw, fh));
        }
        if self.setup.is_some() {
            panels.push(ui::setup(fw, fh).panel);
        }
        if self.picker.is_some() {
            panels.push(ui::picker(fw, fh).panel);
        }
        panels.iter().any(|p| p.overlaps(&bar))
    }

    /// 游標底下是什麼。三個視圖各有自己的版面。
    fn hit_test(&self) -> Option<ui::Hit> {
        let r = self.r.as_ref()?;
        let (fw, fh) = gfx::logical(r.target.width, r.target.height);
        let (mx, my) = self.cursor;
        if let Some(p) = &self.picker {
            return ui::hit_picker(&ui::picker(fw, fh), p.top, p.rows.len(), mx, my);
        }
        if self.setup.is_some() {
            return ui::hit_setup(&ui::setup(fw, fh), mx, my);
        }
        if self.about_open {
            return ui::hit_about(&ui::about(fw, fh), mx, my);
        }
        if self.devices_open {
            let n = self.device_count();
            return ui::hit_devices(&ui::devices(fw, fh, n), n, mx, my);
        }
        // 設定面板開著的時候它吃掉全部的點擊 —— 疊層本來就該擋住底下的東西
        if self.settings_open {
            return ui::hit_settings(&ui::settings(fw, fh, settings::ITEMS.len()), mx, my);
        }
        match self.view {
            View::Browse => {
                if self.drill.is_some() && ui::drill_back(self.back_w).contains(mx, my) {
                    return Some(ui::Hit::DrillBack);
                }
                ui::hit_flow(&ui::flow(fw, fh, self.cfg.cover_scale), self.album_count(), self.centre, mx, my)
            }
            View::Now => {
                let ly = self.player.shared.lyrics.lock().ok().and_then(|g| g.clone());
                let empty = scene_now::shows_lrclib(ly.as_ref());
                // 文字 PV：左欄淡掉的時候不給點（看不到的鍵按到很怪），也沒有可以點的歌詞
                if self.pv_want {
                    return (self.chrome > 0.3).then(|| ui::hit_now(&ui::now(fw, fh), self.chrome, empty, mx, my)).flatten();
                }
                ui::hit_now(&ui::now(fw, fh), self.chrome, empty, mx, my)
                    .or_else(|| self.lyric_hits.at(mx, my).map(ui::Hit::Lyric))
            }
            View::Viz => ui::hit_viz(&ui::viz(fw, fh), self.chrome, mx, my),
        }
    }

    /// 真正執行一個動作。命中測試只說「點到什麼」，要做什麼在這裡決定。
    fn act(&mut self, hit: ui::Hit) {
        use ui::Hit;
        match hit {
            Hit::Cover(i) => {
                self.user_touched = true;
                self.target_centre = i as f32;
            }
            Hit::PlayCurrent => self.play_current(),
            Hit::Lyric(i) => self.jump_to_line(i),
            Hit::DrillBack => self.close_drill(),
            Hit::Like => self.toggle_like(),
            Hit::Devices => self.open_devices(),
            Hit::Mute => self.toggle_mute(),
            Hit::Volume(t) => {
                self.vol_drag = true;
                self.set_volume((t * 100.0).round() as i32, false);
            }
            Hit::Prev => {
                let _ = self.player.tx.send(player::Cmd::Prev);
            }
            Hit::Next => {
                let _ = self.player.tx.send(player::Cmd::Next);
            }
            Hit::PlayPause => {
                let _ = self.player.tx.send(player::Cmd::Toggle);
            }
            // 進度條：按下開始拖，拖動中只改畫面，放開才真的跳（Python 版 v1.0.4）。
            // 視覺效果頁沒有可以拖的那條，照舊點了就跳
            Hit::Seek(t) if self.progress_rect().is_some() => self.seek_drag = Some(t),
            Hit::Seek(t) => self.seek_to(t),
            Hit::OpenLyrics => self.open_lyrics(),
            Hit::PresetPrev => self.viz.step(-1),
            Hit::PresetNext => self.viz.step(1),
            Hit::Favourite => {
                self.viz.toggle_favourite();
                self.cfg.viz_favourites = self.viz.favourites.clone();
                self.save_cfg();
                let name = self.viz.current_name().to_string();
                let on = self.viz.favourites.contains(&name);
                self.toast(if on { "加入我的最愛" } else { "移出我的最愛" });
            }
            Hit::Category => {
                let next = self.viz.cat.next();
                self.viz.set_category(next);
                self.cfg.viz_category = next.key().to_string();
                self.save_cfg();
                self.toast(format!("分類：{}", next.label()));
            }
            Hit::Shuffle => {
                let on = !self.now_flag(|n| n.shuffle);
                let _ = self.player.tx.send(player::Cmd::Shuffle(on));
                self.toast(if on { "隨機播放：開" } else { "隨機播放：關" });
            }
            Hit::Repeat => {
                let next = match self.now_flag(|n| n.repeat) {
                    smtc::Repeat::Off => smtc::Repeat::Context,
                    smtc::Repeat::Context => smtc::Repeat::Track,
                    smtc::Repeat::Track => smtc::Repeat::Off,
                };
                let _ = self.player.tx.send(player::Cmd::Repeat(next));
                // 用語照 web 版的 REPEAT_LABEL
                self.toast(match next {
                    smtc::Repeat::Off => "不重複",
                    smtc::Repeat::Context => "重複專輯",
                    smtc::Repeat::Track => "單曲重複",
                });
            }
            Hit::Settings => self.prefs_want = true,
            Hit::CloseSettings => self.settings_open = false,
            Hit::DeviceRow(i) => {
                self.devices_sel = i;
                self.pick_device(i);
            }
            Hit::CloseDevices => self.devices_open = false,
            Hit::CloseAbout => self.close_about(),
            Hit::AboutGuide => {
                if let Err(e) = oauth::open_in_browser(config::GUIDE_URL) {
                    self.toast(format!("開不了瀏覽器：{e}"));
                }
            }
            Hit::AboutLicenses => {
                if let Err(e) = open_licenses() {
                    self.toast(format!("開不了授權檔：{e}"));
                }
            }
            Hit::AboutDisconnect if !self.about_confirm => self.about_confirm = true,
            Hit::AboutDisconnect => {
                let _ = self.player.tx.send(player::Cmd::Disconnect);
                self.close_about();
            }
            Hit::SetupDashboard => {
                if let Err(e) = oauth::open_in_browser(scene_setup::DASHBOARD_URL) {
                    self.toast(format!("開不了瀏覽器：{e}"));
                } else if let Some(st) = self.setup.as_mut() {
                    st.dash_opened = true;
                }
            }
            Hit::SetupCopy => {
                let ok = clipboard::set_text(&self.cfg.redirect_uri());
                match self.setup.as_mut() {
                    Some(st) if ok => st.copied = true,
                    _ => self.toast("複製失敗，請手動選取"),
                }
            }
            Hit::SetupSave => self.setup_save(),
            Hit::SetupClose => self.setup = None,
            Hit::OpenLrclib => {
                if let Err(e) = oauth::open_in_browser("https://lrclib.net/") {
                    self.toast(format!("開不了瀏覽器：{e}"));
                }
            }
            Hit::PickerRow(i) => {
                if let Some(p) = self.picker.as_mut() {
                    p.at = i;
                }
                self.close_picker(true);
            }
            Hit::PickerStar(i) => {
                let item = self.picker.as_ref().and_then(|p| p.rows.get(i).copied());
                if let Some(item) = item {
                    let name = self.viz.item_name(item).to_string();
                    self.viz.toggle_favourite_of(&name);
                    self.cfg.viz_favourites = self.viz.favourites.clone();
                    self.save_cfg();
                    // 在「我的最愛」裡取消收藏，那一列要消失
                    if self.viz.cat == viz::Category::Fav {
                        let rows = self.picker_rows();
                        if let Some(p) = self.picker.as_mut() {
                            let top = p.top;
                            Self::picker_rebuild(&self.viz, p, rows);
                            p.top = top.min(p.rows.len().saturating_sub(rows));
                        }
                    }
                }
            }
            Hit::PickerCat(i) => self.picker_set_category(ui::PICKER_CATS[i]),
            Hit::PickerClose => self.close_picker(false),
            Hit::SettingRow(i) => self.change_setting(i, 1),
        }
    }

    /// 用快捷鍵改某一項設定：跟點設定面板那列走同一條路，副作用才不會漏。
    fn change_item(&mut self, item: settings::Item, d: i32) {
        if let Some(i) = settings::ITEMS.iter().position(|it| *it == item) {
            let keep = self.settings_sel;
            self.change_setting(i, d);
            self.settings_sel = keep;
        }
    }

    /// 改設定面板的第 `i` 列。`d` 是方向。
    fn change_setting(&mut self, i: usize, d: i32) {
        let Some(item) = settings::ITEMS.get(i).copied() else { return };
        if item.read_only() {
            return;
        }
        self.settings_sel = i;
        self.set_item(item, settings::Edit::Cycle(d));
    }

    /// 改一項設定並處理副作用。舊面板、快捷鍵、設定視窗都走這裡。
    ///
    /// 有些設定改完要有人去通知對應的子系統 —— 光存進 `cfg` 不會生效。
    fn set_item(&mut self, item: settings::Item, e: settings::Edit) {
        item.apply(&mut self.cfg, e);
        if let Some(p) = self.prefs.as_mut() {
            p.dirty = true;
        }
        match item {
            settings::Item::VizCategory => {
                self.viz.set_category(viz::Category::parse_pub(&self.cfg.viz_category))
            }
            settings::Item::VizShaders => self.viz.set_shaders(self.cfg.viz_preset_shaders),
            settings::Item::VizFps => self.viz.set_cap(self.cfg.viz_fps),
            // 關掉的時候人正在視覺效果頁的話，送回封面流
            settings::Item::VizEnabled if !self.cfg.viz_enabled && self.view == View::Viz => {
                self.set_view(View::Browse)
            }
            settings::Item::Screen => self.move_to_screen(),
            settings::Item::CloseToTray => self.sync_tray(),
            // 版本那列是按鈕：看狀態決定要檢查、安裝，還是開下載頁
            settings::Item::Version => {
                match self.updater.stage() {
                    update::Stage::Available { installable: true, .. } => {
                        let px = self.proxy.clone();
                        self.updater.install(move || {
                            let _ = px.send_event(UserEv::UpdateReady);
                        });
                    }
                    update::Stage::Available { .. } => {
                        if let Err(e) = oauth::open_in_browser(&self.updater.page()) {
                            self.toast(format!("開不了瀏覽器：{e}"));
                        }
                    }
                    update::Stage::Checking | update::Stage::Downloading { .. } | update::Stage::Ready => {}
                    _ => self.updater.check(false),
                }
                return;
            }
            settings::Item::Refresh => {
                let _ = self.player.tx.send(player::Cmd::RefreshAlbums);
                self.settings_open = false;
                self.toast("重抓收藏清單中…");
                return;
            }
            // 這列是按鈕不是選項：看目前狀態決定要登入還是登出
            // 還沒有 client_id 就別登入了（登入一定失敗），直接開精靈
            settings::Item::SpotifyStatus if !self.cfg.client_id_looks_valid() => {
                self.settings_open = false;
                self.setup = Some(Default::default());
                return;
            }
            settings::Item::SpotifyStatus => {
                let sh = &self.player.shared;
                let authed = sh.authed.load(std::sync::atomic::Ordering::Relaxed);
                // 缺權限的時候按下去是重新授權，不是登出
                let reauth = sh.need_reauth.load(std::sync::atomic::Ordering::Relaxed);
                let cmd = if authed && !reauth { player::Cmd::Logout } else { player::Cmd::Login };
                let _ = self.player.tx.send(cmd);
            }
            // 這列也是按鈕：換成裝置選單（它會把設定面板關掉）
            settings::Item::Device => {
                self.open_devices();
                return;
            }
            settings::Item::About => {
                self.open_about();
                return;
            }
            settings::Item::MiniRecall => {
                self.recall_mini();
                return;
            }
            settings::Item::Sort => {
                // 排序換了，中央那張要跟著它走，不然畫面會跳到別張專輯
                let keep = self.album_at(self.centre_index()).map(|a| a.id.clone());
                sort_library(&mut self.library, &self.cfg.sort);
                // 篩選中的話結果也要照新的順序重排
                self.refilter();
                if let Some(id) = keep
                    && let Some(i) = self.list().iter().position(|a| a.id == id) {
                        let i = if self.ghost_shown() && i >= self.ghost_at { i + 1 } else { i };
                        self.jump_to(i);
                    }
            }
            settings::Item::Appearance => self.sync_appearance(),
            settings::Item::FontScale => self.sync_font(),
            // 曲名、清單這些下一幀就換；已經抓回來的歌詞要等下一首才照新設定
            settings::Item::LyricsTraditional => zh::set_enabled(self.cfg.lyrics_traditional),
            // 拖曳中先不套：設定視窗會跟著縮放，滑桿就在游標底下跑掉。放開時（`DragEnd`）才套
            settings::Item::UiScale
                if self.prefs.as_ref().and_then(|p| p.dragging()) != Some(settings::Item::UiScale) =>
            {
                self.sync_zoom()
            }
            // 舊面板這列是唯讀的，設定視窗才有「打開資料夾」鈕
            settings::Item::DataDir => {
                if let Err(e) = std::process::Command::new("explorer").arg(&self.paths.data).spawn() {
                    self.toast(format!("開不了資料夾：{e}"));
                }
                return;
            }
            _ => {}
        }
        self.save_cfg();
    }

    /// 從「正在播的那首」讀一個欄位。沒有在播就給預設值。
    fn now_flag<T: Default>(&self, f: impl Fn(&player::NowPlaying) -> T) -> T {
        self.player.shared.now.lock().ok().and_then(|g| g.as_ref().map(&f)).unwrap_or_default()
    }

    /// 把視窗搬到設定指定的那個螢幕。設成「自動」就不動它。
    ///
    /// 螢幕的順序是 `available_monitors()` 給的，跟 Windows 顯示設定裡的
    /// 編號**不保證一致** —— 所以面板上寫的是「第 N 個」而不是螢幕名稱。
    fn move_to_screen(&mut self) {
        let (Some(win), Some(i)) = (self.window.as_ref(), self.cfg.screen) else { return };
        let Some(m) = win.available_monitors().nth(i as usize) else {
            println!("沒有第 {i} 個螢幕，維持現狀");
            return;
        };
        match self.screen_mode {
            // 跨所有螢幕的時候「搬到第 N 個螢幕」沒有意義
            ScreenMode::Span => return,
            ScreenMode::One => {
                win.set_fullscreen(Some(Fullscreen::Borderless(Some(m))));
                return;
            }
            ScreenMode::Windowed => {}
        }
        let (pos, size) = (m.position(), m.size());
        let inner = win.inner_size();
        win.set_outer_position(winit::dpi::PhysicalPosition::new(
            pos.x + (size.width as i32 - inner.width as i32) / 2,
            pos.y + (size.height as i32 - inner.height as i32) / 2,
        ));
    }

    /// 封面大小。範圍跟 Python 版一樣是 0.5～2.0。
    /// `[` `]` `0`：調的是**這一首**的專屬微調（存在 offsets.json），換歌就換一份；
    /// 全域的那個在設定面板調。還沒有歌詞可調的時候（沒在播、載入中）退回調全域。
    fn bump_offset(&mut self, d: i64) {
        let id = self.player.shared.now.lock().ok().and_then(|g| g.as_ref().map(|n| n.id.clone()));
        let per_track = id.filter(|i| !i.is_empty()).and_then(|id| {
            let mut g = self.player.shared.lyrics.lock().ok()?;
            let l = g.as_mut().filter(|l| l.source != "loading")?;
            // d 是「相對現在」的增量；0 鍵傳進來的是 i64::MIN，代表歸零
            l.offset_ms = if d == i64::MIN { 0 } else { (l.offset_ms + d).clamp(-3000, 3000) };
            lyrics::set_track_offset(&self.paths.lyrics, &id, l.offset_ms);
            Some(l.offset_ms)
        });
        match per_track {
            Some(ms) => self.toast(format!("這首歌詞時間 {ms:+}ms（全域 {:+}ms）", self.cfg.lyrics_offset_ms)),
            None => {
                self.cfg.lyrics_offset_ms = if d == i64::MIN { 0 } else { (self.cfg.lyrics_offset_ms + d).clamp(-3000, 3000) };
                self.save_cfg();
                self.toast(format!("歌詞時間 {:+}ms", self.cfg.lyrics_offset_ms));
            }
        }
    }

    fn zoom_covers(&mut self, d: f32) {
        self.set_cover_scale(self.cfg.cover_scale + d);
    }

    fn set_cover_scale(&mut self, v: f32) {
        self.cfg.cover_scale = ((v * 100.0).round() / 100.0).clamp(0.5, 2.0);
        self.save_cfg();
        self.toast(format!("封面大小 {:.0}%", self.cfg.cover_scale * 100.0));
    }

    /// 底部通知列講一句話。
    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
        self.stir = Instant::now();
    }

    /// 設定改動就立刻落地。這些是使用者按鍵直接造成的，不該等到關程式才存。
    fn save_cfg(&self) {
        if let Err(e) = config::write_json(&self.paths.config, &self.cfg) {
            crate::warn!("設定存不進去：{e}");
        }
    }

    fn set_screen_mode(&mut self, mode: ScreenMode) {
        let Some(w) = self.window.clone() else {
            self.screen_mode = mode;
            return;
        };
        sync_min_size(&w, mode);
        // 離開視窗模式之前先把位置大小記下來，回來時放回原處
        if self.screen_mode == ScreenMode::Windowed && mode != ScreenMode::Windowed
            && let Ok(pos) = w.outer_position() {
                self.windowed = Some((pos, w.inner_size()));
            }
        self.screen_mode = mode;
        match mode {
            ScreenMode::Windowed => {
                w.set_fullscreen(None);
                w.set_decorations(true);
                if let Some((pos, size)) = self.windowed.take() {
                    let _ = w.request_inner_size(size);
                    w.set_outer_position(pos);
                }
            }
            ScreenMode::One => {
                w.set_decorations(true);
                w.set_fullscreen(Some(Fullscreen::Borderless(None)));
            }
            // **跨螢幕不能用 winit 的全螢幕** —— `Fullscreen::Borderless` 一次
            // 只吃得下一個螢幕。要跨就得自己來：退出全螢幕、拆掉邊框、
            // 把視窗擺成所有螢幕的聯集（虛擬桌面的外接矩形）。
            //
            // 聯集要用實體像素算。各螢幕的縮放比例可以不一樣，
            // 但 `position()`／`size()` 都已經是實體像素，直接取極值就對了。
            ScreenMode::Span => {
                let Some(r) = span_rect(&w) else {
                    println!("找不到任何螢幕，維持現狀");
                    self.screen_mode = ScreenMode::One;
                    w.set_fullscreen(Some(Fullscreen::Borderless(None)));
                    return;
                };
                w.set_fullscreen(None);
                w.set_decorations(false);
                // 順序有關係：先放大再定位，Windows 才不會把還是小尺寸的
                // 視窗夾回原本那個螢幕的工作區裡。
                let _ = w.request_inner_size(winit::dpi::PhysicalSize::new(r.2, r.3));
                w.set_outer_position(winit::dpi::PhysicalPosition::new(r.0, r.1));
            }
        }
    }

    /// 把收藏清單（或示範資料）組成這一幀要畫的封面。
    fn build_albums(
        art: &mut art::ArtCache,
        library: &[spotify::Album],
        fake: &[scene_flow::Album],
        // 「收藏是空的」跟「篩到沒東西」要分開 —— 後者不能退回示範資料
        use_fake: bool,
        ghost: Option<(&spotify::Album, usize)>,
        // 幽靈的進退場進度與換圖脈衝
        ghost_anim: f32,
        pulse: Option<(f32, f32)>,
    ) -> Vec<scene_flow::Album> {
        if use_fake {
            return fake
                .iter()
                .map(|a| scene_flow::Album {
                    name: a.name.clone(),
                    artist: a.artist.clone(),
                    art: a.art.clone(),
                    fx: Default::default(),
                })
                .collect();
        }
        let mut out: Vec<scene_flow::Album> = library
            .iter()
            .map(|a| scene_flow::Album {
                name: a.name.clone(),
                // 中央那張底下那行：專輯、清單、曲目各自不同
                artist: a.subtitle(),
                // 側面用 300px 就夠，中央那張才值得用 640
                art: art.get(if a.art_sm.is_empty() { &a.art } else { &a.art_sm }),
                fx: Default::default(),
            })
            .collect();
        // 幽靈插在 `ghost_at`，不是接在最後面。SMTC 那條沒有網址，
        // 封面是縮圖 blob，上傳時用曲目 id 當鍵，所以這裡也用 id 去拿。
        if let Some((g, at)) = ghost {
            let at = at.min(out.len());
            // 進退場：本體從半大小淡入，右邊的封面從 idx−1 滑到 idx（「被推開讓位」）
            let (ps, pa) = pulse.unwrap_or((1.0, 1.0));
            let fx = scene_flow::CoverFx {
                shift: 0.0,
                scale: (0.5 + 0.5 * ghost_anim) * ps,
                alpha: (ghost_anim * 1.5).min(1.0) * pa,
            };
            out.insert(
                at,
                scene_flow::Album {
                    name: g.name.clone(),
                    artist: g.artist.clone(),
                    art: if g.art.is_empty() { art.get_cached(&g.id) } else { art.get(&g.art) },
                    fx,
                },
            );
            if ghost_anim < 1.0 {
                for a in out.iter_mut().skip(at + 1) {
                    a.fx.shift = 1.0 - ghost_anim;
                }
            }
        }
        out
    }

    fn draw(&mut self) {
        audio::set_viz(self.view == View::Viz);
        // 只在這首能演（或剛演完）時擷取，沒逐字的歌設定開著也不開
        audio::set_pv(
            self.view == View::Now && self.cfg.lyrics_pv && self.pv_ear.is_some_and(|t| t.elapsed() < PV_EAR_KEEP),
        );
        let now = self.player.shared.now.lock().ok().and_then(|g| g.clone());
        if now.as_ref().is_some_and(|n| n.playing) {
            self.stir = Instant::now();
        }
        self.sync_playing(&now);
        let from_update = self.updater.toast.lock().ok().and_then(|mut g| g.take());
        if let Some(m) = from_update {
            self.toast(m);
        }
        // poller 那邊要說的話（播放失敗、清單抓完）
        let from_poller = self.player.shared.toast.lock().ok().and_then(|mut g| g.take());
        if let Some(m) = from_poller {
            self.toast(m);
        }
        // 播放因為沒有裝置而失敗：直接把選單打開（web 版的 playAlbum 也是）
        let nd = self.player.shared.no_device_rev.load(std::sync::atomic::Ordering::Relaxed);
        if nd != self.no_device_seen {
            self.no_device_seen = nd;
            self.open_devices();
        }
        if self.toast.as_ref().is_some_and(|(_, t)| t.elapsed().as_secs_f32() > scene_toast::LIFE_S) {
            self.toast = None;
        }

        // 展開的曲目抓回來了
        let tr = self.player.shared.tracks_rev.load(std::sync::atomic::Ordering::Relaxed);
        if tr != self.tracks_seen {
            self.tracks_seen = tr;
            let got = self.player.shared.tracks.lock().ok().and_then(|mut g| g.take());
            if let Some(t) = got {
                self.on_tracks(t);
            }
        }
        // 展開中換歌：跟著跳到那一首（web 版的 `followDrill`）。搜尋中不跳，免得打斷
        let cur_gen = self.player.shared.generation.load(std::sync::atomic::Ordering::Relaxed);
        if cur_gen != self.follow_gen && self.drill.is_some() && self.search.is_none() {
            // 曲目 URI 是換歌後另外問的，問到之前歌名比對可能落空，那就下一幀再試
            if let Some(i) = self.drill_playing_index() {
                self.follow_gen = cur_gen;
                if i != self.centre_index() {
                    self.lyrics_wait = None;
                    self.target_centre = i as f32;
                }
            }
        }
        self.flush_volume(false);
        let tools = self.tools();

        // 替身要用的中央那張。`album_at` 借整個 self，要在借走 self.r 之前拿
        let centre_album = self.album_at(self.centre_index()).cloned();
        self.presented = false;
        if let Some(w) = &self.window {
            gfx::set_scale(main_scale(w));
        }
        // 大面板蓋到控制列就讓控制列消散（同樣要在借走 self.r 之前算）
        let covered = self.r.as_ref().is_some_and(|r| {
            let (fw, fh) = gfx::logical(r.target.width, r.target.height);
            self.panel_over_bar(fw, fh)
        });
        self.bar_k.set(if covered { 0.0 } else { 1.0 });
        let bar_k = self.bar_k.value();
        let Some(r) = self.r.as_mut() else { return };
        // 放在開頭而不是結尾：中途 return 的路徑也清得到
        r.text.end_frame();
        r.glass.end_frame();
        r.glass.set_pointer(self.cursor_in.then_some(self.cursor), self.mouse_down);
        r.glass.set_focus(self.focused);
        r.art.pump(&r.gpu);

        // 現在播的那張封面。SMTC 給的是一團 bytes 不是網址，要自己上傳一次。
        // **放在這裡而不是歌詞頁裡面** —— 封面流的幽靈也要用同一張，
        // 只在歌詞頁上傳的話，沒切過去就永遠是空白。
        if let Some(n) = &now
            && matches!(n.art, player::Art::SmtcThumb) && !r.art.has(&n.id) {
                let bytes = self.player.shared.thumb.lock().ok().and_then(|g| g.clone());
                if let Some(b) = bytes {
                    r.art.put_bytes(&r.gpu, &n.id, &b);
                }
            }

        // 強調色：換歌才解一次封面小圖。封面還沒下載到就先維持原本的顏色
        let key = match now.as_ref().filter(|n| !n.id.is_empty() && self.cfg.accent_follow) {
            Some(n) => match &n.art {
                player::Art::Url(u) => u.clone(),
                player::Art::SmtcThumb => n.id.clone(),
            },
            None => String::new(),
        };
        if key.is_empty() {
            if !self.accent.key.is_empty() || self.accent.to.is_some() {
                self.accent.aim("", None);
            }
        } else if self.accent.want(&key) {
            let bytes = match now.as_ref().map(|n| &n.art) {
                Some(player::Art::SmtcThumb) => self.player.shared.thumb.lock().ok().and_then(|g| g.clone()),
                _ => r.art.disk_bytes(&key),
            };
            // 還沒下載到就下一秒再試；下載到了卻解不開（例如 WebP）就當灰階，退回綠
            if let Some(b) = bytes {
                self.accent.aim(&key, mini::accent_from_bytes(&b).flatten());
            }
        }
        theme::set_accent(self.accent.current());

        // 播放位置。輪詢是低頻的，中間靠本機時鐘內插補回來。
        // 拖進度條的時候顯示拖到的位置
        let pos_now = now.as_ref().map_or(0, |n| match self.seek_drag {
            Some(t) if n.duration_ms > 0 => (t as f64 * n.duration_ms as f64) as i64,
            _ => n.progress_ms + if n.playing { n.at.elapsed().as_millis() as i64 } else { 0 },
        });
        // 文字 PV 與一般歌詞頁之間：切歌時不知道下一首有沒有時間軸，一律先退回一般歌詞頁，
        // 至少待 `HOLD_S`，歌詞到了、有逐字才再漫回 PV（只有逐句的不演）。進出都補間，不硬切
        {
            let id = now.as_ref().map_or("", |n| n.id.as_str());
            if id != self.pv_track.0 {
                self.pv_track = (id.to_string(), Instant::now());
                self.stir = Instant::now();
            }
            let guard = self.player.shared.lyrics.lock().ok();
            let synced = guard.as_ref().and_then(|g| g.as_ref()).filter(|l| l.has_words());
            let held = self.pv_track.1.elapsed().as_secs_f32() >= pv::HOLD_S;
            self.pv_want = self.cfg.lyrics_pv && held && synced.is_some();
            if self.cfg.lyrics_pv && (synced.is_some() || self.pv_mix.value() > 0.0) {
                self.pv_ear = Some(Instant::now());
            }
            if self.pv_want
                && let Some(ly) = synced {
                    // 演的那份另外存一份：切歌那一刻共用的歌詞就換成「載入中」了，退場還要接著演
                    let key = format!("{id}|{}|{}|{}", ly.source, ly.provider, ly.synced.len());
                    let ms = pos_now + lyrics::shift(&self.cfg, Some(ly));
                    let playing = now.as_ref().is_some_and(|n| n.playing);
                    match self.pv_snap.as_mut() {
                        Some(s) if s.key == key => (s.ms, s.at, s.playing) = (ms, Instant::now(), playing),
                        _ => self.pv_snap = Some(PvSnap { key, track: id.to_string(), lyrics: ly.clone(), ms, at: Instant::now(), playing }),
                    }
                }
            drop(guard);
            let to = if self.pv_want { 1.0 } else { 0.0 };
            // 不在歌詞頁就直接到位，切回歌詞頁時交給頁面轉場
            if self.view == View::Now {
                self.pv_mix.set(to);
            } else {
                self.pv_mix.jump(to);
            }
            if self.pv_mix.value() <= 0.0 && !self.pv_want {
                self.pv_snap = None;
            }
        }
        let roll = {
            let key = format!("{:?}|{}", self.view, now.as_ref().map_or("", |n| n.id.as_str()));
            if key != self.roll_at.0 {
                self.roll_at = (key, Instant::now());
            }
            Some(self.roll_at.1.elapsed().as_secs_f32())
        };
        // 唱盤只在歌詞頁推進。一進頁面就從休息位重來（Python 版的 VINYL.enter）
        if self.view == View::Now && self.cfg.vinyl {
            if self.vinyl_view != View::Now {
                self.vinyl.enter();
            }
            let (has, playing, frac) = match &now {
                Some(n) if !n.id.is_empty() => {
                    let f = if n.duration_ms > 0 { pos_now as f32 / n.duration_ms as f32 } else { 0.0 };
                    (true, n.playing, f)
                }
                _ => (false, false, 0.0),
            };
            // 替身飛進來的時候唱片定住，落地才開始動，角度才接得上（VINYL.freeze）
            let frozen = self.trans.as_ref().is_some_and(|t| t.shared && t.to == View::Now && t.t() < LAND_S);
            if !frozen {
                self.vinyl.step(self.dt as f32, has, playing, frac, self.cfg.vinyl_rpm as f32);
            }
        }
        self.vinyl_view = self.view;
        // 轉場中要畫兩個視圖：底層畫在 target，上層（淡入或淡出的那個）畫在 layer 再疊上去。
        // 封面流 ⇄ 歌詞頁時封面流一定在上層，歌詞頁墊底（Python 版也是封面流在淡）
        let layers: Vec<(View, bool)> = match &self.trans {
            None => vec![(self.view, false)],
            Some(tr) if tr.shared || tr.tucks() => vec![(View::Browse, true), (View::Now, false)],
            Some(tr) => vec![(tr.from, true), (tr.to, false)],
        };
        let (tw, th) = (r.target.width, r.target.height);
        if self.trans.is_some() {
            if r.layer.as_ref().is_none_or(|l| l.width != tw || l.height != th) {
                r.layer = Some(gfx::Offscreen::new(&r.gpu.device, tw, th));
            }
        } else {
            r.layer = None;
        }
        // 歌詞頁那一端的落點。PV 模式控制項收起來時唱片縮在左上角，要照歌詞頁這一幀實際畫的位置算，
        // 不然替身會從（往）一般歌詞頁的大唱片位置飛，看起來像瞬移
        let now_end = self.trans.as_ref().filter(|t| t.shared).map(|_| {
            let (fw, fh) = gfx::logical(tw, th);
            let nl = ui::now(fw, fh);
            let mix = self.pv_mix.value();
            let pvc = self
                .pv_snap
                .as_ref()
                .filter(|s| mix > 0.0 && !s.lyrics.synced.is_empty())
                .map(|_| {
                    let ch = self.trans.as_ref().filter(|t| t.from == View::Now).map_or(self.chrome, |t| t.chrome0);
                    scene_now::pv_chrome(mix, ch)
                });
            let art = scene_now::art_box(&nl, fh, pvc);
            let s = art.w;
            // 唱片模式落在中央那圈標籤（唱片直徑 86% 的 62%），不然就是整張方形封面
            if self.cfg.vinyl {
                let label = ui::Rect::new(art.x + 0.1634 * s, art.y + 0.3034 * s, 0.5332 * s, 0.5332 * s);
                (label, label.w * 0.5)
            } else {
                (art, theme::radius::CARD * (s / nl.art.w))
            }
        });
        // 替身的起訖，第一幀算一次
        if let Some(tr) = self.trans.as_mut().filter(|t| t.shared && t.fly.is_none())
            && let Some((label, label_r)) = now_end
        {
            let (fw, fh) = gfx::logical(tw, th);
            let fl = ui::flow(fw, fh, self.cfg.cover_scale);
            let cp = fl.cover_px;
            let cover = ui::Rect::new(fl.cx - cp * 0.5, fl.cy - cp * 0.5, cp, cp);
            let vinyl = self.cfg.vinyl;
            let ang = if vinyl { self.vinyl.angle } else { 0.0 };
            let centre_tex = match &centre_album {
                Some(a) if a.art.is_empty() && a.art_sm.is_empty() => r.art.get_cached(&a.id),
                Some(a) => r.art.get(if a.art_sm.is_empty() { &a.art } else { &a.art_sm }),
                None => r.art.placeholder.clone(),
            };
            let now_tex = match &now {
                Some(n) => match &n.art {
                    player::Art::Url(u) => r.art.get(u),
                    player::Art::SmtcThumb => r.art.get_cached(&n.id),
                },
                None => r.art.placeholder.clone(),
            };
            let same = Arc::ptr_eq(&centre_tex, &now_tex);
            tr.fly = Some(if tr.to == View::Now {
                Fly { from: cover, to: label, r0: 6.0, r1: label_r, rot0: 0.0, rot1: ang, img: centre_tex, same }
            } else {
                // 回程從目前角度轉回最近的整圈，不要倒轉大半圈
                let back = if ang > 180.0 { 360.0 } else { 0.0 };
                Fly { from: label, to: cover, r0: label_r, r1: 6.0, rot0: ang, rot1: back, img: now_tex, same }
            });
        }
        // 往歌詞頁飛的落點每幀跟著重算：PV 的唱片會隨控制列收放，在左上角與原位之間移動。
        // 回封面流的起點就留在第一幀，那是使用者按下去時看到的位置
        if let Some(f) = self.trans.as_mut().filter(|t| t.to == View::Now).and_then(|t| t.fly.as_mut())
            && let Some((label, label_r)) = now_end
        {
            (f.to, f.r1) = (label, label_r);
        }
        let mut res: anyhow::Result<()> = Ok(());
        for (layer_view, on_layer) in layers {
        let tgt: &gfx::Offscreen = if on_layer { r.layer.as_ref().unwrap() } else { &r.target };
        res = match layer_view {
            View::Browse => {
                let ghost = if self.filtered.is_none() && self.drill.is_none() {
                    self.ghost.as_ref().map(|g| (g, self.ghost_at))
                } else {
                    None
                };
                let base = match &self.drill {
                    Some(d) => &d.tracks[..],
                    None => &self.library[..],
                };
                let list = self.filtered.as_deref().unwrap_or(base);
                let (fw, _) = gfx::logical(r.target.width, r.target.height);
                let back = self.drill.as_ref().map(|d| {
                    let name = r.text.fit_weight(&d.src.name, 13.5, 18.0, ui::drill_back_text_max(fw), 600);
                    let (w, _) = r.text.measure_weight(&name, 13.5, 18.0, None, 600);
                    (name, w)
                });
                if let Some((_, w)) = &back {
                    self.back_w = *w;
                }
                let albums = Self::build_albums(&mut r.art, list, &self.fake,
                                                self.library.is_empty(), ghost, self.ghost_anim,
                                                self.ghost_pulse.and_then(pulse_fx));
                let bar_art = now.as_ref().map(|n| match &n.art {
                    player::Art::Url(u) => r.art.get(u),
                    player::Art::SmtcThumb => r.art.get_cached(&n.id),
                });
                // 背景換圖的交叉淡化、展開的縮放、替身與控制列的收放
                let centre_art = if albums.is_empty() {
                    None
                } else {
                    albums.get((self.centre.round().max(0.0) as usize).min(albums.len() - 1)).map(|a| a.art.clone())
                };
                let bg_prev = self.bg_flow.track(centre_art.as_ref());
                let (sw_scale, sw_alpha) = self.swap.as_ref().map_or((1.0, 1.0), |s| s.fx());
                let tr = self.trans.as_ref();
                let flow_fx = scene_flow::FlowFx {
                    bg_prev: bg_prev.as_ref().map(|(a, w)| (a, *w)),
                    scale: sw_scale,
                    alpha: sw_alpha,
                    // 去歌詞頁：中央那張整段都交給替身；回封面流：落地才接手
                    hide_centre: tr.is_some_and(|t| t.shared && (t.to == View::Now || t.t() < LAND_S)),
                    // 回封面流：替身落地那一刻倒影才出現，跟替身的影子一起交接 140ms
                    centre_refl: tr.filter(|t| t.shared && t.to == View::Browse).map_or(1.0, |t| {
                        anim::EASE.at(anim::phase(t.t(), LAND_S, 0.14))
                    }),
                    // 控制列：整條（含底下 14px 的留白）再多 4px 收出畫面，.34s
                    bar_dy: tr.filter(|t| t.tucks()).map_or(0.0, |t| {
                        let p = anim::STD.at(anim::phase(t.t(), 0.0, 0.34));
                        82.0 * if t.to == View::Now { p } else { 1.0 - p }
                    }),
                    bar_k,
                };
                // KTV 提詞（封面流頂端）。Python 版的 ktvTick
                let ly_now = self.player.shared.lyrics.lock().ok().and_then(|g| g.clone());
                let ktv_lines = if self.cfg.ktv && self.search.is_none() {
                    match (ly_now.as_ref(), now.as_ref()) {
                        (Some(l), Some(n)) if !l.synced.is_empty() && !n.id.is_empty() => {
                            let pos = pos_now + lyrics::shift(&self.cfg, Some(l));
                            let i = scene_now::current_line(l, pos).map_or(-1, |i| i as i64);
                            if i != self.ktv_idx {
                                self.ktv_idx = i;
                                self.ktv_at = Instant::now();
                            }
                            Some(ktv_view(l, i, pos, n.duration_ms))
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                let bar = scene_flow::NowBar {
                    ktv: ktv_lines.as_ref().map(|(c, nx, w)| scene_flow::Ktv {
                        cur: c,
                        next: nx,
                        wipe: *w,
                        age: self.ktv_at.elapsed().as_secs_f32(),
                    }),
                    fx: flow_fx,
                    title: now.as_ref().map_or("", |n| n.title.as_str()),
                    artist: now.as_ref().map_or("", |n| n.artist.as_str()),
                    art: bar_art,
                    pos_ms: pos_now,
                    dur_ms: now.as_ref().map_or(0, |n| n.duration_ms),
                    playing: now.as_ref().is_some_and(|n| n.playing),
                    shuffle: now.as_ref().is_some_and(|n| n.shuffle),
                    repeat: now.as_ref().map_or(Default::default(), |n| n.repeat),
                    tools,
                    vol_drag: self.vol_drag,
                    back: back.as_ref().map(|(n, w)| (n.as_str(), *w)),
                    roll,
                };
                scene_flow::render(
                    &r.gpu, tgt, &mut r.quads, &mut r.covers, &mut r.glass, &mut r.text,
                    &albums, self.centre, &bar, self.cfg.cover_scale, &r.veil, self.hover,
                )
            }
            View::Now => {
                let ly = self.player.shared.lyrics.lock().ok().and_then(|g| g.clone());
                let (title, artist, album, pos, dur, playing) = match &now {
                    Some(n) => (
                        n.title.clone(),
                        n.artist.clone(),
                        n.album.clone(),
                        pos_now,
                        n.duration_ms,
                        n.playing,
                    ),
                    None => ("沒有播放中".into(), String::new(), String::new(), 0, 0, false),
                };
                // 縮圖的上傳統一在上面做過了，這裡只負責拿
                let art = match &now {
                    Some(n) => match &n.art {
                        player::Art::Url(u) => r.art.get(u),
                        player::Art::SmtcThumb => r.art.get_cached(&n.id),
                    },
                    None => r.art.placeholder.clone(),
                };
                // 歌詞捲動：換句時從上一句滑過來。換歌或歌詞剛到貨就直接跳，不要從頭捲下來
                if let Some(l) = ly.as_ref() {
                    let key = format!("{}|{}|{}", now.as_ref().map_or("", |n| n.id.as_str()), l.source, l.synced.len());
                    let cur = scene_now::current_line(l, pos + lyrics::shift(&self.cfg, Some(l))).map_or(-1.0, |i| i as f32);
                    if key != self.lyric_for {
                        self.lyric_for = key;
                        self.lyric_scroll.jump(cur);
                        self.lyric_focus.jump(cur);
                        self.lyric_manual = None;
                        self.lyric_browse.jump(0.0);
                        self.lyric_flash = None;
                        self.plain_target = 0.0;
                        self.plain_scroll.jump(0.0);
                    } else {
                        // 捲走之後放著不動一陣子，就捲回正在唱的那句
                        if self.lyric_manual.is_some_and(|(_, t)| t.elapsed() > LYRIC_HOLD) {
                            self.lyric_manual = None;
                        }
                        self.lyric_focus.set(cur);
                        match self.lyric_manual {
                            // 滾輪：短一點、一開始就動，連續轉才跟得上手
                            Some((at, _)) => self.lyric_scroll.set_eased(at, 0.3, anim::OUT),
                            // 跟著唱是 .45s；一次跳很多句（點遠處那句、捲回來）拉長一點，不然只看到一閃
                            None => {
                                let far = (cur - self.lyric_scroll.value()).abs();
                                let dur = if far > 1.5 { (0.45 + 0.03 * far).min(0.9) } else { 0.45 };
                                self.lyric_scroll.set_eased(cur, dur, anim::SOFT);
                            }
                        }
                        self.lyric_browse.set(if self.lyric_manual.is_some() { 1.0 } else { 0.0 });
                    }
                }
                // 滑鼠停在哪句：每幀重算，因為歌詞自己會捲，滑鼠不動底下的句子也會換
                // （疊層開著就不算；`r` 借走了 self.r，這裡只能直接讀欄位）
                let covered = self.picker.is_some() || self.settings_open || self.help_open
                    || self.about_open || self.devices_open || self.setup.is_some();
                let over = (self.cursor_in && !covered)
                    .then(|| self.lyric_hits.at(self.cursor.0, self.cursor.1))
                    .flatten();
                if over != self.lyric_hover.0 {
                    self.lyric_hover = (over, self.lyric_hover.0, Instant::now());
                }
                if over.is_some()
                    && let Some((_, t)) = self.lyric_manual.as_mut() {
                        // 還指著歌詞就是還在挑，不要捲回去
                        *t = Instant::now();
                    }
                let pick = {
                    let p = (self.lyric_hover.2.elapsed().as_secs_f32() / scene_now::HOVER_S).min(1.0);
                    scene_now::LyricPick {
                        focus: self.lyric_focus.value(),
                        browse: self.lyric_browse.value(),
                        hover: self.lyric_hover.0.map(|i| (i, p)),
                        unhover: self.lyric_hover.1.map(|i| (i, 1.0 - p)).filter(|(_, a)| *a > 0.0),
                        flash: self.lyric_flash
                            .map(|(i, t)| (i, t.elapsed().as_secs_f32()))
                            .filter(|(_, t)| *t < scene_now::FLASH_S),
                        plain_y: self.plain_scroll.value(),
                    }
                };
                let bg_prev = self.bg_now.track(Some(&art));
                let nfx = match &self.trans {
                    Some(tr) => scene_now::NowFx {
                        t: tr.t(),
                        enter: tr.to == View::Now,
                        leave: tr.from == View::Now && tr.shared,
                        // 封面交給替身：去程落地前、回程整段都不畫
                        hide_art: tr.shared && (tr.from == View::Now || tr.t() < LAND_S),
                    },
                    None => Default::default(),
                };
                let nn = scene_now::Now {
                    bg_prev: bg_prev.as_ref().map(|(a, w)| (a, *w)),
                    lyric_scroll: self.lyric_scroll.value(),
                    pick,
                    roll,
                    fx: nfx,
                    title: &title,
                    artist: &artist,
                    album: &album,
                    art,
                    position_ms: pos,
                    // 時間補償只加在比對歌詞用的位置上 ——
                    // 進度條與時間顯示的仍然是真的播放位置。
                    lyric_ms: pos + lyrics::shift(&self.cfg, ly.as_ref()),
                    duration_ms: dur,
                    lyrics: ly.as_ref(),
                    playing,
                    shuffle: now.as_ref().is_some_and(|n| n.shuffle),
                    repeat: now.as_ref().map_or(Default::default(), |n| n.repeat),
                    // 歌詞頁的逐字掃光一律開著（Python 版也是）；`ktv` 設定管的是封面流頂端的提詞
                    ktv: true,
                    show_trans: self.cfg.lyrics_trans,
                    tools,
                    vol_drag: self.vol_drag,
                    vinyl: self.cfg.vinyl.then_some(&self.vinyl),
                };
                let mix = self.pv_mix.value();
                let pv_in = if let Some(s) = self.pv_snap.as_ref().filter(|_| mix > 0.0) {
                    let pv = r.pv.get_or_insert_with(|| pv::Pv::new(&r.gpu, gfx::OFFSCREEN_FORMAT));
                    Some(scene_now::PvIn {
                        pv,
                        lyrics: &s.lyrics,
                        ms: s.ms_now(),
                        mix,
                        track: &s.track,
                        pulse: self.spec.lock().map(|s| s.pulse).unwrap_or_default(),
                        now: Instant::now(),
                        chrome: self.trans.as_ref().filter(|t| t.from == View::Now).map_or(self.chrome, |t| t.chrome0),
                        look: self.pv_look,
                    })
                } else {
                    None
                };
                scene_now::render_with(
                    &r.gpu, tgt, &mut r.quads, &mut r.glass, &mut r.text, &nn, self.chrome,
                    self.hover, pv_in,
                )
                .map(|h| self.lyric_hits = h)
            }
            View::Viz => {
                let track = match &now {
                    Some(n) => format!("{} — {}", n.artist, n.title),
                    None => "沒有播放中".to_string(),
                };
                scene_viz::render(
                    &r.gpu, tgt, &mut r.quads, &mut r.glass, &mut r.text, &mut self.viz,
                    &self.spec, &track, self.chrome, bar_k, self.hover, self.dt,
                )
            }
        };
        if res.is_err() {
            break;
        }
        }
        if let Err(e) = res {
            crate::warn!("算圖失敗：{e}");
            return;
        }
        // 合成：上層照轉場進度淡入淡出，替身最後畫
        if let (Some(tr), Some(layer)) = (&self.trans, &r.layer) {
            use anim::{phase, EASE, VIEW as EV};
            let t = tr.t();
            let top_a = if tr.shared || tr.tucks() {
                if tr.to == View::Now {
                    1.0 - EASE.at(phase(t, 0.0, 0.34 * 0.62))
                } else {
                    EASE.at(phase(t, 0.1, 0.34 * 0.62))
                }
            } else {
                1.0 - EASE.at(phase(t, 0.0, 0.34 * 0.7))
            };
            let (fw, fh) = gfx::logical(tw, th);
            let layer_tex = Arc::new(layer.texture.create_view(&Default::default()));
            r.quads.begin(&r.gpu);
            if top_a > 0.0 {
                r.quads.push_tex(quad::Quad::new(0.0, 0.0, fw, fh).textured().color(theme::tex(top_a)), &layer_tex);
            }
            if let Some(f) = &tr.fly {
                let k = EV.at(phase(t, 0.0, LAND_S));
                // 落地之後：同一張就收掉；不同張淡出 140ms，底下真的那張已經接手。
                // 回封面流一律淡出：替身的影子比封面流的重，一幀消失看起來就是閃一下
                let fade = !f.same || tr.to == View::Browse;
                let a = if t < LAND_S { 1.0 } else if !fade { 0.0 } else { 1.0 - EASE.at(phase(t, LAND_S, 0.14)) };
                if a > 0.0 {
                    let lerp = anim::lerp;
                    let (x, y) = (lerp(f.from.x, f.to.x, k), lerp(f.from.y, f.to.y, k));
                    let sz = lerp(f.from.w, f.to.w, k);
                    r.quads.push_tex(
                        quad::Quad::new(x, y, sz, sz)
                            .radius(lerp(f.r0, f.r1, k))
                            .cover(1.0, 1.0)
                            .rotate(lerp(f.rot0, f.rot1, k).to_radians())
                            .shadow_of(&theme::SHADOW_FLYER, a)
                            .color(theme::tex(a)),
                        &f.img,
                    );
                }
            }
            let mut enc = r.gpu.device.create_command_encoder(&Default::default());
            {
                let mut pass = gfx::pass(&mut enc, &r.target.view, "trans", None);
                r.quads.flush(&r.gpu, &mut pass, tw, th);
            }
            r.quads.upload(&r.gpu, tw, th);
            r.gpu.queue.submit([enc.finish()]);
        }

        // 設定面板疊在最上面。底下那頁照樣每幀重畫，所以面板也要每幀重疊一次。
        if self.settings_open {
            let logged_in =
                self.player.shared.authed.load(std::sync::atomic::Ordering::Relaxed);
            let dir = self.paths.data.display().to_string();
            let mut msg =
                self.player.shared.login_msg.lock().map(|m| m.clone()).unwrap_or_default();
            if msg.is_empty()
                && logged_in
                && self.player.shared.need_reauth.load(std::sync::atomic::Ordering::Relaxed)
            {
                msg = "需要重新授權（按一下）".into();
            }
            if let Err(e) = scene_settings::render(
                &r.gpu, &r.target, &mut r.quads, &mut r.glass, &mut r.text, &self.cfg, logged_in,
                &dir, &msg, &self.updater.label(), self.settings_sel, self.hover,
            ) {
                crate::warn!("設定面板畫不出來：{e}");
            }
        }
        // 打開時慢一點、有回彈，收回去快、不要回彈
        let (to, dur, ease) = if self.devices_open {
            (1.0, theme::dur::MORPH, anim::GROW)
        } else {
            (0.0, theme::dur::MORPH_BACK, anim::STD)
        };
        self.devices_t.set_eased(to, dur, ease);
        let morph = self.devices_t.value();
        if self.devices_open || morph > 0.0 {
            let list = self.player.shared.devices.lock().ok().and_then(|g| g.clone());
            // 封面流那頁從控制列的裝置鍵長出來；其他頁的控制列不一定在，就從中央長出來
            let from = (self.view == View::Browse).then(|| {
                let (fw, fh) = gfx::logical(r.target.width, r.target.height);
                ui::flow(fw, fh, self.cfg.cover_scale).devices
            });
            if let Err(e) = scene_devices::render(
                &r.gpu, &r.target, &mut r.quads, &mut r.glass, &mut r.text, list.as_ref(),
                self.devices_sel, self.hover, morph, from,
            ) {
                crate::warn!("裝置選單畫不出來：{e}");
            }
        }
        if self.about_open {
            let dir = self.paths.data.display().to_string();
            if let Err(e) = scene_about::render(
                &r.gpu, &r.target, &mut r.quads, &mut r.glass, &mut r.text, self.hover, &dir,
                self.about_confirm,
            ) {
                crate::warn!("關於畫不出來：{e}");
            }
        }
        if self.help_open {
            let viz_on = self.cfg.viz_enabled;
            if let Err(e) =
                scene_help::render(&r.gpu, &r.target, &mut r.quads, &mut r.glass, &mut r.text, viz_on)
            {
                crate::warn!("說明畫不出來：{e}");
            }
        }

        // 游標：打字後的 0.6 秒內一直亮，之後每 0.53 秒切一次（Windows 的預設）
        let ct = self.caret_t.elapsed().as_secs_f32();
        let caret_on = ct < 0.6 || ((ct - 0.6) / 0.53) as u32 % 2 == 1;
        let mut ime_at: Option<(f32, f32)> = None;
        if let (Some(input), View::Browse) = (&self.search, self.view) {
            let no_match = self.filtered.as_ref().is_some_and(|f| f.is_empty());
            match scene_search::render_search(
                &r.gpu, &r.target, &mut r.quads, &mut r.glass, &mut r.text, input, no_match,
                self.cfg.cover_scale, caret_on, self.search_at.elapsed().as_secs_f32(),
            ) {
                Ok(at) => ime_at = Some(at),
                Err(e) => crate::warn!("搜尋框畫不出來：{e}"),
            }
        }
        if let Some(p) = self.picker.as_mut() {
            let v = scene_search::PickerView {
                rows: &p.rows,
                at: p.at,
                top: p.top,
                input: &p.input,
                caret_on,
            };
            match scene_search::render_picker(
                &r.gpu, &r.target, &mut r.quads, &mut r.glass, &mut r.text, &self.viz, &v,
                &mut p.fit, self.hover,
            ) {
                Ok(at) => ime_at = Some(at),
                Err(e) => crate::warn!("preset 選單畫不出來：{e}"),
            }
        }
        if let Some(st) = &self.setup
            && let Err(e) = scene_setup::render(
                &r.gpu, &r.target, &mut r.quads, &mut r.glass, &mut r.text, st,
                &self.cfg.redirect_uri(), caret_on, self.hover,
            ) {
                crate::warn!("設定精靈畫不出來：{e}");
            }
        // 通知列在所有疊層之上。位置依視圖讓開控制列；
        // 視覺效果頁的控制列會收起來，但通知列還是停在同一個高度，不跟著跳
        if let Some((msg, t)) = &self.toast {
            let (fw, fh) = gfx::logical(r.target.width, r.target.height);
            let bottom = match self.view {
                View::Browse => ui::flow(fw, fh, self.cfg.cover_scale).bar.y - 20.0,
                View::Now => fh - 34.0,
                View::Viz => ui::viz(fw, fh).bar.y - 16.0,
            };
            // 視覺效果頁用 clear 玻璃，通知列跟著用（兩種變體不混在同一頁）
            if let Err(e) = scene_toast::render(
                &r.gpu, &r.target, &mut r.quads, &mut r.glass, &mut r.text, msg, t.elapsed().as_secs_f32(),
                bottom, self.view == View::Viz,
            ) {
                crate::warn!("通知列畫不出來：{e}");
            }
        }

        // 輸入法的候選窗要跟著游標，不然它會跳在視窗左上角
        if let (Some((x, y)), Some(w)) = (ime_at, &self.window) {
            w.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(x as f64, y as f64 - 12.0),
                winit::dpi::LogicalSize::new(2.0, 24.0),
            );
        }

        if let (Some(dir), Some(tr)) = (std::env::var_os("VITRO_TRANS_DUMP"), &self.trans) {
            let tag = if tr.to == View::Now { "go" } else { "back" };
            let f = std::path::Path::new(&dir).join(format!("{tag}_{:04}.png", (tr.t() * 1000.0) as u32));
            if let Err(e) = r.target.save_png(&r.gpu, &f) { config::diag(&format!("dump {}: {e}", f.display())); }
        }

        // 離屏貼到畫面上
        use wgpu::CurrentSurfaceTexture as Cst;
        let t_wait = Instant::now();
        let acquired = r.surface.get_current_texture();
        self.prof_wait += t_wait.elapsed();
        let frame = match acquired {
            Cst::Success(f) | Cst::Suboptimal(f) => f,
            Cst::Timeout | Cst::Occluded | Cst::Validation => return,
            Cst::Outdated | Cst::Lost => {
                r.surface.configure(&r.gpu.device, &r.surf_config);
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = r
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("present") });
        {
            let mut pass = gfx::pass(&mut enc, &view, "blit", Some(wgpu::Color::BLACK));
            r.blit.draw(&r.gpu, &mut pass, &r.target.view);
        }
        r.gpu.queue.submit([enc.finish()]);
        r.gpu.queue.present(frame);
        self.presented = true;
    }

    fn resize(&mut self, w: u32, h: u32) {
        let Some(r) = self.r.as_mut() else { return };
        if w == 0 || h == 0 {
            return;
        }
        r.surf_config.width = w;
        r.surf_config.height = h;
        r.surface.configure(&r.gpu.device, &r.surf_config);
        r.target = gfx::Offscreen::new(&r.gpu.device, w, h);
        r.layer = None;
    }
}

impl ApplicationHandler<UserEv> for App {
    fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEv) {
        match ev {
            UserEv::Activate | UserEv::Tray(tray::Cmd::Open) => self.bring_front(el),
            UserEv::Tray(tray::Cmd::Quit) | UserEv::UpdateReady => self.quit(el),
            // 只改設定，真正開關視窗在 about_to_wait 的 sync_mini
            UserEv::Tray(tray::Cmd::Mini) => {
                self.cfg.mini_enabled = !self.cfg.mini_enabled;
                self.save_cfg();
            }
            UserEv::Tray(tray::Cmd::MiniLock) => {
                // 迷你視窗沒開就先開起來，不然鎖了也看不到
                self.cfg.mini_locked = !(self.cfg.mini_locked && self.mini.is_some());
                self.cfg.mini_enabled = true;
                self.save_cfg();
            }
            UserEv::Tray(tray::Cmd::MiniRecall) => self.recall_mini(),
        }
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.r.is_some() {
            return;
        }
        // 指定螢幕。`screen` 是索引，對應 `available_monitors()` 的順序 ——
        // 跟 Python 版一樣，超出範圍就當成沒設定。
        let monitor = self
            .cfg
            .screen
            .and_then(|i| el.available_monitors().nth(i as usize));
        if let Some(m) = &monitor {
            println!(
                "開在螢幕 #{}：{}",
                self.cfg.screen.unwrap_or(0),
                m.name().unwrap_or_else(|| "（無名）".into())
            );
        }
        // 全螢幕要在建視窗時就給，開起來再切會先閃一下視窗框
        let mut attrs = Window::default_attributes()
            .with_title("VitroDeck")
            .with_window_icon(window_icon())
            .with_theme(theme::Appearance::parse(&self.cfg.appearance).title_bar())
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 800))
            .with_fullscreen(
                self.cfg
                    .start_fullscreen
                    .then(|| Fullscreen::Borderless(monitor.clone())),
            );
        // 沒有全螢幕的話，把視窗擺到那個螢幕的中間
        if let (Some(m), false) = (&monitor, self.cfg.start_fullscreen) {
            let pos = m.position();
            let size = m.size();
            // 視窗是 1280x800 邏輯像素，在這個螢幕上的實體大小要乘它的縮放
            let k = m.scale_factor();
            let (ww, wh) = ((1280.0 * k).round() as i32, (800.0 * k).round() as i32);
            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(
                pos.x + (size.width as i32 - ww) / 2,
                pos.y + (size.height as i32 - wh) / 2,
            ));
        }
        self.screen_mode =
            if self.cfg.start_fullscreen { ScreenMode::One } else { ScreenMode::Windowed };
        let window = Arc::new(el.create_window(attrs).expect("開不了視窗"));
        gfx::set_scale(main_scale(&window));
        sync_min_size(&window, self.screen_mode);
        // `--span`：開起來就跨所有螢幕。多螢幕的東西沒辦法用 `--shot` 驗，
        // 只能真的開出來量視窗矩形，所以要有一個不靠按鍵的入口。
        if std::env::args().any(|a| a == "--span") {
            self.window = Some(window.clone());
            self.set_screen_mode(ScreenMode::Span);
        }

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window.clone()).expect("建不了 surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .expect("找不到可用的介面卡");
        let info = adapter.get_info();
        println!("GPU: {} ({:?})", info.name, info.backend);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("要不到 device");
        let gpu = gfx::Gpu { device, queue, adapter_name: info.name.clone() };

        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let mut surf_config =
            surface.get_default_config(&adapter, w, h).expect("surface 沒有可用設定");
        surf_config.width = w;
        surf_config.height = h;
        // 交換鏈也要非 sRGB：blit 出去的是**已經編碼好**的 sRGB 數值，
        // 交給 sRGB 的 view 會再編一次，整個畫面會白掉。
        let plain = surf_config.format.remove_srgb_suffix();
        if surface.get_capabilities(&adapter).formats.contains(&plain) {
            surf_config.format = plain;
        }
        surface.configure(&gpu.device, &surf_config);
        println!("交換鏈格式：{:?}", surf_config.format);
        let fmt = gfx::OFFSCREEN_FORMAT;
        let r = Render {
            target: gfx::Offscreen::new(&gpu.device, w, h),
            quads: quad::QuadRenderer::new(&gpu, fmt),
            covers: cover::CoverRenderer::new(&gpu, fmt),
            glass: glass::Glass::new(&gpu, fmt),
            text: text::Text::new(&gpu, fmt),
            blit: gfx::Blit::new(&gpu, surf_config.format),
            art: art::ArtCache::new(&gpu, self.paths.cache.join("art")),
            veil: scene_flow::Veil::new(&gpu),
            surface,
            surf_config,
            gpu,
            layer: None,
            pv: None,
            instance,
            adapter,
        };
        self.fake = scene_flow::fake_albums(&r.gpu, 12);
        if self.library.is_empty() {
            println!("收藏清單是空的（還沒登入 Spotify），先用內建的示範封面");
        } else {
            println!("收藏 {} 張專輯", self.library.len());
        }
        self.r = Some(r);
        self.window = Some(window);
        self.sync_tray();
        // `VITRO_PREFS=1`：開起來就開設定視窗（實機截圖、測操作用）
        if std::env::var_os("VITRO_PREFS").is_some() {
            self.prefs_want = true;
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.sync_mini(el);
        self.tick_mini();
        self.sync_prefs(el);
        self.tick_prefs();
        if let Some(p) = &self.parked
            && !self.hidden()
            && (p.hidden || self.last_input > p.at || self.stir > p.at || self.stir_sig() != p.sig)
        {
            self.unpark(el);
        }
        // 縮在系統列或停畫時迴圈是 Wait；迷你視窗或設定視窗開著的話要定時醒來，進度條與動畫才會走。
        // 停畫的話還要定時看該不該醒
        if self.in_tray || self.parked.is_some() {
            let poll = self.parked.as_ref().map(|_| Instant::now() + PARK_POLL);
            let due = [self.mini.as_ref().map(|m| m.next_due()), self.prefs.as_ref().map(|p| p.next_due()), poll];
            el.set_control_flow(match due.into_iter().flatten().min() {
                Some(t) => ControlFlow::WaitUntil(t),
                None => ControlFlow::Wait,
            });
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
        if self.mini.as_ref().is_some_and(|m| m.id() == id) {
            self.mini_event(el, &ev);
            return;
        }
        if self.prefs.as_ref().is_some_and(|p| p.id() == id) {
            self.prefs_event(el, &ev);
            return;
        }
        if is_input(&ev) {
            self.last_input = Instant::now();
        }
        // 停畫中的話，這些都要醒來重畫一次（醒不醒在 `about_to_wait` 決定）
        if matches!(
            ev,
            WindowEvent::Focused(_) | WindowEvent::Resized(_) | WindowEvent::Occluded(_) | WindowEvent::ScaleFactorChanged { .. }
        ) {
            self.stir = Instant::now();
        }
        // 玻璃要的三個狀態，先記下來再交給底下各自的處理
        match &ev {
            WindowEvent::Focused(f) => {
                self.focused = *f;
                // 按著切走的話放開那下收不到
                if !*f {
                    self.mouse_down = false;
                }
            }
            WindowEvent::CursorMoved { .. } => self.cursor_in = true,
            WindowEvent::CursorLeft { .. } => {
                self.cursor_in = false;
                self.mouse_down = false;
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                self.mouse_down = *state == ElementState::Pressed;
            }
            _ => {}
        }
        match ev {
            // 開了「關閉時縮到系統列」就藏起來。winit 的 CloseRequested 只有使用者
            // 關（關閉鈕、Alt+F4）；登出、關機走的是 WM_ENDSESSION，不會被擋住
            WindowEvent::CloseRequested if self.cfg.close_to_tray && tray::running() => self.hide_to_tray(el),
            WindowEvent::CloseRequested => self.quit(el),
            WindowEvent::Resized(s) => self.resize(s.width, s.height),
            // 拖到另一個縮放不同的螢幕。winit 接著會送一個新大小的 Resized
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => gfx::set_scale(gfx::zoomed(scale_factor)),
            // 視窗被蓋住就告訴 poller，讓它拉長輪詢間隔省 Spotify 的配額。
            // 這是 Python 版就有的行為（前端回報可見性）。
            // 主視窗也跟著停畫（`pace`），迷你視窗照常
            WindowEvent::Occluded(hidden) => {
                self.occluded = hidden;
                let _ = self.player.tx.send(player::Cmd::Visible(!hidden));
            }
            // 使用者在 Windows 設定改了深淺色：跟隨系統的話要換，沒跟的話 winit 可能把
            // 標題列改回系統的，要重套。回到這個視窗時也重做一次，以防訊息沒收到
            WindowEvent::ThemeChanged(_) | WindowEvent::Focused(true) => self.sync_appearance(),
            // 控制列的出現只跟滑鼠移動綁定，鍵盤不影響。
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Instant::now();
                // winit 給實體像素，版面與命中測試是邏輯像素
                let k = gfx::scale();
                self.cursor = (position.x as f32 / k, position.y as f32 / k);
                if let (Some(_), Some(r)) = (self.seek_drag, self.progress_rect()) {
                    self.seek_drag = Some(((self.cursor.0 - r.x) / r.w).clamp(0.0, 1.0));
                }
                if self.vol_drag
                    && let Some(r) = self.volume_rect() {
                        let t = ui::slider_t(&r, self.cursor.0);
                        self.set_volume((t * 100.0).round() as i32, false);
                    }
                let was = self.hover;
                self.hover = self.hit_test();
                // 拖進度條時游標滑出那條也要維持「在進度條上」，圓鈕才不會不見
                if let Some(t) = self.seek_drag {
                    self.hover = Some(ui::Hit::Seek(t));
                }
                // 滑鼠移到音量上才問一次目前的音量（SMTC 不給），而且距上次夠久才問
                let on_vol = |h: Option<ui::Hit>| matches!(h, Some(ui::Hit::Volume(_)) | Some(ui::Hit::Mute));
                if on_vol(self.hover)
                    && !on_vol(was)
                    && self.vol_synced.is_none_or(|t| t.elapsed() > Duration::from_secs(15))
                {
                    self.vol_synced = Some(Instant::now());
                    let _ = self.player.tx.send(player::Cmd::SyncVolume);
                }
                // 停在可以點的東西上就換成手指游標，這是唯一的「這裡能點」提示
                if let Some(win) = &self.window {
                    win.set_cursor(if self.hover.is_some() {
                        winit::window::CursorIcon::Pointer
                    } else {
                        winit::window::CursorIcon::Default
                    });
                }
            }
            WindowEvent::CursorLeft { .. } => self.hover = None,
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. }
                if self.seek_drag.is_some() =>
            {
                if let Some(t) = self.seek_drag.take() {
                    self.seek_to(t);
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. }
                if self.vol_drag =>
            {
                // 放開才確定送出最後那個值
                self.vol_drag = false;
                self.flush_volume(true);
            }
            WindowEvent::MouseInput { state, button, .. }
                if state == ElementState::Pressed && button == MouseButton::Left =>
            {
                self.last_mouse = Instant::now();
                // 按下當下重算一次，不要信任上一次移動留下的值 ——
                // 畫面會自己動（封面捲、控制列淡出），hover 可能已經過期
                if let Some(hit) = self.hit_test() {
                    self.act(hit);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.last_mouse = Instant::now();
                let d = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => -y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => -(p.y as f32) / gfx::scale() / 60.0,
                };
                // 選單開著：捲的是清單，不是換 preset，也不是翻專輯。
                // 捲動不改反白的那列 —— 跟瀏覽器的捲軸一樣，看而已。
                if self.picker.is_some() {
                    let rows = self.picker_rows();
                    if let Some(p) = self.picker.as_mut() {
                        let max = p.rows.len().saturating_sub(rows) as i32;
                        p.top = (p.top as i32 + (d * 3.0).round() as i32).clamp(0, max) as usize;
                    }
                    self.hover = self.hit_test();
                    return;
                }
                // 覆蓋層開著時滾輪不能穿過去捲底下的封面流（Python 版 v1.0.4 修過同一個）。
                // 設定面板例外：停在某一列上就是調那一列，跟左右鍵一樣
                if self.settings_open || self.help_open || self.about_open || self.devices_open || self.setup.is_some() {
                    if let (true, Some(ui::Hit::SettingRow(i))) = (self.settings_open, self.hover) {
                        self.change_setting(i, if d > 0.0 { 1 } else { -1 });
                    }
                    return;
                }
                // 滾輪停在音量上就調音量，不捲封面流
                if matches!(self.hover, Some(ui::Hit::Volume(_)) | Some(ui::Hit::Mute)) {
                    let cur = self.tools().volume.unwrap_or(50) as i32;
                    self.set_volume(cur + if d > 0.0 { -5 } else { 5 }, false);
                    return;
                }
                // 歌詞頁：捲歌詞（以前會穿過去翻看不見的封面流）
                if self.view == View::Now {
                    self.scroll_lyrics(d);
                    return;
                }
                self.step(d.clamp(-3.0, 3.0));
            }
            WindowEvent::Ime(ime) => self.on_ime(ime),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let k = event.logical_key.clone();
                self.on_key(el, &k);
            }
            // 縮在系統列：不畫，也不排下一幀，迴圈就停了。叫回來時 bring_front 再點火
            WindowEvent::RedrawRequested if self.in_tray => {}
            WindowEvent::RedrawRequested => {
                let pace = self.pace();
                if pace == Pace::Park {
                    self.park(el);
                    return;
                }
                // 影格上限。睡到下一格的時間點再畫 —— 空轉等待會把一整顆
                // 核心吃掉，那比不限幀還糟。
                if let Some(cap) = self.frame_gap(pace) {
                    let target = Duration::from_secs_f32(cap);
                    let spent = self.last.elapsed();
                    if spent < target {
                        std::thread::sleep(target - spent);
                    }
                }
                // 啟動到第一幀。要量的是「使用者看到東西」的時間，
                // 不是 main() 開始執行的時間，所以記在這裡。
                if self.frames == 0 && !self.first_frame_reported {
                    self.first_frame_reported = true;
                    println!("第一幀 {:.0} ms", self.boot.elapsed().as_secs_f64() * 1000.0);
                }
                let dt = self.last.elapsed().as_secs_f32().min(0.1);
                self.last = Instant::now();
                self.dt = dt as f64;
                // `VITRO_TRANS_DUMP=<資料夾>`：自己走一趟 封面流→歌詞頁→封面流，
                // 轉場中每一幀存成 PNG（檔名是轉場開始後的毫秒）。查轉場閃爍用
                if std::env::var_os("VITRO_TRANS_DUMP").is_some() {
                    let s = self.boot.elapsed().as_secs_f32();
                    if s > 5.0 && self.view == View::Browse && self.dump_step == 0 {
                        self.dump_step = 1;
                        self.set_view(View::Now);
                    } else if s > 7.0 && self.dump_step == 1 {
                        self.dump_step = 2;
                        self.set_view(View::Browse);
                    } else if s > 9.0 && self.dump_step == 2 {
                        el.exit();
                    }
                }
                let t_a = Instant::now();
                self.animate(dt);
                let t_d = Instant::now();
                self.draw();
                // `VITRO_PROF=1`：慢幀記進 diag.log（animate／draw 各花多久）
                if std::env::var_os("VITRO_PROF").is_some() {
                    let (a, d) = (t_d.duration_since(t_a), t_d.elapsed());
                    self.prof_cpu.0 += a;
                    self.prof_cpu.1 += d;
                    if a + d > Duration::from_millis(25) {
                        config::diag(&format!(
                            "慢幀 {:.1} ms：animate {:.1}、draw {:.1}、視圖 {:?}、轉場 {}",
                            (a + d).as_secs_f64() * 1e3, a.as_secs_f64() * 1e3, d.as_secs_f64() * 1e3,
                            self.view, self.trans.is_some(),
                        ));
                    }
                }
                if !self.presented {
                    std::thread::sleep(Duration::from_millis(16));
                }

                self.frames += 1;
                let el_s = self.fps_at.elapsed().as_secs_f32();
                if el_s >= 3.0 {
                    println!("{:.0} fps   視圖={:?}", self.frames as f32 / el_s, self.view);
                    if std::env::var_os("VITRO_PROF").is_some() {
                        // 每幀平均：閒置時的成本主要看這個，慢幀那行只記超過 25 ms 的
                        let n = self.frames.max(1) as f64;
                        config::diag(&format!(
                            "{:.0} fps 視圖={:?} {:.0}，每幀 animate {:.2} ms、draw {:.2} ms（其中等畫布 {:.2}）",
                            self.frames as f32 / el_s, self.view, el_s,
                            self.prof_cpu.0.as_secs_f64() * 1e3 / n, self.prof_cpu.1.as_secs_f64() * 1e3 / n,
                            self.prof_wait.as_secs_f64() * 1e3 / n,
                        ));
                        self.prof_cpu = (Duration::ZERO, Duration::ZERO);
                        self.prof_wait = Duration::ZERO;
                    }
                    self.frames = 0;
                    self.fps_at = Instant::now();
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}

/// 依設定排收藏清單。
///
/// `added` 是「最近加入的在前」—— Spotify 給的 `added_at` 是 ISO 字串，
/// 字典序剛好就是時間序，所以倒著排就好。其他三個是正序。
/// 名稱用小寫比，不然大小寫混在一起會分成兩堆。
///
/// 播放清單沒有加入時間也沒有年份，這兩種排序下一律排在專輯前面，
/// 彼此之間照 Spotify 給的順序。名稱、演出者（建立者）就跟專輯混排（web 版的 `playlistsFirst`）。
fn sort_library(library: &mut [spotify::Album], how: &str) {
    use std::cmp::Ordering;
    let lists_first = |a: &spotify::Album, b: &spotify::Album, f: &dyn Fn() -> Ordering| {
        match (a.is_playlist(), b.is_playlist()) {
            (true, true) => a.order.cmp(&b.order),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => f(),
        }
    };
    match how {
        "name" => library.sort_by_key(|a| a.name.to_lowercase()),
        "artist" => library.sort_by_key(|a| (a.artist.to_lowercase(), a.name.to_lowercase())),
        "year" => library.sort_by(|a, b| lists_first(a, b, &|| b.year.cmp(&a.year).then(a.name.cmp(&b.name)))),
        _ => library.sort_by(|a, b| lists_first(a, b, &|| b.added.cmp(&a.added))),
    }
}

/// 收藏專輯＋播放清單，排好。兩份檔案分開存是跟 Python 版一樣的格式。
fn load_library(paths: &config::Paths, sort: &str) -> Vec<spotify::Album> {
    let mut list: Vec<spotify::Album> = config::read_json(&paths.albums);
    let lists: Vec<spotify::Album> = config::read_json(&paths.playlists);
    list.extend(lists);
    sort_library(&mut list, sort);
    list
}

fn probe() -> anyhow::Result<()> {
    smtc::init_apartment();
    let mut now: Option<smtc::Track> = None;
    match smtc::Smtc::new("spotify") {
        Ok(s) => {
            println!("SMTC 啟動      OK");
            match s.read() {
                Some(t) => {
                    println!(
                        "SMTC 讀取      OK  {} - {}｜{}  {}  {}/{} ms",
                        t.artist,
                        t.title,
                        t.album,
                        if t.playing { "播放中" } else { "暫停" },
                        t.progress_ms,
                        t.duration_ms
                    );
                    now = Some(t);
                }
                None => println!("SMTC 讀取      沒有播放中的 Spotify session"),
            }
            match s.thumbnail() {
                Some(b) => println!("SMTC 縮圖      OK  {} bytes", b.len()),
                None => println!("SMTC 縮圖      沒有"),
            }
        }
        Err(e) => println!("SMTC 啟動      FAIL  {e}"),
    }

    // 簡繁轉換必須是 1:1 字元對映，逐字歌詞才不會錯位。
    for p in ["方便面 软件", "荡秋千", "简体字测试"] {
        let out = lyrics::to_traditional_probe(p);
        let same = out.chars().count() == p.chars().count();
        println!("簡繁 s2tw      {}  {p} -> {out}", if same { "OK  " } else { "FAIL" });
    }

    let paths = config::Paths::discover();
    let cfg = config::Config::load(&paths.config);
    println!("DATA           {}", paths.data.display());
    let lib: Vec<spotify::Album> = config::read_json(&paths.albums);
    println!("收藏快取       {} 張", lib.len());

    // Spotify 的新端點。**只讀**：不會改播放、不會動收藏
    let mut sp = spotify::Spotify::new(&cfg, paths.tokens.clone());
    if sp.logged_in() {
        let missing = sp.tokens.missing_scopes();
        println!("權杖缺的權限   {}", if missing.is_empty() { "沒有".to_string() } else { missing.join(" ") });
        if let Some(a) = lib.iter().find(|a| a.can_drill()) {
            let id = a.uri.rsplit(':').next().unwrap_or("");
            match sp.fetch_album_tracks(id) {
                Ok(t) => {
                    let first = t.first().map(|x| format!("{}（pos {:?}）", x.name, x.pos)).unwrap_or_default();
                    println!("專輯曲目       OK  {}：{} 首，第一首 {first}", a.name, t.len());
                    if let Some(x) = t.first() {
                        match sp.liked(&x.uri) {
                            Ok(v) => println!("收藏查詢       OK  {} → {}", x.name, if v { "已收藏" } else { "未收藏" }),
                            Err(e) => println!("收藏查詢       FAIL  {e}"),
                        }
                    }
                }
                Err(e) => println!("專輯曲目       FAIL  {e}"),
            }
        }
        if missing.iter().any(|s| s.starts_with("playlist-")) {
            println!("播放清單       略過（要先重新授權）");
        } else {
            match sp.fetch_all_playlists() {
                Ok(pl) => {
                    let own = pl.iter().filter(|p| p.own).count();
                    println!("播放清單       OK  {} 個，自己的 {own} 個", pl.len());
                    if let Some(p) = pl.iter().find(|p| p.own) {
                        let id = p.uri.rsplit(':').next().unwrap_or("");
                        match sp.fetch_playlist_tracks(id) {
                            Ok(t) => println!("清單曲目       OK  {}：{} 首", p.name, t.len()),
                            Err(e) => println!("清單曲目       FAIL  {e}"),
                        }
                    }
                }
                Err(e) => println!("播放清單       FAIL  {e}"),
            }
        }
        match sp.volume() {
            Ok(Some((v, ok))) => println!("音量           OK  {v:?}  可遠端調整={ok}"),
            Ok(None) => println!("音量           沒有作用中的裝置"),
            Err(e) => println!("音量           FAIL  {e}"),
        }
    } else {
        println!("Spotify        沒有登入，略過");
    }

    if let Some(t) = &now {
        let mut f = lyrics::Fetcher::new(paths.lyrics.clone());
        let started = Instant::now();
        let got = f.fetch(&cfg, &t.id, &t.artist, &t.title, &t.album, t.duration_ms, None);
        println!(
            "歌詞抓取       {}  source={} provider={} 行數={} 耗時={} ms",
            if got.source == "error" { "FAIL" } else { "OK  " },
            got.source,
            got.provider,
            got.synced.len(),
            started.elapsed().as_millis()
        );
    }

    // 擷取只在有人要的時候開；探測時假裝在視覺效果頁
    audio::set_viz(true);
    let spec = audio::start();
    std::thread::sleep(Duration::from_millis(1200));
    match spec.lock() {
        Ok(s) if s.alive => println!("loopback 擷取  OK  bass={:.2} treb={:.2}", s.bass, s.treb),
        Ok(_) => println!("loopback 擷取  沒有訊號（現在沒東西在播就是正常的）"),
        Err(_) => println!("loopback 擷取  FAIL"),
    }
    match ledfeed::start() {
        Ok(()) => println!("燈條橋接       OK  UDP {}", ledfeed::PORT),
        Err(e) => println!("燈條橋接       起不來：{e}"),
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    config::install_panic_log();
    let boot = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    // release 是 GUI 程式（不會多開一個黑色主控台視窗），代價是 `--check-*`、`--shot`
    // 這些在終端機裡會完全沒有輸出。有帶參數就接回叫起它的那個主控台。
    // 在 PowerShell 裡要接管線（`| Out-Host`）它才會等程式跑完；不接的話輸出會晚於提示字元出現。
    if args.len() > 1 {
        attach_parent_console();
    }
    if args.iter().any(|a| a == "--probe") {
        return probe();
    }
    if let Some(i) = args.iter().position(|a| a == "--mini-test") {
        return layered::run_test(args.get(i + 1).is_some_and(|a| a == "locked"));
    }
    if args.iter().any(|a| a == "--check-shaders") {
        return shader::check(std::path::Path::new("presets"));
    }
    if args.iter().any(|a| a == "--check-presets") {
        return preset::check(std::path::Path::new("presets"));
    }
    if let Some(i) = args.iter().position(|a| a == "--extract-presets") {
        let Some(vendor) = args.get(i + 1).cloned() else {
            anyhow::bail!("用法：--extract-presets <butterchurn 的 vendor 資料夾> [輸出資料夾]");
        };
        let out = args.get(i + 2).cloned().unwrap_or_else(|| "presets".into());
        return extract::run(std::path::Path::new(&vendor), std::path::Path::new(&out));
    }
    if let Some(i) = args.iter().position(|a| a == "--sheet") {
        let out = args.get(i + 1).cloned().unwrap_or_else(|| "sheet.png".into());
        let start = args.get(i + 2).and_then(|s| s.parse().ok()).unwrap_or(0);
        let count = args.get(i + 3).and_then(|s| s.parse().ok()).unwrap_or(12);
        return shot::sheet(std::path::Path::new(&out), start, count);
    }
    if args.iter().any(|a| a == "--update-selftest") {
        return update::selftest();
    }
    if let Some(i) = args.iter().position(|a| a == "--shot") {
        let out = args.get(i + 1).cloned().unwrap_or_else(|| "shot.png".into());
        let scene = args.get(i + 2).cloned().unwrap_or_else(|| "test".into());
        // `VITRO_SHOT_SIZE=寬x高`：換畫布大小（例如拍小視窗時歌詞折行的樣子）
        let (w, h) = std::env::var("VITRO_SHOT_SIZE")
            .ok()
            .and_then(|v| {
                let (a, b) = v.split_once('x')?;
                Some((a.parse().ok()?, b.parse().ok()?))
            })
            .unwrap_or((1280, 720));
        // `VITRO_SCALE=1.5`：模擬 DPI 縮放。畫布大小是邏輯像素，存出來的圖是實體像素
        let k: f32 = std::env::var("VITRO_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
        gfx::set_scale(k);
        let (w, h) = ((w as f32 * gfx::scale()).round() as u32, (h as f32 * gfx::scale()).round() as u32);
        if scene.starts_with("pv") {
            return pv::shot::run(std::path::Path::new(&out), w, h, &scene);
        }
        return shot::run(std::path::Path::new(&out), w, h, &scene);
    }

    let paths = config::Paths::discover();
    let cfg = config::Config::load(&paths.config);
    theme::set_mode(theme::Appearance::parse(&cfg.appearance).resolve());
    text::set_font(&cfg.font_latin, &cfg.font_cjk, cfg.font_scale);
    zh::set_enabled(cfg.lyrics_traditional);
    gfx::set_ui_zoom(cfg.ui_scale);
    let library = load_library(&paths, &cfg.sort);
    // 剛被舊版（或 Python 版）更新叫起來：先等它們結束、刪掉 .old。
    // 要在搶單一實例的鎖之前 —— 舊版還握著那把鎖
    update::after_update(&args);
    // 已經有一份在跑就叫它出來，自己不開。要在綁燈條的埠之前，免得搶埠
    if !instance::claim(cfg.port) {
        return Ok(());
    }
    let viz = viz::Viz::new(&cfg);
    // 燈條的音量回報。綁不起來（Python 版開著）只影響燈條，不擋程式
    if let Err(e) = ledfeed::start() {
        crate::warn!("燈條橋接起不來（UDP {}）：{e}", ledfeed::PORT);
    }
    // 沒有 client_id 就一開始打開設定精靈 —— 不然第一次用的人不知道要去編 config.json
    let need_setup = !cfg.client_id_looks_valid();

    let el = EventLoop::<UserEv>::with_user_event().build()?;
    let proxy = el.create_proxy();
    let tray_proxy = el.create_proxy();
    instance::listen(move || {
        let _ = proxy.send_event(UserEv::Activate);
    });
    el.set_control_flow(ControlFlow::Poll);
    let now = Instant::now();
    let mut app = App {
        window: None,
        r: None,
        player: player::spawn(cfg.clone(), config::Paths::discover()),
        spec: audio::start(),
        paths,
        library,
        albums_rev: 0,
        positioned: false,
        user_touched: false,
        ghost: None,
        ghost_at: 0,
        fake: Vec::new(),
        drill: None,
        track_cache: Default::default(),
        drill_loading: None,
        tracks_seen: 0,
        follow_gen: 0,
        back_w: 0.0,
        vol_drag: false,
        seek_drag: None,
        proxy: tray_proxy,
        in_tray: false,
        mini: None,
        mini_ly_sent: false,
        mini_recall: false,
        prefs: None,
        prefs_want: false,
        updater: Arc::default(),
        ghost_anim: 1.0,
        ghost_removing: false,
        ghost_pulse: None,
        bg_flow: BgFade::default(),
        bg_now: BgFade::default(),
        accent: AccentFade::default(),
        swap: None,
        trans: None,
        ktv_idx: -2,
        ktv_at: Instant::now(),
        lyric_scroll: anim::Tween::new(-1.0, 0.45, anim::SOFT),
        lyric_for: String::new(),
        lyric_focus: anim::Tween::new(-1.0, 0.45, anim::SOFT),
        lyric_manual: None,
        lyric_browse: anim::Tween::new(0.0, 0.35, anim::EASE),
        lyric_hover: (None, None, Instant::now()),
        lyric_flash: None,
        plain_scroll: anim::Tween::new(0.0, 0.3, anim::OUT),
        plain_target: 0.0,
        lyric_hits: Default::default(),
        roll_at: (String::new(), Instant::now()),
        vinyl: Default::default(),
        vinyl_view: View::Browse,
        dump_step: 0,
        vol_pending: None,
        vol_sent: now,
        vol_before_mute: 50,
        vol_synced: None,
        viz,
        cfg,
        // `--viz` / `--now`：直接開在那個視圖。量測時要能指定，不然得靠按鍵。
        view: if std::env::args().any(|a| a == "--viz") {
            View::Viz
        } else if std::env::args().any(|a| a == "--now") {
            View::Now
        } else {
            View::Browse
        },
        centre: 0.0,
        target_centre: 0.0,
        screen_mode: ScreenMode::Windowed,
        windowed: None,
        chrome: 1.0,
        pv_look: None,
        pv_mix: anim::Tween::new(0.0, pv::MIX_S, anim::EASE),
        pv_want: false,
        pv_track: (String::new(), Instant::now()),
        pv_snap: None,
        chrome_t: 1.0,
        last_mouse: now,
        cursor: (-1.0, -1.0),
        hover: None,
        cursor_in: false,
        mouse_down: false,
        focused: true,
        last_input: now,
        prefs_focused: false,
        stir: now,
        parked: None,
        occluded: false,
        pv_ear: None,
        refresh: None,
        settings_open: false,
        filtered: None,
        filter_q: String::new(),
        search_at: Instant::now(),
        search: None,
        search_keep: None,
        picker: None,
        caret_t: now,
        settings_sel: 0,
        devices_open: false,
        devices_sel: 0,
        devices_t: anim::Tween::new(0.0, theme::dur::MORPH, anim::SPRING),
        bar_k: anim::Tween::new(1.0, theme::dur::YIELD, anim::STD),
        no_device_seen: 0,
        help_open: false,
        about_open: false,
        about_confirm: false,
        toast: None,
        setup: need_setup.then(scene_setup::Setup::default),
        mods: Default::default(),
        lyrics_wait: None,
        last: now,
        presented: false,
        fps_cap: args
            .iter()
            .position(|a| a == "--fps")
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0),
        boot,
        first_frame_reported: false,
        dt: 1.0 / 60.0,
        frames: 0,
        fps_at: now,
        prof_cpu: (Duration::ZERO, Duration::ZERO),
        prof_wait: Duration::ZERO,
    };
    // 走 poller 那條：畫面開始跑才取走、才開始計時，視窗還沒出來就不會先過期
    if config::CONFIG_BAD.load(std::sync::atomic::Ordering::Relaxed) {
        app.player.shared.toast("設定檔讀不進來，已改用預設值；原檔在 config.json.bad");
    }
    // 啟動時問一次 GitHub（Python 版的 update_check）。沒新版不出聲
    if app.cfg.update_check {
        app.updater.check(true);
    }
    el.run_app(&mut app)?;
    Ok(())
}

/// 接回父行程的主控台。debug 版本來就是主控台程式，呼叫了也沒事（已經有就會失敗，忽略）。
///
/// **stdout 已經接到東西（`> 檔案`、管線）就不要動它。** 無條件 AttachConsole 的話，
/// 用 cmd 重導到檔案時輸出會整個不見 —— 實測過。只有 handle 是空的（直接在主控台裡
/// 打指令）才接，而且接上之後 GUI 程式的標準 handle 不會自己指過去，要自己開 `CONOUT$`。
fn attach_parent_console() {
    use windows::Win32::Foundation::GENERIC_WRITE;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_OUTPUT_HANDLE,
    };
    unsafe {
        let out = GetStdHandle(STD_OUTPUT_HANDLE).ok().filter(|h| !h.is_invalid() && !h.0.is_null());
        if out.is_some() || AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return;
        }
        let Ok(con) = CreateFileW(
            windows::core::w!("CONOUT$"),
            GENERIC_WRITE.0,
            FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        ) else {
            return;
        };
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, con);
        let _ = SetStdHandle(STD_ERROR_HANDLE, con);
        // 主控台已經先印出提示字元了，換一行免得輸出黏在它後面
        println!();
    }
}

/// 標題列與 Alt+Tab 的圖示。exe 本身的圖示是 build.rs 嵌的資源，
/// 但執行中的視窗不會自己去拿它，要另外給。
fn window_icon() -> Option<winit::window::Icon> {
    let img = image::load_from_memory(include_bytes!("../assets/icon.png")).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    winit::window::Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// 第三方授權全文是編進 exe 的（單一執行檔，旁邊沒有別的檔案）。
/// 要看的時候寫到暫存資料夾，交給預設的文字編輯器開 —— 兩千多行，記事本比自己做捲動清單好讀。
fn open_licenses() -> std::io::Result<()> {
    const TEXT: &str = include_str!("../assets/THIRD-PARTY-LICENSES.txt");
    let p = std::env::temp_dir().join("VitroDeck-THIRD-PARTY-LICENSES.txt");
    std::fs::write(&p, TEXT)?;
    oauth::open_in_browser(&p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn al(name: &str, added: &str, year: &str) -> spotify::Album {
        spotify::Album { name: name.into(), added: added.into(), year: year.into(), kind: "album".into(), ..Default::default() }
    }

    fn pl(name: &str, order: u32) -> spotify::Album {
        spotify::Album { name: name.into(), kind: "playlist".into(), order: Some(order), ..Default::default() }
    }

    #[test]
    fn ktv_行級用到下一句的間隔內插_逐字照字數() {
        let line = |t: i64, s: &str| lyrics::Line { t, text: s.into(), words: None, ..Default::default() };
        let mut l = lyrics::Lyrics { synced: vec![line(1000, "一二三"), line(3000, "四五")], ..Default::default() };
        let (c, n, w) = ktv_view(&l, 0, 2000, 10_000);
        assert_eq!((c.as_str(), n.as_str()), ("一二三", "四五"));
        assert!((w - 0.5).abs() < 1e-4, "{w}");
        // 逐字：兩個字唱完、第三個字唱一半 → 2.5 / 3
        let wd = |t: i64, s: &str| lyrics::Word { t, d: 200, text: s.into() };
        l.synced[0].words = Some(vec![wd(1000, "一"), wd(1200, "二"), wd(1400, "三")]);
        let (_, _, w) = ktv_view(&l, 0, 1500, 10_000);
        assert!((w - 2.5 / 3.0).abs() < 1e-4, "{w}");
        // 最後一句、間奏
        let (c, n, _) = ktv_view(&l, -1, 0, 10_000);
        assert!(c.is_empty() && n == "一二三");
    }

    #[test]
    fn f11_只在留下的模式之間轉() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        use ScreenMode::*;
        let all = s(&["one", "span"]);
        assert_eq!(Windowed.next_in(&all), One);
        assert_eq!(One.next_in(&all), Span);
        assert_eq!(Span.next_in(&all), Windowed);
        let only_span = s(&["span"]);
        assert_eq!(Windowed.next_in(&only_span), Span);
        // 從設定直接切到被排除的那段：下一下回視窗
        assert_eq!(One.next_in(&only_span), Windowed);
        assert_eq!(Windowed.next_in(&[]), Windowed);
    }

    #[test]
    fn 加入時間與年份排序時清單排最前面而且照原順序() {
        let mut v = vec![al("舊", "2020", "1990"), pl("B", 1), al("新", "2024", "2001"), pl("A", 0)];
        sort_library(&mut v, "added");
        let names: Vec<_> = v.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "新", "舊"]);
        sort_library(&mut v, "year");
        let names: Vec<_> = v.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "新", "舊"]);
        // 名稱排序就混排：清單 Z 排到專輯後面
        v.push(pl("Z", 2));
        sort_library(&mut v, "name");
        let names: Vec<_> = v.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "Z", "新", "舊"]);
        let mut w = vec![pl("Z", 0), al("a", "", "")];
        sort_library(&mut w, "name");
        assert_eq!(w[0].name, "a");
    }
}
