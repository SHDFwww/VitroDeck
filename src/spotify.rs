//! Spotify Web API 與 OAuth（PKCE）—— 對應 Python 版 `app.py` 的
//! 「OAuth (PKCE)」與「Spotify API」兩節。
//!
//! 這支程式**不播音訊**：它透過 `/v1/me/player` 控制使用者現有的桌面版
//! Spotify，自己只當控制器加顯示器。所以需要 Premium（播放控制端點對免費
//! 帳號一律 403）。
//!
//! Web API 有配額，而且**爆掉是完全靜默的**（被鎖數小時），所以
//! 播放狀態優先走 SMTC，這裡只負責 SMTC 給不了的事。

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::{self, Config, Tokens};

const API: &str = "https://api.spotify.com/v1";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const AUTH_URL: &str = "https://accounts.spotify.com/authorize";

#[derive(Debug)]
pub enum ApiError {
    /// 還沒授權，或 refresh 失敗。前端要退回登入畫面。
    NotAuthed,
    /// 被 Spotify 限流。`until` 是可以再試的絕對時間（Unix 秒）。
    RateLimited { until: f64 },
    Status(u16),
    Net(String),
    /// 讀不到內容。2026-02 起開發者模式只讀得到自己建立或共同編輯的播放清單，
    /// 別人的清單回應裡沒有 `items`。
    NotReadable,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::NotAuthed => write!(f, "尚未授權"),
            ApiError::RateLimited { until } => {
                write!(f, "被限流，{:.0} 秒後才能再試", until - config::now_secs())
            }
            ApiError::Status(c) => write!(f, "HTTP {c}"),
            ApiError::Net(e) => write!(f, "連線失敗：{e}"),
            ApiError::NotReadable => write!(f, "Spotify 不開放讀取這個播放清單的內容"),
        }
    }
}

impl std::error::Error for ApiError {}

/// 欄位名刻意跟 Python 版的 `albums.json` / `playlists.json` 一模一樣，
/// 兩邊才能共用同一份快取。
///
/// 封面流上的每一格都是這個：收藏的專輯、播放清單，以及展開之後的曲目
/// （`kind` 區分）。Python 版也是同一種 dict 混著放。
#[derive(Debug, Clone, Default, serde::Serialize, Deserialize)]
#[serde(default)]
pub struct Album {
    pub id: String,
    pub uri: String,
    pub name: String,
    /// 全部演出者以「, 」串起來，跟 Python 版一致。播放清單放的是建立者。
    pub artist: String,
    pub year: String,
    pub added: String,
    /// 640px，中央大圖用
    pub art: String,
    /// 300px，側面封面用這張就夠
    pub art_sm: String,
    pub tracks: u32,
    /// album / playlist / track。舊版 Rust 寫的快取沒有這欄，空字串當專輯
    pub kind: String,
    /// 播放清單：自己建立或共同編輯的才讀得到內容、才能展開
    #[serde(skip_serializing_if = "is_false")]
    pub own: bool,
    /// 播放清單在 Spotify 裡的順序。「加入時間」「年份」排序時清單排最前面，彼此照這個
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<u32>,
    // ---- 以下只有曲目（kind = track）才有 ----
    #[serde(skip_serializing_if = "String::is_empty")]
    pub track_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub album: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub album_uri: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub duration: i64,
    #[serde(skip_serializing_if = "is_false")]
    pub is_local: bool,
    /// 在專輯／清單裡的位置。清單會跳過 episode，所以跟陣列索引不一定一樣，播放時要用這個
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos: Option<u32>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

impl Album {
    pub fn is_playlist(&self) -> bool {
        self.kind == "playlist"
    }

    pub fn is_track(&self) -> bool {
        self.kind == "track"
    }

    /// 按下去會不會展開成曲目。專輯與自己的清單會；追蹤的清單直接播
    /// （讀不到內容）；幽靈專輯若沒對應到 Spotify（`smtc:album:…`）也只能直接播。
    pub fn can_drill(&self) -> bool {
        if self.is_playlist() {
            return self.own;
        }
        !self.is_track() && self.uri.starts_with("spotify:album:")
    }

    /// 中央那張底下的副標，依種類不同（web 版的 `metaSub`）。
    pub fn subtitle(&self) -> String {
        let parts: Vec<String> = if self.is_track() {
            vec![self.artist.clone(), self.album.clone()]
        } else if self.is_playlist() {
            // 追蹤的清單 Spotify 不給內容，連首數都沒有
            if self.own {
                vec![self.artist.clone(), format!("{} 首", self.tracks), "播放清單".into()]
            } else {
                vec![self.artist.clone(), "追蹤的清單".into()]
            }
        } else {
            let mut v = vec![self.artist.clone(), self.year.clone()];
            if self.tracks > 0 {
                v.push(format!("{} 首", self.tracks));
            }
            v
        };
        parts.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("  ·  ")
    }
}

/// Spotify Connect 的一個播放裝置（`/me/player/devices`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Device {
    /// 有些裝置（受限的、私人工作階段）沒有 id，那種選了也轉不過去
    pub id: Option<String>,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub is_active: bool,
}

pub struct Spotify {
    http: reqwest::blocking::Client,
    tokens_path: PathBuf,
    pub tokens: Tokens,
    client_id: String,
    redirect_uri: String,
    /// Spotify 對開發者模式的 app 有配額。被擋到什麼時候（絕對時間），
    /// 在這之前直接不發請求 —— 硬打只會讓情況更糟。
    rate_limit_until: f64,
    /// token 續期失敗後，在這之前不再重試。poller 每 500ms 就會問一次 token，
    /// 沒有冷卻的話 refresh_token 失效時會一秒打兩次 token 端點。
    refresh_retry_at: f64,
    /// 每個 Web API 請求記一行（時間 方法 路徑 狀態），事後查是哪條路徑在燒配額
    log_path: PathBuf,
    /// state -> code_verifier。授權分頁可能開好幾個，所以要記得哪個是哪個。
    pending: HashMap<String, String>,
}

impl Spotify {
    pub fn new(cfg: &Config, tokens_path: PathBuf) -> Self {
        let http = reqwest::blocking::Client::builder()
            .user_agent(config::user_agent())
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client 建不起來");
        Self {
            tokens: config::read_json(&tokens_path),
            http,
            log_path: tokens_path.with_file_name("api_log.txt"),
            tokens_path,
            client_id: cfg.client_id.clone(),
            redirect_uri: cfg.redirect_uri(),
            rate_limit_until: 0.0,
            refresh_retry_at: 0.0,
            pending: HashMap::new(),
        }
    }

    pub fn set_client_id(&mut self, id: &str) {
        self.client_id = id.to_string();
    }

    pub fn rate_limited_until(&self) -> f64 {
        self.rate_limit_until
    }

    pub fn logged_in(&self) -> bool {
        !self.tokens.refresh_token.is_empty()
    }

    pub fn logout(&mut self) {
        self.tokens = Tokens::default();
        let _ = std::fs::remove_file(&self.tokens_path);
    }

    // ---------------------------- OAuth ----------------------------

    pub fn build_auth_url(&mut self) -> String {
        let verifier = B64.encode(random_bytes(48));
        let challenge = B64.encode(Sha256::digest(verifier.as_bytes()));
        let state = B64.encode(random_bytes(16));
        self.pending.insert(state.clone(), verifier);

        let q = [
            ("client_id", self.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("code_challenge_method", "S256"),
            ("code_challenge", challenge.as_str()),
            ("state", state.as_str()),
            ("scope", config::SCOPES),
        ]
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
        format!("{AUTH_URL}?{q}")
    }

    pub fn exchange_code(&mut self, code: &str, state: &str) -> anyhow::Result<()> {
        let verifier = self
            .pending
            .remove(state)
            .ok_or_else(|| anyhow::anyhow!("state 對不上，可能是舊的授權分頁，請重新登入"))?;
        let resp = self
            .http
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("client_id", self.client_id.as_str()),
                ("code_verifier", verifier.as_str()),
            ])
            .send()?;
        if !resp.status().is_success() {
            anyhow::bail!("換 token 失敗：HTTP {} {}", resp.status().as_u16(), resp.text()?);
        }
        let body: TokenResponse = resp.json()?;
        self.store_tokens(body, None);
        Ok(())
    }

    pub fn refresh(&mut self) -> bool {
        if self.tokens.refresh_token.is_empty() {
            return false;
        }
        let old = self.tokens.refresh_token.clone();
        let Ok(resp) = self
            .http
            .post(TOKEN_URL)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", old.as_str()),
                ("client_id", self.client_id.as_str()),
            ])
            .send()
        else {
            return false;
        };
        if !resp.status().is_success() {
            return false;
        }
        match resp.json::<TokenResponse>() {
            Ok(body) => {
                self.store_tokens(body, Some(old));
                true
            }
            Err(_) => false,
        }
    }

    fn store_tokens(&mut self, body: TokenResponse, keep_refresh: Option<String>) {
        self.tokens.access_token = body.access_token;
        // 續期回應不一定帶新的 refresh_token，沒有就沿用舊的。
        if let Some(rt) = body.refresh_token.filter(|s| !s.is_empty()) {
            self.tokens.refresh_token = rt;
        } else if let Some(old) = keep_refresh {
            self.tokens.refresh_token = old;
        }
        // 提早 60 秒到期，免得剛好卡在邊界上發出去一個過期請求。
        self.tokens.expires_at = config::now_secs() + body.expires_in.unwrap_or(3600) as f64 - 60.0;
        // 續期回應沒帶 scope 就留著原本的（Python 版是 dict.update，行為相同）
        if body.scope.is_some() {
            self.tokens.scope = body.scope;
        }
        let _ = config::write_json(&self.tokens_path, &self.tokens);
    }

    fn access_token(&mut self) -> Option<String> {
        if self.tokens.access_token.is_empty() {
            return None;
        }
        if !self.tokens.valid_now() {
            if config::now_secs() < self.refresh_retry_at {
                return None;
            }
            if !self.refresh() {
                self.refresh_retry_at = config::now_secs() + 60.0;
                return None;
            }
        }
        Some(self.tokens.access_token.clone())
    }

    // ---------------------------- 請求 ----------------------------

    pub fn api(
        &mut self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, ApiError> {
        self.api_inner(method, path, query, body, true)
    }

    fn api_inner(
        &mut self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<serde_json::Value>,
        retry: bool,
    ) -> Result<Option<serde_json::Value>, ApiError> {
        if config::now_secs() < self.rate_limit_until {
            return Err(ApiError::RateLimited { until: self.rate_limit_until });
        }
        let token = self.access_token().ok_or(ApiError::NotAuthed)?;

        let mut req = self
            .http
            .request(method.clone(), format!("{API}{path}"))
            .bearer_auth(&token);
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(ref b) = body {
            req = req.json(b);
        }
        let resp = match req.send() {
            Ok(r) => r,
            Err(e) => {
                self.log_api(&method, path, "net-error");
                return Err(ApiError::Net(e.to_string()));
            }
        };
        let status = resp.status().as_u16();
        if status == 429 {
            let ra = resp.headers().get("Retry-After").and_then(|v| v.to_str().ok()).unwrap_or("?");
            self.log_api(&method, path, &format!("429 retry-after={ra}"));
        } else {
            self.log_api(&method, path, &status.to_string());
        }

        if status == 401 && retry {
            return if self.refresh() {
                self.api_inner(method, path, query, body, false)
            } else {
                Err(ApiError::NotAuthed)
            };
        }
        if status == 429 {
            let wait: u64 = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(5);
            // 短暫的塞車就地重試一次；配額爆掉時 Retry-After 可能是好幾小時，
            // 那種絕對不能 sleep（會把輪詢執行緒鎖死），改成記下解除時間，
            // 在那之前所有請求直接不發。
            if wait <= 5 && retry {
                std::thread::sleep(std::time::Duration::from_secs(wait + 1));
                return self.api_inner(method, path, query, body, false);
            }
            self.rate_limit_until = config::now_secs() + wait as f64;
            return Err(ApiError::RateLimited { until: self.rate_limit_until });
        }
        if status == 204 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(ApiError::Status(status));
        }
        let text = resp.text().map_err(|e| ApiError::Net(e.to_string()))?;
        if text.is_empty() {
            return Ok(None);
        }
        Ok(serde_json::from_str(&text).ok())
    }

    fn log_api(&self, method: &reqwest::Method, path: &str, status: &str) {
        use std::io::Write;
        // 超過 512 KB 就換新檔，舊的留一份
        if std::fs::metadata(&self.log_path).is_ok_and(|m| m.len() > 512 * 1024) {
            let _ = std::fs::rename(&self.log_path, self.log_path.with_extension("old"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.log_path) {
            let _ = writeln!(f, "{:.0} {method} {path} {status}", config::now_secs());
        }
    }

    pub fn get(&mut self, path: &str, query: &[(&str, String)]) -> Result<Option<serde_json::Value>, ApiError> {
        self.api(reqwest::Method::GET, path, query, None)
    }

    /// 可以接手播放的裝置。空的代表 Spotify 一個都沒在線上 ——
    /// 手機或桌面版至少要開著、播過一下，才會出現在這裡。
    pub fn devices(&mut self) -> Result<Vec<Device>, ApiError> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Resp {
            devices: Vec<Device>,
        }
        let v = self.get("/me/player/devices", &[])?.unwrap_or_default();
        Ok(serde_json::from_value::<Resp>(v).unwrap_or_default().devices)
    }

    /// 把播放轉到另一個裝置，並且接著播（`play: true`，跟 Python 版一樣）。
    pub fn transfer(&mut self, device_id: &str) -> Result<(), ApiError> {
        self.api(
            reqwest::Method::PUT,
            "/me/player",
            &[],
            Some(serde_json::json!({ "device_ids": [device_id], "play": true })),
        )
        .map(|_| ())
    }

    /// 抓完整收藏專輯清單，只留畫面用得到的欄位。
    pub fn fetch_all_albums(
        &mut self,
        mut on_progress: impl FnMut(usize, usize),
    ) -> Result<Vec<Album>, ApiError> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let page = self
                .get("/me/albums", &[("limit", "50".into()), ("offset", offset.to_string())])?
                .ok_or(ApiError::Status(204))?;
            let items = page["items"].as_array().cloned().unwrap_or_default();
            let total = page["total"].as_u64().unwrap_or(0) as usize;
            for it in &items {
                let a = &it["album"];
                // images 依序是 640 / 300 / 64。
                let imgs: Vec<&str> = a["images"]
                    .as_array()
                    .map(|v| v.iter().filter_map(|i| i["url"].as_str()).collect())
                    .unwrap_or_default();
                let art = imgs.first().copied().unwrap_or("");
                out.push(Album {
                    id: a["id"].as_str().unwrap_or_default().to_string(),
                    uri: a["uri"].as_str().unwrap_or_default().to_string(),
                    name: a["name"].as_str().unwrap_or_default().to_string(),
                    artist: a["artists"]
                        .as_array()
                        .map(|v| {
                            v.iter()
                                .filter_map(|x| x["name"].as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default(),
                    year: a["release_date"].as_str().unwrap_or_default().chars().take(4).collect(),
                    added: it["added_at"].as_str().unwrap_or_default().to_string(),
                    art: art.to_string(),
                    art_sm: imgs.get(1).copied().unwrap_or(art).to_string(),
                    tracks: a["total_tracks"].as_u64().unwrap_or(0) as u32,
                    kind: "album".into(),
                    ..Default::default()
                });
            }
            on_progress(out.len(), total);
            if items.len() < 50 {
                break;
            }
            offset += 50;
        }
        Ok(out)
    }

    /// 使用者的播放清單（自己建的＋追蹤的），欄位對齊專輯好讓封面流混著放。
    ///
    /// 2026-02 起開發者模式只讀得到「自己建立或共同編輯」的清單內容，
    /// 追蹤的清單只有封面與名稱、讀不到曲目 —— `own` 標記哪些能展開。
    pub fn fetch_all_playlists(&mut self) -> Result<Vec<Album>, ApiError> {
        let me = self.get("/me", &[])?.unwrap_or_default();
        let my_id = me["id"].as_str().unwrap_or("").to_string();
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let page = self
                .get("/me/playlists", &[("limit", "50".into()), ("offset", offset.to_string())])?
                .unwrap_or_default();
            let items = page["items"].as_array().cloned().unwrap_or_default();
            for pl in &items {
                // API 偶爾回 null 項目
                if pl.is_null() {
                    continue;
                }
                let owner = &pl["owner"];
                // 新欄位是 items，舊的叫 tracks；非自己的清單兩個都可能不存在
                let cnt = pl["items"]["total"].as_u64().or_else(|| pl["tracks"]["total"].as_u64());
                let (art, art_sm) = images(&pl["images"]);
                let owner_id = owner["id"].as_str().unwrap_or("");
                let id = str_of(&pl["id"]);
                // 用邀請連結加入的協作者，API 照樣回 collaborative=false，
                // 也沒有別的欄位分得出來 —— 別人的清單只能實際讀一次試試
                let own = (!my_id.is_empty() && owner_id == my_id)
                    || pl["collaborative"].as_bool().unwrap_or(false)
                    || self.playlist_readable(&id)?;
                let n = out.len() as u32;
                out.push(Album {
                    id,
                    uri: str_of(&pl["uri"]),
                    name: str_of(&pl["name"]),
                    artist: owner["display_name"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(owner_id)
                        .to_string(),
                    art,
                    art_sm,
                    tracks: cnt.unwrap_or(0) as u32,
                    kind: "playlist".into(),
                    order: Some(n),
                    own,
                    ..Default::default()
                });
            }
            offset += items.len();
            if items.is_empty() || offset as u64 >= page["total"].as_u64().unwrap_or(0) {
                break;
            }
        }
        Ok(out)
    }

    /// 別人的清單讀不讀得到內容。被拒（403／404）就是讀不到；
    /// 限流與斷線往上丟，別因為一時抓不到就把清單標成不能展開。
    fn playlist_readable(&mut self, pid: &str) -> Result<bool, ApiError> {
        match self.get(&format!("/playlists/{pid}/items"), &[("limit", "1".into())]) {
            Ok(v) => Ok(v.is_some_and(|v| v["items"].is_array())),
            Err(ApiError::Status(_) | ApiError::NotReadable) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// 專輯裡的曲目。這個端點不帶封面與專輯名，呼叫端用專輯那張補上。
    pub fn fetch_album_tracks(&mut self, aid: &str) -> Result<Vec<Album>, ApiError> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let page = self
                .get(&format!("/albums/{aid}/tracks"), &[("limit", "50".into()), ("offset", offset.to_string())])?
                .unwrap_or_default();
            let items = page["items"].as_array().cloned().unwrap_or_default();
            for (k, t) in items.iter().enumerate() {
                if t.is_null() {
                    continue;
                }
                out.push(track_of(t, out.len(), offset + k));
            }
            offset += items.len();
            if items.is_empty() || offset as u64 >= page["total"].as_u64().unwrap_or(0) {
                break;
            }
        }
        Ok(out)
    }

    /// 清單裡的曲目。2026-02 起端點改名 `/items`、每筆的曲目鍵 track→item，兩邊都接。
    pub fn fetch_playlist_tracks(&mut self, pid: &str) -> Result<Vec<Album>, ApiError> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        let mut path = format!("/playlists/{pid}/items");
        loop {
            let q = [
                ("limit", "100".to_string()),
                ("offset", offset.to_string()),
                ("additional_types", "track".to_string()),
            ];
            let page = match self.get(&path, &q) {
                // 還沒換到新端點的帳號：退回舊的 /tracks
                Err(ApiError::Status(404)) if offset == 0 && path.ends_with("/items") => {
                    path = format!("/playlists/{pid}/tracks");
                    continue;
                }
                other => other?.unwrap_or_default(),
            };
            let Some(items) = page["items"].as_array().cloned() else {
                return Err(ApiError::NotReadable);
            };
            for (k, it) in items.iter().enumerate() {
                let t = if it["item"].is_object() { &it["item"] } else { &it["track"] };
                // episode 之類的跳過
                if !t.is_object() || t["type"].as_str().unwrap_or("track") != "track" {
                    continue;
                }
                let mut tr = track_of(t, out.len(), offset + k);
                let al = &t["album"];
                let (art, art_sm) = images(&al["images"]);
                tr.album = str_of(&al["name"]);
                tr.album_uri = str_of(&al["uri"]);
                tr.art = art;
                tr.art_sm = art_sm;
                tr.is_local = t["is_local"].as_bool().unwrap_or(false);
                out.push(tr);
            }
            offset += items.len();
            if items.is_empty() || offset as u64 >= page["total"].as_u64().unwrap_or(0) {
                break;
            }
        }
        Ok(out)
    }

    /// 播專輯或播放清單。給 `track_uri` 就從那首開始，下一首接清單的下一首。
    /// 本機檔案（`spotify:local:`）不能當 offset 的 uri，改用位置。
    pub fn play_context(&mut self, uri: &str, track_uri: Option<&str>, pos: u32) -> Result<(), ApiError> {
        let offset = match track_uri {
            Some(t) if t.starts_with("spotify:track:") => serde_json::json!({ "uri": t }),
            _ => serde_json::json!({ "position": pos }),
        };
        self.api(
            reqwest::Method::PUT,
            "/me/player/play",
            &[],
            Some(serde_json::json!({ "context_uri": uri, "offset": offset })),
        )
        .map(|_| ())
    }

    /// Spotify 自己的音量（不是 Windows 的）。SMTC 不提供，只能問 Web API。
    /// 回傳 (音量, 裝置允不允許遠端調)；沒有裝置是 `Ok(None)`。
    pub fn volume(&mut self) -> Result<Option<(Option<u8>, bool)>, ApiError> {
        let st = self.get("/me/player", &[])?.unwrap_or_default();
        let dev = &st["device"];
        if !dev.is_object() {
            return Ok(None);
        }
        Ok(Some((
            dev["volume_percent"].as_u64().map(|v| v.min(100) as u8),
            dev["supports_volume"].as_bool().unwrap_or(true),
        )))
    }

    pub fn set_volume(&mut self, pct: u8) -> Result<(), ApiError> {
        self.api(
            reqwest::Method::PUT,
            "/me/player/volume",
            &[("volume_percent", pct.min(100).to_string())],
            None,
        )
        .map(|_| ())
    }

    /// 收藏／取消收藏一首歌。2026-02 起 `/me/tracks` 那組被拿掉，改走 `/me/library`。
    pub fn set_liked(&mut self, uri: &str, on: bool) -> Result<(), ApiError> {
        let m = if on { reqwest::Method::PUT } else { reqwest::Method::DELETE };
        self.api(m, "/me/library", &[("uris", uri.to_string())], None).map(|_| ())
    }

    pub fn liked(&mut self, uri: &str) -> Result<bool, ApiError> {
        let v = self.get("/me/library/contains", &[("uris", uri.to_string())])?;
        Ok(v.and_then(|v| v.get(0).and_then(|b| b.as_bool())).unwrap_or(false))
    }
}

fn str_of(v: &serde_json::Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// (大圖, 小圖)。專輯固定是 640/300/64；播放清單可能只有一張，也可能是 null。
fn images(v: &serde_json::Value) -> (String, String) {
    let urls: Vec<&str> = v
        .as_array()
        .map(|a| a.iter().filter_map(|i| i["url"].as_str()).collect())
        .unwrap_or_default();
    let big = urls.first().copied().unwrap_or("").to_string();
    let small = urls.get(1).map(|s| s.to_string()).unwrap_or_else(|| big.clone());
    (big, small)
}

/// 曲目共同的欄位。`n` 是在輸出清單裡的第幾首（拿來讓 id 唯一），
/// `pos` 是在原清單裡的位置。
fn track_of(t: &serde_json::Value, n: usize, pos: usize) -> Album {
    let tid = t["id"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    Album {
        // 同一首可能在清單裡出現兩次，id 加上序號才不會撞
        id: format!("t{n}:{}", tid.clone().unwrap_or_else(|| str_of(&t["uri"]))),
        track_id: tid.unwrap_or_default(),
        uri: str_of(&t["uri"]),
        name: str_of(&t["name"]),
        artist: t["artists"]
            .as_array()
            .map(|v| v.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default(),
        duration: t["duration_ms"].as_i64().unwrap_or(0),
        pos: Some(pos as u32),
        kind: "track".into(),
        ..Default::default()
    }
}

fn random_bytes(n: usize) -> Vec<u8> {
    // rand 各版本的 API 一直在改，用最不會壞的那個形式。
    (0..n).map(|_| rand::random::<u8>()).collect()
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 清單曲目新舊兩種鍵都接() {
        // 2026-02 起是 item，以前是 track；episode 要跳過，但位置照算
        let new = json!({ "id": "a", "uri": "spotify:track:a", "name": "甲", "type": "track",
                          "artists": [{ "name": "VitroDeck" }, { "name": "某某" }], "duration_ms": 1000 });
        let t = track_of(&new, 0, 3);
        assert_eq!(t.uri, "spotify:track:a");
        assert_eq!(t.artist, "VitroDeck, 某某");
        assert_eq!(t.pos, Some(3));
        assert_eq!(t.id, "t0:a");
        assert!(t.is_track() && !t.can_drill());
        // 本機檔案沒有 id，用 uri 當 id 的後半
        let local = json!({ "id": null, "uri": "spotify:local:x", "name": "本機" });
        assert_eq!(track_of(&local, 2, 5).id, "t2:spotify:local:x");
    }

    #[test]
    fn 圖片只有一張時大小圖同一張() {
        assert_eq!(images(&json!([{ "url": "a" }])), ("a".into(), "a".into()));
        assert_eq!(images(&json!([{ "url": "a" }, { "url": "b" }])), ("a".into(), "b".into()));
        assert_eq!(images(&serde_json::Value::Null), (String::new(), String::new()));
    }

    #[test]
    fn 能不能展開與副標() {
        let al = Album { uri: "spotify:album:x".into(), kind: "album".into(), artist: "VitroDeck".into(),
                         year: "1989".into(), tracks: 10, ..Default::default() };
        assert!(al.can_drill());
        assert_eq!(al.subtitle(), "VitroDeck  ·  1989  ·  10 首");
        // 舊版 Rust 寫的快取沒有 kind：當專輯
        let old = Album { uri: "spotify:album:x".into(), ..Default::default() };
        assert!(old.can_drill());
        let ghost = Album { uri: "smtc:album:1".into(), ..Default::default() };
        assert!(!ghost.can_drill(), "沒對應到 Spotify 的幽靈只能直接播");
        let mine = Album { kind: "playlist".into(), own: true, artist: "我".into(), tracks: 3, ..Default::default() };
        assert!(mine.can_drill());
        assert_eq!(mine.subtitle(), "我  ·  3 首  ·  播放清單");
        let follow = Album { kind: "playlist".into(), artist: "別人".into(), ..Default::default() };
        assert!(!follow.can_drill(), "追蹤的清單讀不到內容，直接播");
        assert_eq!(follow.subtitle(), "別人  ·  追蹤的清單");
    }

    #[test]
    fn 讀得懂_python_版的_playlists_json() {
        let s = r#"[{"id":"p","uri":"spotify:playlist:p","name":"清單","artist":"我","year":"",
                     "added":"","art":"a","art_sm":"a","tracks":4,"kind":"playlist","order":0,"own":true}]"#;
        let v: Vec<Album> = serde_json::from_str(s).unwrap();
        assert!(v[0].is_playlist() && v[0].own && v[0].order == Some(0));
        // 寫回去：專輯不會多出曲目專用的欄位
        let al = Album { kind: "album".into(), ..Default::default() };
        let out = serde_json::to_string(&al).unwrap();
        assert!(!out.contains("track_id") && !out.contains("\"pos\"") && !out.contains("own"), "{out}");
    }
}
