/**
 * 结构化错误与输入校验（ADR-002 前端侧）。
 * 错误码与后端 ErrorCode::as_str() 一一对应，发布后冻结。
 */

/** 结构化错误码（ADR-002）：与后端 ErrorCode::as_str() 一一对应，发布后冻结 */
export type ErrorCode =
  | "e_unknown"
  | "e_path_invalid"
  | "e_path_not_found"
  | "e_perm_denied"
  | "e_protected"
  | "e_file_busy"
  | "e_engine_failure"
  | "e_process_identity"
  | "e_internal"
  | "e_update_failure";

/** 后端 AppError 的 IPC 载荷：{ code, message } */
export interface AppErrorPayload {
  code: ErrorCode;
  message: string;
}

/**
 * 统一错误提取：Tauri invoke 失败时 reject 的值可能是 Error、string、
 * 或 ADR-002 结构化错误对象 { code, message }，这里收敛为可展示的中文消息。
 */
export function toErrorMessage(e: unknown): string {
  if (isAppError(e)) return e.message || "未知错误";
  if (typeof e === "string") return e || "未知错误";
  if (e instanceof Error) return e.message || "未知错误";
  try {
    return JSON.stringify(e);
  } catch {
    return "未知错误";
  }
}

/** 类型守卫：判断 reject 的值是否为后端结构化错误 */
export function isAppError(e: unknown): e is AppErrorPayload {
  return (
    typeof e === "object" &&
    e !== null &&
    "code" in e &&
    "message" in e &&
    typeof (e as AppErrorPayload).code === "string" &&
    typeof (e as AppErrorPayload).message === "string"
  );
}

/** 提取结构化错误码；非结构化错误返回 null（调用方按需回退） */
export function errorCodeOf(e: unknown): ErrorCode | null {
  return isAppError(e) ? e.code : null;
}

/** 前端输入校验：与后端规则呼应，提前拦截明显非法的路径 */
export function validatePath(filePath: string): string | null {
  if (!filePath.trim()) return "文件路径为空";
  if (!/^[a-zA-Z]:[\\/]/.test(filePath) && !filePath.startsWith("\\\\")) {
    return `需要 Windows 绝对路径，收到：${filePath}`;
  }
  return null;
}

/** 进程身份三元组校验：PID 复用防护的前端侧前置检查 */
export function validateProcessIdentity(
  pid: number,
  creationTime: string,
  exePath: string,
): void {
  if (!Number.isInteger(pid) || pid <= 0) {
    throw new Error(`无效的进程 ID：${pid}`);
  }
  if (!creationTime || !/^\d+$/.test(creationTime)) {
    throw new Error("进程创建时间无效，请重新检测后再操作");
  }
  if (!exePath.trim()) {
    throw new Error("进程路径为空，请重新检测后再操作");
  }
}
