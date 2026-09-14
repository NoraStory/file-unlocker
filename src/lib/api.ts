import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { ProcessInfo } from "./types";

/** 查询占用指定文件的进程列表 */
export function getLockingProcesses(filePath: string) {
  return invoke<ProcessInfo[]>("get_locking_processes", { filePath });
}

/** 强制结束指定 PID 的进程 */
export function killProcess(pid: number) {
  return invoke<void>("kill_process", { pid });
}

/** 结束整个进程树（含全部子进程） */
export function killProcessTree(pid: number) {
  return invoke<void>("kill_process_tree", { pid });
}

/** 立即删除文件 */
export function deleteFile(filePath: string) {
  return invoke<void>("delete_file", { filePath, delete: true });
}

/** 计划下次重启时删除文件（对被锁定的文件有效） */
export function deleteFileOnReboot(filePath: string) {
  return invoke<void>("delete_file_on_reboot", { filePath, delete: true });
}

/** 取走启动参数中携带的初始文件路径（一次性） */
export function takePendingFile() {
  return invoke<string | null>("take_pending_file");
}

/** 弹出系统文件选择对话框（真实路径） */
export function pickFile() {
  return open({ multiple: false, title: "选择要检测的文件" });
}
