//! `--shot <檔名.png> pv`：文字 PV 的一幀（`VITRO_MS` 換時間點，預設 9.5 秒）。
//! `--shot <資料夾> pv:seq`：逐幀輸出成 00000.png、00001.png…，
//! `VITRO_PV_SEQ=起,迄,fps`（毫秒，預設 1500,40000,30）。
//!
//! 示範歌詞是自己寫的句子，照 100 BPM 的八分音符排逐字時間；節拍是照同一個速度假造的
//! （`VITRO_PV_NOBEAT=1` 拿掉，看沒有音訊時的樣子）。`VITRO_CHROME=1` 拍左欄控制項出來的樣子，
//! `VITRO_ART=<圖檔>` 換封面、`VITRO_ACCENT=e0453a` 換強調色。
//! 示範歌詞是「主歌、副歌、主歌、副歌、橋段、副歌、尾聲」，段落之間隔空行，所以會換段、換風格，
//! 副歌三次都是同一個風格；假造的響度副歌高、主歌低（`VITRO_PV_FLAT=1` 拿掉，只看歌詞結構）。
//! `VITRO_PV_LOOK=paper`（neon／paper／impact／glitch／drift／type／pop／sign／cinema／retro）鎖定一種。
//! `VITRO_PV_SCENE=depth`（plain／flight／depth／marquee／wall／carousel／mirror／split／pendulum／cascade／vortex／spotlight／echo）整首鎖定一種段落場景（正式執行也認）。
//! `VITRO_PV_INTRO=毫秒` 把整份歌詞往後推（拉長前奏）、`VITRO_PV_BREAK=毫秒` 在第 8 句後面多空一段間奏，看沒字時的流光。
//! `VITRO_PV_MIX=進,退`（毫秒，退可省略）：在那個時間點從一般歌詞頁換進 PV、再退回去，看進出的轉場。
//! `VITRO_PV_SWAP=切,到`（毫秒，v2）：在「切」切到第二首（從頭開始、歌詞還沒到，只演曲名卡），「到」歌詞到了，看換歌接著演。
//! `VITRO_PV_V2=1` 拍文字PV_v2。`VITRO_FONT`、`VITRO_FONT_LATIN`、`VITRO_FONT_SCALE` 換字型（同 `--shot` 其他場景）。
//! `VITRO_TRANS=1` 打開「顯示翻譯」（示範歌詞每句都有自己寫的翻譯）。示範曲在最後一句後面留 15 秒尾奏（看片尾曲名卡）。
//! `VITRO_PV_GEO=1` 打開 v2 的背景幾何圖形（五個頻段照錄音量到的大小假造，每拍跳一下）。

use std::path::Path;
use std::time::{Duration, Instant};

use crate::gfx::{Gpu, Offscreen, OFFSCREEN_FORMAT};
use crate::glass::Glass;
use crate::lyrics::{Line, Lyrics, Word};
use crate::quad::QuadRenderer;
use crate::text::Text;

/// 示範曲的速度：一拍 600 ms，第一拍在 2 秒
const BEAT: i64 = 600;
const FIRST: i64 = 2000;

pub fn run(path: &Path, width: u32, height: u32, scene: &str) -> anyhow::Result<()> {
    if let Some(c) = std::env::var("VITRO_ACCENT").ok().and_then(|v| u32::from_str_radix(v.trim_start_matches('#'), 16).ok()) {
        let rgb = [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff].map(|x| x as f32 / 255.0);
        crate::theme::set_accent(Some(crate::theme::accent_from(rgb, 0.8)));
    }
    crate::text::set_font(
        &std::env::var("VITRO_FONT_LATIN").unwrap_or_default(),
        &std::env::var("VITRO_FONT").unwrap_or_default(),
        std::env::var("VITRO_FONT_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0),
    );
    let v2 = std::env::var_os("VITRO_PV_V2").is_some();
    let gpu = Gpu::headless()?;
    println!("GPU: {}", gpu.adapter_name);
    let target = Offscreen::new(&gpu.device, width, height);
    let mut quads = QuadRenderer::new(&gpu, OFFSCREEN_FORMAT);
    let mut text = Text::new(&gpu, OFFSCREEN_FORMAT);
    let mut glass = Glass::new(&gpu, OFFSCREEN_FORMAT);
    glass.set_instant(true);
    let mut pv = super::Pv::new(&gpu, OFFSCREEN_FORMAT);

    let albums = crate::scene_flow::fake_albums(&gpu, 1);
    let art = match std::env::var("VITRO_ART").ok().and_then(|p| std::fs::read(p).ok()) {
        Some(b) => crate::gfx::upload_encoded(&gpu, &b, "shot-art").unwrap_or_else(|_| albums[0].art.clone()),
        None => albums[0].art.clone(),
    };
    let (ly, loud) = demo();
    let flat = std::env::var_os("VITRO_PV_FLAT").is_some();
    let dur = ly.synced.last().map_or(60_000, |l| l.t + 15_000);
    let chrome: f32 = std::env::var("VITRO_CHROME").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let beats = std::env::var_os("VITRO_PV_NOBEAT").is_none();
    let look = std::env::var("VITRO_PV_LOOK").ok().and_then(|v| {
        super::style::LOOKS.iter().copied().find(|l| format!("{l:?}").eq_ignore_ascii_case(v.trim()))
    });
    let mix_at = std::env::var("VITRO_PV_MIX").ok().and_then(|v| {
        let mut it = v.split(',').filter_map(|x| x.trim().parse::<i64>().ok());
        Some((it.next()?, it.next()))
    });
    // 切歌接著演（v2）：在 a 切到第二首（從頭開始、歌詞還沒到），b 歌詞到（同一份示範歌詞）
    let swap = std::env::var("VITRO_PV_SWAP").ok().and_then(|v| {
        let mut it = v.split(',').filter_map(|x| x.trim().parse::<i64>().ok());
        let a = it.next()?;
        Some((a, it.next().unwrap_or(i64::MAX).max(a)))
    });
    let empty = Lyrics::default();
    let base = Instant::now();

    // `next` 是下一幀的時間：切歌、歌詞到的前一幀要先存起來（正式執行時 main.rs 也是這樣）
    let mut frame = |ms: i64, next: Option<i64>, out: &Path| -> anyhow::Result<()> {
        glass.settle(&gpu);
        glass.end_frame();
        let now = crate::scene_now::Now {
            title: "示範曲",
            artist: "VitroDeck",
            album: "",
            art: art.clone(),
            position_ms: ms,
            lyric_ms: ms,
            duration_ms: dur,
            lyrics: Some(&ly),
            playing: true,
            shuffle: false,
            repeat: crate::smtc::Repeat::Off,
            ktv: true,
            show_trans: std::env::var_os("VITRO_TRANS").is_some(),
            tools: crate::ui::Tools { liked: Some(false), volume: Some(62), vol_locked: false },
            vol_drag: false,
            vinyl: None,
            bg_prev: None,
            lyric_scroll: 0.0,
            pick: Default::default(),
            roll: None,
            fx: Default::default(),
        };
        let at = base + Duration::from_millis(ms.max(0) as u64);
        let (track, pv_ly, sms, title, artist) = match swap {
            Some((a, b)) if ms >= a => ("demo2", if ms >= b { &ly } else { &empty }, ms - a, "第二首歌", "VitroDeck 示範二"),
            _ => ("demo", &ly, ms, "霓虹之夜", "VitroDeck 示範"),
        };
        let freeze = swap.zip(next).is_some_and(|((a, b), n)| (ms < a && n >= a) || (ms < b && n >= b));
        let mut pulse = if beats { fake_pulse(base, sms) } else { Default::default() };
        if !flat {
            // 最近一秒的平均（真的響度也是平滑過的）
            pulse.energy = (0..5).map(|j| loud(sms - j * 250)).sum::<f32>() / 5.0;
            pulse.energy_conf = 0.9;
        }
        // 從一般歌詞頁換進來、退回去的進度
        let ramp = |from: i64| crate::anim::EASE.at(((ms - from) as f32 / (super::MIX_S * 1000.0)).clamp(0.0, 1.0));
        let mix = match mix_at {
            Some((a, b)) => ramp(a) * (1.0 - b.map_or(0.0, ramp)),
            None => 1.0,
        };
        crate::scene_now::render_with(
            &gpu,
            &target,
            &mut quads,
            &mut glass,
            &mut text,
            &now,
            chrome,
            None,
            Some(crate::scene_now::PvIn {
                pv: &mut pv,
                lyrics: pv_ly,
                ms: sms,
                mix,
                track,
                pulse,
                now: at,
                chrome,
                look,
                v2,
                title,
                artist,
                duration: dur,
                spec: fake_spec(sms),
                geo: std::env::var_os("VITRO_PV_GEO").is_some(),
                bands: fake_bands(sms),
                freeze,
            }),
        )?;
        target.save_png(&gpu, out)?;
        text.end_frame();
        Ok(())
    };

    if scene.ends_with(":seq") {
        let mut v = std::env::var("VITRO_PV_SEQ")
            .unwrap_or_default()
            .split(',')
            .filter_map(|x| x.trim().parse::<i64>().ok())
            .collect::<Vec<_>>()
            .into_iter();
        let (from, to, fps) = (v.next().unwrap_or(1500), v.next().unwrap_or(40_000), v.next().unwrap_or(30).max(1));
        std::fs::create_dir_all(path)?;
        let n = ((to - from) * fps / 1000).max(1);
        let t0 = Instant::now();
        for f in 0..n {
            frame(from + f * 1000 / fps, Some(from + (f + 1) * 1000 / fps), &path.join(format!("{f:05}.png")))?;
        }
        println!("寫出 {n} 幀到 {}（{:.1} 秒）", path.display(), t0.elapsed().as_secs_f32());
        return Ok(());
    }
    let ms = std::env::var("VITRO_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(9_500);
    frame(ms, None, path)?;
    println!("寫出 {} 場景=文字PV {ms}ms", path.display());
    Ok(())
}

/// 假造的頻譜（給 v2 間奏的光帶）：低頻高、高頻低，各段跟著時間起伏
fn fake_spec(ms: i64) -> [f32; super::idle::SPEC] {
    std::array::from_fn(|j| {
        let f = j as f32 / super::idle::SPEC as f32;
        let wob = ((ms as f32 / 170.0 + j as f32 * 0.9).sin() * 0.5 + 0.5) * ((ms as f32 / 410.0 - j as f32 * 0.4).cos() * 0.25 + 0.75);
        (0.8 - 0.55 * f) * (0.45 + 0.55 * wob)
    })
}

/// 假造的五個頻段（給 v2 的背景幾何）：大小照錄音量到的中位數，每拍低音、反拍高音各跳一下
fn fake_bands(ms: i64) -> [f32; 5] {
    let on = if ms >= FIRST { 1.0 } else { 0.0 };
    let since = |t0: i64| (ms - t0).rem_euclid(BEAT) as f32;
    let kick = on * (-since(FIRST) / 120.0).exp();
    let hat = on * (-since(FIRST + BEAT / 2) / 80.0).exp();
    [0.72 + 0.12 * kick, 0.72 + 0.05 * kick, 0.62, 0.52 + 0.03 * hat, 0.33 + 0.1 * hat]
}

/// 照 100 BPM 假造的節拍：每拍都有鼓點，小節第一拍重
fn fake_pulse(base: Instant, ms: i64) -> super::beat::Pulse {
    let at = |t: i64| base + Duration::from_millis(t.max(0) as u64);
    let n = (ms - FIRST).div_euclid(BEAT);
    let last = FIRST + n * BEAT;
    super::beat::Pulse {
        period: BEAT as f32 / 1000.0,
        beat_at: Some(at(FIRST)),
        conf: 0.85,
        hit_at: (ms >= FIRST).then(|| at(last)),
        hit: if n.rem_euclid(4) == 0 { 1.0 } else { 0.55 },
        ..Default::default()
    }
}

/// 自己寫的示範歌詞。每句兩小節，字落在八分音符上，詞尾拖長；
/// 第 6 句故意不給逐字，看退回「整句平均分」的樣子。段落之間接一行空行，
/// 只有第二段主歌直接接副歌（最後兩句是副歌前的往上推）。
/// 另外回傳假造的響度（歌詞時間 → 0..1）：副歌響、橋段中等、其他小聲
fn demo() -> (Lyrics, impl Fn(i64) -> f32) {
    const CHORUS: [&str; 4] = ["把今晚 唱到最後一秒", "就算明天 會忘記", "此刻 閃耀吧！", "Light it up tonight"];
    // (句子, 響度, 後面接空行)
    let mut lines: Vec<(&str, f32, bool)> = Vec::new();
    let mut add = |ls: &[&'static str], loud: f32, gap: bool| {
        for (j, l) in ls.iter().enumerate() {
            lines.push((l, loud, gap && j + 1 == ls.len()));
        }
    };
    add(&["霓虹在雨裡慢慢融化", "我們在午夜 奔跑", "心跳 比節拍還快", "Don't let the city sleep"], 0.3, true);
    add(&CHORUS, 0.85, true);
    add(&["風吹過 空蕩的月台", "末班車 載走了夏天", "還沒說出口的 那句話", "Say it before the dawn"], 0.4, false);
    add(&CHORUS, 0.85, true);
    add(&["燃燒吧 燃燒吧", "把影子 甩在身後"], 0.6, true);
    add(&CHORUS, 0.95, true);
    add(&["直到 世界盡頭", "再見 了"], 0.25, false);
    // 自己寫的翻譯（`VITRO_TRANS=1` 才看得到）：中文句配英文、英文句配中文
    let trans = |s: &str| -> Option<&'static str> {
        Some(match s {
            "霓虹在雨裡慢慢融化" => "Neon slowly melting in the rain",
            "我們在午夜 奔跑" => "We run through the midnight",
            "心跳 比節拍還快" => "A heartbeat faster than the beat",
            "Don't let the city sleep" => "別讓這座城市睡去",
            "把今晚 唱到最後一秒" => "Sing tonight to the very last second",
            "就算明天 會忘記" => "Even if tomorrow we forget",
            "此刻 閃耀吧！" => "Shine, right now",
            "Light it up tonight" => "今晚把它點亮",
            "風吹過 空蕩的月台" => "Wind across the empty platform",
            "末班車 載走了夏天" => "The last train took the summer away",
            "還沒說出口的 那句話" => "The words still left unsaid",
            "Say it before the dawn" => "在天亮之前說出口",
            "燃燒吧 燃燒吧" => "Burn, keep burning",
            "把影子 甩在身後" => "Leave the shadows behind",
            "直到 世界盡頭" => "Until the end of the world",
            "再見 了" => "Goodbye",
            _ => return None,
        })
    };
    let half = BEAT / 2;
    let ms_env = |k: &str| std::env::var(k).ok().and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0).max(0);
    let (intro, brk) = (ms_env("VITRO_PV_INTRO"), ms_env("VITRO_PV_BREAK"));
    // 第 k 句往後推多少
    let shift = |k: usize| intro + if k >= 8 { brk } else { 0 };
    let mut synced: Vec<Line> = lines
        .iter()
        .enumerate()
        .map(|(k, &(s, _, _))| {
            let t0 = FIRST + k as i64 * BEAT * 8 + shift(k);
            let mut t = t0;
            let mut words = Vec::new();
            if s.is_ascii() {
                let ws: Vec<&str> = s.split(' ').collect();
                for (j, w) in ws.iter().enumerate() {
                    let d = if j + 1 == ws.len() { BEAT * 2 } else { half };
                    words.push(Word { t, d, text: format!("{w} ") });
                    t += d;
                }
            } else {
                let cs: Vec<char> = s.chars().collect();
                for (j, c) in cs.iter().enumerate() {
                    if *c == ' ' {
                        t += half;
                        continue;
                    }
                    let d = if j + 1 == cs.len() { BEAT * 2 } else if cs.get(j + 1) == Some(&' ') { BEAT } else { half };
                    words.push(Word { t, d, text: c.to_string() });
                    t += d;
                }
            }
            Line { t: t0, text: s.to_string(), words: (k != 5).then_some(words), trans: trans(s).map(Into::into), ..Default::default() }
        })
        .collect();
    let times: Vec<(i64, f32)> = synced.iter().zip(&lines).map(|(l, x)| (l.t, x.1)).collect();
    // 空行插在段落最後一句後面，時間是那句的最後一拍
    for k in (0..lines.len()).rev().filter(|&k| lines[k].2) {
        let t = FIRST + (k as i64 + 1) * BEAT * 8 - BEAT + shift(k);
        synced.insert(k + 1, Line { t, ..Default::default() });
    }
    let ly = Lyrics { synced, source: "word".into(), provider: "示範".into(), ..Default::default() };
    (ly, move |ms: i64| times.iter().rev().find(|x| x.0 <= ms).map_or(0.2, |x| x.1))
}
