/** 后端 `ProcessInfo` 结构体的前端镜像 */
export interface ProcessInfo {
  pid: number;
  /** 进程可执行文件名，如 WINWORD.EXE */
  process_name: string;
  /** 进程可执行文件完整路径（查询失败时为空） */
  exe_path: string;
  /** exe 版本信息中的文件描述（如 "Microsoft Word"，查询失败时为空） */
  description: string;
  /** Restart Manager 报告的应用显示名（可能为空） */
  app_name: string;
  /** 检测来源：restart_manager / handle_scan / both / directory_scan */
  source: string;
  /** 目录模式下该进程锁定的文件数量（单文件模式恒为 1） */
  locked_files: number;
}
