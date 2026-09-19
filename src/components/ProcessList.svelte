<script lang="ts">
  /**
   * ProcessList：占用进程卡片 + 终止操作 + 扫描进度/空态。
   */
  import { fade, fly } from "svelte/transition";
  import { killProcess, killProcessTree, toErrorMessage } from "../lib/api";
  import type { ProcessInfo } from "../lib/types";
  import {
    filePath,
    processes,
    refreshCurrent,
    scan,
    scanError,
    scanned,
    scanProgress,
    scanning,
  } from "../stores/scan.svelte";
  import { isBusy, killingPid } from "../stores/app.svelte";

  async function kill(process: ProcessInfo, tree: boolean) {
    if (isBusy() || !filePath.value) return;
    killingPid.value = process.pid;
    scanError.value = null;
    try {
      if (tree) {
        await killProcessTree(
          process.pid,
          process.creation_time,
          process.exe_path,
        );
      } else {
        await killProcess(process.pid, process.creation_time, process.exe_path);
      }
      await scan(filePath.value);
    } catch (e) {
      scanError.value = toErrorMessage(e);
      // 失败后仍刷新一次：进程可能实际已退出（如权限拒绝但进程崩溃）
      await refreshCurrent();
    } finally {
      killingPid.value = null;
    }
  }
</script>

<section class="card flex min-h-0 flex-1 flex-col overflow-hidden">
  <div class="flex items-center justify-between border-b px-4 py-3" style="border-color: var(--stroke);">
    <div class="flex items-center gap-2 text-sm font-medium">
      占用进程
      {#if scanned.value && !scanning.value}
        <span
          class="rounded-full px-2 py-0.5 text-xs"
          style="background: {processes.value.length > 0 ? 'var(--danger)' : 'var(--ok)'}33; color: {processes.value.length > 0 ? 'var(--danger)' : 'var(--ok)'};"
        >{processes.value.length} 个</span>
      {/if}
    </div>
    {#if filePath.value}
      <button
        class="flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs transition hover:opacity-80 active:scale-95 disabled:opacity-50"
        style="background: var(--stroke);"
        disabled={isBusy()}
        onclick={() => filePath.value && scan(filePath.value)}
      >
        <svg class={scanning.value ? "spin" : ""} width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M21 12a9 9 0 1 1-2.64-6.36" />
          <polyline points="21 3 21 9 15 9" />
        </svg>
        重新检测
      </button>
    {/if}
  </div>

  <div class="min-h-0 flex-1 overflow-y-auto p-3">
    {#if scanning.value}
      <div class="flex h-full min-h-32 flex-col items-center justify-center gap-3" in:fade>
        <svg class="spin" width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="2.2" stroke-linecap="round">
          <path d="M21 12a9 9 0 1 1-2.64-6.36" />
          <polyline points="21 3 21 9 15 9" />
        </svg>
        <span class="dim text-sm">正在扫描…</span>
        {#if scanProgress.value}
          <div class="w-56">
            <div
              class="h-1.5 w-full overflow-hidden rounded-full"
              style="background: var(--stroke);"
            >
              <div
                class="h-full rounded-full transition-all duration-150"
                style="width: {Math.round((scanProgress.value.done / Math.max(scanProgress.value.total, 1)) * 100)}%; background: var(--accent);"
              ></div>
            </div>
            <div class="dim mt-1.5 text-center text-xs">
              目录模式：已解析 {scanProgress.value.done} / {scanProgress.value.total} 个候选句柄
            </div>
          </div>
        {:else}
          <span class="dim text-xs">正在通过 Restart Manager 检测…</span>
        {/if}
      </div>
    {:else if !scanned.value}
      <div class="flex h-full min-h-32 items-center justify-center">
        <span class="dim text-sm">选择文件后将自动检测占用情况</span>
      </div>
    {:else if processes.value.length === 0}
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
        {#each processes.value as p (p.pid)}
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
              disabled={isBusy()}
              title="结束该进程及其全部子进程"
              onclick={() => kill(p, true)}
            >结束树</button>
            <button
              class="danger-btn shrink-0 px-3 py-1.5 text-xs font-medium disabled:opacity-50"
              disabled={isBusy()}
              onclick={() => kill(p, false)}
            >
              {#if killingPid.value === p.pid}
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
