@echo off
chcp 65001 >nul
:: 卸载 FileUnlocker 右键菜单（需管理员权限）

net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [!] 请右键选择"以管理员身份运行"本脚本
    pause
    exit /b 1
)

reg delete "HKEY_CLASSES_ROOT\*\shell\FileUnlocker" /f >nul 2>&1
reg delete "HKEY_CLASSES_ROOT\Directory\shell\FileUnlocker" /f >nul 2>&1

echo [√] 已卸载右键菜单
pause
