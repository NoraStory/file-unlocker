<div align="center">

<img src="src-tauri/icons/icon.png" width="88" alt="FileUnlocker" />

# FileUnlocker

**谁锁了我的文件？一查便知，一键释放。**

文件被占用删不掉、移不动，系统只说"文件已在 xxx 中打开"，却不给任何办法——
把它拖进来，占用它的进程一目了然。

[![Windows 10/11](https://img.shields.io/badge/Windows-10%20%7C%2011-0078d4?logo=windows11&logoColor=white)](#-系统要求)
[![Release](https://img.shields.io/badge/Release-v0.3.7-2ea44f?logo=github)](https://github.com/NoraStory/file-unlocker/releases/tag/v0.3.7)
[![License: MIT](https://img.shields.io/badge/License-MIT-8A2BE2.svg)](LICENSE)
![Size](https://img.shields.io/badge/%E4%B8%BB%E7%A8%8B%E5%BA%8F-3.3%20MB-orange)

> ⚠️ **当前推荐 v0.3.7。** v0.3.4 及更早版本在句柄扫描阶段可能因系统异常句柄卡死/超时失败；v0.3.5 已修复该问题，v0.3.6 进一步修复目录扫描上限提示，v0.3.7 加固进程终止身份校验与路径匹配。建议使用 [v0.3.7](https://github.com/NoraStory/file-unlocker/releases/tag/v0.3.7)。

**[下载 v0.3.7](https://github.com/NoraStory/file-unlocker/releases/tag/v0.3.7)** · [加入右键菜单](#-右键菜单) · [自己构建](#-构建)

</div>

---

## 这是什么

一个 Windows 桌面小工具，专门解决"文件被占用"这件事：

- 拖入（或右键）任意文件 → 列出当前锁住它的**全部进程**
- 每个进程显示 PID、路径、程序描述——看到的是"腾讯桌面整理"而不是 `QQPCRTP.exe`
- 点一下结束进程，或连子进程一起结束（进程树）
- 文件被锁死删不掉？可以**删除**，或者**计划下次重启时由系统删除**（那时锁早没了）

## 检测原理

**v0.3.7 安全加固**：结束进程前会核对进程创建时间与可执行文件路径，避免 PID 复用后误杀无关进程；句柄扫描路径统一处理正斜杠、`.`、`..`、大小写与设备前缀。

两条独立的路，结果合并去重：

1. **Restart Manager** —— 资源管理器判断"文件正在使用"用的同一套机制，结果权威，还附带应用显示名
2. **全系统句柄扫描** —— 遍历系统句柄表，逐个解析文件句柄的真实路径再比对。能抓到 Restart Manager 漏掉的占用，比如某些系统服务悄悄持有的句柄

部分新版 Windows（实测 25H2）上 Restart Manager 对第三方进程的查询会异常，此时句柄扫描独立扛下全部工作，**不影响使用**。

> v0.3.5+ 对个别系统坏句柄加入了 1 秒超时保护：遇到无法解析的句柄会自动跳过并返回已收集结果，不会再导致整体扫描失败。
> v0.3.6 的目录模式按目录前缀完整扫描，不再提示“目录文件过多，仅检测前 2000 个”。

## 实测环境

以下数据全部来自真实运行，非理论值：

| 项目 | 环境 |
|---|---|
| 操作系统 | Windows 11 家庭中文版 26200（25H2） |
| CPU | AMD Ryzen 7 8845H |
| 内存 | 16 GB |
| 主程序体积 | 3.3 MB（单文件免安装） |
| 安装包体积 | 1.1 MB（NSIS） |
| 支持系统 | Windows 10 1809+ / 11，x64 |
| 推荐版本 | **v0.3.7**（v0.3.4 及更早存在句柄扫描超时失败问题） |

> 程序以管理员权限运行（结束系统进程、扫描句柄都需要）。
> 普通权限下检测和删除个人文件仍可用，仅结束系统进程会被拒绝。

## 下载使用

到 [v0.3.7 Release](https://github.com/NoraStory/file-unlocker/releases/tag/v0.3.7) 页面（当前推荐版本）：

| 文件 | 说明 |
|---|---|
| `FileUnlocker_x.y.z_x64-setup.exe` | 安装版，装完自动创建开始菜单项 |
| `file-unlocker.exe` | 免安装单文件，下载即用 |

使用方式三选一：**拖文件进窗口** · **点窗口选文件** · **右键菜单**（见下节）

## 右键菜单

注册脚本在 `scripts/` 目录，双击运行，UAC 弹窗点"是"即可：

```
register-context-menu.bat     注册（也可把 exe 拖到脚本上注册任意位置）
unregister-context-menu.bat   卸载
```

注册后，任意文件或文件夹右键即出现"解除文件占用"，固定在菜单顶部。

> Windows 11 的新版右键菜单可能把它收进"显示更多选项"，按住 **Shift** 再右键能直接看到。

## 构建

需要 Rust（MSVC 工具链）和 Node.js 18+：

```bash
git clone https://github.com/NoraStory/file-unlocker.git
cd file-unlocker
npm install
npm run tauri build     # 产物在 src-tauri/target/release/
npm run tauri dev       # 开发调试
cargo test --lib        # 运行测试
```

不想要求管理员权限的话，把 `src-tauri/build.rs` 里的 `requireAdministrator` 改成 `asInvoker` 重新编译。

> 注意：跑测试请用 `cargo test --lib`。直接 `cargo test` 会因为构建链把提权 manifest 链进 bin 测试目标而要求管理员终端。

## 项目结构

```
src/                          前端（Svelte 5 + Tailwind CSS 4）
  App.svelte                  主界面
  lib/api.ts                  Tauri IPC 封装与输入校验
src-tauri/src/
  lock_detector.rs            双引擎合并、结束进程/进程树
  handle_scan.rs              全系统句柄扫描
  winutil.rs                  进程路径、版本信息、错误码翻译
  file_actions.rs             删除 / 重启后删除
  lib.rs                      Tauri 命令、单实例、启动参数
scripts/                      右键菜单注册脚本、图标生成
```

## 须知

- 结束进程不可撤销，点之前看清是哪个程序
- 程序会核对 PID 创建时间和可执行文件路径，避免 PID 复用后误杀无关进程
- 少数受系统保护的进程（如 `csrss.exe`）Windows 会拒绝终止，这是系统设计，不是 bug
- "重启后删除"写入 `PendingFileRenameOperations`，由内核在下次启动早期执行，别对系统文件用
- 句柄扫描是只读的，不干扰目标进程；没权限的进程自动跳过

## License

[MIT](LICENSE)
