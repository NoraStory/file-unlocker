// 生成用：register-context-menu.bat 的 UTF-8 源模板。
// 构建产物用 iconv 转 GBK + CRLF（中文 Windows cmd 的解析要求），
// 保留此文件以便后续维护，直接改 .bat 会因编码问题损坏。
@echo off
rem ============================================
rem FileUnlocker 右键菜单注册脚本
rem 用法：直接双击运行（无管理员权限时会自动弹出 UAC 提权），
rem       或把 FileUnlocker.exe 拖到本脚本上注册任意位置的程序
rem ============================================

net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [*] 正在请求管理员权限，请在 UAC 弹窗中点击"是"...
    rem 用户拒绝 UAC 时，-Verb RunAs 抛异常；catch 分支保持窗口开启给出提示
    powershell -NoProfile -Command "try { Start-Process -FilePath '%~f0' -Verb RunAs -ArgumentList @('%~f1') } catch { Write-Host ''; Write-Host '[!] 未获得管理员权限，注册已取消。'; Write-Host '    如需注册，请重新运行本脚本并在 UAC 弹窗中点击\"是\"。'; Start-Sleep -Seconds 5 }"
    exit /b
)

set "EXE=%~f1"
if "%EXE%"=="" (
    rem 依次回退：cargo 产物 → NSIS 默认安装位置 → NSIS PerMachine 备选
    if exist "%~dp0..\src-tauri\target\release\file-unlocker.exe" (
        set "EXE=%~dp0..\src-tauri\target\release\file-unlocker.exe"
    ) else if exist "%~dp0..\src-tauri\target\release\FileUnlocker.exe" (
        set "EXE=%~dp0..\src-tauri\target\release\FileUnlocker.exe"
    ) else if exist "%ProgramFiles%\FileUnlocker\FileUnlocker.exe" (
        set "EXE=%ProgramFiles%\FileUnlocker\FileUnlocker.exe"
    ) else if exist "%LocalAppData%\FileUnlocker\FileUnlocker.exe" (
        set "EXE=%LocalAppData%\FileUnlocker\FileUnlocker.exe"
    )
)
if "%EXE%"=="" (
    echo [!] 未找到程序，请先执行 npm run tauri build 或安装 NSIS 安装包
    echo     或把 FileUnlocker.exe 拖到本脚本上重新运行
    pause
    exit /b 1
)

echo [*] 正在注册右键菜单（程序路径：%EXE%）

rem ---- 任意文件的右键菜单 ----
reg add "HKEY_CLASSES_ROOT\*\shell\FileUnlocker" /ve /d "解除文件占用" /f >nul
reg add "HKEY_CLASSES_ROOT\*\shell\FileUnlocker" /v "Icon" /d "%EXE%" /f >nul
reg add "HKEY_CLASSES_ROOT\*\shell\FileUnlocker" /v "Position" /d "Top" /f >nul
reg add "HKEY_CLASSES_ROOT\*\shell\FileUnlocker\command" /ve /d "\"%EXE%\" \"%%1\"" /f >nul

rem ---- 文件夹的右键菜单 ----
reg add "HKEY_CLASSES_ROOT\Directory\shell\FileUnlocker" /ve /d "解除文件夹占用" /f >nul
reg add "HKEY_CLASSES_ROOT\Directory\shell\FileUnlocker" /v "Icon" /d "%EXE%" /f >nul
reg add "HKEY_CLASSES_ROOT\Directory\shell\FileUnlocker" /v "Position" /d "Top" /f >nul
reg add "HKEY_CLASSES_ROOT\Directory\shell\FileUnlocker\command" /ve /d "\"%EXE%\" \"%%1\"" /f >nul

echo.
echo [完成] 注册成功！在任意文件/文件夹上右键即可看到"解除文件占用"
echo 提示：Windows 11 新式菜单可能将其收纳，需点击"显示更多选项"或按住 Shift 右键
echo.
pause
