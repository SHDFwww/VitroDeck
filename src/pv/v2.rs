//! 文字PV_v2（設定裡的「文字PV_v2」，預設關）：跟原本的文字 PV 差在四件事。
//!
//! - **一首歌一個世界**：整首只用一種風格、一種段落場景（[`world`]），不再每段輪換。
//!   變化留在每句的構圖配方裡（[`super::vary::recipes`] 照舊）。
//! - **前後句留在畫面上**：正在唱的那句一開始就整句淡淡地在，唱到的字才亮；下一兩句先在自己的位置淡淡地等，
//!   唱過的暗下來留在場景裡。十三個場景都演，本來每句擺在畫面中間的那幾個改成捲動（[`super::scene::Track::rolls`]）。
//! - **顏色只從封面來**（[`super::style::Style::painted`]），拿掉色差、閃光這類多色或一直在動的東西（[`super::style::Style::v2`]）。
//!   底是夜色（藍到靛，挑離強調色最遠的那個，[`night`]）；強調色只在正在唱的字上，字背後亮一團光，唱完褪回字色。
//! - **換歌不中斷**：演著的時候切歌不退回一般歌詞頁，接著演下一首的片頭曲名卡等歌詞（[`super::gate`]）；
//!   換一份歌詞時，上一份的最後一幀疊在上面淡掉（[`super::Pv::freeze`]）。

use super::layout::Rng;
use super::scene::Scene;
use super::style::{self, Look, Segment};
use super::{smooth01, tint};
use crate::lyrics::Lyrics;

/// v2 的風格：安靜、發光、不描邊的三種（細的手寫體也撐得住）
pub const LOOKS: [Look; 3] = [Look::Drift, Look::Neon, Look::Cinema];
/// v2 的場景：全部（還要看風格演不演得了，[`Scene::fits`]）
pub const SCENES: [Scene; 13] = super::scene::SCENES;

/// 還沒唱到的字有多亮，唱過那句退到背景時有多亮。
/// 後面等著的句子再暗一層（`LATER` 倍），跟正在唱的這句、唱過的分得開
pub const WAIT: f32 = 0.4;
pub const PAST: f32 = 0.26;
/// 唱過那句退到背景時多糊（邏輯 px）
pub const PAST_BLUR: f32 = 1.8;
pub const LATER: f32 = 0.55;
/// 唱到的字最慢多久亮到強調色、唱完多久褪回字色（毫秒）
pub const RISE: f32 = 400.0;
pub const COOL: f32 = 800.0;
/// 夜色底的色相範圍（OKLCH 的度）：偏青的藍到靛（[`night`]）
pub const NIGHT: (f32, f32) = (245.0, 290.0);

/// 夜色底的色相（度）與彩度倍率：在 [`NIGHT`] 裡挑離強調色色相最遠的。
/// 金、橘的對面就在這段裡，直接用對面；紅、粉、紫靠青藍那頭，綠、青靠靛那頭。
/// 強調色本身是藍的話挑哪個都太近，底的彩度收到剩三成、接近中性的深灰，強調色靠亮度跳出來
/// （藍的強調色兩頭一樣遠，色相會在兩頭之間跳，彩度收掉了就看不出來）。
/// 強調色幾乎沒有彩度（灰階的封面）的時候色相是亂的，不收彩度
pub fn night(hue: f32, chroma: f32) -> (f32, f32) {
    let (lo, hi) = NIGHT;
    let anti = (hue + 180.0).rem_euclid(360.0);
    let h = if (lo..=hi).contains(&anti) {
        anti
    } else if tint::gap(lo, hue) >= tint::gap(hi, hue) {
        lo
    } else {
        hi
    };
    let k = 0.3 + 0.7 * smooth01((tint::gap(h, hue) - 20.0) / 30.0);
    (h, 1.0 - (1.0 - k) * smooth01((chroma - 0.02) / 0.05))
}

/// 這個場景要先淡淡地擺出後面幾句。
/// 縱深只擺一句：隔兩句的那句跟正在唱的在同一側，會疊在一起。
/// 倒影的下面是水面、牆與分鏡的格子留給唱過的句子，不先擺
pub fn ahead(scene: Scene) -> usize {
    match scene {
        Scene::Flight => 2,
        Scene::Mirror | Scene::Wall | Scene::Split => 0,
        _ => 1,
    }
}

/// 整首歌的風格與場景。`force` 是使用者按 O 鎖定的風格；`VITRO_PV_SCENE` 鎖定場景（實測用）。
/// 風格照整首唱多快偏好：慢歌偏電影、浮遊，快歌偏霓虹；場景從這個風格演得了的裡面挑
pub fn world(ly: &Lyrics, segs: &[Segment], seed: u64, force: Option<Look>) -> (Look, Scene) {
    let mut rng = Rng::new(seed ^ 0x0F01_1A00);
    let n = segs.len().max(1);
    let fast = (0..segs.len()).map(|k| style::fast(ly, segs, k)).sum::<f32>() / n as f32;
    let look = force.unwrap_or_else(|| {
        let w = |l: Look| match l {
            Look::Drift => 1.2 - 0.8 * fast,
            Look::Neon => 0.6 + 0.9 * fast,
            Look::Cinema => (0.9 - 0.9 * fast).max(0.05),
            _ => 0.0,
        };
        let total: f32 = LOOKS.iter().map(|&l| w(l)).sum();
        let mut r = rng.f() * total;
        LOOKS.iter().copied().find(|&l| {
            r -= w(l);
            r <= 0.0
        })
        .unwrap_or(Look::Drift)
    });
    let fits: Vec<Scene> = SCENES.iter().copied().filter(|s| s.fits(look)).collect();
    let scene = super::scene::forced().unwrap_or_else(|| {
        if fits.is_empty() {
            Scene::Plain
        } else {
            fits[((rng.f() * fits.len() as f32) as usize).min(fits.len() - 1)]
        }
    });
    (look, scene)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::Line;

    fn song(n: usize) -> Lyrics {
        Lyrics {
            synced: (0..n).map(|k| Line { t: k as i64 * 3000, text: "我們一起唱歌到天亮".into(), ..Default::default() }).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn 整首一個世界而且場景演得了() {
        let ly = song(30);
        let mut seen = std::collections::HashSet::new();
        for seed in 0..60 {
            let plan = super::super::plan::Plan::build(&ly, seed, None, 1920.0, 1080.0, true);
            let s0 = plan.segs[0];
            assert!(plan.segs.len() > 1);
            for s in &plan.segs {
                assert_eq!((s.look, s.scene, s.hue), (s0.look, s0.scene, 0.0));
                assert!(s.v2);
            }
            assert!(LOOKS.contains(&s0.look));
            assert!(SCENES.contains(&s0.scene) && s0.scene.fits(s0.look), "{s0:?}");
            seen.insert((s0.look as u8, s0.scene as u8));
        }
        // 不同的歌會拿到不同的世界
        assert!(seen.len() >= 4, "{seen:?}");
    }

    #[test]
    fn 夜色底挑離強調色最遠的() {
        // (強調色的色相, 該挑的)：金的對面在範圍裡；紅、紫靠青藍那頭；綠、青靠靛那頭
        for (hue, want) in [(80.0, 260.0), (26.0, NIGHT.0), (304.0, NIGHT.0), (147.0, NIGHT.1), (214.0, NIGHT.1)] {
            let (h, k) = night(hue, 0.15);
            assert!((h - want).abs() < 0.01, "{hue} → {h}");
            assert!(k > 0.99, "{hue} 離得夠遠，彩度不收：{k}");
            for i in 0..=45 {
                let o = NIGHT.0 + i as f32;
                assert!(tint::gap(o, hue) <= tint::gap(h, hue) + 0.01, "{hue}：{o} 比 {h} 遠");
            }
        }
        // 藍的強調色：挑哪個都太近，彩度收到剩三成多
        let (_, k) = night(264.0, 0.2);
        assert!((0.29..0.4).contains(&k), "{k}");
        // 灰階的封面色相是亂的，不收
        assert!(night(264.0, 0.005).1 > 0.99);
    }

    #[test]
    fn 鎖定的風格照用() {
        let ly = song(12);
        let plan = super::super::plan::Plan::build(&ly, 3, Some(Look::Paper), 1920.0, 1080.0, true);
        assert!(plan.segs.iter().all(|s| s.look == Look::Paper && s.scene.fits(Look::Paper)));
    }
}
