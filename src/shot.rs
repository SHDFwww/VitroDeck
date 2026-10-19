//! `--shot <檔名.png>`：離屏算一張圖存檔。
//!
//! 原生視窗沒有 DevTools、也截不了圖，這是這個專案唯一能「看到」自己畫了
//! 什麼的辦法。順便可以拿來做視覺回歸比對。

use std::path::Path;

use crate::gfx::{Gpu, Offscreen, OFFSCREEN_FORMAT};
use crate::glass::{Glass, Panel};
use crate::quad::{Quad, QuadRenderer};
use crate::text::Text;
use crate::theme;

pub fn run(path: &Path, width: u32, height: u32, scene: &str) -> anyhow::Result<()> {
    // `VITRO_THEME=light`：只供驗證用，預設與實機一樣是深色
    if std::env::var("VITRO_THEME").is_ok_and(|v| v == "light") {
        theme::set_mode(theme::Mode::Light);
    }
    // `VITRO_ACCENT=e0453a`：假裝封面取到這個顏色，看任何場景換了強調色的樣子。
    // 沒設就是 Spotify 綠，舊基準因此不受影響
    if let Some(c) = std::env::var("VITRO_ACCENT").ok().and_then(|v| parse_hex(&v)) {
        theme::set_accent(Some(theme::accent_from(c, 0.8)));
    }
    // `VITRO_FONT=<中文字型家族名>`、`VITRO_FONT_LATIN=<英文字型>`、`VITRO_FONT_SCALE=1.3`：
    // 換字型與字級倍率（沒設就是預設的微軟正黑體、英文跟中文一樣、100%）
    crate::text::set_font(
        &std::env::var("VITRO_FONT_LATIN").unwrap_or_default(),
        &std::env::var("VITRO_FONT").unwrap_or_default(),
        std::env::var("VITRO_FONT_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0),
    );
    let gpu = Gpu::headless()?;
    println!("GPU: {}", gpu.adapter_name);
    let target = Offscreen::new(&gpu.device, width, height);
    // 版面用的邏輯尺寸（`VITRO_SCALE` 不是 1 的時候比材質小）
    let (lw, lh) = crate::gfx::logical(width, height);
    let mut quads = QuadRenderer::new(&gpu, OFFSCREEN_FORMAT);
    let mut text = Text::new(&gpu, OFFSCREEN_FORMAT);
    let mut glass = Glass::new(&gpu, OFFSCREEN_FORMAT);
    let veil = crate::scene_flow::Veil::new(&gpu);

    glass.set_instant(true);
    // `VITRO_UNFOCUS=1`：假裝視窗沒有焦點，看玻璃退後的樣子
    if std::env::var_os("VITRO_UNFOCUS").is_some() {
        glass.set_focus(false);
    }
    // `VITRO_PRESS=1`（或 `=dx,dy`）：游標停在封面流 `VITRO_HOVER` 那顆的中心
    // （再偏 dx、dy）而且按著，看凝膠形變
    if let Ok(v) = std::env::var("VITRO_PRESS")
        && let Some(r) = crate::scene_flow::hot_rect(&crate::ui::flow(lw, lh, 1.0), fake_hover())
    {
        let mut d = v.split(',').filter_map(|x| x.trim().parse::<f32>().ok());
        let (dx, dy) = match (d.next(), d.next()) {
            (Some(x), Some(y)) => (x, y),
            _ => (0.0, 0.0),
        };
        glass.set_pointer(Some((r.cx() + dx, r.cy() + dy)), true);
    }
    // 玻璃上的字翻不翻明暗，看的是讀回來的背景，要晚一幀才拿得到。截圖只有一幀，
    // 所以整個畫兩次：第一次只為了讀回背景（存的圖會被第二次蓋掉）
    let mut draw = || -> anyhow::Result<()> {
        glass.settle(&gpu);
        glass.end_frame();
        if let Some(rest) = scene.strip_prefix("milk") {
            return milk_shot(&gpu, &target, path, rest.trim_start_matches(':'));
        }

        // `glassref`：跟網頁版的對照頁 `glass_ref.html`（不在 repo 內）畫同一個畫面，好把著色器的輸出跟
        // Chromium 真的 `backdrop-filter` 疊在一起比。**這是唯一能證明
        // 「像不像玻璃」的辦法** —— 光看自己的圖永遠覺得差不多。
        if scene == "glassref" {
            return glass_ref(&gpu, &target, &mut quads, &mut glass, path);
        }
        // `glasslab`：調玻璃材質用的試驗場（第 5 階段）；`motionlab`：同一個底，
        // 擺互動、出現、融合的各個階段（第 5 階段第三批）
        if scene == "glasslab" || scene == "motionlab" {
            return glass_lab(&gpu, &target, &mut quads, &mut text, &mut glass, path, scene == "motionlab");
        }

        // `settings[:第幾列]`：設定面板疊在封面流上。
        if let Some(rest) = scene.strip_prefix("settings") {
            let sel: usize = rest.trim_start_matches(':').parse().unwrap_or(0);
            let mut covers = crate::cover::CoverRenderer::new(&gpu, OFFSCREEN_FORMAT);
            let albums = crate::scene_flow::fake_albums(&gpu, 12);
            crate::scene_flow::render(
                &gpu, &target, &mut quads, &mut covers, &mut glass, &mut text, &albums, 4.0, &bar_under(&albums, crate::ui::settings(lw, lh, crate::settings::ITEMS.len()).panel, lw, lh), 1.0, &veil,
                None,
            )?;
            let cfg = crate::scene_settings::demo_cfg();
            crate::scene_settings::render(
                &gpu, &target, &mut quads, &mut glass, &mut text, &cfg, false,
                r"C:\VitroDeck", "", "", sel, fake_hover(),
            )?;
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=設定", path.display());
            return Ok(());
        }

        // `prefs[:頁]`：設定視窗（第 4 階段）。頁是 account／display／lyrics／vinyl／mini／viz／about，
        // 預設 display；`fonts`／`fonts-latin` 是顯示頁打開的中文／英文字型清單（捲到選中的字型，
        // `VITRO_PREFS_HOVER=<第幾列>` 假裝滑鼠停在那個字型上，看預覽框）。圖的大小是視窗的預設大小，不是 `VITRO_SHOT_SIZE`；
        // `VITRO_PREFS_SIZE=寬x高`（邏輯像素）換大小、`VITRO_PREFS_SCROLL=<px>` 捲下去看
        if let Some(rest) = scene.strip_prefix("prefs") {
            use crate::settings::{PAGES, Page};
            let page = match rest.trim_start_matches(':') {
                "account" => Page::Account,
                "lyrics" => Page::Lyrics,
                "vinyl" => Page::Vinyl,
                "mini" => Page::Mini,
                "viz" => Page::Viz,
                "about" => Page::About,
                _ => Page::Display,
            };
            let (pw, ph) = std::env::var("VITRO_PREFS_SIZE")
                .ok()
                .and_then(|v| v.split_once('x').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))))
                .unwrap_or((theme::win::W, theme::win::H));
            let k = crate::gfx::scale();
            let target = Offscreen::new(&gpu.device, (pw * k).round() as u32, (ph * k).round() as u32);
            let cfg = crate::scene_settings::demo_cfg();
            let fonts = match rest.trim_start_matches(':') {
                "fonts" => Some(crate::prefs::FontSlot::Cjk),
                "fonts-latin" => Some(crate::prefs::FontSlot::Latin),
                _ => None,
            };
            let mut ui = crate::prefs::Ui {
                page: PAGES.iter().position(|p| *p == page).unwrap_or(0),
                fonts,
                fonts_reveal: fonts.is_some(),
                hot: std::env::var("VITRO_PREFS_HOVER").ok().and_then(|v| v.parse().ok()).map(crate::prefs::Hit::FontRow),
                ..Default::default()
            };
            ui.scroll[ui.page] = std::env::var("VITRO_PREFS_SCROLL").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let ctx = crate::prefs::Ctx { cfg: &cfg, st: crate::prefs::Status::demo() };
            // 捲動字停在第幾秒（沒設就停在開頭）
            let roll = std::env::var("VITRO_MARQUEE_T").ok().and_then(|v| v.parse().ok());
            crate::prefs::shot(&gpu, &target, &mut quads, &mut text, &ctx, ui, roll)?;
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=設定視窗 {:?}", path.display(), page);
            return Ok(());
        }

        // `devices[:empty|err|wait][:morph=0..1]`：裝置選單。預設是三台假裝置，第二台反白。
        // `morph` 是從裝置鍵裡長出來的進度
        if let Some(rest) = scene.strip_prefix("devices") {
            let opts: Vec<&str> = rest.split(':').filter(|o| !o.is_empty()).collect();
            let morph: Option<f32> = opts.iter().find_map(|o| o.strip_prefix("morph=")).and_then(|v| v.parse().ok());
            let mut covers = crate::cover::CoverRenderer::new(&gpu, OFFSCREEN_FORMAT);
            let albums = crate::scene_flow::fake_albums(&gpu, 12);
            crate::scene_flow::render(
                &gpu, &target, &mut quads, &mut covers, &mut glass, &mut text, &albums, 4.0, &bar_under(&albums, crate::ui::devices(lw, lh, 3).panel, lw, lh), 1.0, &veil,
                None,
            )?;
            let list = match opts.first().copied() {
                Some("empty") => Some(Ok(Vec::new())),
                Some("err") => Some(Err("HTTP 401".to_string())),
                Some("wait") => None,
                _ => Some(Ok(crate::scene_devices::demo())),
            };
            let from = morph.map(|_| crate::ui::flow(lw, lh, 1.0).devices);
            crate::scene_devices::render(
                &gpu, &target, &mut quads, &mut glass, &mut text, list.as_ref(), 1, fake_hover(),
                morph.unwrap_or(1.0), from,
            )?;
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=裝置", path.display());
            return Ok(());
        }

        // `setup[:打到一半的字]`：首次設定精靈。`VITRO_SETUP_ERR=1` 看格式錯誤的那行
        if let Some(rest) = scene.strip_prefix("setup") {
            let mut covers = crate::cover::CoverRenderer::new(&gpu, OFFSCREEN_FORMAT);
            let albums = crate::scene_flow::fake_albums(&gpu, 12);
            crate::scene_flow::render(
                &gpu, &target, &mut quads, &mut covers, &mut glass, &mut text, &albums, 4.0, &bar_under(&albums, crate::ui::setup(lw, lh).panel, lw, lh), 1.0, &veil,
                None,
            )?;
            let mut st = crate::scene_setup::Setup::default();
            st.input.text = rest.trim_start_matches(':').to_string();
            st.dash_opened = true;
            if std::env::var_os("VITRO_SETUP_ERR").is_some() {
                st.err = crate::scene_setup::validate(&st.input.text).err().unwrap_or_default();
            }
            let cfg = crate::config::Config::default();
            crate::scene_setup::render(
                &gpu, &target, &mut quads, &mut glass, &mut text, &st, &cfg.redirect_uri(), true,
                fake_hover(),
            )?;
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=設定精靈", path.display());
            return Ok(());
        }

        // `about[:confirm]`：關於面板；`:confirm` 是中斷連結按過一次的樣子
        if let Some(rest) = scene.strip_prefix("about") {
            let mut covers = crate::cover::CoverRenderer::new(&gpu, OFFSCREEN_FORMAT);
            let albums = crate::scene_flow::fake_albums(&gpu, 12);
            crate::scene_flow::render(
                &gpu, &target, &mut quads, &mut covers, &mut glass, &mut text, &albums, 4.0, &bar_under(&albums, crate::ui::about(lw, lh).panel, lw, lh), 1.0, &veil,
                None,
            )?;
            let dir = crate::config::data_dir().display().to_string();
            crate::scene_about::render(
                &gpu, &target, &mut quads, &mut glass, &mut text, fake_hover(), &dir, rest == ":confirm",
            )?;
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=關於", path.display());
            return Ok(());
        }

        // `mini[:lyrics][:hover][:slide=0.4]`：迷你視窗（分層那條，含陰影留白）。
        // `VITRO_MINI_SIZE=寬x高`（邏輯像素，預設 380x120）、`VITRO_MINI_SCALE`（預設 2）、
        // `VITRO_MS` 換歌詞時間點、`VITRO_LYRICS=none|instrumental|loading` 看沒有歌詞的樣子、
        // `VITRO_TITLE`／`VITRO_ARTIST` 換長名字
        if let Some(rest) = scene.strip_prefix("mini") {
            let opts: Vec<&str> = rest.split(':').filter(|s| !s.is_empty()).collect();
            let lyrics_on = opts.contains(&"lyrics");
            let slide: Option<f32> = opts.iter().find_map(|o| o.strip_prefix("slide=")).and_then(|v| v.parse().ok());
            let (lw, lh) = std::env::var("VITRO_MINI_SIZE")
                .ok()
                .and_then(|v| v.split_once('x').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))))
                .unwrap_or(crate::mini::DEFAULT);
            let s: f32 = std::env::var("VITRO_MINI_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(2.0);
            let pad = (28.0 * s).round();
            let (cw, ch) = ((lw * s).round(), (lh * s).round());
            let target = Offscreen::new(&gpu.device, (cw + 2.0 * pad) as u32, (ch + 2.0 * pad) as u32);
            let albums = crate::scene_flow::fake_albums(&gpu, 1);
            let art = match std::env::var("VITRO_ART").ok().and_then(|p| std::fs::read(p).ok()) {
                Some(b) => crate::gfx::upload_encoded(&gpu, &b, "shot-art").unwrap_or_else(|_| albums[0].art.clone()),
                None => albums[0].art.clone(),
            };
            let title = std::env::var("VITRO_TITLE").unwrap_or_else(|_| demo_title().into());
            let artist = std::env::var("VITRO_ARTIST").unwrap_or_else(|_| demo_artist().into());
            let ms: i64 = std::env::var("VITRO_MS").ok().and_then(|v| v.parse().ok()).unwrap_or_else(demo_pos_ms);
            let ly = demo_lyrics();
            let i = crate::scene_now::current_line(&ly, ms).map_or(-1, |i| i as i64);
            let line = |j: i64| ly.synced.get(j as usize).filter(|_| j >= 0).map_or(String::new(), |l| l.text.clone());
            let (idx, cur, next) = match std::env::var("VITRO_LYRICS").ok().as_deref() {
                Some("none") => (i64::MIN, "找不到這首的歌詞".to_string(), String::new()),
                Some("instrumental") => (i64::MIN, "♪ 純音樂".to_string(), String::new()),
                Some("loading") => (i64::MIN, String::new(), String::new()),
                _ => (i, line(i), line(i + 1)),
            };
            // `VITRO_CUR`／`VITRO_NEXT`：換成別的語言的句子（字型 fallback 的成本不一樣）
            let cur = std::env::var("VITRO_CUR").unwrap_or(cur);
            let next = std::env::var("VITRO_NEXT").unwrap_or(next);
            let prev = line(i - 1);
            let c = crate::mini::Content {
                title: &title,
                artist: &artist,
                pos_ms: ms,
                dur_ms: demo_dur_ms(),
                has: true,
                art: Some(art),
                lyric: lyrics_on.then(|| crate::mini::Lyric { key: "demo", idx, cur: &cur, next: &next, next_in_ms: None }),
            };
            let look = crate::mini::Look {
                s,
                pad,
                card: (cw, ch),
                layered: true,
                show_tools: opts.contains(&"hover"),
                hot: None,
                tint: crate::mini::default_tint(),
                lyrics: lyrics_on,
                roll: std::env::var("VITRO_MARQUEE_T").ok().and_then(|v| v.parse().ok()),
            };
            crate::mini::paint(&gpu, &mut quads, &mut text, &target, &look, &c, slide.map(|p| (prev.as_str(), p)))?;
            // `VITRO_BENCH=<次數>`：重畫這麼多次，量每幀 CPU 端花多久（不含讀回）
            if let Some(n) = std::env::var("VITRO_BENCH").ok().and_then(|v| v.parse::<u32>().ok()) {
                let t0 = std::time::Instant::now();
                for _ in 0..n {
                    crate::mini::paint(&gpu, &mut quads, &mut text, &target, &look, &c, None)?;
                    text.end_frame();
                }
                gpu.device.poll(wgpu::PollType::wait_indefinitely()).ok();
                println!("每幀 {:.3} ms", t0.elapsed().as_secs_f64() * 1000.0 / n as f64);
            }
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=迷你視窗", path.display());
            return Ok(());
        }

        // `widgets`：控件畫廊，每種控件的各個狀態排在一起（第 3 階段的新基準）
        if scene == "widgets" {
            let mut covers = crate::cover::CoverRenderer::new(&gpu, OFFSCREEN_FORMAT);
            let albums = crate::scene_flow::fake_albums(&gpu, 12);
            crate::scene_flow::render(
                &gpu, &target, &mut quads, &mut covers, &mut glass, &mut text, &albums, 4.0, &crate::scene_flow::NowBar::demo(&albums, true), 1.0, &veil,
                None,
            )?;
            widget_gallery(&gpu, &target, &mut quads, &mut glass, &mut text)?;
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=控件畫廊", path.display());
            return Ok(());
        }

        // `accents`：一排假封面色（或 `VITRO_ACCENT_ARTS=a.jpg;b.jpg` 的真封面）各自取出的強調色
        if scene == "accents" {
            let mut covers = crate::cover::CoverRenderer::new(&gpu, OFFSCREEN_FORMAT);
            let albums = crate::scene_flow::fake_albums(&gpu, 12);
            crate::scene_flow::render(
                &gpu, &target, &mut quads, &mut covers, &mut glass, &mut text, &albums, 4.0, &crate::scene_flow::NowBar::demo(&albums, true), 1.0, &veil,
                None,
            )?;
            accent_gallery(&gpu, &target, &mut quads, &mut glass, &mut text)?;
            theme::set_accent(None);
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=封面強調色", path.display());
            return Ok(());
        }

        if scene == "help" {
            let mut covers = crate::cover::CoverRenderer::new(&gpu, OFFSCREEN_FORMAT);
            let albums = crate::scene_flow::fake_albums(&gpu, 12);
            crate::scene_flow::render(
                &gpu, &target, &mut quads, &mut covers, &mut glass, &mut text, &albums, 4.0, &bar_under(&albums, crate::scene_help::panel_rect(lw, lh), lw, lh), 1.0, &veil,
                None,
            )?;
            crate::scene_help::render(&gpu, &target, &mut quads, &mut glass, &mut text, true)?;
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=快捷鍵", path.display());
            return Ok(());
        }

        if scene == "now" {
            let albums = crate::scene_flow::fake_albums(&gpu, 1);
            // `VITRO_ART=<檔>`：拿真的封面來驗。示範封面是平滑的漸層，
            // 模糊取樣不足造成的條紋在上面看不出來 —— 要有高頻細節才照得出來。
            let art = match std::env::var("VITRO_ART").ok() {
                Some(p) => match std::fs::read(&p) {
                    Ok(b) => crate::gfx::upload_encoded(&gpu, &b, "shot-art")
                        .unwrap_or_else(|_| albums[0].art.clone()),
                    Err(e) => {
                        println!("讀不到 {p}：{e}");
                        albums[0].art.clone()
                    }
                },
                None => albums[0].art.clone(),
            };
            // `VITRO_LYRICS=none|error|instrumental`：看右欄那三種說明
            let ly = match std::env::var("VITRO_LYRICS").ok() {
                Some(src) => crate::lyrics::Lyrics { source: src, ..Default::default() },
                None => demo_lyrics(),
            };
            // 唱片：照真的播放推進到定位。VITRO_VINYL=0 是方形封面，VITRO_LIFT=1 拍磁頭抬起
            let mut vinyl = crate::vinyl::Vinyl::default();
            let lift = std::env::var("VITRO_LIFT").is_ok_and(|v| v == "1");
            for i in 0..900 {
                vinyl.step(1.0 / 60.0, true, !(lift && i > 880), demo_pos_ms() as f32 / demo_dur_ms() as f32, 3.0);
            }
            if lift {
                vinyl.lift = 1.0;
            }
            let vinyl_on = !std::env::var("VITRO_VINYL").is_ok_and(|v| v == "0");
            // `VITRO_MS=<毫秒>` 換歌詞時間點；`VITRO_SCROLL=<句>` 拍換句捲到一半的樣子
            let ms: i64 = std::env::var("VITRO_MS").ok().and_then(|v| v.parse().ok()).unwrap_or_else(demo_pos_ms);
            let scroll: f32 = std::env::var("VITRO_SCROLL").or_else(|_| std::env::var("VITRO_BROWSE")).ok().and_then(|v| v.parse().ok()).unwrap_or_else(
                || crate::scene_now::current_line(&ly, ms).map_or(-1.0, |i| i as f32),
            );
            // `VITRO_TITLE`／`VITRO_ARTIST` 換長名字（看捲動字，配 `VITRO_MARQUEE_T`）
            let title = std::env::var("VITRO_TITLE").unwrap_or_else(|_| demo_title().into());
            let artist = std::env::var("VITRO_ARTIST").unwrap_or_else(|_| demo_artist().into());
            let now = crate::scene_now::Now {
                title: &title,
                artist: &artist,
                album: demo_title(),
                art,
                position_ms: demo_pos_ms(),
                lyric_ms: ms,
                duration_ms: demo_dur_ms(),
                lyrics: Some(&ly),
                playing: fake_playing(),
                // 模式鍵開著的樣子（綠色＋小圓點、單曲重複的「1」）也一起拍到
                shuffle: true,
                repeat: crate::smtc::Repeat::Track,
                ktv: true,
                show_trans: std::env::var("VITRO_TRANS").is_ok(),
                tools: crate::ui::Tools { liked: Some(false), volume: Some(62), vol_locked: false },
                vol_drag: false,
                vinyl: vinyl_on.then_some(&vinyl),
                bg_prev: None,
                lyric_scroll: scroll,
                // `VITRO_BROWSE=<句>` 拍用滾輪捲走的樣子；`VITRO_PICK=<句>` 拍滑鼠停在那句上
                pick: {
                    let browse = std::env::var("VITRO_BROWSE").is_ok();
                    crate::scene_now::LyricPick {
                        focus: if browse { crate::scene_now::current_line(&ly, ms).map_or(-1.0, |i| i as f32) } else { scroll },
                        browse: if browse { 1.0 } else { 0.0 },
                        hover: std::env::var("VITRO_PICK").ok().and_then(|v| v.parse().ok()).map(|i| (i, 1.0)),
                        ..Default::default()
                    }
                },
                roll: std::env::var("VITRO_MARQUEE_T").ok().and_then(|v| v.parse().ok()),
                fx: Default::default(),
            };
            let c = std::time::Instant::now();
            crate::scene_now::render(
                &gpu, &target, &mut quads, &mut glass, &mut text, &now, fake_chrome(), fake_hover(),
            )?;
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
            if std::env::var("VITRO_BENCH").is_ok() {
                println!("第一幀（快取全空）{:.2} ms", c.elapsed().as_secs_f64() * 1e3);
            }
            // `VITRO_BENCH=<幀數>`：連畫 N 幀量每幀時間（CPU 錄製＋等 GPU 做完）
            if let Some(n) = std::env::var("VITRO_BENCH").ok().and_then(|v| v.parse::<u32>().ok()) {
                let mut cpu = 0.0f64;
                let t0 = std::time::Instant::now();
                for _ in 0..n {
                    let c = std::time::Instant::now();
                    crate::scene_now::render(
                        &gpu, &target, &mut quads, &mut glass, &mut text, &now, fake_chrome(), fake_hover(),
                    )?;
                    cpu += c.elapsed().as_secs_f64();
                    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
                }
                let all = t0.elapsed().as_secs_f64();
                println!("bench {n} 幀：每幀 {:.2} ms（CPU {:.2} ms）", all * 1e3 / n as f64, cpu * 1e3 / n as f64);
            }
            fake_toast(&gpu, &target, &mut quads, &mut glass, &mut text, height as f32 - 34.0, false)?;
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=歌詞", path.display());
            return Ok(());
        }

        // `search[:查詢字串]`：封面流＋搜尋膠囊。`VITRO_PREEDIT=ㄓㄡ` 看組字中的樣子。
        if let Some(rest) = scene.strip_prefix("search") {
            let q = rest.trim_start_matches(':');
            let all = crate::scene_flow::fake_albums(&gpu, 12);
            let nq = crate::search::norm(q);
            let albums: Vec<_> = all
                .into_iter()
                .filter(|a| {
                    nq.is_empty()
                        || a.name.to_lowercase().contains(&nq)
                        || a.artist.to_lowercase().contains(&nq)
                })
                .collect();
            let mut covers = crate::cover::CoverRenderer::new(&gpu, OFFSCREEN_FORMAT);
            crate::scene_flow::render(
                &gpu, &target, &mut quads, &mut covers, &mut glass, &mut text, &albums, 0.0,
                &crate::scene_flow::NowBar::demo(&albums, fake_playing()), 1.0, &veil, fake_hover(),
            )?;
            let input = crate::search::Input {
                text: q.to_string(),
                preedit: std::env::var("VITRO_PREEDIT").unwrap_or_default(),
            };
            crate::scene_search::render_search(
                &gpu, &target, &mut quads, &mut glass, &mut text, &input,
                !nq.is_empty() && albums.is_empty(), 1.0, true, 10.0,
            )?;
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=搜尋「{q}」→ {} 張", path.display(), albums.len());
            return Ok(());
        }

        // `picker[:查詢字串]`：視覺效果＋preset 選單。反白停在清單第 3 列，
        // 看得到「反白」跟「目前在跑的那顆（▸）」是兩件事。
        if let Some(rest) = scene.strip_prefix("picker") {
            let q = rest.trim_start_matches(':');
            let spec: crate::audio::Shared = Default::default();
            let mut v = crate::viz::Viz::new(&crate::config::Config {
                viz_favourites: vec![
                    "martin - amandio c - a different view of the green machine".into(),
                ],
                ..Default::default()
            });
            let wait = std::time::Instant::now();
            while !v.ready() && wait.elapsed().as_secs_f32() < 20.0 {
                v.take_load();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            for _ in 0..60 {
                glass.end_frame();
                crate::scene_viz::render(
                    &gpu, &target, &mut quads, &mut glass, &mut text, &mut v, &spec,
                    &format!("{} — {}", demo_artist(), demo_title()), 1.0, 1.0, None, 1.0 / 60.0,
                )?;
            }
            let nq = crate::search::norm(q);
            let rows: Vec<usize> = v
                .pool()
                .into_iter()
                .filter(|i| crate::search::name_matches(v.item_name(*i), &nq))
                .collect();
            let l = crate::ui::picker(lw, lh);
            let cur = v.current_item();
            let at = rows.iter().position(|i| Some(*i) == cur).map(|a| a + 2).unwrap_or(2)
                .min(rows.len().saturating_sub(1));
            let top = crate::ui::picker_centre(at, l.rows, rows.len());
            let input = crate::search::Input {
                text: q.to_string(),
                preedit: std::env::var("VITRO_PREEDIT").unwrap_or_default(),
            };
            let view = crate::scene_search::PickerView { rows: &rows, at, top, input: &input,
                                                         caret_on: true };
            let mut fit = crate::scene_search::FitCache::default();
            crate::scene_search::render_picker(
                &gpu, &target, &mut quads, &mut glass, &mut text, &v, &view, &mut fit, None,
            )?;
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=preset 選單「{q}」→ {} 個", path.display(), rows.len());
            return Ok(());
        }

        // `viz-live`：等 preset 載好，跑滿 90 幀，看的是「MilkDrop ＋ 控制列」
        // 疊在一起的樣子。限幀與中繼畫布那條路徑只有這裡看得到。
        // `viz-live`：等 preset 載好、跑滿一段，看的是「MilkDrop ＋ 控制列」
        // 疊起來的樣子。限幀與中繼畫布那條路徑只有這裡看得到。
        // `viz-live:blend` 會在中途換一次 preset，停在溶解到一半的地方。
        if let Some(rest) = scene.strip_prefix("viz-live") {
            // 後面接數字 = 換 preset 之後再跑幾幀（溶解 2.8 秒 ≈ 168 幀）
            let after: Option<u32> = rest.trim_start_matches(':').parse().ok();
            let blend = after.is_some() || rest.trim_start_matches(':') == "blend";
            let after = after.unwrap_or(84);
            let spec: crate::audio::Shared = Default::default();
            let mut v = crate::viz::Viz::new(&crate::config::Config::default());
            // `VITRO_LOCKED=1`：看「· 已鎖定」那段有沒有畫對
            v.locked = std::env::var_os("VITRO_LOCKED").is_some();
            let wait = std::time::Instant::now();
            while !v.ready() && wait.elapsed().as_secs_f32() < 20.0 {
                v.take_load();
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            println!("preset 載入{}，共 {} 個", if v.ready() { "完成" } else { "逾時" }, v.count());
            // 溶解要 2.8 秒，所以換完之後再跑 84 幀（1.4 秒）剛好停在一半
            let total = if blend { 90 + after } else { 90 };
            let mut names = Vec::new();
            for f in 0..total {
                if blend && f == 90 {
                    names.push(v.current_name().to_string());
                    v.step(1);
                }
                let t = f as f32 / 60.0;
                if let Ok(mut s) = spec.lock() {
                    s.alive = true;
                    s.bass = 0.55 + 0.45 * (t * 5.2).sin();
                    s.mid = 0.45 + 0.35 * (t * 8.1).sin();
                    s.treb = 0.35 + 0.30 * (t * 12.7).sin();
                    for i in 0..crate::audio::WAVE {
                        let p = i as f32 / crate::audio::WAVE as f32;
                        s.wave[i] = (p * 40.0 + t * 9.0).sin() * 0.5 * s.bass
                            + (p * 130.0 + t * 21.0).sin() * 0.2 * s.treb;
                    }
                    for i in 0..crate::audio::BINS {
                        let p = i as f32 / crate::audio::BINS as f32;
                        s.bins[i] = (1.0 - p).powf(1.6) * s.bass;
                    }
                }
                // 每一輪算一幀，玻璃的場景序號跟實機一樣每幀從 0 數
                glass.end_frame();
                crate::scene_viz::render(
                    &gpu, &target, &mut quads, &mut glass, &mut text, &mut v, &spec,
                    &format!("{} — {}", demo_artist(), demo_title()), fake_chrome(), 1.0, fake_hover(), 1.0 / 60.0,
                )?;
            }
            names.push(v.current_name().to_string());
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=視覺效果（{}）", path.display(), names.join(" → "));
            return Ok(());
        }

        if scene == "viz" {
            // 造一組看得出形狀的假頻譜：低頻高、高頻低，加上幾個峰。
            let spec: crate::audio::Shared = Default::default();
            if let Ok(mut s) = spec.lock() {
                for i in 0..crate::audio::BINS {
                    let t = i as f32 / crate::audio::BINS as f32;
                    let base = (1.0 - t).powf(1.6) * 0.8;
                    let peak = (-((t - 0.34) * 18.0).powi(2)).exp() * 0.5
                        + (-((t - 0.62) * 26.0).powi(2)).exp() * 0.35;
                    s.bins[i] = (base + peak).clamp(0.02, 1.0);
                }
                s.bass = 0.72;
                s.alive = true;
            }
            // 預設餵一個沒載 preset 的 Viz，讓它走替代畫面（頻譜長條）。
            let mut v = crate::viz::Viz::new(&crate::config::Config::default());
            // `VITRO_QUIET=1`：假裝進來很久了而且一直沒聲音，看「沒有偵測到聲音」的提示
            if std::env::var_os("VITRO_QUIET").is_some() {
                v.entered -= std::time::Duration::from_secs(10);
            }
            crate::scene_viz::render(
                &gpu, &target, &mut quads, &mut glass, &mut text, &mut v, &spec,
                &format!("{} — {}", demo_artist(), demo_title()), fake_chrome(), 1.0, fake_hover(), 1.0 / 60.0,
            )?;
            let bottom = crate::ui::viz(lw, lh).bar.y - 16.0;
            fake_toast(&gpu, &target, &mut quads, &mut glass, &mut text, bottom, true)?;
            target.save_png(&gpu, path)?;
            println!("寫出 {} 場景=視覺效果", path.display());
            return Ok(());
        }

        if scene != "test" {
            // 封面流。`scene` 可以直接寫成小數，用來看捲動途中的樣子。
            let centre: f32 = scene.parse().unwrap_or(4.0);
            let mut covers = crate::cover::CoverRenderer::new(&gpu, OFFSCREEN_FORMAT);
            let albums = crate::scene_flow::fake_albums(&gpu, 12);
            let mut bar = crate::scene_flow::NowBar::demo(&albums, fake_playing());
            // `VITRO_BACK=名字`：展開中，左上角的返回鍵
            let back = std::env::var("VITRO_BACK").ok().map(|n| {
                let name = text.fit_weight(&n, 13.5, 18.0, crate::ui::drill_back_text_max(lw), 600);
                let (w, _) = text.measure_weight(&name, 13.5, 18.0, None, 600);
                (name, w)
            });
            bar.back = back.as_ref().map(|(n, w)| (n.as_str(), *w));
            // `VITRO_LIKE=none|off|on`、`VITRO_VOL=0..100|locked`
            match std::env::var("VITRO_LIKE").ok().as_deref() {
                Some("none") => bar.tools.liked = None,
                Some("off") => bar.tools.liked = Some(false),
                _ => {}
            }
            match std::env::var("VITRO_VOL").ok().as_deref() {
                Some("locked") => bar.tools.vol_locked = true,
                Some(v) => bar.tools.volume = v.parse().ok(),
                None => {}
            }
            crate::scene_flow::render(
                &gpu, &target, &mut quads, &mut covers, &mut glass, &mut text, &albums, centre,
                &bar, 1.0, &veil, fake_hover(),
            )?;
            let bottom = crate::ui::flow(lw, lh, 1.0).bar.y - 20.0;
            fake_toast(&gpu, &target, &mut quads, &mut glass, &mut text, bottom, false)?;
            target.save_png(&gpu, path)?;
            println!("寫出 {} ({}x{}) 場景=封面流 中心={centre}", path.display(), width, height);
            return Ok(());
        }

        // 底層：玻璃底下會被看到的東西
        quads.begin(&gpu);
        build_test_scene(&gpu, &mut quads, lw, lh);

        // 玻璃面板
        glass.begin();
        build_glass_scene(&mut glass);

        // 頂層文字
        text.begin();
        build_text_scene(&mut text);
        text.prepare(&gpu, width, height)?;

        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("shot") });

        // ---- 第 1 趟：把底層畫進 scene ----
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.05, g: 0.05, b: 0.07, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            quads.flush(&gpu, &mut pass, width, height);
        }

        // ---- 第 2 趟：模糊 ----
        let backdrop = glass.blur_scene(&gpu, &mut enc, &target, 30.0);
        let sharp = glass.sharp_scene(&gpu, &mut enc, &target);

        // ---- 第 3、4 趟：玻璃疊上去，再蓋文字 ----
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("glass + text"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // 留著第 1 趟的結果，玻璃是疊上去的
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            glass.render(&gpu, &mut pass, &backdrop, &sharp, width, height);
            text.render(&mut pass)?;
        }
        gpu.queue.submit([enc.finish()]);

        target.save_png(&gpu, path)?;
        println!("寫出 {} ({}x{})", path.display(), width, height);
        Ok(())
    };
    draw()?;
    draw()
}

/// 玻璃面板故意壓在對比強烈的東西上 —— 折射與模糊在純色上看不出來。
fn build_glass_scene(g: &mut Glass) {
    // 大面板，蓋住底下的貼圖與色塊
    g.push(
        Panel::new(120.0, 400.0, 420.0, 170.0)
            .radius(28.0)
            .tint([1.0, 1.0, 1.0, 0.10])
            .edge(22.0, 12.0, 2.0, 0.30),
    );
    // 折射強度由弱到強，一眼看出差別
    for (i, s) in [0.0f32, 6.0, 14.0, 26.0].iter().enumerate() {
        g.push(
            Panel::new(600.0 + i as f32 * 110.0, 430.0, 96.0, 200.0)
                .radius(20.0)
                .tint([1.0, 1.0, 1.0, 0.06])
                .edge(20.0, *s, 1.6, 0.28),
        );
    }
    // 圓形玻璃（半徑大於一半就會變圓），壓在色塊邊界上
    g.push(
        Panel::new(1060.0, 250.0, 160.0, 160.0)
            .radius(999.0)
            .tint([0.6, 0.8, 1.0, 0.12])
            .edge(26.0, 18.0, 2.0, 0.4),
    );
}

/// 把 quad 渲染器的每個能力各畫一次，好一眼看出哪裡壞了。
fn build_test_scene(gpu: &Gpu, q: &mut QuadRenderer, w: f32, _h: f32) {
    // 背景的幾條帶子，用來確認 alpha 混合與疊放順序
    q.push(Quad::new(0.0, 0.0, w, 90.0).color([0.10, 0.11, 0.16, 1.0]));

    // 一排半徑不同的圓角
    for (i, r) in [0.0f32, 4.0, 12.0, 28.0, 60.0].iter().enumerate() {
        let x = 40.0 + i as f32 * 130.0;
        q.push(
            Quad::new(x, 130.0, 110.0, 110.0)
                .color([0.35, 0.72, 0.95, 1.0])
                .radius(*r),
        );
    }

    // 陰影：模糊半徑由小到大。要墊一層淺色底才看得出來 —— 黑影打在
    // 近乎黑的背景上是看不見的，那不是 bug。
    q.push(Quad::new(20.0, 275.0, 520.0, 165.0).color([0.82, 0.83, 0.86, 1.0]).radius(14.0));
    for (i, blur) in [2.0f32, 8.0, 20.0, 40.0].iter().enumerate() {
        let x = 40.0 + i as f32 * 130.0;
        q.push(
            Quad::new(x, 300.0, 110.0, 110.0)
                .color([1.0, 1.0, 1.0, 1.0])
                .radius(18.0)
                .shadow(0.0, 10.0, *blur, 0.0, [0.0, 0.0, 0.0, 0.65]),
        );
    }

    // 外框 + 半透明底（Liquid Glass 的雛形，還沒有折射）
    q.push(
        Quad::new(560.0, 300.0, 240.0, 110.0)
            .color([1.0, 1.0, 1.0, 0.10])
            .radius(24.0)
            .border(1.5, [1.0, 1.0, 1.0, 0.45]),
    );

    // 四角不同半徑
    q.push(
        Quad::new(830.0, 300.0, 160.0, 110.0)
            .color([0.95, 0.45, 0.35, 1.0])
            .radius4(40.0, 4.0, 40.0, 4.0),
    );

    // 貼圖：現場合成一張漸層當假封面，順便驗 cover 裁切
    let (tw, th) = (256u32, 256u32);
    let mut rgba = Vec::with_capacity((tw * th * 4) as usize);
    for y in 0..th {
        for x in 0..tw {
            let fx = x as f32 / tw as f32;
            let fy = y as f32 / th as f32;
            // 斜向漸層加上格線，裁切有沒有對可以直接用格線看出來
            let grid = if (x % 32 < 2) || (y % 32 < 2) { 40u8 } else { 0 };
            rgba.push(((fx * 200.0) as u8).saturating_add(grid));
            rgba.push(((fy * 180.0) as u8).saturating_add(grid));
            rgba.push((((1.0 - fx) * 210.0) as u8).saturating_add(grid));
            rgba.push(255);
        }
    }
    let tex = crate::gfx::upload_rgba(gpu, tw, th, &rgba, "test");

    // 正方形（應該完整顯示）
    q.push_tex(
        Quad::new(40.0, 470.0, 160.0, 160.0)
            .radius(16.0)
            .cover(tw as f32, th as f32)
            .shadow(0.0, 14.0, 28.0, -4.0, [0.0, 0.0, 0.0, 0.7]),
        &tex,
    );
    // 寬的（應該左右滿、上下被裁）
    q.push_tex(
        Quad::new(230.0, 470.0, 300.0, 160.0)
            .radius(16.0)
            .cover(tw as f32, th as f32),
        &tex,
    );
    // 高的（應該上下滿、左右被裁）
    q.push_tex(
        Quad::new(560.0, 470.0, 110.0, 200.0)
            .radius(16.0)
            .cover(tw as f32, th as f32),
        &tex,
    );
    // 整張鋪滿、不維持比例
    q.push_tex(Quad::new(700.0, 470.0, 300.0, 120.0).radius(8.0).textured(), &tex);

    // 疊在貼圖上的半透明白，驗混合
    q.push(Quad::new(1040.0, 130.0, 200.0, 500.0).color([1.0, 1.0, 1.0, 0.08]).radius(20.0));
    for i in 0..5 {
        let a = 0.15 + i as f32 * 0.2;
        q.push(
            Quad::new(1060.0, 160.0 + i as f32 * 95.0, 160.0, 70.0)
                .color([1.0, 0.85, 0.3, a])
                .radius(12.0),
        );
    }
}

/// 中文排版是這一關最大的風險，所以拿歌詞樣的句子（`demo_lyrics` 那幾句）來試，
/// 混上英數與標點，看 fallback 鏈會不會斷。
fn build_text_scene(t: &mut Text) {
    t.push("VitroDeck　原生介面測試", 40.0, 22.0, 30.0, 40.0, [1.0, 1.0, 1.0, 1.0], 700, None, None);

    // 歌名／演出者，粗細與大小不同
    t.push("一場遊戲一場夢", 40.0, 660.0, 26.0, 34.0, [1.0, 1.0, 1.0, 0.95], 600, None, None);
    t.push("王傑 · 一場遊戲一場夢 (1987)", 260.0, 666.0, 17.0, 24.0, [1.0, 1.0, 1.0, 0.55], 400, None, None);

    // 逐句歌詞，驗長句斷行
    t.push(
        "窗外的雨還沒有停　我把燈關掉又重新打開\nThat's just how the night goes — 只是腳步太急",
        560.0,
        305.0,
        19.0,
        30.0,
        [0.1, 0.1, 0.12, 1.0],
        400,
        Some(230.0),
        None,
    );

    // 小字級：設定面板那種密集清單
    for (i, s) in ["影格上限　60 fps", "類別　復古", "簡繁轉換　開", "封面大小　100%"]
        .iter()
        .enumerate()
    {
        t.push(s, 1060.0, 168.0 + i as f32 * 95.0, 15.0, 20.0, [0.15, 0.12, 0.05, 0.9], 500, None, None);
    }

    // 標點與全形半形混排
    t.push(
        "「引號」、逗號，句號。ABC 123 ×÷ ♪",
        40.0,
        690.0,
        16.0,
        22.0,
        [0.6, 0.85, 1.0, 1.0],
        400,
        None,
        None,
    );
}

fn demo_trans(t: i64) -> Option<String> {
    let s = match t {
        35_100 => "The rain outside hasn't stopped yet",
        39_400 => "I turn off the light, then turn it back on",
        44_100 => "The city slowly wakes up far away",
        72_300 => "Why does the moon move so slowly, and why does the morning come so fast, before we have said anything we meant to say",
        78_000 => "夜晚就是這樣過去的",
        _ => return None,
    };
    Some(s.to_string())
}

/// `VITRO_DEMO=anthem`：示範曲換成中華民國國歌（詞曲皆已進公有領域），給網站截圖用。
/// 不設的話一切照舊，截圖基準不受影響。
pub fn anthem() -> bool {
    std::env::var("VITRO_DEMO").is_ok_and(|v| v == "anthem")
}
pub fn demo_title() -> &'static str {
    if anthem() { "中華民國國歌" } else { "一場遊戲一場夢" }
}
pub fn demo_artist() -> &'static str {
    if anthem() { "程懋筠" } else { "王傑" }
}
pub fn demo_pos_ms() -> i64 {
    if anthem() { 31_000 } else { 64_300 }
}
pub fn demo_dur_ms() -> i64 {
    if anthem() { 58_000 } else { 255_333 }
}

/// 給 `--shot now` 用的假歌詞（自己寫的，不是哪首歌）。刻意混中英與長短句。
fn demo_lyrics() -> crate::lyrics::Lyrics {
    let lines: Vec<(i64, &str)> = if anthem() {
        vec![
            (5_000, "三民主義 吾黨所宗"),
            (12_500, "以建民國 以進大同"),
            (20_000, "咨爾多士 為民前鋒"),
            (27_500, "夙夜匪懈 主義是從"),
            (35_000, "矢勤矢勇 必信必忠"),
            (42_500, "一心一德 貫徹始終"),
        ]
    } else {
        vec![
            (35_100, "窗外的雨還沒有停"),
            (39_400, "我把燈關掉又重新打開"),
            (44_100, "城市在很遠的地方醒來"),
            (49_000, "你在左邊 我在右邊"),
            (54_200, "不要急著說再見"),
            (58_900, "我把這首歌留到最後"),
            (62_000, "那是今晚最後一班列車"),
            (67_100, "你在左邊 我在右邊"),
            (72_300, "為什麼月亮走得慢 天又亮得那麼快"),
            (78_000, "That's just how the night goes"),
            (82_600, "只是腳步太急"),
            (87_400, "不要再回頭"),
        ]
    };
    // 逐字掃光給哪幾行、每行掃多久
    let (word_a, word_b, span) = if anthem() { (27_500, -1, 7_000) } else { (62_000, 72_300, 4_600) };
    // 逐字只給一行 —— `--shot now` 停在 62 秒，剛好是這行正在唱。
    // 每個字平均分時間，看得出掃光掃到哪就夠了。
    let words_of = |t: i64, s: &str| {
        let n = s.chars().count().max(1) as i64;
        let per = span / n;
        Some(
            s.chars()
                .enumerate()
                .map(|(i, c)| crate::lyrics::Word {
                    t: t + i as i64 * per,
                    d: per,
                    text: c.to_string(),
                })
                .collect::<Vec<_>>(),
        )
    };
    crate::lyrics::Lyrics {
        synced: lines
            .iter()
            .map(|(t, s)| crate::lyrics::Line {
                t: *t,
                text: s.to_string(),
                // 72.3 秒那句夠長，小視窗會折行，拿來驗折行的逐字
                words: if *t == word_a || *t == word_b { words_of(*t, s) } else { None },
                // VITRO_TRANS 才畫翻譯；有的句子故意不給，看沒有翻譯的那幾句高度對不對
                trans: demo_trans(*t),
                ..Default::default()
            })
            .collect(),
        plain: String::new(),
        source: "word".into(),
        provider: "示範".into(),
        error: None,
        ..Default::default()
    }
}


/// 示範用的控制列，`panel` 蓋到它的話控制列消散（跟主程式 `panel_over_bar` 同一條規則）
fn bar_under<'a>(albums: &'a [crate::scene_flow::Album], panel: crate::ui::Rect, lw: f32, lh: f32) -> crate::scene_flow::NowBar<'a> {
    let mut b = crate::scene_flow::NowBar::demo(albums, true);
    if panel.overlaps(&crate::ui::flow(lw, lh, 1.0).bar) {
        b.fx.bar_k = 0.0;
    }
    b
}

/// `VITRO_TOAST=<一句話>`：疊一則通知列上去。`VITRO_TOAST_AGE=<秒>` 看淡入淡出
/// 途中的樣子（預設 1 秒，完全顯示）。封面流、歌詞、視覺效果三個場景有接。
/// `clear` = 視覺效果頁（clear 玻璃）
fn fake_toast(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    bottom: f32,
    clear: bool,
) -> anyhow::Result<()> {
    let Ok(msg) = std::env::var("VITRO_TOAST") else { return Ok(()) };
    let age = std::env::var("VITRO_TOAST_AGE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0);
    crate::scene_toast::render(gpu, target, quads, glass, text, &msg, age, bottom, clear)
}

/// 控件畫廊：玻璃面板裡左邊側欄、右邊兩欄分組，每一列是一種狀態。
/// 版面隨手排的，只求每個狀態都看得到；真正的設定視窗版面是第 4 階段的事。
fn widget_gallery(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
) -> anyhow::Result<()> {
    use crate::ui::Rect;
    use crate::widgets::{self as wd, Kind, Paint, State};
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let panel = Rect::new(24.0, 24.0, fw - 48.0, fh - 48.0);

    let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("widgets") });
    quads.begin(gpu);
    quads.push(Quad::new(0.0, 0.0, fw, fh).color(theme::modal(0.42)));
    {
        let mut pass = crate::gfx::pass(&mut enc, &target.view, "widgets-dim", None);
        quads.flush(gpu, &mut pass, w, h);
    }
    let backdrop = glass.blur_scene(gpu, &mut enc, target, theme::GLASS_PANEL.sigma);
    let sharp = glass.sharp_scene(gpu, &mut enc, target);
    glass.begin();
    glass.push(Panel::new(panel.x, panel.y, panel.w, panel.h).radius(theme::radius::PANEL).style(&theme::GLASS_PANEL));
    text.begin();

    let mut an = wd::Anims::default();
    let mut p = Paint::new(gpu, quads, text);
    p.text.push("控件", panel.x + theme::space::PANEL_X, panel.y + theme::space::PANEL_TITLE_Y,
                theme::ty::TITLE.size, theme::ty::TITLE.line, theme::ink(0.95), 600, None, None);

    let idle = State::default();
    let hot = State { hot: true, ..idle };
    let down = State { hot: true, down: true, ..idle };
    let focus = State { focus: true, ..idle };
    let off = State { disabled: true, ..idle };

    // ---- 側欄
    let top = panel.y + 64.0;
    let (sx, sw) = (panel.x + 12.0, 200.0);
    let pages = ["帳號與播放", "顯示", "歌詞", "唱片", "迷你視窗", "視覺效果", "關於與更新"];
    for (i, name) in pages.iter().enumerate() {
        let st = match i {
            3 => hot,
            5 => focus,
            _ => idle,
        };
        wd::side_item(&mut p, &wd::side_rect(sx, top, sw, i), None, name, i == 1, st);
    }
    let icons_top = top + pages.len() as f32 * theme::ctl::SIDE_H + 40.0;
    p.text.push("有圖示", sx + 10.0, icons_top - 22.0, theme::ty::CAPTION.size, theme::ty::CAPTION.line,
                theme::ink(0.55), 600, None, None);
    wd::side_item(&mut p, &wd::side_rect(sx, icons_top, sw, 0), Some(crate::icons::Svg::Device), "播放裝置", true, idle);
    wd::side_item(&mut p, &wd::side_rect(sx, icons_top, sw, 1), Some(crate::icons::Svg::Gear), "一般", false, hot);
    let div_x = sx + sw + 12.0;
    p.quads.push(Quad::new(div_x, panel.y + 56.0, 1.0, panel.h - 76.0).color(theme::ink(theme::ctl::DIV_A)));

    // ---- 兩欄分組
    let gap = 24.0;
    let cx0 = div_x + 1.0 + gap;
    let col_w = (panel.x + panel.w - gap - cx0 - gap) * 0.5;
    let cols = [cx0, cx0 + col_w + gap];
    // 一組：標題、列高、每列畫什麼（回傳控件佔掉的寬度，標籤要讓開）
    type Ctrl<'r> = Box<dyn Fn(&mut Paint, &Rect) -> f32 + 'r>;
    type Row<'r> = (&'r str, &'r str, Ctrl<'r>);
    let draw_group = |p: &mut Paint, x: f32, y: &mut f32, title: &str, rows: Vec<Row>| {
        let hs: Vec<f32> = rows.iter().map(|r| wd::row_h(!r.1.is_empty())).collect();
        let (g, rs) = wd::group_rows(x, *y + wd::group_title_h(), col_w, &hs);
        wd::group_title(p, &g, title);
        wd::group(p, &g, &rs);
        for (r, (label, hint, ctrl)) in rs.iter().zip(rows) {
            let used = ctrl(p, r);
            let max_w = r.w - theme::ctl::ROW_PAD * 2.0 - used - 12.0;
            wd::row_label(p, r, label, hint, max_w, idle);
        }
        *y = g.y + g.h + 22.0;
    };
    // 一列放好幾個開關，由左到右照給的順序
    let switch_row = |items: Vec<(f32, State)>| -> Ctrl<'static> {
        Box::new(move |p, r| {
            let (mut right, cy) = wd::row_ctrl(r);
            let start = right;
            for (t, st) in items.iter().rev() {
                wd::switch(p, &wd::switch_rect(right, cy), *t, *st, if st.down { 1.0 } else { 0.0 });
                right -= theme::ctl::SWITCH_W + 14.0;
            }
            start - right
        })
    };
    let buttons_row = |items: Vec<(&'static str, Kind, State)>| -> Ctrl<'static> {
        Box::new(move |p, r| {
            let (mut right, cy) = wd::row_ctrl(r);
            let start = right;
            for (label, kind, st) in items.iter().rev() {
                let bw = wd::button_width(p.text, label);
                let b = Rect::new(right - bw, cy - theme::ctl::BTN_H * 0.5, bw, theme::ctl::BTN_H);
                wd::button(p, &b, label, *kind, *st);
                right -= bw + 8.0;
            }
            start - right
        })
    };
    let slider_row = |v: f32, steps: Option<usize>, st: State| -> Ctrl<'static> {
        Box::new(move |p, r| {
            let (right, cy) = wd::row_ctrl(r);
            let sw = 200.0;
            wd::slider(p, &wd::slider_rect(right - sw, cy, sw), v, steps, st, if st.down { 1.0 } else { 0.0 });
            sw
        })
    };
    let seg_row = |labels: Vec<&'static str>, sel: f32, hot_i: Option<usize>, st: State| -> Ctrl<'static> {
        Box::new(move |p, r| {
            let (right, cy) = wd::row_ctrl(r);
            let sw = wd::segmented_width(p.text, &labels);
            let s = Rect::new(right - sw, cy - theme::ctl::SEG_H * 0.5, sw, theme::ctl::SEG_H);
            wd::segmented(p, &s, &labels, sel, hot_i, st);
            sw
        })
    };

    let mut y0 = panel.y + 56.0;
    let sw_on = an.switch(1, true);
    draw_group(&mut p, cols[0], &mut y0, "開關", vec![
        ("關／開", "", switch_row(vec![(0.0, idle), (sw_on, idle)])),
        ("指著（關）／按住（開）", "", switch_row(vec![(0.0, hot), (1.0, down)])),
        ("滑到一半／滑過頭回彈", "", switch_row(vec![(0.5, idle), (1.08, idle)])),
        ("停用（關／開）／焦點", "", switch_row(vec![(0.0, off), (1.0, off), (1.0, focus)])),
    ]);
    draw_group(&mut p, cols[0], &mut y0, "按鈕", vec![
        ("一般", "", buttons_row(vec![("平常", Kind::Normal, idle), ("指著", Kind::Normal, hot), ("按住", Kind::Normal, down)])),
        ("主要", "", buttons_row(vec![("平常", Kind::Primary, idle), ("指著", Kind::Primary, hot), ("按住", Kind::Primary, down)])),
        ("危險", "", buttons_row(vec![("平常", Kind::Danger, idle), ("指著", Kind::Danger, hot), ("按住", Kind::Danger, down)])),
        ("停用與焦點", "", buttons_row(vec![("停用", Kind::Normal, off), ("停用", Kind::Primary, off), ("焦點", Kind::Normal, focus)])),
    ]);

    let mut y = panel.y + 56.0;
    let rpm = wd::slider_step(6.0 / 7.0, 8) as f32 / 7.0;
    draw_group(&mut p, cols[1], &mut y, "滑桿", vec![
        ("連續", "封面大小、音量這類", slider_row(0.35, None, idle)),
        ("有刻度", "唱片轉速的八個刻度，停在第七格", slider_row(rpm, Some(8), idle)),
        ("拖曳中", "旋鈕變成玻璃，看得到底下放大的軌道", slider_row(0.55, None, down)),
        ("停用與焦點", "焦點環畫在旋鈕外面", slider_row(0.5, Some(5), focus)),
    ]);
    let sort = vec!["加入時間", "專輯名", "演出者", "年份"];
    let seg_sel = an.segment(2, 0);
    draw_group(&mut p, cols[1], &mut y, "分段控制", vec![
        ("選第一段", "", seg_row(sort.clone(), seg_sel, None, idle)),
        ("指著第三段", "", seg_row(sort.clone(), 1.0, Some(2), hot)),
        ("滑動中", "", seg_row(vec!["30", "60", "120", "不限"], 1.5, None, idle)),
        ("停用", "", seg_row(sort, 3.0, None, off)),
    ]);
    // 「設定列」那組在第四批列高加大後放不下了；設定列的樣子看 `prefs:<頁>` 的截圖

    text.prepare(gpu, w, h)?;
    {
        let mut pass = crate::gfx::pass(&mut enc, &target.view, "widgets-glass", None);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    }
    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok(())
}

fn parse_hex(s: &str) -> Option<[f32; 3]> {
    let v = u32::from_str_radix(s.trim_start_matches('#'), 16).ok()?;
    Some([(v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff].map(|c| c as f32 / 255.0))
}

/// 封面強調色一覽：每列左邊是封面（或它的代表色），右邊是取出來的強調色套在
/// 開關、滑桿、主要按鈕、選單反白上。第一列是預設的 Spotify 綠，拿來對照亮度。
fn accent_gallery(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
) -> anyhow::Result<()> {
    use crate::ui::Rect;
    use crate::widgets::{self as wd, Kind, Paint, State};
    type Sample = (String, Option<[f32; 3]>, Option<std::sync::Arc<wgpu::TextureView>>, Option<[f32; 3]>);
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let panel = Rect::new(24.0, 24.0, fw - 48.0, fh - 48.0);

    // 假封面：一個主色，往下漸暗，再撒一點灰（封面很少是純色）
    let fake = |rgb: [u8; 3]| -> Vec<u8> {
        let mut px = Vec::with_capacity(24 * 24 * 4);
        for y in 0..24u32 {
            for x in 0..24u32 {
                let k = 1.0 - y as f32 / 40.0;
                let c = if (x + y * 7) % 5 == 0 { [90, 90, 96] } else { rgb.map(|v| (v as f32 * k) as u8) };
                px.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        px
    };
    let mut samples: Vec<Sample> = vec![("預設（Spotify 綠）".into(), None, None, None)];
    match std::env::var("VITRO_ACCENT_ARTS").ok() {
        Some(list) => {
            for p in list.split(';').filter(|p| !p.is_empty()) {
                let name = std::path::Path::new(p).file_stem().map_or(p.to_string(), |s| s.to_string_lossy().into_owned());
                let Ok(b) = std::fs::read(p) else {
                    println!("讀不到 {p}");
                    continue;
                };
                let tex = crate::gfx::upload_encoded(gpu, &b, "accent-art").ok();
                let acc = crate::mini::accent_from_bytes(&b).flatten();
                samples.push((name, None, tex, acc.or(Some([-1.0; 3]))));
            }
        }
        None => {
            for (name, rgb) in [
                ("紅", [200, 40, 40]), ("橘", [230, 120, 30]), ("黃", [235, 200, 40]), ("黃綠", [120, 190, 50]),
                ("綠", [30, 160, 80]), ("青", [30, 160, 160]), ("藍", [40, 80, 200]), ("靛", [80, 60, 180]),
                ("紫", [140, 50, 170]), ("粉紅", [220, 70, 140]), ("灰棕（低飽和）", [120, 95, 80]), ("灰階", [128, 128, 128]),
            ] {
                let acc = crate::mini::accent_from_pixels(&fake(rgb));
                samples.push((name.into(), Some(rgb.map(|v| v as f32 / 255.0)), None, acc.or(Some([-1.0; 3]))));
            }
        }
    }

    let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("accents") });
    quads.begin(gpu);
    quads.push(Quad::new(0.0, 0.0, fw, fh).color(theme::modal(0.42)));
    {
        let mut pass = crate::gfx::pass(&mut enc, &target.view, "accents-dim", None);
        quads.flush(gpu, &mut pass, w, h);
    }
    let backdrop = glass.blur_scene(gpu, &mut enc, target, theme::GLASS_PANEL.sigma);
    let sharp = glass.sharp_scene(gpu, &mut enc, target);
    glass.begin();
    glass.push(Panel::new(panel.x, panel.y, panel.w, panel.h).radius(theme::radius::PANEL).style(&theme::GLASS_PANEL));
    text.begin();
    let mut p = Paint::new(gpu, quads, text);
    p.text.push("跟著封面的強調色", panel.x + theme::space::PANEL_X, panel.y + theme::space::PANEL_TITLE_Y,
                theme::ty::TITLE.size, theme::ty::TITLE.line, theme::ink(0.95), 600, None, None);

    let idle = State::default();
    let row_h = ((panel.h - 72.0) / samples.len() as f32).min(44.0);
    let x0 = panel.x + theme::space::PANEL_X;
    for (i, (name, rgb, tex, acc)) in samples.iter().enumerate() {
        let y = panel.y + 60.0 + i as f32 * row_h;
        let cy = y + row_h * 0.5;
        let sq = (row_h - 8.0).min(32.0);
        let sw = Quad::new(x0, cy - sq * 0.5, sq, sq).radius(4.0);
        match (tex, rgb) {
            (Some(t), _) => p.quads.push_tex(sw.textured(), t),
            (None, Some(c)) => p.quads.push(sw.color([c[0], c[1], c[2], 1.0])),
            _ => {}
        }
        // 灰階封面標成 [-1]：退回綠
        let grey = acc.is_some_and(|c| c[0] < 0.0);
        theme::set_accent(acc.filter(|_| !grey));
        let note = if grey { format!("{name} → 退回綠") } else { name.clone() };
        let lx = x0 + 44.0;
        let ty = cy - theme::ty::BODY.line * 0.5;
        p.text.push(&note, lx, ty, theme::ty::BODY.size, theme::ty::BODY.line, theme::ink(0.9), 400, Some(220.0), None);
        let c = theme::accent();
        p.quads.push(Quad::new(lx + 230.0, cy - 10.0, 20.0, 20.0).radius(10.0).color(c));
        let [l, ch, _] = theme::to_oklch([c[0], c[1], c[2]]);
        p.text.push(&format!("L {l:.2}  C {ch:.2}"), lx + 260.0, cy - theme::ty::CAPTION.line * 0.5, theme::ty::CAPTION.size,
                    theme::ty::CAPTION.line, theme::ink(0.55), 400, None, None);

        let mut x = lx + 370.0;
        wd::switch(&mut p, &wd::switch_rect(x + theme::ctl::SWITCH_W, cy), 1.0, idle, 0.0);
        x += theme::ctl::SWITCH_W + 24.0;
        wd::slider(&mut p, &wd::slider_rect(x, cy, 160.0), 0.6, None, idle, 0.0);
        x += 160.0 + 24.0;
        let bw = wd::button_width(p.text, "播放");
        wd::button(&mut p, &Rect::new(x, cy - theme::ctl::BTN_H * 0.5, bw, theme::ctl::BTN_H), "播放", Kind::Primary, idle);
        x += bw + 24.0;
        // preset 選單那種強調色底配深字的反白列
        let chip = Rect::new(x, cy - 13.0, 150.0, 26.0);
        p.quads.push(Quad::new(chip.x, chip.y, chip.w, chip.h).radius(theme::radius::CHIP).color(theme::accent()));
        p.text.push("★ 正在播放", chip.x + 10.0, cy - theme::ty::BODY.line * 0.5, theme::ty::BODY.size,
                    theme::ty::BODY.line, theme::on_accent(), 600, None, None);
        x += chip.w + 24.0;
        wd::focus_ring(p.quads, &Rect::new(x, cy - 12.0, 60.0, 24.0), theme::radius::CHIP);
    }
    theme::set_accent(None);

    text.prepare(gpu, w, h)?;
    {
        let mut pass = crate::gfx::pass(&mut enc, &target.view, "accents-glass", None);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    }
    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok(())
}

/// `VITRO_CHROME=0.4`：控制列的展開程度。神燈動畫只有靠它才看得到 ——
/// 1 = 完全展開，0 = 已經吸進中心。
fn fake_chrome() -> f32 {
    std::env::var("VITRO_CHROME").ok().and_then(|v| v.parse().ok()).unwrap_or(1.0f32).clamp(0.0, 1.0)
}

/// `VITRO_PLAYING=0`：假裝現在是暫停，用來看播放鍵的另一個狀態。
fn fake_playing() -> bool {
    std::env::var("VITRO_PLAYING").map(|v| v != "0").unwrap_or(true)
}

/// `VITRO_HOVER=play` 之類的，讓 `--shot` 也能看到滑鼠停在按鈕上的樣子。
/// 原生視窗截不了圖，這是唯一能驗 hover 有沒有畫出來的辦法。
fn fake_hover() -> Option<crate::ui::Hit> {
    use crate::ui::Hit;
    match std::env::var("VITRO_HOVER").ok()?.as_str() {
        "play" => Some(Hit::PlayPause),
        "prev" => Some(Hit::Prev),
        "next" => Some(Hit::Next),
        "settings" => Some(Hit::Settings),
        "fav" => Some(Hit::Favourite),
        "seek" => Some(Hit::Seek(0.3)),
        "cat" => Some(Hit::Category),
        "preset-prev" => Some(Hit::PresetPrev),
        "preset-next" => Some(Hit::PresetNext),
        "like" => Some(Hit::Like),
        "devices" => Some(Hit::Devices),
        "shuffle" => Some(Hit::Shuffle),
        "repeat" => Some(Hit::Repeat),
        "mute" => Some(Hit::Mute),
        "volume" => Some(Hit::Volume(0.5)),
        "back" => Some(Hit::DrillBack),
        "about-licenses" => Some(Hit::AboutLicenses),
        "about-disconnect" => Some(Hit::AboutDisconnect),
        _ => None,
    }
}

/// 假頻譜。`spectrum=1` 的 custom wave 讀的是這個而不是波形 ——
/// 不填的話那 57 個波形在截圖裡會是一條直線，看起來像壞掉。
fn fake_bins(snd: &mut crate::milk::Sound, t: f32) {
    for (i, b) in snd.bins.iter_mut().enumerate() {
        let p = i as f32 / crate::audio::BINS as f32;
        // 低頻重、高頻輕，再疊一點會動的紋路
        *b = (1.0 - p).powf(1.6) * (0.55 + 0.45 * (t * 7.0 + p * 30.0).sin()) * snd.bass;
    }
}

/// `--shot <檔> milk[:<第幾個>]`：跑幾十幀讓回授累積起來，再存圖。
fn milk_shot(
    gpu: &Gpu,
    target: &Offscreen,
    path: &Path,
    which: &str,
) -> anyhow::Result<()> {
    let all = crate::preset::load_any(Path::new("presets"))?;
    if all.is_empty() {
        anyhow::bail!("presets/ 是空的，先跑 --extract-presets");
    }
    // 可以給索引，也可以給名字的一部分
    let idx = which
        .parse::<usize>()
        .ok()
        .filter(|i| *i < all.len())
        .or_else(|| {
            if which.is_empty() {
                None
            } else {
                let w = which.to_lowercase();
                all.iter().position(|(n, _)| n.to_lowercase().contains(&w))
            }
        })
        .unwrap_or(0);
    let (name, raw) = all[idx].clone();
    println!("preset #{idx}「{name}」  著色器={}", if raw.has_shaders() { "有" } else { "無" });

    let assets = std::sync::Arc::new(crate::milkasset::MilkAssets::new(gpu));
    // `--shot` 這邊用環境變數開，方便兩條路徑對照
    let use_shaders = std::env::var_os("VITRO_PRESET_SHADERS").is_some();
    let mut m = crate::milk::Milk::new(
        gpu,
        assets,
        name,
        raw,
        target.width,
        target.height,
        use_shaders,
    )
        .map_err(|e| anyhow::anyhow!("preset 編不起來：{e}"))?;

    // 造一段有起伏的假聲音，不然方程式全是 0，畫面會是死的
    let mut snd = crate::milk::Sound::default();
    let mut wave = [0.0f32; crate::audio::WAVE];
    let started = std::time::Instant::now();
    const FRAMES: usize = 90;
    for f in 0..FRAMES {
        let t = f as f32 / 60.0;
        snd.bass = 0.55 + 0.45 * (t * 5.2).sin();
        snd.mid = 0.45 + 0.35 * (t * 8.1).sin();
        snd.treb = 0.35 + 0.30 * (t * 12.7).sin();
        let k = 0.06;
        snd.bass_att += (snd.bass - snd.bass_att) * k;
        snd.mid_att += (snd.mid - snd.mid_att) * k;
        snd.treb_att += (snd.treb - snd.treb_att) * k;
        fake_bins(&mut snd, t);
        for (i, s) in wave.iter_mut().enumerate() {
            let p = i as f32 / crate::audio::WAVE as f32;
            *s = (p * 40.0 + t * 9.0).sin() * 0.5 * snd.bass
                + (p * 130.0 + t * 21.0).sin() * 0.2 * snd.treb;
        }
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("milk") });
        m.render(gpu, &mut enc, target, &snd, &wave, 1.0 / 60.0);
        gpu.queue.submit([enc.finish()]);
        if let Some(e) = pollster::block_on(scope.pop())
            && f == 0 {
                println!("!! 算圖驗證錯誤：{e}");
            }
    }
    println!("{}", m.debug_dump());
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    println!("跑了 {FRAMES} 幀，共 {ms:.0} ms，平均 {:.2} ms/幀", ms / FRAMES as f64);

    target.save_png(gpu, path)?;
    println!("寫出 {}", path.display());
    Ok(())
}

/// `--sheet <檔> [起始] [數量]`：把好幾個 preset 各跑一段，拼成一張接觸印樣。
///
/// 一次看十幾個比一張張開有效率太多 —— 判斷「整體對不對」需要的是樣本，
/// 不是單張。
pub fn sheet(path: &Path, start: usize, count: usize) -> anyhow::Result<()> {
    let gpu = Gpu::headless()?;
    let all = crate::preset::load_any(Path::new("presets"))?;
    if all.is_empty() {
        anyhow::bail!("presets/ 是空的");
    }
    let assets = std::sync::Arc::new(crate::milkasset::MilkAssets::new(&gpu));
    let use_shaders = std::env::var_os("VITRO_PRESET_SHADERS").is_some();

    let (cw, ch) = (320u32, 180u32);
    let cols = 4u32;
    let rows = count.div_ceil(cols as usize) as u32;
    let (sw, sh) = (cw * cols, ch * rows);
    let mut canvas = vec![0u8; (sw * sh * 4) as usize];

    let cell = Offscreen::new(&gpu.device, cw, ch);
    let mut wave = [0.0f32; crate::audio::WAVE];

    for k in 0..count {
        let idx = (start + k) % all.len();
        let (name, raw) = all[idx].clone();
        let short: String = name.chars().take(34).collect();
        let Ok(mut m) =
            crate::milk::Milk::new(&gpu, assets.clone(), name, raw, cw, ch, use_shaders)
        else {
            continue;
        };
        let mut snd = crate::milk::Sound::default();
        for f in 0..70 {
            let t = f as f32 / 60.0;
            snd.bass = 0.55 + 0.45 * (t * 5.2).sin();
            snd.mid = 0.45 + 0.35 * (t * 8.1).sin();
            snd.treb = 0.35 + 0.30 * (t * 12.7).sin();
            let a = 0.06;
            snd.bass_att += (snd.bass - snd.bass_att) * a;
            snd.mid_att += (snd.mid - snd.mid_att) * a;
            snd.treb_att += (snd.treb - snd.treb_att) * a;
            fake_bins(&mut snd, t);
            for (i, s) in wave.iter_mut().enumerate() {
                let p = i as f32 / crate::audio::WAVE as f32;
                *s = (p * 40.0 + t * 9.0).sin() * 0.5 * snd.bass
                    + (p * 130.0 + t * 21.0).sin() * 0.2 * snd.treb;
            }
            let mut enc = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            m.render(&gpu, &mut enc, &cell, &snd, &wave, 1.0 / 60.0);
            gpu.queue.submit([enc.finish()]);
        }

        let px = cell.read_rgba(&gpu);
        let (gx, gy) = (k as u32 % cols, k as u32 / cols);
        for y in 0..ch {
            let dst = (((gy * ch + y) * sw + gx * cw) * 4) as usize;
            let src = (y * cw * 4) as usize;
            canvas[dst..dst + (cw * 4) as usize]
                .copy_from_slice(&px[src..src + (cw * 4) as usize]);
        }
        println!("  [{:>3}] {short}", idx);
    }

    let img = image::RgbaImage::from_raw(sw, sh, canvas)
        .ok_or_else(|| anyhow::anyhow!("尺寸對不上"))?;
    img.save(path)?;
    println!("寫出 {} ({sw}x{sh}，{count} 格)", path.display());
    Ok(())
}

/// 跟 `glass_ref.html` 一模一樣的畫面：模糊的封面當背景、上面一張清晰的
/// 封面、底部一條 12px 黑白條紋，再壓三段玻璃。
///
/// 條紋是重點 —— 平滑的漸層底下折射與模糊都是隱形的，
/// 高頻圖樣才照得出「玻璃到底有沒有把背後的東西折彎」。
fn glass_ref(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    path: &Path,
) -> anyhow::Result<()> {
    use crate::glass::Panel;
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let veil = crate::scene_flow::Veil::new(gpu);
    let albums = crate::scene_flow::fake_albums(gpu, 1);
    let art = match std::env::var("VITRO_ART").ok() {
        Some(p) => match std::fs::read(&p) {
            Ok(b) => crate::gfx::upload_encoded(gpu, &b, "ref-art").unwrap_or_else(|_| albums[0].art.clone()),
            Err(_) => albums[0].art.clone(),
        },
        None => albums[0].art.clone(),
    };

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("glassref") });
    quads.begin(gpu);

    // 背景：封面鋪滿 → 模糊 90
    let bg_src = Offscreen::new(&gpu.device, w, h);
    {
        quads.push_tex(Quad::new(0.0, 0.0, fw, fh).cover(1.0, 1.0), &art);
        let mut pass = crate::scene_flow::clear_pass(&mut enc, &bg_src.view, wgpu::Color::BLACK, "bg");
        quads.flush(gpu, &mut pass, w, h);
    }
    let bg = std::sync::Arc::new(glass.blur_scene(gpu, &mut enc, &bg_src, theme::SIGMA_BACKDROP));

    // 預設拿平的 50% 灰當背景 —— 兩邊的底色完全一樣，玻璃本身的數學才比得出來。
    // `VITRO_REF_BG=art` 換成真的「模糊封面 + 兩層紗」。
    if std::env::var("VITRO_REF_BG").as_deref() == Ok("art") {
        quads.push_tex(
            Quad::new(0.0, 0.0, fw, fh).textured().saturate(1.7).color([0.47, 0.47, 0.47, 1.0]),
            &bg,
        );
        quads.push_tex(Quad::new(0.0, 0.0, fw, fh).textured(), veil.radial());
        quads.push_tex(Quad::new(0.0, 0.0, fw, fh).textured(), veil.vert());
    } else {
        quads.push(Quad::new(0.0, 0.0, fw, fh).color([0.502, 0.502, 0.502, 1.0]));
    }
    // 清晰的封面：300x300，跟 HTML 那邊同一個位置
    quads.push_tex(Quad::new((fw - 300.0) * 0.5, 150.0, 300.0, 300.0).cover(1.0, 1.0).radius(6.0), &art);
    // 條紋帶：12px 白、12px 黑，整條 opacity .9（跟 HTML 那邊同一個算法）
    let mut x = 0.0;
    while x < fw {
        quads.push(Quad::new(x, fh - 120.0, 12.0, 120.0).color(theme::ink(0.9)));
        quads.push(Quad::new(x + 12.0, fh - 120.0, 12.0, 120.0).color(theme::scrim(0.9)));
        x += 24.0;
    }
    {
        let mut pass = crate::scene_flow::load_pass(&mut enc, &target.view, "main", true);
        quads.flush(gpu, &mut pass, w, h);
    }

    let backdrop = glass.blur_scene(gpu, &mut enc, target, theme::GLASS_BAR.sigma);
    let sharp = glass.sharp_scene(gpu, &mut enc, target);
    // `VITRO_DUMP_BLUR=1`：直接把模糊圖鋪滿畫面，用來分辨「模糊不夠」
    // 是出在模糊那幾趟、還是出在玻璃取樣。
    if std::env::var_os("VITRO_DUMP_BLUR").is_some() {
        let bd = std::sync::Arc::new(backdrop);
        // =raw 時照小圖原尺寸畫在左上角，看得到真正的 texel
        if std::env::var("VITRO_DUMP_BLUR").as_deref() == Ok("raw") {
            quads.push_tex(Quad::new(0.0, 0.0, 160.0, 90.0).textured(), &bd);
        } else {
            quads.push_tex(Quad::new(0.0, 0.0, fw, fh).textured(), &bd);
        }
        {
            let mut pass = crate::scene_flow::load_pass(&mut enc, &target.view, "dump", true);
            quads.flush(gpu, &mut pass, w, h);
        }
        quads.upload(gpu, w, h);
        gpu.queue.submit([enc.finish()]);
        target.save_png(gpu, path)?;
        println!("寫出 {} 場景=模糊圖", path.display());
        return Ok(());
    }
    glass.begin();
    // CSS 的數字：padding 0 14px 14px、height 64、gap 10、
    // #bar-now = clamp(220, 24vw, 340)、#bar-extra = 124
    let y = fh - 14.0 - 64.0;
    let now_w = (fw * 0.24).clamp(220.0, 340.0);
    let extra_w = 124.0;
    let main_x = 14.0 + now_w + 10.0;
    for (x, w) in [
        (14.0, now_w),
        (main_x, fw - 14.0 - extra_w - 10.0 - main_x),
        (fw - 14.0 - extra_w, extra_w),
    ] {
        glass.push(
            // 照 glass_ref.html 的 CSS（圓角 20），不跟著 `radius::BAR` 改成膠囊，才比得起來
            Panel::new(x, y, w, 64.0)
                .radius(20.0)
                .style(&theme::GLASS_BAR),
        );
    }
    {
        let mut pass = crate::scene_flow::load_pass(&mut enc, &target.view, "glass", false);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
    }
    // 實際的頂點資料在這裡才寫進去 —— 漏了這行，所有 quad 都不會出現。
    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    target.save_png(gpu, path)?;
    println!("寫出 {} 場景=玻璃比對", path.display());
    Ok(())
}

/// 調玻璃材質用的試驗場：大小不同的玻璃壓在條紋、文字、清晰封面、
/// 模糊封面、淺色純色上。看折射（條紋與文字被彎）、高光、投影隨尺寸與
/// 背景深淺的變化。只用 1280x720 看，版面寫死。
/// 玻璃上置中寫一行字：(字, x, y, 寬, 高)
type Label<'a> = &'a dyn Fn(&mut Text, &str, f32, f32, f32, f32);

/// `motionlab` 的玻璃：互動、出現、融合、選單長出來，各擺幾個階段
fn motion_items(glass: &mut Glass, text: &mut Text, label: Label) {
    use crate::glass::Touch;
    let pill = |x: f32, y: f32, ww: f32| Panel::new(x, y, ww, 48.0).radius(24.0).style(&theme::GLASS_BAR);
    // ---- 第一排：互動（壓在大字上）----
    // 鄰近的那對放最前面：一批玻璃只有最亮的那團光會映到別塊上，同樣亮的取先推的
    let near = Touch { hover: 1.0, press: 1.0, at: (785.0, 194.0) };
    glass.push(pill(700.0, 170.0, 90.0).touch(&near, false));
    label(text, "按著", 700.0, 170.0, 90.0, 48.0);
    glass.push(pill(800.0, 170.0, 70.0));
    label(text, "鄰近", 800.0, 170.0, 70.0, 48.0);
    glass.push(pill(40.0, 170.0, 140.0));
    label(text, "平常", 40.0, 170.0, 140.0, 48.0);
    glass.push(pill(200.0, 170.0, 140.0).touch(&Touch { hover: 1.0, press: 0.0, at: (235.0, 186.0) }, true));
    label(text, "指著", 200.0, 170.0, 140.0, 48.0);
    glass.push(pill(360.0, 170.0, 140.0).touch(&Touch { hover: 1.0, press: 1.0, at: (430.0, 194.0) }, true));
    label(text, "按下", 360.0, 170.0, 140.0, 48.0);
    glass.push(pill(520.0, 170.0, 140.0).touch(&Touch { hover: 1.0, press: 1.0, at: (655.0, 180.0) }, true));
    label(text, "按住往右拉", 520.0, 170.0, 140.0, 48.0);
    glass.push(pill(200.0, 250.0, 140.0).touch(&Touch { hover: 1.0, press: -0.35, at: (270.0, 274.0) }, true));
    label(text, "彈回來", 200.0, 250.0, 140.0, 48.0);
    // ---- 第二排：出現的途中（壓在清晰的封面上）----
    for (i, a) in [0.15f32, 0.35, 0.6, 0.85, 1.0].into_iter().enumerate() {
        let x = 40.0 + i as f32 * 150.0;
        glass.push(pill(x, 426.0, 130.0).materialize(a));
        label(text, &format!("{:.0}%", a * 100.0), x, 426.0, 130.0, 48.0);
    }
    // ---- 第三排：融合（距離 36、18、6、0）----
    for (i, gap) in [36.0f32, 18.0, 6.0, -10.0].into_iter().enumerate() {
        let x = 40.0 + i as f32 * 170.0;
        let g = i as u32 + 1;
        for k in 0..2 {
            let cx = x + k as f32 * (56.0 + gap);
            glass.push(Panel::new(cx, 580.0, 56.0, 56.0).radius(28.0).style(&theme::GLASS_BAR).merge(g, theme::MERGE_K));
        }
    }
    // ---- 選單從按鈕長出來（淺色區，三個階段左右排開）----
    for (i, p) in [0.15f32, 0.3, 0.5].into_iter().enumerate() {
        let x = 900.0 + i as f32 * 125.0;
        let b = crate::ui::Rect::new(x + 40.0, 660.0, 36.0, 36.0);
        let fin = crate::ui::Rect::new(x, 330.0, 115.0, 200.0);
        crate::scene_devices::push_morph(glass, Some(b), fin, theme::radius::PANEL, &theme::GLASS_PANEL, p, 10 + i as u32);
    }
}

fn glass_lab(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    text: &mut Text,
    glass: &mut Glass,
    path: &Path,
    motion: bool,
) -> anyhow::Result<()> {
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let veil = crate::scene_flow::Veil::new(gpu);
    let albums = crate::scene_flow::fake_albums(gpu, 12);
    let art = &albums[0].art;

    // ---- 第一趟：背景 ----
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    quads.begin(gpu);
    let bg_src = Offscreen::new(&gpu.device, w, h);
    {
        quads.push_tex(Quad::new(0.0, 0.0, fw, fh).cover(1.0, 1.0), art);
        let mut pass = crate::scene_flow::clear_pass(&mut enc, &bg_src.view, wgpu::Color::BLACK, "bg");
        quads.flush(gpu, &mut pass, w, h);
    }
    let bg = std::sync::Arc::new(glass.blur_scene(gpu, &mut enc, &bg_src, theme::SIGMA_BACKDROP));
    // 跟封面流一樣的底：模糊封面＋兩層紗
    quads.push_tex(Quad::new(0.0, 0.0, fw, fh).textured().saturate(1.7).color([0.47, 0.47, 0.47, 1.0]), &bg);
    quads.push_tex(Quad::new(0.0, 0.0, fw, fh).textured(), veil.radial());
    quads.push_tex(Quad::new(0.0, 0.0, fw, fh).textured(), veil.vert());
    // 右邊一塊淺色純色
    let light_x = 880.0;
    quads.push(Quad::new(light_x, 0.0, fw - light_x, fh).color([0.93, 0.93, 0.94, 1.0]));
    // 上面一條黑白條紋（12px）
    let mut x = 0.0;
    while x < fw {
        quads.push(Quad::new(x, 40.0, 12.0, 90.0).color(theme::ink(0.9)));
        quads.push(Quad::new(x + 12.0, 40.0, 12.0, 90.0).color(theme::scrim(0.9)));
        x += 24.0;
    }
    // 清晰的封面幾張
    for (i, al) in albums.iter().take(4).enumerate() {
        quads.push_tex(Quad::new(40.0 + i as f32 * 150.0, 380.0, 140.0, 140.0).cover(1.0, 1.0).radius(6.0), &al.art);
    }
    {
        let mut pass = crate::scene_flow::load_pass(&mut enc, &target.view, "lab bg", true);
        quads.flush(gpu, &mut pass, w, h);
    }
    text.begin();
    for (i, s) in ["那只是昨夜的一場夢境", "我還是我 你還是你", "Kind of Blue — Miles Davis"].iter().enumerate() {
        text.push(s, 40.0, 160.0 + i as f32 * 60.0, 40.0, 52.0, theme::ink(0.92), 700, None, None);
    }
    for (i, s) in ["淺色背景上的深色字", "Light backdrop", "玻璃壓在這裡"].iter().enumerate() {
        text.push(s, light_x + 30.0, 170.0 + i as f32 * 56.0, 30.0, 40.0, [0.1, 0.1, 0.12, 1.0], 600, None, None);
    }
    text.prepare(gpu, w, h)?;
    {
        let mut pass = crate::scene_flow::load_pass(&mut enc, &target.view, "lab text", false);
        text.render(&mut pass)?;
    }
    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    text.end_frame();

    // ---- 第二趟：玻璃與玻璃上的字 ----
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    let backdrop = glass.blur_scene(gpu, &mut enc, target, theme::GLASS_BAR.sigma);
    let sharp = glass.sharp_scene(gpu, &mut enc, target);
    glass.begin();
    text.begin();
    let label = |text: &mut Text, s: &str, x: f32, y: f32, ww: f32, hh: f32| {
        let (tw, _) = text.measure(s, 14.0, 20.0, None);
        text.push(s, x + (ww - tw) * 0.5, y + hh * 0.5 - 10.0, 14.0, 20.0, theme::ink(0.95), 500, None, None);
    };
    if motion {
        motion_items(glass, text, &label);
        text.prepare(gpu, w, h)?;
        {
            let mut pass = crate::scene_flow::load_pass(&mut enc, &target.view, "lab glass", false);
            glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
            text.render(&mut pass)?;
        }
        gpu.queue.submit([enc.finish()]);
        target.save_png(gpu, path)?;
        println!("寫出 {} 場景=互動試驗場", path.display());
        return Ok(());
    }
    // (x, y, w, h, 圓角, 預設, 字, 認元件的鍵)。鍵是空的 = 大面板，不翻明暗
    type Item = (f32, f32, f32, f32, f32, &'static theme::GlassStyle, &'static str, &'static str);
    let items: [Item; 11] = [
        (330.0, 70.0, 380.0, 230.0, theme::radius::PANEL, &theme::GLASS_PANEL, "面板 380x230", ""),
        (60.0, 62.0, 44.0, 44.0, 22.0, &theme::GLASS_BAR, "", "a"),
        (130.0, 62.0, 120.0, 44.0, 22.0, &theme::GLASS_SEARCH, "膠囊", "b"),
        (14.0, 642.0, 300.0, 64.0, theme::radius::BAR, &theme::GLASS_BAR, "控制列", "c"),
        (324.0, 642.0, 520.0, 64.0, theme::radius::BAR, &theme::GLASS_BAR, "控制列", "d"),
        (420.0, 470.0, 160.0, 40.0, theme::radius::CARD, &theme::GLASS_TOAST, "通知", "e"),
        (840.0, 300.0, 120.0, 120.0, 24.0, &theme::GLASS_BAR, "跨界", "f"),
        (920.0, 460.0, 300.0, 64.0, theme::radius::BAR, &theme::GLASS_BAR, "淺色上的控制列", "g"),
        (1000.0, 70.0, 44.0, 44.0, 22.0, &theme::GLASS_BAR, "", "h"),
        // 染色玻璃：同一個顏色在暗、亮背景上
        (620.0, 560.0, 140.0, 44.0, 22.0, &theme::GLASS_BAR, "染色", "i"),
        (1060.0, 560.0, 140.0, 44.0, 22.0, &theme::GLASS_BAR, "染色", "j"),
    ];
    for (x, y, ww, hh, r, st, s, key) in items {
        let tone = if key.is_empty() { 0.0 } else { glass.tone(key, [x, y, ww, hh]) };
        let mut p = Panel::new(x, y, ww, hh).radius(r).style(st).tone(tone);
        let tinted = s == "染色";
        if tinted {
            p = p.tinted(theme::accent());
        }
        glass.push(p);
        let _tone = theme::scoped_tone(tone);
        if tinted {
            let (tw, _) = text.measure(s, 14.0, 20.0, None);
            text.push(s, x + (ww - tw) * 0.5, y + hh * 0.5 - 10.0, 14.0, 20.0, theme::on_accent(), 600, None, None);
        } else if !s.is_empty() {
            label(text, s, x, y, ww, hh);
        }
    }
    text.prepare(gpu, w, h)?;
    {
        let mut pass = crate::scene_flow::load_pass(&mut enc, &target.view, "lab glass", false);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        text.render(&mut pass)?;
    }
    gpu.queue.submit([enc.finish()]);
    target.save_png(gpu, path)?;
    println!("寫出 {} 場景=玻璃試驗場", path.display());
    Ok(())
}
