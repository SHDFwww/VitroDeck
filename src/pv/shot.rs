//! `--shot <檔名.png> pv`：文字 PV 的一幀（`VITRO_MS` 換時間點，預設 9.5 秒）。
//! `--shot <資料夾> pv:seq`：逐幀輸出成 00000.png、00001.png…，
//! `VITRO_PV_SEQ=起,迄,fps`（毫秒，預設 1500,40000,30）。
//!
//! 示範歌詞是自己寫的句子，照 100 BPM 的八分音符排逐字時間；節拍是照同一個速度假造的
//! （`VITRO_PV_NOBEAT=1` 拿掉，看沒有音訊時的樣子）。`VITRO_CHROME=1` 拍左欄控制項出來的樣子，
//! `VITRO_ART=<圖檔>` 換封面、`VITRO_ACCENT=e0453a` 換強調色。
//! 示範歌詞每四句隔一行空行，所以會換段、換風格；`VITRO_PV_LOOK=paper`（neon／paper／impact／glitch／drift／type／pop／sign／cinema／retro）鎖定一種。
//! `VITRO_PV_INTRO=毫秒` 把整份歌詞往後推（拉長前奏）、`VITRO_PV_BREAK=毫秒` 在第 8 句後面多空一段間奏，看沒字時的流光。
//! `VITRO_PV_MIX=進,退`（毫秒，退可省略）：在那個時間點從一般歌詞頁換進 PV、再退回去，看進出的轉場。

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
    let ly = demo();
    let dur = ly.synced.last().map_or(60_000, |l| l.t + 6000);
    let chrome: f32 = std::env::var("VITRO_CHROME").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let beats = std::env::var_os("VITRO_PV_NOBEAT").is_none();
    let look = std::env::var("VITRO_PV_LOOK").ok().and_then(|v| {
        super::style::LOOKS.iter().copied().find(|l| format!("{l:?}").eq_ignore_ascii_case(v.trim()))
    });
    let mix_at = std::env::var("VITRO_PV_MIX").ok().and_then(|v| {
        let mut it = v.split(',').filter_map(|x| x.trim().parse::<i64>().ok());
        Some((it.next()?, it.next()))
    });
    let base = Instant::now();

    let mut frame = |ms: i64, out: &Path| -> anyhow::Result<()> {
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
            show_trans: false,
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
        let pulse = if beats { fake_pulse(base, ms) } else { Default::default() };
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
            Some(crate::scene_now::PvIn { pv: &mut pv, lyrics: &ly, ms, mix, track: "demo", pulse, now: at, chrome, look }),
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
            frame(from + f * 1000 / fps, &path.join(format!("{f:05}.png")))?;
        }
        println!("寫出 {n} 幀到 {}（{:.1} 秒）", path.display(), t0.elapsed().as_secs_f32());
        return Ok(());
    }
    let ms = std::env::var("VITRO_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(9_500);
    frame(ms, path)?;
    println!("寫出 {} 場景=文字PV {ms}ms", path.display());
    Ok(())
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
    }
}

/// 自己寫的示範歌詞。每句兩小節，字落在八分音符上，詞尾拖長；
/// 第 6 句故意不給逐字，看退回「整句平均分」的樣子。每四句後面接一行空行（段落的分界）
fn demo() -> Lyrics {
    let lines = [
        "霓虹在雨裡慢慢融化",
        "我們在午夜 奔跑",
        "心跳 比節拍還快",
        "Don't let the city sleep",
        "把今晚 唱到最後一秒",
        "就算明天 會忘記",
        "此刻 閃耀吧！",
        "Light it up tonight",
        "一起 飛",
        "風吹過 空蕩的月台",
        "末班車 載走了夏天",
        "還沒說出口的 那句話",
        "Say it before the dawn",
        "燃燒吧 燃燒吧",
        "把影子 甩在身後",
        "Turn it up turn it up",
        "直到 世界盡頭",
        "再見 了",
    ];
    let half = BEAT / 2;
    let ms_env = |k: &str| std::env::var(k).ok().and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0).max(0);
    let (intro, brk) = (ms_env("VITRO_PV_INTRO"), ms_env("VITRO_PV_BREAK"));
    // 第 k 句往後推多少
    let shift = |k: usize| intro + if k >= 8 { brk } else { 0 };
    let mut synced: Vec<Line> = lines
        .iter()
        .enumerate()
        .map(|(k, s)| {
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
            Line { t: t0, text: s.to_string(), words: (k != 5).then_some(words), ..Default::default() }
        })
        .collect();
    // 空行插在第 4、8、12、16 句後面，時間是那句的最後一拍
    for k in [16usize, 12, 8, 4] {
        let t = FIRST + k as i64 * BEAT * 8 - BEAT + shift(k - 1);
        synced.insert(k, Line { t, ..Default::default() });
    }
    Lyrics { synced, source: "word".into(), provider: "示範".into(), ..Default::default() }
}
