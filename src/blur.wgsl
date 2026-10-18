// 可分離高斯模糊。玻璃底下那層要先模糊過。
//
// 三趟：先用盒狀平均降取樣，再水平、垂直各一趟高斯。
//
// **降取樣一定要自己平均，不能靠高斯那趟順便做。** 高斯的 tap 之間隔著
// 好幾個來源 texel，等於在對來源做點取樣 —— 背後有 12px 條紋這種高頻
// 圖樣時會直接摺疊成假紋路。盒狀平均把每個目標 texel 蓋到的來源全部吃
// 進去，之後的高斯就只是在一張已經沒有高頻的小圖上跑。

struct Params {
    /// 1 / 目標尺寸。**必須是目標不是來源** —— 降取樣那趟兩者不一樣大，
    /// 用錯的話只會取樣到來源的左上角那一塊。
    inv_target: vec2<f32>,
    /// 取樣方向，已經除過目標尺寸（所以單位是 texel）
    dir: vec2<f32>,
    /// 高斯趟：標準差（σ，單位是目標的 texel）。降取樣趟：降幾倍。
    sigma: f32,
    _pad: f32,
    _pad2: vec2<f32>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(1) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

/// 單邊取樣數。**每個 tap 就是一個 texel，間距固定 1。**
///
/// 間距一旦不是整數，tap 就會跟畫面上的圖樣打拍子：背後那排 24px 條紋
/// 降取樣之後是 3 個 texel 一循環，用 1.4 個 texel 的間距去取樣，17 個
/// tap 每次都落在差不多的相位上，平均完還留著整整 10% 的振幅 ——
/// 算出來的權重完全正確，就是取樣點錯了。條紋因此穿過毛玻璃透出來。
///
/// 覆蓋 ±16 個 texel，所以降取樣要讓 σ 落在 5 個 texel 以內（見 blur_div）。
const N: i32 = 16;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // 全螢幕三角形
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) & 1) * 4.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

/// 盒狀平均降取樣。一個雙線性 tap 剛好等於 2x2 個來源 texel 的平均，
/// 所以每軸取 div/2 個 tap 就把整個 div x div 的方塊吃乾淨。
@fragment
fn fs_down(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = pos.xy * p.inv_target;
    let n = i32(clamp(p.sigma * 0.5, 1.0, 8.0));
    var sum = vec3<f32>(0.0);
    for (var y = 0; y < n; y = y + 1) {
        for (var x = 0; x < n; x = x + 1) {
            let o = (vec2<f32>(f32(x), f32(y)) + 0.5) / f32(n) - 0.5;
            sum = sum + textureSample(src, samp, uv + o * p.inv_target).rgb;
        }
    }
    return vec4<f32>(sum / f32(n * n), 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = pos.xy * p.inv_target;

    var sum = vec3<f32>(0.0);
    var wsum = 0.0;
    let sigma = max(p.sigma, 0.4);
    for (var i = -N; i <= N; i = i + 1) {
        let off = f32(i);
        let w = exp(-0.5 * (off * off) / (sigma * sigma));
        sum = sum + textureSample(src, samp, uv + p.dir * off).rgb * w;
        wsum = wsum + w;
    }
    return vec4<f32>(sum / wsum, 1.0);
}
