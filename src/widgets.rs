//! 可重用的控件：開關、滑桿、分段控制、側欄項目、按鈕、設定列與分組。
//!
//! 設定視窗（`prefs.rs`）用的。舊的面板不改用這裡 —— 它們的像素要跟基準一致。
//!
//! 每個控件分成三件事，跟 `ui.rs` 一樣守「命中框只有一份」：
//! - 版面：控件畫在給定的 `Rect` 裡，命中測試也拿同一個 `Rect`；
//! - 換算：滑鼠位置換成值（[`slider_at`]、[`segment_at`]）；
//! - 畫法：收「動畫進行中的值」與互動狀態 [`State`]，自己不記任何東西。
//!
//! 動畫值（開關的旋鈕位置、分段控制的選取塊位置）由 [`Anims`] 依控件 id 保管。
//! 截圖要停在動畫中途時，直接把值傳給畫法就好，不必經過 `Anims`。
//!
//! 尺寸全是邏輯像素（第 2 階段的規則），數字在 `theme::ctl`。

use std::collections::HashMap;

use crate::anim::{self, Ease, Tween};
use crate::gfx::Gpu;
use crate::icons::Svg;
use crate::quad::{Quad, QuadRenderer};
use crate::text::Text;
use crate::theme::{self, ctl, ty};
use crate::ui::Rect;

/// 畫控件要用的東西。包成一個才不會每個函式都拖一長串參數。
pub struct Paint<'a> {
    pub gpu: &'a Gpu,
    pub quads: &'a mut QuadRenderer,
    pub text: &'a mut Text,
    /// 捲動字（[`marquee`]）的時鐘，秒。`None` 時放不下的字停在開頭（截圖、視窗沒焦點）
    pub clock: Option<f32>,
    /// 這一幀有字在捲：呼叫端要繼續重畫
    pub rolling: bool,
}

impl<'a> Paint<'a> {
    pub fn new(gpu: &'a Gpu, quads: &'a mut QuadRenderer, text: &'a mut Text) -> Self {
        Self { gpu, quads, text, clock: None, rolling: false }
    }
}

// ---------------------------------------------------------------- 捲動字

/// 放不下的字往左捲多少：停在開頭 → 捲到尾巴露出來 → 停一下 → 捲回來，一直重複。
/// `over` 是字寬超出框的量。捲動段頭尾放慢（smoothstep），不會一下子衝出去
pub fn marquee_offset(t: f32, over: f32) -> f32 {
    if over <= 0.0 {
        return 0.0;
    }
    let (hold, run) = (ctl::MARQUEE_HOLD, over / ctl::MARQUEE_SPEED);
    let u = t.rem_euclid((hold + run) * 2.0);
    let ease = |v: f32| v * v * (3.0 - 2.0 * v);
    if u < hold {
        0.0
    } else if u < hold + run {
        over * ease((u - hold) / run)
    } else if u < hold * 2.0 + run {
        over
    } else {
        over * (1.0 - ease((u - hold * 2.0 - run) / run))
    }
}

/// 捲動中的字多久後會再動（秒）：在捲動段是 0，停在兩端時是還要停多久。
/// 不常重畫的視窗（迷你視窗）照這個排下一幀，停住的那 1.6 秒不用一直畫
pub fn marquee_wait(t: f32, over: f32) -> f32 {
    if over <= 0.0 {
        return f32::INFINITY;
    }
    let (hold, run) = (ctl::MARQUEE_HOLD, over / ctl::MARQUEE_SPEED);
    let u = t.rem_euclid((hold + run) * 2.0);
    if u < hold {
        hold - u
    } else if u < hold + run || u >= hold * 2.0 + run {
        0.0
    } else {
        hold * 2.0 + run - u
    }
}

/// 一行字畫在 `x`～`x + max_w` 裡。放得下就照常畫；放不下就裁在框裡來回捲動（取代刪節號，
/// 看得到整段字）。`clock` 是捲動的時鐘（秒），`None` 時停在開頭（截圖）。
/// `unit` 是一個邏輯像素等於幾個座標單位：主視窗是 1，迷你視窗自己乘縮放、傳縮放量，
/// 捲動速度才跟主視窗一樣。`fonts` 同 `Text::push_fonts`；`clip` 是外面另外要的裁切框，左右會跟框取交集。
///
/// 回傳 `None`＝放得下；`Some(秒)`＝在捲，多久後字會再動（見 [`marquee_wait`]）
#[allow(clippy::too_many_arguments)]
pub fn roll_line(
    text: &mut Text,
    clock: Option<f32>,
    unit: f32,
    s: &str,
    (x, y, max_w): (f32, f32, f32),
    (size, line, weight): (f32, f32, u16),
    color: [f32; 4],
    fonts: Option<(&str, &str)>,
    clip: Option<glyphon::TextBounds>,
) -> Option<f32> {
    let over = text.line_width(fonts, s, size, line, weight) - max_w;
    let (dx, bounds, wait) = if over > 0.5 {
        let base = clip.unwrap_or(glyphon::TextBounds {
            left: i32::MIN / 2,
            top: i32::MIN / 2,
            right: i32::MAX / 2,
            bottom: i32::MAX / 2,
        });
        // 左右各往外放 1px：裁切框是整數，字的邊緣才不會被多削掉一條
        let b = glyphon::TextBounds {
            left: base.left.max(x.floor() as i32 - 1),
            right: base.right.min((x + max_w).ceil() as i32 + 1),
            ..base
        };
        let o = over / unit;
        match clock {
            Some(c) => (marquee_offset(c, o) * unit, Some(b), Some(marquee_wait(c, o))),
            None => (0.0, Some(b), Some(f32::INFINITY)),
        }
    } else {
        (0.0, clip, None)
    };
    match fonts {
        Some(f) => text.push_fonts(f, s, x - dx, y, size, line, color, weight, bounds),
        None => text.push(s, x - dx, y, size, line, color, weight, None, bounds),
    }
    wait
}

/// [`roll_line`] 的設定視窗版：時鐘拿 `p.clock`，在捲的話標記 `p.rolling` 讓呼叫端繼續重畫
#[allow(clippy::too_many_arguments)]
pub fn marquee(
    p: &mut Paint,
    s: &str,
    x: f32,
    y: f32,
    max_w: f32,
    t: theme::Type,
    color: [f32; 4],
    weight: u16,
    fonts: Option<(&str, &str)>,
    clip: Option<glyphon::TextBounds>,
) {
    let wait = roll_line(p.text, p.clock, 1.0, s, (x, y, max_w), (t.size, t.line, weight), color, fonts, clip);
    if wait.is_some() && p.clock.is_some() {
        p.rolling = true;
    }
}

/// 互動狀態。`Default` 是一般、能用、沒被指著。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// 滑鼠停在上面
    pub hot: bool,
    /// 按住中
    pub down: bool,
    /// 鍵盤焦點：外面畫一圈焦點環
    pub focus: bool,
    /// 不能用：整個變淡，也不理會 hot／down
    pub disabled: bool,
}

impl State {
    fn alpha(self) -> f32 {
        if self.disabled { ctl::DISABLED_A } else { 1.0 }
    }
    fn is_hot(self) -> bool {
        self.hot && !self.disabled
    }
    fn is_down(self) -> bool {
        self.down && !self.disabled
    }
}

fn fade(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], c[3] * a]
}

fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    std::array::from_fn(|i| anim::lerp(a[i], b[i], t))
}

/// 對齊實體像素格。1px 的分隔線與刻度落在半像素上會被抗鋸齒攤成兩條淡線。
fn snap(v: f32) -> f32 {
    let k = crate::gfx::scale();
    (v * k).round() / k
}

/// 字的頂端：讓一行字垂直置中在 `cy`
fn text_top(cy: f32, line: f32) -> f32 {
    cy - line * 0.5
}

// ---------------------------------------------------------------- 動畫值

/// 每個控件的動畫值，依 id 保管。
///
/// 第一次見到的 id 直接停在目標值，不從 0 長出來 —— 打開設定視窗時，
/// 開著的開關不該全部重新滑一次。
#[derive(Default)]
pub struct Anims {
    map: HashMap<u64, Tween>,
}

impl Anims {
    /// 控件 `id` 往 `to` 補間。時長與曲線以第一次見到時為準。
    pub fn value(&mut self, id: u64, to: f32, dur: f32, ease: Ease) -> f32 {
        let t = self.map.entry(id).or_insert_with(|| Tween::new(to, dur, ease));
        t.set(to);
        t.value()
    }

    /// 開關的旋鈕位置（0 關、1 開，回彈時會稍微超過）
    pub fn switch(&mut self, id: u64, on: bool) -> f32 {
        self.value(id, if on { 1.0 } else { 0.0 }, theme::dur::SWITCH, anim::SPRING)
    }

    /// 分段控制選取塊的位置（第幾段，滑動中是小數）
    pub fn segment(&mut self, id: u64, sel: usize) -> f32 {
        self.value(id, sel as f32, theme::dur::SEGMENT, anim::SOFT)
    }

    /// 旋鈕變成玻璃的程度（0 = 平常的白色旋鈕，1 = 整顆是玻璃）。按住時往 1 走
    pub fn lens(&mut self, id: u64, held: bool) -> f32 {
        self.value(id, if held { 1.0 } else { 0.0 }, theme::dur::LENS, anim::SOFT)
    }

    /// 還有控件在動：設定視窗靠這個決定要不要繼續重畫
    pub fn moving(&self) -> bool {
        self.map.values().any(|t| t.moving())
    }
}

// ---------------------------------------------------------------- 焦點環

/// 控件外面一圈強調色，中間留一道縫。`radius` 是控件本身的圓角。
pub fn focus_ring(quads: &mut QuadRenderer, r: &Rect, radius: f32) {
    let g = ctl::FOCUS_GAP + ctl::FOCUS_W;
    let o = r.grow(g);
    quads.push(
        Quad::new(o.x, o.y, o.w, o.h)
            .radius(radius + g)
            .color([0.0; 4])
            .border(ctl::FOCUS_W, fade(theme::accent(), ctl::FOCUS_A)),
    );
}

// ---------------------------------------------------------------- 開關

/// 開關的命中框：右緣貼在 `right`、垂直置中在 `cy`。
pub fn switch_rect(right: f32, cy: f32) -> Rect {
    Rect::new(right - ctl::SWITCH_W, cy - ctl::SWITCH_H * 0.5, ctl::SWITCH_W, ctl::SWITCH_H)
}

/// 滑塊式開關。`t` 是旋鈕位置：0 關、1 開（[`Anims::switch`] 給的值）。
/// 軌道顏色跟著旋鈕走，滑到一半就是兩色之間。
/// `lens` 是旋鈕變成玻璃的程度（[`Anims::lens`]，按住時往 1 走）
pub fn switch(p: &mut Paint, r: &Rect, t: f32, st: State, lens: f32) {
    let a = st.alpha();
    let off = theme::ink(if st.is_hot() { ctl::TRACK_HOT_A } else { ctl::TRACK_A });
    let track = fade(mix(off, theme::accent(), t.clamp(0.0, 1.0)), a);
    let rad = r.h * 0.5;
    p.quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(rad).color(track));

    // 按住時旋鈕往兩側拉長一點（macOS、iOS 都這樣），行程跟著變短，兩端仍貼齊
    let d = r.h - ctl::SWITCH_INSET * 2.0;
    let kw = d + if st.is_down() { ctl::SWITCH_STRETCH } else { 0.0 };
    let travel = r.w - ctl::SWITCH_INSET * 2.0 - kw;
    let kx = r.x + ctl::SWITCH_INSET + travel * t;
    let knob = Rect::new(kx, r.y + ctl::SWITCH_INSET, kw, d);
    if lens > 0.0 {
        lens_knob(p, &knob, ctl::SWITCH_LENS_GROW, lens, a);
    } else {
        p.quads.push(
            Quad::new(knob.x, knob.y, kw, d)
                .radius(d * 0.5)
                .color(theme::knob(a))
                .shadow_of(&theme::SHADOW_CONTROL, a),
        );
    }
    if st.focus {
        focus_ring(p.quads, r, rad);
    }
}

/// 變成玻璃中的旋鈕：以原本的中心放大，白色本體漸漸變透明、只剩一圈亮環，
/// 底下的軌道就從裡面透出來。投影跟著變淡 —— 玻璃浮在控件上，不是壓在上面的實心鈕
/// `grow` 是完全變成玻璃時放大的倍數
fn lens_knob(p: &mut Paint, k: &Rect, grow: f32, lens: f32, a: f32) {
    let l = lens.clamp(0.0, 1.0);
    let g = anim::lerp(1.0, grow, l);
    let o = Rect::centred(k.cx(), k.cy(), k.w + k.h * (g - 1.0), k.h * g);
    p.quads.push(
        Quad::new(o.x, o.y, o.w, o.h)
            .radius(o.h * 0.5)
            .color(theme::knob(anim::lerp(1.0, ctl::LENS_FILL_A, l) * a))
            .border(1.0, theme::knob(ctl::LENS_RIM_A * l * a))
            .shadow_of(&theme::SHADOW_CONTROL, a * (1.0 - 0.7 * l)),
    );
}

// ---------------------------------------------------------------- 滑桿

/// 滑桿的命中框：從 `x` 開始、寬 `w`、垂直置中在 `cy`。
pub fn slider_rect(x: f32, cy: f32, w: f32) -> Rect {
    Rect::new(x, cy - ctl::SLIDER_H * 0.5, w, ctl::SLIDER_H)
}

/// 旋鈕中心能走的範圍。兩端各內縮半顆旋鈕，拉到底也不會凸出命中框。
fn knob_span(r: &Rect) -> (f32, f32) {
    let h = ctl::KNOB * 0.5;
    (r.x + h, (r.x + r.w - h).max(r.x + h))
}

/// 滑鼠 `x` 換成 0..1。`steps` 是刻度數（含兩端），有的話吸到最近的刻度。
/// 拖曳時游標會跑出滑桿，所以夾住。
pub fn slider_at(r: &Rect, x: f32, steps: Option<usize>) -> f32 {
    let (a, b) = knob_span(r);
    let v = ((x - a) / (b - a).max(1.0)).clamp(0.0, 1.0);
    match steps {
        Some(n) if n >= 2 => {
            let m = (n - 1) as f32;
            (v * m).round() / m
        }
        _ => v,
    }
}

/// 有刻度的滑桿：`v` 落在第幾格
pub fn slider_step(v: f32, n: usize) -> usize {
    if n < 2 {
        return 0;
    }
    ((v.clamp(0.0, 1.0) * (n - 1) as f32).round() as usize).min(n - 1)
}

/// 滑桿。`v` 是 0..1；`steps` 有值的話在軌道下面畫刻度（刻度會畫到命中框外面，
/// 排版時列高要留得夠）。軌道填滿到旋鈕中心。
/// `lens` 是旋鈕變成玻璃的程度（拖曳時往 1 走）：變成玻璃後，透過它看得到底下放大的軌道
pub fn slider(p: &mut Paint, r: &Rect, v: f32, steps: Option<usize>, st: State, lens: f32) {
    let a = st.alpha();
    let (x0, x1) = knob_span(r);
    let (cy, th) = (r.cy(), ctl::TRACK_H);
    let kx = anim::lerp(x0, x1, v.clamp(0.0, 1.0));
    let track_a = if st.is_hot() { ctl::TRACK_HOT_A } else { ctl::TRACK_A };
    let (gray, fill) = (theme::ink(track_a * a), fade(theme::accent(), a));
    let l = lens.clamp(0.0, 1.0);
    if l > 0.0 {
        // 軌道在玻璃外面的兩截照常畫，玻璃裡面那段換成放大過的
        let rr = ctl::KNOB * 0.5 * anim::lerp(1.0, ctl::LENS_GROW, l);
        let (gl, gr) = (kx - rr, kx + rr);
        if gl > r.x {
            p.quads.push(Quad::new(r.x, cy - th * 0.5, gl - r.x, th).radius(th * 0.5).color(fill));
        }
        if gr < r.x + r.w {
            p.quads.push(Quad::new(gr, cy - th * 0.5, r.x + r.w - gr, th).radius(th * 0.5).color(gray));
        }
        // 放大的那段只畫到玻璃的弦長，兩端才不會凸出圓外
        let mh = th * anim::lerp(1.0, ctl::LENS_MAG, l);
        let cw = ((rr * rr - mh * mh * 0.25).max(0.0).sqrt() - 1.0).max(0.0);
        p.quads.push(Quad::new(kx - cw, cy - mh * 0.5, cw * 2.0, mh).radius(mh * 0.5).color(gray));
        p.quads.push(Quad::new(kx - cw, cy - mh * 0.5, cw, mh).radius(mh * 0.5).color(fill));
    } else {
        p.quads.push(Quad::new(r.x, cy - th * 0.5, r.w, th).radius(th * 0.5).color(gray));
        p.quads.push(Quad::new(r.x, cy - th * 0.5, (kx - r.x).max(th), th).radius(th * 0.5).color(fill));
    }
    if let Some(n) = steps.filter(|n| *n >= 2) {
        let ty0 = cy + ctl::KNOB * 0.5 + ctl::TICK_GAP;
        for i in 0..n {
            let tx = anim::lerp(x0, x1, i as f32 / (n - 1) as f32);
            p.quads.push(
                Quad::new(snap(tx - ctl::TICK_W * 0.5), snap(ty0), ctl::TICK_W, ctl::TICK_H)
                    .radius(ctl::TICK_W * 0.5)
                    .color(theme::ink(ctl::TICK_A * a)),
            );
        }
    }
    let d = ctl::KNOB;
    let knob = Rect::centred(kx, cy, d, d);
    if l > 0.0 {
        lens_knob(p, &knob, ctl::LENS_GROW, l, a);
    } else {
        p.quads.push(
            Quad::new(knob.x, knob.y, d, d)
                .radius(d * 0.5)
                .color(theme::knob(a))
                .shadow_of(&theme::SHADOW_CONTROL, a),
        );
    }
    if st.focus {
        focus_ring(p.quads, &knob, d * 0.5);
    }
}

// ---------------------------------------------------------------- 分段控制

/// 把 `r` 扣掉內縮後等分成 `n` 段，回傳每段的範圍（選取塊就畫在這裡）。
pub fn segments(r: &Rect, n: usize) -> Vec<Rect> {
    let n = n.max(1);
    let i = ctl::SEG_INSET;
    let w = (r.w - i * 2.0) / n as f32;
    (0..n).map(|k| Rect::new(r.x + i + w * k as f32, r.y + i, w, r.h - i * 2.0)).collect()
}

/// 點在第幾段。內縮的那圈也算進最近的那段，邊緣不會點不到。
pub fn segment_at(r: &Rect, n: usize, x: f32, y: f32) -> Option<usize> {
    if n == 0 || !r.contains(x, y) {
        return None;
    }
    let w = (r.w - ctl::SEG_INSET * 2.0) / n as f32;
    Some((((x - r.x - ctl::SEG_INSET) / w).floor().max(0.0) as usize).min(n - 1))
}

/// 自然寬度：每段都跟最寬的那段一樣寬
pub fn segmented_width(text: &mut Text, labels: &[&str]) -> f32 {
    let widest = labels
        .iter()
        .map(|s| text.measure_weight(s, ty::CAPTION.size, ty::CAPTION.line, None, 500).0)
        .fold(0.0f32, f32::max);
    (widest + ctl::SEG_PAD * 2.0) * labels.len() as f32 + ctl::SEG_INSET * 2.0
}

/// 分段控制。`sel` 是選取塊的位置（第幾段，滑動中是小數）、`hot` 是被指著的那段。
pub fn segmented(p: &mut Paint, r: &Rect, labels: &[&str], sel: f32, hot: Option<usize>, st: State) {
    let n = labels.len();
    if n == 0 {
        return;
    }
    let a = st.alpha();
    // 膠囊；選取塊與外框同心
    let outer = r.h * 0.5;
    let inner = theme::radius::concentric(outer, ctl::SEG_INSET);
    p.quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(outer).color(theme::ink(ctl::SEG_BG_A * a)));
    let segs = segments(r, n);
    let sel = sel.clamp(0.0, (n - 1) as f32);
    let near = sel.round() as usize;

    if let Some(h) = hot.filter(|h| *h < n && *h != near && st.is_hot()) {
        let s = segs[h];
        p.quads.push(Quad::new(s.x, s.y, s.w, s.h).radius(inner).color(theme::ink(ctl::SEG_HOT_A)));
    }
    // 分隔線只畫在兩段都沒被選到的縫上（macOS 的做法，選取塊旁邊不會多一條線）
    for (k, s) in segs.iter().enumerate().skip(1) {
        if k == near || k == near + 1 {
            continue;
        }
        let x = snap(s.x - 0.5);
        p.quads.push(Quad::new(x, r.y + 7.0, 1.0, r.h - 14.0).color(theme::ink(ctl::SEG_DIV_A * a)));
    }
    let s0 = segs[0];
    let sx = s0.x + s0.w * sel;
    p.quads.push(
        Quad::new(sx, s0.y, s0.w, s0.h)
            .radius(inner)
            // 淺色模式是不透明的白塊（macOS 淺色的分段控制）
            .color(theme::pick(theme::ink(ctl::SEG_SEL_A * a), theme::knob(a)))
            .shadow_of(&theme::SHADOW_CONTROL, a),
    );
    // 字的亮度跟著選取塊離它多近走，滑動中是漸變的
    for (k, (s, label)) in segs.iter().zip(labels).enumerate() {
        let near_k = (1.0 - (k as f32 - sel).abs()).clamp(0.0, 1.0);
        let c = fade(mix(theme::soft(), theme::fg(), near_k), a);
        let (tw, _) = p.text.measure_weight(label, ty::CAPTION.size, ty::CAPTION.line, None, 500);
        p.text.push(label, s.cx() - tw * 0.5, text_top(s.cy(), ty::CAPTION.line), ty::CAPTION.size,
                    ty::CAPTION.line, c, 500, None, None);
    }
    if st.focus {
        focus_ring(p.quads, r, outer);
    }
}

// ---------------------------------------------------------------- 側欄項目

/// 側欄的第 `i` 項（從 `top` 往下排，每項之間不留縫）
pub fn side_rect(x: f32, top: f32, w: f32, i: usize) -> Rect {
    Rect::new(x, top + i as f32 * ctl::SIDE_H, w, ctl::SIDE_H)
}

/// 側欄項目：選取的那項有底、字亮；指著的有淡底。
pub fn side_item(p: &mut Paint, r: &Rect, icon: Option<Svg>, label: &str, selected: bool, st: State) {
    side_item_with(p, r, icon.map(SideIcon::Plain), label, selected, st);
}

/// 側欄項目，圖示畫在彩色圓角方塊裡（macOS 系統設定的樣子）。`color` 是方塊的顏色
pub fn side_item_tile(p: &mut Paint, r: &Rect, icon: Svg, color: [f32; 3], label: &str, selected: bool, st: State) {
    side_item_with(p, r, Some(SideIcon::Tile(icon, color)), label, selected, st);
}

/// 圓角方塊裡一個白色圖案，中心在 (cx, cy)。圓角與圖案大小照 `ctl::TILE` 的比例放大
pub fn tile(p: &mut Paint, icon: Svg, color: [f32; 3], cx: f32, cy: f32, size: f32, a: f32) {
    let r = Rect::centred(cx, cy, size, size);
    p.quads.push(
        Quad::new(snap(r.x), snap(r.y), size, size)
            .radius(size * ctl::TILE_R / ctl::TILE)
            .color([color[0], color[1], color[2], a]),
    );
    let glyph = size * ctl::TILE_GLYPH / ctl::TILE;
    p.quads.svg(p.gpu, icon, glyph, cx, cy, [1.0, 1.0, 1.0, a]);
}

enum SideIcon {
    /// 單色圖示，跟字同色
    Plain(Svg),
    /// 彩色方塊＋白色圖案
    Tile(Svg, [f32; 3]),
}

fn side_item_with(p: &mut Paint, r: &Rect, icon: Option<SideIcon>, label: &str, selected: bool, st: State) {
    let a = st.alpha();
    // 跟側欄同心：側欄圓角扣掉項目離側欄邊的距離
    let rad = theme::radius::concentric(theme::radius::SIDEBAR, theme::win::SIDE_PAD);
    let bg = if selected {
        Some(ctl::SIDE_SEL_A)
    } else if st.is_hot() {
        Some(ctl::SIDE_HOT_A)
    } else {
        None
    };
    if let Some(b) = bg {
        p.quads.push(Quad::new(r.x, r.y + 1.0, r.w, r.h - 2.0).radius(rad).color(theme::ink(b * a)));
    }
    let c = fade(if selected { theme::fg() } else { theme::soft() }, a);
    let mut x = r.x + 10.0;
    match icon {
        Some(SideIcon::Plain(ic)) => {
            p.quads.svg(p.gpu, ic, ctl::SIDE_ICON, x + ctl::SIDE_ICON * 0.5, r.cy(), c);
            x += ctl::SIDE_ICON + 10.0;
        }
        // 方塊比單色圖示大，往左多佔一點，字的起點才不會離邊太遠
        Some(SideIcon::Tile(ic, col)) => {
            let x0 = r.x + 6.0;
            tile(p, ic, col, x0 + ctl::TILE * 0.5, r.cy(), ctl::TILE, a);
            x = x0 + ctl::TILE + 9.0;
        }
        None => {}
    }
    marquee(p, label, x, text_top(r.cy(), ty::BODY_S.line), r.x + r.w - 8.0 - x, ty::BODY_S, c, 500, None, None);
    if st.focus {
        focus_ring(p.quads, r, rad);
    }
}

// ---------------------------------------------------------------- 按鈕

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 一般：淡淡的墨色底
    Normal,
    /// 主要動作：強調色底
    Primary,
    /// 會刪東西：紅底
    Danger,
}

/// 按鈕的自然寬度（字寬加左右留白）
pub fn button_width(text: &mut Text, label: &str) -> f32 {
    text.measure_weight(label, ty::CAPTION.size, ty::CAPTION.line, None, 600).0 + ctl::BTN_PAD * 2.0
}

/// 文字按鈕，膠囊形。高度照 `r` 畫（標準是 `ctl::BTN_H`）。
pub fn button(p: &mut Paint, r: &Rect, label: &str, kind: Kind, st: State) {
    let a = st.alpha();
    // 平常、指著、按住三階
    let lvl = if st.is_down() { 2 } else if st.is_hot() { 1 } else { 0 };
    let (bg, fg) = match kind {
        Kind::Normal => (theme::ink(ctl::BTN_A[lvl]), theme::ink(0.92)),
        // 指著時亮一階（綠的時候跟設定精靈的主要鈕一樣），按住時壓暗
        Kind::Primary => (
            [theme::accent(), theme::accent_hot(), mix(theme::accent(), theme::scrim(1.0), 0.18)][lvl],
            theme::on_accent(),
        ),
        Kind::Danger => (theme::danger(ctl::BTN_DANGER_A[lvl]), theme::ink(0.92)),
    };
    let rad = r.h * 0.5;
    p.quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(rad).color(fade(bg, a)));
    // 放得下就置中；放不下就在按鈕裡捲（左右各留半個留白）
    let max_w = r.w - ctl::BTN_PAD;
    let tw = p.text.line_width(None, label, ty::CAPTION.size, ty::CAPTION.line, 600);
    let x = if tw <= max_w { r.cx() - tw * 0.5 } else { r.x + ctl::BTN_PAD * 0.5 };
    marquee(p, label, x, text_top(r.cy(), ty::CAPTION.line), max_w, ty::CAPTION, fade(fg, a), 600, None, None);
    if st.focus {
        focus_ring(p.quads, r, rad);
    }
}

/// 只有圖示的圓形按鈕（字型清單的返回鈕）。底色跟一般文字按鈕同三階
pub fn icon_button(p: &mut Paint, r: &Rect, icon: Svg, st: State) {
    let a = st.alpha();
    let lvl = if st.is_down() { 2 } else if st.is_hot() { 1 } else { 0 };
    let rad = r.h * 0.5;
    p.quads.push(Quad::new(r.x, r.y, r.w, r.h).radius(rad).color(fade(theme::ink(ctl::BTN_A[lvl]), a)));
    p.quads.svg(p.gpu, icon, ctl::SIDE_ICON, r.cx(), r.cy(), fade(theme::ink(0.92), a));
    if st.focus {
        focus_ring(p.quads, r, rad);
    }
}

// ---------------------------------------------------------------- 設定列與分組

/// 字級倍率放大時，一行字多出來的高度（縮小時列不跟著縮）
fn grow(line: f32) -> f32 {
    line * (crate::text::pref_scale().max(1.0) - 1.0)
}

/// 設定列的高度。字級放大時跟著加高
pub fn row_h(has_hint: bool) -> f32 {
    if has_hint {
        ctl::ROW_H_HINT + grow(ty::BODY_S.line) + grow(ty::CAPTION.line)
    } else {
        ctl::ROW_H + grow(ty::BODY_S.line)
    }
}

/// 控件排到標籤下面一行的設定列（字級放大、標籤擠到幾乎看不見時）。
/// 上面那段照一般列的高度放標籤，下面多一段 `ROW_STACK_BAND` 放控件，再留一點底
pub fn row_h_stacked(has_hint: bool) -> f32 {
    row_h(has_hint) - 4.0 + ctl::ROW_STACK_BAND + 8.0
}

/// 控件排到下一行時，控件那一段的中線
pub fn stacked_ctrl_cy(r: &Rect, has_hint: bool) -> f32 {
    r.y + row_h(has_hint) - 4.0 + ctl::ROW_STACK_BAND * 0.5
}

/// 標籤要不要讓控件排到下一行：標籤放不下，而且露出來的不到 `LABEL_MIN_EM` 個字寬
pub fn needs_stack(text: &mut Text, label: &str, label_w: f32) -> bool {
    let b = ty::BODY_S;
    let natural = text.line_width(None, label, b.size, b.line, 500);
    natural > label_w && label_w < ctl::LABEL_MIN_EM * b.size * text.font_scale()
}

/// 一組設定列由上往下排：回傳整組的範圍與每一列的範圍。列與列之間不留縫，
/// 分隔線畫在交界上。
pub fn group_rows(x: f32, y: f32, w: f32, heights: &[f32]) -> (Rect, Vec<Rect>) {
    let mut rows = Vec::with_capacity(heights.len());
    let mut cy = y;
    for h in heights {
        rows.push(Rect::new(x, cy, w, *h));
        cy += h;
    }
    (Rect::new(x, y, w, cy - y), rows)
}

/// 分組的底與列之間的分隔線。分隔線從標籤的左緣開始（macOS 的分組清單都這樣）。
pub fn group(p: &mut Paint, g: &Rect, rows: &[Rect]) {
    p.quads.push(Quad::new(g.x, g.y, g.w, g.h).radius(theme::radius::GROUP).color(theme::ink(ctl::GROUP_BG_A)));
    for r in rows.iter().skip(1) {
        p.quads.push(
            Quad::new(r.x + ctl::ROW_PAD, snap(r.y - 0.5), r.w - ctl::ROW_PAD * 2.0, 1.0).color(theme::ink(ctl::DIV_A)),
        );
    }
}

/// 分組上方的小標題。`g` 是分組的範圍，字的底離它 `GROUP_TITLE_GAP`。
pub fn group_title(p: &mut Paint, g: &Rect, label: &str) {
    let c = ty::CAPTION;
    // 字放大時行高也放大，頂端往上移，`lift` 補回 `push` 為了保持中線而上移的那半格
    let y = g.y - ctl::GROUP_TITLE_GAP - c.line * p.text.font_scale() + p.text.lift(c.line);
    p.text.push(label, g.x + ctl::ROW_PAD, y, c.size, c.line, theme::ink(0.55), 600, None, None);
}

/// 分組標題佔掉的高度（排版時往下推這麼多才放分組）。字級放大時跟著加高
pub fn group_title_h() -> f32 {
    ctl::GROUP_TITLE_GAP + ty::CAPTION.line + grow(ty::CAPTION.line)
}

/// 列右側控件的右緣與中線。控件自己靠 `switch_rect` 之類的函式對齊這兩個值。
pub fn row_ctrl(r: &Rect) -> (f32, f32) {
    (r.x + r.w - ctl::ROW_PAD, r.cy())
}

/// 列左邊的標籤與說明。各一行，比 `max_w`（呼叫端扣掉右側控件的寬度）長就在裡面來回捲動
pub fn row_label(p: &mut Paint, r: &Rect, label: &str, hint: &str, max_w: f32, st: State) {
    let a = st.alpha();
    let x = r.x + ctl::ROW_PAD;
    let (b, c) = (ty::BODY_S, ty::CAPTION);
    if hint.is_empty() {
        marquee(p, label, x, text_top(r.cy(), b.line), max_w, b, theme::ink(0.92 * a), 500, None, None);
        return;
    }
    let (top, hint_y) = label_lines(p.text, r);
    marquee(p, label, x, top, max_w, b, theme::ink(0.92 * a), 500, None, None);
    marquee(p, hint, x, hint_y, max_w, c, theme::ink(0.5 * a), 400, None, None);
}

/// 有說明的列：標籤與說明兩行一起垂直置中，回傳兩行交給 `push` 的 y。
/// 行高跟著字級倍率放大；`push` 會讓放大的字留在原本那行的中線，這裡要頂端對齊，加 `lift` 補回來
fn label_lines(text: &Text, r: &Rect) -> (f32, f32) {
    let k = text.font_scale();
    let (b, c) = (ty::BODY_S, ty::CAPTION);
    let top = r.cy() - (b.line * k + 2.0 + c.line * k) * 0.5;
    (top + text.lift(b.line), top + b.line * k + 2.0 + text.lift(c.line))
}

/// 徽章的字級（比說明再小一號）
const BADGE: theme::Type = theme::Type { size: 11.0, line: 14.0 };

/// 標籤右邊的小徽章（「淺色實驗中」之類）。位置跟 `row_label` 的標籤同一行，
/// 淡金底配墨色字：金色的字在淺色底上對比不夠
pub fn row_badge(p: &mut Paint, r: &Rect, label: &str, has_hint: bool, badge: &str) {
    // 標籤那一行的中線
    let cy = if has_hint { label_lines(p.text, r).0 + ty::BODY_S.line * 0.5 } else { r.cy() };
    let (lw, _) = p.text.measure_weight(label, ty::BODY_S.size, ty::BODY_S.line, None, 500);
    let (bw, _) = p.text.measure_weight(badge, BADGE.size, BADGE.line, None, 600);
    let (w, h) = (bw + 14.0, BADGE.line + 4.0);
    let x = r.x + ctl::ROW_PAD + lw + 8.0;
    p.quads.push(Quad::new(x, snap(cy - h * 0.5), w, h).radius(h * 0.5).color(theme::warn(0.24)));
    p.text.push(badge, x + 7.0, text_top(cy, BADGE.line), BADGE.size, BADGE.line, theme::ink(0.8), 600, None, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 捲動字停在開頭_捲到尾_停一下_再捲回來() {
        let (hold, over) = (ctl::MARQUEE_HOLD, 64.0);
        let run = over / ctl::MARQUEE_SPEED;
        assert_eq!(marquee_offset(0.0, over), 0.0);
        assert_eq!(marquee_offset(hold * 0.9, over), 0.0, "開頭先停一下");
        assert!((marquee_offset(hold + run * 0.5, over) - over * 0.5).abs() < 1e-3, "捲動段的中間走到一半");
        assert_eq!(marquee_offset(hold + run + hold * 0.5, over), over, "捲到尾巴停住");
        // 一輪之後回到開頭；放得下的字不動
        let period = (hold + run) * 2.0;
        assert!(marquee_offset(period + 0.01, over).abs() < 1e-3);
        assert_eq!(marquee_offset(5.0, 0.0), 0.0);
        // 位置連續，不會跳
        let mut prev = 0.0;
        for i in 1..2000 {
            let v = marquee_offset(i as f32 * period / 1000.0, over);
            assert!((v - prev).abs() < 1.0, "第 {i} 步跳了 {}", v - prev);
            prev = v;
        }
    }

    #[test]
    fn 滑桿兩端與刻度() {
        let r = slider_rect(100.0, 50.0, 220.0);
        assert_eq!(slider_at(&r, 0.0, None), 0.0);
        assert_eq!(slider_at(&r, 999.0, None), 1.0);
        // 旋鈕中心的行程兩端各內縮半顆
        assert_eq!(slider_at(&r, 100.0 + ctl::KNOB * 0.5, None), 0.0);
        assert!((slider_at(&r, r.cx(), None) - 0.5).abs() < 1e-6);
        // 5 格：0、.25、.5、.75、1，吸到最近的那格
        let v = slider_at(&r, r.cx() + 20.0, Some(5));
        assert_eq!(v, 0.5);
        assert_eq!(slider_step(v, 5), 2);
        assert_eq!(slider_step(1.0, 8), 7);
        assert_eq!(slider_step(0.5, 1), 0);
    }

    #[test]
    fn 分段點到哪段就是哪段() {
        let r = Rect::new(0.0, 0.0, 302.0, ctl::SEG_H);
        let segs = segments(&r, 3);
        assert_eq!(segs.len(), 3);
        assert!((segs[2].x + segs[2].w - (302.0 - ctl::SEG_INSET)).abs() < 1e-4, "最後一段貼齊右邊的內縮");
        for (k, s) in segs.iter().enumerate() {
            assert_eq!(segment_at(&r, 3, s.cx(), s.cy()), Some(k));
        }
        // 內縮的那圈也點得到
        assert_eq!(segment_at(&r, 3, 0.5, 14.0), Some(0));
        assert_eq!(segment_at(&r, 3, 301.5, 14.0), Some(2));
        assert_eq!(segment_at(&r, 3, 400.0, 14.0), None);
        assert_eq!(segment_at(&r, 0, 10.0, 14.0), None);
    }

    #[test]
    fn 設定列由上往下接起來() {
        let (g, rows) = group_rows(10.0, 20.0, 400.0, &[row_h(false), row_h(true), row_h(false)]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].y, rows[0].y + rows[0].h);
        assert_eq!(g.h, ctl::ROW_H * 2.0 + ctl::ROW_H_HINT);
        // 右側控件貼在列的右內距上、在列裡面
        let (right, cy) = row_ctrl(&rows[1]);
        let s = switch_rect(right, cy);
        assert!(s.x > rows[1].x && s.x + s.w <= rows[1].x + rows[1].w);
        assert!(s.y >= rows[1].y && s.y + s.h <= rows[1].y + rows[1].h);
    }

    #[test]
    fn 第一次見到的控件直接停在目標() {
        let mut an = Anims::default();
        assert_eq!(an.switch(1, true), 1.0);
        assert_eq!(an.segment(2, 3), 3.0);
        assert!(!an.moving());
        // 換目標之後就在動了，而且從原本的值出發
        let v = an.switch(1, false);
        assert!(v > 0.5, "剛換目標還在起點附近：{v}");
        assert!(an.moving());
    }

    #[test]
    fn 停用的控件不理會滑鼠() {
        let st = State { hot: true, down: true, focus: false, disabled: true };
        assert!(!st.is_hot() && !st.is_down());
        assert_eq!(st.alpha(), ctl::DISABLED_A);
        assert_eq!(State::default().alpha(), 1.0);
    }
}
