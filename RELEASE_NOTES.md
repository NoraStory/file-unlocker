修复"点击选择文件没反应"。

## 修复

- **点击窗口后文件选择对话框不弹出**：Tauri v2 的权限系统要求前端调用的每个插件命令都在 capabilities 里显式授权，之前漏掉了文件对话框的授权（`dialog:allow-open`），调用被静默拒绝。已在 `src-tauri/capabilities/main.json` 补上。

拖拽文件、右键菜单传参不受此影响，一直可用。

## 下载

- FileUnlocker_0.1.3_x64-setup.exe — 安装版
- file-unlocker.exe — 免安装单文件
