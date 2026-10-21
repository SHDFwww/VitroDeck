//! Spotify 給 SMTC 的播放位置，在「自然播完接下一首」之後比實際聽到的早，這裡把它扣回來。
//!
//! 2026-10-20 用 loopback 實測（見筆記.md「歌詞自動對齊」）：
//! - 無縫接到下一首時，Spotify 每 4.5 秒推一次的時間軸（Position＋LastUpdatedTime）整條**早約 480 ms**
//!   （推測報的是寫進輸出緩衝的位置）。快結束時（約剩 1 秒）它推一筆對的，位置往回跳約 -485。
//! - **手動切歌、拖進度條、暫停再播之後是準的**（用 SMTC 實際操作量過，X 都在 ±150 ms 內）。
//!
//! 所以只在「上一首播到結尾才換」的那段扣；Spotify 自己往回跳、或使用者做了任何操作就停。
//! 早多少從那次往回跳學（學到之前不修）。拖到結尾附近時 Spotify 也會推小幅更正（量到 -106），
//! 那時本來就不在「早」的狀態，不會被學進去。
//!
//! `VITRO_NO_LEAD=1` 關掉（比對用）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 往回跳多少算「Spotify 自己更正」。實測 34 首幾乎都在 466～500，少數 267～715
const LEARN_MS: std::ops::RangeInclusive<i64> = 200..=1000;
/// 新的一筆跟上一筆推過來的差超過這麼多，就是不連續（拖進度條之類），不是同一條時間軸
const SAME_CLOCK_MS: i64 = 60;
/// 上一首播到剩這麼多以內才換，算自然接歌。畫面藏起來時輪詢間隔 2 秒
const NATURAL_MS: i64 = 3000;

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    lead_ms: f64,
    samples: u32,
}

#[derive(Clone, Copy, PartialEq)]
struct Stamp {
    pos: i64,
    at: i64,
}

pub struct Lead {
    path: Option<PathBuf>,
    saved: Saved,
    off: bool,
    id: String,
    last: Option<Stamp>,
    playing: bool,
    /// 最近兩輪推到「當下」的位置與當時的曲長。換歌時 SMTC 會先換時間軸（位置歸零、曲長變成下一首的）、
    /// 下一輪才換曲名，所以判斷上一首是不是播完要看前兩輪，而且位置要跟同一輪的曲長比
    recent: [(i64, i64); 2],
    /// 現在這段是「自然接歌、Spotify 還沒自己更正」，要扣
    early: bool,
}

impl Lead {
    pub fn load(path: PathBuf) -> Self {
        let saved = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { path: Some(path), saved, ..Self::new() }
    }

    fn new() -> Self {
        Self {
            path: None,
            saved: Saved::default(),
            off: std::env::var_os("VITRO_NO_LEAD").is_some(),
            id: String::new(),
            last: None,
            playing: false,
            recent: [(0, 0); 2],
            early: false,
        }
    }

    /// 學到的值（毫秒），還沒學到是 `None`
    pub fn learned(&self) -> Option<i64> {
        (self.saved.samples > 0).then(|| self.saved.lead_ms.round() as i64)
    }

    /// 現在給的位置可不可信：自然接歌、Spotify 還沒自己更正，又還沒學到（或關掉了）要扣多少時不可信。
    /// 歌詞自動對齊在不可信的時候不錄，不然多出來的那半秒會被存成這首的偏移
    pub fn sure(&self) -> bool {
        !(self.early && (self.off || self.learned().is_none()))
    }

    /// 歌詞自動對齊關著時每輪呼叫，代替 [`Lead::fix`]：不修也不學。狀態清掉，
    /// 之後打開時當成剛開程式（不知道上一首怎麼結束的）
    pub fn idle(&mut self) {
        self.id.clear();
        self.last = None;
        self.playing = false;
        self.recent = [(0, 0); 2];
        self.early = false;
    }

    /// 每輪輪詢一次。回傳要從 `progress_ms` 扣掉的毫秒（0 是不修）。
    /// `stamp` 是 `smtc::Track::stamp`（位置, 時刻, 讀的當下）；web 來源沒有，就不修
    pub fn fix(&mut self, id: &str, playing: bool, dur: i64, stamp: Option<(i64, i64, i64)>) -> i64 {
        let Some((pos, at, now)) = stamp else {
            self.last = None;
            self.early = false;
            return 0;
        };
        if id != self.id {
            // 上一首有播到結尾才換的才算自然接歌；剛開程式（不知道上一首）當作不是
            let ended = self.recent.iter().any(|&(p, d)| d > 0 && p >= d - NATURAL_MS);
            self.early = !self.id.is_empty() && self.playing && ended;
            self.id = id.to_string();
            self.last = None;
            self.recent = [(0, 0); 2];
        }
        let s = Stamp { pos, at };
        if self.last != Some(s) {
            match self.last {
                // 位置一樣、只有時刻變新：快結束時停在終點那種，不是新的時間軸
                Some(prev) if prev.pos == pos => {}
                Some(prev) if self.playing && playing => {
                    let jump = pos - (prev.pos + (at - prev.at) / 10_000);
                    if self.early && LEARN_MS.contains(&-jump) {
                        self.learn(-jump);
                        self.early = false;
                    } else if jump.abs() > SAME_CLOCK_MS {
                        // 拖進度條：之後的時間軸是準的
                        self.early = false;
                    }
                }
                // 暫停（或從暫停恢復）：恢復後的時間軸是準的
                _ if self.playing != playing => self.early = false,
                _ => {}
            }
            self.last = Some(s);
        }
        if playing != self.playing {
            self.early = false;
        }
        self.playing = playing;
        let cur = if playing { pos + (now - at) / 10_000 } else { pos };
        self.recent = [self.recent[1], (cur, dur)];
        if self.off || !self.early || !playing { 0 } else { self.learned().unwrap_or(0) }
    }

    fn learn(&mut self, ms: i64) {
        let s = &mut self.saved;
        // 前幾次直接平均，之後指數平滑（新的佔三成），換耳機之類讓延遲變了也跟得上
        s.lead_ms = if s.samples < 3 {
            (s.lead_ms * s.samples as f64 + ms as f64) / (s.samples + 1) as f64
        } else {
            s.lead_ms * 0.7 + ms as f64 * 0.3
        };
        s.samples = s.samples.saturating_add(1);
        if let Some(p) = &self.path
            && let Ok(b) = serde_json::to_vec(s)
        {
            let _ = std::fs::write(p, b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Lead;

    const T0: i64 = 1_000_000_000;

    /// 一首歌從 `from` 播到結尾：每 4.5 秒一筆、時間軸連續；`end_jump` 是快結束時往回跳多少
    fn play(l: &mut Lead, id: &str, dur: i64, from: i64, end_jump: Option<i64>) -> Vec<i64> {
        let mut out = vec![];
        let mut pos = from;
        let at = |p: i64| T0 + p * 10_000;
        while pos < dur - 1000 {
            out.push(l.fix(id, true, dur, Some((pos, at(pos), at(pos)))));
            pos += 4500;
        }
        if let Some(j) = end_jump {
            out.push(l.fix(id, true, dur, Some((pos - j, at(pos), at(pos)))));
        }
        // 播到結尾
        out.push(l.fix(id, true, dur, Some((dur, at(dur), at(dur)))));
        out
    }

    fn lead() -> Lead {
        let mut l = Lead::new();
        l.off = false;
        l
    }

    #[test]
    fn 自然接歌才扣_學到之前不修() {
        let mut l = lead();
        // 剛開程式那首：不知道怎麼來的，不扣，但結尾的往回跳也不學（不在「早」的狀態）
        play(&mut l, "a", 30_000, 0, Some(480));
        assert_eq!(l.learned(), None);
        // 自然接到 b：還沒學到，不修；結尾往回跳學到 480
        assert!(l.sure(), "剛開程式那首當作準的");
        // 自然接到 b 的第一筆：還沒學到，位置不可信
        let at = |p: i64| T0 + p * 10_000;
        l.fix("b", true, 30_000, Some((0, at(40_000), at(40_000))));
        assert!(!l.sure());
        let b = play(&mut l, "b", 30_000, 0, Some(480));
        assert!(b.iter().all(|&x| x == 0));
        assert_eq!(l.learned(), Some(480));
        assert!(l.sure(), "Spotify 自己更正之後就準了");
        // 自然接到 c：整首扣，Spotify 自己更正之後不扣
        let c = play(&mut l, "c", 30_000, 0, Some(470));
        assert!(c[..c.len() - 2].iter().all(|&x| x == 480), "{c:?}");
        assert_eq!(c[c.len() - 2..], [0, 0]);
        assert_eq!(l.learned(), Some(475));
    }

    fn learned_lead() -> Lead {
        let mut l = lead();
        play(&mut l, "a", 30_000, 0, None);
        play(&mut l, "b", 30_000, 0, Some(500));
        assert_eq!(l.learned(), Some(500));
        l
    }

    /// 對齊關著的那段沒在看：打開後的第一首當成剛開程式，不管上一首怎麼結束的
    #[test]
    fn 關著之後當作剛開程式() {
        let mut l = learned_lead();
        play(&mut l, "c", 30_000, 0, None);
        l.idle();
        let at = |p: i64| T0 + p * 10_000;
        assert_eq!(l.fix("d", true, 30_000, Some((0, at(40_000), at(40_000)))), 0);
        assert!(l.sure());
    }

    #[test]
    fn 手動切歌不扣() {
        let mut l = learned_lead();
        play(&mut l, "c", 60_000, 0, None);
        // 播到 10 秒就切到 d（上一首離結尾還很遠）
        let at = |p: i64| T0 + p * 10_000;
        assert_eq!(l.fix("d", true, 60_000, Some((0, at(0), at(0)))), 500, "c 是自然接的，d 也是");
        assert_eq!(l.fix("d", true, 60_000, Some((10_000, at(10_000), at(10_000)))), 500);
        assert_eq!(l.fix("e", true, 60_000, Some((0, at(20_000), at(20_000)))), 0, "d 播到一半就換了");
    }

    #[test]
    fn 拖進度條之後不扣_也不學() {
        let mut l = learned_lead();
        let at = |p: i64| T0 + p * 10_000;
        assert_eq!(l.fix("c", true, 60_000, Some((0, at(0), at(0)))), 500);
        // 往後拖到 40 秒
        assert_eq!(l.fix("c", true, 60_000, Some((40_000, at(4500), at(4500)))), 0);
        // 之後 Spotify 推小幅往回的更正（實測 -106）：不學
        assert_eq!(l.fix("c", true, 60_000, Some((44_400, at(9000), at(9000)))), 0);
        assert_eq!(l.learned(), Some(500));
    }

    #[test]
    fn 暫停再播之後不扣() {
        let mut l = learned_lead();
        let at = |p: i64| T0 + p * 10_000;
        assert_eq!(l.fix("c", true, 60_000, Some((0, at(0), at(0)))), 500);
        assert_eq!(l.fix("c", false, 60_000, Some((3000, at(3000), at(3000)))), 0);
        assert_eq!(l.fix("c", true, 60_000, Some((3000, at(8000), at(8000)))), 0);
        assert_eq!(l.fix("c", true, 60_000, Some((7500, at(12_500), at(12_500)))), 0);
    }

    #[test]
    fn 換歌時位置先歸零也認得出播完() {
        let mut l = learned_lead();
        let at = |p: i64| T0 + p * 10_000;
        l.fix("c", true, 60_000, Some((0, at(0), at(0))));
        l.fix("c", true, 60_000, Some((59_500, at(59_500), at(59_500))));
        // SMTC 先把位置歸零、曲名還是 c
        l.fix("c", true, 60_000, Some((0, at(60_100), at(60_100))));
        assert_eq!(l.fix("d", true, 60_000, Some((300, at(60_400), at(60_400)))), 500);
    }

    /// 2026-10-20 實機〈破監牢〉自然接下一首的原始時間戳：結尾 Spotify 自己更正，接著位置歸零、下一輪才換曲名
    #[test]
    fn 實機_自然接歌() {
        let mut l = lead();
        l.saved.samples = 3;
        l.saved.lead_ms = 485.0;
        let a = 171_522;
        // b 是自然接進來的，正在扣
        (l.id, l.early, l.playing) = ("b".into(), true, true);
        let seq: &[(&str, i64, (i64, i64, i64))] = &[
            ("b", a, (162989, 134369690942894086, 134369690969062348)),
            ("b", a, (167503, 134369690988034494, 134369690989173315)),
            ("b", a, (167503, 134369690988034494, 134369691019263104)),
            ("b", a, (170540, 134369691023145036, 134369691024281422)),
            ("b", a, (170540, 134369691023145036, 134369691029310308)),
            // 時間軸已經換成下一首（位置歸零、曲長 232400），曲名還沒換
            ("b", 232_400, (6, 134369691033444010, 134369691034182078)),
            ("c", 232_400, (6, 134369691033444010, 134369691039210035)),
            ("c", 232_400, (6, 134369691033444010, 134369691044279912)),
        ];
        let out: Vec<i64> = seq.iter().map(|(id, d, s)| l.fix(id, true, *d, Some(*s))).collect();
        assert_eq!(out[6..], [482, 482], "{out:?}");
    }

    #[test]
    fn web_來源不修() {
        let mut l = learned_lead();
        assert_eq!(l.fix("c", true, 60_000, None), 0);
    }
}
