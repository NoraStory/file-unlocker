@echo off
rem ============================================
rem FileUnlocker 右键菜单卸载脚本
rem 用法：直接双击运行（无管理员权限时会自动弹出 UAC 提权）
rem ============================================

net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [*] 正在请求管理员权限，请在 UAC 弹窗中点击"是"...
    powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
    exit /b
)

reg delete "HKEY_CLASSES_ROOT\*\shell\FileUnlocker" /f >nul 2>&1
reg delete "HKEY_CLASSES_ROOT\Directory\shell\FileUnlocker" /f >nul 2>&1

echo [完成] 已卸载右键菜单
pause
