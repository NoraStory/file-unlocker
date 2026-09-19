<script lang="ts">
  /**
   * FileActions：删除/重启删除 + 确认条 + 文件夹模式提示。
   * 文件夹模式只允许查占用，删除语义不适用。
   */
  import { fade } from "svelte/transition";
  import {
    confirmingDelete,
    doDelete,
    fileActionBusy,
    fileNotice,
  } from "../stores/app.svelte";
  import { filePath, isDirectory, scanned } from "../stores/scan.svelte";
  import { isBusy } from "../stores/app.svelte";

  const fileName = $derived(
    filePath.value
      ? filePath.value.replaceAll("\\", "/").split("/").pop()!
      : "",
  );
</script>

{#if filePath.value && scanned.value}
  <footer class="flex flex-col gap-2">
    {#if fileNotice.value}
      <div
        class="card px-4 py-2.5 text-sm"
        in:fade={{ duration: 150 }}
        style="color: var(--ok); border-color: var(--ok);"
      >{fileNotice.value}</div>
    {/if}
    {#if isDirectory.value}
      <div class="card px-4 py-2.5 text-xs" in:fade={{ duration: 150 }}>
        📁 文件夹模式：已递归检测内部文件的占用者，删除操作请针对具体文件
      </div>
    {:else if confirmingDelete.value}
      <!-- 删除确认条：替代原生 confirm()，风格统一且需二次点击，防误触 -->
      <div
        class="card flex items-center gap-2 px-4 py-2.5"
        in:fade={{ duration: 120 }}
        style="border-color: var(--danger);"
      >
        <span class="min-w-0 flex-1 text-xs" style="color: var(--danger);">
          确认永久删除「{fileName}」？此操作不可恢复
        </span>
        <button
          class="danger-btn shrink-0 px-3 py-1.5 text-xs font-medium disabled:opacity-50"
          disabled={isBusy()}
          onclick={() => {
            confirmingDelete.value = false;
            doDelete(false);
          }}
        >
          {#if fileActionBusy.value === "delete"}正在删除…{:else}确认删除{/if}
        </button>
        <button
          class="shrink-0 rounded-md px-2.5 py-1.5 text-xs transition hover:opacity-80 active:scale-95"
          style="background: var(--stroke);"
          onclick={() => (confirmingDelete.value = false)}
        >取消</button>
      </div>
    {:else}
      <div class="flex items-center gap-2">
        <button
          class="danger-btn flex-1 px-3 py-2.5 text-sm font-medium disabled:opacity-50 disabled:cursor-not-allowed"
          disabled={isBusy()}
          onclick={() => (confirmingDelete.value = true)}
        >
          删除文件
        </button>
        <button
          class="flex-1 rounded-lg px-3 py-2.5 text-sm font-medium transition hover:opacity-80 active:scale-[0.98] disabled:opacity-50 disabled:cursor-not-allowed"
          style="background: var(--glass); border: 1px solid var(--stroke-strong);"
          disabled={isBusy()}
          onclick={() => doDelete(true)}
        >
          {#if fileActionBusy.value === "delete-reboot"}正在计划…{:else}重启后删除（占用时）{/if}
        </button>
      </div>
    {/if}
  </footer>
{/if}
