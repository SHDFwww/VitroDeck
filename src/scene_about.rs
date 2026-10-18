//! 關於面板（I，或設定裡的「關於 / 中斷連結…」）。web 版的 `#about`。
//!
//! Spotify 條款 I.1.c 要求提供容易找到的「中斷連結」；II 也要求講清楚
//! 這是第三方程式、跟 Spotify 無關。兩件事都在這一頁。

use crate::gfx::{self, Gpu, Offscreen};
use crate::glass::{Glass, Panel};
use crate::quad::{Quad, QuadRenderer};
use crate::text::Text;
use crate::theme;
use crate::ui::{self, Hit};

const PARAS: [&str; 3] = [
    "VitroDeck 是第三方程式，與 Spotify 無關聯、未經其背書。",
    "曲目資訊來自 Windows 系統媒體控制；專輯與封面來自 Spotify Web API；歌詞來自 LRCLIB。",
    "所有資料（授權權杖、收藏快取、歌詞）都只存在你自己的電腦上，不會傳送到任何第三方伺服器。",
];

/// `confirm`：中斷連結鈕按過一次，等第二次確認。
#[allow(clippy::too_many_arguments)]
pub fn render(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    hover: Option<Hit>,
    data_dir: &str,
    confirm: bool,
) -> anyhow::Result<()> {
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let l = ui::about(fw, fh);
    let x = l.panel.x + 28.0;
    let tw = l.panel.w - 56.0;

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("about") });

    quads.begin(gpu);
    quads.push(Quad::new(0.0, 0.0, fw, fh).color(theme::modal(0.46)));
    {
        let mut pass = gfx::pass(&mut enc, &target.view, "about-dim", None);
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
    text.push("關於", x, l.panel.y + 20.0, theme::ty::TITLE.size, theme::ty::TITLE.line, theme::ink(0.95), 600, None, None);
    ui::button(text, &l.close, "✕", 16.0, 1.0, hover == Some(Hit::CloseAbout));
    for (i, p) in PARAS.iter().enumerate() {
        // 第一段是聲明，比其他兩段亮一點
        let (a, wt) = if i == 0 { (0.92, 500) } else { (0.70, 400) };
        text.push(p, x, l.para_y[i], 14.0, 21.0, theme::ink(a), wt, Some(tw), None);
    }

    // 兩個連結：底下一條細線，滑過去變亮
    for (r, label, hit) in [
        (l.guide, "使用教學", Hit::AboutGuide),
        (l.licenses, "第三方授權", Hit::AboutLicenses),
    ] {
        let hot = hover == Some(hit);
        let (lw, _) = text.measure(label, theme::ty::BODY_S.size, theme::ty::BODY_S.line, None);
        if hot {
            quads.push(Quad::new(r.x - 8.0, r.y, lw + 16.0, r.h).radius(8.0).color(theme::ink(0.10)));
        }
        text.push(label, r.x, r.cy() - 10.0, theme::ty::BODY_S.size, theme::ty::BODY_S.line, theme::link(if hot { 1.0 } else { 0.85 }),
                  500, None, None);
        quads.push(Quad::new(r.x, r.cy() + 10.0, lw, 1.0).color(theme::link(0.45)));
    }

    let meta = format!("VitroDeck v{}　·　{}", crate::config::VERSION, data_dir);
    let meta = text.fit(&meta, 12.0, 17.0, tw);
    text.push(&meta, x, l.meta_y, 12.0, 17.0, theme::ink(0.42), 400, None, None);

    // 中斷連結：紅色系。會刪東西，第一次按只是「確定嗎」
    let r = l.disconnect;
    let hot = hover == Some(Hit::AboutDisconnect);
    let fill = match (confirm, hot) {
        (true, _) => theme::danger(0.55),
        (false, true) => theme::danger(0.30),
        (false, false) => theme::danger(0.18),
    };
    quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(12.0).color(fill));
    let label = if confirm {
        "再按一次確認：登出，並刪除收藏快取與歌詞快取"
    } else {
        "中斷 Spotify 連結並刪除本機資料"
    };
    let label = text.fit_weight(label, 14.0, 20.0, r.w - 24.0, 500);
    let (lw, _) = text.measure(&label, theme::ty::BODY_S.size, theme::ty::BODY_S.line, None);
    text.push(&label, r.cx() - lw * 0.5, r.cy() - 10.0, theme::ty::BODY_S.size, theme::ty::BODY_S.line, theme::ink(0.95), 500, None, None);

    text.push("I 或 Esc 關閉", x, l.panel.y + l.panel.h - 30.0, theme::ty::HINT.size, theme::ty::HINT.line,
              theme::ink(0.32), 400, None, None);
    text.prepare(gpu, w, h)?;

    {
        let mut pass = gfx::pass(&mut enc, &target.view, "about-glass", None);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    }

    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok(())
}
