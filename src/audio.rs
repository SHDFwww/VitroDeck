//! WASAPI loopback 擷取 —— 對應 Python 版的 `audiofeed.py`。
//!
//! 這台機器沒有「立體聲混音」擷取端點，所以只能從輸出裝置 loopback 回來。
//! wasapi crate 的作法是：拿 **Render** 裝置，但用 `Direction::Capture`
//! 初始化，它會自己補上 `AUDCLNT_STREAMFLAGS_LOOPBACK`。
//!
//! `autoconvert: true` 讓我們不管裝置原生格式是什麼都能要到 48k/2ch/f32 ——
//! 那正是 Python 版靠 `soundcard` 幫忙吞掉的事，這裡要自己講明。
//!
//! **只在有人要的時候擷取**（Python 版的省電條件）：畫面在視覺效果頁，
//! 或燈條訂閱中而且本機正在播。平常在封面流與歌詞頁完全不碰音訊裝置。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use realfft::RealFftPlanner;
use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

pub const SAMPLE_RATE: usize = 48_000;
pub const FFT_SIZE: usize = 2048;
/// 給 shader 的頻段數。128 = 32 個 vec4，塞得進 uniform buffer。
pub const BINS: usize = 128;
/// 時域波形的長度。MilkDrop 的示波器吃 512 個取樣。
pub const WAVE: usize = 512;

pub struct Spectrum {
    pub bins: [f32; BINS],
    /// 最近的時域取樣（單聲道，−1..1）。頻譜畫長條，這個畫示波器。
    pub wave: [f32; WAVE],
    /// 低／中／高的能量，之後 MilkDrop 的 bass/mid/treb 要用同一組。
    pub bass: f32,
    pub mid: f32,
    pub treb: f32,
    /// 最近有沒有真的收到聲音。loopback 在完全無聲時是「不給資料」而不是
    /// 給一堆零，所以這個旗標不能用振幅判斷，要看有沒有收到 buffer。
    pub alive: bool,
    /// 最後一次真的有聲音（振幅過門檻）的時刻。`alive` 只說有沒有收到 buffer ——
    /// 有些驅動在沒聲音時照樣送全零的 buffer，Spotify 在別台裝置播的時候
    /// 這台也可能只收到其他程式的微弱雜音，所以「安不安靜」要看振幅。
    pub last_loud: Option<Instant>,
    /// 節拍（文字 PV 用）。只有 PV 開著時才算
    pub pulse: crate::pv::beat::Pulse,
}

/// 安靜多久才說「沒有偵測到聲音」。web 版的 `QUIET_MS`，歌與歌之間的空檔不能觸發它。
pub const QUIET: Duration = Duration::from_millis(2500);
/// 算「有聲音」的振幅門檻，web 版 `peak > 0.0005`
const LOUD: f32 = 0.0005;

impl Spectrum {
    /// 從 `since`（通常是進入視圖的時刻）起算，已經安靜超過 [`QUIET`] 了嗎。
    pub fn quiet(&self, since: Instant) -> bool {
        let from = self.last_loud.map_or(since, |t| t.max(since));
        from.elapsed() > QUIET
    }
}

// 陣列只有長度 ≤32 才有內建的 Default，128 要自己來。
impl Default for Spectrum {
    fn default() -> Self {
        Self {
            bins: [0.0; BINS],
            wave: [0.0; WAVE],
            bass: 0.0,
            mid: 0.0,
            treb: 0.0,
            alive: false,
            last_loud: None,
            pulse: Default::default(),
        }
    }
}

pub type Shared = Arc<Mutex<Spectrum>>;

/// 燈條的 RMS 多少個取樣算一次。Python 版是 soundcard 的 blocksize 1024
const RMS_BLOCK: usize = 1024;

static VIZ: AtomicBool = AtomicBool::new(false);
static PV: AtomicBool = AtomicBool::new(false);

/// 畫面端告訴這裡「現在在視覺效果頁」。
pub fn set_viz(on: bool) {
    VIZ.store(on, Ordering::Relaxed);
}

/// 畫面端告訴這裡「歌詞頁的文字 PV 開著」：要擷取，而且要抓節拍
pub fn set_pv(on: bool) {
    PV.store(on, Ordering::Relaxed);
}

fn needed() -> bool {
    VIZ.load(Ordering::Relaxed)
        || PV.load(Ordering::Relaxed)
        || crate::ledfeed::wanted()
        || crate::alignrec::on()
        || crate::align::wants_audio()
}

/// 歌詞對齊（含 `VITRO_ALIGN_REC`）要的擷取方式：大緩衝、每次醒來讀到空。
/// 對齊關著時照原本的小緩衝、一次讀一個封包，跟沒有這個功能時一樣
fn wide() -> bool {
    crate::align::enabled() || crate::alignrec::on()
}

pub fn start() -> Shared {
    let shared: Shared = Arc::new(Mutex::new(Spectrum::default()));
    let out = shared.clone();
    std::thread::spawn(move || {
        // COM 要在自己這條執行緒初始化。MTA 是因為我們不開訊息迴圈。
        let _ = wasapi::initialize_mta();
        loop {
            // 沒人要就只是等。訂閱到期不會有事件通知，所以用輪詢
            if !needed() {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            // 裝置是進迴圈前抓的。使用者中途插耳機會讓 client 失效，
            // 那就跳出來重抓一次預設裝置 —— 跟 Python 版同樣的策略。
            let r = capture_once(&out);
            if let Ok(mut s) = out.lock() {
                s.alive = false;
            }
            if let Err(e) = r {
                crate::warn!("音訊擷取中斷，重試：{e}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });
    shared
}

fn capture_once(out: &Shared) -> anyhow::Result<()> {
    let enumerator = wasapi::DeviceEnumerator::new()?;
    let device = enumerator.get_default_device(&Direction::Render)?;
    let mut client = device.get_iaudioclient()?;

    let fmt = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE, 2, None);
    let blockalign = fmt.get_blockalign() as usize;
    let (_def, min_time) = client.get_device_period()?;
    let big = wide();

    // Render 裝置 + Direction::Capture == loopback。Exclusive 不支援 loopback。
    client.initialize_client(
        &fmt,
        &Direction::Capture,
        // 對齊時緩衝至少 100 ms：視窗藏起來、程式閒置時這條執行緒會晚醒（實測一次晚到兩個封包），
        // 用最小週期（約 10 ms）當緩衝的話每 30 ms 就溢位一次、掉 8 ms 的聲音
        &StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: if big { min_time.max(1_000_000) } else { min_time },
        },
    )?;

    let h_event = client.set_get_eventhandle()?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;

    let mut raw: VecDeque<u8> = VecDeque::with_capacity(blockalign * FFT_SIZE * 4);
    let mut window: VecDeque<f32> = VecDeque::with_capacity(FFT_SIZE);
    window.resize(FFT_SIZE, 0.0);

    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let mut scratch = fft.make_scratch_vec();
    let mut spectrum = fft.make_output_vec();
    let hann: Vec<f32> = (0..FFT_SIZE)
        .map(|i| {
            let x = std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32;
            x.sin() * x.sin()
        })
        .collect();

    let mut smoothed = [0.0f32; BINS];
    let (mut sq, mut n_sq) = (0.0f32, 0usize);
    // 節拍：每 HOP 個新取樣餵一次頻譜
    let mut tracker = crate::pv::beat::Tracker::default();
    let mut hop = 0usize;
    // `VITRO_ALIGN_REC`：歌詞對齊的資料收集，沒設就什麼都不做
    let mut rec = crate::alignrec::Rec::default();

    loop {
        // 沒人要了就收掉，回到外面等；對齊剛開關的話也收掉，外面照新的方式重開
        if !needed() || wide() != big {
            let _ = client.stop_stream();
            return Ok(());
        }
        // 一次 GetBuffer 只拿一個封包。醒得晚時佇列裡會有好幾個，只讀一個的話剩下的
        // 要等下一次事件，積著積著就溢位 —— 對齊時每次醒來都讀到空為止
        loop {
            let before = raw.len();
            let info = capture.read_from_device_to_deque(&mut raw)?;
            if raw.len() == before {
                break;
            }
            if crate::alignrec::on() {
                let fl = &info.flags;
                rec.push(raw.range(before..).copied(), info.timestamp, info.index, fl.silent, fl.data_discontinuity);
            }
            if crate::align::wants_audio() {
                crate::align::audio(info.timestamp, raw.range(before..).copied(), info.flags.silent);
            }
            if !big || capture.get_next_packet_size()?.unwrap_or(0) == 0 {
                break;
            }
        }

        // 一次讀完手上所有的立體聲 frame，混成單聲道推進滑動視窗。
        let mut got = false;
        while raw.len() >= blockalign {
            let mut f = [0u8; 4];
            for b in f.iter_mut() {
                *b = raw.pop_front().unwrap();
            }
            let l = f32::from_le_bytes(f);
            for b in f.iter_mut() {
                *b = raw.pop_front().unwrap();
            }
            let r = f32::from_le_bytes(f);
            let mono = (l + r) * 0.5;
            window.pop_front();
            window.push_back(mono);
            got = true;
            hop += 1;
            sq += mono * mono;
            n_sq += 1;
            if n_sq == RMS_BLOCK {
                crate::ledfeed::send_rms((sq / RMS_BLOCK as f32).sqrt());
                (sq, n_sq) = (0.0, 0);
            }
        }

        if got {
            // 時域波形直接取視窗最後 WAVE 個取樣，不加窗 —— 示波器要的是原始波形。
            if let Ok(mut sh) = out.lock() {
                let n = window.len();
                for (i, dst) in sh.wave.iter_mut().enumerate() {
                    *dst = window[n - WAVE + i];
                }
                if sh.wave.iter().any(|v| v.abs() > LOUD) {
                    sh.last_loud = Some(Instant::now());
                }
            }
            let mut buf: Vec<f32> =
                window.iter().zip(&hann).map(|(s, w)| s * w).collect();
            fft.process_with_scratch(&mut buf, &mut spectrum, &mut scratch)?;
            let mags: Vec<f32> =
                spectrum.iter().map(|c| (c.re * c.re + c.im * c.im).sqrt()).collect();
            write_bins(&mags, &mut smoothed, out);
            // 一次事件通常不到一個 HOP；跨過幾個就餵幾次（同一份頻譜再餵一次通量是 0，無害）
            if PV.load(Ordering::Relaxed) {
                let now = Instant::now();
                while hop >= crate::pv::beat::HOP {
                    hop -= crate::pv::beat::HOP;
                    tracker.push(&mags, now);
                }
                if let Ok(mut s) = out.lock() {
                    s.pulse = tracker.pulse;
                }
            } else {
                hop = 0;
            }
        }

        // 完全沒在播的時候 loopback 不給事件，這裡會 timeout。那不是錯誤，
        // 只是靜音 —— 讓頻譜自然衰減，不要重建 client。
        if h_event.wait_for_event(500).is_err() {
            // Python 版那邊靜音時照樣收到全零的塊，燈條每塊都有一包；
            // 這裡靜音就沒有資料，至少半秒告訴它一次「現在是 0」
            crate::ledfeed::send_rms(0.0);
            for v in smoothed.iter_mut() {
                *v *= 0.85;
            }
            tracker.silence();
            if let Ok(mut s) = out.lock() {
                s.pulse = tracker.pulse;
                s.bins = smoothed;
                s.alive = false;
                s.bass = 0.0;
                s.mid = 0.0;
                s.treb = 0.0;
            }
        }
    }
}

/// 把線性的 FFT bin 折成對數頻段。人耳是對數的，線性分下去低頻會擠成一條。
#[allow(clippy::needless_range_loop)]
fn write_bins(mags: &[f32], smoothed: &mut [f32; BINS], out: &Shared) {
    let nyq = mags.len() as f32;
    let (lo, hi) = (30.0f32, 16_000.0f32);
    let bin_of = |hz: f32| (hz / (SAMPLE_RATE as f32 / 2.0) * nyq) as usize;

    let mut raw = [0.0f32; BINS];
    for i in 0..BINS {
        let t0 = i as f32 / BINS as f32;
        let t1 = (i + 1) as f32 / BINS as f32;
        let a = bin_of(lo * (hi / lo).powf(t0)).max(1);
        let b = bin_of(lo * (hi / lo).powf(t1)).max(a + 1).min(mags.len());
        let peak = mags[a..b].iter().fold(0.0f32, |m, v| m.max(*v));
        // dB 化再壓到 0..1。+60 dB 動態範圍，跟 Web Audio 的預設接近。
        let db = 20.0 * (peak.max(1e-6)).log10();
        raw[i] = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    }

    // 上升快、下降慢 —— 不然畫面會抖得看不清楚。
    for i in 0..BINS {
        smoothed[i] = if raw[i] > smoothed[i] {
            raw[i]
        } else {
            smoothed[i] * 0.82 + raw[i] * 0.18
        };
    }

    let avg = |r: std::ops::Range<usize>| {
        let n = r.len() as f32;
        smoothed[r].iter().sum::<f32>() / n
    };
    if let Ok(mut s) = out.lock() {
        s.bins = *smoothed;
        s.bass = avg(0..BINS / 8);
        s.mid = avg(BINS / 8..BINS / 2);
        s.treb = avg(BINS / 2..BINS);
        s.alive = true;
    }
}
