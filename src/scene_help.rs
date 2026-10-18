//! 快捷鍵一覽。原生視窗沒有選單列，不列出來沒有人會知道有這些鍵。
//!
//! 對應表跟 web 版一致（`app.js` 的 `onkeydown`），這樣兩邊的手感一樣。

use crate::gfx::{self, Gpu, Offscreen};
use crate::glass::{Glass, Panel};
use crate::quad::{Quad, QuadRenderer};
use crate::text::Text;
use crate::theme;
use crate::ui::Rect;

/// (鍵, 說明)。空字串的鍵當成分隔標題。
const KEYS: &[(&str, &str)] = &[
    ("", "切換"),
    ("Tab", "封面流 → 歌詞 → 視覺效果"),
    ("← →", "上一張／下一張專輯"),
    ("Esc Backspace", "從曲目回到封面流"),
    ("l", "封面流 ⇄ 歌詞"),
    ("PgUp PgDn", "一次跳十張"),
    ("Home End", "跳到頭／尾"),
    ("Enter / 點封面", "展開這張／播放這首；旁邊的：捲過去"),
    ("/", "搜尋（Esc 取消，Enter 播放）"),
    ("a–z", "跳到下一張以該字母開頭的"),
    ("", "播放"),
    ("空白鍵", "播放／暫停"),
    ("↑ ↓", "上一首／下一首（每一頁都是）"),
    ("x", "隨機播放"),
    ("e", "循環模式"),
    ("d", "選播放裝置"),
    ("", "畫面"),
    ("F11", "全螢幕 → 跨所有螢幕 → 視窗"),
    ("- = \\", "封面大小（縮小／放大／回到 100%）"),
    ("s", "專輯排序"),
    ("r", "重抓收藏清單"),
    ("k", "KTV 提詞（封面流頂端）"),
    ("v", "唱片模式（歌詞頁）"),
    ("t", "歌詞翻譯（歌詞頁）"),
    ("[ ]　0", "歌詞時間 ∓100ms／歸零"),
    ("", "視覺效果"),
    ("← →", "上一個／下一個 preset"),
    ("/ 或 Enter", "preset 選單（打字篩選、↑↓ 預覽）"),
    ("p", "鎖定目前這個，不自動換"),
    ("c", "切換分類"),
    ("h", "加入／移除我的最愛"),
    ("f", "影格上限"),
    ("", "其他"),
    (", ", "設定"),
    ("?", "這張表"),
    ("i", "關於／中斷 Spotify 連結"),
    ("Esc", "退回一步（不會關程式）"),
    ("q", "關閉程式"),
];

const ROW_H: f32 = 26.0;

/// 字級倍率放大時，整個面板（寬、列高、欄位、留白）乘同一個倍率，排版跟 100% 一模一樣，
/// 不會多出折行；視窗放不下就停在剛好放得下的倍率。`k` 是使用者設的字級倍率
fn fit_scale(fw: f32, fh: f32, k: f32) -> f32 {
    if k <= 1.0 {
        return k;
    }
    let rows = KEYS.len().div_ceil(2) as f32;
    let (pw, ph) = ((fw * 0.78).min(860.0), 64.0 + rows * ROW_H + 28.0);
    k.min(((fw - 40.0) / pw).min((fh - 40.0) / ph).max(1.0))
}

/// 面板的範圍。主程式也要知道（面板蓋到控制列時控制列先消散）
pub fn panel_rect(fw: f32, fh: f32) -> Rect {
    // 兩欄排。一欄塞不下 25 行，而且視窗高度不一定夠。
    let k = fit_scale(fw, fh, crate::text::pref_scale());
    let rows = KEYS.len().div_ceil(2);
    let pw = (fw * 0.78).min(860.0) * k;
    let ph = (64.0 + rows as f32 * ROW_H + 28.0) * k;
    Rect::new((fw - pw) * 0.5, (fh - ph) * 0.5, pw, ph.min(fh - 40.0))
}

pub fn render(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    viz_on: bool,
) -> anyhow::Result<()> {
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);

    let rows = KEYS.len().div_ceil(2);
    let k = fit_scale(fw, fh, crate::text::pref_scale());
    let row_h = ROW_H * k;
    let panel = panel_rect(fw, fh);
    let pw = panel.w;
    let col_w = (pw - 56.0 * k) * 0.5;

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("help") });

    quads.begin(gpu);
    quads.push(Quad::new(0.0, 0.0, fw, fh).color(theme::modal(0.46)));
    {
        let mut pass = gfx::pass(&mut enc, &target.view, "help-dim", None);
        quads.flush(gpu, &mut pass, w, h);
    }

    let backdrop = glass.blur_scene(gpu, &mut enc, target, theme::GLASS_PANEL.sigma);
    let sharp = glass.sharp_scene(gpu, &mut enc, target);
    glass.begin();
    glass.push(
        Panel::new(panel.x, panel.y, panel.w, panel.h)
            .radius(theme::radius::PANEL)
            .style(&theme::GLASS_PANEL),
    );

    text.begin();
    text.cap_scale(k);
    // 版面整個乘了 k，字要頂端對齊（`push` 預設是讓放大的字留在原本那行的中線）
    let top = |text: &Text, y: f32, line: f32| y + text.lift(line);
    let t = theme::ty::TITLE;
    text.push("快捷鍵", panel.x + theme::space::PANEL_X * k, top(text, panel.y + theme::space::PANEL_TITLE_Y * k, t.line),
              t.size, t.line, theme::ink(0.95), 600, None, None);
    for (i, (key, desc)) in KEYS.iter().enumerate() {
        // Tab 那列跟著視覺效果開關變
        let desc = if *key == "Tab" && !viz_on { "封面流 ⇄ 歌詞" } else { desc };
        let col = i / rows;
        let row = i % rows;
        let x = panel.x + 22.0 * k + col as f32 * (col_w + 12.0 * k);
        let y = panel.y + 60.0 * k + row as f32 * row_h;
        if key.is_empty() {
            // 分隔標題
            text.push(desc, x, top(text, y + 6.0 * k, 16.0), 12.0, 16.0, theme::ink(0.40), 600, None, None);
            continue;
        }
        let c = theme::ty::CAPTION;
        let y = top(text, y, c.line);
        text.push(key, x, y, c.size, c.line, theme::ink(0.92), 600, Some(120.0 * k), None);
        text.push(desc, x + 128.0 * k, y, c.size, c.line, theme::ink(0.62), 400, Some(col_w - 128.0 * k), None);
    }
    let foot = format!(
        "再按一次 ? 或 Esc 關掉　·　VitroDeck {}　·　{}",
        crate::config::VERSION,
        crate::config::GUIDE_URL
    );
    let hint = theme::ty::HINT;
    text.push(&foot, panel.x + 22.0 * k, top(text, panel.y + panel.h - 26.0 * k, hint.line), hint.size, hint.line,
              theme::ink(0.32), 400, None, None);
    text.prepare(gpu, w, h)?;

    {
        let mut pass = gfx::pass(&mut enc, &target.view, "help-glass", None);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        text.render(&mut pass)?;
    }

    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok(())
}
