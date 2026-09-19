/**
 * 应用级 store（ADR-004）：版本号、全局 busy、自检、文件处置动作状态。
 */
import { open as pickDirectory } from "@tauri-apps/plugin-dialog";
import {
  deleteFile,
  deleteFileOnReboot,
  exportLogs,
  getAppVersion,
  getLogDir,
  openLogDir,
  runDiagnostics,
  toErrorMessage,
  type DiagItem,
} from "../lib/api";
import {
  clearFile,
  filePath,
  scanError,
  scanning,
} from "./scan.svelte";

/** 当前应用版本（启动时注入） */
export const appVersion = $state({ value: "" });

/** 终止进程状态（ProcessList 与 FileActions 共享互斥） */
export const killingPid = $state<{ value: number | null }>({ value: null });

/** 文件处置状态 */
export const fileActionBusy = $state<{ value: "" | "delete" | "delete-reboot" }>({ value: "" });
export const fileNotice = $state<{ value: string | null }>({ value: null });
/** 删除确认条展开中（替代原生 confirm，风格统一且不会被误触跳过） */
export const confirmingDelete = $state({ value: false });

/** 全局互斥：任一后端操作进行中时，其它操作按钮全部禁用。
 *  Svelte 5 不允许从模块导出 $derived，改用函数形式暴露当前值。 */
export function isBusy(): boolean {
  return scanning.value || killingPid.value !== null || fileActionBusy.value !== "";
}

/* ---- 自检面板状态 ---- */
export const diagOpen = $state({ value: false });
export const diagRunning = $state({ value: false });
export const diagItems = $state<{ value: DiagItem[] }>({ value: [] });
/** 日志目录路径（诊断面板展示） */
export const logDirPath = $state({ value: "" });
export const logNotice = $state<{ value: string | null }>({ value: null });

/** 注入版本号（App onMount 调用一次） */
export async function fetchVersion(): Promise<void> {
  appVersion.value = await getAppVersion();
}

/** 打开自检面板并执行诊断 */
export async function openDiagnostics(): Promise<void> {
  diagOpen.value = true;
  diagRunning.value = true;
  diagItems.value = [];
  refreshLogDir();
  try {
    diagItems.value = await runDiagnostics();
  } finally {
    diagRunning.value = false;
  }
}

async function refreshLogDir(): Promise<void> {
  logDirPath.value = await getLogDir();
}

/** 导出日志：选目录 → 后端复制日志文件 */
export async function doExportLogs(): Promise<void> {
  logNotice.value = null;
  try {
    const dir = await pickDirectory({
      directory: true,
      multiple: false,
      title: "选择日志导出位置",
    });
    if (typeof dir !== "string" || !dir) return;
    logNotice.value = await exportLogs(dir);
  } catch (e) {
    logNotice.value = `导出失败：${toErrorMessage(e)}`;
  }
}

export async function doOpenLogDir(): Promise<void> {
  logNotice.value = null;
  try {
    await openLogDir();
  } catch (e) {
    logNotice.value = `打开日志目录失败：${toErrorMessage(e)}`;
  }
}

/** 删除文件（立即删除；成功后清空目标） */
export async function doDelete(onReboot: boolean): Promise<void> {
  const path = filePath.value;
  if (!path || isBusy()) return;
  fileActionBusy.value = onReboot ? "delete-reboot" : "delete";
  scanError.value = null;
  fileNotice.value = null;
  try {
    if (onReboot) {
      await deleteFileOnReboot(path);
      fileNotice.value = "已计划：下次系统重启时删除该文件";
    } else {
      await deleteFile(path);
      // 删除成功后清空目标（clearFile 会一并清掉提示区，
      // 因此这里不再设置"已删除"提示——设置了也立即被清掉）
      clearFile();
    }
  } catch (e) {
    scanError.value = toErrorMessage(e);
  } finally {
    fileActionBusy.value = "";
  }
}
