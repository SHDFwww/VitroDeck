//! 首次設定精靈：沒有 client_id 就開不了 Spotify，這一頁帶使用者走完三步。
//!
//! 內容照 web 版的 `#wizard`。原本 Rust 版要自己去編 `config.json`，
//! 對「第一次打開這個程式」的人來說等於沒有入口。

use crate::gfx::{self, Gpu, Offscreen};
use crate::glass::{Glass, Panel};
use crate::quad::{Quad, QuadRenderer};
use crate::search::Input;
use crate::text::Text;
use crate::theme;
use crate::ui::{self, Hit, Rect};

pub const DASHBOARD_URL: &str = "https://developer.spotify.com/dashboard";

/// 精靈的狀態。畫的東西在這裡，按鍵在 main。
#[derive(Default)]
pub struct Setup {
    pub input: Input,
    /// 存檔失敗的原因（格式不對）。空字串代表沒有錯
    pub err: String,
    /// 按過「開啟 Dashboard」／「複製」—— 按鈕換成打勾，web 版也是
    pub dash_opened: bool,
    pub copied: bool,
}

const STEPS: [(&str, &str); 3] = [
    ("建立 Spotify 應用程式", "用你自己的 Spotify 帳號登入，按 Create app，名稱隨便填。"),
    (
        "Redirect URI 填這個",
        "必須一字不差。Spotify 不接受 localhost，只認 127.0.0.1。同一頁的 API 記得勾 Web API，然後存檔。",
    ),
    ("貼上 Client ID", "回到 app 的 Settings 頁，複製 Client ID 整串貼進來（Ctrl+V）。"),
];

/// 回傳游標的位置（給輸入法定位候選窗）。
#[allow(clippy::too_many_arguments)]
pub fn render(
    gpu: &Gpu,
    target: &Offscreen,
    quads: &mut QuadRenderer,
    glass: &mut Glass,
    text: &mut Text,
    st: &Setup,
    redirect: &str,
    caret_on: bool,
    hover: Option<Hit>,
) -> anyhow::Result<(f32, f32)> {
    let (w, h) = (target.width, target.height);
    let (fw, fh) = crate::gfx::logical(w, h);
    let l = ui::setup(fw, fh);

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("setup") });

    quads.begin(gpu);
    quads.push(Quad::new(0.0, 0.0, fw, fh).color(theme::modal(0.5)));
    {
        let mut pass = gfx::pass(&mut enc, &target.view, "setup-dim", None);
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
    text.push("設定 Spotify", l.panel.x + theme::space::PANEL_X, l.panel.y + theme::space::PANEL_TITLE_Y, theme::ty::TITLE.size, theme::ty::TITLE.line,
              theme::ink(0.95), 600, None, None);
    ui::button(text, &l.close, "✕", 16.0, 1.0, hover == Some(Hit::SetupClose));

    for (i, (title, desc)) in STEPS.iter().enumerate() {
        let y = l.step_y[i];
        // 編號圓圈
        let c = Rect::new(l.panel.x + 22.0, y - 4.0, 28.0, 28.0);
        quads.push(Quad::new(c.x, c.y, c.w, c.h).radius(theme::radius::CARD).color(theme::ink(0.12)));
        let n = format!("{}", i + 1);
        let (nw, _) = text.measure_weight(&n, 14.0, 18.0, None, 600);
        text.push(&n, c.cx() - nw * 0.5, c.cy() - 9.0, 14.0, 18.0,
                  theme::ink(0.9), 600, None, None);
        text.push(title, l.body_x, y, theme::ty::BODY.size, theme::ty::BODY.line, theme::ink(0.95), 600, None, None);
        text.push(desc, l.body_x, y + 24.0, theme::ty::CAPTION.size, theme::ty::CAPTION.line, theme::ink(0.55), 400,
                  Some(l.body_w), None);
    }

    // 1：開 Dashboard
    let dash = if st.dash_opened { "已開啟 ✓" } else { "開啟 Dashboard" };
    pill(quads, text, &l.dashboard, dash, false, hover == Some(Hit::SetupDashboard));

    // 2：redirect URI（唯讀）＋ 複製
    field(quads, &l.redirect, false);
    text.push(redirect, l.redirect.x + 12.0, l.redirect.cy() - 10.0, theme::ty::BODY_S.size, theme::ty::BODY_S.line,
              theme::ink(0.85), 400, None, None);
    let copy = if st.copied { "已複製 ✓" } else { "複製" };
    pill(quads, text, &l.copy, copy, false, hover == Some(Hit::SetupCopy));

    // 3：client_id 輸入框 ＋ 儲存
    field(quads, &l.input, true);
    let caret = crate::scene_search::draw_input(
        text, quads, &st.input, l.input.x + 12.0, l.input.cy(), l.input.w - 24.0, 14.0,
        "32 位英數字", caret_on,
    );
    pill(quads, text, &l.save, "儲存", true, hover == Some(Hit::SetupSave));

    if !st.err.is_empty() {
        text.push(&st.err, l.body_x, l.err_y, theme::ty::CAPTION.size, theme::ty::CAPTION.line, theme::danger_fg(0.95), 400,
                  Some(l.body_w), None);
    }
    text.push("Enter 儲存　·　Esc 先略過（之後從設定面板的 Spotify 那列再開）",
              l.panel.x + 22.0, l.panel.y + l.panel.h - 26.0, theme::ty::HINT.size, theme::ty::HINT.line,
              theme::ink(0.32), 400, None, None);
    text.prepare(gpu, w, h)?;

    {
        let mut pass = gfx::pass(&mut enc, &target.view, "setup-glass", None);
        glass.render(gpu, &mut pass, &backdrop, &sharp, w, h);
        quads.flush(gpu, &mut pass, w, h);
        text.render(&mut pass)?;
    }
    quads.upload(gpu, w, h);
    gpu.queue.submit([enc.finish()]);
    Ok((caret, l.input.cy()))
}

/// 輸入框的底。可以打字的那個亮一點，唯讀的暗一點。
fn field(quads: &mut QuadRenderer, r: &Rect, editable: bool) {
    let a = if editable { 0.34 } else { 0.22 };
    quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(theme::radius::CONTROL).color(theme::field(a))
        .border(1.0, theme::ink(if editable { 0.18 } else { 0.08 })));
}

/// 按鈕：`.wz-btn`，主要的那顆是綠底（`.primary`）。
fn pill(quads: &mut QuadRenderer, text: &mut Text, r: &Rect, label: &str, primary: bool, hot: bool) {
    let bg = match (primary, hot) {
        (true, false) => theme::ok(0.9),
        (true, true) => theme::ok(1.0),
        (false, false) => theme::ink(0.10),
        (false, true) => theme::ink(0.18),
    };
    quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(theme::radius::CONTROL).color(bg));
    let fg = if primary { [0.02, 0.05, 0.03, 1.0] } else { theme::ink(0.92) };
    let (tw, _) = text.measure_weight(label, theme::ty::CAPTION.size, theme::ty::CAPTION.line, None, 600);
    text.push(label, r.cx() - tw * 0.5, r.cy() - 9.0, theme::ty::CAPTION.size, theme::ty::CAPTION.line, fg, 600, None, None);
}

/// 存檔前的檢查。回傳清理過的 id，或給使用者看的錯誤。規則跟 `Config::client_id_looks_valid` 一樣。
pub fn validate(raw: &str) -> Result<String, String> {
    let id = raw.trim().to_string();
    if id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(id)
    } else {
        // 用語照 app.py 的 set_client_id
        Err("Client ID 應該是 32 位英數字，請在 Dashboard 的 Settings 頁複製整串".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_id_檢查() {
        let ok = "0123456789abcdef0123456789ABCDEF";
        assert_eq!(validate(&format!("  {ok}\n")).as_deref(), Ok(ok), "頭尾空白要去掉");
        assert!(validate("abc").is_err());
        assert!(validate(&"g".repeat(32)).is_err(), "不是十六進位");
    }
}
