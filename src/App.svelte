<script lang="ts">
  import { onMount } from "svelte";
  import { fly, fade } from "svelte/transition";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import {
    getLockingProcesses,
    killProcess,
    killProcessTree,
    deleteFile,
    deleteFileOnReboot,
    takePendingFile,
    pickFile,
    toErrorMessage,
  } from "./lib/api";
  import type { ProcessInfo } from "./lib/types";

  let filePath = $state<string | null>(null);
  let processes = $state<ProcessInfo[]>([]);
  let scanned = $state(false);
  let scanning = $state(false);
  let error = $state<string | null>(null);
  let dragging = $state(false);
  let killingPid = $state<number | null>(null);
  let fileActionBusy = $state<"" | "delete" | "delete-reboot">("");
  let fileNotice = $state<string | null>(null);
  /** 当前目标是文件夹（右键菜单支持文件夹入口） */
  let isDirectory = $state(false);

  /** 全局互斥：任一后端操作进行中时，其它操作按钮全部禁用 */
  let busy = $derived(scanning || killingPid !== null || fileActionBusy !== "");

  /**
   * 扫描代次：防止并发扫描交叠（事件路径不受 busy 按钮禁用约束）。
   * 每次发起 scan 递增，await 返回后若代次已变说明有更新的扫描接手，
   * 当前结果作废不再写入 state——UI 始终显示最后一次请求的结果。
   */
  let scanGeneration = 0;

  /** 目录模式扫描进度：null 表示无进度（单文件模式/未在扫描） */
  let scanProgress = $state<{ done: number; total: number } | null>(null);

  let unlisteners: Array<() => void> = [];

  async function scan(path: string) {
    const gen = ++scanGeneration;
    filePath = path;
    // 文件夹入口：句柄扫描可查目录占用，但删除/重启删除语义不同
    isDirectory = path.endsWith("\\") || path.endsWith("/");
    error = null;
    fileNotice = null;
    scanProgress = null;
    scanning = true;
    try {
      const list = await getLockingProcesses(path);
      if (gen !== scanGeneration) return; // 已被更新的扫描取代
      processes = list;
      scanned = true;
    } catch (e) {
      if (gen !== scanGeneration) return;
      processes = [];
      scanned = true;
      error = toErrorMessage(e);
    } finally {
      // 只有最新一次扫描有权熄灭"扫描中"指示
      if (gen === scanGeneration) {
        scanning = false;
        scanProgress = null;
      }
    }
  }

  async function kill(pid: number, tree: boolean) {
    if (busy || !filePath) return;
    killingPid = pid;
    error = null;
    try {
      if (tree) {
        await killProcessTree(pid);
      } else {
        await killProcess(pid);
      }
      await scan(filePath);
    } catch (e) {
      error = toErrorMessage(e);
      // 失败后仍刷新一次：进程可能实际已退出（如权限拒绝但进程崩溃）
      try {
        await getLockingProcesses(filePath).then((p) => (processes = p));
      } catch {
        /* 刷新失败保持原列表 */
      }
    } finally {
      killingPid = null;
    }
  }

  async function doDelete(onReboot: boolean) {
    if (busy || !filePath) return;
    fileActionBusy = onReboot ? "delete-reboot" : "delete";
    error = null;
    fileNotice = null;
    try {
      if (onReboot) {
        await deleteFileOnReboot(filePath);
        fileNotice = "已计划：下次系统重启时删除该文件";
      } else {
        await deleteFile(filePath);
        fileNotice = "文件已删除";
        clearFile();
      }
    } catch (e) {
      error = toErrorMessage(e);
    } finally {
      fileActionBusy = "";
    }
  }

  function clearFile() {
    filePath = null;
    processes = [];
    scanned = false;
    error = null;
    fileNotice = null;
  }

  async function chooseFile() {
    if (busy) return;
    try {
      const path = await pickFile();
      if (path) await scan(path);
    } catch (e) {
      // 对话框打不开（如权限被拒）必须让用户看到原因，不能静默
      error = toErrorMessage(e);
    }
  }

  onMount(() => {
    (async () => {
      // 右键菜单 / 二次启动传入的新文件路径
      unlisteners.push(
        await listen<string>("new-file", (e) => {
          if (e.payload) scan(e.payload);
        }),
      );

      // 目录模式扫描进度（done, total）
      unlisteners.push(
        await listen<[number, number]>("scan-progress", (e) => {
          const [done, total] = e.payload;
          scanProgress = { done, total };
        }),
      );

      // Tauri 原生拖拽：拿到的是真实文件系统路径
      unlisteners.push(
        await getCurrentWebview().onDragDropEvent((event) => {
          const p = event.payload;
          if (p.type === "over") {
            dragging = true;
          } else if (p.type === "leave") {
            dragging = false;
          } else if (p.type === "drop") {
            dragging = false;
            const path = p.paths[0];
            if (path) scan(path);
          }
        }),
      );

      // 启动参数中带路径（右键菜单首次启动）时立即检测
      const pending = await takePendingFile();
      if (pending && !filePath) scan(pending);
    })();

    return () => {
      for (const u of unlisteners) u();
      unlisteners = [];
    };
  });

  const fileName = $derived(
    filePath ? filePath.replaceAll("\\", "/").split("/").pop()! : "",
  );
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
    <div>
      <h1 class="text-base font-semibold leading-tight">文件占用解除</h1>
      <p class="dim text-xs leading-tight">
        检测并结束锁定文件的进程 · 拖入文件或通过右键菜单启动
      </p>
    </div>
  </header>

  <!-- 拖拽 / 文件选择区 -->
  <div
    class="dropzone flex cursor-pointer flex-col items-center justify-center gap-2 px-4 py-7"
    class:dragging
    role="button"
    tabindex="0"
    onclick={chooseFile}
    onkeydown={(e) => e.key === "Enter" && chooseFile()}
  >
    {#if filePath}
      <div class="flex w-full items-center gap-3" in:fly={{ y: 8, duration: 200 }}>
        <svg class="shrink-0" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
          <path d="M14 2v6h6" />
        </svg>
        <div class="min-w-0 flex-1 text-left">
          <div class="truncate font-medium">{fileName}</div>
          <div class="dim truncate text-xs">{filePath}</div>
        </div>
        <button
          type="button"
          class="dim shrink-0 rounded-md px-2 py-1 text-xs transition hover:opacity-80 active:scale-95"
          style="background: var(--stroke);"
          onclick={(e) => {
            e.stopPropagation();
            clearFile();
          }}
        >清除</button>
      </div>
    {:else}
      <svg width="30" height="30" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
        <polyline points="7 10 12 15 17 10" />
        <line x1="12" y1="15" x2="12" y2="3" />
      </svg>
      <div class="text-sm font-medium">将文件拖到此处，或点击选择</div>
      <div class="dim text-xs">也可以在资源管理器中右键文件 →「解除文件占用」</div>
    {/if}
  </div>

  <!-- 错误提示 -->
  {#if error}
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
      <span class="min-w-0 break-all">{error}</span>
    </div>
  {/if}

  <!-- 结果区 -->
  <section class="card flex min-h-0 flex-1 flex-col overflow-hidden">
    <div class="flex items-center justify-between border-b px-4 py-3" style="border-color: var(--stroke);">
      <div class="flex items-center gap-2 text-sm font-medium">
        占用进程
        {#if scanned && !scanning}
          <span
            class="rounded-full px-2 py-0.5 text-xs"
            style="background: {processes.length > 0 ? 'var(--danger)' : 'var(--ok)'}33; color: {processes.length > 0 ? 'var(--danger)' : 'var(--ok)'};"
          >{processes.length} 个</span>
        {/if}
      </div>
      {#if filePath}
        <button
          class="flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs transition hover:opacity-80 active:scale-95 disabled:opacity-50"
          style="background: var(--stroke);"
          disabled={busy}
          onclick={() => filePath && scan(filePath)}
        >
          <svg class={scanning ? "spin" : ""} width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M21 12a9 9 0 1 1-2.64-6.36" />
            <polyline points="21 3 21 9 15 9" />
          </svg>
          重新检测
        </button>
      {/if}
    </div>

    <div class="min-h-0 flex-1 overflow-y-auto p-3">
      {#if scanning}
        <div class="flex h-full min-h-32 flex-col items-center justify-center gap-3" in:fade>
          <svg class="spin" width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="2.2" stroke-linecap="round">
            <path d="M21 12a9 9 0 1 1-2.64-6.36" />
            <polyline points="21 3 21 9 15 9" />
          </svg>
          <span class="dim text-sm">正在扫描…</span>
          {#if scanProgress}
            <div class="w-56">
              <div
                class="h-1.5 w-full overflow-hidden rounded-full"
                style="background: var(--stroke);"
              >
                <div
                  class="h-full rounded-full transition-all duration-150"
                  style="width: {Math.round((scanProgress.done / Math.max(scanProgress.total, 1)) * 100)}%; background: var(--accent);"
                ></div>
              </div>
              <div class="dim mt-1.5 text-center text-xs">
                目录模式：{scanProgress.done} / {scanProgress.total} 个文件
              </div>
            </div>
          {:else}
            <span class="dim text-xs">正在通过 Restart Manager 检测…</span>
          {/if}
        </div>
      {:else if !scanned}
        <div class="flex h-full min-h-32 items-center justify-center">
          <span class="dim text-sm">选择文件后将自动检测占用情况</span>
        </div>
      {:else if processes.length === 0}
        <div class="pulse-ok flex h-full min-h-32 flex-col items-center justify-center gap-2">
          <svg width="36" height="36" viewBox="0 0 24 24" fill="none" stroke="var(--ok)" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="12" cy="12" r="10" />
            <polyline points="8 12.5 11 15.5 16 9.5" />
          </svg>
          <span class="text-sm font-medium" style="color: var(--ok);">✅ 文件未被占用</span>
          <span class="dim text-xs">当前没有任何进程锁定该文件</span>
        </div>
      {:else}
        <ul class="flex flex-col gap-2">
          {#each processes as p (p.pid)}
            <li
              class="glass-strong flex items-center gap-3 px-3.5 py-3"
              in:fly={{ y: 10, duration: 220 }}
              style="background: var(--glass-strong); border: 1px solid var(--stroke); border-radius: 10px;"
            >
              <div
                class="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg"
                style="background: var(--accent-soft); color: var(--accent);"
              >
                <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <rect x="2" y="3" width="20" height="14" rx="2" />
                  <line x1="8" y1="21" x2="16" y2="21" />
                  <line x1="12" y1="17" x2="12" y2="21" />
                </svg>
              </div>
              <div class="min-w-0 flex-1">
                <div class="truncate text-sm font-medium">
                  {p.description || p.process_name || p.app_name || `PID ${p.pid}`}
                </div>
                <div class="dim truncate text-xs">
                  {#if p.exe_path}
                    <span title={p.exe_path}>{p.exe_path}</span>
                    ·
                  {:else if p.app_name && p.app_name !== p.process_name}
                    {p.app_name} ·
                  {/if}
                  PID {p.pid}
                  {#if p.locked_files > 1}
                    · 占用 {p.locked_files} 个文件
                  {/if}
                  {#if p.source === "handle_scan"}
                    · 句柄扫描
                  {/if}
                </div>
              </div>
              <button
                class="shrink-0 rounded-md px-2 py-1.5 text-xs transition hover:opacity-80 active:scale-95 disabled:opacity-50"
                style="background: var(--stroke);"
                disabled={busy}
                title="结束该进程及其全部子进程"
                onclick={() => kill(p.pid, true)}
              >结束树</button>
              <button
                class="danger-btn shrink-0 px-3 py-1.5 text-xs font-medium disabled:opacity-50"
                disabled={busy}
                onclick={() => kill(p.pid, false)}
              >
                {#if killingPid === p.pid}
                  <svg class="spin inline-block align-[-2px]" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round">
                    <path d="M21 12a9 9 0 1 1-2.64-6.36" />
                    <polyline points="21 3 21 9 15 9" />
                  </svg>
                  正在结束…
                {:else}
                  结束进程释放
                {/if}
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  </section>

  <!-- 文件处置区：文件夹模式只允许查占用，删除语义不适用 -->
  {#if filePath && scanned}
    <footer class="flex flex-col gap-2">
      {#if fileNotice}
        <div
          class="card px-4 py-2.5 text-sm"
          in:fade={{ duration: 150 }}
          style="color: var(--ok); border-color: var(--ok);"
        >{fileNotice}</div>
      {/if}
      {#if isDirectory}
        <div class="card px-4 py-2.5 text-xs" in:fade={{ duration: 150 }}>
          📁 文件夹模式：已递归检测内部文件的占用者，删除操作请针对具体文件
        </div>
      {:else}
      <div class="flex items-center gap-2">
        <button
          class="danger-btn flex-1 px-3 py-2.5 text-sm font-medium disabled:opacity-50 disabled:cursor-not-allowed"
          disabled={busy}
          onclick={() => confirm(`确定永久删除文件？\n${filePath}`) && doDelete(false)}
        >
          {#if fileActionBusy === "delete"}正在删除…{:else}删除文件{/if}
        </button>
        <button
          class="flex-1 rounded-lg px-3 py-2.5 text-sm font-medium transition hover:opacity-80 active:scale-[0.98] disabled:opacity-50 disabled:cursor-not-allowed"
          style="background: var(--glass); border: 1px solid var(--stroke-strong);"
          disabled={busy}
          onclick={() => doDelete(true)}
        >
          {#if fileActionBusy === "delete-reboot"}正在计划…{:else}重启后删除（占用时）{/if}
        </button>
      </div>
      {/if}
    </footer>
  {/if}
</div>
