/**
 * 更新域 store（ADR-004）：updateInfo/progress/error + 检查/下载动作。
 * 静默复查策略（启动时不弹面板、失败仅 console.warn）保持原行为。
 */
import {
  checkUpdate,
  downloadUpdate,
  type UpdateInfo,
} from "../lib/api/update";
import { toErrorMessage } from "../lib/api/errors";

/** 发现的新版本信息 */
export const updateInfo = $state<{ value: UpdateInfo | null }>({ value: null });
/** 更新面板展开中 */
export const updateOpen = $state({ value: false });
/** 手动检查进行中 */
export const updateChecking = $state({ value: false });
/** 下载进行中 */
export const updateDownloading = $state({ value: false });
/** 下载进度：null 表示未在下载 */
export const updateProgress = $state<{ value: { done: number; total: number } | null }>({ value: null });
/** 更新域错误消息 */
export const updateError = $state<{ value: string | null }>({ value: null });

/** 安装包资产（后端只接受 installer；缺失时禁止下载并提示） */
export function updateInstaller(): UpdateInfo["assets"][number] | null {
  return updateInfo.value?.assets.find((a) => a.kind === "installer") ?? null;
}

/**
 * 检查更新。silent 用于启动时的静默复查：失败仅 console.warn，
 * 成功也不弹面板；手动检查失败时保留已有 updateInfo
 * （启动时已知的新版本徽标不能因为检查失败而消失）。
 */
export async function checkForUpdates(silent = false): Promise<void> {
  updateChecking.value = true;
  if (!silent) updateError.value = null;
  try {
    const info = await checkUpdate();
    if (info) {
      updateInfo.value = info;
      if (!silent) updateOpen.value = true;
    } else if (!silent) {
      updateInfo.value = null;
      updateError.value = "已是最新版本";
      updateOpen.value = true;
    }
  } catch (e) {
    if (silent) {
      console.warn("静默检查更新失败:", toErrorMessage(e));
    } else {
      updateError.value = `检查更新失败：${toErrorMessage(e)}`;
      updateOpen.value = true;
    }
  } finally {
    updateChecking.value = false;
  }
}

/** 下载并安装更新（后端启动安装器后会退出本进程，此调用通常不可达返回） */
export async function doDownloadUpdate(): Promise<void> {
  if (!updateInfo.value || updateDownloading.value) return;
  const installer = updateInstaller();
  if (!installer) {
    updateError.value = "无安装包资产，无法自动安装";
    return;
  }
  updateDownloading.value = true;
  updateError.value = null;
  updateProgress.value = { done: 0, total: 0 };
  try {
    await downloadUpdate(installer);
    // 后端启动安装器后会退出本进程，此行为不可达
  } catch (e) {
    updateError.value = toErrorMessage(e);
    updateDownloading.value = false;
    updateProgress.value = null;
  }
}
