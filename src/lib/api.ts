import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { ProcessInfo } from "./types";

/**
 * 统一错误提取：Tauri invoke 失败时 reject 的值可能是 Error、string 或
 * 序列化后的对象，这里收敛为可展示的中文消息。
 */
export function toErrorMessage(e: unknown): string {
  if (typeof e === "string") return e || "未知错误";
  if (e instanceof Error) return e.message || "未知错误";
  try {
    return JSON.stringify(e);
  } catch {
    return "未知错误";
  }
}

/** 前端输入校验：与后端规则呼应，提前拦截明显非法的路径 */
function validatePath(filePath: string): string | null {
  if (!filePath.trim()) return "文件路径为空";
  if (!/^[a-zA-Z]:[\\/]/.test(filePath) && !filePath.startsWith("\\\\")) {
    return `需要 Windows 绝对路径，收到：${filePath}`;
  }
  return null;
}

/** 查询占用指定文件的进程列表 */
export async function getLockingProcesses(
  filePath: string,
): Promise<ProcessInfo[]> {
  const invalid = validatePath(filePath);
  if (invalid) throw new Error(invalid);
  try {
    const list = await invoke<ProcessInfo[]>("get_locking_processes", {
      filePath,
    });
    if (!Array.isArray(list)) {
      throw new Error("后端返回数据格式异常");
    }
    return list;
  } catch (e) {
    throw new Error(toErrorMessage(e));
  }
}

/** 强制结束指定 PID 的进程 */
export async function killProcess(pid: number): Promise<void> {
  if (!Number.isInteger(pid) || pid <= 0) {
    throw new Error(`无效的进程 ID：${pid}`);
  }
  try {
    await invoke<void>("kill_process", { pid });
  } catch (e) {
    throw new Error(toErrorMessage(e));
  }
}

/** 结束整个进程树（含全部子进程） */
export async function killProcessTree(pid: number): Promise<void> {
  if (!Number.isInteger(pid) || pid <= 0) {
    throw new Error(`无效的进程 ID：${pid}`);
  }
  try {
    await invoke<void>("kill_process_tree", { pid });
  } catch (e) {
    throw new Error(toErrorMessage(e));
  }
}

/** 立即删除文件 */
export async function deleteFile(filePath: string): Promise<void> {
  const invalid = validatePath(filePath);
  if (invalid) throw new Error(invalid);
  try {
    await invoke<void>("delete_file", { filePath, delete: true });
  } catch (e) {
    throw new Error(toErrorMessage(e));
  }
}

/** 计划下次重启时删除文件（对被锁定的文件有效） */
export async function deleteFileOnReboot(filePath: string): Promise<void> {
  const invalid = validatePath(filePath);
  if (invalid) throw new Error(invalid);
  try {
    await invoke<void>("delete_file_on_reboot", { filePath, delete: true });
  } catch (e) {
    throw new Error(toErrorMessage(e));
  }
}

/** 取走启动参数中携带的初始文件路径（一次性） */
export async function takePendingFile(): Promise<string | null> {
  try {
    const path = await invoke<string | null>("take_pending_file");
    return typeof path === "string" && path ? path : null;
  } catch (e) {
    // 启动阶段取不到 pending path 不影响主流程，仅记录
    console.error("take_pending_file 失败:", toErrorMessage(e));
    return null;
  }
}

/**
 * 弹出系统文件选择对话框（真实路径）。
 * 用户取消返回 null；调用失败（如权限缺失）向上抛出，
 * 由界面展示——权限类静默失败（capabilities 漏配等）不应无反馈。
 */
export async function pickFile(): Promise<string | null> {
  try {
    const path = await open({ multiple: false, title: "选择要检测的文件" });
    return typeof path === "string" ? path : null;
  } catch (e) {
    throw new Error(`打开文件选择器失败：${toErrorMessage(e)}`);
  }
}

/** 自检结果条目 */
export interface DiagItem {
  name: string;
  status: "ok" | "warn" | "fail";
  detail: string;
}

/** 更新信息 */
export interface UpdateInfo {
  version: string;
  notes: string;
  assets: Array<{ name: string; kind: string; url: string; sha256: string }>;
  source: string;
}

/** 运行自检 */
export async function runDiagnostics(): Promise<DiagItem[]> {
  try {
    const list = await invoke<DiagItem[]>("run_diagnostics");
    return Array.isArray(list) ? list : [];
  } catch (e) {
    return [
      { name: "自检执行", status: "fail", detail: toErrorMessage(e) },
    ];
  }
}

/** 日志目录路径 */
export async function getLogDir(): Promise<string> {
  try {
    return await invoke<string>("get_log_dir");
  } catch {
    return "";
  }
}

/** 打开日志目录 */
export async function openLogDir(): Promise<void> {
  try {
    await invoke<void>("open_log_dir");
  } catch (e) {
    throw new Error(toErrorMessage(e));
  }
}

/** 导出日志到指定目录 */
export async function exportLogs(destDir: string): Promise<string> {
  try {
    return await invoke<string>("export_logs", { destDir });
  } catch (e) {
    throw new Error(toErrorMessage(e));
  }
}

/** 检查更新（GitHub → Gitee） */
export async function checkUpdate(): Promise<UpdateInfo | null> {
  try {
    return await invoke<UpdateInfo | null>("check_update");
  } catch (e) {
    console.error("check_update 失败:", toErrorMessage(e));
    return null;
  }
}

/** 下载并安装更新（后端校验 SHA256 后启动安装器并退出本程序） */
export async function downloadUpdate(
  asset: UpdateInfo["assets"][number],
): Promise<void> {
  try {
    await invoke<void>("download_update", { asset });
  } catch (e) {
    throw new Error(toErrorMessage(e));
  }
}
