/**
 * API 门面：按域拆分后的统一出口。
 * 调用方仍可 `from "../lib/api"` 导入全部能力（IPC 行为零变化，ADR-004）。
 */
export * from "./errors";
export * from "./scan";
export * from "./process";
export * from "./file";
export * from "./system";
export * from "./update";
