import { invoke } from "@tauri-apps/api/core";
import { toErrorMessage, validateProcessIdentity } from "./errors";

/** 强制结束指定 PID 的进程 */
export async function killProcess(
  pid: number,
  creationTime: string,
  exePath: string,
): Promise<void> {
  validateProcessIdentity(pid, creationTime, exePath);
  try {
    await invoke<void>("kill_process", {
      pid,
      creationTime,
      exePath,
    });
  } catch (e) {
    throw new Error(toErrorMessage(e));
  }
}

/** 结束整个进程树（含全部子进程） */
export async function killProcessTree(
  pid: number,
  creationTime: string,
  exePath: string,
): Promise<void> {
  validateProcessIdentity(pid, creationTime, exePath);
  try {
    await invoke<void>("kill_process_tree", {
      pid,
      creationTime,
      exePath,
    });
  } catch (e) {
    throw new Error(toErrorMessage(e));
  }
}
