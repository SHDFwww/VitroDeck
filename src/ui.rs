//! 版面與命中測試。
//!
//! **版面的數字只能有一份。** 場景畫的時候用的，跟這裡算命中框用的，
//! 必須是同一組 —— 「看得到但點不到」這種 bug 在截圖上完全看不出來，
//! 只有真的拿滑鼠點下去才會發現，所以寧可多繞一層也不要兩邊各寫一次。
//!
//! 封面的命中測試比較麻煩：封面是被透視投影過的四邊形，不是矩形。
//! 這裡把 `cover.wgsl` 的頂點變換照抄一遍算出四個角，再做凸多邊形內側判定。

use crate::cover;
use crate::theme;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// 以中心點與大小建立。按鈕都是這樣定位的 —— 文字也是置中畫，
    /// 兩邊用同一個中心才不會差半個字。
    pub fn centred(cx: f32, cy: f32, w: f32, h: f32) -> Self {
        Self { x: cx - w * 0.5, y: cy - h * 0.5, w, h }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn cx(&self) -> f32 {
        self.x + self.w * 0.5
    }

    pub fn cy(&self) -> f32 {
        self.y + self.h * 0.5
    }

    /// 往外長一圈。手指／滑鼠的命中區比畫出來的圖形大一點才好按。
    pub fn grow(&self, d: f32) -> Self {
        Self { x: self.x - d, y: self.y - d, w: self.w + d * 2.0, h: self.h + d * 2.0 }
    }

    /// `[x, y, w, h]`，玻璃那邊的寫法
    pub fn arr(&self) -> [f32; 4] {
        [self.x, self.y, self.w, self.h]
    }

    /// 兩塊有沒有重疊（只碰到邊不算）
    pub fn overlaps(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

/// 把一個圖示置中畫在按鈕的命中框裡。
///
/// 圖示與可以點的地方共用同一個中心 —— 分開寫的話差個幾像素也看不出來，
/// 但滑鼠會落空。`hot` 是滑鼠正停在上面，稍微提亮。
pub fn button(
    text: &mut crate::text::Text,
    r: &Rect,
    glyph: &str,
    size: f32,
    alpha: f32,
    hot: bool,
) {
    if alpha <= 0.01 {
        return;
    }
    let lh = size * 1.3;
    let (tw, _) = text.measure(glyph, size, lh, None);
    let a = (if hot { 1.0 } else { 0.78 }) * alpha;
    text.push(
        glyph,
        r.cx() - tw * 0.5,
        r.cy() - lh * 0.5,
        size,
        lh,
        theme::ink(a),
        500,
        None,
        None,
    );
}

/// 視覺效果頁的按鈕框。
pub fn viz_btn(l: &VizL, hit: Hit) -> Option<Rect> {
    Some(match hit {
        Hit::PresetPrev => l.prev,
        Hit::PresetNext => l.next,
        Hit::Favourite => l.fav,
        Hit::Category => l.cat,
        _ => return None,
    })
}

/// 點下去會發生什麼事。場景只負責算出「哪塊區域對應哪個動作」，
/// 真正要做什麼由 `main.rs` 決定。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    /// 點到側邊的封面 —— 捲過去
    Cover(usize),
    /// 點到中央的封面 —— 播這張專輯
    PlayCurrent,
    Prev,
    PlayPause,
    Next,
    /// 進度條，值是 0..1
    Seek(f32),
    /// 控制列上的曲名 —— 切到歌詞頁
    OpenLyrics,
    PresetPrev,
    PresetNext,
    Favourite,
    Category,
    Shuffle,
    Repeat,
    Settings,
    /// 設定面板的第幾列
    SettingRow(usize),
    CloseSettings,
    /// preset 選單的第幾列（篩選後清單裡的絕對位置，不是畫面上第幾列）
    PickerRow(usize),
    /// 那一列左邊的星號 —— 收藏，不套用
    PickerStar(usize),
    /// 分類鈕，值是 [`PICKER_CATS`] 的索引
    PickerCat(usize),
    /// 點在面板外面
    PickerClose,
    /// 裝置選單的第幾列
    DeviceRow(usize),
    CloseDevices,
    /// 歌詞頁沒有歌詞時的「前往 LRCLIB」
    OpenLrclib,
    /// 歌詞頁的第幾句（`synced` 的索引）—— 跳到那句開始唱的地方
    Lyric(usize),
    /// 設定精靈的四顆鈕
    SetupDashboard,
    SetupCopy,
    SetupSave,
    SetupClose,
    /// 收藏／取消收藏正在播的這首（愛心）
    Like,
    /// 裝置選單
    Devices,
    /// 音量左邊那顆喇叭：靜音／取消靜音
    Mute,
    /// 音量滑桿，值是 0..1
    Volume(f32),
    /// 展開中，左上角的返回鍵
    DrillBack,
    /// 關於面板（I）：使用教學、第三方授權、中斷連結、關閉
    AboutGuide,
    AboutLicenses,
    AboutDisconnect,
    CloseAbout,
}

/// 愛心、裝置、音量那一組要的狀態。封面流的控制列與歌詞頁共用。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tools {
    /// `None` = 還不知道正在播的是 Spotify 上的哪一首，愛心是灰的、按了沒反應
    pub liked: Option<bool>,
    /// `None` = 還沒問到音量
    pub volume: Option<u8>,
    /// 裝置不允許遠端調音量（有些音箱、手機的省電模式）
    pub vol_locked: bool,
}

/// 音量滑桿上 `x` 的位置換成 0..1。拖曳時游標會跑出滑桿，所以要夾住。
pub fn slider_t(r: &Rect, x: f32) -> f32 {
    ((x - r.x) / r.w.max(1.0)).clamp(0.0, 1.0)
}

/// 展開中左上角的返回鍵：箭頭＋現在在看的那張的名字。CSS 的 `#drill-back`：
/// top/left 20、高 42、左右留白 12／18、箭頭 20、間距 8。寬度跟著名字走，
/// 所以要先量字 —— 畫的和點的都用這一個函式，數字才不會兩份。
pub fn drill_back(name_w: f32) -> Rect {
    Rect::new(20.0, 20.0, 12.0 + 20.0 + 8.0 + name_w + 18.0, 42.0)
}

/// 名字最多能佔多寬（`max-width: min(420px, 40vw)` 扣掉箭頭與留白）。
pub fn drill_back_text_max(fw: f32) -> f32 {
    (420.0f32).min(fw * 0.4) - (12.0 + 20.0 + 8.0 + 18.0)
}

/// 一顆 `.barbtn`。外觀照 CSS：平常 #c2c3cc，滑鼠停著變 --fg 並墊一層 .1 的白色圓底
/// （直徑 `d`，以命中框為中心）；開著的模式鍵是綠色，`dot` 的話底下 3px 處加一顆 3px 小圓點；
/// 不能按的透明度 .35、停著也不亮。`one` 是單曲重複：圖示中間加一個 8px 粗體的「1」。
/// 封面流與歌詞頁共用，兩邊的按鈕才會長得一樣。
#[allow(clippy::too_many_arguments)]
pub fn svg_button(
    gpu: &crate::gfx::Gpu,
    quads: &mut crate::quad::QuadRenderer,
    text: &mut crate::text::Text,
    r: &Rect,
    d: f32,
    icon: crate::icons::Svg,
    on: bool,
    dot: bool,
    enabled: bool,
    hot: bool,
    one: bool,
) {
    use crate::quad::Quad;
    use crate::theme::{accent, fg, soft};
    let hot = hot && enabled;
    if hot {
        quads.push(Quad::new(r.cx() - d * 0.5, r.cy() - d * 0.5, d, d).radius(d * 0.5).color(theme::ink(0.1)));
    }
    let mut col = if on { accent() } else if hot { fg() } else { soft() };
    if !enabled {
        col[3] *= 0.35;
    }
    quads.svg(gpu, icon, 19.0, r.cx(), r.cy(), col);
    if on && dot {
        quads.push(Quad::new(r.cx() - 1.5, r.cy() + d * 0.5 - 6.0, 3.0, 3.0).radius(1.5).color(col));
    }
    if one {
        let (tw, _) = text.measure_weight("1", 8.0, 8.0, None, 700);
        text.push("1", r.cx() - tw * 0.5, r.cy() - 3.0, 8.0, 8.0, col, 700, None, None);
    }
}

/// 喇叭圖示照音量換：0 打叉、1–49 一道聲波、其餘兩道（`.js-vol-btn.mute` / `.low`）。
pub fn vol_icon(volume: Option<u8>) -> crate::icons::Svg {
    use crate::icons::Svg;
    match volume {
        Some(0) => Svg::VolMute,
        Some(v) if v < 50 => Svg::VolLow,
        _ => Svg::Vol,
    }
}

/// 封面流的愛心、裝置、喇叭、音量滑桿這一組。
#[allow(clippy::too_many_arguments)]
pub fn draw_tools(
    gpu: &crate::gfx::Gpu,
    quads: &mut crate::quad::QuadRenderer,
    text: &mut crate::text::Text,
    like: &Rect,
    devices: &Rect,
    mute: &Rect,
    volume: &Rect,
    t: &Tools,
    hover: Option<Hit>,
    dragging_volume: bool,
) {
    use crate::icons::Svg;
    use crate::quad::Quad;
    const D: f32 = 36.0;
    let liked = t.liked == Some(true);
    // 收藏是填滿，不加小圓點（`.js-like.on::after { display: none }`）
    let heart = if liked { Svg::HeartFilled } else { Svg::Heart };
    svg_button(gpu, quads, text, like, D, heart, liked, false, t.liked.is_some(), hover == Some(Hit::Like), false);
    svg_button(gpu, quads, text, devices, D, Svg::Device, false, false, true, hover == Some(Hit::Devices), false);
    let vol_ok = !t.vol_locked;
    svg_button(gpu, quads, text, mute, D, vol_icon(t.volume), false, false, vol_ok, hover == Some(Hit::Mute), false);
    // 滑桿：4px、底 .16、填滿 --fg
    let va = if vol_ok { 1.0 } else { 0.35 };
    let v = t.volume.unwrap_or(0) as f32 / 100.0;
    let r = volume;
    quads.push(Quad::new(r.x, r.cy() - 2.0, r.w, 4.0).radius(theme::radius::TRACK).color(theme::ink(0.16 * va)));
    if v > 0.0 {
        quads.push(
            Quad::new(r.x, r.cy() - 2.0, (r.w * v).max(4.0), 4.0)
                .radius(theme::radius::TRACK)
                .color({ let f = theme::fg(); [f[0], f[1], f[2], va] }),
        );
    }
    if vol_ok && (dragging_volume || matches!(hover, Some(Hit::Volume(_)))) {
        let kx = r.x + r.w * v;
        quads.push(
            Quad::new(kx - 6.0, r.cy() - 6.0, 12.0, 12.0)
                .radius(theme::radius::SM)
                .color(theme::ink(1.0))
                .shadow_of(&theme::SHADOW_KNOB, 1.0),
        );
    }
}

// ---------------------------------------------------------------- 通知列

/// 通知列的字級與留白，照 CSS 的 `#toast`：`padding: 11px 22px; font-size: 14px`。
pub const TOAST_SIZE: f32 = 14.0;
pub const TOAST_LINE: f32 = 20.0;
const TOAST_PAD_X: f32 = 22.0;
const TOAST_PAD_Y: f32 = 11.0;

/// 通知列的外框。水平置中，底邊貼在 `bottom` 上方。
///
/// `bottom` 由呼叫端依視圖給 —— 封面流與視覺效果要讓開控制列，
/// 歌詞頁沒有浮動列，照 CSS 的 34px 貼底。`text_w` 是量好的字寬，
/// 超過畫面寬度就讓外框夾在兩側各留 16px。
pub fn toast(fw: f32, text_w: f32, bottom: f32) -> Rect {
    let w = (text_w + TOAST_PAD_X * 2.0).min(fw - 32.0);
    let h = TOAST_LINE + TOAST_PAD_Y * 2.0;
    Rect::new((fw - w) * 0.5, bottom - h, w, h)
}

/// 通知列的字要限在多寬以內（外框扣掉左右留白）。
pub fn toast_text_max(fw: f32) -> f32 {
    fw - 32.0 - TOAST_PAD_X * 2.0
}

// ---------------------------------------------------------------- 設定面板

pub struct SettingsL {
    pub panel: Rect,
    pub close: Rect,
    /// 每一列的命中框
    pub rows: Vec<Rect>,
    #[allow(dead_code)]
    pub row_h: f32,
}

pub fn settings(fw: f32, fh: f32, n: usize) -> SettingsL {
    let head = 64.0;
    let pad = 14.0;
    let gap = 12.0;
    // 列高平常 46；視窗矮到放不下時往下縮（最低 38，說明那行還塞得進去），
    // 讓整個面板停在控制列（底下 128px）上方，不要蓋到它
    let room = fh - 128.0 - 20.0 - head - pad - 26.0;
    // 一欄塞不下（最低列高也不夠）而且夠寬，就排成兩欄，跟說明表一樣
    let cols = if room / (n.max(1) as f32) < 38.0 && fw >= 900.0 { 2 } else { 1 };
    let per = n.div_ceil(cols).max(1);
    let row_h = (room / per as f32).clamp(38.0, 46.0);
    let pw = if cols == 1 { (fw * 0.62).min(620.0) } else { (fw * 0.92).min(1120.0) };
    let col_w = (pw - pad * 2.0 - gap * (cols - 1) as f32) / cols as f32;
    // 底部要多留一行放操作提示，不然它會壓在最後一列上
    let ph = head + per as f32 * row_h + pad + 26.0;
    // 垂直置中，但不要壓到底下的控制列 —— 面板愈長愈容易蓋到它
    let y = ((fh - ph) * 0.5).min(fh - 128.0 - ph).max(20.0);
    let panel = Rect::new((fw - pw) * 0.5, y, pw, ph.min(fh - 40.0));
    // 先排滿左欄再排右欄，分組才不會被拆得前後顛倒
    let rows = (0..n)
        .map(|i| {
            let (c, r) = (i / per, i % per);
            Rect::new(
                panel.x + pad + c as f32 * (col_w + gap),
                panel.y + head + r as f32 * row_h,
                col_w,
                row_h,
            )
        })
        .collect();
    SettingsL {
        close: Rect::centred(panel.x + pw - 34.0, panel.y + 32.0, BTN, BTN),
        panel,
        rows,
        row_h,
    }
}

pub fn hit_settings(l: &SettingsL, mx: f32, my: f32) -> Option<Hit> {
    if l.close.contains(mx, my) {
        return Some(Hit::CloseSettings);
    }
    for (i, r) in l.rows.iter().enumerate() {
        if r.contains(mx, my) {
            return Some(Hit::SettingRow(i));
        }
    }
    // 面板以外點一下就關掉 —— 沒有標題列可以拖，這是最順的關法
    if !l.panel.contains(mx, my) {
        return Some(Hit::CloseSettings);
    }
    None
}

/// 裝置選單跟設定面板長得一樣（一疊等高列 ＋ 右上角關閉鈕），版面直接共用 [`settings`]，
/// 只有命中的結果換成自己的。`n` 是裝置數，沒有裝置時也要留一列放說明。
pub fn devices(fw: f32, fh: f32, n: usize) -> SettingsL {
    settings(fw, fh, n.max(1))
}

pub fn hit_devices(l: &SettingsL, n: usize, mx: f32, my: f32) -> Option<Hit> {
    match hit_settings(l, mx, my)? {
        Hit::CloseSettings => Some(Hit::CloseDevices),
        // 沒有裝置時那一列只是說明，點了不算
        Hit::SettingRow(i) if i < n => Some(Hit::DeviceRow(i)),
        _ => None,
    }
}

// ---------------------------------------------------------------- 關於

/// 關於面板（web 版的 `#about`）：三段說明、兩個連結、版本與資料夾、中斷連結鈕。
pub struct AboutL {
    pub panel: Rect,
    pub close: Rect,
    /// 三段說明各自的頂端 y
    pub para_y: [f32; 3],
    pub guide: Rect,
    pub licenses: Rect,
    /// 版本與資料夾那一行
    pub meta_y: f32,
    pub disconnect: Rect,
}

pub fn about(fw: f32, fh: f32) -> AboutL {
    let pw = (fw - 40.0).min(560.0);
    let ph = 384.0f32.min(fh - 40.0);
    let panel = Rect::new((fw - pw) * 0.5, ((fh - ph) * 0.5).max(20.0), pw, ph);
    let (x, y) = (panel.x + 28.0, panel.y);
    AboutL {
        close: Rect::centred(panel.x + pw - 34.0, panel.y + 32.0, BTN, BTN),
        para_y: [y + 70.0, y + 104.0, y + 158.0],
        guide: Rect::new(x, y + 220.0, 92.0, 30.0),
        licenses: Rect::new(x + 104.0, y + 220.0, 104.0, 30.0),
        meta_y: y + 266.0,
        disconnect: Rect::new(x, y + 298.0, pw - 56.0, 40.0),
        panel,
    }
}

pub fn hit_about(l: &AboutL, mx: f32, my: f32) -> Option<Hit> {
    if l.close.contains(mx, my) || !l.panel.contains(mx, my) {
        return Some(Hit::CloseAbout);
    }
    [(l.guide, Hit::AboutGuide), (l.licenses, Hit::AboutLicenses), (l.disconnect, Hit::AboutDisconnect)]
        .into_iter()
        .find(|(r, _)| r.contains(mx, my))
        .map(|(_, h)| h)
}

// ---------------------------------------------------------------- 設定精靈

/// 首次設定精靈。三步，照 web 版的 `#wizard`：開 Dashboard、填 redirect URI、貼 client_id。
pub struct SetupL {
    pub panel: Rect,
    pub close: Rect,
    /// 每一步的頂端 y（編號圓圈與標題對齊這條線）
    pub step_y: [f32; 3],
    /// 步驟內文的左緣（讓開編號圓圈）
    pub body_x: f32,
    pub body_w: f32,
    pub dashboard: Rect,
    /// 唯讀的 redirect URI 框與它的「複製」
    pub redirect: Rect,
    pub copy: Rect,
    /// client_id 輸入框與「儲存」
    pub input: Rect,
    pub save: Rect,
    /// 錯誤訊息那行
    pub err_y: f32,
}

/// 一步的高度：標題 20 ＋ 4 ＋ 說明兩行 36 ＋ 10 ＋ 控制項 34 ＋ 步與步之間 18
const SETUP_STEP: f32 = 20.0 + 4.0 + 36.0 + 10.0 + 34.0 + 18.0;
const SETUP_CTRL_Y: f32 = 20.0 + 4.0 + 36.0 + 10.0;

pub fn setup(fw: f32, fh: f32) -> SetupL {
    let head = 64.0;
    let pw = (fw * 0.8).min(620.0);
    let ph = head + SETUP_STEP * 3.0 + 22.0 + 30.0;
    let panel = Rect::new((fw - pw) * 0.5, ((fh - ph) * 0.5).max(20.0), pw, ph);
    let body_x = panel.x + 22.0 + 30.0 + 12.0;
    let body_w = panel.x + pw - 22.0 - body_x;
    let step_y = [0, 1, 2].map(|i| panel.y + head + i as f32 * SETUP_STEP);
    let ctrl = |i: usize| step_y[i] + SETUP_CTRL_Y;
    const BTN_W: f32 = 76.0;
    let field_w = body_w - BTN_W - 8.0;
    SetupL {
        close: Rect::centred(panel.x + pw - 34.0, panel.y + 32.0, BTN, BTN),
        dashboard: Rect::new(body_x, ctrl(0), 140.0, 34.0),
        redirect: Rect::new(body_x, ctrl(1), field_w, 34.0),
        copy: Rect::new(body_x + field_w + 8.0, ctrl(1), BTN_W, 34.0),
        input: Rect::new(body_x, ctrl(2), field_w, 34.0),
        save: Rect::new(body_x + field_w + 8.0, ctrl(2), BTN_W, 34.0),
        err_y: step_y[2] + SETUP_STEP - 12.0,
        panel,
        step_y,
        body_x,
        body_w,
    }
}

/// 點在面板外面**不關** —— 這是開程式就要完成的設定，誤觸關掉就得重開程式找它。
pub fn hit_setup(l: &SetupL, mx: f32, my: f32) -> Option<Hit> {
    for (r, h) in [
        (l.close, Hit::SetupClose),
        (l.dashboard, Hit::SetupDashboard),
        (l.copy, Hit::SetupCopy),
        (l.save, Hit::SetupSave),
    ] {
        if r.contains(mx, my) {
            return Some(h);
        }
    }
    None
}

// ---------------------------------------------------------------- 搜尋框

/// 封面流上方那顆膠囊。CSS：`top: 40px`、`padding: 11px 24px`、
/// 輸入框 `min(420px, 46vw)`，前面一個淡淡的「/」。
pub struct SearchL {
    pub pill: Rect,
    pub label_x: f32,
    pub text_x: f32,
    pub text_w: f32,
}

pub fn search(fw: f32, _fh: f32) -> SearchL {
    let input_w = (fw * 0.46).min(420.0);
    let (pad, label_w, gap) = (24.0, 9.0, 10.0);
    let w = pad + label_w + gap + input_w + pad;
    let pill = Rect::new((fw - w) * 0.5, 40.0, w, 44.0);
    SearchL {
        label_x: pill.x + pad,
        text_x: pill.x + pad + label_w + gap,
        text_w: input_w,
        pill,
    }
}

// ---------------------------------------------------------------- preset 選單

/// 分類鈕的順序與名稱。跟 `viz::Category` 的 `next()` 是同一圈。
pub const PICKER_CATS: [crate::viz::Category; 5] = [
    crate::viz::Category::All,
    crate::viz::Category::Fav,
    crate::viz::Category::Retro,
    crate::viz::Category::Mid,
    crate::viz::Category::Flashy,
];

/// 一列的高度。CSS 是 13.5px 字、上下 6px padding。
pub const PICKER_ROW: f32 = 30.0;

/// 照 web 版 `#viz-picker .panel`：寬 `min(600px, 80vw)`、
/// padding `26px 30px 20px`、各段之間 14px，整塊垂直置中。
///
/// ```text
///   PRESET               標題
///   ( / 輸入關鍵字篩選 )  搜尋框
///   [全部][復古]…         分類鈕
///   ┌──────────────┐
///   │ ☆ 名稱        │     清單，高 min(48vh, 440)
///   └──────────────┘
///   N 個 · ↑↓ 預覽 …      提示
/// ```
pub struct PickerL {
    pub panel: Rect,
    pub title_y: f32,
    pub search: Rect,
    pub cats: Vec<Rect>,
    pub list: Rect,
    /// 清單一次放得下幾列
    pub rows: usize,
    pub foot_y: f32,
}

pub fn picker(fw: f32, fh: f32) -> PickerL {
    let pw = (fw * 0.8).min(600.0);
    let (px, gap) = (30.0, 14.0);
    let inner_w = pw - px * 2.0;
    let rows = (((fh * 0.48).min(440.0) / PICKER_ROW).floor() as usize).max(3);
    let list_h = rows as f32 * PICKER_ROW;
    // 各段高度：標題 26、搜尋框 38、分類鈕 24、清單、提示 16
    let ph = 26.0 + 26.0 + gap + 38.0 + gap + 24.0 + gap + list_h + gap + 16.0 + 20.0;
    let panel = Rect::new((fw - pw) * 0.5, ((fh - ph) * 0.5).max(10.0), pw, ph);
    let x = panel.x + px;
    let mut y = panel.y + 26.0;
    let title_y = y;
    y += 26.0 + gap;
    let search = Rect::new(x, y, inner_w, 38.0);
    y += 38.0 + gap;

    // 分類鈕：字寬用「字數 x 12」估。全是中文，估出來跟量的差不到一兩像素，
    // 而版面這層拿不到字型 —— 命中框跟畫的地方用同一個估計值就不會錯位。
    let widths: Vec<f32> = PICKER_CATS
        .iter()
        .map(|c| c.label().chars().count() as f32 * 12.0 + 26.0)
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 6.0 * (widths.len() as f32 - 1.0);
    let mut cx = x + (inner_w - total) * 0.5;
    let cats = widths
        .iter()
        .map(|w| {
            let r = Rect::new(cx, y, *w, 24.0);
            cx += w + 6.0;
            r
        })
        .collect();
    y += 24.0 + gap;
    let list = Rect::new(x, y, inner_w, list_h);
    y += list_h + gap;
    PickerL { panel, title_y, search, cats, list, rows, foot_y: y }
}

/// 第 `i` 列（篩選後的絕對位置）畫在哪。`top` 是清單捲到的第一列。
pub fn picker_row(l: &PickerL, top: usize, i: usize) -> Option<Rect> {
    if i < top || i >= top + l.rows {
        return None;
    }
    Some(Rect::new(l.list.x, l.list.y + (i - top) as f32 * PICKER_ROW, l.list.w, PICKER_ROW))
}

/// 列上的星號。CSS 是 `width: 1.5em`，緊貼在列的左 padding 後面。
pub fn picker_star(row: &Rect) -> Rect {
    Rect::new(row.x + 6.0, row.y, 28.0, row.h)
}

pub fn hit_picker(l: &PickerL, top: usize, n: usize, mx: f32, my: f32) -> Option<Hit> {
    if !l.panel.contains(mx, my) {
        return Some(Hit::PickerClose);
    }
    for (i, r) in l.cats.iter().enumerate() {
        if r.contains(mx, my) {
            return Some(Hit::PickerCat(i));
        }
    }
    if l.list.contains(mx, my) {
        let i = top + ((my - l.list.y) / PICKER_ROW) as usize;
        if i < n {
            let row = picker_row(l, top, i)?;
            if picker_star(&row).contains(mx, my) {
                return Some(Hit::PickerStar(i));
            }
            return Some(Hit::PickerRow(i));
        }
    }
    None
}

/// 讓 `at` 那列留在看得到的範圍裡，捲動量最小。
///
/// 上下鍵移動時只做最小捲動 —— 每按一次整個清單都在跳的話根本沒法看。
/// 剛開啟或換了篩選字時才置中（見 [`picker_centre`]）。
pub fn picker_follow(top: usize, at: usize, rows: usize, n: usize) -> usize {
    let top = if at < top {
        at
    } else if at >= top + rows {
        at + 1 - rows
    } else {
        top
    };
    top.min(n.saturating_sub(rows))
}

/// 把 `at` 那列捲到正中間。
pub fn picker_centre(at: usize, rows: usize, n: usize) -> usize {
    at.saturating_sub(rows / 2).min(n.saturating_sub(rows))
}

// ---------------------------------------------------------------- 封面流

/// 底部控制列。**是三段獨立的玻璃橫跨整個底部**，不是一顆置中的長膠囊 ——
/// 那是這個介面的形狀語言，做成一整條就完全不像了。
///
/// ```text
///   [ 封面＋曲名 ]  [ 傳輸鍵 ＋ 進度條 ]        [ 循環 隨機 設定 ]
///     220..340         中間全部              128
/// ```
pub struct Flow {
    pub cover_px: f32,
    pub cx: f32,
    pub cy: f32,
    /// 三段玻璃
    pub bar_now: Rect,
    pub bar_main: Rect,
    pub bar_extra: Rect,
    /// 整條的範圍，拿來擋住後面的封面
    pub bar: Rect,
    /// 左段那張小封面
    pub bar_art: Rect,
    pub prev: Rect,
    pub play: Rect,
    pub next: Rect,
    pub progress: Rect,
    pub pos_x: f32,
    pub dur_x: f32,
    pub repeat: Rect,
    pub shuffle: Rect,
    pub settings: Rect,
    /// 左段右邊的愛心
    pub like: Rect,
    pub devices: Rect,
    pub mute: Rect,
    /// 音量滑桿畫出來的範圍（命中區會再放大）
    pub volume: Rect,
}

impl Flow {
    /// 控制列整組往下移 `dy`（進出歌詞頁的收下去／升上來）。命中測試不用這個 ——
    /// 動畫期間點不到它沒關係。
    pub fn shift_bar(&mut self, dy: f32) {
        if dy == 0.0 {
            return;
        }
        for r in [
            &mut self.bar_now, &mut self.bar_main, &mut self.bar_extra, &mut self.bar, &mut self.bar_art,
            &mut self.prev, &mut self.play, &mut self.next, &mut self.progress, &mut self.repeat,
            &mut self.shuffle, &mut self.settings, &mut self.like, &mut self.devices, &mut self.mute,
            &mut self.volume,
        ] {
            r.y += dy;
        }
    }
}

/// 音量滑桿的寬度
const VOL_W: f32 = 84.0;

/// 按鈕的大小。畫的圖示比這個小，但命中區要夠大。
const BTN: f32 = 36.0;
/// 封面流的播放鍵（`.barbtn.big`）
const BIG: f32 = 42.0;

/// `scale` 是設定裡的 `cover_scale`（0.5～2.0）。Python 版用 `-` `+` `\`
/// 調整並存進同一份 config.json，兩邊要吃同一個值。
pub fn flow(fw: f32, fh: f32, scale: f32) -> Flow {
    let cover_px = (fh * 0.42).min(fw * 0.28) * scale.clamp(0.5, 2.0);
    let (cx, cy) = (fw * 0.5, fh * 0.40);

    // 數字全部照 CSS：padding 0 14px 14px、gap 10、.barseg 高 64
    const PAD: f32 = 14.0;
    const GAP: f32 = 10.0;
    const H: f32 = 64.0;
    let y = fh - PAD - H;
    let now_w = (fw * 0.24).clamp(220.0, 340.0);
    // 右段：重複、隨機、裝置、喇叭＋滑桿、設定
    let extra_w = 8.0 + BTN * 5.0 + VOL_W + 6.0 + 2.0 * 4.0 + 8.0;
    let bar_now = Rect::new(PAD, y, now_w, H);
    let bar_extra = Rect::new(fw - PAD - extra_w, y, extra_w, H);
    let main_x = PAD + now_w + GAP;
    let bar_main = Rect::new(main_x, y, (bar_extra.x - GAP - main_x).max(120.0), H);

    // 中段：左邊傳輸鍵（gap 2），間隔 18，右邊是時間＋進度條（gap 12）
    let tx = bar_main.x + 20.0;
    let by = bar_main.cy();
    let scrub_x = tx + BTN * 2.0 + BIG + 4.0 + 18.0;
    let scrub_w = (bar_main.x + bar_main.w - 20.0 - scrub_x).max(60.0);
    let ex = bar_extra.x + 8.0;

    Flow {
        cover_px,
        cx,
        cy,
        bar: Rect::new(PAD, y, fw - PAD * 2.0, H),
        bar_now,
        bar_main,
        bar_extra,
        bar_art: Rect::new(bar_now.x + 8.0, bar_now.y + 8.0, 48.0, 48.0),
        prev: Rect::centred(tx + BTN * 0.5, by, BTN, BTN),
        play: Rect::centred(tx + BTN + 2.0 + BIG * 0.5, by, BIG, BIG),
        next: Rect::centred(tx + BTN * 1.5 + BIG + 4.0, by, BTN, BTN),
        progress: Rect::new(scrub_x + 46.0, by - 3.0, (scrub_w - 92.0).max(40.0), 6.0),
        pos_x: scrub_x,
        dur_x: scrub_x + scrub_w,
        repeat: Rect::centred(ex + BTN * 0.5, by, BTN, BTN),
        shuffle: Rect::centred(ex + BTN * 1.5 + 2.0, by, BTN, BTN),
        devices: Rect::centred(ex + BTN * 2.5 + 4.0, by, BTN, BTN),
        mute: Rect::centred(ex + BTN * 3.5 + 6.0, by, BTN, BTN),
        volume: Rect::new(ex + BTN * 4.0 + 6.0, by - 7.0, VOL_W, 14.0),
        settings: Rect::centred(ex + BTN * 4.5 + VOL_W + 6.0 + 8.0, by, BTN, BTN),
        like: Rect::centred(bar_now.x + bar_now.w - 8.0 - BTN * 0.5, bar_now.cy(), BTN, BTN),
    }
}

/// 一張封面投影到畫面上的四個角。順序是左上、右上、右下、左下。
///
/// 這段跟 `cover.wgsl` 的頂點階段是同一套數學。那邊改了這裡就得跟著改 ——
/// 兩份程式碼描述同一件事本來就是壞味道，但跨 CPU／GPU 這道邊界躲不掉。
pub fn cover_corners(d: f32, l: &Flow) -> [[f32; 2]; 4] {
    let (xform, _) = cover::transform(d, l.cover_px, 1.0);
    let (tx, tz, s, rot) = (xform[0], xform[1], xform[2], xform[3]);
    let (cs, sn) = (rot.cos(), rot.sin());
    let half = l.cover_px * 0.5;
    let mut out = [[0.0f32; 2]; 4];
    for (k, (lx, ly)) in
        [(-half, -half), (half, -half), (half, half), (-half, half)].into_iter().enumerate()
    {
        let px = tx + s * lx * cs;
        let py = s * ly;
        let pz = tz - s * lx * sn;
        let pw = (cover::PERSPECTIVE - pz) / cover::PERSPECTIVE;
        out[k] = [l.cx + px / pw, l.cy + py / pw];
    }
    out
}

/// 凸多邊形內側判定：所有邊的外積同號就在裡面。
fn inside(poly: &[[f32; 2]; 4], x: f32, y: f32) -> bool {
    let mut pos = false;
    let mut neg = false;
    for i in 0..4 {
        let a = poly[i];
        let b = poly[(i + 1) % 4];
        let cross = (b[0] - a[0]) * (y - a[1]) - (b[1] - a[1]) * (x - a[0]);
        if cross > 0.0 {
            pos = true;
        } else if cross < 0.0 {
            neg = true;
        }
        if pos && neg {
            return false;
        }
    }
    true
}

pub fn hit_flow(l: &Flow, n: usize, centre: f32, mx: f32, my: f32) -> Option<Hit> {
    for (r, h) in [
        (l.play, Hit::PlayPause),
        (l.prev, Hit::Prev),
        (l.next, Hit::Next),
        (l.settings, Hit::Settings),
        (l.repeat, Hit::Repeat),
        (l.shuffle, Hit::Shuffle),
        (l.like, Hit::Like),
        (l.devices, Hit::Devices),
        (l.mute, Hit::Mute),
    ] {
        if r.contains(mx, my) {
            return Some(h);
        }
    }
    if l.volume.grow(4.0).contains(mx, my) {
        return Some(Hit::Volume(slider_t(&l.volume, mx)));
    }
    if l.progress.grow(10.0).contains(mx, my) {
        return Some(Hit::Seek(((mx - l.progress.x) / l.progress.w).clamp(0.0, 1.0)));
    }
    // 左段整塊都可以點，不只那張小封面
    if l.bar_now.contains(mx, my) {
        return Some(Hit::OpenLyrics);
    }
    // 控制列是實心的，點在上面就不要穿透到後面的封面
    if l.bar.contains(mx, my) {
        return None;
    }

    // 由前往後找：離中央近的畫在上面，所以先試它
    let idx = centre.round() as i32;
    let mut order: Vec<i32> = (-SPAN..=SPAN).collect();
    order.sort_by_key(|o| o.abs());
    for off in order {
        let i = idx + off;
        if i < 0 || i >= n as i32 {
            continue;
        }
        let poly = cover_corners(i as f32 - centre, l);
        if inside(&poly, mx, my) {
            return Some(if i == idx { Hit::PlayCurrent } else { Hit::Cover(i as usize) });
        }
    }
    None
}

/// 跟 `scene_flow` 畫的範圍一致。
const SPAN: i32 = 7;

// ---------------------------------------------------------------- 歌詞頁

/// 歌詞頁**沒有浮動控制列** —— 傳輸鍵在左欄，接在時間下面
/// （CSS：「歌詞頁：整條往下收出畫面，傳輸鍵改由 `#np-controls` 接手」）。
/// 播放鍵是白色實心圓，比旁邊那幾顆大一圈。
pub struct NowL {
    pub art: Rect,
    /// 進度條畫出來只有 4px 高，命中區要放大到能點
    pub progress: Rect,
    pub title_y: f32,
    pub artist_y: f32,
    pub album_y: f32,
    pub times_y: f32,
    pub shuffle: Rect,
    pub prev: Rect,
    pub play: Rect,
    pub next: Rect,
    pub repeat: Rect,
    /// 第二排：愛心、裝置、喇叭＋音量、設定（web 版的 `#np-tools`）
    pub like: Rect,
    pub devices: Rect,
    pub mute: Rect,
    pub volume: Rect,
    pub settings: Rect,
    /// 右欄：歌詞的左緣、寬度、當前句的中線
    pub lyrics_x: f32,
    pub lyrics_w: f32,
    pub lyrics_mid: f32,
    /// 沒有歌詞時的說明（`.lyric-empty`）：標題、副標、「前往 LRCLIB」鈕
    pub empty_title: f32,
    pub empty_title_y: f32,
    pub empty_sub: f32,
    pub empty_sub_y: f32,
    pub lrclib: Rect,
}

/// 左欄照 web 版的比例排：封面、22px、曲名／演出者／專輯、22px、進度條、時間。
///
/// **整欄是垂直置中的，不是「封面置中再往下長」** —— 我原本寫成後者，
/// 專輯名那行就會壓在進度條上。行高要算進去，`ay` 才會對。
pub fn now(fw: f32, fh: f32) -> NowL {
    let art_px = (fh * 0.42).min(fw * 0.26);
    let ax = fw * 0.09;
    // 垂直方向照 CSS：`#np-left` 是 flex column、gap 22，再加上各段自己的 margin。
    // 曲名 30px 行高 1.25；演出者 17px、上距 8；專輯 13.5px、上距 4（行高 normal ≈ 1.33）。
    // 進度條 3px；時間 11.5px、margin-top -14（＝離進度條 8）；
    // 傳輸鍵 margin-top -6（＝離時間 16）；工具列 margin-top -10（＝離傳輸鍵 12）
    const GAP: f32 = 22.0;
    let text_h = 37.5 + 8.0 + 23.0 + 4.0 + 18.0; // 曲名＋演出者＋專輯
    const TIMES_H: f32 = 15.0;
    // 傳輸鍵與工具列兩排也算進去，整欄才會真的置中
    let column = art_px + GAP + text_h + GAP + 3.0 + 8.0 + TIMES_H + 16.0 + 54.0 + 12.0 + 40.0;
    let ay = ((fh - column) * 0.5).max(24.0);
    let title_y = ay + art_px + GAP;
    let artist_y = title_y + 37.5 + 8.0;
    let album_y = artist_y + 23.0 + 4.0;
    let progress_y = album_y + 18.0 + GAP;
    // 傳輸鍵那一排：40 / 40 / 54（播放鍵，左右各留 6）/ 40 / 40，間距 10
    const B: f32 = 40.0;
    const BIG: f32 = 54.0;
    let row_w = B * 4.0 + 10.0 * 4.0 + BIG + 12.0;
    let row_cx = ax + art_px * 0.5;
    let row_y = progress_y + 3.0 + 8.0 + TIMES_H + 16.0 + BIG * 0.5;
    let x0 = row_cx - row_w * 0.5;
    // 工具列拉滿整欄：愛心、裝置靠左，齒輪貼右緣，音量滑桿吃掉中間（`.vol { flex: 1 }`）。
    // 間距 6；喇叭與滑桿之間 2＋2，滑桿右邊留 8
    let tools_y = row_y + BIG * 0.5 + 12.0 + B * 0.5;
    let t0 = ax;
    let vol_x = t0 + B * 3.0 + 6.0 * 2.0 + 2.0 + 2.0;
    let vol_r = ax + art_px - B - 6.0 - 8.0;

    // 沒有歌詞時的說明，數字照 CSS 的 `.lyric-empty`：標題 clamp(20px,1.8vw,28px)、
    // 副標 clamp(14px,1.15vw,18px) 上距 10、按鈕 12px 字＋8/14 留白、上距 18。
    // 整塊以歌詞的中線垂直置中。
    let lyrics_mid = fh * 0.46;
    let empty_title = (fw * 0.018).clamp(20.0, 28.0);
    let empty_sub = (fw * 0.0115).clamp(14.0, 18.0);
    const BTN_W: f32 = 96.0;
    const BTN_H: f32 = 31.0;
    let block = empty_title * 1.5 + 10.0 + empty_sub * 1.6 + 18.0 + BTN_H;
    let empty_title_y = lyrics_mid - block * 0.5;
    let empty_sub_y = empty_title_y + empty_title * 1.5 + 10.0;
    // 歌詞接在左欄後面，不是固定在寬度的 44%：封面是照高度縮的，
    // 超寬螢幕（3440x1377）上固定比例會讓中間空出 600 多 px，歌詞看起來擠到右邊。
    // 間距照 CSS 的 gap 5vw，但不超過高度的 15%（一般 16:10、16:9 跟原本差不多）。
    let lyrics_x = (ax + art_px + (fw * 0.09).min(fh * 0.15)).min(fw * 0.44);
    NowL {
        lyrics_x,
        lyrics_w: fw * 0.92 - lyrics_x,
        lyrics_mid,
        empty_title,
        empty_title_y,
        empty_sub,
        empty_sub_y,
        lrclib: Rect::new(lyrics_x, empty_sub_y + empty_sub * 1.6 + 18.0, BTN_W, BTN_H),
        art: Rect::new(ax, ay, art_px, art_px),
        progress: Rect::new(ax, progress_y, art_px, 3.0),
        title_y,
        artist_y,
        album_y,
        times_y: progress_y + 3.0 + 8.0,
        shuffle: Rect::centred(x0 + B * 0.5, row_y, B, B),
        prev: Rect::centred(x0 + B * 1.5 + 10.0, row_y, B, B),
        play: Rect::centred(x0 + B * 2.0 + 26.0 + BIG * 0.5, row_y, BIG, BIG),
        next: Rect::centred(x0 + row_w - B * 1.5 - 10.0, row_y, B, B),
        repeat: Rect::centred(x0 + row_w - B * 0.5, row_y, B, B),
        like: Rect::centred(t0 + B * 0.5, tools_y, B, B),
        devices: Rect::centred(t0 + B * 1.5 + 6.0, tools_y, B, B),
        mute: Rect::centred(t0 + B * 2.5 + 12.0, tools_y, B, B),
        volume: Rect::new(vol_x, tools_y - 7.0, (vol_r - vol_x).max(40.0), 14.0),
        settings: Rect::centred(ax + art_px - B * 0.5, tools_y, B, B),
    }
}

/// `no_lyrics`：右欄正在顯示「不知道這首的歌詞」，那顆「前往 LRCLIB」才存在。
pub fn hit_now(l: &NowL, _bar_alpha: f32, no_lyrics: bool, mx: f32, my: f32) -> Option<Hit> {
    if no_lyrics && l.lrclib.contains(mx, my) {
        return Some(Hit::OpenLrclib);
    }
    // 這一排一直都在（它不是會收起來的浮動列），所以不吃 bar_alpha
    for (r, h) in [
        (l.play, Hit::PlayPause),
        (l.prev, Hit::Prev),
        (l.next, Hit::Next),
        (l.shuffle, Hit::Shuffle),
        (l.repeat, Hit::Repeat),
        (l.like, Hit::Like),
        (l.devices, Hit::Devices),
        (l.mute, Hit::Mute),
        (l.settings, Hit::Settings),
    ] {
        if r.contains(mx, my) {
            return Some(h);
        }
    }
    if l.volume.grow(4.0).contains(mx, my) {
        return Some(Hit::Volume(slider_t(&l.volume, mx)));
    }
    let hot = l.progress.grow(11.0);
    if hot.contains(mx, my) {
        return Some(Hit::Seek(((mx - l.progress.x) / l.progress.w).clamp(0.0, 1.0)));
    }
    if l.art.contains(mx, my) {
        return Some(Hit::PlayPause);
    }
    None
}

// ---------------------------------------------------------------- 視覺效果頁

pub struct VizL {
    pub bar: Rect,
    pub prev: Rect,
    pub next: Rect,
    pub fav: Rect,
    pub cat: Rect,
    /// 中間那兩行字可用的寬度
    pub text_w: f32,
}

pub fn viz(fw: f32, fh: f32) -> VizL {
    let bar_w = (fw * 0.62).min(680.0);
    let bar = Rect::new((fw - bar_w) * 0.5, fh - 104.0, bar_w, 72.0);
    let by = bar.cy();
    let right = bar.x + bar.w;
    VizL {
        bar,
        prev: Rect::centred(bar.x + 34.0, by, BTN, BTN),
        next: Rect::centred(bar.x + 78.0, by, BTN, BTN),
        fav: Rect::centred(right - 78.0, by, BTN, BTN),
        cat: Rect::centred(right - 34.0, by, BTN, BTN),
        // 文字要讓開兩邊的按鈕
        text_w: bar_w - 220.0,
    }
}

pub fn hit_viz(l: &VizL, bar_alpha: f32, mx: f32, my: f32) -> Option<Hit> {
    if bar_alpha <= 0.25 {
        return None;
    }
    if l.prev.contains(mx, my) {
        return Some(Hit::PresetPrev);
    }
    if l.next.contains(mx, my) {
        return Some(Hit::PresetNext);
    }
    if l.fav.contains(mx, my) {
        return Some(Hit::Favourite);
    }
    if l.cat.contains(mx, my) {
        return Some(Hit::Category);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 選單的列點得到而且星號跟列分得開() {
        let l = picker(1280.0, 800.0);
        let row = picker_row(&l, 5, 7).unwrap();
        assert_eq!(hit_picker(&l, 5, 100, row.cx(), row.cy()), Some(Hit::PickerRow(7)));
        let star = picker_star(&row);
        assert_eq!(hit_picker(&l, 5, 100, star.cx(), star.cy()), Some(Hit::PickerStar(7)));
        // 捲到看不見的列沒有命中框
        assert!(picker_row(&l, 5, 4).is_none());
        // 清單比畫面短的時候，空白處點下去不能選到不存在的列
        let last = picker_row(&l, 0, l.rows - 1).unwrap();
        assert_eq!(hit_picker(&l, 0, 2, last.cx(), last.cy()), None);
        assert_eq!(hit_picker(&l, 0, 2, 3.0, 3.0), Some(Hit::PickerClose));
        let c = l.cats[4];
        assert_eq!(hit_picker(&l, 0, 2, c.cx(), c.cy()), Some(Hit::PickerCat(4)));
    }

    #[test]
    fn 選單捲動只做最小移動() {
        // 10 列的視窗、200 個項目
        assert_eq!(picker_follow(0, 3, 10, 200), 0);
        assert_eq!(picker_follow(0, 10, 10, 200), 1);
        assert_eq!(picker_follow(50, 49, 10, 200), 49);
        // 不能捲過頭
        assert_eq!(picker_follow(0, 199, 10, 200), 190);
        assert_eq!(picker_centre(100, 10, 200), 95);
        assert_eq!(picker_centre(2, 10, 200), 0);
        assert_eq!(picker_centre(199, 10, 5), 0);
    }

    // 1280x800，跟視窗預設大小一樣
    fn l() -> Flow {
        flow(1280.0, 800.0, 1.0)
    }

    #[test]
    fn 中央封面正中間點得到() {
        let f = l();
        assert_eq!(hit_flow(&f, 12, 3.0, f.cx, f.cy), Some(Hit::PlayCurrent));
    }

    #[test]
    fn 左邊那張是前一個索引() {
        let f = l();
        // 中央封面左緣再往左一點，落在第一張側面封面上
        let x = f.cx - f.cover_px * 0.5 - 20.0;
        assert_eq!(hit_flow(&f, 12, 3.0, x, f.cy), Some(Hit::Cover(2)));
    }

    #[test]
    fn 右邊那張是後一個索引() {
        let f = l();
        let x = f.cx + f.cover_px * 0.5 + 20.0;
        assert_eq!(hit_flow(&f, 12, 3.0, x, f.cy), Some(Hit::Cover(4)));
    }

    #[test]
    fn 邊界外面沒有東西() {
        let f = l();
        assert_eq!(hit_flow(&f, 12, 3.0, 4.0, 4.0), None);
    }

    #[test]
    fn 清單頭尾不會超出範圍() {
        let f = l();
        // centre=0 時左邊沒有封面了，點過去不該回傳負的索引
        let x = f.cx - f.cover_px * 0.8;
        assert!(!matches!(hit_flow(&f, 12, 0.0, x, f.cy), Some(Hit::Cover(_))));
    }

    #[test]
    fn 控制列擋住後面的封面() {
        let f = l();
        // 右段裡沒有按鈕的地方：擋住，但也不該觸發任何動作
        assert_eq!(hit_flow(&f, 12, 3.0, f.bar_extra.x + 2.0, f.bar_extra.y + 2.0), None);
        assert_eq!(hit_flow(&f, 12, 3.0, f.play.cx(), f.play.cy()), Some(Hit::PlayPause));
        // 左段整塊都是「去歌詞頁」
        assert_eq!(
            hit_flow(&f, 12, 3.0, f.bar_now.cx(), f.bar_now.cy()),
            Some(Hit::OpenLyrics)
        );
    }

    #[test]
    fn 進度條點到哪就跳到哪() {
        let n = now(1280.0, 800.0);
        let x = n.progress.x + n.progress.w * 0.25;
        match hit_now(&n, 1.0, false, x, n.progress.cy()) {
            Some(Hit::Seek(t)) => assert!((t - 0.25).abs() < 0.01, "t={t}"),
            other => panic!("應該是 Seek，拿到 {other:?}"),
        }
    }

    #[test]
    fn 歌詞頁的傳輸鍵一直都在() {
        // 這一排不是會收起來的浮動列（那是封面流那條），所以不吃 bar_alpha
        let n = now(1280.0, 800.0);
        assert_eq!(hit_now(&n, 0.0, false, n.play.cx(), n.play.cy()), Some(Hit::PlayPause));
        assert_eq!(hit_now(&n, 1.0, false, n.play.cx(), n.play.cy()), Some(Hit::PlayPause));
    }

    #[test]
    fn 前往_lrclib_只在沒歌詞時存在() {
        let n = now(1280.0, 800.0);
        let (x, y) = (n.lrclib.cx(), n.lrclib.cy());
        assert_eq!(hit_now(&n, 1.0, true, x, y), Some(Hit::OpenLrclib));
        assert_eq!(hit_now(&n, 1.0, false, x, y), None);
        // 按鈕在右欄，不會跟左欄的傳輸鍵重疊
        assert!(n.lrclib.x > n.art.x + n.art.w);
        // 播放鍵比旁邊那幾顆大一圈
        assert!(n.play.w > n.prev.w);
    }

    #[test]
    fn 通知列置中而且不會超出畫面() {
        let t = toast(1280.0, 200.0, 700.0);
        assert!((t.cx() - 640.0).abs() < 0.01);
        assert!((t.y + t.h - 700.0).abs() < 0.01, "底邊要貼在 bottom 上");
        // 字太長：外框夾在畫面內，兩側各留 16
        let t = toast(400.0, 2000.0, 300.0);
        assert!(t.x >= 16.0 - 0.01 && t.x + t.w <= 384.0 + 0.01, "{t:?}");
    }

    #[test]
    fn 設定面板不蓋到控制列() {
        for fh in [720.0, 800.0, 1080.0] {
            let l = settings(1280.0, fh, crate::settings::ITEMS.len());
            assert!(l.panel.y + l.panel.h <= fh - 100.0, "fh={fh} 面板底 {}", l.panel.y + l.panel.h);
            // 每一列都要在面板裡（排兩欄時也是）
            for r in &l.rows {
                assert!(r.x >= l.panel.x && r.x + r.w <= l.panel.x + l.panel.w + 0.01, "fh={fh} {r:?}");
                assert!(r.y + r.h <= l.panel.y + l.panel.h - 26.0 + 0.01, "fh={fh} {r:?}");
            }
        }
    }

    #[test]
    fn 設定精靈塞得進矮視窗而且鈕不重疊() {
        let l = setup(1280.0, 720.0);
        assert!(l.panel.y >= 20.0 && l.panel.y + l.panel.h <= 720.0, "{:?}", l.panel);
        assert!(l.redirect.x + l.redirect.w < l.copy.x, "框與「複製」之間要有縫");
        assert!(l.save.x + l.save.w <= l.panel.x + l.panel.w - 20.0);
        assert_eq!(hit_setup(&l, l.save.cx(), l.save.cy()), Some(Hit::SetupSave));
        assert_eq!(hit_setup(&l, l.copy.cx(), l.copy.cy()), Some(Hit::SetupCopy));
        // 面板外面不算關閉
        assert_eq!(hit_setup(&l, 2.0, 2.0), None);
    }

    #[test]
    fn 裝置選單的命中() {
        let l = devices(1280.0, 800.0, 3);
        let r = l.rows[2];
        assert_eq!(hit_devices(&l, 3, r.cx(), r.cy()), Some(Hit::DeviceRow(2)));
        assert_eq!(hit_devices(&l, 3, 2.0, 2.0), Some(Hit::CloseDevices), "面板外面點一下就關");
        // 沒有裝置：留一列放說明，但點了不是選裝置
        let l = devices(1280.0, 800.0, 0);
        assert_eq!(l.rows.len(), 1);
        assert_eq!(hit_devices(&l, 0, l.rows[0].cx(), l.rows[0].cy()), None);
    }

    #[test]
    fn 控制列的新按鈕不重疊而且點得到() {
        let f = flow(1280.0, 800.0, 1.0);
        let btns = [f.repeat, f.shuffle, f.devices, f.mute, f.settings];
        for (i, a) in btns.iter().enumerate() {
            assert!(a.x >= f.bar_extra.x && a.x + a.w <= f.bar_extra.x + f.bar_extra.w + 0.5, "{i} 超出右段");
            for b in &btns[i + 1..] {
                assert!(a.x + a.w <= b.x + 0.01 || b.x + b.w <= a.x + 0.01, "{a:?} {b:?}");
            }
        }
        assert!(f.mute.x + f.mute.w <= f.volume.x + 0.5 && f.volume.x + f.volume.w <= f.settings.x);
        assert_eq!(hit_flow(&f, 12, 3.0, f.like.cx(), f.like.cy()), Some(Hit::Like), "愛心不能被左段吃掉");
        match hit_flow(&f, 12, 3.0, f.volume.x + f.volume.w * 0.75, f.volume.cy()) {
            Some(Hit::Volume(t)) => assert!((t - 0.75).abs() < 0.01),
            other => panic!("{other:?}"),
        }
        let n = now(1280.0, 800.0);
        assert_eq!(hit_now(&n, 1.0, false, n.like.cx(), n.like.cy()), Some(Hit::Like));
        assert_eq!(hit_now(&n, 1.0, false, n.settings.cx(), n.settings.cy()), Some(Hit::Settings));
        assert!(n.like.y > n.play.y + n.play.h * 0.5, "工具列在傳輸鍵下面");
        assert!(n.like.y + n.like.h < 800.0, "工具列塞得進畫面");
    }

    #[test]
    fn 控制列是三段不是一整條() {
        let f = flow(1280.0, 800.0, 1.0);
        assert!(f.bar_now.x + f.bar_now.w < f.bar_main.x, "左段與中段之間要有縫");
        assert!(f.bar_main.x + f.bar_main.w < f.bar_extra.x, "中段與右段之間要有縫");
        // 三段加起來大致鋪滿（左右各留 14 的邊）
        assert!((f.bar_extra.x + f.bar_extra.w - (1280.0 - 14.0)).abs() < 0.5);
    }
}
