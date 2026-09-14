<div align="center">

# 🔓 FileUnlocker

**一秒找出是谁锁住了你的文件，然后结束它。**

[![Windows 10/11](https://img.shields.io/badge/platform-Windows%2010%2F11-0078d4.svg)](https://github.com/features)
[![Tauri v2](https://img.shields.io/badge/built%20with-Tauri%20v2-24C8DB.svg)](https://v2.tauri.app)
[![Rust](https://img.shields.io/badge/backend-Rust-DEA584.svg?logo=rust)](https://www.rust-lang.org)
[![Svelte 5](https://img.shields.io/badge/frontend-Svelte%205-FF3E00.svg?logo=svelte)](https://svelte.dev)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](#license)

*安装包约 3~5 MB · 内存占用约 20~40 MB · 无任何后台驻留*

</div>

---

你是否也遇到过这样的弹窗？

> **操作无法完成，因为文件已在 "WINWORD.EXE" 中打开。**

FileUnlocker 一键解决这个问题：拖入（或右键）任意文件，立即列出所有锁定它的进程，点击按钮强制释放——原理与资源管理器"文件正在使用"对话框同源，并额外内置 PowerToys File Locksmith 式的**全系统句柄扫描**兜底。

## ✨ 功能特性

| | 特性 | 说明 |
|---|---|---|
| 🔍 | **双引擎检测** | Windows Restart Manager + 全系统句柄枚举，比任务管理器更准 |
| 🏷️ | **可读的进程信息** | 显示 "Microsoft Word" 而非 `WINWORD.EXE`（读取 exe 版本信息），附完整路径与 PID |
| ⚡ | **结束进程 / 进程树** | 单个进程或连同全部子进程一并结束（等价 `taskkill /T`） |
| 🗑️ | **删除 / 重启后删除** | 文件被死锁时可选"下次开机由内核删除"（LockHunter 同款方案） |
| 🖱️ | **右键菜单集成** | 资源管理器右键直达，`%1` 自动传参 |
| 🔁 | **单实例** | 连续右键多个文件不会开一堆窗口，新路径推送到已有窗口 |
| 🛡️ | **SeDebugPrivilege** | 自动启用调试特权，可枚举/结束 SYSTEM 级进程的句柄 |
| 🪟 | **Win11 原生质感** | 圆角 + 毛玻璃卡片 + 明暗跟随系统，Svelte 5 runes 零虚拟 DOM |

## 🚀 快速开始

### 从源码构建

```bash
git clone https://github.com/<你的用户名>/file-unlocker.git
cd file-unlocker
npm install
npm run tauri build
```

构建产物：

- 绿色单文件：`src-tauri/target/release/FileUnlocker.exe`
- NSIS 安装包：`src-tauri/target/release/bundle/nsis/*.exe`

### 注册右键菜单（可选）

以管理员身份运行：

```bat
scripts\register-context-menu.bat
```

支持两种用法：直接双击（注册 release 目录下的 exe），或把任意位置的 `FileUnlocker.exe` 拖到脚本上。卸载运行 `scripts\unregister-context-menu.bat`。

## 📖 使用方式

1. **拖拽**文件到窗口 / **点击**选择文件 / **右键**文件选择"解除文件占用"
2. 查看占用进程列表（PID、进程名、描述、路径、检测来源）
3. 点击红色 **结束进程释放**（或 **结束树** 连带子进程），列表自动刷新
4. 进程全部清空后，可选择 **删除文件**；若仍被顽固占用，选择 **重启后删除**

> 🖥️ 开发调试用 `npm run tauri dev`（启动时弹 UAC 属正常，因 manifest 要求管理员权限）。

## 🔬 工作原理

```
┌────────────────────────────────────────────────┐
│ 检测引擎 1：Restart Manager                     │
│ RmStartSession → RmRegisterResources           │
│ → RmGetList ×2（探测+取数）→ RmEndSession(RAII) │
│  · 与资源管理器"文件正在使用"同源，结果权威      │
├────────────────────────────────────────────────┤
│ 检测引擎 2：全系统句柄枚举（File Locksmith 原理）│
│ NtQuerySystemInformation(ExtendedHandleInfo)   │
│ → 按 "File" 对象类型过滤 → DuplicateHandle     │
│ → GetFinalPathNameByHandleW 逐个比对路径        │
│  · 抓到 RM 漏报的占用（SYSTEM 服务句柄等）       │
├────────────────────────────────────────────────┤
│ 按 PID 合并去重 → 进程路径/版本描述查询 → UI     │
└────────────────────────────────────────────────┘
```

- 全部 `unsafe` FFI 收敛在 3 个底层模块；RM 会话与进程句柄均 RAII 管理，不泄漏
- 句柄扫描只读解析路径，不干扰目标进程；对无权限进程自动跳过

## 📁 项目结构

```
src/                      # Svelte 5 + TS + Tailwind 4 前端
  App.svelte              # 主界面（拖拽区 + 进程列表 + 文件处置区）
  lib/api.ts              # Tauri IPC 封装
  lib/types.ts            # 后端结构体镜像
src-tauri/
  src/lock_detector.rs    # 双引擎合并 + kill / kill_tree
  src/handle_scan.rs      # 全系统句柄枚举
  src/winutil.rs          # 进程路径 / 版本信息 / SeDebugPrivilege
  src/file_actions.rs     # 删除 / 重启后延迟删除
  src/lib.rs              # commands + 单实例 + 启动参数捕获
  build.rs                # 注入 requireAdministrator manifest
scripts/
  gen-icon.mjs            # 零依赖生成 icon.ico
  register-context-menu.bat / unregister-context-menu.bat
```

## ❓ 常见问题

**Q: 为什么每次启动都要弹 UAC？**
结束 SYSTEM 进程、句柄枚举、重启后删除都需要管理员权限。若不需要这些能力，把 `src-tauri/build.rs` 中的 `requireAdministrator` 改为 `asInvoker` 即可。

**Q: 点击"结束进程"提示失败？**
部分受保护进程（如 `csrss.exe`）受 Windows 严苛保护，`TerminateProcess` 会被系统拒绝——这是设计如此，避免蓝屏。其余绝大多数进程均可正常结束。

**Q: Win11 右键菜单里没看到？**
新式右键菜单可能收纳了该入口：按住 `Shift` 右键查看完整菜单，或点击菜单底部的"显示更多选项"。

**Q: "重启后删除"安全吗？**
它只写入 `PendingFileRenameOperations` 注册表项，由内核在下次启动早期原子执行。请勿对系统目录下的文件使用。

## 🧰 技术栈

[Rust](https://www.rust-lang.org) · [windows-rs](https://github.com/microsoft/windows-rs) · [Tauri v2](https://v2.tauri.app) · [Svelte 5](https://svelte.dev) · [Tailwind CSS 4](https://tailwindcss.com) · [Vite](https://vitejs.dev)

设计参考：[PowerToys File Locksmith](https://github.com/microsoft/PowerToys)（句柄扫描原理）、[LockHunter](https://lockhunter.com/)（重启删除策略）。

## License

[MIT](LICENSE) © 2026
