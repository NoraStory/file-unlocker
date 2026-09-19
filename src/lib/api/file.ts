import { invoke } from "@tauri-apps/api/core";
import { isAppError, toErrorMessage, validatePath } from "./errors";

/** 立即删除文件。失败时抛出结构化错误（可按 code 分支：占用/权限/受保护） */
export async function deleteFile(filePath: string): Promise<void> {
  const invalid = validatePath(filePath);
  if (invalid) throw new Error(invalid);
  try {
    await invoke<void>("delete_file", { filePath, delete: true });
  } catch (e) {
    // 结构化错误原样上抛（保留 code 供 UI 分支），其余包装为 Error
    if (isAppError(e)) throw e;
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
    if (isAppError(e)) throw e;
    throw new Error(toErrorMessage(e));
  }
}
