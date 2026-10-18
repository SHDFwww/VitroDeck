//! 設定面板。疊在目前的畫面上，不是獨立視圖。
//!
//! 背景先模糊一次當毛玻璃的取樣來源，再把面板與每一列畫上去。
//! 這條路徑跟控制列一樣，只是面板大得多。

use crate::config::Config;
use crate::gfx::{self, Gpu, Offscreen};
use crate::glass::{Glass, Panel};
use crate::quad::{Quad, QuadRenderer};
use crate::settings::ITEMS;
use crate::text::Text;
use crate::theme;
use crate::ui::{self, Hit};

#[allow(clippy::too_many_arguments)]
pub fn render(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    cfg: &Config,
    logged_in: bool,
    data_dir: &str,
    login_msg: &str,
    update: &str,
    sel: usize,
    hover: Option<Hit>,
) -> anyhow::Result<()> {
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let l = ui::settings(fw, fh, ITEMS.len());

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("settings") });

    quads.begin(gpu);
    // 整個底先壓暗，面板才跳得出來
    quads.push(Quad::new(0.0, 0.0, fw, fh).color(theme::modal(0.42)));
    {
        let mut pass = gfx::pass(&mut enc, &target.view, "settings-dim", None);
        quads.flush(gpu, &mut pass, w, h);
    }

    let backdrop = glass.blur_scene(gpu, &mut enc, target, theme::GLASS_PANEL.sigma);
    let sharp = glass.sharp_scene(gpu, &mut enc, target);
    glass.begin();
    glass.push(
        Panel::new(l.panel.x, l.panel.y, l.panel.w, l.panel.h)
            .radius(theme::radius::PANEL)
            .style(&theme::GLASS_PANEL),
    );

    text.begin();
    text.push("設定", l.panel.x + theme::space::PANEL_X, l.panel.y + theme::space::PANEL_TITLE_Y, theme::ty::TITLE.size, theme::ty::TITLE.line,
              theme::ink(0.95), 600, None, None);
    ui::button(text, &l.close, "✕", 16.0, 1.0, hover == Some(Hit::CloseSettings));

    for (i, item) in ITEMS.iter().enumerate() {
        let r = l.rows[i];
        let hot = hover == Some(Hit::SettingRow(i));
        let on = i == sel;
        if on || hot {
            // 選到的那列給底色。鍵盤選的比滑鼠停的重一點，
            // 兩個都有的時候才分得出焦點在哪。
            let a = if on { 0.20 } else { 0.09 };
            quads.push(Quad::new(r.x, r.y + 3.0, r.w, r.h - 6.0).radius(theme::radius::ROW)
                .color(theme::ink(a)));
        }
        let dim = if item.read_only() { 0.55 } else { 0.92 };
        let hint = item.hint();
        // 有說明的話標籤往上挪，說明擠在下面那行
        let ly = if hint.is_empty() { r.cy() - 10.0 } else { r.y + 5.0 };
        text.push(item.label(), r.x + 14.0, ly, theme::ty::BODY.size, theme::ty::BODY.line,
                  theme::ink(dim), 500, None, None);
        if !hint.is_empty() {
            let hint = text.fit(hint, 11.0, 15.0, r.w - 200.0);
            text.push(&hint, r.x + 14.0, r.y + 24.0, theme::ty::HINT.size, theme::ty::HINT.line,
                      theme::ink(0.38), 400, None, None);
        }
        // 兩欄的時候每欄各自靠右
        let val_x = r.x + r.w - 16.0;
        let v = item.value(cfg, logged_in, data_dir, login_msg, update);
        let v = text.fit(&v, 15.0, 20.0, r.w * 0.5);
        let (vw, _) = text.measure(&v, theme::ty::BODY.size, theme::ty::BODY.line, None);
        text.push(&v, val_x - vw, r.cy() - 10.0, theme::ty::BODY.size, theme::ty::BODY.line,
                  theme::ink(if item.read_only() { 0.5 } else { 0.85 }), 400, None, None);
    }
    text.push("↑↓ 選　←→／Enter 改　Esc 關", l.panel.x + 22.0,
              l.panel.y + l.panel.h - 26.0, theme::ty::HINT.size, theme::ty::HINT.line,
              theme::ink(0.32), 400, None, None);
    text.prepare(gpu, w, h)?;

    {
        let mut pass = gfx::pass(&mut enc, &target.view, "settings-glass", None);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    }

    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok(())
}

/// 給 `--shot settings` 用的假設定。
pub fn demo_cfg() -> Config {
    Config { client_id: "0".repeat(32), ..Default::default() }
}
