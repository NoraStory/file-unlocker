import { invoke } from "@tauri-apps/api/core";
import { toErrorMessage } from "./errors";

/** 更新信息 */
export interface UpdateInfo {
  version: string;
  notes: string;
  assets: Array<{ name: string; kind: string; url: string; sha256: string }>;
  source: string;
}

/** 取走启动静默检查发现的更新信息（一次性；防 WebView 未就绪时事件丢失） */
export async function takePendingUpdate(): Promise<UpdateInfo | null> {
  try {
    return await invoke<UpdateInfo | null>("take_pending_update");
  } catch {
    return null;
  }
}

/**
 * 检查更新（GitHub → Gitee）。失败时抛错（不吞成 null），
 * 由调用方区分"检查失败"与"已是最新"。
 */
export async function checkUpdate(): Promise<UpdateInfo | null> {
  try {
    return await invoke<UpdateInfo | null>("check_update");
  } catch (e) {
    throw new Error(toErrorMessage(e));
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
