//! 应用级共享状态：pending 文件路径与 pending 更新信息。
#![forbid(unsafe_code)]

use std::sync::Mutex;

use tauri::{Emitter, Manager};

use crate::updater::UpdateInfo;

/// 启动参数中携带、等待前端取走的文件路径。
///
/// 存在的意义：单实例回调或提权启动时，主窗口可能尚未创建，
/// 直接 emit 事件会丢失，所以先落地到 state，由前端挂载后主动取走。
pub struct PendingFile(pub Mutex<Option<String>>);

/// 启动静默检查发现的更新信息，同样落地等前端取走——
/// WebView 未加载完成时 emit 的 "update-available" 事件会丢失。
pub struct PendingUpdate(pub Mutex<Option<UpdateInfo>>);

/// 从命令行参数中找出第一个真实存在的文件/目录路径
pub fn extract_path_from_args(args: &[String]) -> Option<String> {
    args.iter()
        .skip(1) // 跳过 argv[0]（程序自身路径）
        .find(|a| std::path::Path::new(a).exists())
        .cloned()
}

/// 保存最新待处理路径。真正的消费由前端 `take_pending_file` 完成，
/// 因为 `emit` 成功不代表 WebView 已注册监听器。
pub fn set_pending_file(state: &PendingFile, path: String) {
    state.0.lock().unwrap().replace(path);
}

/// 一次性取走 pending 文件路径。
pub fn take_pending_file_impl(state: &PendingFile) -> Option<String> {
    state.0.lock().unwrap().take()
}

/// 推送新文件路径给主窗口（焦点 + 事件；窗口未就绪时仅落地 pending state）
pub fn push_file(app: &tauri::AppHandle, path: String) {
    let state = app.state::<PendingFile>();
    set_pending_file(&state, path.clone());
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_focus();
        // 这里不清 pending：Tauri 的 emit 在没有 JS 监听器时也可能返回 Ok。
        // 前端收到事件后会调用 take_pending_file，未收到则 mount 时兜底消费。
        let _ = window.emit("new-file", path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_file_take_is_one_shot() {
        let state = PendingFile(Mutex::new(None));
        set_pending_file(&state, "C:\\first.txt".into());
        assert_eq!(
            take_pending_file_impl(&state).as_deref(),
            Some("C:\\first.txt")
        );
        assert_eq!(take_pending_file_impl(&state), None);
    }

    #[test]
    fn pending_file_keeps_latest_path() {
        let state = PendingFile(Mutex::new(None));
        set_pending_file(&state, "C:\\first.txt".into());
        set_pending_file(&state, "C:\\second.txt".into());
        assert_eq!(
            take_pending_file_impl(&state).as_deref(),
            Some("C:\\second.txt")
        );
    }
}
