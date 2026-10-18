//! 播放裝置選單（D）。web 版的 `showDevices()`。
//!
//! 版面跟設定面板共用（[`ui::devices`]），差別只在每一列的內容。

use crate::gfx::{self, Gpu, Offscreen};
use crate::glass::{Glass, Panel};
use crate::quad::{Quad, QuadRenderer};
use crate::spotify::Device;
use crate::text::Text;
use crate::theme;
use crate::ui::{self, Hit};

/// `list` 是 `Shared::devices`：`None` = 還在抓，`Some(Err)` = 抓不到。
///
/// `morph` 是打開的進度（0 = 收著，1 = 開著，回彈時會稍微超過 1），`from` 是它從哪顆
/// 按鈕長出來。選單從按鈕裡冒出來：形狀由按鈕那顆圓長成面板，途中跟留在按鈕上、
/// 一路縮掉的那顆融合成一塊，拉出一條液體的頸子；字最後才淡進來
#[allow(clippy::too_many_arguments)]
pub fn render(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    list: Option<&Result<Vec<Device>, String>>,
    sel: usize,
    hover: Option<Hit>,
    morph: f32,
    from: Option<ui::Rect>,
) -> anyhow::Result<()> {
    use crate::anim::{phase, SOFT};
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let devs: &[Device] = match list {
        Some(Ok(v)) => v,
        _ => &[],
    };
    let l = ui::devices(fw, fh, devs.len());
    let pc = morph.clamp(0.0, 1.0);
    let open = pc >= 1.0;
    // 玻璃的位置到了字才淡進來
    let content = SOFT.at(phase(pc, 0.7, 0.3));

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("devices") });

    quads.begin(gpu);
    quads.push(Quad::new(0.0, 0.0, fw, fh).color(theme::modal(if open { 0.42 } else { 0.42 * pc })));
    {
        let mut pass = gfx::pass(&mut enc, &target.view, "devices-dim", None);
        quads.flush(gpu, &mut pass, w, h);
    }

    let backdrop = glass.blur_scene(gpu, &mut enc, target, theme::GLASS_PANEL.sigma);
    let sharp = glass.sharp_scene(gpu, &mut enc, target);
    glass.begin();
    push_morph(glass, from, l.panel, theme::radius::PANEL, &theme::GLASS_PANEL, morph, 1);

    text.begin();
    if !open {
        quads.set_fx(content, 0.0);
        text.set_fx(content, 0.0);
    }
    text.push("播放裝置", l.panel.x + theme::space::PANEL_X, l.panel.y + theme::space::PANEL_TITLE_Y, theme::ty::TITLE.size, theme::ty::TITLE.line,
              theme::ink(0.95), 600, None, None);
    ui::button(text, &l.close, "✕", 16.0, 1.0, hover == Some(Hit::CloseDevices));

    if devs.is_empty() {
        let r = l.rows[0];
        let msg = match list {
            None => "正在找裝置…".to_string(),
            Some(Err(e)) => format!("抓不到裝置清單：{e}"),
            Some(Ok(_)) => "沒有可用裝置。先在手機或桌面版 Spotify 播一下任何東西。".to_string(),
        };
        let msg = text.fit(&msg, 14.0, 20.0, r.w - 28.0);
        text.push(&msg, r.x + 14.0, r.cy() - 10.0, theme::ty::BODY_S.size, theme::ty::BODY_S.line,
                  theme::ink(0.55), 400, None, None);
    }

    let right = l.panel.x + l.panel.w - 30.0;
    for (i, d) in devs.iter().enumerate() {
        let r = l.rows[i];
        let hot = hover == Some(Hit::DeviceRow(i));
        let on = i == sel;
        if on || hot {
            let a = if on { 0.20 } else { 0.09 };
            quads.push(Quad::new(r.x, r.y + 3.0, r.w, r.h - 6.0).radius(theme::radius::ROW)
                .color(theme::ink(a)));
        }
        // 前面的數字就是快捷鍵，跟 web 版的 <kbd> 一樣
        if i < 9 {
            text.push(&format!("{}", i + 1), r.x + 14.0, r.cy() - 10.0, theme::ty::BODY.size, theme::ty::BODY.line,
                      theme::ink(0.40), 600, None, None);
        }
        // 正在用的那台前面畫一顆綠點。**不用字元**：● 之類的會被 emoji 字型接走
        if d.is_active {
            quads.push(Quad::new(r.x + 34.0, r.cy() - 4.0, 8.0, 8.0).radius(4.0)
                .color([0.12, 0.84, 0.38, 1.0]));
        }
        let (kw, _) = text.measure(&d.kind, theme::ty::CAPTION.size, theme::ty::CAPTION.line, None);
        let name = text.fit_weight(&d.name, 15.0, 20.0, r.w - 80.0 - kw - 30.0, 500);
        let dim = if d.id.is_some() { 0.92 } else { 0.45 };
        text.push(&name, r.x + 50.0, r.cy() - 10.0, theme::ty::BODY.size, theme::ty::BODY.line,
                  theme::ink(dim), 500, None, None);
        text.push(&d.kind, right - kw, r.cy() - 9.0, theme::ty::CAPTION.size, theme::ty::CAPTION.line,
                  theme::ink(0.45), 400, None, None);
    }
    text.push("↑↓ 選　Enter／數字鍵 切換　Esc 關", l.panel.x + 22.0,
              l.panel.y + l.panel.h - 26.0, theme::ty::HINT.size, theme::ty::HINT.line,
              theme::ink(0.32), 400, None, None);
    quads.set_fx(1.0, 0.0);
    text.set_fx(1.0, 0.0);
    text.prepare(gpu, w, h)?;

    {
        let mut pass = gfx::pass(&mut enc, &target.view, "devices-glass", None);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    }

    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok(())
}

/// 選單從按鈕裡長出來的玻璃。`p` 是進度（回彈時會稍微超過 1），`fin` 是開好的樣子，
/// `group` 是融合用的組號（同一批玻璃裡不能跟別組重複）。`--shot motionlab` 也用這個。
///
/// 先在按鈕上長大、還跟按鈕連在一起，再移過去；按鈕上留一顆跟著縮掉的玻璃，
/// 兩塊融合，中間拉出一條液體的頸子。沒有按鈕可長的話在原地從小一點長出來
pub fn push_morph(glass: &mut Glass, from: Option<ui::Rect>, fin: ui::Rect, radius: f32, style: &theme::GlassStyle, p: f32, group: u32) {
    use crate::anim::{lerp, phase, SOFT};
    let p = p.max(0.0);
    if p == 1.0 {
        glass.push(Panel::new(fin.x, fin.y, fin.w, fin.h).radius(radius).style(style));
        return;
    }
    let pc = p.min(1.0);
    let present = SOFT.at(phase(pc, 0.0, 0.3));
    let Some(b) = from else {
        let k = lerp(0.9, 1.0, p);
        let r = ui::Rect::new(fin.cx() - fin.w * k * 0.5, fin.cy() - fin.h * k * 0.5, fin.w * k, fin.h * k);
        glass.push(Panel::new(r.x, r.y, r.w, r.h).radius(radius).style(style).materialize(present));
        return;
    };
    // 位置與大小各走各的：大小比位置早長好，字出來的時候玻璃已經夠大，不會蓋不住字
    let k = SOFT.at(phase(p, 0.1, 0.55));
    let ks = if p > 1.0 { p } else { SOFT.at(phase(p, 0.0, 0.75)) };
    let (cx, cy) = (lerp(b.cx(), fin.cx(), k), lerp(b.cy(), fin.cy(), k));
    let (pw, ph) = (lerp(b.w, fin.w, ks), lerp(b.h, fin.h, ks));
    // 圓角也從按鈕的圓長成面板的圓角
    glass.push(
        Panel::new(cx - pw * 0.5, cy - ph * 0.5, pw, ph)
            .radius(lerp(b.h * 0.5, radius, pc))
            .style(style)
            .materialize(present)
            .merge(group, theme::MERGE_K_MORPH),
    );
    // 按鈕上的那顆被面板拖著走一段、邊走邊縮，像一滴從按鈕拉出來的液體；
    // 跟在後面的距離夠近，頸子才拉得久
    let s = b.w * (1.0 - SOFT.at(phase(pc, 0.3, 0.45)));
    let kt = 0.7 * SOFT.at(phase(p, 0.18, 0.5));
    let (tx, ty) = (lerp(b.cx(), cx, kt), lerp(b.cy(), cy, kt));
    if s > 1.0 {
        glass.push(
            Panel::new(tx - s * 0.5, ty - s * 0.5, s, s)
                .radius(s * 0.5)
                .style(style)
                .materialize(present)
                .merge(group, theme::MERGE_K_MORPH),
        );
    }
}

/// 給 `--shot devices` 用的假清單。
pub fn demo() -> Vec<Device> {
    let d = |id: &str, name: &str, kind: &str, active: bool| Device {
        id: Some(id.into()),
        name: name.into(),
        kind: kind.into(),
        is_active: active,
    };
    vec![
        d("a", "DESKTOP-PC", "Computer", true),
        d("b", "Pixel 9", "Smartphone", false),
        d("c", "客廳的 Chromecast Audio", "CastAudio", false),
    ]
}
