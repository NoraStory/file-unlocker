/**
 * 扫描域 store（ADR-004）：filePath/processes/scanning/scanGeneration。
 * scanGeneration 并发防护逻辑收进 store，一处实现处处生效。
 */
import {
  getLockingProcesses,
  pathIsDirectory,
} from "../lib/api/scan";
import { toErrorMessage } from "../lib/api/errors";
import type { ProcessInfo } from "../lib/types";

/** 当前目标路径 */
export const filePath = $state<{ value: string | null }>({ value: null });
/** 占用进程列表 */
export const processes = $state<{ value: ProcessInfo[] }>({ value: [] });
/** 已完成过一次扫描（决定结果区显示空态还是数据） */
export const scanned = $state({ value: false });
/** 扫描进行中 */
export const scanning = $state({ value: false });
/** 当前目标是文件夹（右键菜单支持文件夹入口） */
export const isDirectory = $state({ value: false });
/** 目录模式扫描进度：null 表示无进度（单文件模式/未在扫描） */
export const scanProgress = $state<{ value: { done: number; total: number } | null }>({ value: null });
/** 扫描错误消息 */
export const scanError = $state<{ value: string | null }>({ value: null });

/**
 * 扫描代次：防止并发扫描交叠（事件路径不受 busy 按钮禁用约束）。
 * 每次发起 scan 递增，await 返回后若代次已变说明有更新的扫描接手，
 * 当前结果作废不再写入 state——UI 始终显示最后一次请求的结果。
 */
let scanGeneration = 0;

/** 发起扫描：设置目标路径并查询占用进程 */
export async function scan(path: string): Promise<void> {
  const gen = ++scanGeneration;
  filePath.value = path;
  scanError.value = null;
  scanProgress.value = null;
  scanning.value = true;
  // 右键菜单/拖拽传入的目录路径不带尾部分隔符，必须问后端真实文件类型；
  // 判定错了会对文件夹显示"删除文件"按钮（删除必失败）
  const dir = await pathIsDirectory(path);
  if (gen !== scanGeneration) return; // 已被更新的扫描取代
  isDirectory.value = dir;
  try {
    const outcome = await getLockingProcesses(path);
    if (gen !== scanGeneration) return; // 已被更新的扫描取代
    processes.value = outcome.processes;
    scanned.value = true;
  } catch (e) {
    if (gen !== scanGeneration) return;
    processes.value = [];
    scanned.value = true;
    scanError.value = toErrorMessage(e);
  } finally {
    // 只有最新一次扫描有权熄灭"扫描中"指示
    if (gen === scanGeneration) {
      scanning.value = false;
      scanProgress.value = null;
    }
  }
}

/** 静默刷新当前路径的扫描结果（kill 后调用）；返回是否成功 */
export async function refreshCurrent(): Promise<boolean> {
  const path = filePath.value;
  if (!path) return false;
  const gen = scanGeneration;
  try {
    const outcome = await getLockingProcesses(path);
    // 期间若用户发起了新扫描，刷新结果不得覆盖新扫描
    if (gen === scanGeneration) {
      processes.value = outcome.processes;
    }
    return true;
  } catch {
    /* 刷新失败保持原列表 */
    return false;
  }
}

/** 清空目标文件与全部扫描状态 */
export function clearFile(): void {
  filePath.value = null;
  processes.value = [];
  scanned.value = false;
  scanError.value = null;
  scanProgress.value = null;
}

/** 仅重置确认态等瞬态 UI 标记（新扫描开始时调用） */
export function resetTransient(): void {
  scanProgress.value = null;
}
