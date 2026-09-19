import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { toErrorMessage } from "./errors";

/** 自检结果条目 */
export interface DiagItem {
  name: string;
  status: "ok" | "warn" | "fail";
  detail: string;
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

/** 当前应用版本号（读取失败返回空串，标题栏版本角标不显示） */
export async function getAppVersion(): Promise<string> {
  try {
    return await invoke<string>("app_version");
  } catch {
    return "";
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
