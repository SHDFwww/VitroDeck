//! 把 preset 的逐像素著色器包成完整的 GLSL，再交給 naga 翻成 WGSL。
//!
//! preset 裡的 `warp` / `comp` 只有一段 `shader_body { … ret = …; }`，
//! 前面那一大串 uniform 宣告是 butterchurn 執行期黏上去的。模板照抄它的，
//! 因為 preset 會直接引用 `q17`、`roam_cos`、`texsize` 這些名字。
//!
//! 305 個 preset 有著色器；能不能走這條路，決定的是 naga 的 GLSL 前端
//! 收不收這些程式碼 —— 所以有 `--check-shaders` 把全部編一遍。

/// 共用的前言。warp 與 comp 只差進來的 varying 名稱。
const PRELUDE: &str = r#"#version 440
// naga 的 GLSL 前端有三個限制，模板全部要繞過去：
//   1. 不吃 `#version 300 es`，也不吃 `precision` 宣告 → 改用桌面版 440
//   2. 不吃沒有 layout 限定子的裸 uniform → 全部給 binding
//   3. **用 Vulkan 風格的分離式取樣器**：型別是 `texture2D` ＋ `sampler`，
//      沒有 GLSL ES 那種合體的 `sampler2D`。
//      所以影像與取樣器分開宣告，再用巨集把 preset 寫的 `sampler_main`
//      展開成 `sampler2D(影像, 取樣器)` 這個建構式。
layout(binding = 0) uniform texture2D _img_sampler_main;
layout(binding = 1) uniform texture2D _img_sampler_fw_main;
layout(binding = 2) uniform texture2D _img_sampler_fc_main;
layout(binding = 3) uniform texture2D _img_sampler_pw_main;
layout(binding = 4) uniform texture2D _img_sampler_pc_main;
layout(binding = 5) uniform texture2D _img_sampler_blur1;
layout(binding = 6) uniform texture2D _img_sampler_blur2;
layout(binding = 7) uniform texture2D _img_sampler_blur3;
layout(binding = 8) uniform texture2D _img_sampler_noise_lq;
layout(binding = 9) uniform texture2D _img_sampler_noise_lq_lite;
layout(binding = 10) uniform texture2D _img_sampler_noise_mq;
layout(binding = 11) uniform texture2D _img_sampler_noise_hq;
layout(binding = 12) uniform texture2D _img_sampler_pw_noise_lq;
layout(binding = 13) uniform texture3D _img_sampler_noisevol_lq;
layout(binding = 14) uniform texture3D _img_sampler_noisevol_hq;
// MilkDrop 的 fw/fc/pw/pc **不是四張不同的貼圖**，是同一張配不同的
// 取樣器狀態：f=filtered／p=point、w=wrap／c=clamp。
// butterchurn 也是 bindSampler(1..4) 綁同一張 main。
layout(binding = 15) uniform sampler _smp_main;
layout(binding = 16) uniform sampler _smp_fw;
layout(binding = 17) uniform sampler _smp_fc;
layout(binding = 18) uniform sampler _smp_pw;
layout(binding = 19) uniform sampler _smp_pc;
layout(binding = 21) uniform sampler _smp_noise;
layout(binding = 22) uniform sampler _smp_noise_pt;

layout(std140, binding = 20) uniform Globals {
  float time;
  float decay;
  float bass;
  float mid;
  float treb;
  float vol;
  float bass_att;
  float mid_att;
  float treb_att;
  float vol_att;
  float frame;
  float fps;
  float gammaAdj;
  float echo_zoom;
  float echo_alpha;
  float echo_orientation;
  float invert;
  float brighten;
  float darken;
  float solarize;
  float fShader;
  float blur1_min;
  float blur1_max;
  float blur2_min;
  float blur2_max;
  float blur3_min;
  float blur3_max;
  float scale1;
  float scale2;
  float scale3;
  float bias1;
  float bias2;
  float bias3;
  vec2 resolution;
  vec4 aspect;
  vec4 texsize;
  vec4 texsize_noise_lq;
  vec4 texsize_noise_mq;
  vec4 texsize_noise_hq;
  vec4 texsize_noise_lq_lite;
  vec4 texsize_noisevol_lq;
  vec4 texsize_noisevol_hq;
  vec4 _qa;
  vec4 _qb;
  vec4 _qc;
  vec4 _qd;
  vec4 _qe;
  vec4 _qf;
  vec4 _qg;
  vec4 _qh;
  vec4 slow_roam_cos;
  vec4 roam_cos;
  vec4 slow_roam_sin;
  vec4 roam_sin;
  vec4 rand_frame;
  vec4 rand_preset;
};

// ↓ 以下的巨集會把型別名 texture2D 也蓋掉，所以一定要放在宣告之後
#define sampler_main sampler2D(_img_sampler_main, _smp_main)
#define sampler_fw_main sampler2D(_img_sampler_fw_main, _smp_fw)
#define sampler_fc_main sampler2D(_img_sampler_fc_main, _smp_fc)
#define sampler_pw_main sampler2D(_img_sampler_pw_main, _smp_pw)
#define sampler_pc_main sampler2D(_img_sampler_pc_main, _smp_pc)
#define sampler_blur1 sampler2D(_img_sampler_blur1, _smp_fc)
#define sampler_blur2 sampler2D(_img_sampler_blur2, _smp_fc)
#define sampler_blur3 sampler2D(_img_sampler_blur3, _smp_fc)
#define sampler_noise_lq sampler2D(_img_sampler_noise_lq, _smp_noise)
#define sampler_noise_lq_lite sampler2D(_img_sampler_noise_lq_lite, _smp_noise)
#define sampler_noise_mq sampler2D(_img_sampler_noise_mq, _smp_noise)
#define sampler_noise_hq sampler2D(_img_sampler_noise_hq, _smp_noise)
#define sampler_pw_noise_lq sampler2D(_img_sampler_pw_noise_lq, _smp_noise_pt)
#define sampler_noisevol_lq sampler3D(_img_sampler_noisevol_lq, _smp_noise)
#define sampler_noisevol_hq sampler3D(_img_sampler_noisevol_hq, _smp_noise)

#define q1 _qa.x
#define q2 _qa.y
#define q3 _qa.z
#define q4 _qa.w
#define q5 _qb.x
#define q6 _qb.y
#define q7 _qb.z
#define q8 _qb.w
#define q9 _qc.x
#define q10 _qc.y
#define q11 _qc.z
#define q12 _qc.w
#define q13 _qd.x
#define q14 _qd.y
#define q15 _qd.z
#define q16 _qd.w
#define q17 _qe.x
#define q18 _qe.y
#define q19 _qe.z
#define q20 _qe.w
#define q21 _qf.x
#define q22 _qf.y
#define q23 _qf.z
#define q24 _qf.w
#define q25 _qg.x
#define q26 _qg.y
#define q27 _qg.z
#define q28 _qg.w
#define q29 _qh.x
#define q30 _qh.y
#define q31 _qh.z
#define q32 _qh.w

#define texture2D texture
#define texture3D texture

const float PI = 3.141592653589793;
const float PI2 = 6.283185307179586;

vec3 lum(vec3 v) { return vec3(dot(v, vec3(0.32, 0.49, 0.29))); }

// naga 在「該出現宣告」的位置（例如 for 的條件式）看到 `float(` 會把 float
// 當型別、然後找不到識別字。包成函式就繞過去了，語意完全一樣。
float _f(int v) { return float(v); }
float _f(float v) { return v; }
float _f(bool v) { return v ? 1.0 : 0.0; }
"#;

/// warp 著色器的進出口。
const WARP_IO: &str = r#"
layout(location = 0) in vec2 uv;
layout(location = 1) in vec2 uv_orig;
layout(location = 2) in vec4 vColor;
// comp 的頂點著色器會逐角傳一個色調過來，43 個 preset 直接引用它
layout(location = 3) in vec3 hue_shader;
layout(location = 0) out vec4 fragColor;
"#;

/// comp 著色器的進出口。注意它拿到的是 `vUv` 不是 `uv`。
const COMP_IO: &str = r#"
layout(location = 0) in vec2 vUv;
layout(location = 1) in vec4 vColor;
layout(location = 2) in vec3 hue_shader;
layout(location = 0) out vec4 fragColor;
// 全域變數不能用另一個全域來初始化，所以用巨集代換而不是宣告
#define uv vUv
#define uv_orig vUv
"#;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Warp,
    Comp,
}

/// 把 `shader_body { … }` 包成一支完整的 fragment shader。
pub fn wrap(src: &str, kind: Kind) -> String {
    let src = src.trim();
    // **`shader_body` 之前可能還有全域宣告**，不能整段丟掉。
    // glsl-optimizer 在 preset 寫入 `uv` 這種 in varying 時會產生
    // `vec2 xlat_mutableuv3;` 這類可變副本，宣告就放在 shader_body 前面。
    let (globals, rest) = match src.find("shader_body") {
        Some(i) => (&src[..i], &src[i..]),
        None => ("", src),
    };
    // 剩下的 `shader_body { … }` 把外層大括號剝掉，留下裡面的敘述。
    let inner = match (rest.find('{'), rest.rfind('}')) {
        (Some(a), Some(b)) if b > a => &rest[a + 1..b],
        _ => rest,
    };
    let io = match kind {
        Kind::Warp => WARP_IO,
        Kind::Comp => COMP_IO,
    };
    let inner = fix_indexed_field(inner);
    let globals = fix_indexed_field(globals);
    let globals = swap_user_textures(&globals);
    let inner = inner.replace("float(", "_f(");
    let globals = globals.replace("float(", "_f(");
    format!(
        // 結尾一定要夾。MilkDrop 的回授緩衝是 8-bit，寫入時自動夾在 [0,1]，
        // preset 全都是照那個行為寫的。像 FractalDrop 那種銳化回授
        // （`ret = main + (main − blur1) * 0.5` 再乘 0.9）高頻淨增益是 1.35，
        // 沒有夾的話幾十幀就衝破 1，整個畫面變成純黑白。
        "{PRELUDE}{io}\n{globals}\nvoid main(void) {{\n  vec3 ret = vec3(0.0);\n  float rad = length(uv - 0.5);\n  float ang = atan(uv.y - 0.5, uv.x - 0.5);\n{inner}\n  fragColor = vec4(clamp(ret, 0.0, 1.0), 1.0) * vColor;\n}}\n"
    )
}

/// MilkDrop 允許 preset 自己宣告外部貼圖（`uniform sampler2D sampler_cells;`），
/// 對應到貼圖資料夾裡的圖檔。那些檔案不在我們手上，而且裸 uniform 宣告
/// naga 也不收 —— 所以把宣告換成指向雜訊貼圖的巨集。
///
/// 畫面會跟原版不同，但那 7 個 preset 至少跑得起來，不是整個消失。
fn swap_user_textures(src: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?m)^\s*uniform\s+sampler(2D|3D)\s+(\w+)\s*;\s*$").unwrap()
    });
    re.replace_all(src, |c: &regex::Captures| {
        let fallback = if &c[1] == "3D" { "sampler_noisevol_lq" } else { "sampler_noise_lq" };
        format!("#define {} {}
", &c[2], fallback)
    })
    .into_owned()
}

/// naga 的 GLSL 前端不支援「索引之後再取單一分量」，例如 `m[0].x`。
/// 但 `m[0][0]` 是等價寫法而且它收 —— 對矩陣與向量陣列都成立。
///
/// 只轉單一分量，`.xy` 這種 swizzle 不能動（那沒有等價的索引寫法）。
fn fix_indexed_field(src: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"\]\s*\.\s*([xyzwrgba])(?P<tail>[^A-Za-z0-9_])").unwrap()
    });
    re.replace_all(src, |c: &regex::Captures| {
        let idx = match &c[1] {
            "x" | "r" => 0,
            "y" | "g" => 1,
            "z" | "b" => 2,
            _ => 3,
        };
        format!("][{idx}]{}", &c["tail"])
    })
    .into_owned()
}

/// `--check-shaders`：把 305 個有著色器的 preset 全部編一遍，看 naga 收不收。
///
/// 這是「能不能支援 MilkDrop 2 那批 preset」的唯一有意義的答案 ——
/// 猜沒有用，編過才算。
pub fn check(dir: &std::path::Path) -> anyhow::Result<()> {
    let all = crate::preset::load_any(dir)?;
    let gpu = crate::gfx::Gpu::headless()?;
    println!("GPU: {}", gpu.adapter_name);

    let with = all.iter().filter(|(_, p)| p.has_shaders()).count();
    println!("{} 個 preset，其中 {with} 個有著色器", all.len());

    let mut ok = 0usize;
    let mut fail: Vec<(String, &'static str, String)> = Vec::new();
    let started = std::time::Instant::now();

    // naga 的錯誤是透過 wgpu 的 error scope 回報的，不是 Result
    for (name, p) in &all {
        for (src, kind, label) in [
            (&p.warp, Kind::Warp, "warp"),
            (&p.comp, Kind::Comp, "comp"),
        ] {
            if src.trim().is_empty() {
                continue;
            }
            let glsl = wrap(src, kind);
            // wgpu 30 的 error scope 改成 guard 物件，pop 在 guard 上而不是 device
            let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let _module = gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Glsl {
                    shader: glsl.as_str().into(),
                    stage: naga::ShaderStage::Fragment,
                    defines: Default::default(),
                },
            });
            match pollster::block_on(scope.pop()) {
                None => ok += 1,
                Some(e) => {
                    let msg = e.to_string();
                    if std::env::args().any(|a| a == "-v") {
                        println!("
--- {name} [{label}] ---
{msg}");
                    }
                    // 真正有資訊的是帶 "error:" 的那一行
                    let first = msg
                        .lines()
                        .map(|l| l.trim())
                        .find(|l| l.contains("error:"))
                        .unwrap_or("?")
                        .rsplit("error:")
                        .next()
                        .unwrap_or("?")
                        .trim()
                        .to_string();
                    fail.push((name.clone(), label, first));
                }
            }
        }
    }

    let total = ok + fail.len();
    println!(
        "\n編譯成功 {ok} / {total}（{:.1}%），耗時 {} ms",
        ok as f64 / total.max(1) as f64 * 100.0,
        started.elapsed().as_millis()
    );
    if !fail.is_empty() {
        println!("\n失敗的前 15 個：");
        for (n, k, e) in fail.iter().take(15) {
            println!("  [{k}] {n}\n      {e}");
        }
        // 把錯誤訊息分類，看是少了什麼語法還是真的不支援
        let mut kinds: std::collections::HashMap<String, usize> = Default::default();
        for (_, _, e) in &fail {
            let key = e.chars().take(60).collect::<String>();
            *kinds.entry(key).or_insert(0) += 1;
        }
        let mut v: Vec<_> = kinds.into_iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        println!("\n錯誤類型統計：");
        for (k, n) in v.iter().take(12) {
            println!("  {n:>4}  {k}");
        }
    }
    Ok(())
}
