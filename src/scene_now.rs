//! 歌詞畫面 —— 對應 web 版的 `#now` 視圖。
//!
//! 逐句捲動，目前這句放大並提高亮度，前後幾句淡出。
//! 有 `words`（LyricsPlus 那條來源）而且設定開著逐字的話，正在唱的那行
//! 會有掃光。

use crate::gfx::{self, Gpu, Offscreen};
use crate::glass::Glass;
use crate::lyrics::Lyrics;
use crate::quad::{Quad, QuadRenderer};
use crate::text::Text;
use crate::theme;
use crate::ui::{self, Hit};
use std::sync::Arc;

pub struct Now<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub album: &'a str,
    pub art: Arc<wgpu::TextureView>,
    pub position_ms: i64,
    /// 拿去比對歌詞的位置 = 播放位置 ＋ 使用者設的補償。
    /// 跟 `position_ms` 分開是故意的：進度條要顯示真的位置。
    pub lyric_ms: i64,
    pub duration_ms: i64,
    pub lyrics: Option<&'a Lyrics>,
    pub playing: bool,
    /// 隨機播放、重複播放開著的話，那顆鍵是綠的、底下多一顆小圓點（CSS 的 `.barbtn.on`）
    pub shuffle: bool,
    pub repeat: crate::smtc::Repeat,
    /// 逐字歌詞。有 `words` 才有效，沒有的話照樣整行畫。
    pub ktv: bool,
    /// 每句下面畫翻譯（有 `trans` 的才畫）
    pub show_trans: bool,
    /// 第二排的愛心與音量
    pub tools: ui::Tools,
    pub vol_drag: bool,
    /// 唱片模式。`None` 就是原本的方形封面
    pub vinyl: Option<&'a crate::vinyl::Vinyl>,
    /// 背景換歌時的交叉淡化：上一張與它還剩多少（1 → 0）
    pub bg_prev: Option<(&'a Arc<wgpu::TextureView>, f32)>,
    /// 歌詞捲到哪（句的索引，小數＝捲動中）。由呼叫端補間
    pub lyric_scroll: f32,
    pub pick: LyricPick,
    /// 曲名、歌手、專輯放不下時來回捲動的時鐘（秒，換歌歸零）。`None` 停在開頭（截圖）
    pub roll: Option<f32>,
    pub fx: NowFx,
}

/// 進出歌詞頁的轉場（Python 版 style.css 的 `#now.v-in` 與 `#now.v-out.shared`）。
#[derive(Clone, Copy, Debug, Default)]
pub struct NowFx {
    /// 轉場開始後幾秒
    pub t: f32,
    /// 正在進場
    pub enter: bool,
    /// 正在跟封面流共用元素離場（只有這種離場要逐項淡出；其他的整頁一起淡）
    pub leave: bool,
    /// 封面交給替身在飛，這裡不畫
    pub hide_art: bool,
}

/// 滾輪自己捲、點一句跳過去要的狀態。全部由呼叫端補間好再交進來
#[derive(Clone, Copy, Debug, Default)]
pub struct LyricPick {
    /// 亮度與放大跟著它走（補間過的「正在唱第幾句」）。平常跟 `lyric_scroll` 一樣；
    /// 用滾輪捲走的時候畫面停在別處，亮的仍然是正在唱的那句
    pub focus: f32,
    /// 自己捲的程度 0..1：遠處的句子跟著亮起來，看得清楚才知道要點哪句
    pub browse: f32,
    /// 滑鼠停著的那句與底色淡入到哪（0..1）；剛離開的那句在淡出
    pub hover: Option<(usize, f32)>,
    pub unhover: Option<(usize, f32)>,
    /// 剛點的那句、點下去後幾秒
    pub flash: Option<(usize, f32)>,
    /// 沒有時間軸的純文字歌詞捲了幾 px
    pub plain_y: f32,
}

/// 文字 PV 模式（實驗中）要的東西。有給就是 PV 模式：歌詞換成全螢幕演出，
/// 左欄的封面、曲目資訊與按鍵跟著 `chrome`（滑鼠一陣子沒動就淡出）
pub struct PvIn<'a> {
    pub pv: &'a mut crate::pv::Pv,
    /// 演的是哪份歌詞、唱到哪（已加補償）。平常就是這首的；剛切歌、要退回一般歌詞頁的那一段，
    /// 是上一首留下來的（`Now::lyrics` 已經換成「載入中」了）。
    /// v2 演著的時候切歌、新歌的歌詞還沒到，給的是空的（只演片頭曲名卡）
    pub lyrics: &'a Lyrics,
    pub ms: i64,
    /// 一般歌詞頁換成 PV 的進度（0..1，補間過）。0 與 1 之間兩邊都畫：PV 從右邊漫過來、
    /// 一般的歌詞淡掉；左欄從一般的樣子換成跟著 `chrome`
    pub mix: f32,
    pub track: &'a str,
    pub pulse: crate::pv::beat::Pulse,
    pub now: std::time::Instant,
    pub chrome: f32,
    /// 鎖定的演出風格，`None` 照段落自動換
    pub look: Option<crate::pv::style::Look>,
    /// 設定裡的「文字PV_v2」
    pub v2: bool,
    /// v2 的片頭、片尾曲名卡、間奏光帶要的：歌名、歌手、整首多長（毫秒）、頻譜
    pub title: &'a str,
    pub artist: &'a str,
    pub duration: i64,
    pub spec: [f32; crate::pv::idle::SPEC],
    /// 設定的「背景幾何圖形」與它跟的五個頻段（`audio::Spectrum::bands`，[`crate::pv::geo`]）
    pub geo: bool,
    pub bands: [f32; 5],
    /// 這一幀演出層畫完先存起來：v2 下一幀要換一份歌詞了（[`crate::pv::Pv::freeze`]）
    pub freeze: bool,
}

/// PV 模式左欄後面那片磨砂：模糊多重（跟其他模糊錯開，快取才不會互蓋）、右緣淡出多寬
const FROST_SIGMA: f32 = 24.0;
const FROST_FADE: f32 = 260.0;

/// 這一幀每句歌詞畫在哪，下一次點擊拿來比對。純文字歌詞順便回報最多能捲多少
#[derive(Clone, Debug, Default)]
pub struct LyricHits {
    pub lines: Vec<(usize, ui::Rect)>,
    pub plain_max: f32,
}

impl LyricHits {
    pub fn at(&self, x: f32, y: f32) -> Option<usize> {
        self.lines.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| *i)
    }
}

/// 滑鼠停留的底色淡入淡出、點下去那一閃的長度
pub const HOVER_S: f32 = 0.18;
pub const FLASH_S: f32 = 0.7;

/// CSS 的 `--view-anim`
const VIEW_S: f32 = theme::dur::VIEW;


impl NowFx {
    /// rise-in：從下面 30px 浮上來並淡入。回傳 (透明度, 位移)
    fn rise(&self, delay: f32, dur: f32) -> (f32, f32) {
        use crate::anim::{phase, OUT, EASE};
        if self.enter {
            let p = OUT.at(phase(self.t, delay, dur));
            (p, 30.0 * (1.0 - p))
        } else if self.leave {
            (1.0 - EASE.at(phase(self.t, 0.0, VIEW_S * 0.45)), 0.0)
        } else {
            (1.0, 0.0)
        }
    }

    /// 左欄的文字、進度、按鈕
    fn left(&self) -> (f32, f32) {
        self.rise(0.09, VIEW_S * 0.85)
    }

    /// 右欄的歌詞，晚一拍
    fn lyrics(&self) -> (f32, f32) {
        self.rise(0.15, VIEW_S * 0.9)
    }

    fn vinyl(&self) -> crate::vinyl::Fx {
        use crate::anim::{phase, OUT, EASE};
        let mut fx = crate::vinyl::Fx { label: !self.hide_art, ..Default::default() };
        if self.enter {
            let g = OUT.at(phase(self.t, 0.12, VIEW_S * 1.1));
            fx.disc_scale = 0.62 + 0.38 * g;
            fx.disc_alpha = g;
            fx.arm_alpha = EASE.at(phase(self.t, 0.2, VIEW_S * 0.8));
        } else if self.leave {
            let a = 1.0 - EASE.at(phase(self.t, 0.0, VIEW_S * 0.45));
            fx.disc_alpha = a;
            fx.arm_alpha = a;
        }
        fx
    }
}

/// 這一行唱到哪了：回傳「唱到第幾個位元組」，正在唱的那個字照時間給小數。
/// 整行都唱完了回 `None`（照一般的整行畫就好）。
///
/// 逐字不是把每個字分開畫再各自變色 —— 那樣字距會跑掉，中文尤其明顯。
/// 做法是整行畫兩次：暗的一份打底、亮的一份用裁切框切出已經唱過的部分；
/// 每個排版行各切一次，所以折行的句子也做得了（`Text::lit_runs`）。
fn ktv_progress(line: &str, words: &[crate::lyrics::Word], pos_ms: i64) -> Option<f32> {
    let end = words.iter().map(|w| w.t + w.d.max(1)).max()?;
    if pos_ms >= end {
        return None;
    }
    // `text` 是 trim 過的，字串接起來可能多了開頭的空白
    let joined: String = words.iter().map(|w| w.text.as_str()).collect();
    let lead = (joined.len() - joined.trim_start().len()) as f32;
    let mut at = 0.0f32;
    for w in words {
        let n = w.text.len() as f32;
        if pos_ms >= w.t + w.d.max(1) {
            at += n;
            continue;
        }
        if pos_ms >= w.t {
            at += n * ((pos_ms - w.t) as f32 / w.d.max(1) as f32).clamp(0.0, 1.0);
        }
        break;
    }
    Some((at - lead).clamp(0.0, line.len() as f32))
}

/// 目前落在哪一句。回傳在 `synced` 裡的索引。
pub fn current_line(ly: &Lyrics, pos_ms: i64) -> Option<usize> {
    if ly.synced.is_empty() {
        return None;
    }
    // 二分找最後一個 t <= pos
    let mut lo = 0usize;
    let mut hi = ly.synced.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if ly.synced[mid].t <= pos_ms {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo == 0 { None } else { Some(lo - 1) }
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    now: &Now<'_>,
    bar_alpha: f32,
    hover: Option<Hit>,
) -> anyhow::Result<LyricHits> {
    render_with(gpu, target, quads, glass, text, now, bar_alpha, hover, None)
}

/// 同 [`render`]，`pv` 有給就是文字 PV 模式
#[allow(clippy::too_many_arguments)]
/// 封面（唱片）這一幀畫在哪。PV 模式控制項收起來時，封面不跟著淡掉，縮成左上角一小格繼續轉；
/// `pv_chrome` 是 PV 模式下左欄的 chrome（已乘過 mix），不是 PV 就給 `None`。
/// 封面流 ⇄ 歌詞頁的替身也照這個飛，落點才接得上
pub fn art_box(l: &ui::NowL, fh: f32, pv_chrome: Option<f32>) -> ui::Rect {
    let a = l.art;
    let Some(c) = pv_chrome else { return a };
    let g = c * c * (3.0 - 2.0 * c);
    let small = (fh * 0.15).clamp(84.0, 150.0);
    let m = 26.0;
    let s = small + (a.w - small) * g;
    ui::Rect::new(m + (a.x - m) * g, m + (a.y - m) * g, s, s)
}

/// PV 模式下左欄的 chrome：換過來的途中，左欄從一般歌詞頁的樣子（一直都在）慢慢變成跟著 chrome
pub fn pv_chrome(mix: f32, chrome: f32) -> f32 {
    1.0 - mix.min(1.0) * (1.0 - chrome)
}

pub fn render_with(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    now: &Now<'_>,
    bar_alpha: f32,
    hover: Option<Hit>,
    mut pv: Option<PvIn<'_>>,
) -> anyhow::Result<LyricHits> {
    let mut hits = LyricHits::default();
    // PV 只演有時間軸的歌詞；沒有的照原本的右欄說明（v2 換歌等歌詞的那段例外）。完全退回去了也不畫
    if pv.as_ref().is_some_and(|p| (p.lyrics.synced.is_empty() && !p.v2) || p.mix <= 0.0) {
        pv = None;
    }
    let pv_on = pv.is_some();
    // 一般歌詞頁還看得到多少（換成 PV 的途中兩邊都畫）
    let mix = pv.as_ref().map_or(0.0, |p| p.mix.min(1.0));
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let l = ui::now(fw, fh);

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("now") });

    quads.begin(gpu);

    // ---- 背景：封面鋪滿 → 模糊 ----
    let bg_src = glass.backdrop_src(gpu, 1, w, h);
    {
        // 換歌時上一張墊底，新的那張淡進來
        let k = match now.bg_prev {
            Some((prev, w)) => {
                quads.push_tex(Quad::new(0.0, 0.0, fw, fh).cover(1.0, 1.0), prev);
                1.0 - w
            }
            None => 1.0,
        };
        quads.push_tex(Quad::new(0.0, 0.0, fw, fh).cover(1.0, 1.0).color(theme::tex(k)), &now.art);
        let mut pass = gfx::pass(&mut enc, &bg_src.view, "now-bg", Some(crate::scene_flow::empty_bg()));
        quads.flush(gpu, &mut pass, w, h);
    }
    let bg = Arc::new(glass.blur_scene(gpu, &mut enc, &bg_src, theme::SIGMA_BACKDROP));

    // 深色壓暗再蓋一層黑；淺色補飽和度再蓋一層白（`theme::Backdrop`）
    let bd = theme::backdrop();
    let m = bd.now_mul;
    let bgq = Quad::new(0.0, 0.0, fw, fh).textured().color([m, m, m, 1.0]);
    quads.push_tex(if bd.now_sat != 1.0 { bgq.saturate(bd.now_sat) } else { bgq }, &bg);
    quads.push(Quad::new(0.0, 0.0, fw, fh).color(bd.now_wash));

    // ---- 文字 PV：先畫好背景再疊演出層（壓暗也在演出層裡），左欄跟著 chrome 淡入淡出 ----
    let ly_a0 = now.fx.lyrics().0;
    let mut chrome = 1.0;
    if let Some(p) = pv.as_mut() {
        {
            let mut pass = gfx::pass(&mut enc, &target.view, "now-bg-pv", Some(wgpu::Color::BLACK));
            quads.flush(gpu, &mut pass, w, h);
        }
        chrome = pv_chrome(mix, p.chrome);
        p.pv.render(gpu, &mut enc, target, text, &crate::pv::Input {
            lyrics: p.lyrics,
            track: p.track,
            ms: p.ms,
            pulse: p.pulse,
            now: p.now,
            accent: theme::accent(),
            alpha: ly_a0,
            inset: l.lyrics_x * chrome,
            look: p.look,
            mix,
            v2: p.v2,
            title: p.title,
            artist: p.artist,
            duration: p.duration,
            trans: now.show_trans,
            spec: p.spec,
            geo: p.geo,
            bands: p.bands,
        });
        // v2 換一份歌詞：換之前那一幀存起來，換之後疊在演出上面淡掉（左欄與磨砂不在裡面）
        if p.freeze {
            p.pv.freeze(gpu, &mut enc, target, p.now);
        } else if let Some((view, a, s)) = p.pv.ghost((w, h), p.now) {
            let (gw, gh) = (fw * s, fh * s);
            quads.push_tex(Quad::new((fw - gw) * 0.5, (fh - gh) * 0.5, gw, gh).textured().color([1.0, 1.0, 1.0, a * mix]), &view);
        }
        // 控制項出來的時候，左欄後面墊一片淺灰磨砂：把演出那一格模糊掉再蒙一層灰，
        // 右緣很寬地淡進演出裡，看不出一條邊。演出換成米白底的風格時左欄的字也還讀得到
        let frost_a = chrome * mix;
        if frost_a > 0.0 {
            let frost = Arc::new(glass.blur_scene(gpu, &mut enc, target, FROST_SIGMA));
            let pw = (l.lyrics_x + FROST_FADE * 0.35).min(fw);
            let fade = FROST_FADE.min(pw);
            // 模糊那層先壓暗、灰那層蓋得厚：底下是黑底還是米白底，磨砂都落在差不多的灰，白字一直讀得到
            let mut q = Quad::new(0.0, 0.0, pw, fh).fade_right(fade).saturate(0.6).color([0.55, 0.55, 0.57, frost_a]);
            q.uv = [0.0, 0.0, pw / fw, 1.0];
            quads.push_tex(q, &frost);
            quads.push(Quad::new(0.0, 0.0, pw, fh).fade_right(fade).color([0.47, 0.48, 0.51, 0.6 * frost_a]));
        }
    }

    let pv_debug = pv.as_ref().filter(|_| pv_debug()).map(|p| p.pulse);

    // ---- 左側：封面與曲目資訊 ----
    let (ax, art_px) = (l.art.x, l.art.w);
    let b = art_box(&l, fh, pv_on.then_some(chrome));
    let (bx, by, bs) = (b.x, b.y, b.w);
    match now.vinyl {
        Some(v) => crate::vinyl::draw(quads, v, bx, by, bs, &now.art, now.fx.vinyl()),
        None if now.fx.hide_art => {}
        None => quads.push_tex(
            Quad::new(bx, by, bs, bs)
                .radius(theme::radius::CARD * (bs / art_px))
                .cover(1.0, 1.0)
                .shadow_of(&theme::SHADOW_ART, 1.0),
            &now.art,
        ),
    }

    let (left_a, left_dy) = now.fx.left();
    let left_a = left_a * chrome;
    let (ly_a, ly_dy) = now.fx.lyrics();
    // 換成 PV 的途中，一般的歌詞比 PV 漫過來早一點淡掉、往上飄一點（淡到一點點就不給點了）
    let (ly_a, ly_dy) = if pv_on { (ly_a * (1.0 - mix * 1.4).max(0.0), ly_dy - 24.0 * mix) } else { (ly_a, ly_dy) };
    quads.set_fx(left_a, left_dy);

    // 進度條
    let pw = l.progress.w;
    let py = l.progress.y;
    let frac = if now.duration_ms > 0 {
        (now.position_ms as f32 / now.duration_ms as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // CSS 的 `#np-progress`：3px、底 .13、填滿 --fg
    quads.push(Quad::new(ax, py, pw, 3.0).radius(1.5).color(theme::ink(0.13)));
    if frac > 0.0 {
        quads.push(Quad::new(ax, py, (pw * frac).max(3.0), 3.0).radius(1.5).color(theme::fg()));
    }
    // 停在上面或拖著的時候給一顆 11px 的圓鈕（Python 版的 #np-knob）
    if matches!(hover, Some(Hit::Seek(_))) {
        let kx = ax + pw * frac;
        quads.push(
            Quad::new(kx - 5.5, py + 1.5 - 5.5, 11.0, 11.0)
                .radius(5.5)
                .color(theme::ink(1.0))
                .shadow_of(&theme::SHADOW_KNOB, 1.0),
        );
    }

    quads.set_fx(1.0, 0.0);

    // ---- 文字 ----
    text.begin();
    text.set_fx(left_a, left_dy);
    // 曲名只給一行 —— 兩行會把下面整欄往下推，撞到進度條。放不下的在封面寬度裡來回捲動
    // 字級與顏色照 CSS：曲名 30px／700／--fg，演出者 17px #c2c3cc，專輯 13.5px --dim
    let roll = |text: &mut Text, s: &str, y: f32, (size, line, weight): (f32, f32, u16), color| {
        crate::widgets::roll_line(text, now.roll, 1.0, s, (ax, y, art_px), (size, line, weight), color, None, None);
    };
    roll(text, now.title, l.title_y, (30.0, 37.5, 700), theme::fg());
    roll(text, now.artist, l.artist_y, (17.0, 23.0, 400), theme::soft());
    // 專輯名。跟曲名重複的話就不畫 —— 單曲很常兩者一樣，寫兩次很蠢。
    if !now.album.is_empty() && now.album != now.title {
        roll(text, now.album, l.album_y, (13.5, 18.0, 400), theme::dim());
    }
    text.push(&fmt_ms(now.position_ms), ax, l.times_y, 11.5, 15.0, theme::dim(), 400, None, None);
    let (dw, _) = text.measure(&fmt_ms(now.duration_ms), 11.5, 15.0, None);
    text.push(&fmt_ms(now.duration_ms), ax + pw - dw, l.times_y, 11.5, 15.0, theme::dim(), 400, None, None);
    // `VITRO_PV_DEBUG=1`：右下角寫出抓到的節拍，實測時判斷準不準
    if let Some(p) = pv_debug {
        text.set_fx(1.0, 0.0);
        let bpm = if p.period > 0.0 { 60.0 / p.period } else { 0.0 };
        let s = format!("BPM {bpm:.1}　信心 {:.2}　鼓點 {:.2}　響度 {:.2}（可信 {:.2}）", p.conf, p.hit, p.energy, p.energy_conf);
        let (tw, _) = text.measure(&s, 12.0, 16.0, None);
        text.push(&s, fw - tw - 16.0, fh - 28.0, 12.0, 16.0, theme::ink(0.7), 400, None, None);
        text.set_fx(left_a, left_dy);
    }

    // ---- 右側：歌詞 ----
    text.set_fx(ly_a, ly_dy);
    quads.set_fx(ly_a, ly_dy);
    let (lx, lw, mid_y) = (l.lyrics_x, l.lyrics_w, l.lyrics_mid);
    match now.lyrics {
        // 文字 PV 已經畫過了，而且整個換過去了
        _ if pv_on && mix >= 1.0 => {}
        Some(ly) if !ly.synced.is_empty() => {
            let cur = current_line(ly, now.lyric_ms);
            let cur_i = cur.map(|i| i as i64).unwrap_or(-1);
            // 字級照 Python 版歌詞頁的 CSS：clamp(30px, 3.3vw, 52px)、行高 1.32、
            // 上下留白 clamp(10px, 1.1vh, 18px)。每一句一樣大，當前句只放大 3%。
            //
            // 捲動：`scroll` 是呼叫端補間過的「目前捲到第幾句」，換句時從上一句滑過來
            // （CSS 的 transform .45s）。每句的高度看它折成幾行，所以位置是逐句累加的；
            // 大小與亮度看它離 `scroll` 多遠，換句時是漸變的（.35s 那組）
            const SPAN: i64 = 5;
            let fs = (fw * 0.033).clamp(30.0, 52.0);
            let flh = fs * 1.32;
            let pad = (fh * 0.011).clamp(10.0, 18.0);
            let scroll = now.lyric_scroll;
            let k0 = scroll.floor() as i64;
            let (lo, hi) = (k0 - SPAN - 1, k0 + SPAN + 2);
            let n = ly.synced.len() as i64;
            // 翻譯：原文下面一行小字。字級是原文的一半多一點，沒有 `trans` 的句子高度不變
            let (tsize, tgap) = ((fs * 0.52).max(15.0), fs * 0.12);
            let tlh = tsize * 1.4;
            let trans_of = |i: i64| -> Option<&str> {
                if !now.show_trans || i < 0 || i >= n {
                    return None;
                }
                ly.synced[i as usize].trans.as_deref().filter(|s| !s.trim().is_empty())
            };
            // 每句（含上下留白）的高度。空行照 CSS 的 `.lyric.blank` 只留一小段
            // `orig_h` 只有原文那一段，畫翻譯時要知道從哪裡接下去
            let mut heights = Vec::with_capacity((hi - lo + 1) as usize);
            let mut orig_h = Vec::with_capacity((hi - lo + 1) as usize);
            for i in lo..=hi {
                let (h, oh) = if i < 0 || i >= n {
                    (flh + 2.0 * pad, 0.0)
                } else if ly.synced[i as usize].text.is_empty() {
                    (flh * 0.5, 0.0)
                } else {
                    // 要用畫的時候的字重（600）量：粗體比較寬，用 400 量會一行放得下、
                    // 畫出來卻折成兩行，第二行就壓到下一句上（實際發生過）
                    let oh = text.measure_weight(&ly.synced[i as usize].text, fs, flh, Some(lw), 600).1;
                    let th = match trans_of(i) {
                        Some(t) => tgap + text.measure_weight(t, tsize, tlh, Some(lw), 400).1,
                        None => 0.0,
                    };
                    (oh + th + 2.0 * pad, oh)
                };
                heights.push(h);
                orig_h.push(oh);
            }
            let hgt = |i: i64| heights[(i - lo) as usize];
            // 每句中心相對於第 k0 句中心的位移
            let mut centers = vec![0.0f32; heights.len()];
            for i in k0 + 1..=hi {
                centers[(i - lo) as usize] = centers[(i - 1 - lo) as usize] + (hgt(i - 1) + hgt(i)) * 0.5;
            }
            for i in (lo..k0).rev() {
                centers[(i - lo) as usize] = centers[(i + 1 - lo) as usize] - (hgt(i) + hgt(i + 1)) * 0.5;
            }
            let at = |i: i64| centers[(i - lo) as usize];
            let shift = at(k0) + (at(k0 + 1) - at(k0)) * (scroll - k0 as f32);

            for i in lo..=hi {
                if i < 0 || i >= n {
                    continue;
                }
                let l = &ly.synced[i as usize];
                if l.text.is_empty() {
                    continue;
                }
                // 畫不畫看它離畫面中線（`scroll`）多遠；亮度與放大看它離正在唱的那句（`focus`）多遠。
                // 跟著唱的時候兩個一樣，用滾輪捲走時才分開
                let dist_s = (i as f32 - scroll).abs();
                if dist_s > SPAN as f32 + 0.5 {
                    continue;
                }
                let dist = (i as f32 - now.pick.focus).abs();
                let e = (1.0 - dist).clamp(0.0, 1.0);
                let mut alpha = if dist < 1.0 { 1.0 + (0.43 - 1.0) * dist } else { (0.52 - dist * 0.09).max(0.10) };
                // 自己捲的時候：中線附近的句子都提亮到看得清楚，上下邊緣照樣淡出
                alpha = alpha.max((0.62 - dist_s * 0.08).max(0.14) * now.pick.browse);
                let fade = |p: Option<(usize, f32)>| p.filter(|(j, _)| *j == i as usize).map_or(0.0, |(_, a)| a);
                let hov = fade(now.pick.hover).max(fade(now.pick.unhover));
                alpha = alpha.max(0.8 * hov);
                // 點下去那一下：先縮一點再彈回來，底色亮一下再慢慢退掉
                let flash = now.pick.flash.filter(|(j, _)| *j == i as usize).map(|(_, t)| t);
                let press = flash.map_or(0.0, |t| (std::f32::consts::PI * (t / 0.28).min(1.0)).sin());
                let k = (1.0 + 0.03 * e) * (1.0 - 0.025 * press);
                let (size, lh) = (fs * k, flh * k);
                // 放大時排版寬度跟著放大，折行位置才會跟原本一樣（CSS 的 scale 不會重排）
                // （多給 1 px：字形寬度放大後的捨入，可能讓剛好貼齊的句子多折一行）
                let mw = lw * k + 1.0;
                let th = (hgt(i) - 2.0 * pad) * k;
                let ty = mid_y + at(i) - shift - th * 0.5;

                // 點得到的範圍＝滑鼠停著時的底色：字的寬度往外留一點、上下吃掉一半留白。
                // 太淡的（快捲出畫面的）不給點
                if alpha > 0.12 && ty + th > 0.0 && ty < fh {
                    let mut tw = text.measure_weight(&l.text, size, lh, Some(mw), 600).0;
                    if let Some(t) = trans_of(i) {
                        tw = tw.max(text.measure_weight(t, tsize * k, tlh * k, Some(mw), 400).0);
                    }
                    let r = ui::Rect::new(lx - 16.0, ty - pad * 0.7, tw + 32.0, th + pad * 1.4);
                    let glow = 0.07 * crate::anim::EASE.at(hov)
                        + flash.map_or(0.0, |t| 0.12 * (1.0 - crate::anim::OUT.at((t / FLASH_S).min(1.0))));
                    if glow > 0.002 {
                        quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(theme::radius::CARD).color(theme::ink(glow)));
                    }
                    // 進場還在浮上來時位置不準，先不給點
                    if ly_a > 0.9 {
                        hits.lines.push((i as usize, r));
                    }
                }

                // 逐字：正在唱的那一句，加上剛換走、最後一個字還沒唱完的上一句
                // （有些來源最後一個字會拖過下一句的開頭約 300 ms，換句時不能就這樣停在半途）。
                // 捲動途中也要走這條：不然新的一句會先照「離中線多近」漸亮到全白，
                // 停穩那一刻才換成暗底，看起來就是閃一下白
                let lit = if now.ktv && (i == cur_i || i == cur_i - 1) {
                    l.words.as_ref().and_then(|ws| ktv_progress(&l.text, ws, now.lyric_ms))
                } else {
                    None
                };

                // 翻譯：原文下面接一行小字、亮度打六五折，不跟著逐字掃光
                if let Some(t) = trans_of(i) {
                    let ty2 = ty + (orig_h[(i - lo) as usize] + tgap) * k;
                    text.push(t, lx, ty2, tsize * k, tlh * k, theme::ink(alpha * 0.65), 400, Some(mw), None);
                }

                if let Some(lit) = lit {
                    // 暗的打底。從下面滑上來時 alpha 比 0.38 還低，取小的才不會亮一下
                    let dim = alpha.min(0.38);
                    text.push(&l.text, lx, ty, size, lh, theme::ink(dim), 600, Some(mw), None);
                    // 亮的只露出已經唱過的那一段，每個排版行各切一次
                    for (top, bottom, right) in text.lit_runs(&l.text, size, lh, Some(mw), 600, lit) {
                        text.push(
                            &l.text,
                            lx,
                            ty,
                            size,
                            lh,
                            theme::ink(alpha),
                            600,
                            Some(mw),
                            Some(glyphon::TextBounds {
                                left: (lx - 4.0) as i32,
                                top: (ty + top).floor() as i32,
                                right: (lx + right).round() as i32,
                                bottom: (ty + bottom).ceil() as i32,
                            }),
                        );
                    }
                    continue;
                }

                text.push(
                    &l.text,
                    lx,
                    ty,
                    size,
                    lh,
                    theme::ink(alpha),
                    600, // CSS 的 .lyric 每句都是 600；換字重會在換句時跳一下
                    Some(mw),
                    None,
                );
            }
        }
        // 還在抓：留白，不要閃一下「不知道這首的歌詞」
        Some(ly) if ly.source == "loading" => {}
        Some(ly) if ly.source == "instrumental" => {
            notice(text, &l, "這是演奏曲。", "沒有歌詞可以顯示。");
        }
        Some(ly) if ly.source == "error" => {
            notice(text, &l, "歌詞連線失敗。", "檢查一下網路，或稍後再回到這首。");
        }
        Some(ly) if !ly.plain.is_empty() => {
            // 沒有時間軸，只能用滾輪捲。底部留 10% 才不會最後一行貼著視窗邊
            let top = fh * 0.2;
            let th = text.measure(&ly.plain, 19.0, 30.0, Some(lw)).1;
            hits.plain_max = (top + th - fh * 0.9).max(0.0);
            let y = top - now.pick.plain_y.min(hits.plain_max);
            text.push(&ly.plain, lx, y, 19.0, 30.0, theme::ink(0.6), 400, Some(lw), None);
        }
        _ => {
            // 用語照 web 版。附一顆「前往 LRCLIB」—— 歌詞是社群投稿的，
            // 缺的話能補的只有使用者自己
            notice(text, &l, "嗯，我們也不知道這首歌曲的歌詞。", "或許你可以考慮向 LRCLIB 提交？");
            let r = l.lrclib;
            let hot = hover == Some(Hit::OpenLrclib);
            quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(theme::radius::CONTROL)
                .color(theme::ink(if hot { 0.14 } else { 0.07 })));
            let label = "前往 LRCLIB";
            let (bw, _) = text.measure_weight(label, 12.0, 15.0, None, 600);
            let c = if hot { theme::pick([0.93, 0.93, 0.95, 1.0], theme::fg()) } else { theme::pick([0.76, 0.77, 0.80, 1.0], theme::soft()) };
            text.push(label, r.cx() - bw * 0.5, r.cy() - 7.5, 12.0, 15.0, c, 600, None, None);
        }
    }

    text.set_fx(left_a, left_dy);
    quads.set_fx(left_a, left_dy);
    {
        // PV 模式的背景與演出層已經在上面畫好了，不能清掉
        let clear = if pv_on { None } else { Some(wgpu::Color::BLACK) };
        let mut pass = gfx::pass(&mut enc, &target.view, "now-main", clear);
        quads.flush(gpu, &mut pass, w, h);
    }

    // ---- 傳輸鍵 ----
    //
    // **歌詞頁沒有浮動控制列。** 原版是整條往下收出畫面，傳輸鍵改由左欄的
    // `#np-controls` 接手 —— 所以這一排一直都在，不會自動隱藏。
    // 播放鍵是白色實心圓，圖示反過來是深色的。
    // 外觀全部照 CSS 的 `.barbtn`：40px 圓形、圖示 19px、平常 #c2c3cc，
    // 滑鼠停著變 --fg 並墊一層 .1 的白底；開著的模式鍵是綠色加底下一顆 3px 小圓點；
    // 不能按的（還不知道是哪首的愛心、不給調的音量）透明度 .35。
    use crate::icons::Svg;
    use crate::smtc::Repeat;
    text.set_fx(left_a, left_dy);
    let t = &now.tools;
    let vol_ok = !t.vol_locked;
    let liked = t.liked == Some(true);
    for (r, ic, hit, on, dot, enabled) in [
        (l.shuffle, Svg::Shuffle, Hit::Shuffle, now.shuffle, true, true),
        (l.prev, Svg::Prev, Hit::Prev, false, false, true),
        (l.next, Svg::Next, Hit::Next, false, false, true),
        (l.repeat, Svg::Repeat, Hit::Repeat, now.repeat != Repeat::Off, true, true),
        // 收藏是填滿，不加小圓點（`.js-like.on::after { display: none }`）
        (l.like, if liked { Svg::HeartFilled } else { Svg::Heart }, Hit::Like, liked, false, t.liked.is_some()),
        (l.devices, Svg::Device, Hit::Devices, false, false, true),
        (l.mute, ui::vol_icon(t.volume), Hit::Mute, false, false, vol_ok),
        (l.settings, Svg::Gear, Hit::Settings, false, false, true),
    ] {
        let one = hit == Hit::Repeat && now.repeat == Repeat::Track;
        ui::svg_button(gpu, quads, text, &r, r.w, ic, on, dot, enabled, hover == Some(hit), one);
    }

    // 播放鍵：54px 的 --fg 實心圓，圖示 24px 深色；滑鼠停著放大 1.06、變純白
    let hot = hover == Some(Hit::PlayPause);
    let s = if hot { l.play.w * 1.06 } else { l.play.w };
    quads.push(
        Quad::new(l.play.cx() - s * 0.5, l.play.cy() - s * 0.5, s, s)
            .radius(s * 0.5)
            .color(if hot { theme::ink(1.0) } else { theme::fg() }),
    );
    let ic = if now.playing { Svg::Pause } else { Svg::Play };
    quads.svg(gpu, ic, 24.0, l.play.cx(), l.play.cy(), theme::on_ink());

    // 音量滑桿：4px、底 .16、填滿 --fg，停著或拖著時給 12px 圓鈕
    let r = l.volume;
    let va = if vol_ok { 1.0 } else { 0.35 };
    let v = t.volume.unwrap_or(0) as f32 / 100.0;
    quads.push(Quad::new(r.x, r.cy() - 2.0, r.w, 4.0).radius(theme::radius::TRACK).color(theme::ink(0.16 * va)));
    if v > 0.0 {
        quads.push(Quad::new(r.x, r.cy() - 2.0, (r.w * v).max(4.0), 4.0).radius(theme::radius::TRACK)
            .color({ let f = theme::fg(); [f[0], f[1], f[2], va] }));
    }
    if vol_ok && (now.vol_drag || matches!(hover, Some(Hit::Volume(_)) | Some(Hit::Mute))) {
        let kx = r.x + r.w * v;
        quads.push(
            Quad::new(kx - 6.0, r.cy() - 6.0, 12.0, 12.0)
                .radius(theme::radius::SM)
                .color(theme::ink(1.0))
                .shadow_of(&theme::SHADOW_KNOB, 1.0),
        );
    }

    {
        text.prepare(gpu, w, h)?;
        let mut pass = gfx::pass(&mut enc, &target.view, "now-controls", None);
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    }
    let _ = bar_alpha;

    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok(hits)
}

/// `VITRO_PV_DEBUG` 有沒有設（只讀一次）
fn pv_debug() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VITRO_PV_DEBUG").is_some())
}

pub fn fmt_ms(ms: i64) -> String {
    let s = (ms.max(0) / 1000) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// 右欄的說明文字（`.lyric-empty`）：一行標題、一行副標，對齊歌詞的左緣。
fn notice(text: &mut Text, l: &ui::NowL, title: &str, sub: &str) {
    text.push(title, l.lyrics_x, l.empty_title_y, l.empty_title, l.empty_title * 1.5,
              theme::pick([0.925, 0.93, 0.95, 0.82], { let f = theme::fg(); [f[0], f[1], f[2], 0.82] }), 600, Some(l.lyrics_w), None);
    text.push(sub, l.lyrics_x, l.empty_sub_y, l.empty_sub, l.empty_sub * 1.6,
              theme::ink(0.5), 400, Some(l.lyrics_w), None);
}

/// 右欄現在是不是「不知道這首的歌詞」（有「前往 LRCLIB」那顆鈕）。
/// 命中測試要跟上面 `match` 的分支一致 —— 條件改了兩邊要一起改。
pub fn shows_lrclib(ly: Option<&Lyrics>) -> bool {
    match ly {
        Some(ly) => ly.synced.is_empty()
            && ly.source != "instrumental"
            && ly.source != "loading"
            && ly.source != "error"
            && ly.plain.is_empty(),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::ktv_progress;
    use crate::lyrics::Word;

    fn w(t: i64, d: i64, s: &str) -> Word {
        Word { t, d, text: s.into() }
    }

    #[test]
    fn 逐字進度照位元組與時間() {
        // 「你好 嗎」：中文一字 3 位元組
        let ws = [w(0, 100, "你"), w(100, 100, "好 "), w(200, 200, "嗎")];
        let line = "你好 嗎";
        assert_eq!(ktv_progress(line, &ws, 0), Some(0.0));
        assert_eq!(ktv_progress(line, &ws, 50), Some(1.5));
        assert_eq!(ktv_progress(line, &ws, 150), Some(5.0)); // 「好 」是 4 位元組
        assert_eq!(ktv_progress(line, &ws, 300), Some(8.5));
        // 唱完了就交回整行畫
        assert_eq!(ktv_progress(line, &ws, 400), None);
    }

    #[test]
    fn 開頭空白不會讓進度歪掉() {
        // `text` 是 trim 過的，字串接起來多一個開頭空白
        let ws = [w(0, 100, " 你"), w(100, 100, "好")];
        assert_eq!(ktv_progress("你好", &ws, 100), Some(3.0));
    }
}
