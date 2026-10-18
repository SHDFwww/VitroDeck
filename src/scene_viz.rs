//! 視覺效果畫面。
//!
//! 主體由 [`crate::viz`] 的 MilkDrop 引擎畫；preset 還在載入時退回頻譜長條，
//! 這樣一進視圖就有東西看，不會空幾百毫秒。

use crate::audio;
use crate::gfx::{self, Gpu, Offscreen};
use crate::glass::{Glass, Panel};
use crate::quad::{Quad, QuadRenderer};
use crate::text::Text;
use crate::theme;
use crate::ui::{self, Hit};
use crate::viz::Viz;

#[allow(clippy::too_many_arguments)]
pub fn render(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    viz: &mut Viz,
    spec: &audio::Shared,
    track: &str,
    bar_alpha: f32,
    bar_k: f32,
    hover: Option<Hit>,
    dt: f64,
) -> anyhow::Result<()> {
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let l = ui::viz(fw, fh);

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("viz") });

    quads.begin(gpu);

    let drew = viz.render(gpu, &mut enc, target, spec, dt);
    if !drew {
        fallback_spectrum(gpu, &mut enc, target, quads, spec, fw, fh);
    }

    // ---- 疊層 ----
    // MilkDrop 是內容，不跟外觀變；上面的字與 clear 玻璃也固定用深色那一套
    let _dark = theme::scoped_dark();
    text.begin();
    glass.begin();
    // 沒聲音的提示放在畫面正中央、**不跟著控制列收起來** —— 控制列閒置三秒就被
    // 神燈吸走了，提示放在裡面的話，使用者真正盯著畫面發呆的時候反而看不到。
    // 位置與用語照 web 版的 `#viz-msg`。
    if spec.lock().map(|s| s.quiet(viz.entered)).unwrap_or(false) {
        let msg = "沒有偵測到聲音。確認 Spotify 正在這台裝置播放。";
        let (mw, _) = text.measure(msg, theme::ty::BODY_S.size, theme::ty::BODY_S.line, None);
        let (x, y) = ((fw - mw) * 0.5, fh * 0.5 - 10.0);
        // 底下是 MilkDrop，什麼顏色都可能出現 —— 墊一層字影才讀得到
        text.push(msg, x + 1.0, y + 1.0, theme::ty::BODY_S.size, theme::ty::BODY_S.line, theme::scrim(0.7), 400, None, None);
        text.push(msg, x, y, theme::ty::BODY_S.size, theme::ty::BODY_S.line, theme::ink(0.75), 400, None, None);
    }

    // 神燈：收起來是被吸進畫面中心，不是淡出。細節見 `scene_now`。
    let genie_p = 1.0 - bar_alpha;
    let vis = 1.0 - ((genie_p - 0.88) / 0.12).clamp(0.0, 1.0);
    // 大面板蓋到底欄時整條消散（玻璃不疊玻璃）
    let bar_k = bar_k.clamp(0.0, 1.0);
    if vis > 0.01 && bar_k > 0.0 {
        let (px, py, pw) = (l.bar.x, l.bar.y, l.bar.w);
        // 底下是 MilkDrop，什麼亮度都可能出現。clear 玻璃不依背景調整，字要讀得到
        // 靠的是先在玻璃後面把那一小塊壓暗（局部變暗層），玻璃再去折射壓暗過的畫面。
        // 跟著神燈一起淡：收起來之後畫面不該留一塊暗影
        let (dim_a, dim_grow, dim_blur) = theme::CLEAR_DIM;
        let dk = (1.0 - genie_p / 0.25).clamp(0.0, 1.0) * bar_k;
        quads.push(
            Quad::new(px - dim_grow, py - dim_grow, pw + dim_grow * 2.0, 72.0 + dim_grow * 2.0)
                .radius(36.0 + dim_grow)
                .color([0.0; 4])
                .shadow(0.0, 0.0, dim_blur, 0.0, theme::scrim(dim_a * dk)),
        );
        {
            let mut pass = gfx::pass(&mut enc, &target.view, "viz-dim", None);
            quads.flush(gpu, &mut pass, w, h);
        }
        let backdrop = glass.blur_scene(gpu, &mut enc, target, theme::GLASS_VIZ.sigma);
        let sharp = glass.sharp_scene(gpu, &mut enc, target);
        glass.push(
            Panel::new(px, py, pw, 72.0)
                .radius(36.0)
                .style(&theme::GLASS_VIZ.faded(vis))
                .genie(genie_p, (fw * 0.5, fh * 0.5))
                .tone(theme::dark_tone())
                .materialize(bar_k),
        );

        // 字沒辦法跟著轉，所以先淡掉
        let a = (1.0 - genie_p / 0.25).clamp(0.0, 1.0) * bar_k;
        // 上排：目前的 preset
        let name = if viz.current_name().is_empty() { "—" } else { viz.current_name() };
        let star = if viz.is_favourite() { "★ " } else { "" };
        // preset 的名字動輒六七十個字元 —— 限寬只會讓它折到第二行疊在
        // 下面那排上，要截斷才行。
        // 鎖住的話後面接一段綠色的「· 已鎖定」（web 版的 `.locked::after`）。
        // 顏色不同就得分兩段畫，寬度一起算才置中得對。
        let lock = if viz.locked { " · 已鎖定" } else { "" };
        // 量寬要用跟畫的時候一樣的字重（500），不然名字比量的寬，
        // 會把後面接的「 · 已鎖定」蓋掉一截
        let (lw, _) = text.measure_weight(lock, 16.0, 21.0, None, 500);
        let line = text.fit_weight(&format!("{star}{name}"), 16.0, 21.0, l.text_w - lw, 500);
        let (tw, _) = text.measure_weight(&line, 16.0, 21.0, None, 500);
        let x0 = px + (pw - tw - lw) * 0.5;
        text.push(&line, x0, py + 14.0, 16.0, 21.0,
                  theme::ink(0.94 * a), 500, None, None);
        if !lock.is_empty() {
            text.push(lock, x0 + tw, py + 14.0, 16.0, 21.0,
                      theme::ok(0.95 * a), 500, None, None);
        }

        // 下排：分類、編號、曲目
        let sub = if viz.ready() {
            format!("{}　{}/{}　·　{}", viz.cat.label(), viz.position(), viz.count(), track)
        } else {
            viz.message.clone()
        };
        let sub = text.fit(&sub, 13.0, 18.0, l.text_w);
        let (sw, _) = text.measure(&sub, theme::ty::CAPTION.size, theme::ty::CAPTION.line, None);
        text.push(&sub, px + (pw - sw) * 0.5, py + 40.0, theme::ty::CAPTION.size, theme::ty::CAPTION.line,
                  theme::ink(0.52 * a), 400, None, None);

        ui::button(text, &l.prev, "‹", 20.0, a, hover == Some(Hit::PresetPrev));
        ui::button(text, &l.next, "›", 20.0, a, hover == Some(Hit::PresetNext));
        ui::button(text, &l.fav, if viz.is_favourite() { "★" } else { "☆" }, 17.0, a,
                   hover == Some(Hit::Favourite));
        ui::button(text, &l.cat, "◆", 15.0, a, hover == Some(Hit::Category));

        if let Some(r) = hover.and_then(|hit| ui::viz_btn(&l, hit)) {
            quads.push(
                Quad::new(r.x, r.y, r.w, r.h).radius(r.h * 0.5).color(theme::ink(0.10 * a)),
            );
        }
        text.prepare(gpu, w, h)?;
        let mut pass = gfx::pass(&mut enc, &target.view, "viz-glass", None);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    } else {
        text.prepare(gpu, w, h)?;
        let mut pass = gfx::pass(&mut enc, &target.view, "viz-text", None);
        text.render(&mut pass)?;
    }

    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok(())
}

/// preset 還沒載好時的替代畫面。
fn fallback_spectrum(
    gpu: &Gpu,
    enc: &mut wgpu::CommandEncoder,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    spec: &audio::Shared,
    fw: f32,
    fh: f32,
) {
    let (bins, bass) = match spec.lock() {
        Ok(s) => (s.bins, s.bass),
        Err(_) => ([0.0; audio::BINS], 0.0),
    };
    let pulse = (bass * 0.10).min(0.10);
    quads.push(Quad::new(0.0, 0.0, fw, fh).color([0.05 + pulse, 0.03, 0.09 + pulse * 0.6, 1.0]));

    let n = audio::BINS;
    let gap = 2.0f32;
    let bw = (fw - gap) / n as f32 - gap;
    let base_y = fh * 0.86;
    for (i, v) in bins.iter().enumerate() {
        let x = gap + i as f32 * (bw + gap);
        let bh = (v * fh * 0.62).max(2.0);
        let hue = i as f32 / n as f32;
        let (r, g, b) = hsv(0.58 + hue * 0.42, 0.62, 0.55 + v * 0.45);
        quads.push(Quad::new(x, base_y - bh, bw, bh).radius(bw * 0.4).color([r, g, b, 0.92]));
        quads.push(
            Quad::new(x, base_y + 2.0, bw, bh * 0.34).radius(bw * 0.4).color([r, g, b, 0.16]),
        );
    }

    let mut pass = gfx::pass(enc, &target.view, "viz-fallback", Some(wgpu::Color::BLACK));
    quads.flush(gpu, &mut pass, target.width, target.height);
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
