//! 迷你視窗的圓角。Win11 交給 DWM（見 [`Backend`]），Win10 退回這裡的
//! 逐像素 alpha 分層視窗（`UpdateLayeredWindow`）。
//!
//! 為什麼不讓 wgpu 的交換鏈直接輸出透明：Win10 沒有 DWM 圓角
//! （`DWMWA_WINDOW_CORNER_PREFERENCE` 是 Win11 才有），一般 HWND surface 的
//! alpha 模式通常只有不透明，而分層視窗（滑鼠穿透要它）跟 flip model 交換鏈
//! 又合不來。所以改成：離屏畫好一張**預乘 alpha** 的圖、讀回 CPU、整張交給
//! `UpdateLayeredWindow`。圓角的反鋸齒、陰影、穿透全在同一張圖裡解決 ——
//! Python 版為了同一件事疊了三個視窗（見 CoverFlow 的 `minishadow.py`）。
//!
//! 代價是每次更新一趟 GPU→CPU 讀回；迷你視窗只有幾十萬像素，一秒幾次無所謂。

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{STYLESTRUCT, WM_STYLECHANGING, WS_EX_APPWINDOW};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
    HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, HWND_NOTOPMOST, HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SetWindowLongPtrW, SetWindowPos, ULW_ALPHA, UpdateLayeredWindow,
    WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

pub struct Layered {
    hwnd: HWND,
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: u32,
    h: u32,
}

const SUBCLASS_ID: usize = 0x5644_4D49; // "VDMI"

/// **winit 會把擴充樣式整個重寫回它自己的版本**（`set_visible` 等時候），
/// 我們加上去的 `WS_EX_LAYERED` 一下就被拿掉，之後 `UpdateLayeredWindow`
/// 只會回「參數錯誤」。所以在 `WM_STYLECHANGING` 把關：`data` 的低 32 位元是
/// 一定要有的位元、高 32 位元是一定不能有的，誰來改樣式都得過這一關。
unsafe extern "system" fn style_guard(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    if msg == WM_STYLECHANGING && wp.0 as i32 == GWL_EXSTYLE.0 {
        let ss = unsafe { &mut *(lp.0 as *mut STYLESTRUCT) };
        ss.styleNew = (ss.styleNew | (data as u32)) & !((data >> 32) as u32);
    }
    unsafe { DefSubclassProc(hwnd, msg, wp, lp) }
}

fn guard(hwnd: HWND, locked: bool) {
    // TOOLWINDOW：不佔工作列，Alt+Tab 也不會出現；APPWINDOW 會跟它打架，一律拿掉
    let mut on = WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0;
    let mut off = WS_EX_APPWINDOW.0;
    if locked {
        on |= WS_EX_TRANSPARENT.0;
    } else {
        off |= WS_EX_TRANSPARENT.0;
    }
    unsafe {
        // 同一個 id 再呼叫一次只會更新 data，不會疊兩層
        let _ = SetWindowSubclass(hwnd, Some(style_guard), SUBCLASS_ID, (on as usize) | ((off as usize) << 32));
        // 實際套用：把關函式會照上面的規則改寫
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
    }
}

impl Layered {
    /// `hwnd` 要在**還沒顯示**之前交進來：先變成分層視窗、貼上第一張圖，
    /// 再 show。分層視窗在第一次 `UpdateLayeredWindow` 之前是看不見的。
    pub fn new(hwnd: isize) -> Self {
        let hwnd = HWND(hwnd as *mut _);
        guard(hwnd, false);
        let dc = unsafe { CreateCompatibleDC(None) };
        Self { hwnd, dc, bmp: HBITMAP::default(), old: HGDIOBJ::default(), bits: std::ptr::null_mut(), w: 0, h: 0 }
    }

    /// 鎖定＝最上層＋滑鼠穿透。穿透要 `WS_EX_TRANSPARENT`；
    /// 沒鎖的時候，alpha 為 0 的像素本來就點不到（陰影外圍會穿過去）。
    pub fn set_locked(&self, locked: bool) {
        guard(self.hwnd, locked);
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(if locked { HWND_TOPMOST } else { HWND_NOTOPMOST }),
                0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }

    fn ensure(&mut self, w: u32, h: u32) -> anyhow::Result<()> {
        if self.w == w && self.h == h && !self.bits.is_null() {
            return Ok(());
        }
        self.free_bitmap();
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                // 負的高度＝由上往下，跟 GPU 讀回來的列順序一樣，不用翻
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmp = unsafe { CreateDIBSection(Some(self.dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)? };
        self.old = unsafe { SelectObject(self.dc, bmp.into()) };
        self.bmp = bmp;
        self.bits = bits as *mut u8;
        self.w = w;
        self.h = h;
        Ok(())
    }

    fn free_bitmap(&mut self) {
        if !self.bmp.is_invalid() {
            unsafe {
                SelectObject(self.dc, self.old);
                let _ = DeleteObject(self.bmp.into());
            }
            self.bmp = HBITMAP::default();
            self.bits = std::ptr::null_mut();
        }
    }

    /// `rgba` 必須是**預乘過**的 RGBA8，由上往下。`QuadRenderer` 的輸出本來就是。
    /// `pos` 給 `None` 就留在原地（拖曳中不要跟系統搶位置）。
    #[allow(clippy::chunks_exact_to_as_chunks)] // as_chunks 改寫後借用檢查過不了，維持 chunks_exact
    pub fn present(&mut self, rgba: &[u8], w: u32, h: u32, pos: Option<(i32, i32)>) -> anyhow::Result<()> {
        anyhow::ensure!(rgba.len() == (w * h * 4) as usize, "像素數量對不上");
        self.ensure(w, h)?;
        // GDI 要 BGRA
        let dst = unsafe { std::slice::from_raw_parts_mut(self.bits, rgba.len()) };
        for (d, s) in dst.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
        let size = SIZE { cx: w as i32, cy: h as i32 };
        let src = POINT { x: 0, y: 0 };
        let dst_pt = pos.map(|(x, y)| POINT { x, y });
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                None,
                dst_pt.as_ref().map(|p| p as *const POINT),
                Some(&size),
                Some(self.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )?;
        }
        Ok(())
    }
}

impl Drop for Layered {
    fn drop(&mut self) {
        self.free_bitmap();
        unsafe {
            let _ = DeleteDC(self.dc);
        }
    }
}

// ------------------------------------------------------------------
// 兩條路：Win11 交給 DWM，Win10 走上面的分層視窗
// ------------------------------------------------------------------

/// 迷你視窗怎麼畫圓角。
///
/// - `Dwm`：Win11（build 22000 起）。一般的 wgpu 交換鏈直接畫，圓角、陰影、
///   外框都由 DWM 處理（`DWMWA_WINDOW_CORNER_PREFERENCE`），不用讀回 CPU。
///   代價是半徑固定是系統的那個值（約 8px），而且卡片是不透明的。
/// - `Layered`：Win10。離屏畫、讀回、`UpdateLayeredWindow`，圓角和陰影自己畫。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Dwm,
    Layered,
}

#[repr(C)]
struct OsVersionInfo {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    csd: [u16; 128],
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
}

/// Windows 的 build 號。不用 `GetVersionEx`：程式沒有相容性 manifest 時，
/// 它在 Win10/11 都會謊報成 Win8（6.2.9200）。
pub fn windows_build() -> u32 {
    let mut v = OsVersionInfo { size: std::mem::size_of::<OsVersionInfo>() as u32, major: 0, minor: 0, build: 0, platform: 0, csd: [0; 128] };
    if unsafe { RtlGetVersion(&mut v) } == 0 { v.build } else { 0 }
}

/// 依系統版本挑。`VITRO_MINI_BACKEND=dwm|layered` 可以強制指定，測試用。
pub fn pick_backend() -> Backend {
    match std::env::var("VITRO_MINI_BACKEND").as_deref() {
        Ok("dwm") => Backend::Dwm,
        Ok("layered") => Backend::Layered,
        _ if windows_build() >= 22000 => Backend::Dwm,
        _ => Backend::Layered,
    }
}

// ------------------------------------------------------------------
// `--mini-test [locked]`：驗證這條路走不走得通的最小原型
// ------------------------------------------------------------------

mod test_app {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use winit::application::ApplicationHandler;
    use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
    use winit::event::{ElementState, MouseButton, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::keyboard::{Key, NamedKey};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winit::window::{Window, WindowId, WindowLevel};

    use super::{Backend, Layered};
    use crate::gfx::{self, Blit, Gpu, Offscreen, OFFSCREEN_FORMAT};
    use crate::quad::{Quad, QuadRenderer};
    use crate::text::Text;

    /// 卡片本體（邏輯像素）
    const CARD_W: f32 = 360.0;
    const CARD_H: f32 = 110.0;
    const RADIUS: f32 = 16.0;
    /// 陰影要畫得下，四周留白（只有分層視窗要；DWM 的陰影畫在視窗外面）
    const PAD: f32 = 28.0;
    /// DWM 那條路的卡片底色。交換鏈是不透明的，所以不能跟分層版一樣用 0.92
    const CARD_RGB: [f32; 3] = [0.10, 0.10, 0.13];

    enum Out {
        Layered(Layered),
        Dwm { surface: wgpu::Surface<'static>, config: wgpu::SurfaceConfiguration, blit: Blit },
    }

    struct R {
        window: Arc<Window>,
        out: Out,
        gpu: Gpu,
        quads: QuadRenderer,
        text: Text,
        target: Option<Offscreen>,
    }

    struct App {
        backend: Backend,
        locked: bool,
        start: Instant,
        r: Option<R>,
        shown: bool,
    }

    impl App {
        fn draw(&mut self) -> anyhow::Result<()> {
            let Some(r) = &mut self.r else { return Ok(()) };
            let layered = matches!(r.out, Out::Layered(_));
            let s = r.window.scale_factor() as f32;
            // 分層：整張圖含陰影留白，大小由我們決定。DWM：照視窗實際大小畫滿
            let (w, h, pad) = if layered {
                (((CARD_W + PAD * 2.0) * s).round() as u32, ((CARD_H + PAD * 2.0) * s).round() as u32, PAD * s)
            } else {
                let sz = r.window.inner_size();
                (sz.width.max(1), sz.height.max(1), 0.0)
            };
            if r.target.as_ref().is_none_or(|t| t.width != w || t.height != h) {
                r.target = Some(Offscreen::new(&r.gpu.device, w, h));
            }
            let target = r.target.as_ref().unwrap();
            let t = self.start.elapsed().as_secs_f32();
            let (x0, y0) = (pad, pad);
            let (cw, ch) = (w as f32 - pad * 2.0, h as f32 - pad * 2.0);

            r.quads.begin(&r.gpu);
            if layered {
                // 卡片：深色半不透明＋陰影＋細外框（跟 CSS 版的 box-shadow 同一個寫法）
                r.quads.push(
                    Quad::new(x0, y0, cw, ch)
                        .radius(RADIUS * s)
                        .color([CARD_RGB[0], CARD_RGB[1], CARD_RGB[2], 0.92])
                        .shadow(0.0, 8.0 * s, 22.0 * s, 0.0, [0.0, 0.0, 0.0, 0.55])
                        .border(1.0 * s, [1.0, 1.0, 1.0, 0.14]),
                );
            }
            // DWM 那條：底色由清除色給，圓角、陰影、外框都是系統畫的
            // 假封面
            let art = ch - 24.0 * s;
            r.quads.push(
                Quad::new(x0 + 12.0 * s, y0 + 12.0 * s, art, art)
                    .radius(10.0 * s)
                    .color([0.85, 0.35, 0.45, 1.0]),
            );
            // 進度條，會動，順便看重繪有沒有閃
            let bx = x0 + 24.0 * s + art;
            let bw = cw - (bx - x0) - 16.0 * s;
            let by = y0 + ch - 22.0 * s;
            let p = (t * 0.1).fract();
            r.quads.push(Quad::new(bx, by, bw, 4.0 * s).radius(2.0 * s).color([1.0, 1.0, 1.0, 0.18]));
            r.quads.push(Quad::new(bx, by, bw * p, 4.0 * s).radius(2.0 * s).color([1.0, 1.0, 1.0, 0.85]));

            r.text.begin();
            r.text.push("一整面封面牆", bx, y0 + 18.0 * s, 17.0 * s, 22.0 * s,
                        [1.0, 1.0, 1.0, 0.95], 600, Some(bw), None);
            let mode = if layered { "分層" } else { "DWM" };
            let sub = if self.locked {
                format!("VitroDeck · {mode} · 已鎖定（穿透）")
            } else {
                format!("VitroDeck · {mode} · 拖曳移動、Esc 結束")
            };
            r.text.push(&sub, bx, y0 + 44.0 * s, 13.0 * s, 18.0 * s,
                        [1.0, 1.0, 1.0, 0.6], 400, Some(bw), None);
            r.text.prepare(&r.gpu, w, h)?;

            // 分層：一定要清成全透明，分層視窗看的是 alpha，殘留的底色會變成一塊板子
            let clear = if layered {
                wgpu::Color::TRANSPARENT
            } else {
                let [cr, cg, cb] = CARD_RGB;
                wgpu::Color { r: cr as f64, g: cg as f64, b: cb as f64, a: 1.0 }
            };
            let mut enc = r.gpu.device.create_command_encoder(&Default::default());
            {
                let mut pass = gfx::pass(&mut enc, &target.view, "mini", Some(clear));
                r.quads.flush(&r.gpu, &mut pass, w, h);
                r.text.render(&mut pass)?;
            }
            r.quads.upload(&r.gpu, w, h);
            r.gpu.queue.submit([enc.finish()]);

            if std::env::var_os("VITRO_MINI_DUMP").is_some() && !self.shown {
                let _ = target.save_png(&r.gpu, std::path::Path::new("mini_dump.png"));
            }
            match &mut r.out {
                Out::Layered(lay) => {
                    let rgba = target.read_rgba(&r.gpu);
                    lay.present(&rgba, w, h, None)?;
                }
                Out::Dwm { surface, config, blit } => {
                    use wgpu::CurrentSurfaceTexture as Cst;
                    if config.width != w || config.height != h {
                        config.width = w;
                        config.height = h;
                        surface.configure(&r.gpu.device, config);
                    }
                    let frame = match surface.get_current_texture() {
                        Cst::Success(f) | Cst::Suboptimal(f) => f,
                        Cst::Timeout | Cst::Occluded | Cst::Validation => return Ok(()),
                        Cst::Outdated | Cst::Lost => {
                            surface.configure(&r.gpu.device, config);
                            return Ok(());
                        }
                    };
                    let view = frame.texture.create_view(&Default::default());
                    let mut enc = r.gpu.device.create_command_encoder(&Default::default());
                    {
                        let mut pass = gfx::pass(&mut enc, &view, "mini-blit", Some(wgpu::Color::BLACK));
                        blit.draw(&r.gpu, &mut pass, &target.view);
                    }
                    r.gpu.queue.submit([enc.finish()]);
                    r.gpu.queue.present(frame);
                }
            }
            if !self.shown {
                r.window.set_visible(true);
                self.shown = true;
            }
            Ok(())
        }

        fn setup(&self, el: &ActiveEventLoop) -> anyhow::Result<R> {
            use winit::platform::windows::{CornerPreference, WindowAttributesExtWindows};
            let mut attrs = Window::default_attributes()
                .with_title("VitroDeck mini test")
                .with_decorations(false)
                .with_resizable(false)
                .with_visible(false)
                .with_skip_taskbar(true)
                .with_position(PhysicalPosition::new(200, 200));
            attrs = match self.backend {
                // 分層視窗的大小由 UpdateLayeredWindow 決定，這裡隨便給
                Backend::Layered => attrs.with_inner_size(PhysicalSize::new(400, 160)),
                // 沒有標題列的視窗，DWM 預設不給陰影也不給圓角，兩個都要明講
                Backend::Dwm => attrs
                    .with_inner_size(LogicalSize::new(CARD_W, CARD_H))
                    .with_undecorated_shadow(true)
                    .with_corner_preference(CornerPreference::Round),
            };
            let window = Arc::new(el.create_window(attrs)?);
            let RawWindowHandle::Win32(h) = window.window_handle()?.as_raw() else {
                anyhow::bail!("不是 Win32 視窗");
            };
            let hwnd = h.hwnd.get();
            println!("hwnd={hwnd} backend={:?} build={}", self.backend, super::windows_build());

            let (gpu, out) = match self.backend {
                Backend::Layered => {
                    let lay = Layered::new(hwnd);
                    lay.set_locked(self.locked);
                    (Gpu::headless()?, Out::Layered(lay))
                }
                Backend::Dwm => {
                    let instance = wgpu::Instance::default();
                    let surface = instance.create_surface(window.clone())?;
                    let gpu = Gpu::for_surface(&instance, &surface)?;
                    let sz = window.inner_size();
                    let adapter = gpu_adapter(&instance, &surface)?;
                    let mut config = surface
                        .get_default_config(&adapter, sz.width.max(1), sz.height.max(1))
                        .ok_or_else(|| anyhow::anyhow!("surface 沒有可用設定"))?;
                    // 跟主視窗同一個理由：離屏裡已經是編碼好的 sRGB，交換鏈不能再編一次
                    let plain = config.format.remove_srgb_suffix();
                    if surface.get_capabilities(&adapter).formats.contains(&plain) {
                        config.format = plain;
                    }
                    surface.configure(&gpu.device, &config);
                    let blit = Blit::new(&gpu, config.format);
                    // 穿透：winit 的作法是 WS_EX_LAYERED|WS_EX_TRANSPARENT
                    window.set_cursor_hittest(!self.locked)?;
                    window.set_window_level(if self.locked { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal });
                    (gpu, Out::Dwm { surface, config, blit })
                }
            };
            let quads = QuadRenderer::new(&gpu, OFFSCREEN_FORMAT);
            let text = Text::new(&gpu, OFFSCREEN_FORMAT);
            Ok(R { window, out, gpu, quads, text, target: None })
        }
    }

    /// `get_default_config` 要 adapter，`Gpu` 沒留著，再要一次（同一個 instance 會拿到同一張卡）
    fn gpu_adapter(instance: &wgpu::Instance, surface: &wgpu::Surface<'static>) -> anyhow::Result<wgpu::Adapter> {
        Ok(pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(surface),
            ..Default::default()
        }))?)
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, el: &ActiveEventLoop) {
            if self.r.is_some() {
                return;
            }
            match self.setup(el) {
                Ok(r) => self.r = Some(r),
                Err(e) => {
                    eprintln!("建不出迷你視窗：{e:#}");
                    el.exit();
                    return;
                }
            }
            if let Err(e) = self.draw() {
                eprintln!("第一幀失敗：{e:#}");
                el.exit();
            }
        }

        fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
            match ev {
                WindowEvent::CloseRequested => el.exit(),
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed
                        && event.logical_key == Key::Named(NamedKey::Escape) =>
                {
                    el.exit()
                }
                WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                    if let Some(r) = &self.r {
                        let _ = r.window.drag_window();
                    }
                }
                _ => {}
            }
        }

        fn about_to_wait(&mut self, el: &ActiveEventLoop) {
            // 安全閥：鎖定（穿透）時點不到也按不到 Esc，30 秒後自己結束
            if self.start.elapsed() > Duration::from_secs(30) {
                el.exit();
                return;
            }
            if let Err(e) = self.draw() {
                eprintln!("{e:#}");
            }
            el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(33)));
        }
    }

    pub fn run(locked: bool) -> anyhow::Result<()> {
        let el = EventLoop::new()?;
        let mut app = App { backend: super::pick_backend(), locked, start: Instant::now(), r: None, shown: false };
        el.run_app(&mut app)?;
        Ok(())
    }
}

pub use test_app::run as run_test;
