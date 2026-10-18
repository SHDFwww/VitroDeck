//! 通知列：畫面底部一小片玻璃，講一句話然後自己消失。
//!
//! web 版的 `toast()`。原生版原本沒有它，結果是播放失敗（沒有裝置、
//! 非 Premium）完全靜默，排序、封面大小、KTV 這些鍵按了也沒有回饋。

use crate::gfx::{self, Gpu, Offscreen};
use crate::glass::{Glass, Panel};
use crate::quad::{Quad, QuadRenderer};
use crate::text::Text;
use crate::theme;
use crate::ui;

/// 時間照 CSS：`transition: opacity .25s, transform .25s`，停留 2600ms。
const FADE_S: f32 = theme::dur::TOAST_FADE;
const HOLD_S: f32 = theme::dur::TOAST_HOLD;
/// 進場時從下方 14px 滑上來（`translateY(14px)` → 0）
const RISE_PX: f32 = 14.0;

/// 顯示到第幾秒就該拿掉了。
pub const LIFE_S: f32 = HOLD_S + FADE_S;

/// 目前的不透明度。`age` 是從發出那一刻算起的秒數。
pub fn alpha(age: f32) -> f32 {
    if age < FADE_S {
        age / FADE_S
    } else if age < HOLD_S {
        1.0
    } else {
        (1.0 - (age - HOLD_S) / FADE_S).max(0.0)
    }
}

/// `bottom` 是通知列底邊的 y，依視圖由呼叫端決定（見 [`ui::toast`]）。
/// `clear` = 這一頁用 clear 玻璃（視覺效果頁），通知列跟著用，兩種變體不混在同一頁
#[allow(clippy::too_many_arguments)]
pub fn render(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    msg: &str,
    age: f32,
    bottom: f32,
    clear: bool,
) -> anyhow::Result<()> {
    let a = alpha(age);
    if a <= 0.0 {
        return Ok(());
    }
    let (w, h) = (target.width, target.height);
    let (fw, _) = crate::gfx::logical(w, h);

    let line = text.fit(msg, ui::TOAST_SIZE, ui::TOAST_LINE, ui::toast_text_max(fw));
    let (tw, _) = text.measure(&line, ui::TOAST_SIZE, ui::TOAST_LINE, None);
    // 進場那段往上滑；退場只淡不動，跟 CSS 一樣（.show 拿掉時 transform 也回去，
    // 但 opacity 同時歸零，看起來就是淡掉）
    let rise = if age < FADE_S { RISE_PX * (1.0 - a) } else { 0.0 };
    let r = ui::toast(fw, tw, bottom + rise);

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("toast") });

    let style = if clear { &theme::GLASS_TOAST_CLEAR } else { &theme::GLASS_TOAST };
    if clear {
        // clear 玻璃後面的局部變暗層，跟視覺效果的底欄同一套
        let (dim_a, grow, blur) = theme::CLEAR_DIM;
        quads.begin(gpu);
        quads.push(
            Quad::new(r.x - grow, r.y - grow, r.w + grow * 2.0, r.h + grow * 2.0)
                .radius(r.h * 0.5 + grow)
                .color([0.0; 4])
                .shadow(0.0, 0.0, blur, 0.0, theme::scrim(dim_a * a)),
        );
        let mut pass = gfx::pass(&mut enc, &target.view, "toast-dim", None);
        quads.flush(gpu, &mut pass, w, h);
    }
    // CSS 是 `blur(24px)`，比面板的 30 輕一點
    let backdrop = glass.blur_scene(gpu, &mut enc, target, style.sigma);
    let sharp = glass.sharp_scene(gpu, &mut enc, target);
    // 小元件：壓在很亮的東西上時翻成淺色玻璃，字跟著翻（clear 玻璃不翻，
    // 外觀是淺色也一樣是深色那一套）
    let tone = if clear { theme::dark_tone() } else { glass.tone("toast", [r.x, r.y, r.w, r.h]) };
    glass.begin();
    // 膠囊（第 5 階段第四批，原本是圓角 14）
    glass.push(
        Panel::new(r.x, r.y, r.w, r.h)
            .radius(r.h * 0.5)
            .style(style)
            .materialize(a)
            .tone(tone),
    );

    text.begin();
    let _tone = theme::scoped_tone(tone);
    text.push(&line, r.x + (r.w - tw) * 0.5, r.cy() - ui::TOAST_LINE * 0.5, ui::TOAST_SIZE,
              ui::TOAST_LINE, theme::ink(0.95 * a), 400, None, None);
    text.prepare(gpu, w, h)?;

    {
        let mut pass = gfx::pass(&mut enc, &target.view, "toast", None);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        text.render(&mut pass)?;
    }
    if clear {
        quads.upload(gpu, w, h);
    }
    gpu.queue.submit([enc.finish()]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 淡入停留淡出() {
        assert_eq!(alpha(0.0), 0.0);
        assert!((alpha(FADE_S * 0.5) - 0.5).abs() < 1e-5);
        assert_eq!(alpha(1.0), 1.0);
        assert!(alpha(HOLD_S + FADE_S * 0.5) > 0.0 && alpha(HOLD_S + FADE_S * 0.5) < 1.0);
        assert_eq!(alpha(LIFE_S), 0.0);
        assert_eq!(alpha(LIFE_S + 5.0), 0.0);
    }
}
