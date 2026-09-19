<script lang="ts">
  /**
   * UpdatePanel：更新检查结果展示 + 下载进度 + 安装。
   */
  import { fade } from "svelte/transition";
  import { appVersion } from "../stores/app.svelte";
  import {
    doDownloadUpdate,
    updateDownloading,
    updateError,
    updateInfo,
    updateInstaller,
    updateOpen,
    updateProgress,
  } from "../stores/update.svelte";
</script>

{#if updateOpen.value}
  <div
    class="card flex flex-col overflow-hidden"
    in:fade={{ duration: 150 }}
    style="border-color: var(--stroke-strong);"
  >
    <div class="flex items-center justify-between border-b px-4 py-2.5" style="border-color: var(--stroke);">
      <span class="text-sm font-medium">软件更新</span>
      <button
        class="dim rounded-md px-2 py-1 text-xs transition hover:opacity-80"
        onclick={() => (updateOpen.value = false)}
      >关闭</button>
    </div>
    <div class="flex flex-col gap-2 p-4">
      {#if updateInfo.value}
        <div class="text-sm">
          发现新版本 <span class="font-semibold">v{updateInfo.value.version}</span>
          <span class="dim text-xs">（来源：{updateInfo.value.source}，当前 v{appVersion.value || "?"}）</span>
        </div>
        {#if updateInfo.value.notes}
          <div class="dim max-h-24 overflow-y-auto whitespace-pre-wrap rounded-md p-2 text-xs" style="background: var(--glass-strong);">{updateInfo.value.notes}</div>
        {/if}
        {#if updateProgress.value}
          <div class="w-full">
            <div class="h-1.5 w-full overflow-hidden rounded-full" style="background: var(--stroke);">
              <div
                class="h-full rounded-full transition-all duration-150"
                style="width: {updateProgress.value.total > 0 ? Math.round((updateProgress.value.done / updateProgress.value.total) * 100) : 20}%; background: var(--accent);"
              ></div>
            </div>
            <div class="dim mt-1 text-center text-xs">
              下载中 {updateProgress.value.total > 0 ? `${Math.round(updateProgress.value.done / 1048576)} / ${Math.round(updateProgress.value.total / 1048576)} MB` : `${Math.round(updateProgress.value.done / 1048576)} MB`}
            </div>
          </div>
        {:else}
          <button
            class="accent-btn px-3 py-2 text-sm font-medium disabled:opacity-50"
            disabled={updateDownloading.value || !updateInstaller()}
            onclick={doDownloadUpdate}
          >{updateDownloading.value ? "准备下载…" : "下载并安装"}</button>
          {#if !updateInstaller()}
            <div class="text-xs" style="color: var(--danger);">无安装包资产，无法自动安装</div>
          {/if}
        {/if}
      {:else}
        <div class="dim text-sm">{updateError.value || "正在查询…"}</div>
      {/if}
      {#if updateInfo.value && updateError.value}
        <div class="text-xs" style="color: var(--danger);">{updateError.value}</div>
      {/if}
    </div>
  </div>
{/if}
