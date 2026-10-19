//! 設定面板的內容。
//!
//! 只放**點一下就能換的**選項 —— 原生視窗沒有輸入框，client_id 那種
//! 要打字的東西仍然編輯 `config.json`（面板只顯示目前狀態與檔案位置）。
//!
//! 每一列都是「標籤 ＋ 目前的值」，點一下換下一個值，存檔立刻落地。
//! 順序照 Python 版 `settings.js` 的分組：播放、顯示、歌詞、視覺效果、Spotify、其他。

use crate::config::Config;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Item {
    /// 按鈕：打開裝置選單
    Device,
    Sort,
    CoverScale,
    StartFullscreen,
    Screen,
    /// F11 循環要不要經過「這個螢幕全螢幕」
    F11One,
    /// F11 循環要不要經過「跨所有螢幕」
    F11Span,
    LyricsOffset,
    LyricsTraditional,
    Ktv,
    Vinyl,
    VinylRpm,
    /// 迷你視窗開關與鎖定
    MiniEnabled,
    MiniLocked,
    /// 迷你視窗顯示歌詞
    MiniLyrics,
    /// 按鈕：把迷你視窗移回主螢幕右下角。只在設定視窗
    MiniRecall,
    /// 關閉鈕改成縮到系統列
    CloseToTray,
    /// 關掉的話 Tab 只在封面流與歌詞之間切
    VizEnabled,
    VizFps,
    VizCategory,
    VizShaders,
    /// 按鈕：重抓收藏清單
    Refresh,
    /// 登入／登出／重新授權的按鈕
    SpotifyStatus,
    /// 版本，按一下檢查／安裝更新
    Version,
    /// 開程式時問一次 GitHub
    UpdateCheck,
    /// 唯讀：設定檔在哪
    DataDir,
    /// 按鈕：開關於面板（中斷連結在那裡）
    About,
    /// 深色／淺色／跟隨系統。只在設定視窗；舊面板放不下第 27 列
    Appearance,
    /// 強調色跟著正在播的封面。只在設定視窗
    AccentFollow,
    /// 按鈕：打開中文字型清單（設定視窗的子頁）。只在設定視窗
    FontCjk,
    /// 按鈕：打開英文字型清單。只在設定視窗
    FontLatin,
    /// 字級倍率。只在設定視窗
    FontScale,
    /// 介面倍率（主視窗與設定視窗）。只在設定視窗
    UiScale,
    /// 歌詞頁的文字 PV（實驗中，歌詞頁按 P 也能切）。只在設定視窗
    LyricsPv,
    /// 主視窗的畫面更新率上限。只在設定視窗
    FpsLimit,
}

/// 字型大小與介面大小的滑桿：一格 5%，80%～150%
const ZOOM_STEPS: usize = 15;

fn zoom_step(v: f32) -> usize {
    ((v * 20.0).round() as i32 - 16).clamp(0, ZOOM_STEPS as i32 - 1) as usize
}

fn zoom_of(i: usize) -> f32 {
    (i.min(ZOOM_STEPS - 1) + 16) as f32 / 20.0
}

pub const ITEMS: &[Item] = &[
    Item::Device,
    Item::Sort,
    Item::CoverScale,
    Item::StartFullscreen,
    Item::Screen,
    Item::F11One,
    Item::F11Span,
    Item::LyricsOffset,
    Item::LyricsTraditional,
    Item::Ktv,
    Item::Vinyl,
    Item::VinylRpm,
    Item::MiniEnabled,
    Item::MiniLocked,
    Item::MiniLyrics,
    Item::CloseToTray,
    Item::VizEnabled,
    Item::VizFps,
    Item::VizCategory,
    Item::VizShaders,
    Item::Refresh,
    Item::SpotifyStatus,
    Item::Version,
    Item::UpdateCheck,
    Item::DataDir,
    Item::About,
];

/// 唱片轉速的刻度。Python 版是 1～45 的整數滑桿（停在 33 存成 33⅓），
/// 這裡沒有滑桿，挑幾個有意義的點。
const RPM_STEPS: [f64; 8] = [1.0, 2.0, 3.0, 5.0, 10.0, 20.0, 100.0 / 3.0, 45.0];

fn on_off(v: bool) -> String {
    if v { "開".into() } else { "關".into() }
}

/// 33⅓ 照唱盤的寫法，其他四捨五入
pub fn rpm_label(rpm: f64) -> String {
    if (rpm - 100.0 / 3.0).abs() < 0.01 { "33⅓ rpm".into() } else { format!("{} rpm", rpm.round()) }
}

fn toggle_mode(modes: &mut Vec<String>, key: &str) {
    if let Some(i) = modes.iter().position(|m| m == key) {
        modes.remove(i);
    } else {
        modes.push(key.into());
    }
    // 跟 Python 版一樣固定成 one、span 的順序
    modes.sort_by_key(|m| if m == "one" { 0 } else { 1 });
}

impl Item {
    pub fn label(self) -> &'static str {
        match self {
            Item::Device => "播放裝置",
            Item::Sort => "專輯排序",
            Item::CoverScale => "封面大小",
            Item::StartFullscreen => "開啟時全螢幕",
            Item::Screen => "開在哪個螢幕",
            Item::F11One => "F11 包含全螢幕",
            Item::F11Span => "F11 包含跨所有螢幕",
            Item::LyricsOffset => "歌詞時間偏移",
            Item::LyricsTraditional => "簡體轉繁體",
            Item::Ktv => "KTV 提詞",
            Item::Vinyl => "唱片模式",
            Item::VinylRpm => "唱片轉速",
            Item::MiniEnabled => "迷你視窗",
            Item::MiniLocked => "鎖定迷你視窗",
            Item::MiniLyrics => "迷你視窗歌詞",
            Item::MiniRecall => "找不到迷你視窗",
            Item::CloseToTray => "關閉時縮到系統列",
            Item::VizEnabled => "視覺效果視圖",
            Item::VizFps => "視覺效果影格上限",
            Item::VizCategory => "視覺效果分類",
            Item::VizShaders => "preset 自帶著色器",
            Item::Refresh => "收藏",
            Item::SpotifyStatus => "Spotify",
            Item::Version => "版本",
            Item::UpdateCheck => "啟動時檢查更新",
            Item::DataDir => "設定檔",
            Item::About => "關於",
            Item::Appearance => "外觀",
            Item::AccentFollow => "強調色跟著封面",
            Item::FontCjk => "中文字型",
            Item::FontLatin => "英文字型",
            Item::FontScale => "字型大小",
            Item::UiScale => "介面大小",
            Item::LyricsPv => "文字 PV",
            Item::FpsLimit => "畫面更新率上限",
        }
    }

    /// 這一列的說明。只有值看不出在做什麼的才寫。
    pub fn hint(self) -> &'static str {
        match self {
            Item::F11One => "關掉時 F11 不會停在單一螢幕的全螢幕",
            Item::F11Span => "關掉時 F11 只在視窗與全螢幕之間切換",
            Item::Vinyl => "歌詞頁的專輯圖畫成黑膠唱片，關掉就是方形封面",
            Item::VinylRpm => "3 rpm 是 20 秒一圈；真實唱盤是 33⅓ 或 45",
            Item::Ktv => "封面流時在頂端顯示當前句",
            Item::MiniEnabled => "封面、曲名、歌手與進度；點封面回到這裡",
            Item::MiniLocked => "固定在最上層、滑鼠穿透；鎖定後只能在這裡或系統列解鎖",
            Item::MiniLyrics => "歌手移到歌名後面，空出來的位置放當前句與下一句",
            Item::MiniRecall => "移到主螢幕右下角，大小不變；沒開的話順便打開",
            Item::CloseToTray => "關掉主視窗後只留右下角圖示，畫面與歌詞停下來；右鍵圖示可結束",
            Item::VizEnabled => "關掉時 Tab 只在封面流與歌詞之間切換",
            Item::VizShaders => "關掉只剩網格變形，幾乎整片灰",
            Item::VizFps => "MilkDrop 是按幀推進的，不限的話在高刷新率螢幕上會快好幾倍",
            Item::LyricsOffset => "歌詞比音樂早就調正的",
            Item::LyricsTraditional => "歌詞，以及曲名、歌手、專輯與播放清單的名稱",
            Item::Refresh => "重新抓一次你的專輯與播放清單",
            Item::UpdateCheck => "只問 GitHub 有沒有新版，不會自己裝",
            Item::SpotifyStatus => "還沒有 client_id 的話，按一下開設定精靈",
            Item::Appearance => "迷你視窗與視覺效果頁固定深色",
            Item::AccentFollow => "開關、滑桿與主要按鈕的顏色取自正在播的封面；關掉是 Spotify 綠",
            Item::FontCjk => "漢字、注音、假名與全形標點",
            Item::FontLatin => "英文、數字與半形標點",
            Item::FontScale => "手寫體這類偏小的字型可調大",
            Item::UiScale => "迷你視窗不跟著變",
            Item::LyricsPv => "全螢幕跟著節拍演出，限有逐字歌詞的歌；歌詞頁按 P 也能切換",
            Item::FpsLimit => "取螢幕更新率能整除的值，例如 180 Hz 選 120 是 90 fps；視覺效果頁照它自己的上限",
            _ => "",
        }
    }

    /// 標籤右邊的小徽章。只有還在試的功能才有
    pub fn badge(self) -> &'static str {
        match self {
            Item::Appearance => "淺色實驗中",
            Item::LyricsPv => "實驗中",
            _ => "",
        }
    }

    /// `login_msg` 是登入流程正在說的話（空字串代表沒有在進行中）。
    /// `update` 是更新那列要寫的字（狀態在 `update::Updater`）。
    pub fn value(self, cfg: &Config, logged_in: bool, data_dir: &str, login_msg: &str, update: &str) -> String {
        let has = |k: &str| cfg.f11_modes.iter().any(|m| m == k);
        match self {
            Item::Device => "選擇…（D）".into(),
            Item::Sort => match cfg.sort.as_str() {
                "name" => "專輯名".into(),
                "artist" => "演出者".into(),
                "year" => "年份".into(),
                _ => "加入時間".into(),
            },
            Item::CoverScale => format!("{:.0}%", cfg.cover_scale * 100.0),
            Item::StartFullscreen => on_off(cfg.start_fullscreen),
            Item::Screen => match cfg.screen {
                Some(i) => format!("第 {i} 個"),
                None => "自動".into(),
            },
            Item::F11One => on_off(has("one")),
            Item::F11Span => on_off(has("span")),
            Item::LyricsOffset => format!("{:+} ms", cfg.lyrics_offset_ms),
            Item::LyricsTraditional => on_off(cfg.lyrics_traditional),
            Item::Ktv => on_off(cfg.ktv),
            Item::LyricsPv => on_off(cfg.lyrics_pv),
            Item::Vinyl => on_off(cfg.vinyl),
            Item::VinylRpm => rpm_label(cfg.vinyl_rpm),
            Item::MiniEnabled => on_off(cfg.mini_enabled),
            Item::MiniLocked => on_off(cfg.mini_locked),
            Item::MiniLyrics => on_off(cfg.mini_lyrics),
            Item::MiniRecall => "叫回來".into(),
            Item::CloseToTray => on_off(cfg.close_to_tray),
            Item::VizEnabled => on_off(cfg.viz_enabled),
            Item::VizFps => {
                if cfg.viz_fps == 0 {
                    "不限".into()
                } else {
                    format!("{} fps", cfg.viz_fps)
                }
            }
            Item::VizCategory => crate::viz::Category::parse_pub(&cfg.viz_category).label().into(),
            Item::VizShaders => on_off(cfg.viz_preset_shaders),
            Item::Refresh => "重新整理（R）".into(),
            Item::SpotifyStatus => {
                if !login_msg.is_empty() {
                    login_msg.to_string()
                } else if !cfg.client_id_looks_valid() {
                    "沒設定 client_id".into()
                } else if logged_in {
                    "已登入（按一下登出）".into()
                } else {
                    "按一下登入".into()
                }
            }
            Item::Version if update.is_empty() => format!("VitroDeck {}", crate::config::VERSION),
            Item::Version => update.to_string(),
            Item::UpdateCheck => on_off(cfg.update_check),
            Item::DataDir => data_dir.to_string(),
            Item::About => "關於 / 中斷連結…（I）".into(),
            Item::Appearance => crate::theme::Appearance::parse(&cfg.appearance).label().into(),
            Item::AccentFollow => on_off(cfg.accent_follow),
            Item::FontCjk if cfg.font_cjk.is_empty() => "預設".into(),
            Item::FontCjk => cfg.font_cjk.clone(),
            Item::FontLatin if cfg.font_latin.is_empty() => "跟中文字型一樣".into(),
            Item::FontLatin => cfg.font_latin.clone(),
            Item::FontScale => format!("{:.0}%", cfg.font_scale * 100.0),
            Item::UiScale => format!("{:.0}%", cfg.ui_scale * 100.0),
            Item::FpsLimit if cfg.fps_limit == 0 => "跟隨螢幕".into(),
            Item::FpsLimit => format!("{} fps", cfg.fps_limit),
        }
    }

    /// 純顯示、按了沒有反應的列。
    /// Spotify 那列**不是**唯讀 —— 它是登入／登出的按鈕。
    pub fn read_only(self) -> bool {
        matches!(self, Item::DataDir)
    }

    /// 換下一個值。`d` 是方向（滾輪與左右鍵會給 −1）。
    pub fn cycle(self, cfg: &mut Config, d: i32) {
        match self {
            Item::Sort => {
                const ORDER: [&str; 4] = ["added", "name", "artist", "year"];
                let i = ORDER.iter().position(|v| *v == cfg.sort).unwrap_or(0) as i32;
                let n = (i + d).rem_euclid(ORDER.len() as i32) as usize;
                cfg.sort = ORDER[n].to_string();
            }
            // 一格 10%，範圍跟 Python 版一樣 50%～200%；點到底再點一下繞回去
            Item::CoverScale => {
                let steps = ((cfg.cover_scale * 10.0).round() as i32 - 5).clamp(0, 15);
                let n = (steps + d).rem_euclid(16);
                cfg.cover_scale = (n + 5) as f32 / 10.0;
            }
            Item::StartFullscreen => cfg.start_fullscreen = !cfg.start_fullscreen,
            // 螢幕數量在這裡不知道，所以只在 0..3 之間轉；真的移過去時
            // 超出範圍會被當成「自動」。
            Item::Screen => {
                let cur = cfg.screen.map(|i| i as i32 + 1).unwrap_or(0);
                let n = (cur + d).rem_euclid(4);
                cfg.screen = if n == 0 { None } else { Some(n as u32 - 1) };
            }
            Item::F11One => toggle_mode(&mut cfg.f11_modes, "one"),
            Item::F11Span => toggle_mode(&mut cfg.f11_modes, "span"),
            // 偏移一次調 100ms，範圍 ±3 秒。再多就不是「對時」而是壞掉了。
            Item::LyricsOffset => {
                cfg.lyrics_offset_ms = (cfg.lyrics_offset_ms + d as i64 * 100).clamp(-3000, 3000)
            }
            Item::LyricsTraditional => cfg.lyrics_traditional = !cfg.lyrics_traditional,
            Item::Ktv => cfg.ktv = !cfg.ktv,
            Item::LyricsPv => cfg.lyrics_pv = !cfg.lyrics_pv,
            Item::Vinyl => cfg.vinyl = !cfg.vinyl,
            Item::VinylRpm => {
                // 不在刻度上的（Python 版滑桿拉出來的 7 rpm 之類）從最近的那格開始
                let i = RPM_STEPS
                    .iter()
                    .enumerate()
                    .min_by(|a, b| (a.1 - cfg.vinyl_rpm).abs().total_cmp(&(b.1 - cfg.vinyl_rpm).abs()))
                    .map(|(i, _)| i as i32)
                    .unwrap_or(2);
                cfg.vinyl_rpm = RPM_STEPS[(i + d).rem_euclid(RPM_STEPS.len() as i32) as usize];
            }
            Item::MiniEnabled => cfg.mini_enabled = !cfg.mini_enabled,
            Item::MiniLocked => cfg.mini_locked = !cfg.mini_locked,
            Item::MiniLyrics => cfg.mini_lyrics = !cfg.mini_lyrics,
            Item::CloseToTray => cfg.close_to_tray = !cfg.close_to_tray,
            Item::VizEnabled => cfg.viz_enabled = !cfg.viz_enabled,
            Item::VizFps => {
                const STEPS: [u32; 4] = [30, 60, 120, 0];
                let i = STEPS.iter().position(|v| *v == cfg.viz_fps).unwrap_or(1) as i32;
                let n = (i + d).rem_euclid(STEPS.len() as i32) as usize;
                cfg.viz_fps = STEPS[n];
            }
            Item::FpsLimit => {
                let i = LIMITS.iter().position(|v| *v == cfg.fps_limit).unwrap_or(0) as i32;
                cfg.fps_limit = LIMITS[(i + d).rem_euclid(LIMITS.len() as i32) as usize];
            }
            Item::VizCategory => {
                let c = crate::viz::Category::parse_pub(&cfg.viz_category);
                let n = if d >= 0 { c.next() } else { c.prev() };
                cfg.viz_category = n.key().to_string();
            }
            Item::VizShaders => cfg.viz_preset_shaders = !cfg.viz_preset_shaders,
            Item::UpdateCheck => cfg.update_check = !cfg.update_check,
            Item::Appearance => {
                cfg.appearance = crate::theme::Appearance::parse(&cfg.appearance).next(d).key().to_string()
            }
            Item::AccentFollow => cfg.accent_follow = !cfg.accent_follow,
            // 到底就停，跟滑桿一樣
            Item::FontScale => cfg.font_scale = zoom_of((zoom_step(cfg.font_scale) as i32 + d).max(0) as usize),
            Item::UiScale => cfg.ui_scale = zoom_of((zoom_step(cfg.ui_scale) as i32 + d).max(0) as usize),
            Item::FontCjk
            | Item::FontLatin
            | Item::Device
            | Item::Refresh
            | Item::SpotifyStatus
            | Item::Version
            | Item::DataDir
            | Item::About
            | Item::MiniRecall => {}
        }
    }

    /// 在設定視窗裡用什麼控件
    pub fn control(self) -> Control {
        match self {
            Item::Sort | Item::Screen | Item::VizFps | Item::VizCategory | Item::Appearance | Item::FpsLimit => {
                Control::Choice
            }
            // 一格 10%，50%～200%
            Item::CoverScale => Control::Slider { steps: 16, ticks: false },
            // 一格 100ms，±3 秒
            Item::LyricsOffset => Control::Slider { steps: 61, ticks: false },
            Item::VinylRpm => Control::Slider { steps: RPM_STEPS.len(), ticks: true },
            Item::FontScale | Item::UiScale => Control::Slider { steps: ZOOM_STEPS, ticks: false },
            Item::Device
            | Item::Refresh
            | Item::SpotifyStatus
            | Item::Version
            | Item::DataDir
            | Item::About
            | Item::MiniRecall
            | Item::FontCjk
            | Item::FontLatin => Control::Button,
            _ => Control::Switch,
        }
    }

    /// 開關現在是不是開著（不是開關的回 false）
    pub fn is_on(self, cfg: &Config) -> bool {
        let has = |k: &str| cfg.f11_modes.iter().any(|m| m == k);
        match self {
            Item::StartFullscreen => cfg.start_fullscreen,
            Item::F11One => has("one"),
            Item::F11Span => has("span"),
            Item::LyricsTraditional => cfg.lyrics_traditional,
            Item::Ktv => cfg.ktv,
            Item::LyricsPv => cfg.lyrics_pv,
            Item::Vinyl => cfg.vinyl,
            Item::MiniEnabled => cfg.mini_enabled,
            Item::MiniLocked => cfg.mini_locked,
            Item::MiniLyrics => cfg.mini_lyrics,
            Item::CloseToTray => cfg.close_to_tray,
            Item::VizEnabled => cfg.viz_enabled,
            Item::VizShaders => cfg.viz_preset_shaders,
            Item::UpdateCheck => cfg.update_check,
            Item::AccentFollow => cfg.accent_follow,
            _ => false,
        }
    }

    /// 分段控制的選項。`screens` 是現在接著幾個螢幕
    pub fn choices(self, screens: usize) -> Vec<String> {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect();
        match self {
            Item::Sort => v(&["加入時間", "專輯名", "演出者", "年份"]),
            Item::Screen => std::iter::once("自動".to_string()).chain((1..=screens.max(1)).map(|i| i.to_string())).collect(),
            Item::VizFps => v(&["30", "60", "120", "不限"]),
            Item::VizCategory => VIZ_CATS.iter().map(|c| c.label().to_string()).collect(),
            Item::Appearance => APPEARANCES.iter().map(|a| a.label().to_string()).collect(),
            Item::FpsLimit => v(&["跟隨螢幕", "120", "60", "30"]),
            _ => Vec::new(),
        }
    }

    /// 分段控制選在第幾段。設定檔裡的值不在選項裡（例如螢幕拔掉了）就是第 0 段
    pub fn choice(self, cfg: &Config, screens: usize) -> usize {
        let i = match self {
            Item::Sort => SORTS.iter().position(|v| *v == cfg.sort),
            Item::Screen => Some(cfg.screen.map_or(0, |i| i as usize + 1)),
            Item::VizFps => FPS.iter().position(|v| *v == cfg.viz_fps),
            Item::VizCategory => {
                let c = crate::viz::Category::parse_pub(&cfg.viz_category);
                VIZ_CATS.iter().position(|v| *v == c)
            }
            Item::Appearance => {
                let a = crate::theme::Appearance::parse(&cfg.appearance);
                APPEARANCES.iter().position(|v| *v == a)
            }
            Item::FpsLimit => LIMITS.iter().position(|v| *v == cfg.fps_limit),
            _ => None,
        };
        i.filter(|i| *i < self.choices(screens).len()).unwrap_or(0)
    }

    pub fn set_choice(self, cfg: &mut Config, i: usize) {
        match self {
            Item::Sort => cfg.sort = SORTS.get(i).unwrap_or(&SORTS[0]).to_string(),
            Item::Screen => cfg.screen = i.checked_sub(1).map(|i| i as u32),
            Item::VizFps => cfg.viz_fps = *FPS.get(i).unwrap_or(&FPS[1]),
            Item::VizCategory => cfg.viz_category = VIZ_CATS.get(i).unwrap_or(&VIZ_CATS[0]).key().to_string(),
            Item::Appearance => cfg.appearance = APPEARANCES.get(i).copied().unwrap_or_default().key().to_string(),
            Item::FpsLimit => cfg.fps_limit = *LIMITS.get(i).unwrap_or(&LIMITS[0]),
            _ => {}
        }
    }

    /// 滑桿停在第幾格。不在格上的（Python 版拉出來的 7 rpm 之類）取最近的那格
    pub fn step(self, cfg: &Config) -> usize {
        match self {
            Item::CoverScale => ((cfg.cover_scale * 10.0).round() as i32 - 5).clamp(0, 15) as usize,
            Item::LyricsOffset => ((cfg.lyrics_offset_ms.clamp(-3000, 3000) + 3000) as f64 / 100.0).round() as usize,
            Item::VinylRpm => RPM_STEPS
                .iter()
                .enumerate()
                .min_by(|a, b| (a.1 - cfg.vinyl_rpm).abs().total_cmp(&(b.1 - cfg.vinyl_rpm).abs()))
                .map_or(2, |(i, _)| i),
            Item::FontScale => zoom_step(cfg.font_scale),
            Item::UiScale => zoom_step(cfg.ui_scale),
            _ => 0,
        }
    }

    pub fn set_step(self, cfg: &mut Config, i: usize) {
        match self {
            Item::CoverScale => cfg.cover_scale = (i.min(15) + 5) as f32 / 10.0,
            Item::LyricsOffset => cfg.lyrics_offset_ms = i.min(60) as i64 * 100 - 3000,
            Item::VinylRpm => cfg.vinyl_rpm = RPM_STEPS[i.min(RPM_STEPS.len() - 1)],
            Item::FontScale => cfg.font_scale = zoom_of(i),
            Item::UiScale => cfg.ui_scale = zoom_of(i),
            _ => {}
        }
    }

    /// 設定視窗裡按鈕上寫的字
    pub fn button_label(self) -> &'static str {
        match self {
            Item::Device => "選擇…",
            Item::Refresh => "重新整理",
            Item::DataDir => "打開資料夾",
            Item::About => "打開…",
            Item::MiniRecall => "叫回來",
            _ => "",
        }
    }

    /// 這一項要不要因為別的設定而停用（變淡、不能按）。
    /// 例如迷你視窗關著時，鎖定與歌詞沒有對象
    pub fn disabled(self, cfg: &Config) -> bool {
        match self {
            Item::MiniLocked | Item::MiniLyrics => !cfg.mini_enabled,
            Item::VinylRpm => !cfg.vinyl,
            Item::VizFps | Item::VizCategory | Item::VizShaders => !cfg.viz_enabled,
            _ => false,
        }
    }

    /// 改一個值。設定視窗與鍵盤都經過這裡
    pub fn apply(self, cfg: &mut Config, e: Edit) {
        match e {
            Edit::Cycle(d) => self.cycle(cfg, d),
            Edit::Choice(i) => self.set_choice(cfg, i),
            Edit::Step(i) => self.set_step(cfg, i),
            Edit::Press => {
                if self.control() == Control::Switch {
                    self.cycle(cfg, 1)
                }
            }
        }
    }
}

/// 設定視窗裡一項用的控件
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
    Switch,
    /// 分段控制
    Choice,
    /// `steps` 格（含兩端）；`ticks` 要不要畫刻度
    Slider { steps: usize, ticks: bool },
    Button,
}

/// 對一項做的事
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edit {
    /// 換下一個值（鍵盤左右、舊面板）
    Cycle(i32),
    /// 分段控制選第幾段
    Choice(usize),
    /// 滑桿拉到第幾格
    Step(usize),
    /// 按一下：開關切換、按鈕觸發
    Press,
}

const SORTS: [&str; 4] = ["added", "name", "artist", "year"];
const FPS: [u32; 4] = [30, 60, 120, 0];
/// 畫面更新率上限的選項，0 = 跟隨螢幕
const LIMITS: [u32; 4] = [0, 120, 60, 30];
/// 順序照 `Category::next`（web 版 viz.js 的 CATS）
const VIZ_CATS: [crate::viz::Category; 5] = {
    use crate::viz::Category as C;
    [C::All, C::Fav, C::Retro, C::Mid, C::Flashy]
};
const APPEARANCES: [crate::theme::Appearance; 3] = {
    use crate::theme::Appearance as A;
    [A::Dark, A::Light, A::Auto]
};

/// 設定視窗的分頁
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Page {
    Account,
    Display,
    Lyrics,
    Vinyl,
    Mini,
    Viz,
    About,
}

pub const PAGES: [Page; 7] = [Page::Account, Page::Display, Page::Lyrics, Page::Vinyl, Page::Mini, Page::Viz, Page::About];

impl Page {
    pub fn label(self) -> &'static str {
        match self {
            Page::Account => "帳號與播放",
            Page::Display => "顯示",
            Page::Lyrics => "歌詞",
            Page::Vinyl => "唱片",
            Page::Mini => "迷你視窗",
            Page::Viz => "視覺效果",
            Page::About => "關於與更新",
        }
    }

    /// 頁裡的分組：（標題, 項目）。標題空字串就不畫
    pub fn groups(self) -> &'static [(&'static str, &'static [Item])] {
        use Item as I;
        match self {
            Page::Account => &[("帳號", &[I::SpotifyStatus]), ("播放", &[I::Device, I::Refresh])],
            Page::Display => &[
                ("封面流", &[I::Sort, I::CoverScale]),
                ("視窗", &[I::StartFullscreen, I::Screen, I::F11One, I::F11Span, I::CloseToTray]),
                ("文字與大小", &[I::FontCjk, I::FontLatin, I::FontScale, I::UiScale]),
                ("顏色", &[I::Appearance, I::AccentFollow]),
                ("耗電", &[I::FpsLimit]),
            ],
            Page::Lyrics => &[("", &[I::LyricsOffset, I::LyricsTraditional, I::Ktv]), ("", &[I::LyricsPv])],
            Page::Vinyl => &[("", &[I::Vinyl, I::VinylRpm])],
            Page::Mini => &[("", &[I::MiniEnabled, I::MiniLocked, I::MiniLyrics, I::MiniRecall])],
            Page::Viz => &[("", &[I::VizEnabled]), ("MilkDrop", &[I::VizFps, I::VizCategory, I::VizShaders])],
            Page::About => &[("更新", &[I::Version, I::UpdateCheck]), ("", &[I::DataDir, I::About])],
        }
    }

    /// 頁裡所有項目，由上往下
    pub fn items(self) -> Vec<Item> {
        self.groups().iter().flat_map(|(_, it)| it.iter().copied()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f11_開關維持固定順序() {
        let mut cfg = Config::default();
        Item::F11One.cycle(&mut cfg, 1);
        assert_eq!(cfg.f11_modes, vec!["span".to_string()]);
        Item::F11One.cycle(&mut cfg, 1);
        assert_eq!(cfg.f11_modes, vec!["one".to_string(), "span".to_string()]);
    }

    #[test]
    fn 轉速從最近的刻度開始而且_33_顯示成分數() {
        let mut cfg = Config { vinyl_rpm: 7.0, ..Default::default() };
        Item::VinylRpm.cycle(&mut cfg, 1);
        assert_eq!(cfg.vinyl_rpm, 10.0);
        cfg.vinyl_rpm = 20.0;
        Item::VinylRpm.cycle(&mut cfg, 1);
        assert_eq!(rpm_label(cfg.vinyl_rpm), "33⅓ rpm");
    }

    #[test]
    fn 舊面板每一項在設定視窗都找得到而且只出現一次() {
        let all: Vec<Item> = PAGES.iter().flat_map(|p| p.items()).collect();
        for it in ITEMS {
            assert_eq!(all.iter().filter(|x| *x == it).count(), 1, "{it:?}");
        }
        assert_eq!(all.len(), ITEMS.len() + 9, "多出來的只有外觀、強調色、叫回迷你視窗、兩個字型、字型大小、介面大小、文字 PV 與更新率上限");
    }

    #[test]
    fn 分段與滑桿的值來回一致() {
        let mut cfg = Config::default();
        for p in PAGES {
            for it in p.items() {
                match it.control() {
                    Control::Choice => {
                        for i in 0..it.choices(3).len() {
                            it.set_choice(&mut cfg, i);
                            assert_eq!(it.choice(&cfg, 3), i, "{it:?} 第 {i} 段");
                        }
                    }
                    Control::Slider { steps, .. } => {
                        for i in 0..steps {
                            it.set_step(&mut cfg, i);
                            assert_eq!(it.step(&cfg), i, "{it:?} 第 {i} 格");
                        }
                    }
                    _ => {}
                }
            }
        }
        // 設定檔指到不存在的螢幕：當成「自動」
        cfg.screen = Some(5);
        assert_eq!(Item::Screen.choice(&cfg, 2), 0);
        // 開關按一下就是切換
        let on = Item::Ktv.is_on(&cfg);
        Item::Ktv.apply(&mut cfg, Edit::Press);
        assert_eq!(Item::Ktv.is_on(&cfg), !on);
    }

    #[test]
    fn 字型與介面大小一格_5_百分比而且到底就停() {
        let mut cfg = Config::default();
        assert_eq!(Item::FontScale.step(&cfg), 4, "100% 是第 5 格");
        Item::FontScale.set_step(&mut cfg, 0);
        assert_eq!(cfg.font_scale, 0.8);
        Item::FontScale.cycle(&mut cfg, -1);
        assert_eq!(cfg.font_scale, 0.8);
        Item::UiScale.set_step(&mut cfg, ZOOM_STEPS - 1);
        assert_eq!(cfg.ui_scale, 1.5);
        Item::UiScale.cycle(&mut cfg, 1);
        assert_eq!(cfg.ui_scale, 1.5);
        assert_eq!(Item::UiScale.value(&cfg, false, "", "", ""), "150%");
    }

    #[test]
    fn 封面大小到頂繞回最小() {
        let mut cfg = Config { cover_scale: 2.0, ..Default::default() };
        Item::CoverScale.cycle(&mut cfg, 1);
        assert_eq!(cfg.cover_scale, 0.5);
        Item::CoverScale.cycle(&mut cfg, -1);
        assert_eq!(cfg.cover_scale, 2.0);
    }
}
