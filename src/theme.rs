//! 設計 token：色彩、玻璃材質、圓角、陰影、字級、動畫時長。
//!
//! 第 1 階段的規則：**深色模式的輸出與搬家前逐像素一致**。所以這裡的深色值全部
//! 是從原本寫死在各處的數字原樣搬來的，沒有對齊刻度、沒有修正。淺色值是
//! 推測值，沒有官方數字；第 6 階段起淺色可以用，對比度是拿截圖量過的。
//!
//! 合成一律在 sRGB 數值空間做（`gfx.rs`），目的是對齊網頁版 CSS 的混色。
//! 這裡的 alpha 也都是在那個空間調出來的，不要轉線性。

use std::sync::atomic::{AtomicU8, Ordering};

// ------------------------------------------------------------------ 模式

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Dark,
    Light,
}

/// 使用者在設定裡選的外觀。`Auto` 跟隨 Windows 的「應用程式模式」。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Appearance {
    Auto,
    #[default]
    Dark,
    Light,
}

impl Appearance {
    pub fn parse(s: &str) -> Self {
        match s {
            "auto" => Appearance::Auto,
            "light" => Appearance::Light,
            _ => Appearance::Dark,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Appearance::Auto => "auto",
            Appearance::Dark => "dark",
            Appearance::Light => "light",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Appearance::Auto => "跟隨系統",
            Appearance::Dark => "深色",
            Appearance::Light => "淺色",
        }
    }
    pub fn next(self, d: i32) -> Self {
        const ORDER: [Appearance; 3] = [Appearance::Dark, Appearance::Light, Appearance::Auto];
        let i = ORDER.iter().position(|a| *a == self).unwrap_or(0) as i32;
        ORDER[(i + d).rem_euclid(ORDER.len() as i32) as usize]
    }
    /// 換算成實際要用的模式。`Auto` 讀登錄檔，讀不到就當深色。
    pub fn resolve(self) -> Mode {
        match self {
            Appearance::Dark => Mode::Dark,
            Appearance::Light => Mode::Light,
            Appearance::Auto => system_mode().unwrap_or(Mode::Dark),
        }
    }
    /// 系統標題列用的主題。跟隨系統時交給 Windows（`None`）：winit 只有在
    /// 沒指定主題時才會在系統切換深淺色後送 `ThemeChanged`。
    pub fn title_bar(self) -> Option<winit::window::Theme> {
        match self {
            Appearance::Auto => None,
            Appearance::Dark => Some(winit::window::Theme::Dark),
            Appearance::Light => Some(winit::window::Theme::Light),
        }
    }
}

/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme`
/// （0 = 深色、1 = 淺色）。Win10 1903 以後才有；沒有這個值就回 `None`。
#[cfg(windows)]
pub fn system_mode() -> Option<Mode> {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    use windows::core::w;
    let mut data = 0u32;
    let mut len = std::mem::size_of::<u32>() as u32;
    // SAFETY: data 與 len 都是這裡的區域變數，len 如實標示 data 的大小。
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut len),
        )
    };
    r.is_ok().then_some(if data == 0 { Mode::Dark } else { Mode::Light })
}

#[cfg(not(windows))]
pub fn system_mode() -> Option<Mode> {
    None
}

static MODE: AtomicU8 = AtomicU8::new(0);

pub fn set_mode(m: Mode) {
    MODE.store(if m == Mode::Light { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn mode() -> Mode {
    if MODE.load(Ordering::Relaxed) == 1 { Mode::Light } else { Mode::Dark }
}

// ------------------------------------------------------------------ 調色盤

pub struct Palette {
    /// CSS `--fg` #ecedf2：主要文字
    pub fg: [f32; 4],
    /// `.barbtn` 平常的 #c2c3cc：次要文字、按鈕
    pub soft: [f32; 4],
    /// `--dim` #8a8b96：弱化文字
    pub dim: [f32; 4],
    /// `--accent` #1db954：Spotify 綠
    pub accent: [f32; 4],
    /// 綠底上的字（CSS `#07110b`）
    pub on_accent: [f32; 4],
    /// 「墨色」：文字、分隔線、hover 底、進度條軌道都是它加不同 alpha。
    /// 深色是白，淺色是近黑。
    pub ink: [f32; 3],
    /// 暗角、陰影、遮罩用的黑
    pub scrim: [f32; 3],
    /// 玻璃面板的色調（`GlassStyle` 只給 alpha）
    pub glass: [f32; 3],
    /// 選單那種幾乎不透明的底（preset 選單）
    pub sheet: [f32; 3],
    /// 開關與滑桿的旋鈕
    pub knob: [f32; 3],
    /// 狀態色。深色值都是從各處寫死的數字原樣搬來的：
    /// `ok` 是 #1ed760（設定精靈的主要鈕、「· 已鎖定」），
    /// `warn` 是 preset 選單的星號金，`danger` 是「中斷連結」鈕的紅底，
    /// `danger_fg` 是設定精靈的錯誤字，`link` 是關於面板的連結藍。
    pub ok: [f32; 3],
    pub warn: [f32; 3],
    pub danger: [f32; 3],
    pub danger_fg: [f32; 3],
    pub link: [f32; 3],
    /// 設定視窗的底（它沒有專輯圖可以透，是一般的不透明視窗）
    pub window: [f32; 3],
    /// `on_ink`：墨色實心底上的圖示
    pub on_ink: [f32; 4],
    /// `modal`：暗幕濃度的倍率
    pub modal_k: f32,
    /// `field`：輸入框底的顏色與濃度倍率
    pub field: [f32; 3],
    pub field_k: f32,
    /// `halo`：字底下光暈的顏色與濃度倍率
    pub halo: [f32; 3],
    pub halo_k: f32,
    /// 主視窗的背景怎麼處理
    pub backdrop: Backdrop,
}

/// 主視窗背景：中央封面鋪滿、模糊之後怎麼調。深色是壓暗＋兩層黑紗，
/// 淺色是補飽和度＋洗白＋兩層白紗。`wash` 的 alpha 為 0 就不畫那一層
#[derive(Clone, Copy, Debug)]
pub struct Backdrop {
    /// 封面流：模糊背景乘的亮度、補的飽和度、上面再蓋的一層
    pub flow_mul: f32,
    pub flow_sat: f32,
    pub flow_wash: [f32; 4],
    /// 歌詞頁：同上
    pub now_mul: f32,
    pub now_sat: f32,
    pub now_wash: [f32; 4],
    /// 紗的顏色（sRGB 0..255）。深色模式照 CSS：暈影是純黑、直向漸層是 #08080b
    pub veil_radial_rgb: [u8; 3],
    pub veil_vert_rgb: [u8; 3],
    /// 暈影四周的濃度（中間透明，0.7 處到這個值）
    pub veil_radial_a: f32,
    /// 直向漸層的三個停點：頂端、45% 處、底部
    pub veil_vert_a: [f32; 3],
    /// 沒有封面（例如搜尋篩不到東西）時背景的底色
    pub empty: [f32; 3],
}

pub const DARK: Palette = Palette {
    fg: [0.925, 0.929, 0.949, 1.0],
    soft: [0.761, 0.765, 0.8, 1.0],
    dim: [0.541, 0.545, 0.588, 1.0],
    accent: [0.114, 0.725, 0.329, 1.0],
    on_accent: [0.027, 0.067, 0.043, 1.0],
    ink: [1.0, 1.0, 1.0],
    scrim: [0.0, 0.0, 0.0],
    glass: [1.0, 1.0, 1.0],
    sheet: [11.0 / 255.0, 11.0 / 255.0, 15.0 / 255.0],
    knob: [1.0, 1.0, 1.0],
    ok: [0.118, 0.843, 0.376],
    warn: [1.0, 0.82, 0.4],
    danger: [0.86, 0.22, 0.24],
    danger_fg: [1.0, 0.55, 0.5],
    link: [0.62, 0.86, 1.0],
    window: [0.106, 0.106, 0.118],
    on_ink: [0.043, 0.043, 0.059, 1.0],
    modal_k: 1.0,
    field: [0.0, 0.0, 0.0],
    field_k: 1.0,
    halo: [0.0, 0.0, 0.0],
    halo_k: 1.0,
    // 照 CSS 的 `#backdrop-img`（brightness .55 × opacity .85、saturate 1.7）與
    // `#backdrop-veil`；歌詞頁另外壓得更暗
    backdrop: Backdrop {
        flow_mul: 0.47,
        flow_sat: 1.7,
        flow_wash: [0.0; 4],
        now_mul: 0.38,
        now_sat: 1.0,
        now_wash: [0.02, 0.02, 0.04, 0.58],
        veil_radial_rgb: [0, 0, 0],
        veil_vert_rgb: [8, 8, 11],
        veil_radial_a: 0.55,
        veil_vert_a: [0.35, 0.10, 0.95],
        empty: [0.0, 0.0, 0.0],
    },
};

/// **全部是推測值**，沒有官方或實機驗證。
pub const LIGHT: Palette = Palette {
    fg: [0.110, 0.114, 0.133, 1.0],
    soft: [0.267, 0.275, 0.322, 1.0],
    dim: [0.420, 0.427, 0.478, 1.0],
    accent: [0.082, 0.600, 0.267, 1.0],
    on_accent: [1.0, 1.0, 1.0, 1.0],
    ink: [0.043, 0.047, 0.067],
    scrim: [0.0, 0.0, 0.0],
    glass: [1.0, 1.0, 1.0],
    sheet: [0.953, 0.957, 0.969],
    knob: [1.0, 1.0, 1.0],
    ok: [0.082, 0.600, 0.267],
    warn: [0.780, 0.540, 0.0],
    danger: [0.800, 0.160, 0.180],
    danger_fg: [0.760, 0.180, 0.160],
    link: [0.0, 0.400, 0.800],
    window: [0.945, 0.945, 0.953],
    on_ink: [0.97, 0.97, 0.98, 1.0],
    modal_k: 0.45,
    field: [0.043, 0.047, 0.067],
    field_k: 0.16,
    halo: [0.97, 0.97, 0.98],
    halo_k: 1.2,
    backdrop: Backdrop {
        flow_mul: 1.0,
        flow_sat: 1.5,
        flow_wash: [0.97, 0.97, 0.98, 0.42],
        now_mul: 1.0,
        now_sat: 1.3,
        now_wash: [0.97, 0.97, 0.98, 0.55],
        veil_radial_rgb: [246, 246, 249],
        veil_vert_rgb: [246, 246, 249],
        veil_radial_a: 0.40,
        veil_vert_a: [0.30, 0.04, 0.80],
        empty: [0.90, 0.90, 0.92],
    },
};

pub fn backdrop() -> &'static Backdrop {
    &palette().backdrop
}

pub fn palette() -> &'static Palette {
    match mode() {
        Mode::Dark => &DARK,
        Mode::Light => &LIGHT,
    }
}

fn opposite() -> &'static Palette {
    match mode() {
        Mode::Dark => &LIGHT,
        Mode::Light => &DARK,
    }
}

// ------------------------------------------------------------------ 玻璃上的明暗
//
// 小玻璃壓在很亮的東西上時會整片翻成淺色，上面的字與符號跟著翻成深色
// （Liquid Glass 的自動明暗）。翻的程度由 `Glass::tone` 依背景決定，畫那塊
// 玻璃上的字之前用 `scoped_tone` 開一段，期間 `ink`／`fg`／`soft`／`dim`
// 往另一套調色盤靠。0 = 照外觀模式本來的顏色，位元組跟沒開一樣。

thread_local! {
    static TONE: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

/// `scoped_tone` 的回傳值，離開範圍時還原
pub struct ToneGuard(f32);

impl Drop for ToneGuard {
    fn drop(&mut self) {
        TONE.set(self.0);
    }
}

/// 接下來畫的字往另一套調色盤翻多少（0..1），直到回傳值被丟掉
#[must_use]
pub fn scoped_tone(t: f32) -> ToneGuard {
    ToneGuard(TONE.replace(t.clamp(0.0, 1.0)))
}

fn toned(c: [f32; 4], other: [f32; 4]) -> [f32; 4] {
    let t = TONE.get();
    if t <= 0.0 {
        return c;
    }
    std::array::from_fn(|i| c[i] + (other[i] - c[i]) * t)
}

pub fn fg() -> [f32; 4] {
    toned(palette().fg, opposite().fg)
}
pub fn soft() -> [f32; 4] {
    toned(palette().soft, opposite().soft)
}
pub fn dim() -> [f32; 4] {
    toned(palette().dim, opposite().dim)
}
/// 強調色。有設 `set_accent`（跟著封面）就用那個，沒有就是調色盤的 Spotify 綠。
pub fn accent() -> [f32; 4] {
    match ACCENT.get() {
        Some(c) => rgba(c, 1.0),
        None => palette().accent,
    }
}
/// 強調色底上的字
pub fn on_accent() -> [f32; 4] {
    match ACCENT.get() {
        Some(c) => rgba(on_accent_for(c), 1.0),
        None => palette().on_accent,
    }
}

// ------------------------------------------------------------------ 跟著封面的強調色
//
// 封面的主色不能直接拿來用：黃色看起來比藍色亮得多，暗封面取到的顏色在玻璃上
// 會糊掉。所以只留色相，在 OKLCH（感知均勻）裡把亮度釘死、彩度限制在一個範圍，
// 每首顏色不同但看起來一樣亮。釘死的亮度跟 Spotify 綠（L 0.69）差不多，
// 綠底配深字的對比因此每種顏色都成立。

/// 深色／淺色模式下強調色的 OKLCH 亮度
pub const ACCENT_L: [f32; 2] = [0.70, 0.56];
/// 黃色在 0.70 會變芥末色，黃那一段（色相 95° 左右，寬 30°）往上補這麼多
pub const ACCENT_L_YELLOW: f32 = 0.10;
/// 彩度範圍：封面越灰越靠下限。超出 sRGB 的話再往下收
/// 上限對齊 Spotify 綠的 0.19，飽和的封面才不會比預設的綠還淡
pub const ACCENT_C: [f32; 2] = [0.07, 0.19];

thread_local! {
    static ACCENT: std::cell::Cell<Option<[f32; 3]>> = const { std::cell::Cell::new(None) };
}

/// 換掉強調色。`None` = 回到調色盤的綠。只影響呼叫這個的執行緒（畫面都在主執行緒畫）。
pub fn set_accent(c: Option<[f32; 3]>) {
    ACCENT.set(c);
}

/// 封面的代表色（任何亮度）與它的飽和度 0..1 → 強調色（sRGB 0..1）
pub fn accent_from(rgb: [f32; 3], sat: f32) -> [f32; 3] {
    let [_, _, h] = to_oklch(rgb);
    let dh = ((h.to_degrees() - 95.0 + 540.0) % 360.0 - 180.0) / 30.0;
    let l = ACCENT_L[(mode() == Mode::Light) as usize] + ACCENT_L_YELLOW * (-dh * dh).exp();
    let k = ((sat - 0.2) / 0.5).clamp(0.0, 1.0);
    let mut c = ACCENT_C[0] + (ACCENT_C[1] - ACCENT_C[0]) * k;
    // 藍紫在這個亮度裝不下高彩度，收到 sRGB 裝得下為止
    loop {
        let out = from_oklch(l, c, h);
        if out.iter().all(|v| (-0.001..=1.001).contains(v)) || c <= 0.0 {
            return out.map(|v| v.clamp(0.0, 1.0));
        }
        c -= 0.005;
    }
}

/// 強調色「指著」的那一階：綠的時候是原本的 #1ed760（`ok`），換了色就往亮推一點
pub fn accent_hot() -> [f32; 4] {
    match ACCENT.get() {
        Some(c) => {
            let [l, ch, h] = to_oklch(c);
            rgba(from_oklch(l + 0.07, ch, h).map(|v| v.clamp(0.0, 1.0)), 1.0)
        }
        None => ok(1.0),
    }
}

/// 強調色底上的字：深色模式是同色相的近黑（綠的時候就是原本的 #07110b 那一類），
/// 淺色模式是白
fn on_accent_for(c: [f32; 3]) -> [f32; 3] {
    if mode() == Mode::Light {
        return [1.0, 1.0, 1.0];
    }
    let [_, ch, h] = to_oklch(c);
    from_oklch(0.18, (ch * 0.25).min(0.03), h).map(|v| v.clamp(0.0, 1.0))
}

fn lin(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn unlin(v: f32) -> f32 {
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055 }
}

/// sRGB → OKLCH（L、C、色相弧度）。Björn Ottosson 的矩陣
pub fn to_oklch(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb.map(lin);
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    let ll = 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s;
    let a = 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s;
    let bb = 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s;
    [ll, a.hypot(bb), bb.atan2(a)]
}

/// OKLCH → sRGB。不裁切，超出 0..1 代表在 sRGB 外面
pub fn from_oklch(l: f32, c: f32, h: f32) -> [f32; 3] {
    let (a, b) = (c * h.cos(), c * h.sin());
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    [
        4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_,
        -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_,
        -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_,
    ]
    .map(unlin)
}

/// 墨色加 alpha。深色模式 = `[1, 1, 1, a]`。
pub fn ink(a: f32) -> [f32; 4] {
    toned(rgba(palette().ink, a), rgba(opposite().ink, a))
}

/// 黑色加 alpha（陰影、遮罩）。
pub fn scrim(a: f32) -> [f32; 4] {
    let c = palette().scrim;
    [c[0], c[1], c[2], a]
}

/// 墨色實心底（播放鍵的圓）上面的圖示色：深色模式是近黑，淺色模式是近白
pub fn on_ink() -> [f32; 4] {
    toned(palette().on_ink, opposite().on_ink)
}

/// 舊程式裡寫死、跟 token 差一點點的深色值：深色模式照舊（截圖才會逐位元組相同），
/// 淺色模式用給的 token。跟著 `scoped_tone` 一起翻
pub fn pick(dark: [f32; 4], light: [f32; 4]) -> [f32; 4] {
    let (c, o) = if mode() == Mode::Light { (light, dark) } else { (dark, light) };
    toned(c, o)
}

/// 貼圖的乘色：原色加 alpha（封面、背景圖、轉場的整層圖）。
/// 跟 `ink` 不同，淺色模式也是白 —— 用 ink 的話淺色時貼圖會被乘成黑的
pub fn tex(a: f32) -> [f32; 4] {
    [1.0, 1.0, 1.0, a]
}

/// 大面板打開時蓋住整個畫面的暗幕。`a` 是深色模式的濃度，淺色模式打折
/// （淺色背景上同樣的黑會變成一片髒灰）
pub fn modal(a: f32) -> [f32; 4] {
    scrim(a * palette().modal_k)
}

/// 輸入框、搜尋欄這類凹下去的底。`a` 是深色模式黑底的濃度，淺色模式改用淡墨色
pub fn field(a: f32) -> [f32; 4] {
    let p = palette();
    rgba(p.field, a * p.field_k)
}

/// 壓在封面上的字底下墊的光暈：深色是黑影，淺色是白霧
pub fn halo(a: f32) -> [f32; 4] {
    let p = palette();
    rgba(p.halo, a * p.halo_k)
}

/// 接下來畫的字與墨色一律用深色調色盤，直到回傳值被丟掉。
/// 給「不管外觀模式都是深色」的地方用：迷你視窗、clear 玻璃上的字（clear 不自適應，
/// 後面墊的是變暗層，字永遠是亮的）。深色模式什麼都不改
#[must_use]
pub fn scoped_dark() -> ToneGuard {
    ToneGuard(TONE.replace(dark_tone().max(TONE.get())))
}

/// 「固定用深色那一套」換算成 `Panel::tone`／`scoped_tone` 的值：深色模式 0、淺色模式 1。
/// clear 玻璃用：它不依背景翻，後面墊變暗層、上面永遠是亮字
pub fn dark_tone() -> f32 {
    if mode() == Mode::Light { 1.0 } else { 0.0 }
}

fn rgba(c: [f32; 3], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}

pub fn knob(a: f32) -> [f32; 4] {
    rgba(palette().knob, a)
}
pub fn ok(a: f32) -> [f32; 4] {
    rgba(palette().ok, a)
}
pub fn warn(a: f32) -> [f32; 4] {
    rgba(palette().warn, a)
}
pub fn danger(a: f32) -> [f32; 4] {
    rgba(palette().danger, a)
}
pub fn danger_fg(a: f32) -> [f32; 4] {
    rgba(palette().danger_fg, a)
}
pub fn link(a: f32) -> [f32; 4] {
    rgba(palette().link, a)
}
pub fn window_bg() -> [f32; 4] {
    rgba(palette().window, 1.0)
}

// ------------------------------------------------------------------ 玻璃材質

/// 一組玻璃預設：模糊 σ、色調 alpha、邊緣折射四個參數，加上用樣式表的哪一列。
///
/// `edge` = (折射帶寬 px, 折射強度 px, 亮環寬 px, 折射帶清晰度)。
#[derive(Clone, Copy, Debug)]
pub struct GlassStyle {
    pub sigma: f32,
    pub tint_a: f32,
    pub edge: (f32, f32, f32, f32),
    /// `glass_looks()` 的索引
    pub look: u32,
}

/// 玻璃著色器的光學參數，一列就是一種材質（`glass.wgsl` 的 `Look`）。
///
/// 長度單位是邏輯像素。「小」「大」指短邊落在 `size_range` 的哪一端，
/// 中間平滑內插：元件越大，折射越明顯、投影越深越散、光越柔。
#[derive(Clone, Copy, Debug)]
pub struct GlassLook {
    /// 遠的那層投影，小元件與大元件各一組：(往下 px, 模糊 px, alpha)
    pub shadow_small: (f32, f32, f32),
    pub shadow_large: (f32, f32, f32),
    /// 貼著元件的那層。兩層的合成是 1-(1-a1)(1-a2)，不是相加
    pub shadow_near: (f32, f32, f32),
    /// 四邊留給投影的空間。要大於 往下 + 模糊，否則投影會被切掉
    pub shadow_pad: f32,
    /// 短邊多少 px 以下算小、多少以上算大
    pub size_range: (f32, f32),
    /// 折射帶寬與折射強度在小、大兩端的倍率（乘在 `GlassStyle::edge` 上）
    pub refract_scale: (f32, f32),
    /// 本體取樣模糊圖時跟著折射往內拉的比例
    pub blur_pull: f32,
    /// 色散：R、B 相對 G 多彎或少彎的比例
    pub dispersion: f32,
    /// 背景的飽和度與亮度倍率
    pub saturate: f32,
    pub brightness: f32,
    /// 倒角斜率的上限。四分之一圓在最外圈斜率會趨近無限
    pub slope_cap: f32,
    /// 由清晰漸漸變糊的那段有多寬，折射帶寬的倍數。太窄會像一圈透明框接一片霧
    pub clear_reach: f32,
    /// 同上，但不超過這麼多 px。大面板的折射帶寬，乘上去會一路透進內容區
    pub clear_max: f32,
    /// 邊緣比中央清晰多少。0 = 整片同樣的模糊、折射只彎那張模糊圖（iOS 的做法）；
    /// 1 = 邊緣用清晰圖，往內漸漸變糊
    pub edge_clear: f32,
    /// 光往哪個方向照，CSS 漸層角度的寫法（0 = 往上、90 = 往右、順時針）
    pub light_deg: f32,
    /// 亮度集中在正對光線那段的程度（次方，越大越集中）
    pub rim_focus: f32,
    /// 細亮環的白色 alpha：(受光面, 對面, 兩側的底)
    pub rim: (f32, f32, f32),
    /// 內光（倒角接到的光往內淡開）：(受光面 alpha, 對面 alpha, 寬度 px)
    pub glow: (f32, f32, f32),
    /// 大元件的內光寬度倍率
    pub glow_soften: f32,
    /// 整片往內的亮度：(範圍 px, 白色 alpha)
    pub volume: (f32, f32),

    // ---- 依背景調整（第 5 階段第二批）----
    // 「背景亮度」是玻璃底下那塊的平均 luma（sRGB 編碼值，0..1），
    // 「雜亂度」是同一塊裡細節偏離平均的程度。都在頂點著色器裡對很寬的
    // 模糊圖（`SIGMA_GLASS_WIDE`）取樣算出來，每塊玻璃一組。
    /// 背景亮度在這段之間時，玻璃漸漸往自己的反面壓（深色玻璃壓暗、淺色玻璃提亮）
    pub adapt_lum: (f32, f32),
    /// 壓到底時蓋上去的黑／白 alpha
    pub adapt_amount: f32,
    /// 雜亂度在這段之間算「從乾淨到很雜」
    pub busy_range: (f32, f32),
    /// 很雜的背景透過玻璃時，明暗的起伏縮成原本的幾倍（1 = 不縮）
    pub range_busy: f32,
    /// 投影：四周很雜（壓在字上）時加深的倍率、四周是淺色純色時變淡的倍率
    pub shadow_busy: f32,
    pub shadow_bright: f32,
    /// 周圍彩色內容的環境光（只有大元件，依短邊內插）：
    /// (邊緣染色 alpha, 從邊緣往內多寬 px, 滲進投影的比例)
    pub ambient: (f32, f32, f32),
    /// 淺色玻璃的白色色調 alpha：(小元件, 大元件)，中間依短邊內插。
    /// 大面板上字多，淺色時要蓋得比較白，彩色背景才不會跟字搶（第 6 階段）
    pub tone_tint: (f32, f32),
    /// 染色玻璃（主要動作用）：(暗端亮度倍率, 亮端混白比例, 整體強度)
    pub tinted: (f32, f32, f32),

    // ---- 互動與動態（第 5 階段第三批）----
    /// 指標處往外擴散的內部光：(白色 alpha, 半徑 px, 指著時是按下的幾成)
    pub touch_glow: (f32, f32, f32),
    /// 指著的時候光源往指標那邊轉多少（0 = 不轉，1 = 光完全從指標那邊來）
    pub touch_light: f32,
    /// 那團光也映到鄰近玻璃的邊緣：(半徑 px, 亮環 alpha)
    pub near_light: (f32, f32),
    /// 出現與消失靠改變折射量：(存在度到多少開始起霧, 存在度到多少本體才完全蓋住)
    pub materialize: (f32, f32),
    /// 視窗失去焦點時退後：(亮環與內光剩幾成, 飽和度的增量剩幾成, 色調多蓋多少, 投影剩幾成)
    pub inactive: (f32, f32, f32, f32),
}

/// 樣式表的列號
pub const LOOK_BASE: u32 = 0;

/// regular 變體（第 5 階段）。數值都是看截圖調的，沒有 Apple 的官方數字
const LOOK_REGULAR: GlassLook = GlassLook {
    shadow_small: (2.0, 8.0, 0.22),
    shadow_large: (12.0, 40.0, 0.42),
    shadow_near: (1.0, 3.0, 0.12),
    shadow_pad: 62.0,
    size_range: (40.0, 240.0),
    refract_scale: (0.7, 1.0),
    blur_pull: 1.0,
    dispersion: 0.025,
    saturate: 1.5,
    brightness: 1.0,
    slope_cap: 4.0,
    clear_reach: 2.5,
    clear_max: 36.0,
    edge_clear: 0.0,
    light_deg: 142.0,
    rim_focus: 1.5,
    rim: (1.0, 0.6, 0.10),
    glow: (0.16, 0.08, 14.0),
    glow_soften: 2.0,
    volume: (22.0, 0.03),
    // 白字要讀得清楚，背後大約要壓到 luma 0.35 以下：0.55 的背景要蓋 0.34 的黑。
    // 上端接在 `TONE_UP` 附近，再亮就整片翻成淺色玻璃了
    adapt_lum: (0.25, 0.65),
    adapt_amount: 0.40,
    busy_range: (0.015, 0.07),
    range_busy: 0.7,
    shadow_busy: 1.6,
    shadow_bright: 0.45,
    ambient: (0.22, 36.0, 0.6),
    tone_tint: (0.34, 0.62),
    // 深淺只在強調色附近擺：上面的符號用強調色底上的字色（深色），
    // 暗端壓太深的話符號就看不到了
    tinted: (0.86, 0.25, 0.92),
    touch_glow: (0.32, 30.0, 0.5),
    touch_light: 0.75,
    near_light: (64.0, 0.7),
    materialize: (0.35, 0.2),
    inactive: (0.45, 0.4, 0.10, 0.6),
};

/// clear 變體的列號
pub const LOOK_CLEAR: u32 = 1;

/// clear 變體（第 5 階段第四批）：壓在影像上（視覺效果）用的，固定較透明、
/// **沒有任何自適應** —— 不依背景壓暗、不翻明暗、投影不跟背景變、沒有環境光。
/// 字要讀得到靠的是呼叫端在玻璃後面墊的局部變暗層（`CLEAR_DIM`），不是玻璃本身。
/// regular 與 clear 不混用：同一個畫面只用其中一種
const LOOK_CLEAR_ROW: GlassLook = GlassLook {
    saturate: 1.15,
    adapt_amount: 0.0,
    range_busy: 1.0,
    shadow_busy: 1.0,
    shadow_bright: 1.0,
    ambient: (0.0, 36.0, 0.0),
    rim: (0.85, 0.5, 0.08),
    glow: (0.10, 0.05, 14.0),
    ..LOOK_REGULAR
};

/// 著色器那邊的陣列長度，見 `glass.wgsl` 的 `LOOKS`
pub const LOOK_SLOTS: usize = 8;

/// 玻璃樣式表。`GlassStyle::look` 是這裡的索引
pub fn glass_looks() -> &'static [GlassLook] {
    const LOOKS: [GlassLook; 2] = [LOOK_REGULAR, LOOK_CLEAR_ROW];
    &LOOKS
}

/// clear 玻璃後面的局部變暗層：只壓玻璃底下與周圍一小圈，不是整片畫面。
/// (黑色 alpha, 往外擴多少 px, 邊緣模糊 px)
pub const CLEAR_DIM: (f32, f32, f32) = (0.28, 6.0, 24.0);

impl GlassStyle {
    /// 淡入淡出用：色調 alpha 與折射帶清晰度一起乘上 `k`
    pub fn faded(self, k: f32) -> Self {
        Self { tint_a: self.tint_a * k, edge: (self.edge.0, self.edge.1, self.edge.2, self.edge.3 * k), ..self }
    }

    /// 玻璃色調（顏色來自調色盤，alpha 來自預設）
    pub fn tint(&self) -> [f32; 4] {
        let c = palette().glass;
        [c[0], c[1], c[2], self.tint_a]
    }
}

/// 設定／裝置／關於／說明／設定精靈的大面板
pub const GLASS_PANEL: GlassStyle = GlassStyle { sigma: 20.0, tint_a: 0.09, edge: (26.0, 15.0, 1.2, 0.75), look: LOOK_BASE };
/// 封面流的控制列
pub const GLASS_BAR: GlassStyle = GlassStyle { sigma: 6.0, tint_a: 0.055, edge: (16.0, 12.0, 1.2, 0.7), look: LOOK_BASE };
/// 控制列上被指著的那顆「返回」膠囊
pub const GLASS_BAR_HOT: GlassStyle = GlassStyle { sigma: 6.0, tint_a: 0.10, edge: (16.0, 12.0, 1.2, 0.7), look: LOOK_BASE };
/// 封面流的搜尋膠囊
pub const GLASS_SEARCH: GlassStyle = GlassStyle { sigma: 6.0, tint_a: 0.055, edge: (14.0, 10.0, 1.2, 0.7), look: LOOK_BASE };
/// 通知列
pub const GLASS_TOAST: GlassStyle = GlassStyle { sigma: 6.0, tint_a: 0.055, edge: (14.0, 8.0, 1.2, 0.6), look: LOOK_BASE };
/// 視覺效果頁上的通知列：跟那頁的底欄一樣用 clear 變體
pub const GLASS_TOAST_CLEAR: GlassStyle = GlassStyle { sigma: 2.5, tint_a: 0.025, edge: (14.0, 8.0, 1.2, 0.6), look: LOOK_CLEAR };
/// 視覺效果的 preset 選單：色調是 `Palette::sheet`，幾乎不透明，不做折射
pub const GLASS_PICKER: GlassStyle = GlassStyle { sigma: 30.0, tint_a: 0.94, edge: (26.0, 15.0, 1.2, 0.0), look: LOOK_BASE };
/// 視覺效果頁的底欄。底下是 MilkDrop，所以用 clear 變體：模糊很輕、幾乎不上色，
/// 背後另墊 `CLEAR_DIM`。tint alpha 與 clarity 另乘 `vis`（淡入淡出），見 `scene_viz.rs`
pub const GLASS_VIZ: GlassStyle = GlassStyle { sigma: 2.5, tint_a: 0.025, edge: (22.0, 13.0, 1.2, 0.85), look: LOOK_CLEAR };

/// 選單底板的色調（`Palette::sheet` + `GLASS_PICKER.tint_a`）
pub fn sheet_tint() -> [f32; 4] {
    let c = palette().sheet;
    [c[0], c[1], c[2], GLASS_PICKER.tint_a]
}

/// preset 選單清單的捲動邊緣：那一邊還有東西時，最靠邊的一列、第二列剩多少不透明度
pub const PICKER_EDGE: [f32; 2] = [0.35, 0.75];

/// 歌詞頁與封面流的背景專輯圖模糊
pub const SIGMA_BACKDROP: f32 = 70.0;
/// 選單後面那一頁的模糊
pub const SIGMA_BEHIND_PICKER: f32 = 20.0;
/// 玻璃邊緣由清晰過渡到毛玻璃時墊的中間層
pub const SIGMA_GLASS_MID: f32 = 4.0;
/// 量背景用的很寬的模糊：玻璃底下與四周的平均亮度、顏色都從這張取
pub const SIGMA_GLASS_WIDE: f32 = 32.0;
/// 小玻璃翻成淺色的門檻（背景平均 luma）。上去要過 `UP`、回來要低於 `DOWN`，
/// 中間那段維持原狀，背景在門檻附近晃的時候才不會一直閃
pub const TONE_UP: f32 = 0.66;
pub const TONE_DOWN: f32 = 0.54;

/// 凝膠形變（按住時）：整塊放大多少、往指標那邊拉長多少、中心往指標偏多少（都是比例）
pub const GEL_GROW: f32 = 0.07;
pub const GEL_STRETCH: f32 = 0.08;
pub const GEL_PULL: f32 = 0.05;
/// 形變的彈簧：角頻率（每秒弧度）與阻尼比。阻尼比小於 1 才會放開後晃一兩下
pub const GEL_SPRING: (f32, f32) = (24.0, 0.38);
/// 多塊玻璃融合：兩塊的距離小於這個值的一半左右就連成一塊（smooth union 的 k，px）
pub const MERGE_K: f32 = 34.0;
/// 選單從按鈕長出來時用的融合距離。比平常大，頸子才拉得夠久
pub const MERGE_K_MORPH: f32 = 56.0;

// ------------------------------------------------------------------ 圓角
//
// 現有值原樣建 token，不對齊刻度。巢狀規則：內層圓角 = 外層圓角 − 內距（`concentric`）。
// 第 5 階段第四批：控制列、通知列、按鈕、分段控制改成膠囊（圓角 = 高的一半）。

pub mod radius {
    /// 同心圓角：貼著外框角落、往內縮 `inset` 的東西，圓角要是這麼多，
    /// 兩條弧線才會同一個圓心、間距處處相等
    pub fn concentric(outer: f32, inset: f32) -> f32 {
        (outer - inset).max(0.0)
    }
    /// 設定／裝置／關於／說明／設定精靈面板
    pub const PANEL: f32 = 22.0;
    /// 封面流控制列：膠囊（列高 64 的一半）
    pub const BAR: f32 = 32.0;
    /// 設定視窗浮在裡面的側欄
    pub const SIDEBAR: f32 = 18.0;
    /// preset 選單
    pub const SHEET: f32 = 26.0;
    /// 通知列、歌詞頁封面、設定精靈的圈圈
    pub const CARD: f32 = 14.0;
    /// 列表列的 hover 底
    pub const ROW: f32 = 10.0;
    /// 按鈕、輸入框
    pub const CONTROL: f32 = 9.0;
    /// 小按鈕、選單反白列
    pub const CHIP: f32 = 7.0;
    /// 音量旋鈕之類的小圓角
    pub const SM: f32 = 6.0;
    /// 進度條與音量條的軌道
    pub const TRACK: f32 = 2.0;
    /// 控件層的設定分組底（`widgets::group`）。第四批從 12 加大
    pub const GROUP: f32 = 18.0;
}

// ------------------------------------------------------------------ 陰影

#[derive(Clone, Copy, Debug)]
pub struct Shadow {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    pub spread: f32,
    /// 黑色的 alpha（顏色來自 `Palette::scrim`）
    pub alpha: f32,
}

impl Shadow {
    pub fn color(&self, mul: f32) -> [f32; 4] {
        scrim(self.alpha * mul)
    }
}

/// 小按鈕、進度條旋鈕
pub const SHADOW_KNOB: Shadow = Shadow { dx: 0.0, dy: 2.0, blur: 7.0, spread: 0.0, alpha: 0.6 };
/// 歌詞頁的封面
pub const SHADOW_ART: Shadow = Shadow { dx: 0.0, dy: 22.0, blur: 46.0, spread: -6.0, alpha: 0.66 };
/// 封面流中間那張（alpha 另乘 `ca`）
pub const SHADOW_COVER: Shadow = Shadow { dx: 0.0, dy: 2.0, blur: 46.0, spread: -10.0, alpha: 0.55 };
/// 轉場替身（alpha 另乘 `a`）
pub const SHADOW_FLYER: Shadow = Shadow { dx: 0.0, dy: 18.0, blur: 36.0, spread: -4.0, alpha: 0.55 };
/// 開關、滑桿的旋鈕，分段控制的選取塊（alpha 另乘停用時的淡化）
pub const SHADOW_CONTROL: Shadow = Shadow { dx: 0.0, dy: 1.0, blur: 4.0, spread: 0.0, alpha: 0.35 };

// ------------------------------------------------------------------ 字級

#[derive(Clone, Copy, Debug)]
pub struct Type {
    pub size: f32,
    pub line: f32,
}

pub mod ty {
    use super::Type;
    /// 面板標題（weight 600）
    pub const TITLE: Type = Type { size: 21.0, line: 28.0 };
    /// 列表列的主文字（weight 500～600）
    pub const BODY: Type = Type { size: 15.0, line: 20.0 };
    /// 面板內文、輸入框（weight 400～500）
    pub const BODY_S: Type = Type { size: 14.0, line: 20.0 };
    /// 次要文字、說明（weight 400～600）
    pub const CAPTION: Type = Type { size: 13.0, line: 18.0 };
    /// 面板底部的操作提示（weight 400）
    pub const HINT: Type = Type { size: 11.0, line: 15.0 };
}

// ------------------------------------------------------------------ 間距
//
// 現有的面板內距。其他間距（14、22、46 之類）仍是散在 `ui.rs` 版面函式裡的
// 手算座標，沒有落在 4 的刻度上，原樣保留。

pub mod space {
    /// 面板標題離面板左緣、離上緣
    pub const PANEL_X: f32 = 22.0;
    pub const PANEL_TITLE_Y: f32 = 20.0;
}

// ------------------------------------------------------------------ 動畫時長（秒）

pub mod dur {
    /// 背景專輯圖換歌時的交叉淡化
    pub const BG_FADE: f32 = 0.7;
    /// 封面流 ↔ 歌詞頁共用元素的替身飛行（app.js 的 `VIEW_ANIM_MS`）
    pub const LAND: f32 = 0.42;
    /// 專輯換頁：舊的縮出、新的縮入
    pub const SWAP_OUT: f32 = 0.18;
    pub const SWAP_IN: f32 = 0.28;
    /// 神燈收起／展開（web 版的 `HIDE_MS` / `SHOW_MS`）
    pub const HIDE: f32 = 1.25;
    pub const SHOW: f32 = 1.05;
    /// 歌詞頁控制列往下收（CSS `--view-anim`）
    pub const BAR_SLIDE: f32 = 0.34;
    /// 歌詞頁進出場（CSS `--view-anim`）
    pub const VIEW: f32 = 0.34;
    /// 通知列淡入淡出與停留
    pub const TOAST_FADE: f32 = 0.25;
    pub const TOAST_HOLD: f32 = 2.6;
    /// 迷你視窗換句往上推
    pub const MINI_SLIDE: f32 = 0.32;
    /// 開關的旋鈕滑過去（回彈曲線）
    pub const SWITCH: f32 = 0.28;
    /// 分段控制的選取塊滑過去
    pub const SEGMENT: f32 = 0.24;
    /// 玻璃依背景翻明暗
    pub const TONE: f32 = 0.3;
    /// 指著玻璃時內部光亮起、離開時暗下
    pub const TOUCH: f32 = 0.16;
    /// 視窗失去焦點時玻璃退後
    pub const FOCUS: f32 = 0.3;
    /// 選單從按鈕裡長出來
    pub const MORPH: f32 = 1.1;
    /// 選單縮回按鈕裡
    pub const MORPH_BACK: f32 = 0.46;
    /// 開關、滑桿的旋鈕按住時變成玻璃
    pub const LENS: f32 = 0.18;
    /// 大面板蓋到控制列時，控制列的玻璃消散（玻璃不疊玻璃）
    pub const YIELD: f32 = 0.3;
}

// ------------------------------------------------------------------ 控件尺寸
//
// `widgets.rs` 用的。全是邏輯像素、全是新值（舊面板沒有這些控件），
// 第 5 階段的 Liquid Glass 改版會再調。alpha 都是墨色 `ink(a)` 的 alpha。

pub mod ctl {
    /// 開關：寬、高、旋鈕離軌道邊多遠、按住時旋鈕拉長多少
    pub const SWITCH_W: f32 = 40.0;
    pub const SWITCH_H: f32 = 24.0;
    pub const SWITCH_INSET: f32 = 2.0;
    pub const SWITCH_STRETCH: f32 = 5.0;
    /// 關著的開關與滑桿沒填滿的那段軌道；指著時亮一點
    pub const TRACK_A: f32 = 0.18;
    pub const TRACK_HOT_A: f32 = 0.24;

    /// 滑桿：命中框高、軌道粗、旋鈕直徑
    pub const SLIDER_H: f32 = 24.0;
    pub const TRACK_H: f32 = 4.0;
    pub const KNOB: f32 = 20.0;
    /// 旋鈕按住時變成玻璃（Liquid Glass）：滑桿旋鈕放大幾倍、底下的軌道在裡面放大幾倍、
    /// 玻璃本體剩多少白、亮環的白色 alpha；開關的旋鈕整顆放大幾倍
    pub const LENS_GROW: f32 = 1.4;
    pub const LENS_MAG: f32 = 1.7;
    pub const LENS_FILL_A: f32 = 0.10;
    pub const LENS_RIM_A: f32 = 0.7;
    pub const SWITCH_LENS_GROW: f32 = 1.2;
    /// 有刻度的滑桿：刻度畫在旋鈕下緣再往下 `TICK_GAP`
    pub const TICK_W: f32 = 2.0;
    pub const TICK_H: f32 = 5.0;
    pub const TICK_GAP: f32 = 3.0;
    pub const TICK_A: f32 = 0.30;

    /// 分段控制：高、選取塊內縮、每段字的左右留白
    pub const SEG_H: f32 = 28.0;
    pub const SEG_INSET: f32 = 2.0;
    pub const SEG_PAD: f32 = 14.0;
    pub const SEG_BG_A: f32 = 0.08;
    pub const SEG_SEL_A: f32 = 0.20;
    pub const SEG_HOT_A: f32 = 0.06;
    pub const SEG_DIV_A: f32 = 0.14;

    /// 側欄項目（第四批從 32 加高）
    pub const SIDE_H: f32 = 34.0;
    pub const SIDE_SEL_A: f32 = 0.16;
    pub const SIDE_HOT_A: f32 = 0.07;
    pub const SIDE_ICON: f32 = 16.0;
    /// 清單裡選中那項的勾
    pub const CHECK: f32 = 16.0;

    /// 放不下的字來回捲動：平均速度（邏輯像素／秒）、停在兩端多久（秒）
    pub const MARQUEE_SPEED: f32 = 32.0;
    pub const MARQUEE_HOLD: f32 = 1.6;
    /// 設定列的標籤至少要露出幾個字寬（em），再少的話控件改排到標籤下面一行
    pub const LABEL_MIN_EM: f32 = 4.0;
    /// 控件排到下一行時那一行的高度
    pub const ROW_STACK_BAND: f32 = 36.0;

    /// 按鈕：高、字的左右留白；一般鈕的底（平常／指著／按住）
    pub const BTN_H: f32 = 28.0;
    pub const BTN_PAD: f32 = 14.0;
    pub const BTN_A: [f32; 3] = [0.12, 0.18, 0.26];
    /// 危險鈕的紅底 alpha（跟「中斷連結」鈕同一組）
    pub const BTN_DANGER_A: [f32; 3] = [0.18, 0.30, 0.55];

    /// 設定列：沒說明／有說明的列高、左右內距；分組底與分隔線。
    /// 第四批照 Liquid Glass 加大（原本 44／58／14）
    pub const ROW_H: f32 = 50.0;
    pub const ROW_H_HINT: f32 = 64.0;
    pub const ROW_PAD: f32 = 16.0;
    pub const GROUP_BG_A: f32 = 0.05;
    pub const DIV_A: f32 = 0.08;
    /// 分組標題的底離分組多遠
    pub const GROUP_TITLE_GAP: f32 = 8.0;

    /// 焦點環：粗、離控件多遠、強調色的 alpha
    pub const FOCUS_W: f32 = 2.0;
    pub const FOCUS_GAP: f32 = 1.0;
    pub const FOCUS_A: f32 = 0.8;

    /// 不能用的控件整個乘上這個 alpha
    pub const DISABLED_A: f32 = 0.4;

    /// 側欄分頁圖示的彩色方塊：邊長、圓角、裡面白色圖案的邊長
    pub const TILE: f32 = 22.0;
    pub const TILE_R: f32 = 6.0;
    pub const TILE_GLYPH: f32 = 15.0;
}

/// 設定視窗的版面。全是邏輯像素
pub mod win {
    /// 預設大小與最小大小
    pub const W: f32 = 780.0;
    pub const H: f32 = 580.0;
    pub const MIN_W: f32 = 640.0;
    pub const MIN_H: f32 = 420.0;
    /// 側欄是浮在視窗裡的一塊（Tahoe 的系統設定是這樣），離視窗邊 `SIDE_INSET`
    pub const SIDE_W: f32 = 220.0;
    pub const SIDE_INSET: f32 = 8.0;
    pub const SIDE_A: f32 = 0.045;
    pub const SIDE_EDGE_A: f32 = 0.06;
    /// 側欄裡的項目離側欄邊多遠、第一項離頂多遠
    pub const SIDE_PAD: f32 = 10.0;
    pub const SIDE_TOP: f32 = 14.0;
    /// 右邊內容：上下左右留白、內容最寬多少（再寬就置中）、頁首（大方塊＋頁名）佔的高
    pub const PAD: f32 = 28.0;
    pub const MAX_W: f32 = 640.0;
    pub const HEAD_H: f32 = 56.0;
    /// 頁首的大方塊
    pub const HEAD_TILE: f32 = 34.0;
    /// 分組之間的距離（第四批從 22 加大）
    pub const GROUP_GAP: f32 = 26.0;
    /// 內容捲到視窗上下緣時淡進底色的那段有多高（捲動邊緣效果，soft 樣式）
    pub const EDGE_FADE: f32 = 28.0;
    /// 滑桿的寬、滑桿左邊顯示值的那格寬
    pub const SLIDER_W: f32 = 180.0;
    pub const VALUE_W: f32 = 64.0;
    /// 「字型」那列的按鈕寫字型名稱，最寬這麼多（再長截掉）
    pub const FONT_BTN_MAX: f32 = 220.0;
    /// 字型預覽框：內距、行與行之間、框與下面的分組之間
    pub const PREVIEW_PAD: f32 = 14.0;
    pub const PREVIEW_LINE_GAP: f32 = 4.0;
    pub const PREVIEW_GAP: f32 = 12.0;
    /// 捲動時右邊那條捲軸
    pub const SCROLL_W: f32 = 4.0;
    pub const SCROLL_A: f32 = 0.22;
}

/// 分頁圖示方塊的顏色。macOS 系統色的深色版，深淺色外觀都用同一組
pub mod tile {
    pub const GREEN: [f32; 3] = [0.188, 0.820, 0.345];
    pub const BLUE: [f32; 3] = [0.039, 0.518, 1.0];
    pub const PINK: [f32; 3] = [1.0, 0.216, 0.373];
    pub const ORANGE: [f32; 3] = [1.0, 0.624, 0.039];
    pub const PURPLE: [f32; 3] = [0.749, 0.353, 0.949];
    pub const CYAN: [f32; 3] = [0.196, 0.678, 0.902];
    pub const GRAY: [f32; 3] = [0.557, 0.557, 0.576];
}

// ------------------------------------------------------------------ 著色器內尚未可由 token 供值
//
// 以下數字寫死在 `.wgsl`。glass.wgsl 的已經搬到上面的 `GlassLook`。
//
//   quad.wgsl、cover.wgsl（PERSPECTIVE 1100 在 cover.rs）、milk.wgsl

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 深色的墨色與玻璃就是原本的白() {
        set_mode(Mode::Dark);
        assert_eq!(ink(0.5), [1.0, 1.0, 1.0, 0.5]);
        assert_eq!(scrim(0.6), [0.0, 0.0, 0.0, 0.6]);
        assert_eq!(GLASS_PANEL.tint(), [1.0, 1.0, 1.0, 0.09]);
        assert_eq!(sheet_tint(), [11.0 / 255.0, 11.0 / 255.0, 15.0 / 255.0, 0.94]);
    }

    #[test]
    fn 外觀設定來回() {
        for a in [Appearance::Auto, Appearance::Dark, Appearance::Light] {
            assert_eq!(Appearance::parse(a.key()), a);
        }
        assert_eq!(Appearance::parse("亂寫"), Appearance::Dark);
        assert_eq!(Appearance::Dark.next(1), Appearance::Light);
        assert_eq!(Appearance::Dark.next(-1), Appearance::Auto);
    }

    #[test]
    fn oklch_來回() {
        for c in [[0.114, 0.725, 0.329], [0.9, 0.1, 0.2], [0.2, 0.3, 0.95], [0.5, 0.5, 0.5]] {
            let [l, ch, h] = to_oklch(c);
            let back = from_oklch(l, ch, h);
            for i in 0..3 {
                assert!((back[i] - c[i]).abs() < 1e-3, "{c:?} → {back:?}");
            }
        }
        // Spotify 綠大約是 L 0.69、C 0.19
        let [l, ch, _] = to_oklch([0.114, 0.725, 0.329]);
        assert!((l - 0.69).abs() < 0.01 && (ch - 0.187).abs() < 0.01, "{l} {ch}");
    }

    #[test]
    fn 封面強調色亮度一致而且在色域內() {
        set_mode(Mode::Dark);
        for rgb in [[1.0, 0.9, 0.1], [0.1, 0.2, 0.9], [0.8, 0.1, 0.1], [0.5, 0.1, 0.8], [0.1, 0.8, 0.8]] {
            for sat in [0.2, 0.6, 1.0] {
                let a = accent_from(rgb, sat);
                assert!(a.iter().all(|v| (0.0..=1.0).contains(v)));
                let [l, ..] = to_oklch(a);
                let lo = ACCENT_L[0] - 0.01;
                assert!((lo..=lo + 0.02 + ACCENT_L_YELLOW).contains(&l), "{rgb:?} {sat} → L {l}");
            }
        }
    }

    #[test]
    fn 沒設強調色就是調色盤的綠() {
        set_mode(Mode::Dark);
        set_accent(None);
        assert_eq!(accent(), DARK.accent);
        assert_eq!(on_accent(), DARK.on_accent);
        set_accent(Some([0.2, 0.4, 0.9]));
        assert_eq!(accent(), [0.2, 0.4, 0.9, 1.0]);
        set_accent(None);
    }
}
