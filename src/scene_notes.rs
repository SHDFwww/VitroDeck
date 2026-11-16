//! 更新內容：換了新版第一次啟動時彈一次，講這一版改了什麼。
//!
//! 內容是 `RELEASE_NOTES_v<版本>.md` 的條列，編譯時由 `build.rs` 整理後嵌進 exe
//! （一行一條）。沒有那份檔案的版本不彈。

use crate::gfx::{self, Gpu, Offscreen};
use crate::glass::{Glass, Panel};
use crate::quad::{Quad, QuadRenderer};
use crate::text::Text;
use crate::theme;
use crate::ui::{Hit, Rect};

const NOTES: &str = include_str!(concat!(env!("OUT_DIR"), "/release_notes.txt"));

const SIZE: f32 = 14.0;
const LINE: f32 = 21.0;
const GAP: f32 = 9.0;
const INDENT: f32 = 16.0;
const BTN: f32 = 36.0;

/// 這一版的條列
pub fn items() -> Vec<&'static str> {
    NOTES.lines().map(str::trim).filter(|l| !l.is_empty()).collect()
}

/// 要不要在這次啟動彈出來：這版有條列，而且比上次看過的新（沒記錄過算看過 0.0.0）。
/// 降版（看過的比現在新）不彈
pub fn due(seen: &str) -> bool {
    !items().is_empty() && crate::update::ver_tuple(crate::config::VERSION) > crate::update::ver_tuple(seen)
}

/// 估一條會折成幾行。中日文字一個字一格寬，英數字約 0.55 格；多留一點，寧可面板高一點也不要字衝出去
fn lines(s: &str, width: f32) -> f32 {
    let w: f32 = s.chars().map(|c| if c.is_ascii() { 0.55 } else { 1.0 }).sum::<f32>() * SIZE;
    (w / (width * 0.94)).ceil().max(1.0)
}

pub struct NotesL {
    pub panel: Rect,
    pub close: Rect,
}

pub fn layout(fw: f32, fh: f32) -> NotesL {
    let pw = (fw - 40.0).min(560.0);
    let tw = pw - 56.0 - INDENT;
    let body: f32 = items().iter().map(|s| lines(s, tw) * LINE + GAP).sum();
    let ph = (70.0 + body + 48.0).clamp(200.0, (fh - 40.0).max(200.0));
    let panel = Rect::new((fw - pw) * 0.5, ((fh - ph) * 0.5).max(20.0), pw, ph);
    NotesL { close: Rect::centred(panel.x + pw - 34.0, panel.y + 32.0, BTN, BTN), panel }
}

/// 點到面板外或叉叉關掉；點在面板裡沒事
pub fn hit(l: &NotesL, mx: f32, my: f32) -> Option<Hit> {
    (l.close.contains(mx, my) || !l.panel.contains(mx, my)).then_some(Hit::CloseNotes)
}

pub fn render(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    hover: Option<Hit>,
) -> anyhow::Result<()> {
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let l = layout(fw, fh);
    let x = l.panel.x + 28.0;
    let tw = l.panel.w - 56.0 - INDENT;

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("notes") });

    quads.begin(gpu);
    quads.push(Quad::new(0.0, 0.0, fw, fh).color(theme::modal(0.46)));
    {
        let mut pass = gfx::pass(&mut enc, &target.view, "notes-dim", None);
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
    let title = format!("更新內容　v{}", crate::config::VERSION);
    text.push(&title, x, l.panel.y + 20.0, theme::ty::TITLE.size, theme::ty::TITLE.line, theme::ink(0.95), 600, None, None);
    crate::ui::button(text, &l.close, "✕", 16.0, 1.0, hover == Some(Hit::CloseNotes));
    let mut y = l.panel.y + 70.0;
    let limit = l.panel.y + l.panel.h - 40.0;
    for s in items() {
        let (_, th) = text.measure(s, SIZE, LINE, Some(tw));
        if y + th > limit {
            break;
        }
        text.push("•", x + 2.0, y, SIZE, LINE, theme::ink(0.5), 400, None, None);
        text.push(s, x + INDENT, y, SIZE, LINE, theme::ink(0.82), 400, Some(tw), None);
        y += th + GAP;
    }
    text.push("按任意鍵或點一下關閉", x, l.panel.y + l.panel.h - 30.0, theme::ty::HINT.size, theme::ty::HINT.line,
              theme::ink(0.32), 400, None, None);
    text.prepare(gpu, w, h)?;

    {
        let mut pass = gfx::pass(&mut enc, &target.view, "notes-glass", None);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
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
    fn 估的行數不少於實際() {
        assert_eq!(lines("短", 400.0), 1.0);
        // 40 個中文字、一行約 28 字寬 → 2 行
        let s: String = "字".repeat(40);
        assert_eq!(lines(&s, 400.0), 2.0);
    }

    #[test]
    fn 看過的版本不再彈() {
        // 這個測試只管規則：沒有條列的 build 一律不彈，有的話比對版本
        if items().is_empty() {
            assert!(!due(""));
        } else {
            assert!(due(""));
            assert!(due("0.0.1"));
            assert!(!due(crate::config::VERSION));
            assert!(!due("99.0.0"));
        }
    }

    #[test]
    fn 點外面或叉叉關掉_面板裡不關() {
        let l = layout(1280.0, 720.0);
        assert_eq!(hit(&l, 2.0, 2.0), Some(Hit::CloseNotes));
        assert_eq!(hit(&l, l.close.cx(), l.close.cy()), Some(Hit::CloseNotes));
        assert_eq!(hit(&l, l.panel.x + 40.0, l.panel.y + 100.0), None);
    }
}
