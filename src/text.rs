//! 文字 —— glyphon（cosmic-text + swash）。
//!
//! 這是 CSS 白送而原生要自己顧的一大塊：字型挑選、CJK 字型 fallback、
//! 斷行、字距、以及把字形光柵化進圖集。
//!
//! 歌詞是中文，所以 `Shaping::Advanced` 不能省 —— 基本 shaping 不會處理
//! 標點擠壓與 fallback 鏈。
//!
//! 字型與字級倍率是全域的（[`set_font`]），每個 `Text` 在 `begin` 時對一次版本號跟上。
//! 中文與英文各自一個字型：排版前把字串切成中文段與英文段（[`script_runs`]），各段指定自己的字型。

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight, fontdb,
};

use crate::gfx::Gpu;

/// 預設的中文字型。設定檔是空字串、或指定的字型這台電腦沒有，都用它
pub const DEFAULT_FONT: &str = "Microsoft JhengHei";

/// 使用者選的（英文字型, 中文字型, 字級倍率），`PREF_GEN` 每改一次加一
static PREF: Mutex<(String, String, f32)> = Mutex::new((String::new(), String::new(), 1.0));
static PREF_GEN: AtomicU32 = AtomicU32::new(1);

/// 換字型與字級倍率。`cjk` 空字串＝預設字型（微軟正黑體），`latin` 空字串＝跟中文字型一樣。
/// 所有 `Text` 下一次 `begin` 生效
pub fn set_font(latin: &str, cjk: &str, scale: f32) {
    let scale = if scale.is_finite() { scale.clamp(0.5, 2.0) } else { 1.0 };
    if let Ok(mut p) = PREF.lock() {
        if p.0 == latin && p.1 == cjk && p.2 == scale {
            return;
        }
        *p = (latin.to_string(), cjk.to_string(), scale);
    }
    PREF_GEN.fetch_add(1, Ordering::Relaxed);
}

/// 使用者設的字級倍率。版面要在 `Text::begin` 之前先算的（迷你視窗、快捷鍵面板）用這個
pub fn pref_scale() -> f32 {
    PREF.lock().map_or(1.0, |p| p.2)
}

/// 用中文字型畫的字：漢字、注音、假名、諺文、中日韓標點與全形字。其他都是英文字型
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF       // 諺文字母
        | 0x2E80..=0x303F     // 部首、中日韓符號與標點（、。「」）
        | 0x3040..=0x31FF     // 假名、注音
        | 0x3200..=0x4DBF     // 帶圈字、擴充 A
        | 0x4E00..=0x9FFF     // 基本漢字
        | 0xA960..=0xA97F
        | 0xAC00..=0xD7FF     // 諺文音節
        | 0xF900..=0xFAFF     // 相容漢字
        | 0xFE30..=0xFE4F     // 直書標點
        | 0xFF00..=0xFFEF     // 全形英數與標點（，！（））
        | 0x20000..=0x3FFFF)  // 擴充 B 以後
}

/// 不歸哪一邊的字：空白與一般標點（… — “ ”），跟著前面那段，免得一句話切成一堆小段
fn is_neutral(c: char) -> bool {
    c.is_whitespace() || (0x2000..=0x206F).contains(&(c as u32))
}

/// 把字串切成 (起, 迄, 是中文) 的連續段，位元組位置。開頭的空白與標點併進第一段
fn script_runs(s: &str) -> Vec<(usize, usize, bool)> {
    let mut out: Vec<(usize, usize, bool)> = Vec::new();
    for (i, c) in s.char_indices() {
        let end = i + c.len_utf8();
        if is_neutral(c) {
            if let Some(r) = out.last_mut() {
                r.1 = end;
            }
            continue;
        }
        let cjk = is_cjk(c);
        match out.last_mut() {
            Some(r) if r.2 == cjk => r.1 = end,
            Some(_) => out.push((i, end, cjk)),
            None => out.push((0, end, cjk)),
        }
    }
    if out.is_empty() && !s.is_empty() {
        out.push((0, s.len(), false));
    }
    out
}

/// 字型清單的一項
#[derive(Clone, Debug, PartialEq)]
pub struct FontFamily {
    /// 存進設定檔、指定字型用的名字（字型裡的英文家族名）
    pub key: String,
    /// 顯示用：有繁中名稱就用繁中（「微軟正黑體」），沒有就是 `key`
    pub name: String,
    /// 有沒有中文字。中文字型的清單只列這些
    pub cjk: bool,
    /// 有沒有英文字。英文字型的清單只列這些
    pub latin: bool,
}

/// 一個家族有哪些字重。`None` 是可變字型（有字重軸，要多少都有）。
/// `check_var` 為 false 時不讀字型檔，只看字面資訊（字型清單每幀都要，不能讀檔）
fn family_weights(db: &fontdb::Database, name: &str, check_var: bool) -> Option<Vec<u16>> {
    use glyphon::cosmic_text::skrifa::{FontRef, MetadataProvider, Tag};
    let mut out = Vec::new();
    for f in db.faces().filter(|f| f.families.iter().any(|(n, _)| n == name)) {
        let var = check_var
            && db.with_face_data(f.id, |d, i| Some(FontRef::from_index(d, i).ok()?.axes().get_by_tag(Tag::new(b"wght")).is_some()))
                == Some(Some(true));
        if var {
            return None;
        }
        out.push(f.weight.0);
    }
    Some(out)
}

/// 把要的字重換成這個家族有的。cosmic-text 只認字重完全一樣的字面，差一點就整段丟給
/// fallback 字型（正黑體只有 300／400／700，要 500、600 會變成 Segoe UI 配別的中文字型；
/// 只有一種字重的手寫體，粗體字整個不是手寫體）。挑法照 CSS 的字重比對：
/// 要 500 以上先找更粗的，400 以下先找更細的，400～500 先找到 500 為止更粗的、再找更細的
fn snap_weight(avail: &[u16], want: u16) -> u16 {
    if avail.is_empty() || avail.contains(&want) {
        return want;
    }
    let heavier = avail.iter().copied().filter(|w| *w > want).min();
    let lighter = avail.iter().copied().filter(|w| *w < want).max();
    let pick = match want {
        0..=399 => lighter.or(heavier),
        400..=500 => avail.iter().copied().filter(|w| *w > want && *w <= 500).min().or(lighter).or(heavier),
        _ => heavier.or(lighter),
    };
    pick.unwrap_or(want)
}

/// 要舊式 TrueType hinting 才組得起字的字型（FreeType 叫它們 tricky font）：字是用部件拼的，
/// 部件擺在哪裡靠 hinting 指令。swash 只有新式 hinting（不理會水平方向的指令），畫出來部件錯位、
/// 字距忽大忽小（標楷體實測；關掉 hinting 更糟）。清單不列，設定指到它也退回預設。
/// 名單取自 FreeType 的 trick_names，拿掉細明體系列：Windows 10 的細明體、新細明體、細明體_HKSCS 實測正常
const TRICKY: &[&str] = &[
    "DFGirl", "DFGothic", "DFGyoSho", "DFHei", "DFHSGothic", "DFHSMincho", "DFKai", "DFMing", "DLC",
    "HuaTianKaiTi", "HuaTianSongTi", "Ming(for ISO10646)", "MingMedium", "MingLi43", "cpop",
];

fn is_tricky(name: &str) -> bool {
    TRICKY.iter().any(|t| name.starts_with(t))
}

/// 這台電腦裝的字型，一個家族一項，中文字型在前、各自照名稱排。
/// 直書用的 `@` 開頭家族與符號字型（Wingdings、Segoe MDL2 Assets 之類）不列：
/// 清單上的名字是用它自己的字型畫的，符號字型畫出來是一排圖案
fn list_families(db: &fontdb::Database) -> Vec<FontFamily> {
    use glyphon::cosmic_text::skrifa::{FontRef, MetadataProvider};
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for f in db.faces() {
        let Some((key, _)) = f.families.first() else { continue };
        if key.starts_with('@') || is_tricky(key) || !seen.insert(key.clone()) {
            continue;
        }
        // 一個家族看第一個字面就好
        let cover = db.with_face_data(f.id, |data, i| {
            let cm = FontRef::from_index(data, i).ok()?.charmap();
            Some((cm.is_symbol(), cm.map('A').is_some(), "中文字".chars().all(|c| cm.map(c).is_some())))
        });
        let Some(Some((false, latin, cjk))) = cover else { continue };
        if !latin && !cjk {
            continue;
        }
        let local = |l: fontdb::Language| f.families.iter().find(|(_, x)| *x == l).map(|(n, _)| n.clone());
        let name = local(fontdb::Language::Chinese_Taiwan)
            .or_else(|| local(fontdb::Language::Chinese_HongKongSAR))
            .unwrap_or_else(|| key.clone());
        out.push(FontFamily { key: key.clone(), name, cjk, latin });
    }
    // 中文組裡有中文名稱的（微軟正黑體、標楷體）排最前面，照字碼排的話會落到英文名稱後面。
    // 只限中文組：其他組裡有中文名稱的是「新細明體-ExtB」這種只有罕用字的，放最後就好
    let zh = |f: &FontFamily| f.cjk && f.name != f.key;
    out.sort_by(|a, b| {
        (b.cjk, zh(b)).cmp(&(a.cjk, zh(a))).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

pub struct Span {
    buffer: Buffer,
    x: f32,
    y: f32,
    color: Color,
    bounds: TextBounds,
}

pub struct Text {
    font_system: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    renderer: TextRenderer,
    spans: Vec<Span>,
    /// 跟 `QuadRenderer::set_fx` 一樣：之後 push 的乘上透明度、往下移。`begin` 會重設
    fx_alpha: f32,
    fx_dy: f32,
    /// 中文段與英文段用的字型（已確認這台電腦有）。`None` 是連預設字型都沒有，交給 fallback
    cjk: Option<String>,
    latin: Option<String>,
    /// 兩個字型各有哪些字重（`None` 是可變字型或沒有字型），見 [`snap_weight`]
    w_cjk: Option<Vec<u16>>,
    w_latin: Option<Vec<u16>>,
    /// 字級倍率。字級與行高都乘它，見 [`Text::shape`]。`pref_k` 是使用者設的，
    /// `k` 是這一幀實際用的（面板放不下時 [`Text::cap_scale`] 會壓低，`begin` 換回 `pref_k`）
    k: f32,
    pref_k: f32,
    /// 跟到 `PREF_GEN` 的第幾版
    seen_gen: u32,
    families: Option<Vec<FontFamily>>,
    /// 簡轉繁的結果（`None` = 原文照用）。一幀要排同一串好幾次（`fit` 二分、先量再畫），
    /// 每次都查字典太浪費
    zh: HashMap<String, Option<String>>,
}

impl Text {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db.set_sans_serif_family(DEFAULT_FONT);
        // 語系固定繁中。字型裡沒有的字（例如英文手寫體裡的中文）照語系挑 fallback，
        // 系統語系不是 zh-TW 時會挑到微軟雅黑，「啟」「查」之類的字變成簡體字形
        let font_system = FontSystem::new_with_locale_and_db("zh-TW".into(), db);
        // 先問一次系統有沒有中文字型，沒有的話後面畫出來會整排豆腐，
        // 而且不會有任何錯誤 —— 寧可開場就講。
        let has_cjk = font_system
            .db()
            .faces()
            .any(|f| f.families.iter().any(|(n, _)| {
                let n = n.to_lowercase();
                n.contains("jhenghei") || n.contains("yahei") || n.contains("mingliu")
                    || n.contains("pmingliu") || n.contains("noto sans cjk")
                    || n.contains("noto sans tc") || n.contains("song")
            }));
        if !has_cjk {
            crate::warn!("警告：系統字型裡找不到中文字型，歌詞會變成豆腐");
        }

        let cache = Cache::new(&gpu.device);
        let viewport = Viewport::new(&gpu.device, &cache);
        let mut atlas = TextAtlas::new(&gpu.device, &gpu.queue, &cache, format);
        let renderer =
            TextRenderer::new(&mut atlas, &gpu.device, wgpu::MultisampleState::default(), None);

        let mut t = Self {
            font_system,
            swash: SwashCache::new(),
            atlas,
            viewport,
            renderer,
            spans: Vec::new(),
            fx_alpha: 1.0,
            fx_dy: 0.0,
            cjk: None,
            latin: None,
            w_cjk: None,
            w_latin: None,
            k: 1.0,
            pref_k: 1.0,
            seen_gen: 0,
            families: None,
            zh: HashMap::new(),
        };
        t.sync();
        t
    }

    pub fn begin(&mut self) {
        self.spans.clear();
        self.fx_alpha = 1.0;
        self.fx_dy = 0.0;
        self.sync();
        self.k = self.pref_k;
    }

    /// 到下一次 `begin` 為止，字級倍率最多 `max`（固定大小的面板放不下放大的字時用）
    pub fn cap_scale(&mut self, max: f32) {
        self.k = self.pref_k.min(max);
    }

    /// 跟上 [`set_font`] 改過的字型與倍率
    fn sync(&mut self) {
        let g = PREF_GEN.load(Ordering::Relaxed);
        if g == self.seen_gen {
            return;
        }
        self.seen_gen = g;
        let (latin, cjk, k) = PREF.lock().map(|p| p.clone()).unwrap_or_else(|_| (String::new(), String::new(), 1.0));
        self.pref_k = k;
        self.k = k;
        self.cjk = [cjk.as_str(), DEFAULT_FONT].into_iter().find(|n| self.has(n)).map(str::to_string);
        // 英文字型沒指定或這台電腦沒有：跟中文字型一樣
        self.latin = if self.has(&latin) { Some(latin) } else { self.cjk.clone() };
        let db = self.font_system.db();
        self.w_cjk = self.cjk.as_deref().and_then(|n| family_weights(db, n, true));
        self.w_latin = self.latin.as_deref().and_then(|n| family_weights(db, n, true));
    }

    /// 這台電腦有沒有這個字型家族（畫不好的 tricky font 當作沒有）
    fn has(&self, name: &str) -> bool {
        !name.is_empty()
            && !is_tricky(name)
            && self.font_system.db().faces().any(|f| f.families.iter().any(|(x, _)| x == name))
    }

    /// 這一幀的字級倍率（行高也乘它）
    pub fn font_scale(&self) -> f32 {
        self.k
    }

    /// 這台電腦裝的字型（第一次叫時才掃，之後用同一份）
    pub fn families(&mut self) -> &[FontFamily] {
        if self.families.is_none() {
            self.families = Some(list_families(self.font_system.db()));
        }
        self.families.as_deref().unwrap_or_default()
    }

    /// 字型的顯示名稱。`key` 空字串是預設的中文字型；清單裡沒有的（被移除的字型）原樣回傳
    pub fn family_name(&mut self, key: &str) -> String {
        let key = if key.is_empty() { DEFAULT_FONT } else { key };
        self.families().iter().find(|f| f.key == key).map_or_else(|| key.to_string(), |f| f.name.clone())
    }

    /// 排版。字級與行高都乘上字級倍率 `k`：行高不跟著放大的話，折行的字會疊在一起。
    /// 中文段與英文段各用各的字型（同一個字型時整段一起排）。
    /// `fonts` 是只有這一段要換的（英文字型, 中文字型）：字型清單的名字、預覽框
    ///
    /// 簡轉繁在這裡做（見 [`crate::zh`]）：量、截、畫都經過這裡，所以量出來的寬度
    /// 就是畫出來的那串字的寬度
    fn shape(&mut self, s: &str, size: f32, line_height: f32, max_w: Option<f32>, weight: u16, fonts: Option<(&str, &str)>) -> Buffer {
        let zh = self.traditional(s);
        let s = zh.as_deref().unwrap_or(s);
        let k = self.k;
        let (latin, cjk) = match fonts {
            Some((l, c)) => (Some(l), Some(c)),
            None => (self.latin.as_deref(), self.cjk.as_deref()),
        };
        let db = self.font_system.db();
        // 換掉的字型每次現查（不讀檔）；自己的字型用 `sync` 時查好的
        let snap = |name: Option<&str>, cached: &Option<Vec<u16>>| match (fonts, name) {
            (Some(_), Some(n)) => family_weights(db, n, false).map_or(weight, |w| snap_weight(&w, weight)),
            _ => cached.as_deref().map_or(weight, |w| snap_weight(w, weight)),
        };
        let (wl, wc) = (snap(latin, &self.w_latin), snap(cjk, &self.w_cjk));
        fn attrs(n: Option<&str>, w: u16) -> Attrs<'_> {
            Attrs::new().family(n.map_or(Family::SansSerif, Family::Name)).weight(Weight(w))
        }
        let (al, ac) = (attrs(latin, wl), attrs(cjk, wc));
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(size * k, line_height * k));
        buffer.set_size(max_w, None);
        if (latin, wl) == (cjk, wc) || s.is_empty() {
            buffer.set_text(s, &al, Shaping::Advanced, None);
        } else {
            let runs = script_runs(s);
            let spans = runs.iter().map(|&(a, b, c)| (&s[a..b], if c { ac.clone() } else { al.clone() }));
            buffer.set_rich_text(spans, &al, Shaping::Advanced, None);
        }
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer
    }

    /// 這串字要畫成的繁體版本，`None` 是原文照畫
    fn traditional(&mut self, s: &str) -> Option<String> {
        if !crate::zh::enabled() || !crate::zh::has_han(s) {
            return None;
        }
        if let Some(t) = self.zh.get(s) {
            return t.clone();
        }
        let t = crate::zh::to_trad(s);
        self.zh.insert(s.to_string(), t.clone());
        t
    }

    /// 行高放大後往上挪多少：呼叫端照原本的行高把字放在 `cy - line/2`，
    /// 往上挪半個多出來的行高，第一行的中線才留在原處。
    /// 要頂端對齊的呼叫端（版面本身跟著倍率放大的）把 y 加上這個值補回來
    pub fn lift(&self, line_height: f32) -> f32 {
        line_height * (self.k - 1.0) * 0.5
    }

    pub fn set_fx(&mut self, alpha: f32, dy: f32) {
        self.fx_alpha = alpha;
        self.fx_dy = dy;
    }

    /// 排一段文字並記下來，等 `prepare` 時一次送進 GPU。
    ///
    /// `max_w` 是斷行寬度，`None` 就不斷行。`bounds` 是裁切框 —— 歌詞區
    /// 與 preset 選單都需要，它跟排版寬度是兩回事。
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        s: &str,
        x: f32,
        y: f32,
        size: f32,
        line_height: f32,
        color: [f32; 4],
        weight: u16,
        max_w: Option<f32>,
        bounds: Option<TextBounds>,
    ) {
        let buffer = self.shape(s, size, line_height, max_w, weight, None);
        self.push_buffer(buffer, x, y - self.lift(line_height), color, bounds);
    }

    /// 同 `push`，但這一段的英文、中文換成 `fonts`（字型清單、預覽框用）。不斷行
    #[allow(clippy::too_many_arguments)]
    pub fn push_fonts(
        &mut self,
        fonts: (&str, &str),
        s: &str,
        x: f32,
        y: f32,
        size: f32,
        line_height: f32,
        color: [f32; 4],
        weight: u16,
        bounds: Option<TextBounds>,
    ) {
        let buffer = self.shape(s, size, line_height, None, weight, Some(fonts));
        self.push_buffer(buffer, x, y - self.lift(line_height), color, bounds);
    }

    fn push_buffer(&mut self, buffer: Buffer, x: f32, y: f32, color: [f32; 4], bounds: Option<TextBounds>) {
        let (y, color) = (y + self.fx_dy, [color[0], color[1], color[2], color[3] * self.fx_alpha]);
        let bounds = bounds.map(|b| TextBounds {
            top: b.top + self.fx_dy as i32,
            bottom: b.bottom + self.fx_dy as i32,
            ..b
        });
        let bounds = bounds.unwrap_or(TextBounds {
            left: i32::MIN / 2,
            top: i32::MIN / 2,
            right: i32::MAX / 2,
            bottom: i32::MAX / 2,
        });
        self.spans.push(Span {
            buffer,
            x,
            y,
            color: Color::rgba(
                (color[0] * 255.0) as u8,
                (color[1] * 255.0) as u8,
                (color[2] * 255.0) as u8,
                (color[3] * 255.0) as u8,
            ),
            bounds,
        });
    }

    /// 量一段文字要多寬多高。版面計算需要，不畫。
    pub fn measure(&mut self, s: &str, size: f32, line_height: f32, max_w: Option<f32>) -> (f32, f32) {
        self.measure_weight(s, size, line_height, max_w, 400)
    }

    /// 同上，但指定字重。**粗體比一般字寬** —— 用一般字重量出來的寬度去截
    /// 粗體字，長名字會衝出框外（preset 選單的反白列就是這樣破的）。
    pub fn measure_weight(
        &mut self,
        s: &str,
        size: f32,
        line_height: f32,
        max_w: Option<f32>,
        weight: u16,
    ) -> (f32, f32) {
        let buffer = self.shape(s, size, line_height, max_w, weight, None);
        let mut w: f32 = 0.0;
        let mut n = 0.0f32;
        for run in buffer.layout_runs() {
            w = w.max(run.line_w);
            n += 1.0;
        }
        (w, n * line_height * self.k)
    }

    /// 一行字（不斷行）多寬。`fonts` 同 `push_fonts`，沒給就用現在的字型
    pub fn line_width(&mut self, fonts: Option<(&str, &str)>, s: &str, size: f32, line_height: f32, weight: u16) -> f32 {
        let buffer = self.shape(s, size, line_height, None, weight, fonts);
        buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
    }

    /// 逐字用：排版成跟 `push` 一模一樣的樣子，回傳每一個排版行「亮到哪裡」。
    ///
    /// `lit` 是「唱到第幾個位元組」，可以帶小數（正在唱的那個字唱了一半）。
    /// 回傳 `(行頂, 行底, 亮的右緣)`，座標相對於文字左上角；沒亮到的行不回傳。
    /// 直接看排版結果裡每個字形的位置，所以折行、字距都跟真正畫出來的一致。
    pub fn lit_runs(
        &mut self,
        s: &str,
        size: f32,
        line_height: f32,
        max_w: Option<f32>,
        weight: u16,
        lit: f32,
    ) -> Vec<(f32, f32, f32)> {
        let buffer = self.shape(s, size, line_height, max_w, weight, None);
        // `push` 把整段往上挪了 `lift`，這裡的行頂跟著挪
        let lift = self.lift(line_height);
        let mut out = Vec::new();
        for run in buffer.layout_runs() {
            let mut right: Option<f32> = None;
            for g in run.glyphs {
                let (gs, ge) = (g.start as f32, g.end.max(g.start + 1) as f32);
                let f = ((lit - gs) / (ge - gs)).clamp(0.0, 1.0);
                if f <= 0.0 {
                    continue;
                }
                let r = g.x + g.w * f;
                right = Some(right.map_or(r, |x: f32| x.max(r)));
            }
            if let Some(r) = right {
                out.push((run.line_top - lift, run.line_top + run.line_height - lift, r));
            }
        }
        out
    }

    /// 截成一行。太長就砍掉尾巴補上刪節號。
    ///
    /// cosmic-text 只會換行不會截斷，所以「限寬」在單行的地方沒有用 ——
    /// 它會折成第二行疊到下面那排字上。preset 的名字動輒六七十個字元，
    /// 這是唯一擋得住的辦法。
    pub fn fit(&mut self, s: &str, size: f32, line_height: f32, max_w: f32) -> String {
        self.fit_weight(s, size, line_height, max_w, 400)
    }

    pub fn fit_weight(
        &mut self,
        s: &str,
        size: f32,
        line_height: f32,
        max_w: f32,
        weight: u16,
    ) -> String {
        if self.measure_weight(s, size, line_height, None, weight).0 <= max_w {
            return s.to_string();
        }
        // 以字元為單位二分。中文一個字比英文寬得多，不能用位元組數估。
        let chars: Vec<char> = s.chars().collect();
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let cand: String = chars[..mid].iter().collect::<String>() + "…";
            if self.measure_weight(&cand, size, line_height, None, weight).0 <= max_w {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        chars[..lo].iter().collect::<String>() + "…"
    }

    pub fn prepare(&mut self, gpu: &Gpu, width: u32, height: u32) -> Result<(), glyphon::PrepareError> {
        self.viewport.update(&gpu.queue, Resolution { width, height });
        // 排版是邏輯像素，glyphon 要的位置與裁切框是實體像素，字形照 `scale` 放大後才點陣化
        let k = crate::gfx::scale();
        let px = |v: i32| if k == 1.0 { v } else { (v as f32 * k).round() as i32 };
        let areas: Vec<TextArea> = self
            .spans
            .iter()
            .map(|s| TextArea {
                buffer: &s.buffer,
                left: s.x * k,
                top: s.y * k,
                scale: k,
                bounds: TextBounds {
                    left: px(s.bounds.left),
                    top: px(s.bounds.top),
                    right: px(s.bounds.right),
                    bottom: px(s.bounds.bottom),
                },
                default_color: s.color,
                custom_glyphs: &[],
            })
            .collect();
        let first = self.renderer.prepare(
            &gpu.device,
            &gpu.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            areas.iter().cloned(),
            &mut self.swash,
        );
        // 這一幀用到的字太多、圖集還是滿了：放掉「使用中」名單重來一次，
        // 寧可這幀前面畫過的字可能被擠掉一兩個，也不要整個畫面停住
        if let Err(glyphon::PrepareError::AtlasFull) = first {
            self.atlas.trim();
            return self.renderer.prepare(
                &gpu.device,
                &gpu.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash,
            );
        }
        first
    }

    pub fn render(&self, pass: &mut wgpu::RenderPass<'_>) -> Result<(), glyphon::RenderError> {
        self.renderer.render(&self.atlas, &self.viewport, pass)
    }

    /// **每一幀畫完都要叫一次**（主視窗在 `draw()` 尾、迷你視窗在它自己那幀尾）。
    ///
    /// glyphon 把 prepare 過的字形記成「使用中」，只有 trim 會清這份名單；
    /// 沒清的話所有用過的字形永遠不能被擠掉，圖集只能一直長大，長到顯示卡上限
    /// 之後每次 prepare 都是 AtlasFull。動畫會讓字級連續變化、每一級都是新字形，
    /// 一分鐘左右就塞滿 —— 症狀是主視窗停在某一幀、CPU 吃滿一顆核心（實際發生過）。
    pub fn end_frame(&mut self) {
        self.atlas.trim();
        // 排版快取（cosmic-text 的 shape-run-cache）：以「文字＋屬性」為鍵，字級不在鍵裡，
        // 所以動畫字級照樣命中。一樣要修剪，留最近 600 次 end_frame 用過的
        // （主視窗約 4 秒；縮在系統列只剩迷你視窗時約 2 分半）
        self.font_system.shape_run_cache.trim(600);
        // 簡轉繁快取：會一直長的是捲過去的歌詞與清單，滿了整個清掉重來就好
        if self.zh.len() > 4096 {
            self.zh.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Text;

    #[test]
    fn 中英文切段() {
        use super::script_runs;
        let seg = |s: &str| script_runs(s).iter().map(|&(a, b, c)| (s[a..b].to_string(), c)).collect::<Vec<_>>();
        assert_eq!(seg("That's just 那只是昨夜"), vec![("That's just ".into(), false), ("那只是昨夜".into(), true)]);
        // 全形括號是中文段，裡面的 D 是英文段；… 跟著前面
        assert_eq!(seg("選擇…（D）"), vec![("選擇…（".into(), true), ("D".into(), false), ("）".into(), true)]);
        // 開頭的空白併進第一段；只有空白的整段算英文
        assert_eq!(seg("  一場"), vec![("  一場".into(), true)]);
        assert_eq!(seg("   "), vec![("   ".into(), false)]);
        assert!(seg("").is_empty());
        // 段與段接起來就是原字串
        let s = "F11 包含跨所有螢幕，3 rpm 是 20 秒一圈";
        assert_eq!(seg(s).iter().map(|(t, _)| t.as_str()).collect::<String>(), s);
    }

    #[test]
    fn 字重換成家族裡有的() {
        use super::snap_weight;
        // 正黑體：300／400／700
        let jh = [300, 400, 700];
        assert_eq!(snap_weight(&jh, 400), 400);
        assert_eq!(snap_weight(&jh, 500), 400, "400～500 沒有到 500 為止更粗的，找更細的");
        assert_eq!(snap_weight(&jh, 600), 700, "500 以上先找更粗的");
        assert_eq!(snap_weight(&jh, 200), 300, "沒有更細的就找更粗的");
        // 只有一般字重的手寫體：粗體也用它
        assert_eq!(snap_weight(&[400], 600), 400);
        assert_eq!(snap_weight(&[400, 500], 450), 500);
        assert_eq!(snap_weight(&[], 600), 600);
    }

    /// 字型清單：微軟正黑體顯示繁中名稱、排在中文組；符號字型與直書家族不列
    /// （`cargo test --release --bin vitro -- 字型清單 --nocapture` 會印出整份）
    #[test]
    fn 字型清單() {
        let mut db = super::fontdb::Database::new();
        db.load_system_fonts();
        let list = super::list_families(&db);
        for f in &list {
            println!("{}\t{}\t{}", if f.cjk { "中" } else { "  " }, f.key, f.name);
        }
        let Some(jh) = list.iter().find(|f| f.key == super::DEFAULT_FONT) else {
            return; // 沒裝微軟正黑體的電腦
        };
        assert_eq!(jh.name, "微軟正黑體");
        assert!(jh.cjk);
        assert!(list.iter().all(|f| !f.key.starts_with('@')));
        assert!(!list.iter().any(|f| f.key.starts_with("Wingdings") || f.key == "Segoe MDL2 Assets"));
        // 標楷體畫出來部件錯位，不列；細明體系列正常，要列
        assert!(!list.iter().any(|f| f.key == "DFKai-SB"));
        if db.faces().any(|f| f.families.iter().any(|(n, _)| n == "PMingLiU")) {
            assert!(list.iter().any(|f| f.key == "PMingLiU"));
        }
        // 中文組在前
        let first_other = list.iter().position(|f| !f.cjk).unwrap_or(list.len());
        assert!(list[first_other..].iter().all(|f| !f.cjk));
    }

    /// 字級連續變化（動畫）會一直產生新字形。沒有 trim 的話每個字形都算「使用中」、
    /// 圖集收不回空間，長到上限就每幀 AtlasFull —— 主視窗因此停在某一幀、CPU 吃滿（實際發生過）。
    /// 要 GPU，所以 ignore：`cargo test --release --bin vitro -- --ignored atlas`
    #[test]
    #[ignore]
    fn atlas_不會被動畫字級塞滿() {
        let gpu = crate::gfx::Gpu::headless().unwrap();
        let mut t = Text::new(&gpu, crate::gfx::OFFSCREEN_FORMAT);
        let line = "너의 모든 것을 원해 and some more 一場遊戲一場夢 王傑";
        for i in 0..3000 {
            let size = 10.0 + (i % 760) as f32 * 0.25;
            t.begin();
            t.push(line, 0.0, 0.0, size, size * 1.3, [1.0; 4], 600, None, None);
            if let Err(e) = t.prepare(&gpu, 1920, 1080) {
                panic!("第 {i} 幀（字級 {size}）失敗：{e}");
            }
            t.end_frame();
        }
    }
}
