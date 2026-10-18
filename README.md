<div align="center">

# VitroDeck

**讓 Spotify 變成一整面封面牆**

把收藏的專輯與播放清單排成可以滑過去的 3D 封面流，配上逐句同步歌詞、KTV 提詞與 MilkDrop 視覺效果。<br>
音樂還是由 Spotify 播，VitroDeck 只負責讓它好看。

[**下載最新版**](../../releases/latest) · [使用說明](https://vitrodeck.shdf.work/) · [回報問題](../../issues)

![封面流](site/img/coverflow.jpg)

</div>

---

## 需要什麼

| | |
|---|---|
| **Windows 10／11** | 顯示卡要支援 DirectX 12 或 Vulkan |
| **Spotify Premium** | 播放控制的 API 對免費帳號一律回 403 |
| **Spotify 桌面版** | 音樂由它播放，VitroDeck 讀取並控制它 |
| **一組自己的 Client ID** | 三分鐘申請，程式內有精靈帶你走 |

免安裝，單一執行檔。設定與快取存在 exe 旁邊，搬移時整個資料夾一起搬。

> 第一次執行時 Windows SmartScreen 可能會跳警告，
> 按「其他資訊 → 仍要執行」即可。

從 1.x 版（Python 版）升級：按程式內的更新，或下載新的 `VitroDeck.exe` 蓋掉舊的。
設定、登入與快取都會沿用，不用重新設定。

---

## 功能

**封面流**：方向鍵或滾輪滑過去，兩側封面帶透視傾斜與倒影。正在播的專輯自動置中，
不在收藏裡的話會臨時插進來。按 `Enter` 展開一張專輯或播放清單，直接挑曲目播。

**同步歌詞**：大字歌詞跟著唱到的句子走，可以用滾輪捲動，點任一句跳到那個時間點。
時間對不上可以微調，每首歌各自記住。有翻譯的歌可以按 `t` 顯示翻譯，按 `v` 切成唱片模式。

**KTV 提詞**：封面流上方顯示當前句與下一句，唱過的字逐漸填白。歌詞來源有逐字時間時會一個字一個字填。

**視覺效果**：內建 MilkDrop 引擎，跟著系統播出的聲音即時算圖，可以分類、加最愛、鎖定某一個。

**迷你視窗**：可以縮成桌面上的一小塊，顯示封面、曲名與當前句、下一句歌詞，也可以鎖定成滑鼠穿透。關閉主視窗時可以縮到系統列。

**外觀**：深色與淺色模式（淺色仍在實驗中），可自訂中文字型、英文字型、字級與介面大小。

**鍵盤為主**：翻頁、播放、搜尋、排序都有快捷鍵，按 `?` 叫出對照表，按 `,` 開設定。

<details>
<summary>快捷鍵一覽</summary>

| 鍵 | 作用 |
|---|---|
| `Tab` | 封面流 → 歌詞 → 視覺效果 |
| `L` | 封面流 ⇄ 歌詞 |
| `←` `→` | 上一張／下一張專輯 |
| `PgUp` `PgDn` | 一次十張 |
| `Home` `End` | 頭／尾 |
| `A`–`Z` | 跳到下一張以該字母開頭的 |
| `Enter` | 展開這張／播放這首 |
| `Esc` `Backspace` | 從曲目回到封面流 |
| `/` | 搜尋 |
| `空白鍵` | 播放／暫停 |
| `↑` `↓` | 上一首／下一首 |
| `X` | 隨機播放 |
| `E` | 循環模式 |
| `D` | 選擇播放裝置 |
| `F11` | 全螢幕 → 跨所有螢幕 → 視窗 |
| `-` `=` `\` | 封面縮小／放大／還原 |
| `S` | 專輯排序 |
| `R` | 重抓收藏清單 |
| `K` | KTV 提詞開關 |
| `V` | 唱片模式（歌詞頁） |
| `T` | 歌詞翻譯（歌詞頁） |
| `[` `]` `0` | 歌詞時間 ∓100 ms／歸零 |
| `,` | 設定 |
| `?` | 快捷鍵對照表 |
| `I` | 關於／中斷 Spotify 連結 |
| `Esc` | 退回一步 |
| `Q` | 關閉程式 |

視覺效果頁另有：`←` `→` 換 preset、`/` 或 `Enter` 開 preset 選單、`P` 鎖定、`C` 換分類、`H` 加入最愛、`F` 影格上限。

</details>

---

## 運作方式

播放狀態的主要來源是 **SMTC**（Windows 系統媒體傳輸控制），不是 Spotify Web API。

Web API 有請求配額，穩定輪詢會被鎖上數小時（實測 `Retry-After` 給過 14165 秒）。
SMTC 是本機介面，免費、沒有配額，可以讀得很密，反應比輪詢 API 更即時。

| 資料 | 來源 |
|---|---|
| 曲名／演出者／專輯名／進度／播放狀態／隨機／重複 | SMTC |
| 播放控制（上下首、播放暫停、跳轉、隨機、重複） | SMTC |
| 收藏專輯與播放清單 | Spotify Web API，快取到磁碟，按 `R` 才更新 |
| 專輯 URI 與 640px 封面 | 先比對收藏清單（0 次請求），比對不到才搜尋（每張專輯一次，永久存檔） |
| 播放指定專輯／曲目、裝置清單、音量、收藏 | Spotify Web API（使用者觸發） |
| 遠端裝置的播放狀態 | Spotify Web API，SMTC 只看得到本機 |
| 歌詞 | 依序查本機自訂 → 酷狗（逐字）→ 網易雲 → LRCLIB，查到就停；翻譯向網易雲借。不想用酷狗或網易雲，可在 `config.json` 把 `lyrics_kugou`、`lyrics_netease` 設成 `false` |
| 視覺效果的聲音 | WASAPI loopback，擷取的是系統輸出，不需要麥克風 |

穩定聽歌時 Web API 的請求數趨近於零。

**SMTC 不提供任何 Spotify ID**，所以要用「專輯名 + 演出者」回頭比對收藏清單。
`(Deluxe)`、`- Remastered`、`（豪華版）`、`[Live]` 這類後綴會先正規化再比對。
它的封面縮圖也只有 300×300，所以比對到收藏就改用快取的 640px。

**自訂歌詞**：把 `.lrc` 檔放進 `cache\lyrics\custom\`，檔名用 `演出者 - 曲名.lrc`，就會優先使用它。

---

## 已知限制

- **僅 Windows。** SMTC 與 WASAPI 都是 Windows 專有介面。
- 沒有播放裝置時第一次按 `Enter` 會失敗，先在 Spotify 隨便播一首，或按 `D` 手動選。
- 冷門曲目可能沒有歌詞，或只有沒有時間軸的純文字，那種情況不顯示 KTV 提詞。
- 只有行級時間的歌詞，KTV 填色是整句平均推進，不是真的逐字對齊。
- 視覺效果的 preset 是從 butterchurn 轉來的，少數 preset 跟原版畫面不完全一樣。

---

## 隱私

授權權杖、收藏快取、歌詞全部只存在你自己的電腦上。VitroDeck **沒有任何自己的伺服器**，
不收集、不上傳任何資料。程式會連線的只有 Spotify、上面列的歌詞來源，以及 GitHub（檢查更新，可在設定裡關閉）。

程式內按 `I` 可以隨時**中斷 Spotify 連結並刪除所有本機資料**。
也可以到 [Spotify 帳號頁面](https://www.spotify.com/account/apps/)移除授權。

---

## 從原始碼建置

需要：

- [rustup](https://rustup.rs/) 裝的最新 Rust 穩定版
- Visual Studio Build Tools 的「使用 C++ 的桌面開發」，含 Windows SDK（編圖示資源要用 `rc.exe`）

```bat
建置.bat          :: 等於 cargo build --release，輸出 target\release\vitro.exe
建置.bat test     :: 其他 cargo 指令照樣接在後面
打包.bat          :: 發行用，輸出 dist\VitroDeck.exe，並把本機路徑清出 exe
```

preset 與圖示都編進 exe，`dist\` 裡只要那一個檔。

相依套件的授權列在 [assets/THIRD-PARTY-LICENSES.txt](assets/THIRD-PARTY-LICENSES.txt)，相依有變動時用 `python gen_licenses.py` 重新產生。

---

## 授權

原始碼以 [MIT 授權](LICENSE)公開。

## 免責聲明

VitroDeck 是第三方獨立程式，**與 Spotify AB 無關聯，亦未經其認可或贊助**。
Spotify 是 Spotify AB 的商標。

專輯名稱、演出者與封面圖片由 Spotify Web API 提供；歌詞由各歌詞來源提供，
其著作權屬於各自權利人。使用本程式需要有效的 Spotify Premium 訂閱。
