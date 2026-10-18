// 換 preset 時的溶解。
//
// MilkDrop 換 preset 不是硬切，是一片一片溶過去。原版（butterchurn 的
// `createBlendPattern`）每次隨機挑一種圖樣，算出每個頂點的兩個常數 A、C，
// 然後每幀取 `alpha = clamp(A * 進度 + C, 0, 1)`。
// 位置不同、A 與 C 不同，所以畫面各處不是同時換完的。
//
// 原版有三種圖樣：方向抹除、電漿（遞迴中點位移）、放射。
// 這裡做了方向與放射兩種 —— 電漿要先產生一張雜訊圖，
// 而它跟另外兩種在畫面上的差別沒有大到值得為它多一張材質。
//
// 另外原版是連**取樣座標本身**都在兩個 preset 之間內插（`warpUVs`），
// 那需要兩個 preset 的 pixel 方程式跑在同一張網格上。這裡是各自算完再溶，
// 幾何不會互相牽扯，但溶解的節奏一樣。

struct U {
    /// x=cosθ y=sinθ z=1/寬度 w=模式（0=方向 1=放射 2=反向放射）
    pat: vec4<f32>,
    /// x=進度 yz=長寬比
    misc: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) & 1) * 4.0 - 1.0;
    var o: VsOut;
    o.pos = vec4<f32>(x, y, 0.0, 1.0);
    o.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return o;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let ax = u.misc.y;
    let ay = u.misc.z;
    var c: f32;
    if (u.pat.w < 0.5) {
        // 方向抹除：把位置投影到一條隨機方向的軸上
        let p = (in.uv.x * ax - 0.5) * u.pat.x + (in.uv.y * ay - 0.5) * u.pat.y + 0.5;
        c = (p - 0.5) / 1.41421356 + 0.5;
    } else {
        // 放射：離中心的距離，反向的話就從外面往內收
        let d = vec2<f32>((in.uv.x - 0.5) * ax, (in.uv.y - 0.5) * ay);
        c = 1.41421356 * length(d);
        if (u.pat.w > 1.5) {
            c = 1.0 - c;
        }
    }
    let n = u.pat.z;
    // A = n(1+寬度) = n + 1，C = n·c − n
    let a = clamp((n + 1.0) * u.misc.x + n * c - n, 0.0, 1.0);
    let col = textureSample(tex, samp, in.uv);
    // 目標是一般的 alpha 混合，來源已經是不透明的畫面
    return vec4<f32>(col.rgb * a, a);
}
