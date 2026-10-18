//! 歌詞頁的黑膠唱盤 —— Python 版 `web/vinyl.js` 的移植。
//!
//! 唱片跟著播放轉，磁頭跟著進度由外圈走到內圈。磁頭角度照真實唱盤的
//! 幾何算，不是拿進度線性插角度：溝紋等距、唱盤等角速度，所以「唱針所在
//! 半徑」隨時間線性下降，再用餘弦定理把半徑換成以支點為軸的轉角。
//!
//! 換歌、跳進度、剛進頁面都不必各寫一套流程 —— 磁頭有最大角速度，
//! 目標一跳遠它就自己「抬起 → 擺過去 → 放下」；暫停就原地抬起，唱片慢慢停。
//!
//! 座標是封面那格的百分比（邊長 100），跟 Python 版 `style.css` 的
//! `#np-disc`、`#np-arm` 以及 `index.html` 的 `#arm-shape` 是同一組數字。
//! 改一邊要改另一邊。

use std::sync::Arc;

use crate::quad::{Quad, QuadRenderer};

// 唱片中心與半徑、磁頭支點。臂長取支點到軸心的距離，
// 唱針畫在 (90, 10 + L) = (90, 76.47)
const CX: f32 = 43.0;
const CY: f32 = 57.0;
const R: f32 = 43.0;
const PX: f32 = 90.0;
const PY: f32 = 10.0;

const R_OUTER: f32 = 0.95 * R; // 第一條溝
const R_INNER: f32 = 0.66 * R; // 最後一條溝，標籤（62%）外緣再往外一點
const REST: f32 = -2.0; // 休息位：唱針在唱片右側外面

const ARM_SPEED: f32 = 45.0; // 磁頭最大角速度，°/s
const LIFT_EPS: f32 = 0.5; // 離目標超過這麼多度就算在移動，要抬起
const SPIN_UP_S: f32 = 0.6; // 起轉約這麼久到速
const SPIN_DOWN_S: f32 = 1.5; // 停轉比較慢，唱盤有慣性
/// 抬起／放下的過渡。CSS 那邊是 0.28 秒的 transition
const LIFT_S: f32 = 0.28;

fn dist() -> f32 {
    (CX - PX).hypot(CY - PY)
}

/// 唱針落在半徑 `r` 時，磁頭要轉幾度。取往右外側那一支解。
pub fn arm_for(r: f32) -> f32 {
    let d = dist();
    let l = d;
    let c = (d * d + l * l - r * r) / (2.0 * d * l);
    let phi = c.clamp(-1.0, 1.0).acos().to_degrees();
    let base = (CY - PY).atan2(CX - PX).to_degrees(); // 支點指向軸心的方向
    base - phi - 90.0 // 圖上的磁頭朝 90°（正下方）
}

pub struct Vinyl {
    /// 唱片轉角（°）與角速度（°/s）
    pub angle: f32,
    omega: f32,
    /// 磁頭轉角，0 = 圖上筆直朝下的姿勢
    pub arm: f32,
    /// 進頁面後唱針放下過沒；放下之前唱片不轉
    engaged: bool,
    lifted: bool,
    /// 抬起的程度 0..1，畫的時候用（`lifted` 是開關，這個是過渡）
    pub lift: f32,
}

impl Default for Vinyl {
    fn default() -> Self {
        Self { angle: 0.0, omega: 0.0, arm: REST, engaged: false, lifted: false, lift: 0.0 }
    }
}

impl Vinyl {
    /// 進歌詞頁：磁頭從休息位出發、唱片從靜止開始。唱片轉角留著，
    /// 跟 Python 版一樣（它只重設磁頭與速度）。
    pub fn enter(&mut self) {
        let angle = self.angle;
        *self = Self { angle, ..Default::default() };
    }

    /// 推進一幀。`frac` 是播放進度 0..1；`has` = 有歌（暫停也算）。
    pub fn step(&mut self, dt: f32, has: bool, playing: bool, frac: f32, rpm: f32) {
        // 視窗被蓋住回來那一幀的 dt 很大；夾住免得磁頭瞬移
        let dt = dt.clamp(0.0, 0.1);
        let playing = has && playing;
        let want = if has { arm_for(R_OUTER + (R_INNER - R_OUTER) * frac.clamp(0.0, 1.0)) } else { REST };

        let step = ARM_SPEED * dt;
        self.arm += (want - self.arm).clamp(-step, step);
        let moving = (want - self.arm).abs() > LIFT_EPS;
        let at_rest = !has && !moving;
        self.lifted = !at_rest && (!playing || moving);
        if !has {
            self.engaged = false;
        } else if playing && !self.lifted {
            self.engaged = true;
        }

        let w = if playing && self.engaged { rpm.clamp(1.0, 45.0) * 6.0 } else { 0.0 };
        let tau = (if w > self.omega { SPIN_UP_S } else { SPIN_DOWN_S }) / 3.0;
        self.omega += (w - self.omega) * (1.0 - (-dt / tau).exp());
        if self.omega.abs() < 0.01 && w == 0.0 {
            self.omega = 0.0;
        }
        self.angle = (self.angle + self.omega * dt) % 360.0;

        let goal = if self.lifted { 1.0 } else { 0.0 };
        self.lift += (goal - self.lift) * (1.0 - (-dt / (LIFT_S / 3.0)).exp());
    }

    /// 還在動嗎（唱片在轉、磁頭在擺、抬放還沒做完）。不動的話畫面可以不重畫。
    #[allow(dead_code)]
    pub fn busy(&self) -> bool {
        self.omega != 0.0 || self.lifted != (self.lift > 0.5) || (self.lift - self.lift.round()).abs() > 0.01
    }
}

fn rgb(hex: u32, a: f32) -> [f32; 4] {
    let c = |s: u32| ((hex >> s) & 0xff) as f32 / 255.0;
    [c(16), c(8), c(0), a]
}

/// 把封面那格的百分比座標換成像素，順便把磁頭的旋轉與抬起放大套上去。
struct Frame {
    x: f32,
    y: f32,
    /// 一個百分比單位是幾像素
    u: f32,
    /// 繞支點轉幾度（CSS 的 rotate：正值順時針，畫面 y 朝下）
    rot: f32,
    scale: f32,
    /// 整組平移（影子用），像素
    off: [f32; 2],
}

impl Frame {
    fn map(&self, p: [f32; 2]) -> [f32; 2] {
        let (s, c) = self.rot.to_radians().sin_cos();
        let (dx, dy) = ((p[0] - PX) * self.scale, (p[1] - PY) * self.scale);
        let (rx, ry) = (dx * c - dy * s, dx * s + dy * c);
        [
            self.x + (PX + rx) * self.u + self.off[0],
            self.y + (PY + ry) * self.u + self.off[1],
        ]
    }

    /// 以 `c` 為中心、寬高 `w`×`h`（百分比單位）、自己再轉 `own` 度的矩形。
    fn rect(&self, c: [f32; 2], w: f32, h: f32, own: f32) -> Quad {
        let p = self.map(c);
        let (w, h) = (w * self.u * self.scale, h * self.u * self.scale);
        Quad::new(p[0] - w * 0.5, p[1] - h * 0.5, w, h).rotate((own + self.rot).to_radians())
    }

    /// 從 `a` 到 `b`、寬 `w` 的圓頭線段 —— SVG 的 `stroke-linecap: round`。
    fn line(&self, a: [f32; 2], b: [f32; 2], w: f32, own: f32, pivot: [f32; 2]) -> Quad {
        let (a, b) = (rot_about(a, pivot, own), rot_about(b, pivot, own));
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy);
        // 讓矩形的長邊（本來朝 +y）對齊 a→b
        let th = (-dx).atan2(dy).to_degrees();
        let c = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        self.rect(c, w, len + w, th).radius(w * 0.5 * self.u * self.scale)
    }
}

fn rot_about(p: [f32; 2], o: [f32; 2], deg: f32) -> [f32; 2] {
    if deg == 0.0 {
        return p;
    }
    let (s, c) = deg.to_radians().sin_cos();
    let (dx, dy) = (p[0] - o[0], p[1] - o[1]);
    [o[0] + dx * c - dy * s, o[1] + dx * s + dy * c]
}

/// 磁頭（`index.html` 的 `#arm-shape`）。`shadow` 的話整組畫成黑色剪影。
fn arm(quads: &mut QuadRenderer, f: &Frame, shadow: Option<f32>) {
    // 影子：本體透明、只留模糊的剪影。疊在一起的地方會深一點，看不出來
    let blur = 3.0 * f.u / 4.0;
    let paint = |q: Quad, fill: [f32; 4], stroke: Option<(f32, [f32; 4])>| -> Quad {
        match shadow {
            Some(a) => q.color([0.0; 4]).shadow(0.0, 0.0, blur, 0.0, [0.0, 0.0, 0.0, a]),
            None => match stroke {
                Some((w, c)) => q.color(fill).border(w * f.u * f.scale, c),
                None => q.color(fill),
            },
        }
    };
    let k = f.u * f.scale;
    // 配重
    quads.push(paint(
        f.rect([90.0, 3.75], 8.0, 6.5, 0.0).radius(1.6 * k),
        rgb(0x3a3c43, 1.0),
        Some((0.4, rgb(0x6b6e76, 1.0))),
    ));
    // 臂管：M90 7 V60 Q90 64 88.03 67.22。曲線那段拆成兩截直線就夠了
    let tube = [[90.0, 7.0], [90.0, 60.0], [89.51, 63.8], [88.03, 67.22]];
    for (w, c) in [(2.2, rgb(0x5d6068, 1.0)), (1.3, rgb(0xe4e6eb, 1.0))] {
        for s in tube.windows(2) {
            quads.push(paint(f.line(s[0], s[1], w, 0.0, [0.0, 0.0]), c, None));
        }
    }
    // 唱頭：整組繞唱針 (90, 76.47) 轉 −12°
    let tip = [90.0, 76.47];
    let head = |p: [f32; 2]| rot_about(p, tip, -12.0);
    quads.push(paint(
        f.rect(head([90.0, 71.5]), 4.0, 10.6, -12.0).radius(0.8 * k),
        rgb(0x26272c, 1.0),
        Some((0.35, rgb(0x7a7d86, 1.0))),
    ));
    quads.push(paint(f.line([92.0, 68.4], [95.2, 67.1], 0.8, -12.0, tip), rgb(0xc9ccd3, 1.0), None));
    quads.push(paint(
        f.rect(head([90.0, 74.1]), 2.2, 3.4, -12.0).radius(0.3 * k),
        rgb(0xb8323a, 1.0),
        None,
    ));
    // 支點
    for (d, c, stroke) in [
        (12.0, rgb(0x23242a, 1.0), Some((0.4, [1.0, 1.0, 1.0, 0.16]))),
        (6.2, rgb(0xc4c7ce, 1.0), None),
        (2.2, rgb(0x3a3c43, 1.0), None),
    ] {
        quads.push(paint(f.rect([PX, PY], d, d, 0.0).radius(d * 0.5 * k), c, stroke));
    }
}

/// 進場動畫（Python 版 `#now.v-in` 的 disc-grow 與磁頭淡入；離場的淡出）。
#[derive(Clone, Copy, Debug)]
pub struct Fx {
    /// 溝紋、軸心、反光：從 0.62 倍長到原大
    pub disc_scale: f32,
    pub disc_alpha: f32,
    pub arm_alpha: f32,
    /// 標籤（中央那張封面）。轉場的替身還在飛的時候不畫，落地才接手
    pub label: bool,
}

impl Default for Fx {
    fn default() -> Self {
        Self { disc_scale: 1.0, disc_alpha: 1.0, arm_alpha: 1.0, label: true }
    }
}

/// 畫在封面那一格（`x`, `y`, 邊長 `s`）裡。
pub fn draw(quads: &mut QuadRenderer, v: &Vinyl, x: f32, y: f32, s: f32, art: &Arc<wgpu::TextureView>, fx: Fx) {
    let u = s / 100.0;
    let at = |cx: f32, cy: f32, d: f32| Quad::new(x + (cx - d * 0.5) * u, y + (cy - d * 0.5) * u, d * u, d * u)
        .radius(d * 0.5 * u);
    let ds = fx.disc_scale;

    // 唱片：溝紋與固定方向的反光都在著色器裡（kind 4）。溝紋是旋轉對稱的，
    // 所以這層不必轉 —— 看得出在轉的是中央那張封面
    // 陰影照 CSS 的 0 34px 90px（那邊的封面大約 400px 寬，所以 ×u/4）；
    // 這裡的 blur 是 smoothstep 的半寬，取 CSS 模糊半徑的一半
    quads.set_fx(fx.disc_alpha, 0.0);
    quads.push(at(CX, CY, 2.0 * R * ds).vinyl().shadow(0.0, 8.5 * u, 11.0 * u, -u, [0.0, 0.0, 0.0, 0.75]));
    quads.set_fx(1.0, 0.0);
    // 標籤 = 封面，唱片直徑的 62%
    if fx.label {
        quads.push_tex(
            at(CX, CY, 2.0 * R * 0.62).cover(1.0, 1.0).rotate(v.angle.to_radians())
                .border(0.6 * u / 4.0 + 1.0, [0.0, 0.0, 0.0, 0.5]),
            art,
        );
    }
    // 軸心（跟溝紋一起長出來）
    let sp = 2.0 * R * 0.032 * ds;
    quads.set_fx(fx.disc_alpha, 0.0);
    quads.push(at(CX, CY, sp).color(rgb(0x7a7d85, 1.0)));
    quads.push(at(CX, CY, sp * 0.5).color(rgb(0x0b0b0e, 1.0)));
    quads.set_fx(fx.arm_alpha, 0.0);

    // 磁頭：先影子再本體。抬起時影子拉遠變淡、本體微微放大
    let l = v.lift.clamp(0.0, 1.0);
    let mix = |a: f32, b: f32| a + (b - a) * l;
    let sh = Frame {
        x,
        y,
        u,
        rot: v.arm,
        scale: mix(1.0, 1.01),
        off: [mix(0.7, 2.2) * u, mix(1.1, 3.4) * u],
    };
    arm(quads, &sh, Some(mix(0.55, 0.32)));
    let body = Frame { x, y, u, rot: v.arm, scale: mix(1.0, 1.018), off: [0.0, 0.0] };
    arm(quads, &body, None);
    quads.set_fx(1.0, 0.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 外圈到內圈是往唱片中心擺() {
        let a = arm_for(R_OUTER);
        let b = arm_for(R_INNER);
        // 磁頭在唱片右上方，往內圈走是往順時針（正值）轉
        assert!(b > a, "外圈 {a} 內圈 {b}");
        // 休息位在外圈的外面
        assert!(REST < a);
    }

    #[test]
    fn 唱針真的落在算出來的半徑上() {
        for r in [R_OUTER, R_INNER] {
            let f = Frame { x: 0.0, y: 0.0, u: 1.0, rot: arm_for(r), scale: 1.0, off: [0.0, 0.0] };
            let tip = f.map([PX, PY + dist()]);
            let got = (tip[0] - CX).hypot(tip[1] - CY);
            assert!((got - r).abs() < 0.01, "要 {r} 得到 {got}");
        }
    }

    #[test]
    fn 暫停時唱片慢慢停而且磁頭抬起() {
        let mut v = Vinyl::default();
        // 播放 10 秒：磁頭到位、唱片到速
        for _ in 0..600 {
            v.step(1.0 / 60.0, true, true, 0.0, 3.0);
        }
        assert!(v.engaged && !v.lifted);
        assert!((v.omega - 18.0).abs() < 0.1, "{}", v.omega);
        v.step(1.0 / 60.0, true, false, 0.0, 3.0);
        assert!(v.lifted);
        assert!(v.omega > 10.0, "停轉要有慣性，不是一下就停");
        for _ in 0..600 {
            v.step(1.0 / 60.0, true, false, 0.0, 3.0);
        }
        assert_eq!(v.omega, 0.0);
    }
}
