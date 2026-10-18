@echo off
rem 編 release，輸出單一 exe 到 dist\VitroDeck.exe。
rem preset 與圖示都編進 exe 了，dist 裡只要這一個檔。
rem 設定與快取會寫在 exe 旁邊（DATA），第一次開會跳設定精靈。
rem 檔名跟 Python 版一樣是 VitroDeck.exe：2.0.0 起 Rust 版發到同一個 repo 直接取代 Python 版，
rem 程式內更新只認這個附檔名（也只有這個檔名的 exe 才會自動安裝更新）。
setlocal
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cd /d "%~dp0"
for %%I in ("%~dp0.") do set "ROOT=%%~fI"
rem panic 訊息會帶原始碼的絕對路徑，發行版把使用者資料夾與專案資料夾換掉。
rem RUSTFLAGS 用空白分隔，這兩個路徑有空白就會壞。
rem 另用 target\dist，旗標不同的兩份不會互相把對方整個重編。
set "RUSTFLAGS=--remap-path-prefix=%USERPROFILE%=~ --remap-path-prefix=%ROOT%=."
rem aws-lc-sys 的 C 程式碼用 __FILE__，RUSTFLAGS 管不到，另外叫 MSVC 把使用者資料夾修掉。
set "CFLAGS=/d1trimfile:%USERPROFILE%\"
set "CARGO_TARGET_DIR=%ROOT%\target\dist"
cargo build --release || exit /b 1
if not exist dist mkdir dist
copy /y target\dist\release\vitro.exe dist\VitroDeck.exe >nul || exit /b 1
echo dist\VitroDeck.exe
endlocal
