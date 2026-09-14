@echo off
chcp 65001 >nul
:: ============================================
:: FileUnlocker 右键菜单注册脚本（需管理员权限）
:: 用法：右键"以管理员身份运行"，或将 exe 拖到本脚本上
:: ============================================

net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [!] 请右键选择"以管理员身份运行"本脚本
    pause
    exit /b 1
)

set "EXE=%~1"
if "%EXE%"=="" set "EXE=%~dp0..\src-tauri\target\release\FileUnlocker.exe"
if not exist "%EXE%" (
    echo [!] 找不到程序：%EXE%
    echo     可将 FileUnlocker.exe 拖到本脚本上重新运行
    pause
    exit /b 1
)

echo [*] 正在注册右键菜单（程序路径：%EXE%）
set "EXE_Q=%EXE:"=%"

:: 文件右键菜单（任意文件）
reg add "HKEY_CLASSES_ROOT\*\shell\FileUnlocker" /ve /d "解除文件占用" /f >nul
reg add "HKEY_CLASSES_ROOT\*\shell\FileUnlocker" /v "Icon" /d "%EXE_Q%" /f >nul
reg add "HKEY_CLASSES_ROOT\*\shell\FileUnlocker" /v "Position" /d "Top" /f >nul
reg add "HKEY_CLASSES_ROOT\*\shell\FileUnlocker\command" /ve /d "\"%EXE_Q%\" \"%%1\"" /f >nul

:: 文件夹右键菜单
reg add "HKEY_CLASSES_ROOT\Directory\shell\FileUnlocker" /ve /d "解除文件夹占用" /f >nul
reg add "HKEY_CLASSES_ROOT\Directory\shell\FileUnlocker" /v "Icon" /d "%EXE_Q%" /f >nul
reg add "HKEY_CLASSES_ROOT\Directory\shell\FileUnlocker" /v "Position" /d "Top" /f >nul
reg add "HKEY_CLASSES_ROOT\Directory\shell\FileUnlocker\command" /ve /d "\"%EXE_Q%\" \"%%1\"" /f >nul

echo [√] 注册完成！在任意文件/文件夹上右键即可看到"解除文件占用"
echo     注意：Windows 11 若未显示，可能需在"设置-系统-开发者选项"或
echo     注册表中检查是否被 Win11 新式菜单收纳（可按住 Shift 右键查看旧式菜单）
pause
