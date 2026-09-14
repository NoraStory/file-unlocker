# FileUnlocker

一个小工具，解决一个具体的问题：文件被占用删不掉、移不动，系统只告诉你"文件已在 xxx 中打开"，却不让你做任何事。

把它拖入文件（或在文件上右键选择"解除文件占用"），它会列出当前锁住这个文件的所有进程，点一下就能结束进程、释放占用。也可以直接把文件删掉——即使它还被锁着，可以安排在下次重启时由系统删除。

## 能做什么

- 找出占用文件的进程，显示进程名、所在路径、PID，以及程序的文件描述（比如显示"腾讯桌面整理"而不是一堆看不懂的 exe 名）
- 结束单个进程，或者连子进程一起结束（进程树）
- 删除文件；文件被锁死删不掉时，可以计划重启后删除
- 右键菜单集成，单实例运行（连着右键几个文件不会开一串窗口）

检测用两条路：一是 Windows 自带的 Restart Manager，这是资源管理器判断"文件正在使用"用的同一套机制，结果可靠；二是遍历系统句柄表，把每个文件句柄的路径和目标文件比对。第二条路能抓到第一条漏掉的占用，比如某些系统服务悄悄持有的句柄。两条路的结果合并去重。

## 构建

需要 Rust（MSVC 工具链）和 Node.js。

```bash
npm install
npm run tauri build
```

产物：

- 免安装单文件：`src-tauri/target/release/file-unlocker.exe`
- NSIS 安装包：`src-tauri/target/release/bundle/nsis/`

程序默认要求管理员权限运行（结束系统进程、扫描句柄都需要）。不想要的话，把 `src-tauri/build.rs` 里的 `requireAdministrator` 改成 `asInvoker` 重新编译即可，普通权限下大部分功能仍可用。

## 右键菜单

以管理员运行 `scripts/register-context-menu.bat`（会自动弹 UAC），或者在文件管理器里把 exe 拖到脚本上。卸载用 `scripts/unregister-context-menu.bat`。

Windows 11 的新版右键菜单可能把它收进"显示更多选项"里，按住 Shift 再右键能直接看到。

跑测试：

```bash
cargo test --lib
```

测试不要用 `cargo test` 直接跑——bin 目标的测试二进制会被强制要求管理员权限，普通终端跑不起来，这是构建链的限制，全部测试都放在了 lib 里。

## 一些说明

"结束进程"用的是 TerminateProcess，没有商量余地，点之前看清是哪个进程。少数受系统保护的进程（如 csrss.exe）Windows 会拒绝终止，这是正常现象。

"重启后删除"的原理是把路径写进 PendingFileRenameOperations，由内核在下次启动早期执行删除。对付被死锁的文件很有效，但别对系统目录下的东西用。

检测句柄那一步是只读的，不会干扰目标进程；碰到没有权限打开的进程会自动跳过。

代码结构：`src-tauri/src/` 下 `lock_detector.rs` 是检测和结束进程，`handle_scan.rs` 是句柄扫描，`winutil.rs` 是些系统调用的公共封装，`file_actions.rs` 是删除相关，`lib.rs` 串起 Tauri 的命令。前端在 `src/`，Svelte 5 + Tailwind。

License: MIT
