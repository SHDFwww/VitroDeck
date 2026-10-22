//! 演出風格：整套配色、構圖偏好、進退場、裝飾與鏡頭力道。
//!
//! 一首歌照段落換風格（[`segments`]）：間奏、空行或滿八句就切一段，副歌的頭尾一定切。
//! 每段配哪個風格由 [`super::vary`] 決定（整首不重複），這裡的 [`weight`]、[`chorus_weight`] 是偏好：
//! 唱得快的段落偏向衝擊、故障、普普、復古，慢的偏向浮遊、紙本、電影；副歌不用打字、電影。
//! 跟構圖一樣只看歌詞與曲目，同一首歌每次播換的地方、換成什麼都一樣。

use super::layout::{Enter, Exit, Template};
use super::plan::Part;
use super::scene::Scene;
use super::{tint, v2};
use crate::lyrics::Lyrics;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Look {
    /// 霓虹：白字＋強調色色塊、鼓點色差與閃光（最早那一版）
    Neon,
    /// 紙本：米白底、黑字、朱紅點綴，直書優先，安靜
    Paper,
    /// 衝擊：白字粗黑邊、黃色關鍵字、集中線，蓋章與大字
    Impact,
    /// 故障：常駐色差、橫向切片，字一閃一閃地出現
    Glitch,
    /// 浮遊：字慢慢浮上來、柔光、光點往上飄，鏡頭幾乎不晃
    Drift,
    /// 打字：深色底、暖白字一個一個打出來，游標跟著走，退場時倒退鍵刪回去
    Type,
    /// 普普：黃底、白字粗黑邊＋硬陰影、角落網點，彈跳進場
    Pop,
    /// 燈管：字只剩發光的輪廓，像霓虹招牌一個字一個字通電亮起來、退場時斷電
    Sign,
    /// 電影：上下黑邊、小字置中、失焦淡入，暖色漏光與較重的顆粒，幾乎不動
    Cinema,
    /// 復古：合成波（synthwave）的紫底、往前流的透視網格、粉紅字帶青色錯位陰影
    Retro,
}

pub const LOOKS: [Look; 10] =
    [Look::Neon, Look::Paper, Look::Impact, Look::Glitch, Look::Drift, Look::Type, Look::Pop, Look::Sign, Look::Cinema, Look::Retro];

impl Look {
    pub fn name(self) -> &'static str {
        match self {
            Look::Neon => "霓虹",
            Look::Paper => "紙本",
            Look::Impact => "衝擊",
            Look::Glitch => "故障",
            Look::Drift => "浮遊",
            Look::Type => "打字",
            Look::Pop => "普普",
            Look::Sign => "燈管",
            Look::Cinema => "電影",
            Look::Retro => "復古",
        }
    }
}

#[derive(Clone, Copy, Debug)]
/// 字型不歸風格管：一律用設定裡選的（使用者明講過）
pub struct Style {
    pub look: Look,
    /// 整片墊底的顏色（直通 alpha，0 是不墊）
    pub bg: [f32; 4],
    /// 一般字的填色
    pub ink: [f32; 4],
    /// 關鍵字的填色，`None` 用封面的強調色
    pub key_ink: Option<[f32; 4]>,
    /// 色塊的顏色（`None` 用強調色）與墊在上面的字色
    pub band: Option<[f32; 4]>,
    pub band_ink: [f32; 4],
    /// 實心字也描一圈邊（衝擊的黑邊）：(顏色, 字級的幾分之幾)
    pub stroke: Option<([f32; 3], f32)>,
    /// 線條、框角、圓環這類細裝飾的顏色
    pub line: [f32; 4],
    /// 構圖範本：照順序輪，排在前面的比較常出現
    pub templates: &'static [Template],
    pub enters: &'static [Enter],
    pub key_enters: &'static [Enter],
    pub exits: &'static [Exit],
    pub hollow: f32,
    pub band_p: f32,
    pub corners_p: f32,
    pub ghost: bool,
    /// 最大的字佔畫面高的幾成
    pub cap: f32,
    /// 整句最多佔畫面多大（1 是寬八成、高六成四）
    pub fill: f32,
    /// 鏡頭晃動、色差、閃光、跟拍縮放的倍率
    pub shake: f32,
    pub chroma: f32,
    pub flash: f32,
    pub pump: f32,
    /// 唱到的字發光多強
    pub glow: f32,
    /// 鼓點時畫面中心擴散的圓環
    pub ring: bool,
    /// 特有的裝飾
    pub rays: bool,
    pub slices: bool,
    pub motes: bool,
    /// 句尾蓋一枚朱印（紙本）
    pub seal: bool,
    /// 背景散景光斑的濃淡（0 是沒有）
    pub bokeh: f32,
    /// 四角暗角、底片顆粒有多重
    pub vignette: f32,
    pub grain: f32,
    /// 關鍵字被唱到時迸出的光粒、掃過字面的光澤（強度，0 是沒有）
    pub sparks: bool,
    pub shine: f32,
    /// 暗角的顏色（亮底的風格用深褐、橘，不用黑）
    pub vig: [f32; 3],
    /// 字沒在動的時候上下輕輕浮多少（字級的幾分之幾）
    pub float: f32,
    /// 每個詞組一樣大（不放大關鍵字、不混小字）
    pub even: bool,
    /// 硬陰影：(顏色, 往右, 往下)，位移是字級的幾分之幾
    pub shadow: Option<([f32; 4], f32, f32)>,
    /// 字畫成發光的燈管輪廓（燈管）
    pub tube: bool,
    /// 打字游標（打字）
    pub cursor: bool,
    /// 上下黑邊各佔畫面高的幾成（電影，0 是沒有）
    pub bars: f32,
    /// 鋪在背景、不管有沒有字都在的場景：角落網點、透視網格、漏光
    pub halftone: bool,
    pub grid: bool,
    pub leaks: bool,
    /// 整套配色轉了幾度色相（[`Style::tinted`]，0 是原本的配色）
    pub hue: f32,
    /// 文字PV_v2 收斂過的（[`Style::v2`]）：顏色到畫的時候才從封面推（[`Style::painted`]）
    pub v2: bool,
    /// 可以用直書（v2 的階梯不用：直書一欄太高，會壓到上下兩階）
    pub tall: bool,
}

const WHITE: [f32; 4] = [0.97, 0.97, 0.98, 1.0];
const NEAR_BLACK: [f32; 4] = [0.05, 0.05, 0.07, 1.0];

impl Style {
    pub fn of(look: Look) -> Self {
        use Enter::*;
        use Template::*;
        let neon = Style {
            look,
            bg: [0.0; 4],
            ink: WHITE,
            key_ink: None,
            band: None,
            band_ink: NEAR_BLACK,
            stroke: None,
            line: [1.0, 1.0, 1.0, 1.0],
            templates: &[Stack, Diagonal, Vertical, Zigzag, Slam],
            enters: &[Pop, Spring, Drop, Spin, Slide(-1.0), Slide(1.0), Whip(-1.0), Whip(1.0)],
            key_enters: &[Stamp, Stamp, Stamp, Pop, Pop],
            exits: &[Exit::Scatter, Exit::Fall, Exit::Zoom],
            hollow: 0.25,
            band_p: 0.5,
            corners_p: 0.4,
            ghost: true,
            cap: 0.24,
            fill: 1.0,
            shake: 1.0,
            chroma: 1.0,
            flash: 1.0,
            pump: 1.0,
            glow: 0.55,
            ring: true,
            rays: false,
            slices: false,
            motes: false,
            seal: false,
            bokeh: 1.0,
            vignette: 0.42,
            grain: 0.035,
            sparks: true,
            shine: 0.6,
            vig: [0.0; 3],
            float: 0.018,
            even: false,
            shadow: None,
            tube: false,
            cursor: false,
            bars: 0.0,
            halftone: false,
            grid: false,
            leaks: false,
            hue: 0.0,
            v2: false,
            tall: true,
        };
        match look {
            Look::Neon => neon,
            Look::Paper => Style {
                bg: [0.93, 0.91, 0.86, 0.94],
                ink: [0.09, 0.08, 0.08, 1.0],
                key_ink: Some([0.78, 0.09, 0.12, 1.0]),
                band: Some([0.78, 0.09, 0.12, 1.0]),
                band_ink: [0.96, 0.94, 0.9, 1.0],
                line: [0.09, 0.08, 0.08, 1.0],
                templates: &[Vertical, Stack, Vertical, Diagonal],
                enters: &[Rise, Rise, Pop],
                key_enters: &[Rise, Stamp],
                exits: &[Exit::Fade],
                hollow: 0.0,
                band_p: 0.3,
                corners_p: 0.5,
                ghost: true,
                cap: 0.22,
                shake: 0.25,
                chroma: 0.0,
                flash: 0.0,
                pump: 0.5,
                glow: 0.0,
                ring: false,
                seal: true,
                bokeh: 0.0,
                vignette: 0.22,
                grain: 0.06,
                sparks: false,
                shine: 0.35,
                vig: [0.32, 0.22, 0.12],
                ..neon
            },
            Look::Impact => Style {
                bg: [0.02, 0.02, 0.03, 0.55],
                key_ink: Some([1.0, 0.86, 0.1, 1.0]),
                band: Some([1.0, 0.86, 0.1, 1.0]),
                stroke: Some(([0.0, 0.0, 0.0], 0.07)),
                templates: &[Slam, Zigzag, Diagonal, Slam, Stack],
                enters: &[Pop, Stamp, Slide(-1.0), Slide(1.0), Whip(-1.0), Whip(1.0)],
                key_enters: &[Stamp],
                exits: &[Exit::Zoom, Exit::Scatter],
                hollow: 0.15,
                band_p: 0.25,
                corners_p: 0.0,
                ghost: false,
                cap: 0.3,
                shake: 1.7,
                chroma: 0.4,
                flash: 1.4,
                pump: 1.5,
                glow: 0.0,
                rays: true,
                bokeh: 0.0,
                vignette: 0.55,
                grain: 0.05,
                shine: 0.7,
                ..neon
            },
            Look::Glitch => Style {
                bg: [0.0, 0.02, 0.05, 0.6],
                key_ink: Some([0.35, 1.0, 0.9, 1.0]),
                band: Some([1.0, 0.2, 0.55, 1.0]),
                line: [0.35, 1.0, 0.9, 1.0],
                templates: &[Stack, Slam, Diagonal, Zigzag],
                enters: &[Flicker],
                key_enters: &[Flicker, Stamp],
                exits: &[Exit::Blink],
                hollow: 0.2,
                band_p: 0.35,
                corners_p: 0.6,
                ghost: false,
                shake: 0.8,
                chroma: 2.2,
                flash: 0.6,
                glow: 0.3,
                ring: false,
                slices: true,
                bokeh: 0.0,
                vignette: 0.5,
                grain: 0.06,
                sparks: false,
                shine: 0.4,
                ..neon
            },
            Look::Drift => Style {
                bg: [0.01, 0.02, 0.05, 0.35],
                templates: &[Vertical, Stack, Diagonal],
                enters: &[Rise],
                key_enters: &[Rise],
                exits: &[Exit::Fade],
                hollow: 0.3,
                band_p: 0.0,
                corners_p: 0.0,
                cap: 0.19,
                shake: 0.0,
                chroma: 0.0,
                flash: 0.0,
                pump: 0.5,
                glow: 0.9,
                ring: false,
                motes: true,
                bokeh: 1.3,
                vignette: 0.4,
                grain: 0.03,
                shine: 0.5,
                float: 0.04,
                ..neon
            },
            Look::Type => Style {
                bg: [0.06, 0.055, 0.05, 0.78],
                ink: [0.93, 0.9, 0.82, 1.0],
                key_ink: Some([1.0, 0.72, 0.3, 1.0]),
                line: [1.0, 0.72, 0.3, 1.0],
                templates: &[Typed, Caption],
                enters: &[Type],
                key_enters: &[Type],
                exits: &[Exit::Erase],
                hollow: 0.0,
                band_p: 0.0,
                corners_p: 0.0,
                ghost: false,
                cap: 0.11,
                shake: 0.0,
                chroma: 0.0,
                flash: 0.0,
                pump: 0.15,
                glow: 0.15,
                ring: false,
                bokeh: 0.0,
                vignette: 0.5,
                grain: 0.05,
                sparks: false,
                shine: 0.0,
                float: 0.0,
                even: true,
                cursor: true,
                ..neon
            },
            Look::Pop => Style {
                bg: [1.0, 0.84, 0.2, 0.93],
                key_ink: Some([1.0, 0.25, 0.5, 1.0]),
                band: Some([0.08, 0.07, 0.1, 1.0]),
                band_ink: [1.0, 0.84, 0.2, 1.0],
                stroke: Some(([0.08, 0.07, 0.1], 0.08)),
                line: [0.08, 0.07, 0.1, 1.0],
                templates: &[Zigzag, Stack, Diagonal, Slam],
                enters: &[Pop, Drop, Spring, Spring],
                key_enters: &[Pop, Stamp, Spring],
                exits: &[Exit::Fall, Exit::Scatter],
                hollow: 0.0,
                band_p: 0.35,
                corners_p: 0.0,
                ghost: false,
                cap: 0.27,
                shake: 0.6,
                chroma: 0.0,
                flash: 0.0,
                pump: 1.3,
                glow: 0.0,
                ring: false,
                bokeh: 0.0,
                vignette: 0.2,
                grain: 0.03,
                sparks: false,
                shine: 0.5,
                vig: [0.75, 0.3, 0.05],
                shadow: Some(([0.08, 0.07, 0.1, 1.0], 0.07, 0.08)),
                halftone: true,
                ..neon
            },
            Look::Sign => Style {
                bg: [0.03, 0.02, 0.05, 0.72],
                ink: [0.5, 0.92, 1.0, 1.0],
                key_ink: Some([1.0, 0.3, 0.72, 1.0]),
                line: [1.0, 0.3, 0.72, 1.0],
                templates: &[Stack, Diagonal, Zigzag, Slam],
                enters: &[Ignite],
                key_enters: &[Ignite],
                exits: &[Exit::Off],
                hollow: 0.0,
                band_p: 0.0,
                corners_p: 0.3,
                ghost: false,
                cap: 0.22,
                shake: 0.0,
                chroma: 0.0,
                flash: 0.0,
                pump: 0.6,
                glow: 0.0,
                ring: false,
                bokeh: 0.5,
                vignette: 0.5,
                grain: 0.04,
                sparks: false,
                shine: 0.0,
                tube: true,
                ..neon
            },
            Look::Cinema => Style {
                bg: [0.0, 0.0, 0.0, 0.55],
                ink: [0.95, 0.93, 0.88, 1.0],
                key_ink: Some([1.0, 0.8, 0.5, 1.0]),
                line: [1.0, 0.8, 0.5, 1.0],
                templates: &[Caption, Vertical],
                enters: &[Focus],
                key_enters: &[Focus],
                exits: &[Exit::Fade],
                hollow: 0.0,
                band_p: 0.0,
                corners_p: 0.0,
                ghost: false,
                cap: 0.085,
                shake: 0.0,
                chroma: 0.0,
                flash: 0.0,
                pump: 0.15,
                glow: 0.15,
                ring: false,
                bokeh: 0.0,
                vignette: 0.55,
                grain: 0.07,
                sparks: false,
                shine: 0.25,
                float: 0.006,
                even: true,
                bars: 0.12,
                leaks: true,
                ..neon
            },
            Look::Retro => Style {
                bg: [0.08, 0.02, 0.14, 0.78],
                key_ink: Some([1.0, 0.35, 0.75, 1.0]),
                band: Some([0.25, 0.9, 1.0, 1.0]),
                line: [0.3, 0.9, 1.0, 1.0],
                templates: &[Diagonal, Stack, Slam, Zigzag],
                enters: &[Slide(-1.0), Slide(1.0), Pop, Whip(-1.0), Whip(1.0)],
                key_enters: &[Pop, Stamp],
                exits: &[Exit::Zoom, Exit::Fade],
                hollow: 0.2,
                band_p: 0.25,
                corners_p: 0.0,
                ghost: false,
                cap: 0.25,
                shake: 0.5,
                chroma: 0.5,
                flash: 0.5,
                glow: 0.45,
                ring: false,
                bokeh: 0.0,
                vignette: 0.45,
                grain: 0.04,
                shine: 0.6,
                shadow: Some(([0.25, 0.85, 1.0, 0.9], 0.05, 0.06)),
                grid: true,
                ..neon
            },
        }
    }

    /// 主歌收斂、副歌放開：字級與裝飾的多寡照這句在歌裡的位置調。
    /// 只改機率與大小，不改亂數抽幾次，同一句的構圖其他部分不變
    pub fn tuned(mut self, part: Part) -> Self {
        match part {
            Part::Verse => {
                self.cap *= 0.88;
                self.band_p *= 0.6;
                self.corners_p *= 0.6;
            }
            Part::Pre => self.cap *= 0.95,
            Part::Chorus => {
                self.cap *= 1.12;
                self.fill = 1.08;
                self.band_p = (self.band_p * 1.4).min(0.75);
                self.corners_p = (self.corners_p * 1.3).min(0.8);
            }
        }
        self
    }

    /// 場景要的調整：縱深的句子會被推近放大，先排小一點；大字帶、飛越的光塵已經夠滿，不墊超大淡字
    pub fn scened(mut self, scene: Scene) -> Self {
        match scene {
            // v2 的一般場景是捲動，上下還要擺前後句（跑馬燈、追光、殘響同）
            Scene::Plain | Scene::Spotlight | Scene::Echo | Scene::Marquee if self.v2 => {
                self.fill *= 0.6;
                self.tall = false;
                self.ghost = false;
            }
            Scene::Plain => {}
            // v2 的長卷要看得到前後句，每句排小一點
            Scene::Flight => {
                self.fill *= if self.v2 { 0.75 } else { 0.85 };
                self.ghost = false;
            }
            Scene::Depth => {
                self.fill *= if self.v2 { 0.62 } else { 0.6 };
                self.cap *= 0.72;
                self.ghost = false;
            }
            Scene::Marquee => {
                self.cap *= 0.92;
                self.ghost = false;
            }
            Scene::Wall => self.ghost = false,
            // 轉盤、階梯、分鏡同時有好幾句在畫面上，每句排小一點才不會疊成一團
            // v2 的階梯下面還要擺下一句，再小一點
            Scene::Carousel | Scene::Cascade => {
                let v2 = self.v2 && scene == Scene::Cascade;
                self.fill *= if v2 { 0.66 } else { 0.72 };
                self.tall = !v2;
                self.cap *= 0.82;
                self.ghost = false;
            }
            // v2 的鐘擺旁邊還斜掛著下一句
            Scene::Split | Scene::Pendulum => {
                self.fill *= if self.v2 && scene == Scene::Pendulum { 0.75 } else { 0.88 };
                self.ghost = false;
            }
            // 句子收到上半部，下半部留給倒影
            Scene::Mirror => {
                self.fill *= if self.v2 { 0.6 } else { 0.82 };
                self.tall &= !self.v2;
                self.cap *= 0.85;
                self.ghost = false;
            }
            Scene::Vortex => {
                self.fill *= 0.6;
                self.cap *= 0.72;
                self.ghost = false;
            }
            // 暗一點、字亮一點，聚光燈才看得出來
            Scene::Spotlight => {
                self.glow = self.glow.max(0.35);
                self.vignette = (self.vignette + 0.2).min(0.7);
            }
            // 已經有殘響字了，不再墊超大淡字
            Scene::Echo => self.ghost = false,
        }
        self
    }

    /// 文字PV_v2：一首歌一套、顏色只從封面來，所以拿掉多色與一直在動的東西
    /// （色差、閃光、鼓點圓環、集中線、切片、光粒、漏光、網點、網格），描邊、空心、硬陰影也拿掉
    /// （細的手寫體描邊會糊成一團）。唱到的字會發光，那團光另外畫（不看 `glow`）
    pub fn v2(mut self) -> Self {
        self.v2 = true;
        self.hollow = 0.0;
        self.stroke = None;
        self.shadow = None;
        self.chroma = 0.0;
        self.flash = 0.0;
        self.ring = false;
        self.rays = false;
        self.slices = false;
        self.sparks = false;
        self.seal = false;
        self.halftone = false;
        self.grid = false;
        self.leaks = false;
        self.shake *= 0.4;
        self.pump *= 0.6;
        self.band_p *= 0.5;
        self.corners_p *= 0.5;
        self.bokeh *= 0.35;
        self.grain *= 0.6;
        self.bars = self.bars.min(0.08);
        self.shine = self.shine.min(0.3);
        self
    }

    /// 文字PV_v2 的配色：關鍵字與裝飾是封面強調色本身與它的淡版；底是夜色（深藍一類），
    /// 色相挑離強調色最遠的（[`v2::night`]），字是帶一點點底色的白。不是 v2 的原樣回傳
    pub fn painted(mut self, accent: [f32; 4]) -> Self {
        if !self.v2 {
            return self;
        }
        let p = tint::lab([accent[0], accent[1], accent[2]]);
        let (ch, h) = (p[1].hypot(p[2]), p[2].atan2(p[1]));
        let (nh, nk) = v2::night(h.to_degrees(), ch);
        let c = |l: f32, k: f32, h: f32, a: f32| {
            let [r, g, b] = tint::lch(l, k, h);
            [r, g, b, a]
        };
        // 同色相推出來的暗底，金、橘這類強調色會變咖啡色。墊得夠厚，後面封面的顏色才不會把夜色染髒
        let nh = nh.to_radians();
        self.bg = c(0.2, 0.045 * nk, nh, 0.85);
        self.ink = c(0.96, 0.01 * nk, nh, 1.0);
        self.key_ink = Some(c(p[0].max(0.82), ch.min(0.16), h, 1.0));
        self.band = Some(c(0.6, ch.min(0.13), h, 1.0));
        self.band_ink = c(0.97, ch.min(0.01), h, 1.0);
        self.line = c(0.82, ch.min(0.07), h, 1.0);
        self.vig = [0.0; 3];
        self
    }

    /// 整套配色轉 `deg` 度色相（亮度不動）。封面強調色要到畫的時候才知道，用 [`Style::spin`] 轉
    pub fn tinted(mut self, deg: f32) -> Self {
        let sp = |c: [f32; 4]| tint::spin(c, deg);
        self.bg = sp(self.bg);
        self.ink = sp(self.ink);
        self.key_ink = self.key_ink.map(sp);
        self.band = self.band.map(sp);
        self.band_ink = sp(self.band_ink);
        self.stroke = self.stroke.map(|(c, k)| (tint::spin3(c, deg), k));
        self.line = sp(self.line);
        self.vig = tint::spin3(self.vig, deg);
        self.shadow = self.shadow.map(|(c, x, y)| (sp(c), x, y));
        self.hue += deg;
        self
    }

    /// 封面強調色跟著這段的配色轉（轉完跟字太像就調暗或用原色，見 [`tint::accent`]）
    pub fn accent(&self, a: [f32; 4]) -> [f32; 4] {
        tint::accent(a, self)
    }

    /// 不在配色表裡的顏色（網點、網格、漏光）跟著這段的配色轉
    pub fn spin(&self, c: [f32; 4]) -> [f32; 4] {
        tint::spin(c, self.hue)
    }

    pub fn spin3(&self, c: [f32; 3]) -> [f32; 3] {
        tint::spin3(c, self.hue)
    }

    /// 字要描邊的話，邊有多粗（px）
    pub fn stroke_w(&self, size: f32) -> f32 {
        self.stroke.map_or(0.0, |(_, k)| (size * k).max(2.0))
    }
}

/// 一段：從第 `first` 句開始，`t` 是那句開始的時間
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub first: usize,
    pub t: i64,
    pub look: Look,
    /// 副歌的段落
    pub chorus: bool,
    /// 這段演什麼場景（[`super::vary::cast`] 配的）
    pub scene: Scene,
    /// 這段的配色轉幾度色相（[`super::vary::paint`] 配的）
    pub hue: f32,
    /// 文字PV_v2 的段落（風格照 [`Style::v2`] 收斂）
    pub v2: bool,
}

impl Segment {
    /// 這段的風格（含配色，不含副歌、場景的調整）
    pub fn style(&self) -> Style {
        let st = Style::of(self.look).tinted(self.hue);
        if self.v2 { st.v2() } else { st }
    }
}

/// 第 `i` 段的風格，超出範圍用霓虹
pub fn style_at(segs: &[Segment], i: usize) -> Style {
    segs.get(i).map_or(Style::of(Look::Neon), Segment::style)
}

/// 一段最少、最多幾句（副歌整段不拆，太長才拆）；兩句開頭隔多久算間奏（ms）
const SEG_MIN: usize = 4;
const SEG_MAX: usize = 8;
const CHORUS_MAX: usize = 12;
pub const SEG_GAP: i64 = 9000;

/// 把歌詞切成段落（風格與場景由 [`super::vary::cast`] 配）。`parts` 是 [`super::plan::parts`]
pub fn segments(ly: &Lyrics, parts: &[Part]) -> Vec<Segment> {
    let chorus = |i: usize| parts.get(i) == Some(&Part::Chorus);
    let mut starts: Vec<usize> = Vec::new();
    let mut count = 0;
    let mut prev: Option<(i64, bool)> = None;
    let mut blank = false;
    for (i, l) in ly.synced.iter().enumerate() {
        if l.text.trim().is_empty() {
            blank = true;
            continue;
        }
        let gap = prev.map_or(0, |p| l.t - p.0);
        let flip = prev.is_some_and(|p| p.1 != chorus(i));
        let max = if chorus(i) { CHORUS_MAX } else { SEG_MAX };
        if starts.is_empty() || flip || count >= max || (count >= SEG_MIN && (blank || gap >= SEG_GAP)) {
            starts.push(i);
            count = 0;
        }
        count += 1;
        prev = Some((l.t, chorus(i)));
        blank = false;
    }
    starts
        .into_iter()
        .map(|first| Segment { first, t: ly.synced[first].t, look: Look::Neon, chorus: chorus(first), scene: Scene::Plain, hue: 0.0, v2: false })
        .collect()
}

/// 第 `k` 段唱得多快（0 慢 .. 1 快）：每秒幾個字，慢歌大約 2～3，快歌 5 以上
pub fn fast(ly: &Lyrics, segs: &[Segment], k: usize) -> f32 {
    let first = segs[k].first;
    let end = segs.get(k + 1).map_or(ly.synced.len(), |s| s.first);
    let lines = &ly.synced[first..end];
    let chars: usize = lines.iter().map(|l| l.text.chars().filter(|c| !c.is_whitespace()).count()).sum();
    let t_end = ly.synced.get(end).map_or(lines.last().map_or(0, |l| l.t + 4000), |l| l.t);
    let secs = ((t_end - ly.synced[first].t) as f32 / 1000.0).max(1.0);
    ((chars as f32 / secs - 2.5) / 3.0).clamp(0.0, 1.0)
}

/// 副歌的風格：放得開、畫面滿的優先；字小、安靜的（打字、電影）不選
pub fn chorus_weight(l: Look, fast: f32) -> f32 {
    match l {
        Look::Neon => 1.4,
        Look::Impact => 0.9 + 1.2 * fast,
        Look::Pop => 0.7 + 0.8 * fast,
        Look::Retro => 0.9 + 0.4 * fast,
        Look::Glitch => 0.5 + 0.8 * fast,
        Look::Sign => 1.0,
        Look::Drift => 0.9 * (1.0 - fast),
        Look::Paper => 0.5 * (1.0 - fast),
        Look::Type | Look::Cinema => 0.0,
    }
}

/// 主歌的風格
pub fn weight(l: Look, fast: f32) -> f32 {
    match l {
        Look::Neon => 1.3,
        Look::Paper => 1.4 - 0.7 * fast,
        Look::Impact => 0.9 + 1.4 * fast,
        Look::Glitch => 0.9 + 1.0 * fast,
        Look::Drift => 1.5 - 1.2 * fast,
        Look::Type => 1.0 - 0.4 * fast,
        Look::Pop => 0.8 + 0.8 * fast,
        Look::Sign => 1.0,
        Look::Cinema => 1.2 - 0.9 * fast,
        Look::Retro => 0.8 + 0.8 * fast,
    }
}

/// 第 `i` 句屬於哪一段
pub fn segment_of(segs: &[Segment], i: usize) -> usize {
    segs.iter().rposition(|s| s.first <= i).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::Line;

    fn lyrics(times: &[i64]) -> Lyrics {
        Lyrics {
            synced: times.iter().map(|&t| Line { t, text: if t < 0 { String::new() } else { "我們一起唱歌".into() }, ..Default::default() }).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn 間奏切段() {
        // 六句、隔 15 秒間奏、再六句
        let mut t: Vec<i64> = (0..6).map(|k| k * 3000).collect();
        t.extend((0..6).map(|k| 33_000 + k * 3000));
        let s = segments(&lyrics(&t), &[]);
        assert_eq!(s.len(), 2, "{s:?}");
        assert_eq!(s[1].first, 6);
        let t: Vec<i64> = (0..40).map(|k| k * 2500).collect();
        let s = segments(&lyrics(&t), &[]);
        assert!(s.len() >= 5);
        for w in s.windows(2) {
            assert!(w[1].first - w[0].first <= SEG_MAX);
        }
        assert_eq!(segment_of(&s, 0), 0);
        assert_eq!(segment_of(&s, 39), s.len() - 1);
    }

    #[test]
    fn 副歌頭尾一定切段() {
        // 主歌 6 句、副歌 4 句，輪三次，最後主歌之後連兩次副歌
        let mut parts = Vec::new();
        for _ in 0..3 {
            parts.extend([Part::Verse; 6]);
            parts.extend([Part::Chorus; 4]);
        }
        parts.extend([Part::Verse; 6]);
        parts.extend([Part::Chorus; 8]);
        let t: Vec<i64> = (0..parts.len() as i64).map(|k| k * 2500).collect();
        let s = segments(&lyrics(&t), &parts);
        assert_eq!(s.iter().filter(|x| x.chorus).count(), 4, "{s:?}");
        for x in &s {
            assert_eq!(x.chorus, parts[x.first] == Part::Chorus);
        }
        // 最後兩次副歌連在一起（8 句）不拆
        assert_eq!(s.last().unwrap().first, 36);
    }
}
