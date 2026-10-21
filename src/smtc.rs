//! SMTC（Windows 系統媒體傳輸控制）—— 對應 Python 版的 `smtc.py`。
//!
//! 為什麼要這個：Spotify Web API 有配額，穩定輪詢會被鎖數小時，而且是靜默的。
//! SMTC 是本機的，免費、無配額。播放狀態的絕大部分它都給得出來，
//! 所以拿它當主要來源，Web API 只負責 SMTC 給不了的事
//! （收藏清單、專輯 URI、遠端裝置）。
//!
//! SMTC 給不了的：
//!   - 任何 Spotify ID   -> 靠「專輯名 + 演出者」比對收藏清單
//!   - 大於 300px 的封面  -> 比對到收藏就用快取的 640px，比對不到才用這張
//!   - 遠端裝置的播放狀態  -> 手機或別台電腦在播時這裡是空的，得退回 Web API
//!
//! **執行緒限制**：`Smtc` 裡的 WinRT 物件要在初始化過的 apartment 上用。
//! 呼叫 [`init_apartment`] 之後再 [`Smtc::new`]，而且整個生命週期都待在
//! 同一條執行緒上 —— 跟 Python 版把它關在自己的 asyncio loop 裡是同一個理由。

use md5::{Digest, Md5};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Media::MediaPlaybackAutoRepeatMode as RepeatMode;
use windows::Storage::Streams::DataReader;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// FILETIME/WinRT DateTime 的原點是 1601-01-01，Unix 是 1970-01-01，
/// 中間差這麼多個 100 奈秒。
const EPOCH_DELTA_TICKS: i64 = 116_444_736_000_000_000;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub progress_ms: i64,
    pub duration_ms: i64,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub track_number: i32,
    /// Spotify 給的原始時間軸：（Position 毫秒, LastUpdatedTime, 讀的當下），後兩個是 WinRT 時刻（100 ns）。
    /// 研究位置誤差用（`VITRO_ALIGN_REC`）
    pub stamp: Option<(i64, i64, i64)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Repeat {
    #[default]
    Off,
    Track,
    Context,
}

impl Repeat {
    pub fn as_str(self) -> &'static str {
        match self {
            Repeat::Off => "off",
            Repeat::Track => "track",
            Repeat::Context => "context",
        }
    }
}

/// 跨來源一致的曲目識別。SMTC 沒有平台 ID，而且歌詞快取的檔名不能直接用中文
/// （會被正規化成一堆底線而互相覆蓋），所以統一雜湊。
///
/// 演算法刻意跟 Python 版一模一樣（md5 的前 20 個十六進位字元），
/// 這樣兩邊可以共用同一份歌詞快取。
pub fn track_key(artist: &str, title: &str, album: &str) -> String {
    let raw = format!("{artist}|{title}|{album}").to_lowercase();
    let digest = Md5::digest(raw.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("k{}", &hex[..20])
}

/// COM 要在每條會碰 WinRT 的執行緒上初始化一次。用 MTA 是因為我們不開訊息迴圈
/// —— 非同步完成回呼會走執行緒池，不需要泵訊息就能被 pollster 等到。
pub fn init_apartment() {
    unsafe {
        // 已經初始化過會回 S_FALSE / RPC_E_CHANGED_MODE，那不是問題。
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

pub struct Smtc {
    mgr: Manager,
    app_hint: String,
}

impl Smtc {
    pub fn new(app_hint: &str) -> anyhow::Result<Self> {
        let mgr = pollster::block_on(Manager::RequestAsync()?)?;
        Ok(Self { mgr, app_hint: app_hint.to_lowercase() })
    }

    /// 指定挑 Spotify，不用「目前 session」—— 在瀏覽器播個影片就會把
    /// 目前 session 搶走。
    fn session(&self) -> Option<Session> {
        let sessions = self.mgr.GetSessions().ok()?;
        for s in sessions {
            let id = s.SourceAppUserModelId().ok()?.to_string().to_lowercase();
            if id.contains(&self.app_hint) {
                return Some(s);
            }
        }
        None
    }

    pub fn read(&self) -> Option<Track> {
        let s = self.session()?;

        let info = s.GetPlaybackInfo().ok()?;
        let status = info.PlaybackStatus().ok()?;
        if status == Status::Closed || status == Status::Stopped {
            return None;
        }

        let props = pollster::block_on(s.TryGetMediaPropertiesAsync().ok()?).ok()?;
        let title = props.Title().map(|h| h.to_string()).unwrap_or_default();
        let artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();
        let album = props.AlbumTitle().map(|h| h.to_string()).unwrap_or_default();
        if title.is_empty() && artist.is_empty() {
            return None;
        }

        let tl = s.GetTimelineProperties().ok()?;
        let duration_ms = tl.EndTime().map(|t| t.Duration / 10_000).unwrap_or(0);
        let mut progress_ms = tl.Position().map(|t| t.Duration / 10_000).unwrap_or(0);
        let stamp = tl.LastUpdatedTime().ok().map(|l| (progress_ms, l.UniversalTime, now_winrt_ticks()));

        // position 是「上次更新當下」的值，播放中要自己補上經過的時間，
        // 不然會一直落後。這也是 SMTC 比 Web API 準的地方。
        let playing = status == Status::Playing;
        if playing
            && let Ok(last) = tl.LastUpdatedTime()
                && last.UniversalTime > 0 {
                    let now = now_winrt_ticks();
                    let elapsed_ms = (now - last.UniversalTime) / 10_000;
                    // 超過一小時的差距代表那個時間戳不可信，寧可不補。
                    if (0..3_600_000).contains(&elapsed_ms) {
                        let cap = if duration_ms > 0 { duration_ms } else { i64::MAX };
                        progress_ms = (progress_ms + elapsed_ms).min(cap);
                    }
                }

        let repeat = info
            .AutoRepeatMode()
            .and_then(|r| r.Value())
            .map(|m| match m {
                RepeatMode::Track => Repeat::Track,
                RepeatMode::List => Repeat::Context,
                _ => Repeat::Off,
            })
            .unwrap_or(Repeat::Off);

        Some(Track {
            id: track_key(&artist, &title, &album),
            playing,
            progress_ms: progress_ms.max(0),
            duration_ms: duration_ms.max(0),
            shuffle: info.IsShuffleActive().and_then(|r| r.Value()).unwrap_or(false),
            repeat,
            track_number: props.TrackNumber().unwrap_or(0),
            stamp,
            title,
            artist,
            album,
        })
    }

    /// 封面縮圖（通常是 300x300）。比對得到收藏時不會用到這張。
    pub fn thumbnail(&self) -> Option<Vec<u8>> {
        let s = self.session()?;
        let props = pollster::block_on(s.TryGetMediaPropertiesAsync().ok()?).ok()?;
        let reference = props.Thumbnail().ok()?;
        let stream = pollster::block_on(reference.OpenReadAsync().ok()?).ok()?;
        let size = stream.Size().ok()?;
        if size == 0 {
            return None;
        }
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).ok()?).ok()?;
        pollster::block_on(reader.LoadAsync(size as u32).ok()?).ok()?;
        let mut buf = vec![0u8; size as usize];
        reader.ReadBytes(&mut buf).ok()?;
        Some(buf)
    }

    // ---------- 控制 ----------
    fn try_op(&self, f: impl FnOnce(&Session) -> windows::core::Result<bool>) -> bool {
        self.session().map(|s| f(&s).unwrap_or(false)).unwrap_or(false)
    }

    pub fn play(&self) -> bool {
        self.try_op(|s| pollster::block_on(s.TryPlayAsync()?))
    }
    pub fn pause(&self) -> bool {
        self.try_op(|s| pollster::block_on(s.TryPauseAsync()?))
    }
    pub fn next(&self) -> bool {
        self.try_op(|s| pollster::block_on(s.TrySkipNextAsync()?))
    }
    pub fn previous(&self) -> bool {
        self.try_op(|s| pollster::block_on(s.TrySkipPreviousAsync()?))
    }
    pub fn seek(&self, ms: i64) -> bool {
        // 這個 API 吃 100 奈秒為單位的 ticks
        self.try_op(|s| pollster::block_on(s.TryChangePlaybackPositionAsync(ms * 10_000)?))
    }
    pub fn set_shuffle(&self, on: bool) -> bool {
        self.try_op(|s| pollster::block_on(s.TryChangeShuffleActiveAsync(on)?))
    }
    pub fn set_repeat(&self, mode: Repeat) -> bool {
        let m = match mode {
            Repeat::Off => RepeatMode::None,
            Repeat::Track => RepeatMode::Track,
            Repeat::Context => RepeatMode::List,
        };
        self.try_op(|s| pollster::block_on(s.TryChangeAutoRepeatModeAsync(m)?))
    }
}

fn now_winrt_ticks() -> i64 {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    EPOCH_DELTA_TICKS + unix.as_nanos() as i64 / 100
}
