<script lang="ts">
  /**
   * App 壳（ADR-004）：布局 + 全局事件监听 + 组件装配。
   * 业务状态与动作全部下沉到 stores/（scan/update/app）与子组件。
   */
  import { onMount } from 'svelte';
  import { fade } from 'svelte/transition';
  import { listen } from '@tauri-apps/api/event';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import { takePendingFile } from './lib/api/system';
  import { takePendingUpdate, type UpdateInfo } from './lib/api/update';
  import DropZone from './components/DropZone.svelte';
  import ProcessList from './components/ProcessList.svelte';
  import FileActions from './components/FileActions.svelte';
  import DiagnosticsPanel from './components/DiagnosticsPanel.svelte';
  import UpdatePanel from './components/UpdatePanel.svelte';
  import { appVersion, fetchVersion, openDiagnostics } from './stores/app.svelte';
  import {
    filePath,
    scan,
    scanError,
    scanProgress,
    scanning,
  } from './stores/scan.svelte';
  import {
    checkForUpdates,
    updateInfo,
    updateOpen,
    updateProgress,
    updateChecking,
  } from './stores/update.svelte';
  import { dragging } from './stores/drag.svelte';

  let unlisteners: Array<() => void> = [];

  /** 消费后端最新 pending 路径；事件和 mount 兜底共用同一入口。 */
  async function consumePendingFile(allowReplace = true) {
    const pending = await takePendingFile();
    if (pending && (allowReplace || !filePath.value)) await scan(pending);
  }

  onMount(() => {
    (async () => {
      // 右键菜单 / 二次启动传入的新文件路径
      unlisteners.push(
        await listen<string>('new-file', () => {
          void consumePendingFile(true);
        }),
      );

      // 目录模式扫描进度（done, total）
      // 只有进行中的扫描才接受进度事件：kill 失败后的静默重刷、
      // 旧扫描的迟到事件都不应把 scanProgress 置为非 null 污染新扫描 UI
      unlisteners.push(
        await listen<[number, number]>('scan-progress', (e) => {
          if (!scanning.value) return;
          const [done, total] = e.payload;
          scanProgress.value = { done, total };
        }),
      );

      // 启动静默检查发现新版本
      unlisteners.push(
        await listen<UpdateInfo>('update-available', (e) => {
          updateInfo.value = e.payload;
          updateOpen.value = true;
        }),
      );

      // 更新下载进度
      unlisteners.push(
        await listen<[number, number]>('update-progress', (e) => {
          const [done, total] = e.payload;
          updateProgress.value = { done, total };
        }),
      );

      fetchVersion();

      // Tauri 原生拖拽：拿到的是真实文件系统路径
      unlisteners.push(
        await getCurrentWebview().onDragDropEvent((event) => {
          const p = event.payload;
          if (p.type === 'over') {
            dragging.value = true;
          } else if (p.type === 'leave') {
            dragging.value = false;
          } else if (p.type === 'drop') {
            dragging.value = false;
            const path = p.paths[0];
            if (path) scan(path);
          }
        }),
      );

      // 启动参数或已丢失事件中的 pending 路径兜底消费
      await consumePendingFile(false);

      // 启动静默检查的 emit 可能早于 WebView 就绪而丢失，取落地副本兜底
      const pendingUpdate = await takePendingUpdate();
      if (pendingUpdate) {
        updateInfo.value = pendingUpdate;
        updateOpen.value = true;
      }
    })();

    return () => {
      for (const u of unlisteners) u();
      unlisteners = [];
    };
  });
</script>

<div class="flex h-full flex-col gap-4 p-5">
  <!-- 标题栏 -->
  <header class="flex items-center gap-3">
    <div
      class="flex h-9 w-9 items-center justify-center rounded-[10px]"
      style="background: var(--accent-soft); color: var(--accent);"
    >
      <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <rect x="4" y="11" width="16" height="10" rx="2" />
        <path d="M8 11V7a4 4 0 0 1 7.5-2" />
      </svg>
    </div>
    <div class="min-w-0 flex-1">
      <h1 class="text-base font-semibold leading-tight">
        文件占用解除
        {#if appVersion.value}
          <span class="dim ml-1 text-xs font-normal">v{appVersion.value}</span>
        {/if}
      </h1>
      <p class="dim text-xs leading-tight">
        检测并结束锁定文件的进程 · 拖入文件或通过右键菜单启动
      </p>
    </div>
    <div class="flex shrink-0 items-center gap-1.5">
      {#if updateInfo.value}
        <button
          class="rounded-md px-2 py-1 text-xs font-medium transition hover:opacity-80 active:scale-95"
          style="background: var(--ok); color: #fff;"
          onclick={() => (updateOpen.value = true)}
          title="发现新版本 v{updateInfo.value.version}，点击查看"
        >新版本</button>
      {/if}
      <button
        class="rounded-md px-2 py-1 text-xs transition hover:opacity-80 active:scale-95"
        style="background: var(--stroke);"
        onclick={() => checkForUpdates()}
        disabled={updateChecking.value}
      >{updateChecking.value ? '检查中…' : '检查更新'}</button>
      <button
        class="rounded-md px-2 py-1 text-xs transition hover:opacity-80 active:scale-95"
        style="background: var(--stroke);"
        onclick={openDiagnostics}
        title="检测权限、检测引擎、右键菜单等运行前提"
      >自检</button>
    </div>
  </header>

  <!-- 自检面板 -->
  <DiagnosticsPanel />

  <!-- 更新面板 -->
  <UpdatePanel />

  <!-- 拖拽 / 文件选择区 -->
  <DropZone />

  <!-- 错误提示 -->
  {#if scanError.value}
    <div
      class="card flex items-start gap-2 px-4 py-3 text-sm"
      in:fade={{ duration: 150 }}
      style="border-color: var(--danger); color: var(--danger);"
    >
      <svg class="mt-0.5 shrink-0" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round">
        <circle cx="12" cy="12" r="10" />
        <line x1="12" y1="8" x2="12" y2="12" />
        <line x1="12" y1="16" x2="12.01" y2="16" />
      </svg>
      <span class="min-w-0 break-all">{scanError.value}</span>
    </div>
  {/if}

  <!-- 结果区 -->
  <ProcessList />

  <!-- 文件处置区 -->
  <FileActions />
</div>
