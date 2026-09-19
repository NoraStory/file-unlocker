<script lang="ts">
  /**
   * DiagnosticsPanel：自检结果 + 日志目录操作。
   */
  import { fade } from "svelte/transition";
  import {
    diagItems,
    diagOpen,
    diagRunning,
    doExportLogs,
    doOpenLogDir,
    logDirPath,
    logNotice,
    openDiagnostics,
  } from "../stores/app.svelte";
</script>

{#if diagOpen.value}
  <div
    class="card flex max-h-64 flex-col overflow-hidden"
    in:fade={{ duration: 150 }}
    style="border-color: var(--stroke-strong);"
  >
    <div class="flex items-center justify-between border-b px-4 py-2.5" style="border-color: var(--stroke);">
      <span class="text-sm font-medium">运行自检</span>
      <div class="flex items-center gap-2">
        <button
          class="rounded-md px-2 py-1 text-xs transition hover:opacity-80 active:scale-95"
          style="background: var(--stroke);"
          disabled={diagRunning.value}
          onclick={openDiagnostics}
        >重新检测</button>
        <button
          class="dim rounded-md px-2 py-1 text-xs transition hover:opacity-80"
          onclick={() => (diagOpen.value = false)}
        >关闭</button>
      </div>
    </div>
    <div class="min-h-0 flex-1 overflow-y-auto p-2.5">
      {#if diagRunning.value}
        <div class="dim py-6 text-center text-xs">检测中…</div>
      {:else}
        <ul class="flex flex-col gap-1.5">
          {#each diagItems.value as item (item.name)}
            <li class="flex items-start gap-2 rounded-md px-2 py-1.5 text-xs" style="background: var(--glass-strong);">
              <span
                class="mt-px shrink-0"
                style="color: {item.status === 'ok' ? 'var(--ok)' : item.status === 'warn' ? '#d48a00' : 'var(--danger)'};"
              >{item.status === "ok" ? "✓" : item.status === "warn" ? "!" : "✕"}</span>
              <div class="min-w-0">
                <div class="font-medium">{item.name}</div>
                <div class="dim break-all">{item.detail}</div>
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
    <div class="flex flex-col gap-1.5 border-t px-4 py-2.5" style="border-color: var(--stroke);">
      {#if logDirPath.value}
        <div class="dim truncate text-[11px]" title={logDirPath.value}>日志目录：{logDirPath.value}</div>
      {/if}
      {#if logNotice.value}
        <div class="break-all text-[11px]" style="color: var(--ok);">{logNotice.value}</div>
      {/if}
      <div class="flex items-center gap-2">
        <button
          class="flex-1 rounded-md px-2 py-1.5 text-xs transition hover:opacity-80 active:scale-95"
          style="background: var(--stroke);"
          onclick={doOpenLogDir}
        >打开日志目录</button>
        <button
          class="flex-1 rounded-md px-2 py-1.5 text-xs transition hover:opacity-80 active:scale-95"
          style="background: var(--stroke);"
          onclick={doExportLogs}
        >导出日志…</button>
      </div>
    </div>
  </div>
{/if}
