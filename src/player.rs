//! 播放狀態來源 —— 對應 Python 版 `app.py` 的 `Poller`。
//!
//! 優先 SMTC（本機 Spotify），沒有 session 才退回 Web API —— 後者看得到
//! 遠端裝置，但要花配額，所以只在真的需要時用，而且間隔拉長。
//!
//! 輪詢間隔的取捨：Spotify 有兩套獨立機制，速率限制看「滾動 30 秒視窗」，
//! 配額看總量。會中的是配額（QUOTA_EXCEEDED），所以重點是壓低**總請求數**
//! 而不只是瞬間頻率。播放位置由畫面端自己內插，輪詢只負責抓換歌與修正漂移，
//! 所以間隔可以拉得比直覺長很多，再靠兩個機制補回即時性：
//!
//! 1. **預測曲末**：剩餘時間比間隔短時就排在歌快結束那一刻醒來，
//!    自然換歌會被立刻抓到，不必為此全程高頻輪詢。
//! 2. **看不到時拉長**：最小化或切走時根本沒人在看。
//!
//! SMTC 是本機的、免費的、一次 0.3 毫秒，所以可以打很密。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::albums::Matcher;
use crate::config::{self, Config, Paths};
use crate::lyrics::{self, Lyrics};
use crate::smtc::{self, Repeat, Smtc};
use crate::spotify::{ApiError, Spotify};

const SMTC_TICK: Duration = Duration::from_millis(500);
const WEB_IDLE: Duration = Duration::from_secs(30);
const WEB_HIDDEN: Duration = Duration::from_secs(120);
const POLL_BLOCKED: Duration = Duration::from_secs(30);
/// 跳轉之後多久內不信任讀回來的位置（見 `seek_hold`）
const SEEK_HOLD: Duration = Duration::from_secs(3);
/// 讀回來的位置跟預期差多少以內，算是播放器已經跳過去了
const SEEK_AGREE_MS: i64 = 1500;
/// 換歌後停留多久才去問曲目 URI／愛心／音量。連續切歌時只有最後停下來的那首會發請求。
const META_SETTLE: Duration = Duration::from_millis(1500);
/// 展開過的曲目在磁碟上的有效期。清單的首數變了就算過期，不用等到期。
const TRACKS_TTL_PLAYLIST: f64 = 7.0 * 86400.0;
const TRACKS_TTL_ALBUM: f64 = 60.0 * 86400.0;

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct CachedTracks {
    updated_at: f64,
    /// 抓的當下清單宣稱的首數；之後收藏清單重抓發現首數不同就當過期
    total: u32,
    tracks: Vec<crate::spotify::Album>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Art {
    Url(String),
    /// 比對不到收藏就用 SMTC 那張 300px 縮圖。原生版不需要像 Python 版那樣
    /// 繞一圈自架 HTTP server 供圖，位元組直接進材質。
    SmtcThumb,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    pub playing: bool,
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_uri: String,
    pub art: Art,
    pub duration_ms: i64,
    pub progress_ms: i64,
    pub device: String,
    pub shuffle: bool,
    pub repeat: Repeat,
    /// Spotify 給 SMTC 的原始時間軸（見 `smtc::Track::stamp`），web 來源沒有
    pub smtc_stamp: Option<(i64, i64, i64)>,
    /// 讀到這筆狀態的本機時刻，畫面端拿它內插播放位置。
    pub at: Instant,
    /// smtc / web
    pub source: &'static str,
    /// 曲目的 Spotify URI。只有 web 來源有；SMTC 不給，換歌時另外問（見 [`TrackMeta`]）
    pub track_uri: String,
    /// Spotify 裝置的音量。同樣只有 web 來源有
    pub volume: Option<u8>,
    pub supports_volume: bool,
}

/// 換歌時補的資料：曲目 URI、有沒有收藏。愛心靠它，拿到之前愛心是灰的，免得收藏到別首。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackMeta {
    /// 對應 `NowPlaying::id`
    pub id: String,
    pub uri: String,
    /// `None` = 還不知道
    pub liked: Option<bool>,
}

/// Spotify 的音量。`vol` 是 `None` 代表還沒問到。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Volume {
    pub vol: Option<u8>,
    pub supported: bool,
}

impl Default for Volume {
    fn default() -> Self {
        Self { vol: None, supported: true }
    }
}

/// 展開專輯／清單的結果。`src` 是那張的 uri，畫面端拿它確認回來的是不是自己要的。
pub struct Tracks {
    pub src: String,
    pub res: Result<Vec<crate::spotify::Album>, String>,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Cmd {
    #[allow(dead_code)] // 播放／暫停目前都走 Toggle
    Play,
    #[allow(dead_code)]
    Pause,
    Toggle,
    Next,
    Prev,
    Seek(i64),
    Shuffle(bool),
    Repeat(Repeat),
    /// 前端回報視窗看不看得到，用來決定輪詢間隔。
    Visible(bool),
    /// 主視窗縮到系統列／叫回來。縮著的時候沒人看歌詞與愛心，不抓
    Tray(bool),
    /// 迷你視窗的歌詞模式開著：縮在系統列時也要抓歌詞（曲目資訊照樣不抓）
    WantLyrics(bool),
    RefreshAlbums,
    /// 播一整張專輯（uri, 專輯名）。SMTC 做不到這件事（它只有上一首／下一首），
    /// 所以這條一定走 Web API。名字只拿來寫通知列。
    PlayAlbum(String, String),
    /// 開瀏覽器授權。會把 poller 擋住幾十秒 —— 那段時間本來也沒東西好輪詢。
    Login,
    Logout,
    /// 「關於」面板的中斷連結：登出，再刪掉收藏、清單、比對結果與歌詞快取。
    /// Spotify 條款 I.1.c 要求要有這個（Python 版的 `logout()`）
    Disconnect,
    /// 設定精靈存了新的 client_id。這條執行緒有自己的一份設定，要跟著換
    SetClientId(String),
    /// 抓裝置清單，結果寫進 `Shared::devices`
    Devices,
    /// 把播放轉到這個裝置
    Transfer(String),
    /// 抓專輯或播放清單的曲目，結果寫進 `Shared::tracks`
    LoadTracks(crate::spotify::Album),
    /// 以專輯／清單為 context，從某一首開始播。`name` 只拿來寫通知列
    PlayContext { uri: String, track_uri: String, pos: u32, name: String },
    /// Spotify 自己的音量（0..100）
    SetVolume(u8),
    /// 問一次目前的音量（滑鼠移到音量上、換裝置之後）
    SyncVolume,
    /// 收藏／取消收藏一首歌
    SetLiked { uri: String, on: bool },
    Quit,
}

#[derive(Default)]
pub struct Shared {
    pub now: Mutex<Option<NowPlaying>>,
    pub lyrics: Mutex<Option<Lyrics>>,
    pub thumb: Mutex<Option<Vec<u8>>>,
    /// 換歌時 +1，畫面端靠它知道要重抓資源，不必比對整個結構。
    pub generation: AtomicU64,
    /// 被 Spotify 限流到什麼時候（Unix 秒，0 代表沒有）。爆配額是靜默的，
    /// 一定要顯示出來，不然使用者只會覺得「怎麼突然不動了」。
    pub rate_limited_until: Mutex<f64>,
    pub authed: AtomicBool,
    /// 權杖少了 `config::SCOPES` 裡的權限（舊版登入的），要重新授權一次。
    pub need_reauth: AtomicBool,
    /// 登入流程的狀態，給設定面板顯示。空字串代表沒有在進行中。
    pub login_msg: Mutex<String>,
    /// 收藏清單重抓過幾次。畫面端比對這個數字才知道要重讀 `albums.json` ——
    /// 不然抓回來的東西要等下次開程式才看得到。
    pub albums_rev: AtomicU64,
    /// 要給使用者看的一句話（通知列）。畫面端每幀 `take()` 走。
    /// 播放控制失敗是在這條執行緒上才知道的，不送出來的話就是靜默失敗。
    pub toast: Mutex<Option<String>>,
    /// 裝置選單的內容。`None` = 還在抓；`Some(Err)` = 抓不到（講原因）。
    pub devices: Mutex<Option<Result<Vec<crate::spotify::Device>, String>>>,
    /// 播放失敗的原因是「沒有裝置」時 +1。畫面端看到它變了就自己打開裝置選單，
    /// 跟 web 版 `playAlbum` 的行為一樣 —— 光講「找不到裝置」卻不給選，使用者只能乾瞪眼。
    pub no_device_rev: AtomicU64,
    /// 展開的曲目。`tracks_rev` 變了畫面端才去拿
    pub tracks: Mutex<Option<Tracks>>,
    pub tracks_rev: AtomicU64,
    pub meta: Mutex<TrackMeta>,
    pub volume: Mutex<Volume>,
    /// 音量從這條執行緒更新時 +1。畫面端剛動過滑桿的話會先不理
    pub volume_rev: AtomicU64,
}

impl Shared {
    pub fn toast(&self, msg: impl Into<String>) {
        *self.toast.lock().unwrap() = Some(msg.into());
    }
}

pub struct Handle {
    pub shared: Arc<Shared>,
    pub tx: Sender<Cmd>,
}

pub fn spawn(cfg: Config, paths: Paths) -> Handle {
    let shared = Arc::new(Shared::default());
    let (tx, rx) = std::sync::mpsc::channel();
    let out = shared.clone();
    std::thread::Builder::new()
        .name("poller".into())
        .spawn(move || run(cfg, paths, out, rx))
        .expect("poller 開不起來");
    Handle { shared, tx }
}

fn run(mut cfg: Config, paths: Paths, shared: Arc<Shared>, rx: Receiver<Cmd>) {
    // WinRT 物件是執行緒相關的，所以 SMTC 整個生命週期都關在這條執行緒上。
    smtc::init_apartment();
    let smtc = Smtc::new("spotify").ok();
    if smtc.is_none() {
        crate::warn!("SMTC 起不來，只能靠 Web API（會吃配額）");
    }

    let mut sp = Spotify::new(&cfg, paths.tokens.clone());
    let mut matcher = Matcher::load(paths.resolved.clone());
    let lyrics_tx = spawn_lyrics(&cfg, &paths, &shared);
    // 歌詞自動對齊：配錯歌時要叫歌詞執行緒重抓
    crate::align::start(&cfg, &paths, shared.clone(), lyrics_tx.clone());
    shared.authed.store(sp.logged_in(), Ordering::Relaxed);
    shared.need_reauth.store(!sp.tokens.missing_scopes().is_empty(), Ordering::Relaxed);

    let cached: Vec<crate::spotify::Album> = config::read_json(&paths.albums);
    matcher.rebuild_index(&cached);

    let mut visible = true;
    let mut in_tray = false;
    let mut want_lyrics = false;
    let mut web_next = Instant::now();
    let mut last_id = String::new();
    // 縮在系統列時換歌，這首的曲目資訊（愛心、音量）／歌詞被跳過了沒。叫回來時只補跳過的那首：
    // 以前一律當作剛換歌，縮著期間根本沒換歌也要重打 `/me/player` 與 `/me/library/contains`
    let mut meta_skipped = false;
    let mut lyrics_skipped = false;
    // 剛跳轉的目標位置與時刻。Spotify 回報給 SMTC 的位置會晚一下才更新，
    // 這段時間讀到的還是跳轉前的位置；直接照登的話，畫面已經先跳過去的
    // 進度條會閃回原處、再閃回來。
    let mut seek_hold: Option<(i64, Instant)> = None;
    // 自然接歌後 Spotify 給 SMTC 的位置早約半秒，學出來扣掉（見 lead.rs）
    let mut lead = crate::lead::Lead::load(paths.cache.join("smtc_lead.json"));
    // 換歌後等 META_SETTLE 才問曲目資訊：(那首的 id, 到期時間)
    let mut pending_meta: Option<(String, Instant)> = None;
    let mut tracks_cache: std::collections::HashMap<String, CachedTracks> =
        config::read_json(&paths.tracks);
    // 睡覺時被指令叫醒，叫醒它的那個指令先放這，下一輪第一個處理
    let mut woke: Option<Cmd> = None;

    loop {
        // ---- 先收指令 ----
        while let Some(cmd) = woke.take().or_else(|| rx.try_recv().ok()) {
            match cmd {
                Cmd::Quit => return,
                Cmd::Visible(v) => visible = v,
                Cmd::WantLyrics(v) => {
                    // 縮著的時候才開：這首的歌詞之前被跳過了，當作剛換歌補抓
                    if v && !want_lyrics && in_tray && lyrics_skipped {
                        last_id.clear();
                    }
                    want_lyrics = v;
                }
                Cmd::Tray(v) => {
                    in_tray = v;
                    // 叫回來：縮著時跳過了這首的歌詞或曲目資訊，就當作剛換歌補抓
                    if !v && (meta_skipped || lyrics_skipped) {
                        last_id.clear();
                    }
                }
                Cmd::RefreshAlbums => {
                    refresh_albums(&mut sp, &paths, &mut matcher, &shared);
                }
                Cmd::Login => {
                    login(&mut sp, &cfg, &shared);
                    if sp.logged_in() {
                        refresh_albums(&mut sp, &paths, &mut matcher, &shared);
                    }
                }
                Cmd::SetClientId(id) => {
                    sp.set_client_id(&id);
                    cfg.client_id = id;
                }
                Cmd::Devices => {
                    let got = sp.devices().map_err(|e| e.to_string());
                    *shared.devices.lock().unwrap() = Some(got);
                }
                Cmd::Transfer(id) => match sp.transfer(&id) {
                    Ok(()) => shared.toast("已切換裝置"),
                    Err(e) => shared.toast(format!("切換失敗：{e}")),
                },
                Cmd::LoadTracks(src) => {
                    let id = src.uri.rsplit(':').next().unwrap_or("").to_string();
                    let ttl = if src.is_playlist() { TRACKS_TTL_PLAYLIST } else { TRACKS_TTL_ALBUM };
                    let hit = tracks_cache.get(&src.uri).filter(|c| {
                        !c.tracks.is_empty()
                            && c.total == src.tracks
                            && config::now_secs() - c.updated_at < ttl
                    });
                    let res = if let Some(c) = hit {
                        Ok(c.tracks.clone())
                    } else {
                        let r = if src.is_playlist() {
                            sp.fetch_playlist_tracks(&id)
                        } else {
                            sp.fetch_album_tracks(&id)
                        };
                        if let Ok(v) = &r
                            && !v.is_empty() {
                                tracks_cache.insert(
                                    src.uri.clone(),
                                    CachedTracks { updated_at: config::now_secs(), total: src.tracks, tracks: v.clone() },
                                );
                                let _ = config::write_json(&paths.tracks, &tracks_cache);
                            }
                        r
                    };
                    let res = res.map_err(|e| tracks_error(&e));
                    *shared.tracks.lock().unwrap() = Some(Tracks { src: src.uri, res });
                    shared.tracks_rev.fetch_add(1, Ordering::Relaxed);
                }
                Cmd::SetVolume(v) => match sp.set_volume(v) {
                    Ok(()) => {}
                    Err(ApiError::Status(403)) => {
                        shared.volume.lock().unwrap().supported = false;
                        shared.volume_rev.fetch_add(1, Ordering::Relaxed);
                        shared.toast("這個裝置不允許遠端調整音量");
                    }
                    Err(ApiError::Status(404)) => shared.toast("找不到播放裝置"),
                    Err(ApiError::RateLimited { .. }) => {
                        shared.toast("Spotify 請求配額用完了，暫時無法調整音量")
                    }
                    Err(e) => shared.toast(format!("調整音量失敗：{e}")),
                },
                Cmd::SyncVolume => {
                    if let Ok(Some((vol, ok))) = sp.volume() {
                        *shared.volume.lock().unwrap() = Volume { vol, supported: ok };
                        shared.volume_rev.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Cmd::SetLiked { uri, on } => {
                    let res = sp.set_liked(&uri, on);
                    let mut m = shared.meta.lock().unwrap();
                    match res {
                        Ok(()) => {
                            if m.uri == uri {
                                m.liked = Some(on);
                            }
                            drop(m);
                            shared.toast(if on { "已加入收藏" } else { "已從收藏移除" });
                        }
                        Err(e) => {
                            // 畫面端是先改了再送的，失敗要改回去
                            if m.uri == uri {
                                m.liked = Some(!on);
                            }
                            drop(m);
                            shared.toast(match e {
                                ApiError::RateLimited { .. } => {
                                    "Spotify 請求配額用完了，暫時無法收藏".to_string()
                                }
                                ApiError::Status(403) => "Spotify 拒絕了收藏操作".into(),
                                e => format!("收藏失敗：{e}"),
                            });
                        }
                    }
                }
                Cmd::Logout => {
                    sp.logout();
                    shared.authed.store(false, Ordering::Relaxed);
                    shared.need_reauth.store(false, Ordering::Relaxed);
                    *shared.login_msg.lock().unwrap() = "已登出".into();
                }
                Cmd::Disconnect => {
                    sp.logout();
                    for p in [&paths.albums, &paths.playlists, &paths.resolved, &paths.tracks] {
                        let _ = std::fs::remove_file(p);
                    }
                    // 歌詞快取是一首一個 json，逐一刪；資料夾本身留著
                    if let Ok(rd) = std::fs::read_dir(&paths.lyrics) {
                        for e in rd.flatten() {
                            if e.path().is_file() {
                                let _ = std::fs::remove_file(e.path());
                            }
                        }
                    }
                    matcher.forget_all();
                    tracks_cache.clear();
                    pending_meta = None;
                    last_id.clear();
                    shared.authed.store(false, Ordering::Relaxed);
                    shared.need_reauth.store(false, Ordering::Relaxed);
                    *shared.login_msg.lock().unwrap() = "已中斷連結".into();
                    // 畫面端看到它變了會重讀 albums.json（已經不在了 → 示範封面）
                    shared.albums_rev.fetch_add(1, Ordering::Relaxed);
                    shared.toast("已中斷連結並刪除本機資料");
                }
                Cmd::Seek(ms) => {
                    seek_hold = Some((ms, Instant::now()));
                    apply_control(&smtc, &mut sp, Cmd::Seek(ms), &shared);
                }
                other => apply_control(&smtc, &mut sp, other, &shared),
            }
        }

        // ---- 讀狀態 ----
        let mut np = smtc.as_ref().and_then(|s| from_smtc(s, &mut sp, &mut matcher, &cfg));
        if np.is_none() && Instant::now() >= web_next {
            np = from_web(&mut sp, &shared);
            // 沒有本機 session 時才會走到這，那通常代表在手機上播，
            // 拉長間隔省配額。
            web_next = Instant::now() + if visible { WEB_IDLE } else { WEB_HIDDEN };
        }
        let lead_ms = np.as_mut().map_or(0, |t| {
            // 這個修正是跟自動對齊一起來的，對齊關著時位置照 Spotify 給的，跟沒有這個功能時一樣
            if !crate::align::enabled() && !crate::alignrec::on() {
                lead.idle();
                return 0;
            }
            let f = lead.fix(&t.id, t.playing, t.duration_ms, t.smtc_stamp);
            t.progress_ms = (t.progress_ms - f).max(0);
            f
        });
        if let (Some((ms, at)), Some(t)) = (seek_hold, np.as_mut()) {
            let expect = ms + if t.playing { at.elapsed().as_millis() as i64 } else { 0 };
            // 對上了、超過時限、或歌已經換了，就放手照實際的
            if (t.progress_ms - expect).abs() < SEEK_AGREE_MS || at.elapsed() > SEEK_HOLD || t.id != last_id {
                seek_hold = None;
            } else {
                t.progress_ms = expect;
                t.at = Instant::now();
            }
        }

        // ---- 換歌 ----
        //
        // **順序很重要**：先把新的這首公開出去，再把 `generation` 加一。
        // 以前是「generation +1 → 抓歌詞（好幾秒）→ 才寫 now」，結果有兩個 bug：
        // 控制列要等歌詞抓完才換（約 10 秒），而展開中的封面流看到 generation 變了
        // 就去找「正在播的那首」，找到的卻還是上一首，被拉回去。
        let changed = np.as_ref().is_some_and(|t| t.id != last_id);
        if changed {
            if let Some(s) = smtc.as_ref() {
                *shared.thumb.lock().unwrap() = s.thumbnail();
            }
            // 上一首的歌詞先換成「載入中」，免得新歌配舊詞
            *shared.lyrics.lock().unwrap() =
                Some(Lyrics { source: "loading".into(), ..Default::default() });
        }
        *shared.now.lock().unwrap() = np.clone();
        crate::alignrec::poll(np.as_ref(), lead_ms);
        crate::align::poll(np.as_ref(), lead.sure());
        // 燈條的拾音燈只看本機：遠端裝置在播時 loopback 抓不到聲音
        crate::ledfeed::set_playing(np.as_ref().is_some_and(|t| t.playing && t.source == "smtc"));
        if let (true, Some(t)) = (changed, np.as_ref()) {
            last_id = t.id.clone();
            shared.generation.fetch_add(1, Ordering::Relaxed);
            // 歌詞交給另一條執行緒：一抓好幾秒，擋在這裡的話這段時間
            // 播放狀態不會更新、按暫停也要排隊
            lyrics_skipped = in_tray && !want_lyrics;
            meta_skipped = in_tray;
            if !lyrics_skipped {
                let _ = lyrics_tx.send(t.clone());
            }
            if !in_tray {
                if t.track_uri.is_empty() {
                    // SMTC 來源：要多問兩個請求，等停下來再問
                    pending_meta = Some((t.id.clone(), Instant::now() + META_SETTLE));
                } else {
                    pending_meta = None;
                    track_meta(&mut sp, t, &shared);
                }
            }
        }
        if let Some((id, due)) = pending_meta.take() {
            match np.as_ref() {
                // 已經換下一首了（下一輪會重新排）或停播了：不用問
                Some(t) if t.id == id && !in_tray => {
                    if Instant::now() >= due {
                        // 曲目 URI（愛心要用）與音量，兩個請求，幾百毫秒
                        track_meta(&mut sp, t, &shared);
                    } else {
                        pending_meta = Some((id, due));
                    }
                }
                // 還沒等到就縮進系統列了：記著，叫回來再補
                Some(t) if t.id == id => meta_skipped = true,
                _ => {}
            }
        }
        // 系統列的提示：滑鼠停在圖示上看得到正在播什麼（圖示沒開就什麼都不做）
        crate::tray::set_tip(&match np.as_ref() {
            Some(t) if !t.title.is_empty() => {
                let tip = format!("{} - {}", t.title, t.artist);
                if crate::zh::enabled() { crate::zh::to_trad(&tip).unwrap_or(tip) } else { tip }
            }
            _ => "VitroDeck".into(),
        });

        // ---- 決定睡多久 ----
        let mut wait = if smtc.is_some() { SMTC_TICK } else { WEB_IDLE };
        if config::now_secs() < sp.rate_limited_until() {
            *shared.rate_limited_until.lock().unwrap() = sp.rate_limited_until();
            wait = wait.max(POLL_BLOCKED);
        } else {
            *shared.rate_limited_until.lock().unwrap() = 0.0;
        }
        if !visible {
            wait = wait.max(Duration::from_secs(2));
        }
        if let Some((_, due)) = &pending_meta {
            wait = wait.min(due.saturating_duration_since(Instant::now()) + Duration::from_millis(20));
        }
        // 預測曲末：剩下的時間比這一輪還短，就排在歌結束那一刻醒來。
        if let Some(ref t) = np
            && t.playing && t.duration_ms > 0 {
                let left = (t.duration_ms - t.progress_ms).max(0) as u64;
                if left < wait.as_millis() as u64 {
                    wait = Duration::from_millis(left + 120);
                }
            }
        // 不能用 sleep：指令送來時要馬上醒。以前睡滿才收，只走 Web API 或被限流時
        // 按暫停要等到 30 秒，主視窗被蓋住時迷你視窗的按鍵也要等 2 秒。
        // 醒來只多讀一次 SMTC；Web API 的輪詢照 `web_next`，不會多打。
        match rx.recv_timeout(wait) {
            Ok(cmd) => woke = Some(cmd),
            Err(RecvTimeoutError::Timeout) => {}
            // 畫面端整個收掉了，沒人會再送指令
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn from_smtc(
    s: &Smtc,
    sp: &mut Spotify,
    matcher: &mut Matcher,
    _cfg: &Config,
) -> Option<NowPlaying> {
    let d = s.read()?;
    let m = matcher.resolve(sp, &d.album, &d.artist);
    let (uri, art) = match m {
        Some(ref m) if !m.art.is_empty() => (m.uri.clone(), Art::Url(m.art.clone())),
        Some(ref m) => (m.uri.clone(), Art::SmtcThumb),
        None => (String::new(), Art::SmtcThumb),
    };
    Some(NowPlaying {
        playing: d.playing,
        // 比對不到真正的 URI 就給一個假的，畫面端仍會把它當幽靈專輯處理。
        album_uri: if uri.is_empty() { format!("smtc:album:{}", d.id) } else { uri },
        id: d.id,
        title: d.title,
        artist: d.artist,
        album: d.album,
        art,
        duration_ms: d.duration_ms,
        progress_ms: d.progress_ms,
        smtc_stamp: d.stamp,
        device: "本機 Spotify".into(),
        shuffle: d.shuffle,
        repeat: d.repeat,
        at: Instant::now(),
        source: "smtc",
        track_uri: String::new(),
        volume: None,
        supports_volume: true,
    })
}

fn from_web(sp: &mut Spotify, shared: &Arc<Shared>) -> Option<NowPlaying> {
    let st = match sp.get("/me/player", &[]) {
        Ok(v) => v?,
        Err(ApiError::NotAuthed) => {
            shared.authed.store(false, Ordering::Relaxed);
            return None;
        }
        Err(_) => return None,
    };
    let it = &st["item"];
    if it.is_null() {
        return None;
    }
    let album = &it["album"];
    let art = album["images"]
        .as_array()
        .and_then(|v| v.first())
        .and_then(|i| i["url"].as_str())
        .unwrap_or("")
        .to_string();
    let artist = it["artists"]
        .as_array()
        .map(|v| {
            v.iter().filter_map(|a| a["name"].as_str()).collect::<Vec<_>>().join(", ")
        })
        .unwrap_or_default();
    let title = it["name"].as_str().unwrap_or("").to_string();
    let album_name = album["name"].as_str().unwrap_or("").to_string();
    Some(NowPlaying {
        playing: st["is_playing"].as_bool().unwrap_or(false),
        // Web API 有真的 track id，但歌詞快取要跟 SMTC 那條路共用同一把鑰匙，
        // 所以一樣用雜湊算。不然同一首歌會被抓兩次、存成兩份。
        id: smtc::track_key(&artist, &title, &album_name),
        title,
        artist,
        album: album_name,
        album_uri: album["uri"].as_str().unwrap_or("").to_string(),
        art: Art::Url(art),
        duration_ms: it["duration_ms"].as_i64().unwrap_or(0),
        progress_ms: st["progress_ms"].as_i64().unwrap_or(0),
        smtc_stamp: None,
        device: st["device"]["name"].as_str().unwrap_or("遠端裝置").to_string(),
        shuffle: st["shuffle_state"].as_bool().unwrap_or(false),
        repeat: match st["repeat_state"].as_str() {
            Some("track") => Repeat::Track,
            Some("context") => Repeat::Context,
            _ => Repeat::Off,
        },
        at: Instant::now(),
        source: "web",
        track_uri: it["uri"].as_str().unwrap_or("").to_string(),
        volume: st["device"]["volume_percent"].as_u64().map(|v| v.min(100) as u8),
        supports_volume: st["device"]["supports_volume"].as_bool().unwrap_or(true),
    })
}

/// 抓歌詞的執行緒。只處理最新的那首 —— 連按下一首的時候，
/// 中間跳過的那幾首就不抓了；抓完之後歌已經換掉的話也不寫回去。
fn spawn_lyrics(cfg: &Config, paths: &Paths, shared: &Arc<Shared>) -> Sender<NowPlaying> {
    let (tx, rx) = std::sync::mpsc::channel::<NowPlaying>();
    let cfg = cfg.clone();
    let mut fetcher = lyrics::Fetcher::new(paths.lyrics.clone());
    let shared = shared.clone();
    std::thread::Builder::new()
        .name("lyrics".into())
        .spawn(move || {
            while let Ok(mut t) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    t = newer;
                }
                let got = fetcher.fetch(&cfg, &t.id, &t.artist, &t.title, &t.album, t.duration_ms, None);
                crate::alignrec::lyrics(&t, &got);
                crate::align::lyrics(&t.id, &got);
                let still = shared.now.lock().unwrap().as_ref().is_some_and(|n| n.id == t.id);
                if still {
                    *shared.lyrics.lock().unwrap() = Some(got);
                }
            }
        })
        .expect("歌詞執行緒開不起來");
    tx
}

/// 重抓收藏清單，寫檔，然後**通知畫面端**。
///
/// 少了最後那一步，抓回來的東西要等下次開程式才看得到 —— 這種「做了
/// 但看起來沒做」最容易被當成沒反應而重按好幾次。
fn refresh_albums(sp: &mut Spotify, paths: &Paths, matcher: &mut Matcher, shared: &Arc<Shared>) {
    match sp.fetch_all_albums(|_, _| {}) {
        Ok(list) => {
            let n = list.len();
            let _ = config::write_json(&paths.albums, &list);
            // 專輯比對只看收藏的專輯，播放清單不算
            matcher.rebuild_index(&list);
            // 播放清單抓不到（缺 scope、被限流）就維持舊的那份，
            // 專輯照常 —— 一邊壞掉不該拖垮另一邊
            let lists = if sp.tokens.missing_scopes().iter().any(|s| s.starts_with("playlist-")) {
                Err("需要重新授權才讀得到".to_string())
            } else {
                sp.fetch_all_playlists().map_err(|e| e.to_string())
            };
            let msg = match lists {
                Ok(pl) => {
                    let _ = config::write_json(&paths.playlists, &pl);
                    format!("{n} 張專輯、{} 個播放清單", pl.len())
                }
                Err(e) => format!("{n} 張專輯（播放清單{e}）"),
            };
            shared.albums_rev.fetch_add(1, Ordering::Relaxed);
            *shared.login_msg.lock().unwrap() = format!("已登入 · {msg}");
            shared.toast(format!("已更新：{msg}"));
        }
        Err(e) => {
            *shared.login_msg.lock().unwrap() = format!("收藏清單抓不到：{e}");
            shared.toast(format!("收藏清單抓不到：{e}"));
        }
    }
}

/// 展開失敗的原因，翻成使用者看得懂的話（web 版的 `DRILL_ERR`）。
fn tracks_error(e: &ApiError) -> String {
    match e {
        ApiError::NotReadable | ApiError::Status(403) | ApiError::Status(404) => {
            "Spotify 不開放讀取這個播放清單的內容".into()
        }
        ApiError::RateLimited { .. } => "Spotify 請求配額用完了，暫時讀不到曲目".into(),
        ApiError::NotAuthed => "需要重新授權才能讀取曲目".into(),
        e => format!("讀取失敗：{e}"),
    }
}

/// 換歌時補一次曲目 URI、是否已收藏、Spotify 自己的音量（Python 版的 `_track_meta`）。
///
/// SMTC 不給任何 Spotify ID，也沒有 Spotify 的音量，所以 SMTC 來源時
/// 多問一次 `/me/player`（每首歌一次）。拿到的曲目要跟 SMTC 那首對得上
/// 才算數 —— 遠端裝置在播別的、或剛好換歌的時間差，都會讓愛心掛到別首。
fn track_meta(sp: &mut Spotify, t: &NowPlaying, shared: &Arc<Shared>) {
    // 先清掉上一首的，免得查詢那幾百毫秒裡愛心還掛著上一首的狀態
    *shared.meta.lock().unwrap() = TrackMeta { id: t.id.clone(), ..Default::default() };
    if !sp.logged_in() {
        return;
    }
    let mut uri = t.track_uri.clone();
    let mut vol = Volume { vol: t.volume, supported: t.supports_volume };
    if uri.is_empty()
        && let Ok(Some(st)) = sp.get("/me/player", &[]) {
            let it = &st["item"];
            if !it.is_null()
                && crate::albums::norm_name(it["name"].as_str().unwrap_or(""))
                    == crate::albums::norm_name(&t.title)
            {
                uri = it["uri"].as_str().unwrap_or("").to_string();
                vol = Volume {
                    vol: st["device"]["volume_percent"].as_u64().map(|v| v.min(100) as u8),
                    supported: st["device"]["supports_volume"].as_bool().unwrap_or(true),
                };
            }
        }
    let liked = if uri.starts_with("spotify:track:") { sp.liked(&uri).ok() } else { None };
    *shared.meta.lock().unwrap() = TrackMeta { id: t.id.clone(), uri, liked };
    if vol.vol.is_some() || !vol.supported {
        *shared.volume.lock().unwrap() = vol;
        shared.volume_rev.fetch_add(1, Ordering::Relaxed);
    }
}

/// 走一次 OAuth。**整條是同步的，會把 poller 擋住** —— 授權那幾十秒
/// 本來也沒有東西好輪詢，而且把它丟到另一條執行緒的話，`Spotify` 就得上鎖。
///
/// 埠號不能隨便挑：Spotify 只認註冊時填的那一個 redirect URI。
fn login(sp: &mut Spotify, cfg: &Config, shared: &Arc<Shared>) {
    let say = |m: &str| {
        *shared.login_msg.lock().unwrap() = m.to_string();
    };
    if !cfg.client_id_looks_valid() {
        say("config.json 裡沒有有效的 client_id");
        return;
    }
    let url = sp.build_auth_url();
    // 瀏覽器開不起來或網址被截斷時，使用者至少可以從主控台自己複製
    println!("授權網址：{url}");
    if let Err(e) = crate::oauth::open_in_browser(&url) {
        say(&format!("開不了瀏覽器：{e}"));
        return;
    }
    say("等瀏覽器授權…（兩分鐘內）");
    match crate::oauth::wait_for_callback(cfg.port, Duration::from_secs(120)) {
        Ok(cb) => match sp.exchange_code(&cb.code, &cb.state) {
            Ok(()) => {
                shared.authed.store(true, Ordering::Relaxed);
                shared
                    .need_reauth
                    .store(!sp.tokens.missing_scopes().is_empty(), Ordering::Relaxed);
                say("登入成功，正在抓收藏清單…");
            }
            Err(e) => say(&format!("換權杖失敗：{e}")),
        },
        Err(e) => say(&format!("{e}")),
    }
}

/// 控制指令跟讀取同一套優先序：SMTC 可用就走 SMTC，否則打 Web API。
fn apply_control(smtc: &Option<Smtc>, sp: &mut Spotify, cmd: Cmd, shared: &Arc<Shared>) {
    let has_session = smtc.as_ref().is_some_and(|s| s.read().is_some());
    if has_session {
        let s = smtc.as_ref().unwrap();
        let playing = shared.now.lock().unwrap().as_ref().is_some_and(|n| n.playing);
        let ok = match cmd {
            Cmd::Play => s.play(),
            Cmd::Pause => s.pause(),
            Cmd::Toggle => {
                if playing {
                    s.pause()
                } else {
                    s.play()
                }
            }
            Cmd::Next => s.next(),
            Cmd::Prev => s.previous(),
            Cmd::Seek(ms) => s.seek(ms),
            Cmd::Shuffle(on) => s.set_shuffle(on),
            Cmd::Repeat(m) => s.set_repeat(m),
            // SMTC 做不到的（播整張專輯）一定要掉下去走 Web API。
            // 這裡原本是 `true`，等於把 PlayAlbum 當成「SMTC 已處理」直接吞掉 ——
            // 本機 Spotify 開著的時候點封面什麼都不會發生。
            _ => false,
        };
        if ok {
            return;
        }
        // SMTC 拒絕了（有些指令 Spotify 沒實作）就掉下去用 Web API。
    }

    let playing = shared.now.lock().unwrap().as_ref().is_some_and(|n| n.playing);
    let album = match &cmd {
        Cmd::PlayAlbum(_, name) | Cmd::PlayContext { name, .. } => Some(name.clone()),
        _ => None,
    };
    let res = match cmd {
        Cmd::PlayAlbum(uri, _) => sp.api(
            reqwest::Method::PUT,
            "/me/player/play",
            &[],
            Some(serde_json::json!({ "context_uri": uri, "offset": { "position": 0 } })),
        ),
        Cmd::PlayContext { uri, track_uri, pos, .. } => {
            sp.play_context(&uri, Some(&track_uri), pos).map(|_| None)
        }
        Cmd::Play => sp.api(reqwest::Method::PUT, "/me/player/play", &[], None),
        Cmd::Pause => sp.api(reqwest::Method::PUT, "/me/player/pause", &[], None),
        Cmd::Toggle => sp.api(
            reqwest::Method::PUT,
            if playing { "/me/player/pause" } else { "/me/player/play" },
            &[],
            None,
        ),
        Cmd::Next => sp.api(reqwest::Method::POST, "/me/player/next", &[], None),
        Cmd::Prev => sp.api(reqwest::Method::POST, "/me/player/previous", &[], None),
        Cmd::Seek(ms) => sp.api(
            reqwest::Method::PUT,
            "/me/player/seek",
            &[("position_ms", ms.to_string())],
            None,
        ),
        Cmd::Shuffle(on) => sp.api(
            reqwest::Method::PUT,
            "/me/player/shuffle",
            &[("state", on.to_string())],
            None,
        ),
        Cmd::Repeat(m) => sp.api(
            reqwest::Method::PUT,
            "/me/player/repeat",
            &[("state", m.as_str().to_string())],
            None,
        ),
        _ => return,
    };
    match res {
        Ok(_) => {
            if let Some(name) = album {
                shared.toast(format!("播放  {name}"));
            }
        }
        Err(e) => {
            if matches!(e, ApiError::Status(404)) {
                shared.no_device_rev.fetch_add(1, Ordering::Relaxed);
            }
            shared.toast(control_error(&e));
        }
    }
}

/// Web API 的錯誤翻成使用者看得懂的話。對照 app.py 的 `_cmd` 與 app.js 的 `playAlbum`。
fn control_error(e: &ApiError) -> String {
    match e {
        ApiError::Status(404) => "找不到播放裝置，先在 Spotify 播一下任何歌，或按 D 選裝置".into(),
        ApiError::Status(403) => "Spotify 拒絕了這個操作（需要 Premium）".into(),
        ApiError::NotAuthed => "還沒登入 Spotify（按 , 開設定登入）".into(),
        ApiError::RateLimited { .. } => "Spotify 請求配額用完了，暫時無法控制播放".into(),
        other => format!("播放失敗：{other}"),
    }
}
