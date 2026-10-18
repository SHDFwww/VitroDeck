@echo off
rem rustup 是用 --no-modify-path 裝的，所以 PATH 沒有被動過。
rem 要用 cargo 就透過這支，或自己先把 %USERPROFILE%\.cargo\bin 加進 PATH。
setlocal
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
cd /d "%~dp0"
if "%~1"=="" (
  cargo build --release
) else (
  cargo %*
)
endlocal
