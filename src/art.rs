//! 封面圖的快取與非同步載入。
//!
//! 下載與解碼在背景執行緒做，上傳 GPU 一定要回到算圖執行緒 —— 所以中間用 channel
//! 傳解好的像素，每幀 `pump()` 一次把到貨的做成材質。
//!
//! 磁碟快取放在 `cache/art/`，檔名是網址的雜湊。Spotify 的圖網址是穩定的，
//! 所以存下來之後重開就不用再下載。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use md5::{Digest, Md5};

use crate::gfx::Gpu;

pub struct ArtCache {
    map: HashMap<String, Arc<wgpu::TextureView>>,
    inflight: HashSet<String>,
    req_tx: Sender<(String, PathBuf)>,
    done_rx: Receiver<(String, Decoded)>,
    dir: PathBuf,
    /// 載入中先拿這張頂著，版面才不會跳。
    pub placeholder: Arc<wgpu::TextureView>,
}

impl ArtCache {
    pub fn new(gpu: &Gpu, dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let (req_tx, req_rx) = std::sync::mpsc::channel::<(String, PathBuf)>();
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        std::thread::Builder::new()
            .name("art".into())
            .spawn(move || worker(req_rx, done_tx))
            .expect("封面下載執行緒開不起來");

        // 中性灰，比純黑不刺眼，也看得出「還沒到」
        let px = [40u8, 42, 48, 255];
        let placeholder = crate::gfx::upload_rgba(gpu, 1, 1, &px, "art-placeholder");

        Self {
            map: HashMap::new(),
            inflight: HashSet::new(),
            req_tx,
            done_rx,
            dir,
            placeholder,
        }
    }

    fn path_of(&self, url: &str) -> PathBuf {
        let digest = Md5::digest(url.as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        self.dir.join(format!("{}.img", &hex[..24]))
    }

    /// 取得封面。還沒到就回 placeholder，並在背景去抓。
    pub fn get(&mut self, url: &str) -> Arc<wgpu::TextureView> {
        if url.is_empty() {
            return self.placeholder.clone();
        }
        if let Some(t) = self.map.get(url) {
            return t.clone();
        }
        if self.inflight.insert(url.to_string()) {
            let _ = self.req_tx.send((url.to_string(), self.path_of(url)));
        }
        self.placeholder.clone()
    }

    /// 直接塞位元組（SMTC 的縮圖走這條，它沒有網址）。
    pub fn put_bytes(&mut self, gpu: &Gpu, key: &str, bytes: &[u8]) {
        if self.map.contains_key(key) {
            return;
        }
        if let Ok(t) = crate::gfx::upload_encoded(gpu, bytes, "smtc-thumb") {
            self.map.insert(key.to_string(), t);
        }
    }

    /// 磁碟快取裡那張圖的原始位元組（迷你視窗取色用）。還沒下載到就 `None`。
    pub fn disk_bytes(&self, url: &str) -> Option<Vec<u8>> {
        if url.is_empty() {
            return None;
        }
        std::fs::read(self.path_of(url)).ok()
    }

    pub fn has(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    /// 只看快取，不會去下載。SMTC 的縮圖沒有網址，只能走這條。
    pub fn get_cached(&self, key: &str) -> Arc<wgpu::TextureView> {
        self.map.get(key).cloned().unwrap_or_else(|| self.placeholder.clone())
    }

    /// 每幀呼叫一次，把下載好的做成材質。回傳這一幀新到了幾張。
    pub fn pump(&mut self, gpu: &Gpu) -> usize {
        let mut n = 0;
        while let Ok((url, img)) = self.done_rx.try_recv() {
            self.inflight.remove(&url);
            match img {
                Ok(img) => {
                    let (w, h) = img.dimensions();
                    self.map.insert(url, crate::gfx::upload_rgba(gpu, w, h, &img, "album-art"));
                    n += 1;
                }
                // 沒抓到（空的）不用記
                Err(e) if e.is_empty() => {}
                Err(e) => crate::warn!("封面解碼失敗：{e}"),
            }
        }
        n
    }
}

/// 背景執行緒解好的圖：RGBA 像素，或沒抓到（空的）、解不開（錯誤訊息）
type Decoded = Result<image::RgbaImage, String>;

/// 解碼留在背景執行緒：640px 的 JPEG 一張要幾毫秒，剛啟動整排封面同一幀到貨時，
/// 在算圖執行緒解會卡住那一幀
fn decode(bytes: &[u8]) -> Decoded {
    if bytes.is_empty() {
        return Err(String::new());
    }
    image::load_from_memory(bytes).map(|i| i.to_rgba8()).map_err(|e| e.to_string())
}

fn worker(req_rx: Receiver<(String, PathBuf)>, done_tx: Sender<(String, Decoded)>) {
    let http = reqwest::blocking::Client::builder()
        .user_agent(crate::config::user_agent())
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .expect("reqwest client 建不起來");

    while let Ok((url, path)) = req_rx.recv() {
        // 先看磁碟。Spotify 的圖網址是穩定的，存過就不必再下載。
        if let Ok(bytes) = std::fs::read(&path) {
            let _ = done_tx.send((url, decode(&bytes)));
            continue;
        }
        let bytes = http
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes())
            .map(|b| b.to_vec())
            .unwrap_or_default();
        if !bytes.is_empty() {
            let _ = std::fs::write(&path, &bytes);
        }
        if done_tx.send((url, decode(&bytes))).is_err() {
            return; // 主程式收掉了
        }
    }
}
