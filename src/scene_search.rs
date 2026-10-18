//! 兩個打字用的疊層：封面流上方的搜尋膠囊，與視覺效果的 preset 選單。
//!
//! 都是疊在已經畫好的畫面上。跟設定面板一樣，每幀重新模糊一次底下的
//! 東西給玻璃取樣 —— 底下那頁照樣在動。

use std::collections::HashMap;

use glyphon::TextBounds;

use crate::gfx::{self, Gpu, Offscreen};
use crate::glass::{Glass, Panel};
use crate::quad::{Quad, QuadRenderer};
use crate::search::Input;
use crate::text::Text;
use crate::theme;
use crate::ui::{self, Hit, PICKER_CATS};
use crate::viz::Viz;

/// 在輸入框裡畫文字、組字中的底線與游標。回傳游標的 x（給輸入法定位候選窗）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_input(
    text: &mut Text,
    quads: &mut QuadRenderer,
    input: &Input,
    x: f32,
    cy: f32,
    w: f32,
    size: f32,
    placeholder: &str,
    caret_on: bool,
) -> f32 {
    let lh = size * 1.3;
    let y = cy - lh * 0.5;
    // 框只有這麼寬，打太長就只看得到尾巴 —— 游標永遠在最後面，
    // 所以往左推、讓最右邊那段露出來，跟瀏覽器的 <input> 一樣。
    let committed_w = text.measure(&input.text, size, lh, None).0;
    let pre_w = text.measure(&input.preedit, size, lh, None).0;
    let total = committed_w + pre_w;
    let shift = (total - (w - 4.0)).max(0.0);
    let bounds = TextBounds {
        left: x as i32,
        top: (y - 4.0) as i32,
        right: (x + w) as i32,
        bottom: (y + lh + 4.0) as i32,
    };
    let tx = x - shift;
    let empty = input.text.is_empty() && input.preedit.is_empty();
    if empty {
        // 提示字往右讓一點，游標才不會壓在第一個字上
        if !placeholder.is_empty() {
            text.push(placeholder, x + 4.0, y, size, lh, theme::ink(0.32), 400, None,
                      Some(bounds));
        }
    } else {
        text.push(&input.text, tx, y, size, lh, theme::ink(0.95), 400, None, Some(bounds));
        if input.composing() {
            // 組字中的那段：跟已送出的分開畫，下面拉一條線 ——
            // 這是所有輸入法共通的視覺語言，一看就知道「還沒打完」
            let px = tx + committed_w;
            text.push(&input.preedit, px, y, size, lh, theme::ink(0.8), 400, None,
                      Some(bounds));
            let ux = px.max(x);
            let uw = (px + pre_w).min(x + w) - ux;
            if uw > 0.0 {
                quads.push(Quad::new(ux, y + lh - 1.0, uw, 1.2).color(theme::ink(0.7)));
            }
        }
    }
    let caret_x = if empty { x - 1.0 } else { (tx + total).min(x + w - 2.0) };
    // 組字中不畫游標 —— 輸入法自己的候選窗就是游標
    if caret_on && !input.composing() {
        quads.push(Quad::new(caret_x + 1.0, y + 2.0, 1.5, lh - 4.0).color(theme::ink(0.85)));
    }
    caret_x
}

/// 封面流的搜尋膠囊。回傳游標的畫面座標。
#[allow(clippy::too_many_arguments)]
pub fn render_search(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    input: &Input,
    no_match: bool,
    cover_scale: f32,
    caret_on: bool,
    // 打開後幾秒（出現時的回彈）
    age: f32,
) -> anyhow::Result<(f32, f32)> {
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let l = ui::search(fw, fh);
    let (k, a) = crate::anim::glass_in(age);
    let pill = ui::Rect::new(
        l.pill.cx() - l.pill.w * k * 0.5,
        l.pill.cy() - l.pill.h * k * 0.5,
        l.pill.w * k,
        l.pill.h * k,
    );

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("search") });
    let backdrop = glass.blur_scene(gpu, &mut enc, target, theme::GLASS_SEARCH.sigma);
    let sharp = glass.sharp_scene(gpu, &mut enc, target);
    // 小元件：壓在很亮的東西上時翻成淺色玻璃，字跟著翻
    let tone = glass.tone("search", [l.pill.x, l.pill.y, l.pill.w, l.pill.h]);
    glass.begin();
    // `#search` 就是一顆 `.glass`，圓角 999
    glass.push(
        Panel::new(pill.x, pill.y, pill.w, pill.h)
            .radius(pill.h * 0.5)
            .style(&theme::GLASS_SEARCH)
            .materialize(a)
            .tone(tone),
    );

    quads.begin(gpu);
    text.begin();
    quads.set_fx(a, 0.0);
    text.set_fx(a, 0.0);
    let cy = l.pill.cy();
    let caret = {
        let _tone = theme::scoped_tone(tone);
        text.push("/", l.label_x, cy - 11.0, 17.0, 22.0, theme::ink(0.45), 400, None, None);
        draw_input(text, quads, input, l.text_x, cy, l.text_w, 16.0, "", caret_on)
    };

    quads.set_fx(1.0, 0.0);
    text.set_fx(1.0, 0.0);
    // 篩不到東西：畫在原本專輯名的位置，跟 web 版的 `#meta-title` 同一格
    if no_match {
        let f = ui::flow(fw, fh, cover_scale);
        let msg = "找不到符合的專輯";
        let (mw, _) = text.measure(msg, 30.0, 40.0, None);
        text.push(msg, f.cx - mw * 0.5, f.cy - 20.0, 30.0, 40.0,
                  theme::ink(0.55), 600, None, None);
    }
    text.prepare(gpu, w, h)?;
    {
        let mut pass = gfx::pass(&mut enc, &target.view, "search", None);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    }
    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok((caret, cy))
}

/// preset 選單要畫的東西。狀態本身在 main 那邊。
pub struct PickerView<'a> {
    /// 篩選後的清單（`Viz` 的 item 索引）
    pub rows: &'a [usize],
    /// 反白的那一列（`rows` 裡的位置）
    pub at: usize,
    /// 捲到的第一列
    pub top: usize,
    pub input: &'a Input,
    pub caret_on: bool,
}

/// 截斷過的名稱，依寬度快取。
///
/// `Text::fit` 是二分搜尋、每一步都重新排版一次，一列要量七八次。
/// 清單一次十幾列、一秒 165 幀，每幀重算會把一顆核心吃掉 ——
/// 名字不會變，算一次存起來就好。寬度換了（視窗縮放）才整個清掉。
///
/// 鍵要帶字重：同一個名字在反白（粗體）跟平常時截的位置不一樣。
/// 寬度也要帶 —— 「目前在跑」那列前面多一個箭頭，可用的寬度比較窄。
#[derive(Default)]
pub struct FitCache {
    map: HashMap<(String, u16, u32), String>,
}

impl FitCache {
    fn get(&mut self, text: &mut Text, s: &str, size: f32, lh: f32, w: f32, weight: u16) -> String {
        let key = (s.to_string(), weight, w.round() as u32);
        if let Some(v) = self.map.get(&key) {
            return v.clone();
        }
        // 視窗一直縮放的話鍵會一直長，長到一定程度就整個丟掉重來
        if self.map.len() > 4000 {
            self.map.clear();
        }
        let v = text.fit_weight(s, size, lh, w, weight);
        self.map.insert(key, v.clone());
        v
    }
}

/// preset 選單。回傳游標的畫面座標。
#[allow(clippy::too_many_arguments)]
pub fn render_picker(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    viz: &Viz,
    v: &PickerView,
    fit: &mut FitCache,
    hover: Option<Hit>,
) -> anyhow::Result<(f32, f32)> {
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let l = ui::picker(fw, fh);

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("picker") });

    // ---- 整片底：模糊 20 ＋ 壓暗 .34 ----
    // CSS 故意調淡過（`rgba(6,6,9,.34)`）—— 上下鍵是即時預覽，
    // 要看得到底下的 preset 換成什麼樣子。
    let behind = std::sync::Arc::new(glass.blur_scene(gpu, &mut enc, target, theme::SIGMA_BEHIND_PICKER));
    quads.begin(gpu);
    quads.push_tex(Quad::new(0.0, 0.0, fw, fh).textured().saturate(1.3), &behind);
    quads.push(Quad::new(0.0, 0.0, fw, fh).color([6.0 / 255.0, 6.0 / 255.0, 9.0 / 255.0, 0.34]));
    {
        let mut pass = gfx::pass(&mut enc, &target.view, "picker-dim", None);
        quads.flush(gpu, &mut pass, w, h);
    }

    // ---- 面板 ----
    // 底下是滿版的 MilkDrop，什麼顏色都可能出現，所以面板本身要近乎不透明
    // （CSS 是 .94）。玻璃的亮環與高光照樣有，只是不透出底下的顏色。
    let backdrop = glass.blur_scene(gpu, &mut enc, target, theme::GLASS_PICKER.sigma);
    let sharp = glass.sharp_scene(gpu, &mut enc, target);
    glass.begin();
    glass.push(
        Panel::new(l.panel.x, l.panel.y, l.panel.w, l.panel.h)
            .radius(theme::radius::SHEET)
            .style(&theme::GLASS_PICKER).tint(theme::sheet_tint()),
    );

    text.begin();
    // 標題。CSS 的 letter-spacing .24em 用細空白（U+2009）撐開
    let title = "P\u{2009}R\u{2009}E\u{2009}S\u{2009}E\u{2009}T";
    let (tw, _) = text.measure(title, 20.0, 26.0, None);
    text.push(title, l.panel.cx() - tw * 0.5, l.title_y, 20.0, 26.0,
              theme::ink(0.5), 300, None, None);

    // 搜尋框
    let s = l.search;
    quads.push(Quad::new(s.x, s.y, s.w, s.h).radius(s.h * 0.5).color(theme::field(0.45))
        .border(1.0, theme::ink(0.15)));
    text.push("/", s.x + 20.0, s.cy() - 10.5, 16.0, 21.0, theme::ink(0.4), 400, None, None);
    let caret = draw_input(text, quads, v.input, s.x + 38.0, s.cy(), s.w - 58.0, 15.0,
                           "輸入關鍵字篩選", v.caret_on);

    // 分類鈕
    for (i, (r, c)) in l.cats.iter().zip(PICKER_CATS.iter()).enumerate() {
        let on = *c == viz.cat;
        let hot = hover == Some(Hit::PickerCat(i));
        let (bg, bd, fg, weight) = if on {
            (theme::accent(), theme::accent(), theme::on_accent(), 600)
        } else if hot {
            (theme::ink(0.12), theme::ink(0.1), theme::ink(1.0), 400)
        } else {
            (theme::ink(0.06), theme::ink(0.1), theme::ink(0.62), 400)
        };
        quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(r.h * 0.5).color(bg).border(1.0, bd));
        let (lw, _) = text.measure(c.label(), 12.0, 16.0, None);
        text.push(c.label(), r.cx() - lw * 0.5, r.cy() - 8.0, 12.0, 16.0, fg, weight, None, None);
    }

    // 清單
    let cur = viz.current_item();
    let list_bounds = TextBounds {
        left: l.list.x as i32,
        top: l.list.y as i32,
        right: (l.list.x + l.list.w) as i32,
        bottom: (l.list.y + l.list.h) as i32,
    };
    // 捲動邊緣效果：上面（下面）還有東西的話，最靠邊的兩列淡進底色，
    // 看得出清單是接著捲出去的，不是到這裡就沒了。選到的那列不淡
    let (above, below) = (v.top > 0, v.top + l.rows < v.rows.len());
    for (i, &item) in v.rows.iter().enumerate().skip(v.top).take(l.rows) {
        let Some(r) = ui::picker_row(&l, v.top, i) else { continue };
        let active = i == v.at;
        let k = i - v.top;
        let edge = if active {
            1.0
        } else if (above && k == 0) || (below && k + 1 == l.rows) {
            theme::PICKER_EDGE[0]
        } else if (above && k == 1) || (below && k + 2 == l.rows) {
            theme::PICKER_EDGE[1]
        } else {
            1.0
        };
        text.set_fx(edge, 0.0);
        quads.set_fx(edge, 0.0);
        let hot = matches!(hover, Some(Hit::PickerRow(k)) | Some(Hit::PickerStar(k)) if k == i);
        if active {
            quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(theme::radius::CHIP).color(theme::accent()));
        } else if hot {
            quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(theme::radius::CHIP).color(theme::ink(0.09)));
        }
        let name = viz.item_name(item);
        let fav = viz.is_favourite_name(name);
        let ty = r.cy() - 9.0;
        let mut x = r.x + 12.0;
        // 目前正在跑的那顆，捲到別處也認得出來。
        // **自己畫三角形，不用「▸」這個字** —— 它在 Segoe UI 裡小得跟一個點一樣，
        // 而且這類幾何符號隨時可能被表情符號字型接走（播放鍵就踩過一次）。
        if cur == Some(item) {
            let c = if active { theme::on_accent() } else { theme::accent() };
            quads.push(Quad::new(x, r.cy() - 4.5, 7.0, 9.0).triangle(true, 1.0).color(c));
            x += 13.0;
        }
        let star_hot = hover == Some(Hit::PickerStar(i));
        let star_c = if star_hot {
            theme::warn(1.0)
        } else if active {
            { let o = theme::on_accent(); [o[0], o[1], o[2], 0.45] }
        } else {
            theme::ink(0.28)
        };
        text.push(if fav { "★" } else { "☆" }, x, ty, 13.5, 18.0, star_c, 400, None,
                  Some(list_bounds));
        x += 20.0;
        let fg = if active {
            theme::on_accent()
        } else if hot {
            theme::ink(1.0)
        } else {
            theme::ink(0.8)
        };
        let weight = if active { 600 } else { 400 };
        let shown = fit.get(text, name, 13.5, 18.0, r.x + r.w - 12.0 - x, weight);
        text.push(&shown, x, ty, 13.5, 18.0, fg, weight, None, Some(list_bounds));
    }
    text.set_fx(1.0, 0.0);
    quads.set_fx(1.0, 0.0);

    // 捲軸。清單比視窗長才畫
    let n = v.rows.len();
    if n > l.rows {
        let frac = l.rows as f32 / n as f32;
        let th = (l.list.h * frac).max(24.0);
        let ty = l.list.y + (l.list.h - th) * (v.top as f32 / (n - l.rows) as f32);
        quads.push(Quad::new(l.list.x + l.list.w + 6.0, ty, 3.0, th).radius(1.5)
            .color(theme::ink(0.26)));
    }

    let foot = if n == 0 {
        "沒有符合的 preset".to_string()
    } else {
        format!("{n} 個 · ↑ ↓ 預覽 · Enter 套用 · Esc 取消 · Tab 換分類")
    };
    let (fw_, _) = text.measure(&foot, 12.0, 16.0, None);
    text.push(&foot, l.panel.cx() - fw_ * 0.5, l.foot_y, 12.0, 16.0,
              theme::ink(0.45), 400, None, None);

    text.prepare(gpu, w, h)?;
    {
        let mut pass = gfx::pass(&mut enc, &target.view, "picker", None);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    }
    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok((caret, l.search.cy()))
}

