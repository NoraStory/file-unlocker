import { invoke } from "@tauri-apps/api/core";
import { toErrorMessage, validatePath } from "./errors";
import type { ScanOutcome } from "../types";

/** 查询占用指定文件的进程列表（含目录模式的截断信息） */
export async function getLockingProcesses(
  filePath: string,
): Promise<ScanOutcome> {
  const invalid = validatePath(filePath);
  if (invalid) throw new Error(invalid);
  try {
    const outcome = await invoke<ScanOutcome>("get_locking_processes", {
      filePath,
    });
    if (!outcome || !Array.isArray(outcome.processes)) {
      throw new Error("后端返回数据格式异常");
    }
    return outcome;
  } catch (e) {
    throw new Error(toErrorMessage(e));
  }
}

/** 判断路径是否为目录（目录路径通常不带尾部分隔符，前端无法靠字符串判断） */
export async function pathIsDirectory(path: string): Promise<boolean> {
  try {
    return await invoke<boolean>("is_directory", { path });
  } catch {
    return false;
  }
}
