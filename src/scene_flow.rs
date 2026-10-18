//! 封面流畫面的組裝 —— 目前餵的是程式產生的假封面，M2 完成後會換成
//! `player.rs` 推過來的真資料。
//!
//! 算圖分四趟，順序不能換：
//!
//! ```text
//!   1. bg_src   中央封面鋪滿整個畫面
//!   2. blur     → 當背景（web 版也是這樣做的）
//!   3. main     背景 + 暗紗 + 封面流 + 倒影
//!   4. blur     → 給控制列的玻璃取樣
//!   5. glass    控制列，最後蓋文字
//! ```

use std::sync::Arc;

use crate::cover::{self, CoverQuad, CoverRenderer};
use crate::gfx::{Gpu, Offscreen};
use crate::glass::{Glass, Panel};
use crate::quad::{Quad, QuadRenderer};
use crate::text::Text;
use crate::theme;
use crate::ui::{self, Hit};

pub struct Album {
    pub name: String,
    pub artist: String,
    pub art: Arc<wgpu::TextureView>,
    /// 這張自己的動畫（幽靈進退場、換圖脈衝）
    pub fx: CoverFx,
}

/// 單張封面的動畫參數。
#[derive(Clone, Copy, Debug)]
pub struct CoverFx {
    /// 位置往中央那側少算幾格：幽靈插進來時，右邊的封面從 idx−1 滑到 idx
    pub shift: f32,
    pub scale: f32,
    pub alpha: f32,
}

impl Default for CoverFx {
    fn default() -> Self {
        Self { shift: 0.0, scale: 1.0, alpha: 1.0 }
    }
}

/// 封面流頂端的 KTV 提詞（Python 版的 `#ktv`）：目前這句＋下一句預告。
pub struct Ktv<'a> {
    pub cur: &'a str,
    pub next: &'a str,
    /// 目前這句唱到哪（0..1），亮的部分從左邊掃過去
    pub wipe: f32,
    /// 換句之後幾秒（往上推的動畫）
    pub age: f32,
}

/// 整個封面流這一幀的動畫參數。
pub struct FlowFx<'a> {
    /// 背景換圖時的交叉淡化：上一張與它還剩多少（1 → 0）
    pub bg_prev: Option<(&'a Arc<wgpu::TextureView>, f32)>,
    /// 展開／收起曲目時整片縮放與淡出（Python 版的 `swapFlow`），以中央為軸
    pub scale: f32,
    pub alpha: f32,
    /// 轉場中，中央那張交給替身飛，這裡不畫
    pub hide_centre: bool,
    /// 中央那張的倒影透明度。替身落地時倒影要淡入，一幀冒出來會閃
    pub centre_refl: f32,
    /// 控制列往下位移（進出歌詞頁時收下去、升上來）
    pub bar_dy: f32,
    /// 控制列的存在度：大面板蓋到它時消散成 0（玻璃不疊玻璃），面板收掉再凝回來
    pub bar_k: f32,
}

impl Default for FlowFx<'_> {
    fn default() -> Self {
        Self { bg_prev: None, scale: 1.0, alpha: 1.0, hide_centre: false, centre_refl: 1.0, bar_dy: 0.0, bar_k: 1.0 }
    }
}

/// 背景圖的底色：沒有封面（搜尋篩不到東西）時看到的就是它。深色是黑，淺色是淺灰
pub fn empty_bg() -> wgpu::Color {
    let [r, g, b] = theme::backdrop().empty;
    wgpu::Color { r: r as f64, g: g as f64, b: b as f64, a: 1.0 }
}

/// 背景上那兩層紗。照抄 CSS 的 `#backdrop-veil`：
///
/// ```text
///   radial-gradient(ellipse at 50% 42%, 透明 0%, rgba(0,0,0,.55) 70%)
///   linear-gradient(to bottom, rgba(8,8,11,.35), .1 @45%, .95 @100%)
/// ```
///
/// **少了這兩層，背景就是一片平均過的色塊。** 暈影給深度、直向漸層讓
/// 底部壓得夠暗，控制列才浮得起來。用小張材質拉開就好 —— 漸層本來就沒有
/// 高頻細節，雙線性放大看不出來。
///
/// 淺色模式是同樣兩層，但換成白紗（`theme::Backdrop`）：四周與底部洗白，
/// 控制列一樣浮得起來。兩套都先做好，換外觀時不用重建
pub struct Veil {
    radial: [Arc<wgpu::TextureView>; 2],
    vert: [Arc<wgpu::TextureView>; 2],
}

impl Veil {
    pub fn new(gpu: &Gpu) -> Self {
        let [dr, dv] = Self::build(gpu, &theme::DARK.backdrop);
        let [lr, lv] = Self::build(gpu, &theme::LIGHT.backdrop);
        Self { radial: [dr, lr], vert: [dv, lv] }
    }

    fn build(gpu: &Gpu, b: &theme::Backdrop) -> [Arc<wgpu::TextureView>; 2] {
        const N: u32 = 128;
        let [rr, rg, rb] = b.veil_radial_rgb;
        let mut rad = Vec::with_capacity((N * N * 4) as usize);
        for y in 0..N {
            for x in 0..N {
                let (u, v) = (x as f32 / (N - 1) as f32, y as f32 / (N - 1) as f32);
                // 預設的 ellipse 大小是「到最遠的角」
                let far = ((0.5f32).max(0.5) * 2.0_f32.sqrt()).max(1e-3);
                let d = ((u - 0.5).powi(2) + (v - 0.42).powi(2)).sqrt() / far;
                let a = (d / 0.7).clamp(0.0, 1.0) * b.veil_radial_a;
                rad.extend_from_slice(&[rr, rg, rb, (a * 255.0) as u8]);
            }
        }
        let [vr, vg, vb] = b.veil_vert_rgb;
        let [top, mid, bottom] = b.veil_vert_a;
        let mut col = Vec::with_capacity(256 * 4 * 2);
        for y in 0..256u32 {
            let t = y as f32 / 255.0;
            let a = if t < 0.45 {
                top + (mid - top) * (t / 0.45)
            } else {
                mid + (bottom - mid) * ((t - 0.45) / 0.55)
            };
            // 兩個 texel 寬，取樣時才不會被邊緣夾出奇怪的值
            for _ in 0..2 {
                col.extend_from_slice(&[vr, vg, vb, (a * 255.0) as u8]);
            }
        }
        [
            crate::gfx::upload_rgba_linear(gpu, N, N, &rad, "veil-radial"),
            crate::gfx::upload_rgba_linear(gpu, 2, 256, &col, "veil-vert"),
        ]
    }

    /// 目前外觀模式的暈影
    pub fn radial(&self) -> &Arc<wgpu::TextureView> {
        &self.radial[(theme::mode() == theme::Mode::Light) as usize]
    }

    /// 目前外觀模式的直向漸層
    pub fn vert(&self) -> &Arc<wgpu::TextureView> {
        &self.vert[(theme::mode() == theme::Mode::Light) as usize]
    }
}

/// 可見範圍。超出這個距離的封面被前面的完全遮住，畫了也看不到。
const SPAN: i32 = 7;

#[allow(clippy::too_many_arguments)]
/// 控制列左段與進度條要的東西：**正在播的那首**。
pub struct NowBar<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub art: Option<Arc<wgpu::TextureView>>,
    pub pos_ms: i64,
    pub dur_ms: i64,
    pub playing: bool,
    /// 隨機、重複（模式鍵的綠色與小圓點）
    pub shuffle: bool,
    pub repeat: crate::smtc::Repeat,
    /// 愛心與音量
    pub tools: ui::Tools,
    /// 正在拖音量滑桿（把手要一直亮著，游標跑出滑桿也一樣）
    pub vol_drag: bool,
    /// 展開中：左上角返回鍵的名字（已經截好）與它量出來的寬度。
    /// 寬度由呼叫端量，命中測試才拿得到同一個數字
    pub back: Option<(&'a str, f32)>,
    /// 曲名、歌手放不下時來回捲動的時鐘（秒）。`None` 停在開頭（截圖）
    pub roll: Option<f32>,
    /// 動畫。跟控制列無關，但每個呼叫端本來就傳這個結構，放這裡省得改簽名
    pub fx: FlowFx<'a>,
    /// KTV 提詞，`None` 就不畫。理由同上
    pub ktv: Option<Ktv<'a>>,
}

impl<'a> NowBar<'a> {
    /// `--shot` 用：假裝正在播第一張的某一首，播到一小段。
    pub fn demo(albums: &'a [Album], playing: bool) -> Self {
        NowBar {
            title: "一場遊戲一場夢",
            artist: "王傑",
            art: albums.first().map(|a| a.art.clone()),
            pos_ms: 64_300,
            dur_ms: 255_333,
            playing,
            shuffle: true,
            repeat: crate::smtc::Repeat::Track,
            tools: ui::Tools { liked: Some(true), volume: Some(62), vol_locked: false },
            vol_drag: false,
            back: None,
            roll: std::env::var("VITRO_MARQUEE_T").ok().and_then(|v| v.parse().ok()),
            fx: FlowFx::default(),
            ktv: None,
        }
    }
}

/// 毫秒 → `m:ss`。**截斷不四捨五入**，才跟 Spotify 顯示的秒數一致
/// （web 版的 `fmt` 也是這樣）。
pub fn fmt_ms(ms: i64) -> String {
    let s = (ms.max(0) / 1000) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

#[allow(clippy::too_many_arguments)]
pub fn render(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    covers: &mut CoverRenderer,
    glass: &mut Glass,
    text: &mut Text,
    albums: &[Album],
    centre: f32,
    now: &NowBar,
    scale: f32,
    veil: &Veil,
    hover: Option<Hit>,
) -> anyhow::Result<()> {
    let playing = now.playing;
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    // 版面的數字全部來自 `ui::flow` —— 命中測試讀的是同一份
    let mut l = ui::flow(fw, fh, scale);
    l.shift_bar(now.fx.bar_dy);
    let fx = &now.fx;
    let cover_px = l.cover_px;
    let (cx, cy) = (l.cx, l.cy);

    let idx = centre.round() as i32;
    // **清單可以是空的**：搜尋篩不到東西的時候。`clamp(0, -1)` 會直接
    // panic（min > max），所以先擋掉。
    let cur = if albums.is_empty() {
        None
    } else {
        albums.get(idx.clamp(0, albums.len() as i32 - 1) as usize)
    };

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("flow") });

    // ---- 1+2：中央封面鋪滿 → 模糊，當背景 ----
    // quads 一幀只 begin 一次，中間各趟只 flush。資料統一在最後 upload。
    quads.begin(gpu);
    let bg_src = Offscreen::new(&gpu.device, w, h);
    {
        // 換圖時上一張墊底，新的那張淡進來（CSS `background-image .7s`）
        let k = match fx.bg_prev {
            Some((prev, w)) => {
                quads.push_tex(Quad::new(0.0, 0.0, fw, fh).cover(1.0, 1.0), prev);
                1.0 - w
            }
            None => 1.0,
        };
        if let Some(a) = cur {
            quads.push_tex(Quad::new(0.0, 0.0, fw, fh).cover(1.0, 1.0).color(theme::tex(k)), &a.art);
        }
        let mut pass = clear_pass(&mut enc, &bg_src.view, empty_bg(), "bg");
        quads.flush(gpu, &mut pass, w, h);
    }
    let bg = if std::env::var_os("VITRO_NO_BG_BLUR").is_some() {
        // 診斷用：跳過模糊，直接拿未模糊的當背景，好分辨是哪一段壞掉。
        Arc::new(bg_src.texture.create_view(&Default::default()))
    } else {
        Arc::new(glass.blur_scene(gpu, &mut enc, &bg_src, theme::SIGMA_BACKDROP))
    };

    // ---- 3：背景 + 兩層紗 + 封面流 ----
    // 照 CSS 的 `#backdrop-img`：blur ＋ saturate(1.7) ＋ brightness(.55)，
    // 整張再乘 opacity .85。**平均壓暗不等於這個** —— 模糊會把顏色平均掉，
    // 不補飽和度的話整片是灰的。淺色模式不壓暗，改成蓋一層白（`theme::Backdrop`）
    let bd = theme::backdrop();
    let m = bd.flow_mul;
    quads.push_tex(
        Quad::new(0.0, 0.0, fw, fh).textured().saturate(bd.flow_sat).color([m, m, m, 1.0]),
        &bg,
    );
    if bd.flow_wash[3] > 0.0 {
        quads.push(Quad::new(0.0, 0.0, fw, fh).color(bd.flow_wash));
    }
    // 暈影：中間透明、四周壓暗（淺色是洗白），給深度
    quads.push_tex(Quad::new(0.0, 0.0, fw, fh).textured(), veil.radial());
    // 直向漸層：上面稍暗、中間最淡、底部幾乎全黑，控制列才浮得起來
    quads.push_tex(Quad::new(0.0, 0.0, fw, fh).textured(), veil.vert());

    covers.begin();
    for off in -SPAN..=SPAN {
        let i = idx + off;
        if i < 0 || i >= albums.len() as i32 {
            continue;
        }
        if fx.hide_centre && i == idx {
            continue;
        }
        let al = &albums[i as usize];
        let d = i as f32 - al.fx.shift - centre;
        let (mut xform, _) = cover::transform(d, cover_px, al.fx.scale * fx.scale);
        // 整片縮放以中央為軸：位置跟著縮
        xform[0] *= fx.scale;
        let a = al.fx.alpha * fx.alpha;
        let base = CoverQuad {
            xform,
            size: [cover_px, cover_px, 6.0, cover::PERSPECTIVE],
            tint: theme::tex(a),
            extra: [cx, cy, 0.0, 0.0],
        };
        let art = &al.art;
        covers.push(base, art);
        // 倒影：接在下緣，長度是封面的 0.55 倍
        let refl_a = if i == idx { a * fx.centre_refl } else { a };
        covers.push(CoverQuad { extra: [cx, cy, 1.0, 0.55], tint: theme::tex(refl_a), ..base }, art);
    }
    covers.sort_back_to_front();

    {
        let mut pass = load_pass(&mut enc, &target.view, "main", true);
        quads.flush(gpu, &mut pass, w, h);
        covers.render(gpu, &mut pass, w, h);
    }

    // ---- 4+5：控制列的玻璃 ----
    let backdrop = glass.blur_scene(gpu, &mut enc, target, theme::GLASS_BAR.sigma);
    let sharp = glass.sharp_scene(gpu, &mut enc, target);

    // 控制列壓在很亮的東西上時整條翻成淺色玻璃，上面的字與圖示跟著翻深色。
    // 三段一起翻：各翻各的話同一條控制列會一段深一段淺
    let tone = glass.tone("bar", [l.bar.x, l.bar.y, l.bar.w, l.bar.h]);

    // 三段獨立的玻璃，不是一整條。原本照 CSS 的 `.barseg` 是圓角 20，
    // 第 5 階段第四批改成膠囊（Liquid Glass 的控制列）。
    // 指著某一段上的按鈕時，那一段從指標處亮起來，光源也轉過去；形狀不變。
    // 大面板蓋過來時整條消散（`bar_k`），面板的玻璃才不會疊在控制列的玻璃上
    glass.begin();
    let bk = fx.bar_k.clamp(0.0, 1.0);
    let spot = hot_rect(&l, hover);
    for (seg, key) in [(l.bar_now, "bar_now"), (l.bar_main, "bar_main"), (l.bar_extra, "bar_extra")] {
        let on = spot.filter(|r| seg.contains(r.cx(), r.cy()));
        let t = glass.touch(key, seg.arr(), on.is_some(), on.map_or((seg.cx(), seg.cy()), |r| (r.cx(), r.cy())));
        if bk > 0.0 {
            glass.push(
                Panel::new(seg.x, seg.y, seg.w, seg.h)
                    .radius(theme::radius::BAR)
                    .style(&theme::GLASS_BAR)
                    .tone(tone)
                    .touch(&t, false)
                    .materialize(bk),
            );
        }
    }
    // 播放鍵是這一頁的主要動作：染強調色的玻璃。單顆的按鈕，按住時像凝膠一樣變形
    {
        let r = l.play;
        let hot = hover == Some(Hit::PlayPause);
        let t = glass.touch("play", r.arr(), hot, (r.cx(), r.cy()));
        if bk > 0.0 {
            glass.push(
                Panel::new(r.x, r.y, r.w, r.h)
                    .radius(r.w * 0.5)
                    .style(if hot { &theme::GLASS_BAR_HOT } else { &theme::GLASS_BAR })
                    .tinted(theme::accent())
                    .tone(tone)
                    .touch(&t, true)
                    .materialize(bk),
            );
        }
    }
    let back = now.back.map(|(name, w)| (name, ui::drill_back(w)));
    if let Some((_, r)) = back {
        let hot = hover == Some(Hit::DrillBack);
        let t = glass.touch("back", r.arr(), hot, (r.cx(), r.cy()));
        glass.push(
            Panel::new(r.x, r.y, r.w, r.h)
                .radius(r.h * 0.5)
                .style(if hot { &theme::GLASS_BAR_HOT } else { &theme::GLASS_BAR })
                .tone(tone)
                .touch(&t, true),
        );
    }

    // ---- 文字 ----
    text.begin();
    if let Some(a) = cur {
        let (tw, _) = text.measure(&a.name, 30.0, 40.0, None);
        text.push(&a.name, cx - tw * 0.5, cy + cover_px * 0.72, 30.0, 40.0,
                  theme::ink(0.97), 600, None, None);
        let (aw, _) = text.measure(&a.artist, 18.0, 26.0, None);
        text.push(&a.artist, cx - aw * 0.5, cy + cover_px * 0.72 + 42.0, 18.0, 26.0,
                  theme::ink(0.55), 400, None, None);
    }
    // KTV 提詞：頂端置中兩行。字型大小照 CSS 的 clamp（跟著視窗寬度）
    if let Some(k) = &now.ktv {
        use crate::anim::{phase, SOFT};
        let cs = (fw * 0.021).clamp(22.0, 38.0);
        let ns = (fw * 0.013).clamp(15.0, 23.0);
        let max_w = fw * 0.88;
        let cur = text.fit_weight(k.cur, cs, cs * 1.3, max_w, 700);
        let next = text.fit_weight(k.next, ns, ns * 1.35, max_w, 600);
        let (cw, _) = text.measure_weight(&cur, cs, cs * 1.3, None, 700);
        let (nw, _) = text.measure_weight(&next, ns, ns * 1.35, None, 600);
        let (cy0, ny0) = (38.0, 38.0 + cs * 1.3 + 6.0);
        // 換句時整組往上推一格（ktv-roll，下一句晚 .05s）
        let roll = |delay: f32| {
            let p = SOFT.at(phase(k.age, delay, 0.34));
            (p, 14.0 * (1.0 - p))
        };
        let (ca, cdy) = roll(0.0);
        let (na, ndy) = roll(0.05);
        // 字壓在封面上要看得清楚：CSS 是兩層大半徑的黑色 text-shadow，
        // glyphon 沒有陰影，墊一塊透明、只留模糊影子的矩形代替
        let halo_w = cw.max(nw) + 80.0;
        quads.push(
            Quad::new((fw - halo_w) * 0.5, cy0 - 10.0, halo_w, ny0 + ns * 1.35 - cy0 + 20.0)
                .radius(40.0)
                .color([0.0; 4])
                .halo_of(&theme::SHADOW_COVER, ca),
        );
        let x = (fw - cw) * 0.5;
        text.set_fx(ca, cdy);
        text.push(&cur, x, cy0, cs, cs * 1.3, theme::ink(0.32), 700, None, None);
        // 唱過的部分：同一行再畫一次亮的，用裁切框切出來
        if k.wipe > 0.0 {
            text.push(
                &cur, x, cy0, cs, cs * 1.3, theme::ink(1.0), 700, None,
                Some(glyphon::TextBounds {
                    left: x as i32,
                    top: (cy0 - 6.0) as i32,
                    right: (x + cw * k.wipe.clamp(0.0, 1.0)) as i32,
                    bottom: (cy0 + cs * 1.3 + 6.0) as i32,
                }),
            );
        }
        text.set_fx(na, ndy);
        text.push(&next, (fw - nw) * 0.5, ny0, ns, ns * 1.35, theme::ink(0.42), 600, None, None);
        text.set_fx(1.0, 0.0);
    }

    // 以下都畫在控制列的玻璃上，跟著玻璃翻明暗，也跟著玻璃消散。
    // 上面的專輯名與提詞不在玻璃上，不翻
    let _tone = theme::scoped_tone(tone);
    text.set_fx(bk, 0.0);
    quads.set_fx(bk, 0.0);
    // 左段：**正在播的那首歌**，不是中央那張專輯 —— 中央那張的名字已經
    // 大大地寫在封面底下了，這一格是「現在耳朵裡聽到的是什麼」。
    // 中間那條進度也是這首歌的，兩邊講的必須是同一件事。
    {
        let tx = l.bar_art.x + l.bar_art.w + 12.0;
        // 右邊讓給愛心
        let tw = l.like.x - 4.0 - tx;
        if now.title.is_empty() {
            text.push("沒有播放中", tx, l.bar_now.cy() - 9.0, 13.5, 17.0,
                      theme::ink(0.45), 500, None, None);
        } else {
            // 放不下的在框裡來回捲動
            let y = l.bar_now.cy();
            crate::widgets::roll_line(text, now.roll, 1.0, now.title, (tx, y - 17.0, tw), (13.5, 17.0, 650),
                                      theme::ink(0.95), None, None);
            crate::widgets::roll_line(text, now.roll, 1.0, now.artist, (tx, y + 2.0, tw), (11.5, 15.0, 400),
                                      theme::ink(0.55), None, None);
        }
    }
    // 按鈕全部照 CSS 的 `.barbtn`（36px、圖示 19px），圖示是 Python 版的 SVG（`icons.rs`）。
    // 各自置中畫在自己的命中框裡 —— 用同一個中心，圖示跟可以點的地方才會對得起來。
    use crate::icons::Svg;
    use crate::smtc::Repeat;
    const D: f32 = 36.0;
    ui::svg_button(gpu, quads, text, &l.prev, D, Svg::Prev, false, false, true, hover == Some(Hit::Prev), false);
    // 播放鍵 `.barbtn.big`：42px，圖示 21px。底是染色玻璃（上面推進去的），
    // 圖示用強調色底上的字色；指著的時候玻璃換成 HOT 那組
    {
        let r = l.play;
        let ic = if playing { Svg::Pause } else { Svg::Play };
        quads.svg(gpu, ic, 21.0, r.cx(), r.cy(), theme::on_accent());
    }
    ui::svg_button(gpu, quads, text, &l.next, D, Svg::Next, false, false, true, hover == Some(Hit::Next), false);
    // 中段右半：播放時間與進度條（web 版的 `#bar-pos` / `#bar-dur`）
    let pos = now.pos_ms.clamp(0, now.dur_ms.max(0));
    text.push(&fmt_ms(pos), l.pos_x, l.bar_main.cy() - 8.0,
              11.5, 15.0, theme::ink(0.55), 400, None, None);
    let dur = fmt_ms(now.dur_ms);
    let (dw, _) = text.measure(&dur, 11.5, 15.0, None);
    text.push(&dur, l.dur_x - dw, l.bar_main.cy() - 8.0, 11.5, 15.0,
              theme::ink(0.55), 400, None, None);
    ui::svg_button(gpu, quads, text, &l.repeat, D, Svg::Repeat, now.repeat != Repeat::Off, true, true,
                   hover == Some(Hit::Repeat), now.repeat == Repeat::Track);
    ui::svg_button(gpu, quads, text, &l.shuffle, D, Svg::Shuffle, now.shuffle, true, true,
                   hover == Some(Hit::Shuffle), false);
    ui::svg_button(gpu, quads, text, &l.settings, D, Svg::Gear, false, false, true, hover == Some(Hit::Settings), false);
    // 返回鍵不在控制列上，不跟著消散
    text.set_fx(1.0, 0.0);
    if let Some((name, r)) = back {
        let a = if hover == Some(Hit::DrillBack) { 1.0 } else { 0.92 };
        text.push("‹", r.x + 12.0, r.cy() - 20.0, 30.0, 36.0, theme::ink(a), 400, None, None);
        text.push(name, r.x + 40.0, r.cy() - 9.0, 13.5, 18.0, theme::ink(a), 600, None, None);
    }
    text.set_fx(bk, 0.0);
    ui::draw_tools(gpu, quads, text, &l.like, &l.devices, &l.mute, &l.volume, &now.tools, hover, now.vol_drag);
    text.set_fx(1.0, 0.0);
    text.prepare(gpu, w, h)?;

    {
        let mut pass = load_pass(&mut enc, &target.view, "glass+text", false);
        // 左段那張小封面：正在播的那首的。圓角跟膠囊同心（膠囊圓角扣掉內縮）
        let art_r = theme::radius::concentric(theme::radius::BAR, l.bar_art.x - l.bar_now.x);
        if let Some(art) = &now.art {
            quads.push_tex(
                Quad::new(l.bar_art.x, l.bar_art.y, l.bar_art.w, l.bar_art.h)
                    .radius(art_r)
                    .cover(1.0, 1.0),
                art,
            );
        } else {
            quads.push(Quad::new(l.bar_art.x, l.bar_art.y, l.bar_art.w, l.bar_art.h)
                .radius(art_r).color(theme::pick([0.086, 0.086, 0.11, 1.0], [0.86, 0.86, 0.88, 1.0])));
        }
        // 播放進度。CSS：底 .16、填滿是前景色加一圈 10px 的光暈，
        // 滑鼠停上去才出現 12px 的白色把手
        let frac = if now.dur_ms > 0 {
            (pos as f32 / now.dur_ms as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let p = l.progress;
        quads.push(Quad::new(p.x, p.y, p.w, p.h).radius(p.h * 0.5)
            .color(theme::ink(0.16)));
        if frac > 0.0 {
            quads.push(
                Quad::new(p.x, p.y, (p.w * frac).max(p.h), p.h)
                    .radius(p.h * 0.5)
                    .color(theme::fg())
                    .shadow(0.0, 0.0, 10.0, 0.0, theme::ink(0.3)),
            );
        }
        if matches!(hover, Some(Hit::Seek(_))) {
            let kx = p.x + p.w * frac;
            quads.push(
                Quad::new(kx - 6.0, p.cy() - 6.0, 12.0, 12.0)
                    .radius(theme::radius::SM)
                    .color(theme::ink(1.0))
                    .shadow_of(&theme::SHADOW_KNOB, 1.0),
            );
        }
        quads.set_fx(1.0, 0.0);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    }

    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok(())
}

/// 控制列上被指著的那一顆在哪。玻璃的互動光從這裡擴散，也用它判斷是哪一段被指著
pub fn hot_rect(l: &ui::Flow, hover: Option<Hit>) -> Option<ui::Rect> {
    Some(match hover? {
        Hit::PlayPause => l.play,
        Hit::Prev => l.prev,
        Hit::Next => l.next,
        Hit::Settings => l.settings,
        Hit::Repeat => l.repeat,
        Hit::Shuffle => l.shuffle,
        Hit::Like => l.like,
        Hit::Devices => l.devices,
        Hit::Mute => l.mute,
        Hit::Volume(_) => l.volume,
        Hit::Seek(_) => l.progress,
        // 左段整塊都能點，光從那張小封面擴散
        Hit::OpenLyrics => l.bar_art,
        _ => return None,
    })
}

pub fn clear_pass<'a>(
    enc: &'a mut wgpu::CommandEncoder,
    view: &'a wgpu::TextureView,
    color: wgpu::Color,
    label: &'a str,
) -> wgpu::RenderPass<'a> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(color), store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

pub fn load_pass<'a>(
    enc: &'a mut wgpu::CommandEncoder,
    view: &'a wgpu::TextureView,
    label: &'a str,
    clear: bool,
) -> wgpu::RenderPass<'a> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: if clear {
                    wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                } else {
                    wgpu::LoadOp::Load
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

/// 產生一批假封面。真資料接上之前用這個驗幾何。
pub fn fake_albums(gpu: &Gpu, n: usize) -> Vec<Album> {
    // `VITRO_ART=<檔>`：整排都換成真的封面。**示範封面是平滑的合成圖案，
    // 玻璃與模糊的毛病在上面照不出來** —— 取樣不足的條紋、折射的壓縮感，
    // 都要有真實照片的高頻細節才看得見。
    let real = std::env::var("VITRO_ART")
        .ok()
        .and_then(|p| std::fs::read(&p).ok())
        .and_then(|b| crate::gfx::upload_encoded(gpu, &b, "real-art").ok());
    const NAMES: &[(&str, &str)] = &[
        ("一場遊戲一場夢", "王傑"),
        ("范特西", "周杰倫"),
        ("Random Access Memories", "Daft Punk"),
        ("後青春期的詩", "五月天"),
        ("Kind of Blue", "Miles Davis"),
        ("愛情萬歲", "張惠妹"),
        ("OK Computer", "Radiohead"),
        ("太平盛世", "陳奕迅"),
        ("Discovery", "Daft Punk"),
        ("小宇宙", "五月天"),
        ("In Rainbows", "Radiohead"),
        ("流浪到淡水", "金門王與李炳輝"),
    ];
    let size = 320u32;
    (0..n)
        .map(|i| {
            let hue = i as f32 / n as f32;
            let mut rgba = Vec::with_capacity((size * size * 4) as usize);
            for y in 0..size {
                for x in 0..size {
                    let fx = x as f32 / size as f32;
                    let fy = y as f32 / size as f32;
                    // 同心圓 + 斜條紋，被透視拉歪時一眼看得出來
                    let dx = fx - 0.5;
                    let dy = fy - 0.5;
                    let r = (dx * dx + dy * dy).sqrt();
                    let ring = ((r * 26.0).sin() * 0.5 + 0.5) * 0.35;
                    let stripe = if ((x + y) / 18) % 2 == 0 { 0.06 } else { 0.0 };
                    let v = 0.25 + ring + stripe;
                    let (rr, gg, bb) = hsv(hue + r * 0.12, 0.55, v + 0.30);
                    rgba.push((rr * 255.0) as u8);
                    rgba.push((gg * 255.0) as u8);
                    rgba.push((bb * 255.0) as u8);
                    rgba.push(255);
                }
            }
            let (name, artist) = NAMES[i % NAMES.len()];
            Album {
                fx: CoverFx::default(),
                name: name.to_string(),
                artist: artist.to_string(),
                art: real
                    .clone()
                    .unwrap_or_else(|| crate::gfx::upload_rgba(gpu, size, size, &rgba, "fake-art")),
            }
        })
        .collect()
}

fn hsv(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let h = (h.fract() + 1.0).fract() * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}
