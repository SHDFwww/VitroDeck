//! 設定視窗：獨立的第二個視窗，左邊分頁、右邊分組的設定列（macOS 系統設定的版型）。
//!
//! 取代疊在主視窗上的舊設定面板（`scene_settings.rs`）。資料沿用 `settings::Item`，
//! 值怎麼讀、怎麼改都在那裡；改完的副作用（通知子系統、存檔）由主程式的 `set_item` 負責，
//! 這裡只把「哪一項、做了什麼」交回去（[`Action`]）。
//!
//! 版面與畫法在 [`paint`]，跟視窗無關，`--shot prefs:<頁>` 也走這裡。
//! 命中框是畫的時候順便記下來的（[`Layout`]），所以滑鼠查的永遠是上一幀畫出來的位置。
//!
//! 座標全是邏輯像素；這個視窗有自己的縮放（可能跟主視窗在不同螢幕上），
//! 畫的時候用 `gfx::scoped_scale` 換成自己的。

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::config::Config;
use crate::gfx::{self, Blit, Gpu, Offscreen};
use crate::icons::Svg;
use crate::quad::{Quad, QuadRenderer};
use crate::settings::{Control, Edit, Item, Page, PAGES};
use crate::text::Text;
use glyphon::TextBounds;
use crate::theme::{self, ctl, tile, ty, win};
use crate::ui::Rect;
use crate::widgets::{self as wd, Anims, Kind, Paint, State};

/// 沒有任何變化時多久重畫一次。登入、檢查更新的進度是別的執行緒在改的，靠這個跟上
const REFRESH: Duration = Duration::from_millis(400);
/// 動畫中的重畫間隔（主視窗縮在系統列、迴圈改成 Wait 的時候用）
const FRAME: Duration = Duration::from_millis(16);
/// 建窗後這麼久還沒送出第一幀就直接顯示
const SHOW_DEADLINE: Duration = Duration::from_secs(1);

/// 每頁的圖示與方塊顏色
pub fn page_icon(p: Page) -> (Svg, [f32; 3]) {
    match p {
        Page::Account => (Svg::Person, tile::GREEN),
        Page::Display => (Svg::Monitor, tile::BLUE),
        Page::Lyrics => (Svg::Mic, tile::PINK),
        Page::Vinyl => (Svg::Disc, tile::ORANGE),
        Page::Mini => (Svg::Pip, tile::PURPLE),
        Page::Viz => (Svg::Bars, tile::CYAN),
        Page::About => (Svg::Info, tile::GRAY),
    }
}

/// 一顆按鈕要寫什麼、能不能按
#[derive(Clone, Debug)]
pub struct Btn {
    pub label: String,
    pub kind: Kind,
    pub enabled: bool,
}

impl Btn {
    pub fn new(label: &str, kind: Kind) -> Self {
        Self { label: label.into(), kind, enabled: true }
    }
    pub fn off(label: &str) -> Self {
        Self { label: label.into(), kind: Kind::Normal, enabled: false }
    }
}

/// 主程式那邊才知道的狀態
pub struct Status {
    /// 現在接著幾個螢幕（「開在哪個螢幕」的選項數）
    pub screens: usize,
    /// Spotify 那列：說明寫目前狀態，按鈕看狀態是登入、登出還是設定精靈
    pub spotify_hint: String,
    pub spotify_btn: Btn,
    /// 版本那列：說明寫更新狀態，按鈕是檢查、安裝或開下載頁
    pub update_hint: String,
    pub update_btn: Btn,
    pub data_dir: String,
    /// 介面倍率實際上被降低時寫在「介面大小」那列的說明（空字串＝照設定）
    pub zoom_note: String,
}

impl Status {
    /// 截圖用：一組看得出版面的假狀態
    pub fn demo() -> Self {
        Self {
            screens: 3,
            spotify_hint: "已登入".into(),
            spotify_btn: Btn::new("登出", Kind::Normal),
            update_hint: format!("VitroDeck {}", crate::config::VERSION),
            update_btn: Btn::new("檢查更新", Kind::Normal),
            data_dir: r"C:\Users\使用者\AppData\Roaming\VitroDeck".into(),
            zoom_note: String::new(),
        }
    }
}

/// 畫一幀要的外部狀態
pub struct Ctx<'a> {
    pub cfg: &'a Config,
    pub st: Status,
}

impl Ctx<'_> {
    fn hint(&self, it: Item) -> String {
        match it {
            Item::SpotifyStatus => self.st.spotify_hint.clone(),
            Item::Version => self.st.update_hint.clone(),
            Item::DataDir => self.st.data_dir.clone(),
            Item::Device => "切換到另一台 Spotify 裝置（D）".into(),
            Item::About => "授權、致謝，以及中斷 Spotify 連結（I）".into(),
            Item::UiScale if !self.st.zoom_note.is_empty() => self.st.zoom_note.clone(),
            _ => it.hint().to_string(),
        }
    }

    fn button(&self, it: Item) -> Btn {
        match it {
            Item::SpotifyStatus => self.st.spotify_btn.clone(),
            Item::Version => self.st.update_btn.clone(),
            _ => Btn::new(it.button_label(), Kind::Normal),
        }
    }
}

/// 點得到的東西
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// 側欄第幾頁
    Page(usize),
    /// 某一項的控件
    Ctrl(Item),
    /// 字型清單左上角的返回鈕
    Back,
    /// 字型清單的第幾個字型（`Layout::fonts` 的索引）
    FontRow(usize),
}

/// 字型清單在挑哪一個字型
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSlot {
    Cjk,
    Latin,
}

impl FontSlot {
    /// 打開這份清單的那一列
    pub fn item(self) -> Item {
        match self {
            FontSlot::Cjk => Item::FontCjk,
            FontSlot::Latin => Item::FontLatin,
        }
    }
}

/// 交回主程式的事
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Edit(Item, Edit),
    /// 在字型清單選了一個字型（家族名；英文字型的空字串是「跟中文字型一樣」）
    Font(FontSlot, String),
    /// 滑桿放開了。介面大小拖曳中不套用（視窗會跟著縮放、滑桿在游標底下跑掉），放開才套
    DragEnd(Item),
    Close,
}

/// 互動狀態。畫法只讀它
#[derive(Clone, Debug, Default)]
pub struct Ui {
    pub page: usize,
    /// 每頁各自的捲動量
    pub scroll: [f32; PAGES.len()],
    pub cursor: (f32, f32),
    pub hot: Option<Hit>,
    /// 按下去還沒放開的
    pub down: Option<Hit>,
    /// 鍵盤焦點在哪一項；`focus_visible` 是最後一次操作用的是鍵盤（滑鼠點過就不畫焦點環）
    pub focus: Option<Item>,
    pub focus_visible: bool,
    /// 字型清單開著（顯示頁「中文字型」「英文字型」那列的按鈕打開的子頁，蓋掉整個內容欄）
    pub fonts: Option<FontSlot>,
    pub fonts_scroll: f32,
    /// 下一幀把選中的字型捲到中間（剛打開清單時）
    pub fonts_reveal: bool,
}

impl Ui {
    /// 現在看的那一頁（或字型清單）捲了多少
    fn scroll(&self) -> f32 {
        if self.fonts.is_some() { self.fonts_scroll } else { self.scroll[self.page] }
    }

    fn scroll_mut(&mut self) -> &mut f32 {
        if self.fonts.is_some() { &mut self.fonts_scroll } else { &mut self.scroll[self.page] }
    }
}

/// 上一幀畫出來的位置
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub hits: Vec<(Hit, Rect)>,
    /// 每一列的範圍（鍵盤移焦點時捲到看得見）
    pub rows: Vec<(Item, Rect)>,
    /// 內容總高（含捲出去的部分）與可視高度
    pub content_h: f32,
    pub view_h: f32,
    /// 字型清單：`Hit::FontRow` 的索引對到的家族名，與選中那列的範圍
    pub fonts: Vec<String>,
    pub font_sel: Option<Rect>,
    /// 字型清單頂端固定不捲的那塊（返回鈕、標題、預覽框）有多高
    pub sticky_h: f32,
}

impl Layout {
    fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h)
    }
    fn rect(&self, h: Hit) -> Option<Rect> {
        self.hits.iter().find(|(k, _)| *k == h).map(|(_, r)| *r)
    }
    fn max_scroll(&self) -> f32 {
        (self.content_h - self.view_h).max(0.0)
    }
}

fn text_top(cy: f32, line: f32) -> f32 {
    cy - line * 0.5
}

/// 內容欄的左緣與寬：側欄右邊的空間扣掉留白，太寬就置中
fn column(w: f32) -> (f32, f32) {
    let x0 = win::SIDE_INSET * 2.0 + win::SIDE_W;
    let avail = (w - x0 - win::PAD * 2.0).max(200.0);
    let cw = avail.min(win::MAX_W);
    (x0 + win::PAD + (avail - cw) * 0.5, cw)
}

/// 畫一幀並回傳命中框。`w`、`h` 是視窗的邏輯大小
pub fn paint(p: &mut Paint, w: f32, h: f32, ui: &Ui, an: &mut Anims, ctx: &Ctx) -> Layout {
    let mut lay = Layout { view_h: h, ..Default::default() };
    let cfg = ctx.cfg;

    // ---- 側欄：浮在視窗裡的一塊
    let side = Rect::new(win::SIDE_INSET, win::SIDE_INSET, win::SIDE_W, h - win::SIDE_INSET * 2.0);
    p.quads.push(
        Quad::new(side.x, side.y, side.w, side.h)
            .radius(theme::radius::SIDEBAR)
            .color(theme::ink(win::SIDE_A))
            .border(1.0, theme::ink(win::SIDE_EDGE_A)),
    );
    let (sx, sw) = (side.x + win::SIDE_PAD, side.w - win::SIDE_PAD * 2.0);
    for (i, pg) in PAGES.iter().enumerate() {
        let r = wd::side_rect(sx, side.y + win::SIDE_TOP, sw, i);
        let (icon, col) = page_icon(*pg);
        let st = State { hot: ui.hot == Some(Hit::Page(i)), ..Default::default() };
        wd::side_item_tile(p, &r, icon, col, pg.label(), i == ui.page, st);
        lay.hits.push((Hit::Page(i), r));
    }

    // ---- 內容（或字型清單）
    let top = -ui.scroll();
    if let Some(slot) = ui.fonts {
        font_list(p, w, h, top, ui, cfg, slot, &mut lay);
    } else {
        page_rows(p, w, top, ui, an, ctx, &mut lay);
    }
    let max = lay.max_scroll();

    // ---- 捲動邊緣效果（soft）：內容捲到視窗上下緣時淡進底色，不是被一刀切掉。
    // 那一邊還有內容才出現，而且跟著捲動量慢慢長出來
    let scroll = ui.scroll();
    let left = side.x + side.w;
    // 字型清單的頂端有一塊不捲的，淡出的邊在它下緣
    edge_fade(p, left, w, lay.sticky_h, (scroll / win::EDGE_FADE).clamp(0.0, 1.0), true);
    edge_fade(p, left, w, h, ((max - scroll) / win::EDGE_FADE).clamp(0.0, 1.0), false);

    // ---- 捲軸：內容比視窗高才畫
    if max > 0.0 {
        let track = h - win::SIDE_INSET * 2.0;
        let th = (track * h / lay.content_h).max(28.0);
        let ty0 = win::SIDE_INSET + (track - th) * (scroll / max).clamp(0.0, 1.0);
        let x = w - win::SIDE_INSET * 0.5 - win::SCROLL_W;
        p.quads.push(Quad::new(x, ty0, win::SCROLL_W, th).radius(win::SCROLL_W * 0.5).color(theme::ink(win::SCROLL_A)));
    }
    lay
}

/// 一頁的設定列。`top` 是捲動後內容的頂（0 或負的）
fn page_rows(p: &mut Paint, w: f32, top: f32, ui: &Ui, an: &mut Anims, ctx: &Ctx, lay: &mut Layout) {
    let cfg = ctx.cfg;
    let page = PAGES[ui.page.min(PAGES.len() - 1)];
    let (gx, gw) = column(w);
    // 頁首：大方塊＋頁名
    let (icon, col) = page_icon(page);
    let head_cy = top + win::PAD + win::HEAD_TILE * 0.5;
    wd::tile(p, icon, col, gx + win::HEAD_TILE * 0.5, head_cy, win::HEAD_TILE, 1.0);
    p.text.push(page.label(), gx + win::HEAD_TILE + 14.0, text_top(head_cy, ty::TITLE.line), ty::TITLE.size,
                ty::TITLE.line, theme::fg(), 600, None, None);
    let mut y = top + win::PAD + win::HEAD_H;

    for (title, items) in page.groups() {
        if !title.is_empty() {
            y += wd::group_title_h();
        }
        // 字型那組上面先放預覽框（分組標題寫在預覽框上面）
        let preview_here = items.contains(&Item::FontCjk);
        if preview_here {
            let pr = Rect::new(gx, y, gw, preview_h(p.text));
            if !title.is_empty() {
                wd::group_title(p, &pr, title);
            }
            preview(p, &pr, None);
            y = pr.y + pr.h + win::PREVIEW_GAP;
        }
        let hints: Vec<String> = items.iter().map(|it| ctx.hint(*it)).collect();
        // 標籤擠到幾乎看不見的列（字級放大、控件又寬），控件排到標籤下面一行
        let label_w = |used: f32| gw - ctl::ROW_PAD * 2.0 - used - 16.0;
        let used: Vec<f32> = items.iter().map(|it| ctrl_width(p, *it, ctx)).collect();
        let stacked: Vec<bool> =
            items.iter().zip(&used).map(|(it, u)| wd::needs_stack(p.text, it.label(), label_w(*u))).collect();
        let hs: Vec<f32> = hints
            .iter()
            .zip(&stacked)
            .map(|(s, st)| if *st { wd::row_h_stacked(!s.is_empty()) } else { wd::row_h(!s.is_empty()) })
            .collect();
        let (g, rows) = wd::group_rows(gx, y, gw, &hs);
        if !title.is_empty() && !preview_here {
            wd::group_title(p, &g, title);
        }
        wd::group(p, &g, &rows);
        for (i, ((it, r), hint)) in items.iter().zip(&rows).zip(&hints).enumerate() {
            let off = it.disabled(cfg);
            let has_hint = !hint.is_empty();
            // 排到下一行的控件靠左，標籤佔上面那段（一般列的高度）
            let (label_r, (right, cy)) = if stacked[i] {
                (Rect::new(r.x, r.y, r.w, wd::row_h(has_hint)),
                 (r.x + ctl::ROW_PAD + used[i], wd::stacked_ctrl_cy(r, has_hint)))
            } else {
                (*r, wd::row_ctrl(r))
            };
            let hit = Hit::Ctrl(*it);
            // 滑桿拖曳中游標跑出去也算按著；開關與按鈕滑出去就不算（放開＝取消）
            let dragging = matches!(it.control(), Control::Slider { .. });
            let st = State {
                hot: ui.hot == Some(hit),
                down: ui.down == Some(hit) && (ui.hot == Some(hit) || dragging),
                focus: ui.focus_visible && ui.focus == Some(*it),
                disabled: off,
            };
            // 按著的旋鈕變成玻璃
            let lens = an.lens(2000 + *it as u64, st.down && !off);
            let ctrl = match it.control() {
                Control::Switch => {
                    let t = an.switch(*it as u64, it.is_on(cfg));
                    let s = wd::switch_rect(right, cy);
                    wd::switch(p, &s, t, st, lens);
                    s
                }
                Control::Choice => {
                    let labels = it.choices(ctx.st.screens);
                    let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
                    let sw = used[i];
                    let s = Rect::new(right - sw, cy - ctl::SEG_H * 0.5, sw, ctl::SEG_H);
                    let sel = an.segment(1000 + *it as u64, it.choice(cfg, ctx.st.screens));
                    let hot_i = st.hot.then(|| wd::segment_at(&s, refs.len(), ui.cursor.0, ui.cursor.1)).flatten();
                    wd::segmented(p, &s, &refs, sel, hot_i, st);
                    s
                }
                Control::Slider { steps, ticks } => {
                    let s = wd::slider_rect(right - win::SLIDER_W, cy, win::SLIDER_W);
                    let v = it.step(cfg) as f32 / (steps.max(2) - 1) as f32;
                    wd::slider(p, &s, v, ticks.then_some(steps), st, lens);
                    // 值寫在滑桿左邊，靠右對齊
                    let val = it.value(cfg, false, "", "", "");
                    let (tw, _) = p.text.measure_weight(&val, ty::CAPTION.size, ty::CAPTION.line, None, 500);
                    let a = if off { ctl::DISABLED_A } else { 1.0 };
                    p.text.push(&val, s.x - 14.0 - tw, text_top(cy, ty::CAPTION.line), ty::CAPTION.size,
                                ty::CAPTION.line, theme::ink(0.62 * a), 500, None, None);
                    s
                }
                Control::Button => {
                    let b = row_button(p, *it, ctx);
                    let bw = used[i];
                    let s = Rect::new(right - bw, cy - ctl::BTN_H * 0.5, bw, ctl::BTN_H);
                    let st = State { disabled: off || !b.enabled, ..st };
                    wd::button(p, &s, &b.label, b.kind, st);
                    s
                }
            };
            let max_w = if stacked[i] { r.w - ctl::ROW_PAD * 2.0 } else { label_w(used[i]) };
            wd::row_label(p, &label_r, it.label(), hint, max_w, State { disabled: off, ..Default::default() });
            if !it.badge().is_empty() {
                wd::row_badge(p, &label_r, it.label(), has_hint, it.badge());
            }
            lay.hits.push((hit, ctrl));
            lay.rows.push((*it, *r));
        }
        y = g.y + g.h + win::GROUP_GAP;
    }
    lay.content_h = y - top - win::GROUP_GAP + win::PAD;
}

/// 按鈕列的按鈕。字型那兩列寫目前的字型（太長的在按鈕裡捲）
fn row_button(p: &mut Paint, it: Item, ctx: &Ctx) -> Btn {
    let cfg = ctx.cfg;
    match it {
        Item::FontCjk => Btn::new(&p.text.family_name(&cfg.font_cjk), Kind::Normal),
        Item::FontLatin if cfg.font_latin.is_empty() => Btn::new(SAME_AS_CJK, Kind::Normal),
        Item::FontLatin => Btn::new(&p.text.family_name(&cfg.font_latin), Kind::Normal),
        _ => ctx.button(it),
    }
}

/// 列右邊控件的寬。排版（要不要排到下一行）與畫的時候用同一個值
fn ctrl_width(p: &mut Paint, it: Item, ctx: &Ctx) -> f32 {
    match it.control() {
        Control::Switch => ctl::SWITCH_W,
        Control::Choice => {
            let labels = it.choices(ctx.st.screens);
            let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            wd::segmented_width(p.text, &refs)
        }
        Control::Slider { .. } => win::SLIDER_W + win::VALUE_W,
        Control::Button => {
            let b = row_button(p, it, ctx);
            wd::button_width(p.text, &b.label).min(win::FONT_BTN_MAX)
        }
    }
}

/// 字型清單的一組：(標題, [(家族名, 顯示名稱)])
type FontGroup<'a> = (&'a str, Vec<(String, String)>);

/// 英文字型清單第一項，與英文字型沒指定時按鈕上寫的字
const SAME_AS_CJK: &str = "跟中文字型一樣";

/// 預覽框的三行字：標題級（粗）、內文級、說明級
const PREVIEW: [(&str, theme::Type, u16, f32); 3] = [
    ("一場遊戲一場夢 Kind of Blue", ty::TITLE, 600, 0.95),
    ("那只是昨夜的一場夢境 That's just the way it is", ty::BODY_S, 400, 0.85),
    ("0123456789 ABCDEFG abcdefg，。「」() ?!", ty::CAPTION, 400, 0.55),
];

/// 預覽框的高度。行高跟著字級倍率放大
fn preview_h(text: &Text) -> f32 {
    let k = text.font_scale();
    let lines: f32 = PREVIEW.iter().map(|(_, t, _, _)| t.line * k).sum();
    win::PREVIEW_PAD * 2.0 + lines + win::PREVIEW_LINE_GAP * (PREVIEW.len() - 1) as f32
}

/// 預覽框：用現在的字型與字級畫幾行中英混排的字，改設定立刻看得到。
/// `fonts` 給了就改用那組（英文字型, 中文字型）畫（字型清單裡滑鼠停著的那個）。太長的字裁掉
fn preview(p: &mut Paint, r: &Rect, fonts: Option<(&str, &str)>) {
    p.quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(theme::radius::GROUP).color(theme::ink(ctl::GROUP_BG_A)));
    let k = p.text.font_scale();
    let clip = TextBounds {
        left: (r.x + win::PREVIEW_PAD) as i32,
        top: r.y as i32,
        right: (r.x + r.w - win::PREVIEW_PAD) as i32,
        bottom: (r.y + r.h) as i32,
    };
    let x = r.x + win::PREVIEW_PAD;
    let mut y = r.y + win::PREVIEW_PAD;
    for (s, t, weight, a) in PREVIEW {
        // `push` 把放大的行高往上挪了一半（中線留在原處），這裡要的是頂端對齊，先往下補回來
        let top = y + t.line * (k - 1.0) * 0.5;
        match fonts {
            Some(f) => p.text.push_fonts(f, s, x, top, t.size, t.line, theme::ink(a), weight, Some(clip)),
            None => p.text.push(s, x, top, t.size, t.line, theme::ink(a), weight, None, Some(clip)),
        }
        y += t.line * k + win::PREVIEW_LINE_GAP;
    }
}

/// 字型清單：頂端固定的返回鈕＋標題＋預覽框，下面是捲動的清單，
/// 每個名字用它自己的字型畫，選中的那個右邊打勾。滑鼠停在哪個字型，預覽框就先換成它
#[allow(clippy::too_many_arguments)]
fn font_list(p: &mut Paint, w: f32, h: f32, top: f32, ui: &Ui, cfg: &Config, slot: FontSlot, lay: &mut Layout) {
    use crate::text::DEFAULT_FONT;
    let (gx, gw) = column(w);
    let cjk_now = if cfg.font_cjk.is_empty() { DEFAULT_FONT } else { cfg.font_cjk.as_str() };
    let fams = p.text.families().to_vec();

    // 清單的分組：(標題, [(家族名, 顯示名稱)])。英文字型的空字串是「跟中文字型一樣」
    let pick = |f: fn(&crate::text::FontFamily) -> bool| -> Vec<(String, String)> {
        fams.iter().filter(|x| f(x)).map(|x| (x.key.clone(), x.name.clone())).collect()
    };
    let (cur, groups): (&str, Vec<FontGroup>) = match slot {
        FontSlot::Cjk => (cjk_now, vec![("", pick(|f| f.cjk))]),
        FontSlot::Latin => (
            cfg.font_latin.as_str(),
            vec![
                ("", vec![(String::new(), SAME_AS_CJK.to_string())]),
                ("英文字型", pick(|f| f.latin && !f.cjk)),
                ("中文字型（也有英文字）", pick(|f| f.latin && f.cjk)),
            ],
        ),
    };

    // ---- 頂端固定的一塊：返回鈕、標題、預覽框
    let head_cy = win::PAD + win::HEAD_TILE * 0.5;
    let pr = Rect::new(gx, win::PAD + win::HEAD_H, gw, preview_h(p.text));
    lay.sticky_h = pr.y + pr.h + win::PREVIEW_GAP;

    // ---- 清單
    let hover = match ui.hot {
        Some(Hit::FontRow(i)) => Some(i),
        _ => None,
    };
    let clip_top = lay.sticky_h;
    let clip = TextBounds { left: i32::MIN / 2, top: clip_top as i32, right: i32::MAX / 2, bottom: i32::MAX / 2 };
    let mut y = top + lay.sticky_h;
    let mut hover_key: Option<String> = None;
    for (title, rows_of) in &groups {
        if rows_of.is_empty() {
            continue;
        }
        if !title.is_empty() {
            y += wd::group_title_h();
        }
        let (g, rows) = wd::group_rows(gx, y, gw, &vec![wd::row_h(false); rows_of.len()]);
        // 分組標題沒有裁切框，捲到頂端那塊底下就不畫
        if !title.is_empty() && g.y - wd::group_title_h() >= clip_top {
            wd::group_title(p, &g, title);
        }
        wd::group(p, &g, &rows);
        for ((key, name), r) in rows_of.iter().zip(&rows) {
            let i = lay.fonts.len();
            lay.fonts.push(key.clone());
            let on = key == cur;
            if on {
                lay.font_sel = Some(*r);
            }
            if hover == Some(i) {
                hover_key = Some(key.clone());
            }
            if r.y + r.h < clip_top || r.y > h {
                continue;
            }
            // 被頂端那塊蓋住的部分點不到
            let y0 = r.y.max(clip_top);
            let hit = Hit::FontRow(i);
            lay.hits.push((hit, Rect::new(r.x, y0, r.w, r.y + r.h - y0)));
            if ui.hot == Some(hit) {
                p.quads.push(Quad::new(r.x + 4.0, r.y + 2.0, r.w - 8.0, r.h - 4.0)
                    .radius(theme::radius::concentric(theme::radius::GROUP, 4.0))
                    .color(theme::ink(ctl::SIDE_HOT_A)));
            }
            let (right, cy) = wd::row_ctrl(r);
            if on {
                p.quads.svg(p.gpu, Svg::Check, ctl::CHECK, right - ctl::CHECK * 0.5, cy, theme::accent());
            }
            let is_default = match slot {
                FontSlot::Cjk => key == DEFAULT_FONT,
                FontSlot::Latin => key.is_empty(),
            };
            // 名字右邊留給「預設」與勾
            let mut name_right = right - ctl::CHECK - 12.0;
            if is_default {
                let tag = "預設";
                let (tw, _) = p.text.measure(tag, ty::CAPTION.size, ty::CAPTION.line, None);
                name_right -= tw + 12.0;
                p.text.push(tag, right - ctl::CHECK - 12.0 - tw, text_top(cy, ty::CAPTION.line), ty::CAPTION.size,
                            ty::CAPTION.line, theme::ink(0.5), 400, None, Some(clip));
            }
            // 名字用它自己的字型畫；「跟中文字型一樣」用現在的中文字型。太長的來回捲
            let own = if key.is_empty() { cjk_now } else { key.as_str() };
            let x = r.x + ctl::ROW_PAD;
            wd::marquee(p, name, x, text_top(cy, ty::BODY_S.line), name_right - x, ty::BODY_S, theme::ink(0.92), 400,
                        Some((own, own)), Some(clip));
        }
        y = g.y + g.h + win::GROUP_GAP;
    }
    lay.content_h = y - top - win::GROUP_GAP + win::PAD;

    // ---- 頂端那塊畫在清單上面：先鋪底色蓋掉捲上去的列（字另外用裁切框擋）
    let left = win::SIDE_INSET + win::SIDE_W;
    p.quads.push(Quad::new(left, 0.0, w - left, lay.sticky_h).color(theme::window_bg()));
    let back = Rect::new(gx, head_cy - ctl::BTN_H * 0.5, ctl::BTN_H, ctl::BTN_H);
    let st = State { hot: ui.hot == Some(Hit::Back), down: ui.down == Some(Hit::Back) && ui.hot == Some(Hit::Back), ..Default::default() };
    wd::icon_button(p, &back, Svg::ChevronLeft, st);
    lay.hits.push((Hit::Back, back));
    p.text.push(slot.item().label(), back.x + back.w + 12.0, text_top(head_cy, ty::TITLE.line), ty::TITLE.size,
                ty::TITLE.line, theme::fg(), 600, None, None);
    // 滑鼠停著的字型先預覽，還沒套用
    let latin_now = if cfg.font_latin.is_empty() { None } else { Some(cfg.font_latin.as_str()) };
    let fonts = hover_key.as_deref().map(|k| match slot {
        FontSlot::Cjk => (latin_now.unwrap_or(k), k),
        FontSlot::Latin => (if k.is_empty() { cjk_now } else { k }, cjk_now),
    });
    preview(p, &pr, fonts);
}

/// 從 `x0` 到 `x1`、貼著 `edge` 這條水平線往視窗裡淡進底色（`top` = 上緣往下淡）。
/// 用一塊放在視窗外面、只有投影的方塊做漸層：投影在方塊邊上是一半、往外 `blur`
/// 降到零，所以方塊的邊放在視窗裡 `blur` 處，視窗邊上就是全蓋，往內 2×`blur` 剛好淡完。
/// 左邊也一樣淡進來，側欄旁邊才不會多出一條硬邊
fn edge_fade(p: &mut Paint, x0: f32, x1: f32, edge: f32, k: f32, top: bool) {
    if k <= 0.0 {
        return;
    }
    let b = win::EDGE_FADE * 0.5;
    let far = 400.0;
    let (y, hh) = if top { (edge - far, far + b) } else { (edge - b, far + b) };
    let bg = theme::window_bg();
    p.quads.push(
        Quad::new(x0 + b, y, x1 - x0 + far, hh)
            .color([0.0; 4])
            .shadow(0.0, 0.0, b, 0.0, [bg[0], bg[1], bg[2], k]),
    );
}

pub struct Prefs {
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    blit: Blit,
    target: Option<Offscreen>,
    ui: Ui,
    anims: Anims,
    layout: Layout,
    mods: ModifiersState,
    /// 正在拖的滑桿
    drag: Option<Item>,
    pub dirty: bool,
    last_draw: Option<Instant>,
    /// 建窗的時間。視窗等第一幀送出才顯示，送不出去時靠它兜底（見 `ensure_shown`）
    opened: Instant,
    /// 視窗有焦點（放不下的字只在這時捲）
    focused: bool,
    /// 捲動字的時鐘從哪裡算起：換頁、開關字型清單、重新取得焦點時歸零，字從開頭捲起
    roll_from: Instant,
    /// 上一幀有字在捲，要繼續重畫
    rolling: bool,
}

impl Prefs {
    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    /// 開視窗，擺在 `near`（主視窗）的正中間
    pub fn open(
        el: &ActiveEventLoop,
        gpu: &Gpu,
        instance: &wgpu::Instance,
        adapter: &wgpu::Adapter,
        near: Option<&Window>,
        icon: Option<winit::window::Icon>,
    ) -> anyhow::Result<Self> {
        // 介面放大時視窗也跟著放大，內容才不會擠在原本的大小裡
        let z = gfx::ui_zoom();
        let mut attrs = Window::default_attributes()
            .with_title("VitroDeck 設定")
            .with_window_icon(icon)
            .with_inner_size(LogicalSize::new(win::W * z, win::H * z))
            .with_min_inner_size(LogicalSize::new(win::MIN_W * z, win::MIN_H * z))
            .with_visible(false);
        if let Some(m) = near.filter(|m| !m.is_minimized().unwrap_or(false))
            && let Ok(pos) = m.outer_position()
        {
            let (sz, k) = (m.outer_size(), m.scale_factor());
            let (ww, wh) = ((win::W as f64 * z as f64 * k) as i32, (win::H as f64 * z as f64 * k) as i32);
            attrs = attrs.with_position(PhysicalPosition::new(
                pos.x + (sz.width as i32 - ww) / 2,
                pos.y + (sz.height as i32 - wh) / 2,
            ));
        }
        let window = Arc::new(el.create_window(attrs)?);
        let surface = instance.create_surface(window.clone())?;
        let sz = window.inner_size();
        let mut config = surface
            .get_default_config(adapter, sz.width.max(1), sz.height.max(1))
            .ok_or_else(|| anyhow::anyhow!("surface 沒有可用設定"))?;
        // 跟主視窗同一個理由：離屏裡已經是編碼好的 sRGB，交換鏈不能再編一次
        let plain = config.format.remove_srgb_suffix();
        if surface.get_capabilities(adapter).formats.contains(&plain) {
            config.format = plain;
        }
        // 不等垂直同步：主視窗那條已經在等了，這裡再等一次會把主視窗的幀率拖成一半
        config.present_mode = wgpu::PresentMode::AutoNoVsync;
        surface.configure(&gpu.device, &config);
        let blit = Blit::new(gpu, config.format);
        Ok(Self {
            window,
            surface,
            config,
            blit,
            target: None,
            ui: Ui::default(),
            anims: Anims::default(),
            layout: Layout::default(),
            mods: ModifiersState::empty(),
            drag: None,
            dirty: true,
            last_draw: None,
            opened: Instant::now(),
            focused: true,
            roll_from: Instant::now(),
            rolling: false,
        })
    }

    /// 第一幀一直送不出去的話，視窗就永遠是隱藏的（實機遇過，主視窗關掉才出現，
    /// 原因沒抓到）。建窗超過一秒還沒顯示就直接顯示，內容等下一幀補上
    pub fn ensure_shown(&self) {
        if self.opened.elapsed() > SHOW_DEADLINE && !self.window.is_visible().unwrap_or(true) {
            crate::config::diag("設定視窗一秒內沒送出第一幀，先顯示");
            self.window.set_visible(true);
        }
    }

    /// 已經開著又被叫一次：拉到前面
    pub fn raise(&self) {
        if !self.window.is_visible().unwrap_or(true) {
            self.window.set_visible(true);
        }
        if self.window.is_minimized().unwrap_or(false) {
            self.window.set_minimized(false);
        }
        self.window.focus_window();
    }

    pub fn due(&self) -> bool {
        Instant::now() >= self.next_due()
    }

    pub fn next_due(&self) -> Instant {
        let Some(last) = self.last_draw else { return Instant::now() };
        if self.dirty || self.rolling || self.anims.moving() {
            return last + FRAME;
        }
        last + REFRESH
    }

    fn page(&self) -> Page {
        PAGES[self.ui.page.min(PAGES.len() - 1)]
    }

    fn set_page(&mut self, i: usize) {
        self.close_fonts();
        if i != self.ui.page {
            self.ui.page = i.min(PAGES.len() - 1);
            self.ui.focus = None;
            self.drag = None;
            self.ui.down = None;
            self.dirty = true;
            self.roll_from = Instant::now();
        }
    }

    fn scroll_by(&mut self, dy: f32) {
        let max = self.layout.max_scroll();
        let s = self.ui.scroll_mut();
        let n = (*s + dy).clamp(0.0, max);
        if n != *s {
            *s = n;
            self.dirty = true;
        }
    }

    /// 打開字型清單，第一幀把選中的字型捲到中間
    fn open_fonts(&mut self, slot: FontSlot) {
        self.ui.fonts = Some(slot);
        self.ui.fonts_scroll = 0.0;
        self.ui.fonts_reveal = true;
        self.ui.focus = None;
        self.ui.hot = None;
        self.dirty = true;
        self.roll_from = Instant::now();
    }

    fn close_fonts(&mut self) {
        if let Some(slot) = self.ui.fonts.take() {
            self.ui.hot = None;
            self.ui.focus = Some(slot.item());
            self.dirty = true;
            self.roll_from = Instant::now();
        }
    }

    /// 這個視窗的縮放：系統縮放 × 介面倍率，放不下最小版面（螢幕太小撐不開視窗）時降低倍率
    fn scale(&self) -> f32 {
        let sz = self.window.inner_size();
        gfx::zoomed_fit(self.window.scale_factor(), (sz.width, sz.height), (win::MIN_W, win::MIN_H))
    }

    /// 正在拖的滑桿
    pub fn dragging(&self) -> Option<Item> {
        self.drag
    }

    /// 介面倍率從 `old` 換成 `new`：視窗大小與最小大小跟著等比例縮放
    pub fn rezoom(&self, old: f32, new: f32) {
        let k = self.window.scale_factor();
        let r = (new / old.max(0.1)) as f64;
        let sz = self.window.inner_size().to_logical::<f64>(k);
        self.window.set_min_inner_size(Some(LogicalSize::new(win::MIN_W * new, win::MIN_H * new)));
        let _ = self.window.request_inner_size(LogicalSize::new(sz.width * r, sz.height * r));
        self.window.request_redraw();
    }

    /// 鍵盤把焦點移到的那列要捲進畫面
    fn reveal(&mut self, it: Item) {
        let Some((_, r)) = self.layout.rows.iter().find(|(k, _)| *k == it) else { return };
        let margin = 12.0;
        if r.y < margin {
            self.scroll_by(r.y - margin);
        } else if r.y + r.h > self.layout.view_h - margin {
            self.scroll_by(r.y + r.h - self.layout.view_h + margin);
        }
    }

    /// 滑桿上 `x` 對到的格
    fn slider_step(&self, it: Item, x: f32) -> Option<usize> {
        let Control::Slider { steps, .. } = it.control() else { return None };
        let r = self.layout.rect(Hit::Ctrl(it))?;
        Some(wd::slider_step(wd::slider_at(&r, x, Some(steps)), steps))
    }

    /// 處理一個事件。`screens` 給分段控制算選項數
    pub fn event(&mut self, ev: &WindowEvent, cfg: &Config, screens: usize) -> Option<Action> {
        let k = self.scale();
        match ev {
            WindowEvent::CloseRequested => return Some(Action::Close),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } | WindowEvent::RedrawRequested => {
                self.dirty = true
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::Focused(f) => {
                self.focused = *f;
                self.roll_from = Instant::now();
                self.dirty = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.ui.cursor = (position.x as f32 / k, position.y as f32 / k);
                if let Some(it) = self.drag {
                    self.dirty = true;
                    if let Some(i) = self.slider_step(it, self.ui.cursor.0)
                        && i != it.step(cfg)
                    {
                        return Some(Action::Edit(it, Edit::Step(i)));
                    }
                    return None;
                }
                let hot = self.layout.hit(self.ui.cursor.0, self.ui.cursor.1);
                // 分段控制裡換了被指著的那段也要重畫
                if hot != self.ui.hot || matches!(hot, Some(Hit::Ctrl(it)) if it.control() == Control::Choice) {
                    self.dirty = true;
                }
                self.ui.hot = hot;
                let live = match hot {
                    Some(Hit::Ctrl(it)) => !it.disabled(cfg),
                    Some(Hit::Page(_) | Hit::Back | Hit::FontRow(_)) => true,
                    None => false,
                };
                self.window.set_cursor(if live { CursorIcon::Pointer } else { CursorIcon::Default });
            }
            WindowEvent::CursorLeft { .. } if self.drag.is_none() => {
                self.ui.hot = None;
                self.dirty = true;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.ui.focus_visible = false;
                self.dirty = true;
                let (x, y) = self.ui.cursor;
                match self.layout.hit(x, y) {
                    Some(Hit::Page(i)) => self.set_page(i),
                    // 返回鈕與字型清單的列放開時才算
                    Some(h @ (Hit::Back | Hit::FontRow(_))) => self.ui.down = Some(h),
                    Some(Hit::Ctrl(it)) if !it.disabled(cfg) => {
                        self.ui.focus = Some(it);
                        match it.control() {
                            Control::Choice => {
                                let r = self.layout.rect(Hit::Ctrl(it))?;
                                let i = wd::segment_at(&r, it.choices(screens).len(), x, y)?;
                                return Some(Action::Edit(it, Edit::Choice(i)));
                            }
                            Control::Slider { .. } => {
                                self.drag = Some(it);
                                self.ui.down = Some(Hit::Ctrl(it));
                                let i = self.slider_step(it, x)?;
                                return (i != it.step(cfg)).then_some(Action::Edit(it, Edit::Step(i)));
                            }
                            // 開關與按鈕放開時才算（按下後滑出去再放開＝取消）
                            Control::Switch | Control::Button => self.ui.down = Some(Hit::Ctrl(it)),
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.dirty = true;
                if let Some(it) = self.drag.take() {
                    self.ui.down = None;
                    self.ui.hot = self.layout.hit(self.ui.cursor.0, self.ui.cursor.1);
                    return Some(Action::DragEnd(it));
                }
                let down = self.ui.down.take();
                let now = self.layout.hit(self.ui.cursor.0, self.ui.cursor.1);
                if down != now {
                    return None;
                }
                match down {
                    // 字型那列的按鈕在這個視窗裡開子頁，不用交給主程式
                    Some(Hit::Ctrl(Item::FontCjk)) => self.open_fonts(FontSlot::Cjk),
                    Some(Hit::Ctrl(Item::FontLatin)) => self.open_fonts(FontSlot::Latin),
                    Some(Hit::Ctrl(it)) => return Some(Action::Edit(it, Edit::Press)),
                    Some(Hit::Back) => self.close_fonts(),
                    Some(Hit::FontRow(i)) => {
                        let slot = self.ui.fonts?;
                        return self.layout.fonts.get(i).cloned().map(|k| Action::Font(slot, k));
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 48.0,
                    MouseScrollDelta::PixelDelta(p) => -(p.y as f32) / k,
                };
                self.scroll_by(dy);
                self.ui.hot = self.layout.hit(self.ui.cursor.0, self.ui.cursor.1);
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                return self.key(&event.logical_key, cfg, screens);
            }
            _ => {}
        }
        None
    }

    /// 鍵盤：上下（或 Tab）移焦點、左右調值、Enter／空白鍵按下去、
    /// Ctrl+Tab 或 PageUp／PageDown 換頁、Esc 或 Ctrl+W 關掉
    fn key(&mut self, key: &Key, cfg: &Config, screens: usize) -> Option<Action> {
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let n = PAGES.len();
        let page_step = |s: &mut Self, d: isize| {
            let i = (s.ui.page as isize + d).rem_euclid(n as isize) as usize;
            s.set_page(i);
        };
        if self.ui.fonts.is_some() {
            match key {
                Key::Named(NamedKey::Escape | NamedKey::Backspace) => {
                    self.close_fonts();
                    self.ui.focus_visible = true;
                    return None;
                }
                Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp) => {
                    let d = if matches!(key, Key::Named(NamedKey::ArrowDown)) { 1.0 } else { -1.0 };
                    self.scroll_by(d * ctl::ROW_H);
                    return None;
                }
                // 底下那頁的焦點看不到，別讓它跟著動
                Key::Named(NamedKey::Tab | NamedKey::Enter | NamedKey::Space | NamedKey::ArrowLeft | NamedKey::ArrowRight)
                    if !ctrl =>
                {
                    return None;
                }
                _ => {}
            }
        }
        match key {
            Key::Named(NamedKey::Escape) => return Some(Action::Close),
            Key::Character(c) if ctrl && c.as_str().eq_ignore_ascii_case("w") => return Some(Action::Close),
            Key::Named(NamedKey::Tab) if ctrl => page_step(self, if shift { -1 } else { 1 }),
            Key::Named(NamedKey::PageDown) => page_step(self, 1),
            Key::Named(NamedKey::PageUp) => page_step(self, -1),
            Key::Named(NamedKey::ArrowDown) => self.move_focus(cfg, 1),
            Key::Named(NamedKey::ArrowUp) => self.move_focus(cfg, -1),
            Key::Named(NamedKey::Tab) => self.move_focus(cfg, if shift { -1 } else { 1 }),
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight) => {
                let d = if matches!(key, Key::Named(NamedKey::ArrowLeft)) { -1 } else { 1 };
                let it = self.ui.focus.filter(|it| !it.disabled(cfg))?;
                self.ui.focus_visible = true;
                self.dirty = true;
                return match it.control() {
                    // 分段與滑桿到底就停，不繞回去（跟滑鼠拖到底一樣）
                    Control::Choice => {
                        let m = it.choices(screens).len() as isize;
                        let i = (it.choice(cfg, screens) as isize + d).clamp(0, m - 1) as usize;
                        Some(Action::Edit(it, Edit::Choice(i)))
                    }
                    Control::Slider { steps, .. } => {
                        let i = (it.step(cfg) as isize + d).clamp(0, steps as isize - 1) as usize;
                        Some(Action::Edit(it, Edit::Step(i)))
                    }
                    Control::Switch => (it.is_on(cfg) != (d > 0)).then_some(Action::Edit(it, Edit::Press)),
                    Control::Button => None,
                };
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                let it = self.ui.focus.filter(|it| !it.disabled(cfg))?;
                self.ui.focus_visible = true;
                self.dirty = true;
                if it == Item::FontCjk || it == Item::FontLatin {
                    self.open_fonts(if it == Item::FontCjk { FontSlot::Cjk } else { FontSlot::Latin });
                } else if matches!(it.control(), Control::Switch | Control::Button) {
                    return Some(Action::Edit(it, Edit::Press));
                }
            }
            _ => {}
        }
        None
    }

    /// 焦點往上或往下一項，跳過停用的
    fn move_focus(&mut self, cfg: &Config, d: isize) {
        let items: Vec<Item> = self.page().items().into_iter().filter(|it| !it.disabled(cfg)).collect();
        if items.is_empty() {
            return;
        }
        let i = match self.ui.focus.and_then(|f| items.iter().position(|x| *x == f)) {
            Some(i) => (i as isize + d).rem_euclid(items.len() as isize) as usize,
            None if d > 0 => 0,
            None => items.len() - 1,
        };
        self.ui.focus = Some(items[i]);
        self.ui.focus_visible = true;
        self.dirty = true;
        self.reveal(items[i]);
    }

    /// 畫一幀並送上螢幕。最小化時什麼都不做
    pub fn draw(&mut self, gpu: &Gpu, quads: &mut QuadRenderer, text: &mut Text, ctx: &Ctx) -> anyhow::Result<()> {
        use wgpu::CurrentSurfaceTexture as Cst;
        self.last_draw = Some(Instant::now());
        self.dirty = false;
        let sz = self.window.inner_size();
        if sz.width == 0 || sz.height == 0 {
            return Ok(());
        }
        let (w, h) = (sz.width, sz.height);
        let k = self.scale();
        let _scale = gfx::scoped_scale(k);
        if self.target.as_ref().is_none_or(|t| t.width != w || t.height != h) {
            self.target = Some(Offscreen::new(&gpu.device, w, h));
        }
        let target = self.target.as_ref().unwrap();
        let (lw, lh) = gfx::logical(w, h);

        quads.begin(gpu);
        text.begin();
        self.layout = {
            let mut p = Paint::new(gpu, &mut *quads, &mut *text);
            // 放不下的字只在視窗有焦點時捲，沒焦點就停在開頭、不必一直重畫
            p.clock = self.focused.then(|| self.roll_from.elapsed().as_secs_f32());
            let lay = paint(&mut p, lw, lh, &self.ui, &mut self.anims, ctx);
            self.rolling = p.rolling;
            lay
        };
        if settle(&mut self.ui, &self.layout) {
            self.dirty = true;
        }
        text.prepare(gpu, w, h)?;
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        {
            let bg = theme::window_bg();
            let clear = wgpu::Color { r: bg[0] as f64, g: bg[1] as f64, b: bg[2] as f64, a: 1.0 };
            let mut pass = gfx::pass(&mut enc, &target.view, "prefs", Some(clear));
            quads.flush(gpu, &mut pass, w, h);
            text.render(&mut pass)?;
        }
        quads.upload(gpu, w, h);

        if self.config.width != w || self.config.height != h {
            self.config.width = w;
            self.config.height = h;
            self.surface.configure(&gpu.device, &self.config);
        }
        let frame = match self.surface.get_current_texture() {
            Cst::Success(f) | Cst::Suboptimal(f) => f,
            st @ (Cst::Timeout | Cst::Occluded | Cst::Validation) => {
                self.note_no_frame(match st {
                    Cst::Timeout => "timeout",
                    Cst::Occluded => "occluded",
                    _ => "validation",
                });
                gpu.queue.submit([enc.finish()]);
                return Ok(());
            }
            Cst::Outdated | Cst::Lost => {
                self.note_no_frame("outdated/lost");
                gpu.queue.submit([enc.finish()]);
                self.surface.configure(&gpu.device, &self.config);
                self.dirty = true;
                return Ok(());
            }
        };
        let view = frame.texture.create_view(&Default::default());
        {
            let mut pass = gfx::pass(&mut enc, &view, "prefs-blit", Some(wgpu::Color::BLACK));
            self.blit.draw(gpu, &mut pass, &target.view);
        }
        gpu.queue.submit([enc.finish()]);
        gpu.queue.present(frame);
        if !self.window.is_visible().unwrap_or(true) {
            self.window.set_visible(true);
        }
        Ok(())
    }

    /// 還沒顯示時拿不到畫布，記下原因（上面 `ensure_shown` 那個問題的線索）
    fn note_no_frame(&self, why: &str) {
        if !self.window.is_visible().unwrap_or(true) {
            crate::config::diag(&format!("設定視窗第一幀拿不到畫布：{why}"));
        }
    }
}

/// 畫完一幀之後調捲動量：視窗變高、內容變短之後可能超過新的上限；
/// 剛打開字型清單的話把選中的字型捲到中間。有改就回 true（要再畫一次）
fn settle(ui: &mut Ui, lay: &Layout) -> bool {
    let max = lay.max_scroll();
    let mut s = ui.scroll().min(max);
    if ui.fonts.is_some() && ui.fonts_reveal {
        ui.fonts_reveal = false;
        // 捲到頂端那塊下面的清單區的中間
        if let Some(r) = lay.font_sel {
            s = (s + r.cy() - (lay.sticky_h + lay.view_h) * 0.5).clamp(0.0, max);
        }
    }
    let changed = s != ui.scroll();
    *ui.scroll_mut() = s;
    changed
}

/// 截圖：把設定視窗畫進 `target`（大小就是視窗大小）。`ui` 決定哪一頁、捲到哪、指著什麼；
/// `roll` 是捲動字的時鐘（`None` 停在開頭）
pub fn shot(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    text: &mut Text,
    ctx: &Ctx,
    mut ui: Ui,
    roll: Option<f32>,
) -> anyhow::Result<()> {
    let (w, h) = (target.width, target.height);
    let (lw, lh) = gfx::logical(w, h);
    let mut an = Anims::default();
    quads.begin(gpu);
    text.begin();
    let lay = {
        let mut p = Paint::new(gpu, &mut *quads, &mut *text);
        p.clock = roll;
        paint(&mut p, lw, lh, &ui, &mut an, ctx)
    };
    // 捲動量超過上限（或字型清單要捲到選中那項）的話重畫一次
    if settle(&mut ui, &lay) {
        quads.begin(gpu);
        text.begin();
        let mut p = Paint::new(gpu, &mut *quads, &mut *text);
        p.clock = roll;
        paint(&mut p, lw, lh, &ui, &mut an, ctx);
    }
    text.prepare(gpu, w, h)?;
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    {
        let bg = theme::window_bg();
        let clear = wgpu::Color { r: bg[0] as f64, g: bg[1] as f64, b: bg[2] as f64, a: 1.0 };
        let mut pass = gfx::pass(&mut enc, &target.view, "prefs", Some(clear));
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    }
    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 內容欄在側欄右邊而且太寬就置中() {
        let (x, w) = column(win::W);
        assert!(x >= win::SIDE_INSET * 2.0 + win::SIDE_W + win::PAD - 0.01);
        assert!(x + w <= win::W - win::PAD + 0.01);
        let (x2, w2) = column(1600.0);
        assert_eq!(w2, win::MAX_W);
        let right_gap = 1600.0 - (x2 + w2);
        let left_gap = x2 - (win::SIDE_INSET * 2.0 + win::SIDE_W);
        assert!((right_gap - left_gap).abs() < 0.01, "置中：左 {left_gap} 右 {right_gap}");
        // 最小視窗也排得下
        let (_, w3) = column(win::MIN_W);
        assert!(w3 >= 340.0, "{w3}");
    }

    #[test]
    fn 每頁都有圖示而且顏色不重複() {
        let cols: Vec<[f32; 3]> = PAGES.iter().map(|p| page_icon(*p).1).collect();
        for (i, a) in cols.iter().enumerate() {
            for b in &cols[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
