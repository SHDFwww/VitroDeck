//! 程式內更新 —— Python 版 `app.py` 的 `check_update`／`install_update`／`_after_update`。
//!
//! 發版管道（使用者 2026-10-17 決定）：跟 Python 版**同一個** repo、同一個附檔名
//! `VitroDeck.exe`，版本從 2.0.0 起跳。所以 Python 版的用戶按「更新」就會換成這一支，
//! 設定、權杖、快取原地沿用（兩版的資料位置與格式一樣）。
//!
//! 流程：問 GitHub 最新 release → 16 條連線分段下載到 `VitroDeck.exe.new` → 核對 SHA-256 →
//! 執行中的 exe 改名成 `.old`（Windows 允許改名執行中的映像檔，只是不能刪）→
//! `.new` 改名成正式檔名 → 帶 `--after-update <pid>` 啟動新版 → 自己結束。
//! 新版等舊的程序結束才開始做事，順手刪掉 `.old`。
//!
//! **接 Python 版的班**：Python 版重開新版時帶的是 `--after-update <pid> <ppid>`（onefile 有兩個程序），
//! 所以 pid 收一個或兩個都要能處理。

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

pub const REPO: &str = "SHDFwww/VitroDeck";
pub const ASSET: &str = "VitroDeck.exe";
/// 同時開幾條連線。台灣部分 ISP 連 GitHub 的單一連線常只剩幾十 KB/s，但限的是每條連線
const PARTS: u64 = 16;

/// 版本字串 → 前三段數字。`v2.0.1`、`2.0.1-rc` 都吃
pub fn ver_tuple(s: &str) -> (u32, u32, u32) {
    let mut it = s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

#[derive(Clone, Debug, Default)]
pub struct Release {
    pub version: String,
    pub page: String,
    pub url: String,
    pub size: u64,
    pub sha: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    Idle,
    Checking,
    Latest,
    /// 有新版。`installable` = 打包版而且拿得到校驗碼，否則只給下載頁
    Available { version: String, installable: bool },
    Downloading { done: u64, total: u64 },
    Failed(String),
    /// 換好了，等主程式結束
    Ready,
}

pub struct Updater {
    stage: Mutex<Stage>,
    latest: Mutex<Option<Release>>,
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    /// 要主程式講的一句話（通知列）
    pub toast: Mutex<Option<String>>,
}

impl Default for Updater {
    fn default() -> Self {
        Self {
            stage: Mutex::new(Stage::Idle),
            latest: Mutex::new(None),
            done: Arc::new(AtomicU64::new(0)),
            total: Arc::new(AtomicU64::new(0)),
            toast: Mutex::new(None),
        }
    }
}

/// 打包版才自動安裝：開發時從 `target\release\vitro.exe` 跑，被線上版蓋掉就麻煩了
pub fn installable_build() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case(ASSET)))
        .unwrap_or(false)
}

impl Updater {
    pub fn stage(&self) -> Stage {
        let s = self.stage.lock().unwrap().clone();
        match s {
            Stage::Downloading { .. } => Stage::Downloading {
                done: self.done.load(Ordering::Relaxed),
                total: self.total.load(Ordering::Relaxed),
            },
            s => s,
        }
    }

    fn set(&self, s: Stage) {
        *self.stage.lock().unwrap() = s;
    }

    pub fn page(&self) -> String {
        self.latest
            .lock()
            .unwrap()
            .as_ref()
            .map(|r| r.page.clone())
            .unwrap_or_else(|| format!("https://github.com/{REPO}/releases"))
    }

    /// 設定面板那一列要寫的字
    pub fn label(&self) -> String {
        let v = crate::config::VERSION;
        match self.stage() {
            Stage::Idle => format!("VitroDeck {v}（檢查更新）"),
            Stage::Checking => "檢查中…".into(),
            Stage::Latest => format!("VitroDeck {v}，已是最新版"),
            Stage::Available { version, installable: true } => format!("有新版 {version}（按一下更新）"),
            Stage::Available { version, .. } => format!("有新版 {version}（開下載頁）"),
            Stage::Downloading { done, total } if total > 0 => format!("下載中 {}%", done * 100 / total),
            Stage::Downloading { .. } => "下載中…".into(),
            Stage::Failed(e) => format!("更新失敗：{e}"),
            Stage::Ready => "更新好了，重新開啟中…".into(),
        }
    }

    /// 背景問一次。`quiet`：開機自動檢查，沒新版就不出聲
    pub fn check(self: &Arc<Self>, quiet: bool) {
        if matches!(self.stage(), Stage::Checking | Stage::Downloading { .. } | Stage::Ready) {
            return;
        }
        self.set(Stage::Checking);
        let me = self.clone();
        std::thread::spawn(move || match fetch_latest() {
            Ok(None) => me.set(Stage::Latest),
            Ok(Some(rel)) => {
                let newer = ver_tuple(&rel.version) > ver_tuple(crate::config::VERSION) && !rel.url.is_empty();
                if newer {
                    let installable = installable_build() && !rel.sha.is_empty();
                    *me.toast.lock().unwrap() = Some(format!("有新版 {}，到設定更新", rel.version));
                    me.set(Stage::Available { version: rel.version.clone(), installable });
                } else {
                    me.set(Stage::Latest);
                    if !quiet {
                        *me.toast.lock().unwrap() = Some("已是最新版".into());
                    }
                }
                *me.latest.lock().unwrap() = Some(rel);
            }
            Err(e) => {
                crate::warn!("檢查更新失敗：{e:#}");
                me.set(if quiet { Stage::Idle } else { Stage::Failed(short(&e)) });
            }
        });
    }

    /// 下載、核對、換檔、啟動新版。成功就呼叫 `on_ready`（主程式收到就結束自己）。
    pub fn install(self: &Arc<Self>, on_ready: impl FnOnce() + Send + 'static) {
        let Some(rel) = self.latest.lock().unwrap().clone() else { return };
        if !matches!(self.stage(), Stage::Available { installable: true, .. }) {
            return;
        }
        self.done.store(0, Ordering::Relaxed);
        self.total.store(rel.size, Ordering::Relaxed);
        self.set(Stage::Downloading { done: 0, total: rel.size });
        let me = self.clone();
        std::thread::spawn(move || match install(&rel, &me.done, &me.total) {
            Ok(()) => {
                me.set(Stage::Ready);
                on_ready();
            }
            Err(e) => {
                crate::warn!("更新失敗：{e:#}");
                me.set(Stage::Failed(short(&e)));
            }
        });
    }
}

fn short(e: &anyhow::Error) -> String {
    let s = e.to_string();
    s.chars().take(40).collect()
}

fn http() -> anyhow::Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(crate::config::user_agent())
        .timeout(Duration::from_secs(30))
        .build()?)
}

/// 最新的正式 release。還沒有任何 release 就 `None`
fn fetch_latest() -> anyhow::Result<Option<Release>> {
    // 測試用：VITRO_UPDATE_API 指到本機的假 release（`--update-selftest` 那一套）
    let api = std::env::var("VITRO_UPDATE_API")
        .unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let r = http()?
        .get(api)
        .header("Accept", "application/vnd.github+json")
        .send()?;
    match r.status().as_u16() {
        404 => return Ok(None),
        // 未登入每小時 60 次，一般用不完
        403 | 429 => anyhow::bail!("GitHub 限流，稍後再試"),
        _ => {}
    }
    let rel: serde_json::Value = r.error_for_status()?.json()?;
    Ok(Some(parse_release(&rel)))
}

pub fn parse_release(rel: &serde_json::Value) -> Release {
    let tag = rel["tag_name"].as_str().unwrap_or("").trim();
    let asset = rel["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(ASSET))));
    // GitHub 會替上傳的附檔算 digest；舊 release 沒有就從說明裡找那串校驗碼
    let mut sha = asset
        .and_then(|a| a["digest"].as_str())
        .and_then(|d| d.strip_prefix("sha256:"))
        .unwrap_or("")
        .to_string();
    if sha.is_empty() {
        let body = rel["body"].as_str().unwrap_or("");
        sha = body
            .split(|c: char| !c.is_ascii_hexdigit())
            .find(|w| w.len() == 64)
            .unwrap_or("")
            .to_string();
    }
    Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        page: rel["html_url"].as_str().map(String::from).unwrap_or_else(|| format!("https://github.com/{REPO}/releases")),
        url: asset.and_then(|a| a["browser_download_url"].as_str()).unwrap_or("").to_string(),
        size: asset.and_then(|a| a["size"].as_u64()).unwrap_or(0),
        sha: sha.to_lowercase(),
    }
}

fn install(rel: &Release, done: &Arc<AtomicU64>, total: &Arc<AtomicU64>) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let new = with_suffix(&exe, ".new");
    let old = with_suffix(&exe, ".old");
    let got = download(&rel.url, &new, done, total).inspect_err(|_| {
        let _ = std::fs::remove_file(&new);
    })?;
    if got != rel.sha {
        let _ = std::fs::remove_file(&new);
        anyhow::bail!("校驗碼不符");
    }
    let _ = std::fs::remove_file(&old); // 上次更新沒刪成的
    if std::fs::rename(&exe, &old).is_err() {
        let _ = std::fs::remove_file(&new);
        anyhow::bail!("程式所在的資料夾不能寫入");
    }
    if std::fs::rename(&new, &exe).is_err() {
        let _ = std::fs::rename(&old, &exe); // 換不上去就把舊的擺回來
        anyhow::bail!("換不上新檔");
    }
    // 沿用這次的啟動參數；上次更新附上的 --after-update 一定在最後，切掉
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == "--after-update") {
        args.truncate(i);
    }
    // 測試旗標不能傳下去：新版帶著它啟動又會再更新一次，一路連鎖下去（實際發生過）
    args.retain(|a| a != "--update-selftest");
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    std::process::Command::new(&exe)
        .args(&args)
        .arg("--after-update")
        .arg(std::process::id().to_string())
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()?;
    Ok(())
}

fn with_suffix(p: &Path, suf: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suf);
    PathBuf::from(s)
}

/// 下載到 `dest`，回傳實際的 SHA-256（小寫十六進位）。
///
/// 跟 aria2 -x16 同一招：切成 16 段、每段一條連線用 Range 各抓各的；
/// 伺服器不吃 Range 就退回單線。斷線從斷掉的地方續抓，每段最多 5 次。
fn download(url: &str, dest: &Path, done: &Arc<AtomicU64>, total: &Arc<AtomicU64>) -> anyhow::Result<String> {
    let client = http()?;
    // 先問一個位元組：順便跟完轉址拿到真正的檔案網址，之後 16 條線不用各跳一次
    let probe = client.get(url).header("Range", "bytes=0-0").send()?.error_for_status()?;
    let real = probe.url().to_string();
    let size = (probe.status().as_u16() == 206)
        .then(|| probe.headers().get("content-range")?.to_str().ok()?.rsplit('/').next()?.parse::<u64>().ok())
        .flatten();
    drop(probe);

    match size {
        None => {
            let mut r = client.get(&real).send()?.error_for_status()?;
            total.store(r.content_length().unwrap_or(0), Ordering::Relaxed);
            let mut f = std::fs::File::create(dest)?;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = r.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                f.write_all(&buf[..n])?;
                done.fetch_add(n as u64, Ordering::Relaxed);
            }
        }
        Some(size) => {
            total.store(size, Ordering::Relaxed);
            std::fs::File::create(dest)?.set_len(size)?;
            let step = size.div_ceil(PARTS);
            let errors: Arc<Mutex<Vec<String>>> = Arc::default();
            let handles: Vec<_> = (0..size)
                .step_by(step as usize)
                .map(|start| {
                    let end = (start + step).min(size) - 1;
                    let (client, real, dest) = (client.clone(), real.clone(), dest.to_path_buf());
                    let (done, errors) = (done.clone(), errors.clone());
                    std::thread::spawn(move || {
                        if let Err(e) = part(&client, &real, &dest, start, end, &done) {
                            errors.lock().unwrap().push(e.to_string());
                        }
                    })
                })
                .collect();
            for h in handles {
                let _ = h.join();
            }
            if let Some(e) = errors.lock().unwrap().first() {
                anyhow::bail!("分段下載失敗：{e}");
            }
        }
    }
    let mut h = Sha256::new();
    let mut f = std::fs::File::open(dest)?;
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn part(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    start: u64,
    end: u64,
    done: &AtomicU64,
) -> anyhow::Result<()> {
    let mut pos = start;
    let mut last = anyhow::anyhow!("連線提早結束");
    for _ in 0..5 {
        let res = (|| -> anyhow::Result<()> {
            let mut r = client.get(url).header("Range", format!("bytes={pos}-{end}")).send()?;
            anyhow::ensure!(r.status().as_u16() == 206, "HTTP {}", r.status());
            let mut f = std::fs::OpenOptions::new().write(true).open(dest)?;
            f.seek(SeekFrom::Start(pos))?;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = r.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                f.write_all(&buf[..n])?;
                pos += n as u64;
                done.fetch_add(n as u64, Ordering::Relaxed);
            }
            Ok(())
        })();
        if pos > end {
            return Ok(());
        }
        if let Err(e) = res {
            last = e;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(last)
}

/// `--update-selftest`：不開視窗，檢查 → 有新版就安裝 → 印結果。驗證整條換檔流程用，
/// 搭配 `VITRO_UPDATE_API` 指到本機的假 release。
pub fn selftest() -> anyhow::Result<()> {
    let u = Arc::new(Updater::default());
    u.check(false);
    let wait = |u: &Updater| loop {
        std::thread::sleep(Duration::from_millis(100));
        let s = u.stage();
        if !matches!(s, Stage::Checking | Stage::Downloading { .. }) {
            return s;
        }
    };
    let s = wait(&u);
    println!("檢查：{s:?}");
    if matches!(s, Stage::Available { installable: true, .. }) {
        let t = Instant::now();
        u.install(|| {});
        let s = wait(&u);
        println!("安裝：{s:?}（{} ms）", t.elapsed().as_millis());
    }
    Ok(())
}

/// 新版剛被舊版叫起來：等舊的程序都結束（它們還握著單一實例的鎖、還佔著 exe 檔），
/// 再刪掉 `.old`。要在搶單一實例的鎖**之前**做。
pub fn after_update(args: &[String]) {
    let Some(i) = args.iter().position(|a| a == "--after-update") else { return };
    let pids: Vec<u32> = args[i + 1..].iter().map_while(|a| a.parse().ok()).collect();
    let end = Instant::now() + Duration::from_secs(20);
    for pid in pids {
        unsafe {
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
            // 開不了就是已經結束了
            if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
                WaitForSingleObject(h, left);
                let _ = CloseHandle(h);
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let old = with_suffix(&exe, ".old");
        // 舊程序剛結束時檔案鎖可能還沒放，多試幾次
        for _ in 0..10 {
            if std::fs::remove_file(&old).is_ok() || !old.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 版本比較() {
        assert_eq!(ver_tuple("v2.0.1"), (2, 0, 1));
        assert_eq!(ver_tuple("2.1"), (2, 1, 0));
        assert!(ver_tuple("v2.0.0") > ver_tuple("1.0.4"));
        assert!(ver_tuple("1.0.4") < ver_tuple(crate::config::VERSION), "Rust 版必須比 Python 版新，不然會被更新回 Python 版");
    }

    #[test]
    fn 解析_release_優先用_digest_沒有就從說明找() {
        let sha = "a".repeat(64);
        let rel = serde_json::json!({
            "tag_name": "v2.0.1", "html_url": "https://x/r",
            "assets": [
                {"name": "other.zip", "browser_download_url": "u0", "size": 1},
                {"name": "vitrodeck.exe", "browser_download_url": "u1", "size": 42, "digest": format!("sha256:{sha}")}
            ]
        });
        let r = parse_release(&rel);
        assert_eq!((r.version.as_str(), r.url.as_str(), r.size, r.sha.as_str()), ("2.0.1", "u1", 42, sha.as_str()));
        let body_sha = "B".repeat(64);
        let rel = serde_json::json!({
            "tag_name": "2.0.2", "body": format!("SHA-256: {body_sha}\n"),
            "assets": [{"name": "VitroDeck.exe", "browser_download_url": "u", "size": 1}]
        });
        assert_eq!(parse_release(&rel).sha, body_sha.to_lowercase());
    }
}
