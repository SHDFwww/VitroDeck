//! 設定與權杖的落地位置 —— 對應 Python 版 `app.py` 開頭那段。
//!
//! Python 版有 `BUNDLE`（PyInstaller 的 `_MEI` 暫存）跟 `DATA`（exe 旁邊）
//! 兩個位置，而且混用會出事。原生版把前端資源直接編進二進位，
//! **`BUNDLE` 整個概念消失**，只剩 `DATA`。
//!
//! `DATA` 的位置與檔名刻意跟 Python 版一模一樣，這樣兩邊可以並存、
//! 共用同一份設定與歌詞快取，遷移時使用者不會掉東西。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 跟 Cargo.toml 同一個。**一定要比 Python 版（1.0.x）大**：兩版共用同一個 release 管道，
/// 比較小的話檢查更新會把自己「更新」回 Python 版（update.rs 有測試擋著）
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GUIDE_URL: &str = "https://vitrodeck.shdf.work/";

/// 跟 Python 版 `app.py` 的 `SCOPES` 一致。多出來的三個是播放清單與收藏歌曲
/// （`/me/library`）要的；舊權杖沒有它們，靠 `Tokens::missing_scopes` 提示重新授權。
pub const SCOPES: &str = "user-library-read user-library-modify user-read-playback-state \
                          user-modify-playback-state user-read-currently-playing \
                          playlist-read-private playlist-read-collaborative";

pub fn user_agent() -> String {
    format!("VitroDeck-rs/{VERSION} (local desktop app)")
}

/// 優先放 exe 旁邊（可攜、看得到、好備份）。
/// 真被放到唯讀位置（Program Files 之類）才退回 LOCALAPPDATA。
pub fn data_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(beside) = exe.parent() {
            let probe = beside.join(".write_test");
            if std::fs::write(&probe, b"").is_ok() {
                let _ = std::fs::remove_file(&probe);
                return beside.to_path_buf();
            }
        }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("VitroDeck");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub struct Paths {
    pub data: PathBuf,
    pub cache: PathBuf,
    pub lyrics: PathBuf,
    pub config: PathBuf,
    pub tokens: PathBuf,
    pub albums: PathBuf,
    /// 播放清單另存一份（跟 Python 版一樣）。專輯比對只看 `albums`
    pub playlists: PathBuf,
    pub resolved: PathBuf,
    /// 展開過的清單／專輯曲目，一個檔案裝全部（中斷連結時只要刪這一個檔）
    pub tracks: PathBuf,
}

impl Paths {
    pub fn discover() -> Self {
        let data = data_dir();
        let cache = data.join("cache");
        Self {
            lyrics: cache.join("lyrics"),
            config: data.join("config.json"),
            tokens: cache.join("tokens.json"),
            albums: cache.join("albums.json"),
            playlists: cache.join("playlists.json"),
            resolved: cache.join("resolved_albums.json"),
            tracks: cache.join("tracks_cache.json"),
            cache,
            data,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub client_id: String,
    pub port: u16,
    pub lyrics_offset_ms: i64,
    pub sort: String,
    pub screen: Option<u32>,
    pub start_fullscreen: bool,
    #[serde(serialize_with = "f32_short")]
    pub cover_scale: f32,
    pub ktv: bool,
    /// 自架的 LyricsPlus，空字串就只用 LRCLIB
    pub lyricsplus_url: String,
    /// 簡體歌詞自動轉繁（s2tw 純字元）
    pub lyrics_traditional: bool,
    /// 歌詞頁在每句下面顯示翻譯（來源有給才有）。Rust 版才有
    pub lyrics_trans: bool,
    /// 問酷狗音樂要逐字歌詞（KRC），在網易雲之前。Rust 版才有
    pub lyrics_kugou: bool,
    /// 沒有逐字歌詞時問網易雲音樂（行級歌詞與翻譯）。Rust 版才有
    pub lyrics_netease: bool,
    /// 播放時用 loopback 量歌詞跟歌聲差多少、自動校正，順便抓配錯歌的歌詞（見 align/）。
    /// 第一次用時下載 36 MB 的人聲模型。預設關，關著時跟沒有這個功能一樣。Rust 版才有
    pub lyrics_autoalign: bool,
    /// 指定 LyricsPlus 要用哪些來源。預設排除 apple —— 那條需要 Apple Music
    /// 訂閱，而實測 qq 的逐字覆蓋率反而更好（華語／西洋／日文都中）。
    pub lyricsplus_sources: String,
    /// 視覺效果的影格上限。MilkDrop preset 的每幀方程式是按「幀」推進的，
    /// 不是按時間 —— 在 165Hz 螢幕上放著不管會比當年快五倍多。
    /// 當年的預設是 30，但在現代螢幕上看起來會頓，所以取 60。0 = 不限制。
    pub viz_fps: u32,
    /// all / retro / mid / flashy / fav
    pub viz_category: String,
    /// 用不用 preset 自帶的逐像素著色器。
    ///
    /// 預設**開**。用接觸印樣（`--sheet`）比對過兩條路徑：開著色器多了
    /// 真正的顏色與材質，而且沒有任何一格壞掉，關掉的話幾乎整片灰。
    /// 還缺 echo、wave_mode 1–7、shapes，個別 preset 仍偏亮。
    pub viz_preset_shaders: bool,
    pub viz_favourites: Vec<String>,

    // ---- 以下是 Python 版 v1.0.2 之後加的欄位，預設值照抄 app.py 的 DEFAULT_CONFIG ----
    pub frameless: bool,
    /// 歌詞頁的專輯圖畫成黑膠唱片
    pub vinyl: bool,
    /// 預設 3 rpm（20 秒一圈），真實的 33⅓ 久看很累
    /// f64：Python 版存的是 100/3，f32 來回一趟會變成 33.33300018…
    pub vinyl_rpm: f64,
    /// 額外附在 LyricsPlus 請求上的標頭，例如 Cloudflare Access 的
    /// CF-Access-Client-Id / CF-Access-Client-Secret
    pub lyricsplus_headers: std::collections::BTreeMap<String, String>,
    /// 關掉時 Tab 只在封面流與歌詞頁之間切
    pub viz_enabled: bool,
    pub update_check: bool,
    /// F11 循環除了視窗之外會經過哪些模式："one"／"span"
    pub f11_modes: Vec<String>,
    pub mini_enabled: bool,
    pub mini_locked: bool,
    /// 迷你視窗放歌詞（歌手併到歌名後面，空出來的兩行放當前句與下一句）。Rust 版才有
    pub mini_lyrics: bool,
    /// [x, y, w, h]，None 就擺主螢幕右下角
    /// 用 f64：pywebview 的移動事件給的座標不保證是整數，型別對不上的話
    /// 整份設定會解析失敗而退回預設值
    pub mini_rect: Option<[f64; 4]>,
    pub close_to_tray: bool,
    /// 外觀：auto（跟隨 Windows 的應用程式模式）／dark／light。預設 dark ——
    /// 淺色可以用，但數值是推測的、還在實驗中（設定視窗那列有標）
    pub appearance: String,
    /// 強調色跟著現在播的封面變；關掉就固定 Spotify 綠
    pub accent_follow: bool,
    /// 中文（漢字、注音、假名、全形標點）用的字型，家族名（例如 "Microsoft JhengHei"）。
    /// 空字串＝預設的微軟正黑體，這台電腦沒有的字型也退回預設。Rust 版才有
    pub font_cjk: String,
    /// 英文、數字與半形標點用的字型。空字串＝跟中文字型一樣。Rust 版才有
    pub font_latin: String,
    /// 字級倍率（0.8～1.5）：手寫體之類字面小的字型調大用，迷你視窗也跟。Rust 版才有
    #[serde(serialize_with = "f32_short")]
    pub font_scale: f32,
    /// 介面倍率（0.8～1.5），乘在系統的縮放上；只有主視窗與設定視窗跟，迷你視窗不跟。Rust 版才有
    #[serde(serialize_with = "f32_short")]
    pub ui_scale: f32,
    /// 歌詞頁的文字 PV 模式（實驗中，歌詞頁按 P 切換）：全螢幕演出、跟著節拍動。
    /// 開著時歌詞頁會開 loopback 擷取抓節拍。Rust 版才有
    pub lyrics_pv: bool,
    /// 主視窗的畫面更新率上限（0 = 跟隨螢幕）。實際取螢幕更新率能整除、不超過它的值，
    /// 幀才會平均落在垂直同步上。視覺效果頁照 `viz_fps`。Rust 版才有
    pub fps_limit: u32,

    /// 不認得的欄位原樣留著寫回去。少了這個，存一次設定就會把
    /// Python 版（或之後的版本）的欄位全部刪掉。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            port: 8888,
            lyrics_offset_ms: 0,
            sort: "added".into(),
            screen: None,
            start_fullscreen: false,
            cover_scale: 1.0,
            ktv: true,
            lyricsplus_url: String::new(),
            lyrics_traditional: true,
            lyrics_trans: true,
            lyrics_kugou: true,
            lyrics_netease: true,
            lyrics_autoalign: false,
            lyricsplus_sources: "qq,musixmatch,spotify".into(),
            viz_fps: 60,
            viz_category: "all".into(),
            viz_preset_shaders: true,
            viz_favourites: Vec::new(),
            frameless: false,
            vinyl: true,
            vinyl_rpm: 3.0,
            lyricsplus_headers: Default::default(),
            viz_enabled: true,
            update_check: true,
            f11_modes: vec!["one".into(), "span".into()],
            mini_enabled: false,
            mini_locked: false,
            mini_lyrics: false,
            mini_rect: None,
            close_to_tray: false,
            appearance: "dark".into(),
            accent_follow: true,
            font_cjk: String::new(),
            font_latin: String::new(),
            font_scale: 1.0,
            ui_scale: 1.0,
            lyrics_pv: false,
            fps_limit: 0,
            extra: Default::default(),
        }
    }
}

impl Config {
    /// Spotify 發的 Client ID 一律是 32 位十六進位字元。
    pub fn client_id_looks_valid(&self) -> bool {
        self.client_id.len() == 32 && self.client_id.chars().all(|c| c.is_ascii_hexdigit())
    }

    /// Spotify 現在只放行 loopback 字面位址，`localhost` 會被拒。
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix 秒。存絕對時間而不是 expires_in，重開程式才知道還有沒有效。
    pub expires_at: f64,
    /// 權杖回應裡的 `scope`（空白分隔）。舊權杖沒有這個欄位，是 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl Tokens {
    pub fn valid_now(&self) -> bool {
        !self.access_token.is_empty() && now_secs() < self.expires_at
    }

    /// 權杖還沒拿到的權限。
    ///
    /// 跟 Python 版的 `missing_scopes()` 有一處不同：Python 版一向把整個權杖回應
    /// 存下來，沒有 `scope` 欄位代表無從判斷；這邊 2026-10 以前**從來沒存過**
    /// scope，所以沒有欄位就是當時那 4 個權限。
    pub fn missing_scopes(&self) -> Vec<&'static str> {
        const OLD: &str = "user-library-read user-read-playback-state \
                           user-modify-playback-state user-read-currently-playing";
        if self.refresh_token.is_empty() {
            return Vec::new();
        }
        let scope = self.scope.as_deref().unwrap_or(OLD);
        let have: std::collections::HashSet<&str> = scope.split_whitespace().collect();
        SCOPES.split_whitespace().filter(|s| !have.contains(s)).collect()
    }
}

pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// f32 照它最短的十進位寫出去（1.1 而不是 1.100000023841858），
/// 跟 Python 版共用的設定檔才不會被寫得一團亂。
fn f32_short<S: serde::Serializer>(v: &f32, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(v.to_string().parse().unwrap_or(*v as f64))
}

impl Config {
    /// 讀設定。檔案在但解析失敗時，先把它改名成 `config.json.bad` 再回預設值 ——
    /// 否則下一次存檔會用預設值把使用者的設定整份蓋掉。
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match serde_json::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                crate::warn!("config.json 解析失敗（{e}），已改名成 config.json.bad");
                let _ = std::fs::rename(path, path.with_extension("json.bad"));
                CONFIG_BAD.store(true, std::sync::atomic::Ordering::Relaxed);
                Self::default()
            }
        }
    }
}

/// 這次啟動的 config.json 讀不進來、已經換成預設值。畫面起來後要講一聲，
/// 不然使用者只看到設定全部不見，不知道原檔在 config.json.bad。
pub static CONFIG_BAD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn read_json<T: Default + for<'de> Deserialize<'de>>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 先寫暫存檔再 rename。中途斷電不會留下半個 JSON 把設定弄壞。
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Python 版寫出來的設定檔讀進來再存回去，不能少任何一個鍵。
    #[test]
    fn config_keeps_unknown_keys() {
        let src = serde_json::json!({
            "client_id": "x", "port": 8888, "vinyl_rpm": 33.333,
            "mini_rect": [1520.0, 900, 360, 120],
            "lyricsplus_headers": {"CF-Access-Client-Id": "a"},
            "f11_modes": ["one"],
            "some_future_key": {"nested": [1, 2]},
        });
        let cfg: Config = serde_json::from_value(src.clone()).unwrap();
        assert_eq!(cfg.f11_modes, vec!["one".to_string()]);
        assert_eq!(cfg.mini_rect, Some([1520.0, 900.0, 360.0, 120.0]));
        let back = serde_json::to_value(&cfg).unwrap();
        for (k, v) in src.as_object().unwrap() {
            if k == "mini_rect" {
                continue; // 900 → 900.0，數值相同
            }
            assert_eq!(&back[k], v, "鍵 {k} 走樣了");
        }
    }

    #[test]
    fn missing_scopes_like_python() {
        assert!(Tokens::default().missing_scopes().is_empty(), "沒登入就不提示");
        let mut t = Tokens { refresh_token: "r".into(), ..Default::default() };
        assert_eq!(t.missing_scopes().len(), 3, "沒有 scope 欄位的是舊版那 4 個權限");
        t.scope = Some("user-library-read user-read-playback-state".into());
        let miss = t.missing_scopes();
        assert!(miss.contains(&"playlist-read-private"));
        assert!(!miss.contains(&"user-library-read"));
        t.scope = Some(SCOPES.into());
        assert!(t.missing_scopes().is_empty());
    }
}

/// 拿一份真的設定檔來回讀寫一次，印出差異。
/// `VITRO_CFG=<檔> cargo test --bin vitro real_config_roundtrip -- --ignored --nocapture`
/// 背景執行緒 panic 要留在 diag.log。會裝全域的 panic hook、寫到測試執行檔旁的
/// `cache\diag.log`，所以不自動跑：`cargo test panic_goes_to_diag -- --ignored`
#[cfg(test)]
#[test]
#[ignore]
fn panic_goes_to_diag() {
    install_panic_log();
    let tag = format!("故意的 {}", std::process::id());
    let t = tag.clone();
    let r = std::thread::Builder::new().name("測試執行緒".into()).spawn(move || panic!("{t}")).unwrap().join();
    assert!(r.is_err());
    let log = std::fs::read_to_string(data_dir().join("cache").join("diag.log")).unwrap();
    assert!(log.lines().any(|l| l.contains("測試執行緒") && l.contains(&tag)), "diag.log 沒有這次的 panic");
}

#[cfg(test)]
#[test]
#[ignore]
fn real_config_roundtrip() {
    let path = std::env::var("VITRO_CFG").expect("要給 VITRO_CFG");
    let src: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let cfg: Config = serde_json::from_value(src.clone()).expect("解析失敗");
    let back = serde_json::to_value(&cfg).unwrap();
    let (a, b) = (src.as_object().unwrap(), back.as_object().unwrap());
    for (k, v) in a {
        match b.get(k) {
            Some(w) if w == v => {}
            Some(w) => println!("改變  {k}: {v} -> {w}"),
            None => println!("遺失  {k}"),
        }
    }
    for k in b.keys().filter(|k| !a.contains_key(*k)) {
        println!("新增  {k} = {}", b[k]);
    }
    assert!(a.keys().all(|k| b.contains_key(k)), "有鍵被刪掉");
}

/// 寫一行到 `cache\diag.log`。正式版沒有主控台，`eprintln!` 沒人看得到；
/// 畫面停住這種事要有地方留下原因。同一句話一分鐘內只寫一次（每幀都失敗時不會灌爆），
/// 檔案超過 256 KB 就從頭寫。
///
/// 去重是一句一句記的：只記「上一句」的話，兩種錯誤每幀交替時照樣灌，
/// 灌到 256 KB 從頭寫，最早的那句原因反而被洗掉。
pub fn diag(msg: &str) {
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static SEEN: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);
    if let Ok(mut g) = SEEN.lock() {
        let seen = g.get_or_insert_with(HashMap::new);
        if seen.get(msg).is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return;
        }
        if seen.len() >= 64 {
            seen.retain(|_, t| t.elapsed() < Duration::from_secs(60));
        }
        seen.insert(msg.to_string(), Instant::now());
    }
    let path = data_dir().join("cache").join("diag.log");
    // 剛裝好、還沒抓過任何東西時 cache 資料夾不存在，開檔會失敗而什麼都沒寫
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let fresh = std::fs::metadata(&path).map(|m| m.len() > 256 * 1024).unwrap_or(false);
    let f = std::fs::OpenOptions::new().create(true).append(!fresh).write(true).truncate(fresh).open(&path);
    if let Ok(mut f) = f {
        let _ = writeln!(f, "{:.0}  v{}  {msg}", now_secs(), VERSION);
    }
}

/// `eprintln!` 加寫一份到 `diag.log`。開發版看主控台，正式版沒有主控台就看 diag.log。
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        eprintln!("{msg}");
        $crate::config::diag(&msg);
    }};
}

/// 任何一條執行緒 panic 都寫進 `diag.log`。正式版沒有主控台，背景執行緒（poller、歌詞、
/// 封面…）死掉時主程式不會跟著掛，只會畫面停在最後一首、按鍵沒反應，什麼痕跡都不留。
/// 原本的 hook 照樣呼叫，開發版主控台的輸出不變。
pub fn install_panic_log() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("未命名");
        let what = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "（不是字串）".into());
        let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        diag(&format!("panic 執行緒「{name}」 {at}：{what}"));
        prev(info);
    }));
}
