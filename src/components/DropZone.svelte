<script lang="ts">
  /**
   * DropZone：拖拽/选文件/清除目标。
   * 拖拽事件在 App 壳监听（需要 webview 生命周期），本组件只负责展示与点击选文件。
   */
  import { fly } from "svelte/transition";
  import { pickFile, toErrorMessage } from "../lib/api";
  import {
    clearFile,
    filePath,
    scan,
    scanError,
  } from "../stores/scan.svelte";
  import { isBusy } from "../stores/app.svelte";
  import { dragging } from "../stores/drag.svelte";

  const fileName = $derived(
    filePath.value
      ? filePath.value.replaceAll("\\", "/").split("/").pop()!
      : "",
  );

  async function chooseFile() {
    if (isBusy()) return;
    try {
      const path = await pickFile();
      if (path) await scan(path);
    } catch (e) {
      // 对话框打不开（如权限被拒）必须让用户看到原因，不能静默
      scanError.value = toErrorMessage(e);
    }
  }
</script>

<div
  class="dropzone flex cursor-pointer flex-col items-center justify-center gap-2 px-4 py-7"
  class:dragging
  role="button"
  tabindex="0"
  onclick={chooseFile}
  onkeydown={(e) => e.key === "Enter" && chooseFile()}
>
  {#if filePath.value}
    <div class="flex w-full items-center gap-3" in:fly={{ y: 8, duration: 200 }}>
      <svg class="shrink-0" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
        <path d="M14 2v6h6" />
      </svg>
      <div class="min-w-0 flex-1 text-left">
        <div class="truncate font-medium">{fileName}</div>
        <div class="dim truncate text-xs">{filePath.value}</div>
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
    <div class="text-sm font-medium">将文件拖入此处，或点击选择</div>
    <div class="dim text-xs">也可以在资源管理器中右键文件 →「解除文件占用」</div>
  {/if}
</div>
