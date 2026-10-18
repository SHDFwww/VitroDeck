//! MilkDrop preset 的資料結構與載入。
//!
//! 資料來自 `--extract-presets` 從 butterchurn 的 bundle 挖出來的 JSON。
//! 那些包**已經是轉換過的資料**，不是原始的 `.milk`：
//! INI 已經變成 JSON、`warp`/`comp` 已經是 GLSL ES、方程式已經是 JS。
//! 所以這裡不需要 INI parser，也不需要 HLSL 翻譯器。

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::eel;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Wave {
    #[serde(rename = "baseVals")]
    pub base_vals: HashMap<String, f64>,
    pub init_eqs_str: String,
    pub frame_eqs_str: String,
    pub point_eqs_str: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Shape {
    #[serde(rename = "baseVals")]
    pub base_vals: HashMap<String, f64>,
    pub init_eqs_str: String,
    pub frame_eqs_str: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct RawPreset {
    #[serde(rename = "baseVals")]
    pub base_vals: HashMap<String, f64>,
    pub waves: Vec<Wave>,
    pub shapes: Vec<Shape>,
    pub init_eqs_str: String,
    pub frame_eqs_str: String,
    pub pixel_eqs_str: String,
    /// 逐像素著色器，已經是 GLSL ES。空字串代表這是 MilkDrop 1 世代的 preset。
    pub warp: String,
    pub comp: String,
}

impl RawPreset {
    /// 有沒有逐像素著色器。`viz.js` 的 `classify()` 就是用這個分
    /// 「復古」與「炫砲」—— shader 是 MilkDrop 2 才加的東西。
    pub fn has_shaders(&self) -> bool {
        !self.warp.trim().is_empty() || !self.comp.trim().is_empty()
    }

    pub fn shader_len(&self) -> usize {
        self.warp.len() + self.comp.len()
    }
}

pub const PACKS: &[&str] = &["base", "extra", "extra2", "md1", "nonminimal"];

/// 讀進所有 preset。同名的以先讀到的為準 —— 跟 web 版把五包合併時
/// 後面覆蓋前面的行為相反，但那邊的順序本來就是任意的，
/// 這裡選「先到先得」是為了讓結果可重現。
pub fn load_all(dir: &Path) -> anyhow::Result<Vec<(String, RawPreset)>> {
    load_with(|pack| {
        let path = dir.join(format!("{pack}.json"));
        std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("讀不到 {}：{e}", path.display()))
    })
}

/// 編進 exe 的那一份（約 2.9 MB）。打包成單一 exe 時靠它 ——
/// 外面有 `presets` 資料夾的話仍然優先用外面的，開發時改了 JSON 不用重編。
const EMBEDDED: [(&str, &str); 5] = [
    ("base", include_str!("../presets/base.json")),
    ("extra", include_str!("../presets/extra.json")),
    ("extra2", include_str!("../presets/extra2.json")),
    ("md1", include_str!("../presets/md1.json")),
    ("nonminimal", include_str!("../presets/nonminimal.json")),
];

/// `dir` 裡有就讀它，沒有就用編進 exe 的那份。
pub fn load_any(dir: &Path) -> anyhow::Result<Vec<(String, RawPreset)>> {
    if dir.join("base.json").exists() { load_all(dir) } else { load_embedded() }
}

pub fn load_embedded() -> anyhow::Result<Vec<(String, RawPreset)>> {
    load_with(|pack| {
        EMBEDDED
            .iter()
            .find(|(n, _)| *n == pack)
            .map(|(_, s)| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("exe 裡沒有 {pack}.json"))
    })
}

fn load_with(
    read: impl Fn(&str) -> anyhow::Result<String>,
) -> anyhow::Result<Vec<(String, RawPreset)>> {
    let mut out: Vec<(String, RawPreset)> = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();
    for pack in PACKS {
        let text = read(pack)?;
        let map: HashMap<String, RawPreset> = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("{pack}.json 解不開：{e}"))?;
        let mut names: Vec<String> = map.keys().cloned().collect();
        names.sort();
        for name in names {
            if seen.insert(name.clone(), ()).is_none() {
                out.push((name.clone(), map[&name].clone()));
            }
        }
    }
    Ok(out)
}

/// 一個 preset 編譯好的程式。變數表是整個 preset 共用的 ——
/// init 設的值要被 frame 讀到，frame 設的 q1..q32 要被 pixel 讀到。
pub struct Program {
    pub vars: eel::Vars,
    pub init: Vec<eel::Stmt>,
    pub frame: Vec<eel::Stmt>,
    pub pixel: Vec<eel::Stmt>,
    pub waves: Vec<WaveProgram>,
    pub shapes: Vec<ShapeProgram>,
}

pub struct ShapeProgram {
    pub init: Vec<eel::Stmt>,
    pub frame: Vec<eel::Stmt>,
}

pub struct WaveProgram {
    pub init: Vec<eel::Stmt>,
    pub frame: Vec<eel::Stmt>,
    pub point: Vec<eel::Stmt>,
}

pub fn compile(p: &RawPreset) -> Result<Program, String> {
    let mut vars = eel::Vars::default();
    let init = eel::parse(&p.init_eqs_str, &mut vars)?;
    let frame = eel::parse(&p.frame_eqs_str, &mut vars)?;
    let pixel = eel::parse(&p.pixel_eqs_str, &mut vars)?;
    let mut waves = Vec::new();
    for w in &p.waves {
        waves.push(WaveProgram {
            init: eel::parse(&w.init_eqs_str, &mut vars)?,
            frame: eel::parse(&w.frame_eqs_str, &mut vars)?,
            point: eel::parse(&w.point_eqs_str, &mut vars)?,
        });
    }
    let mut shapes = Vec::new();
    for sh in &p.shapes {
        shapes.push(ShapeProgram {
            init: eel::parse(&sh.init_eqs_str, &mut vars)?,
            frame: eel::parse(&sh.frame_eqs_str, &mut vars)?,
        });
    }
    Ok(Program { vars, init, frame, pixel, waves, shapes })
}

/// `--check-presets`：把全部 preset 的方程式解析一遍，報告解不開的。
///
/// 這是直譯器唯一有意義的驗收標準 —— 方言是機器產生的，
/// 只要有一段解不開就代表我漏了某個語法。
pub fn check(dir: &Path) -> anyhow::Result<()> {
    let all = load_any(dir)?;
    println!("載入 {} 個 preset（去除同名後）", all.len());

    let mut ok = 0usize;
    let mut failed: Vec<(String, String)> = Vec::new();
    let mut with_shader = 0usize;
    let mut total_stmts = 0usize;
    let started = std::time::Instant::now();

    for (name, p) in &all {
        if p.has_shaders() {
            with_shader += 1;
        }
        match compile(p) {
            Ok(prog) => {
                ok += 1;
                total_stmts += prog.init.len() + prog.frame.len() + prog.pixel.len();
            }
            Err(e) => failed.push((name.clone(), e)),
        }
    }

    println!(
        "解析成功 {ok} / {}，有著色器的 {with_shader} 個，共 {total_stmts} 條頂層語句，耗時 {} ms",
        all.len(),
        started.elapsed().as_millis()
    );

    if failed.is_empty() {
        println!("全部解析成功。");
    } else {
        println!("\n解不開的 {} 個：", failed.len());
        for (n, e) in failed.iter().take(25) {
            println!("  {n}\n      {e}");
        }
        if failed.len() > 25 {
            println!("  …還有 {} 個", failed.len() - 25);
        }
    }

    // 再跑一次 init，確認執行期不會 panic 或卡死
    println!("\n開始實跑每個 preset 的 init_eqs…");
    let started = std::time::Instant::now();
    let mut ran = 0usize;
    let mut slowest = (0u128, String::new());
    for (name, p) in &all {
        let Ok(prog) = compile(p) else { continue };
        let t = std::time::Instant::now();
        let mut vm = eel::Vm::new(prog.vars.len());
        vm.run(&prog.init);
        vm.run(&prog.frame);
        ran += 1;
        let ms = t.elapsed().as_millis();
        if ms > slowest.0 {
            slowest = (ms, name.clone());
        }
    }
    println!(
        "跑完 {ran} 個，總耗時 {} ms，最慢的是「{}」{} ms",
        started.elapsed().as_millis(),
        slowest.1,
        slowest.0
    );
    Ok(())
}
