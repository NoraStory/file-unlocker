# FileUnlocker 架构设计与长期演进规划

> 版本：v1.0 · 日期：2026-09-19 · 状态：Proposed
> 评审人：软件架构师（基于 v0.3.7 代码库静态分析 + 模块依赖图谱）

---

## 1. 现状评估

### 1.1 架构盘点

代码规模约 **4,200 行**（Rust 3,139 + 前端 1,069），四层结构：

| 层 | 模块 | 行数 | 依赖 | 职责 |
|---|---|---|---|---|
| 前端 | App.svelte | 790 | api.ts | 全部 UI 状态 + 视图 + 事件处理 |
| 前端 | api.ts / types.ts | 279 | — | IPC 封装、输入校验 |
| 命令层 | lib.rs | 430 | 全部模块 | 15 个 Tauri 命令、单实例、pending 状态机 |
| 业务层 | lock_detector.rs | 968 | handle_scan, winutil | 双引擎编排、合并去重、进程终止 |
| 业务层 | updater.rs | 343 | — | 双源更新 + SHA256 校验 |
| 业务层 | file_actions.rs | 218 | winutil | 删除 / 重启后删除 |
| 业务层 | diagnostics.rs | 251 | probe_utils | 自检 |
| 平台层 | handle_scan.rs | 420 | winutil | NT 句柄表枚举 |
| 平台层 | winutil.rs | 380 | — | FFI 工具（路径、版本信息、错误码） |
| 平台层 | probe_utils.rs | 120 | — | 诊断探针 |

### 1.2 健康度：良好的部分（应保持）

- **依赖方向干净**：无循环依赖，单向流动 前端 → 命令 → 业务 → 平台
- **FFI 收敛**：unsafe 集中在 lock_detector / handle_scan / winutil，业务逻辑不含裸指针
- **RAII 纪律**：RmSession Drop 保证会话释放；两阶段 NtQuerySystemInformation 带重试上限
- **并发纪律**：重活一律 spawn_blocking，避免 WebView2 UI 线程饿死——这是很多 Tauri 项目踩过的坑，这里已经系统性规避
- **事件丢失防护**：pending state 兜底 emit 不可靠的问题（窗口未就绪时事件丢失）
- **安全意识**：PID 复用防护（创建时间 + exe 路径双重校验）、SHA256 校验、系统目录保护、路径规范化防绕过

### 1.3 技术债清单（按风险排序）

| # | 债务 | 位置 | 风险 | 等级 |
|---|---|---|---|---|
| D1 | App.svelte 790 行单体，扫描/终止/删除/自检/更新五块状态机混在一个组件 | 前端 | 每加一个功能都要动同一个文件，回归风险线性上升 | 高 |
| D2 | lock_detector.rs 968 行，检测编排、RM 会话、进程终止、身份校验四种职责耦合 | 后端 | 改终止逻辑可能碰坏检测逻辑；无法对合并策略做隔离单测 | 高 |
| D3 | 15 个命令全部平铺在 lib.rs，命令层与启动逻辑（单实例、panic hook、更新静默检查）混在一起 | 后端 | lib.rs 会持续膨胀，setup 流程难以测试 | 中 |
| D4 | 错误模型是 `Result<T, String>`，错误码靠中文字符串传递 | 全栈 | 前端无法编程式区分错误类型（如"权限不足"vs"文件不存在"），国际化无抓手 | 中 |
| D5 | 检测引擎（RM / 句柄扫描）与编排逻辑无 trait 边界 | 后端 | 未来加第三引擎（如 ETW、minifilter 驱动）必须改核心代码 | 中 |
| D6 | 无 CI、无集成测试矩阵（README 提到 cargo test --lib，但未见 CI 配置） | 工程 | 句柄扫描强依赖 Windows 版本行为，25H2 的 RM 异常就是先例 | 中 |
| D7 | update.json 清单无签名，仅 SHA256（校验的是清单自带的哈希） | 安全 | 清单源被劫持时 SHA256 形同虚设；当前靠 HTTPS 单点防护 | 低（单机工具可接受） |

### 1.4 关键判断

这是一个**健康的模块化单体**，规模（4K 行）和形态（单机桌面工具）决定了它**不需要微服务、不需要插件系统、不需要数据库**。当前最大的架构风险不是"架构不够先进"，而是 **D1/D2 两个热点文件会随着功能增长把干净的结构拖垮**。因此规划的主线是：**保持单体，切割热点，预留引擎扩展点**。

---

## 2. 架构决策记录

### ADR-001：保持 Tauri 模块化单体，不引入插件/微服务架构

**状态**：Accepted

**背景**：需要为后续功能（更多检测引擎、批量操作、国际化）预留扩展性，存在"上插件系统/拆服务"的选项。

**决策**：保持单一 Tauri 应用 + Rust 内部分层。扩展性通过 trait 边界（ADR-003）而非进程/插件隔离实现。

**后果**：
- ✅ 保留 3.3MB 单文件、免安装的核心卖点；构建、发布、更新链路不变
- ✅ 无 IPC/序列化边界开销，检测延迟保持微秒级
- ❌ 放弃了"第三方开发者写检测引擎插件"的可能性——**这是可接受的放弃**：该场景没有真实需求信号
- ❌ 若未来出现多产品线需求，需要重新评估（触发条件见 §6.4）

### ADR-002：错误模型从 String 迁移到结构化错误码

**状态**：Proposed

**背景**：D4。当前 `Result<T, String>` 使前端只能展示文本，无法按错误类型分支处理。

**决策**：定义 `#[derive(Serialize)] enum AppError { Code(ErrorCode), Message(String) }`，ErrorCode 用稳定字符串常量（如 `E_PERM_DENIED`、`E_PATH_INVALID`、`E_RM_SESSION_FAIL`）。IPC 边界序列化为 `{ code, message }`。前端 `api.ts` 提供类型化解析。

**后果**：
- ✅ 前端可按 code 决定 UI 行为（权限错误 → 引导提权；路径错误 → 提示重新拖入）
- ✅ 国际化成为可能（前端按 code 映射文案，后端 message 仅作日志）
- ❌ 迁移期需双轨兼容（新旧命令并存或一次性切换，代码量小可一次性切换）

### ADR-003：检测引擎抽象为 `LockEngine` trait

**状态**：Proposed

**背景**：D5。双引擎已是既定事实，未来可能新增引擎（ETW 文件事件、minifilter 驱动、网络盘远端查询）。

**决策**：

```rust
trait LockEngine: Send + Sync {
    fn name(&self) -> &'static str;
    /// 返回锁定 path 的进程集合；失败返回 Err，不 panic
    fn scan(&self, target: &ScanTarget, ctx: &ScanContext) -> Result<Vec<RawHit>, EngineError>;
}
```

- `core/detect.rs` 编排器持有 `Vec<Box<dyn LockEngine>>`，并行/串行调度由编排器决定
- 合并去重、排序、来源标记（restart_manager / handle_scan / both）全部收进编排器
- RM 引擎的会话 RAII、句柄引擎的缓冲重试逻辑各自内聚

**后果**：
- ✅ 新引擎 = 新文件 + 注册一行，核心编排零改动
- ✅ 合并策略可用 mock 引擎做纯逻辑单测（当前 968 行里最难测的部分）
- ❌ 引入一层间接；对只有两个引擎的现状略有过度设计——**用"未来 12 个月内大概率加引擎"来对冲这个成本**

### ADR-004：前端采用 Svelte 5 runes 组件化拆分，不引入状态库

**状态**：Proposed

**背景**：D1。790 行单组件，但状态间耦合其实不高（扫描、更新、自检三块基本独立）。

**决策**：按功能域拆为 5 个组件 + 3 个 store（`.svelte.ts` runes 模块），**不引入** Redux 类状态库。组件树：

```
App.svelte（壳：布局 + 全局 busy 派生）
├── DropZone.svelte          拖拽/选文件/启动参数消费
├── ProcessList.svelte       进程卡片 + 终止操作 + 扫描进度
├── FileActions.svelte       删除/重启删除 + 确认条
├── DiagnosticsPanel.svelte  自检
└── UpdatePanel.svelte       更新检查/下载/进度
stores/
├── scan.svelte.ts           filePath/processes/scanning/scanGeneration
├── update.svelte.ts         updateInfo/progress/error
└── app.svelte.ts            version/busy/diag
```

**后果**：
- ✅ 每个功能域独立演进，回归半径从"整个 App"缩小到单组件
- ✅ scanGeneration 并发防护逻辑收进 store，一处实现处处生效
- ❌ 拆分本身是一次纯重构提交，需要回归测试护航（见 §4）

---

## 3. 目标架构

### 3.1 分层与依赖规则

**依赖规则（逐步用编译期手段强制）**：

1. 上层可依赖下层，禁止反向
2. `core` 不得依赖 `tauri`、`windows` crate——保证纯逻辑可单测
3. `platform` 是唯一允许 `unsafe` 的层（`#![forbid(unsafe_code)]` 加在 core/commands）
4. IPC 类型定义在 `core::model`，前端 `types.ts` 手工镜像并加注释锚点（规模小，不值得上代码生成）

### 3.2 目录结构（目标态）

```
src-tauri/
├── crates/                  （远期：结构稳定后再升 workspace）
│   ├── fu-core/             领域核心 + 引擎 trait（无 unsafe、无 tauri）
│   ├── fu-engines/          RM + 句柄扫描两个实现
│   └── fu-platform/         FFI 工具（唯一 unsafe 层）
└── src/
    ├── main.rs
    ├── lib.rs               仅 Builder 装配 + 单实例 + panic hook
    ├── commands/            scan.rs process.rs file_ops.rs update.rs app.rs
    ├── core/                model.rs detect.rs kill.rs
    ├── engines/             mod.rs(trait) restart_manager.rs handle_scan.rs
    ├── platform/            winutil.rs process_info.rs path_util.rs
    └── setup/               启动参数、静默更新检查
```

> 注：workspace 拆分是"远期目标态"，非第一步。**先目录内分层（mod 目录），结构稳定后再升 workspace**——避免过早增加构建复杂度。

---

## 4. 演进路线图

### Phase 0 — 止血与护栏（1~2 周，可与功能开发并行）

| 事项 | 产出 | 对应债务 |
|---|---|---|
| 建立 GitHub Actions CI：`cargo test --lib` + `svelte-check` + `cargo clippy -- -D warnings`，Windows runner | 每次提交自动回归 | D6 |
| 补齐合并去重、路径规范化、版本比较的单元测试（这三块是纯逻辑，当前最易测且最易回归） | 测试覆盖核心策略 | D6 |
| ADR-002 错误模型落地：定义 ErrorCode 枚举，新命令一律用新模型 | 结构化错误 | D4 |

### Phase 1 — 后端热点切割（2~3 周）

1. `lock_detector.rs` 拆为 `core/detect.rs`（编排）+ `engines/restart_manager.rs` + `engines/handle_scan.rs` + `core/kill.rs`（终止）
2. `lib.rs` 拆出 `commands/` 五个文件，lib.rs 只留 Builder 装配
3. `#![forbid(unsafe_code)]` 标注非平台层模块，把 FFI 纪律固化为编译期约束
4. **验收标准**：`cargo test --lib` 全绿；对外 IPC 行为零变化（前端不改一行）

### Phase 2 — 前端组件化（2 周）

1. 按 ADR-004 拆分 App.svelte → 5 组件 + 3 store
2. `api.ts` 按域拆为 `api/` 目录（scan/process/file/system/update）
3. **验收标准**：`svelte-check` 零错误；手工回归清单（拖入/右键/目录模式/删除/更新/自检六条路径）通过

### Phase 3 — 能力扩展（按需求节奏，每项独立可交付）

| 能力 | 依赖的架构准备 | 说明 |
|---|---|---|
| 目录模式增强（递归深度、按进程聚合视图） | Phase 1 的 core/detect | 纯编排层改动 |
| 扫描历史 / 收藏（本地 JSON 存储） | 无 | 单机文件存储即可，不引数据库 |
| 第三引擎：ETW 实时监控（"谁即将锁这个文件"） | ADR-003 trait | 新引擎文件 + 注册 |
| 国际化（i18n） | ADR-002 错误码 | 前端文案层改造 |
| 安装版右键菜单整合进安装器 | 无 | 打包层改动 |

### Phase 4 — 长期方向（12 个月+，触发式启动）

- **驱动级检测**（minifilter）：仅当用户报告"句柄扫描也查不到"的案例达到一定量级才启动——它带来签名/WHQL/兼容性成本，没有需求信号不做
- **多产品线**：若出现第二个工具想复用更新/自检框架，再把 `platform` + updater 抽为共享 crate（触发条件，不是计划）

---

## 5. 风险与对策

| 风险 | 概率 | 影响 | 对策 |
|---|---|---|---|
| 重构引入行为回归（尤其句柄扫描的 Windows 版本差异） | 中 | 高 | Phase 0 先建 CI + 核心策略单测，重构提交保持原子化、每步可回滚 |
| 拆分期间与功能开发冲突 | 中 | 中 | Phase 1/2 各占一个独立分支，功能开发基于 main，重构完成后一次性合并 |
| Windows 更新破坏 NT API 假设（25H2 RM 异常已是先例） | 高（长期必然） | 中 | 引擎 trait 化后，单引擎失效可降级运行；diagnostics 增加引擎健康项 |
| 单人项目过度工程化 | 低 | 中 | 本规划所有拆分都以"文件数 ≤ 15"为度，不引入代码生成、DI 框架等重型手段 |

---

## 6. 架构回退触发器（何时需要重新设计）

| 信号 | 动作 |
|---|---|
| commands/ 频繁出现多人修改冲突 | 考虑按 bounded context 拆 workspace crate |
| 出现服务端需求（如企业集中管控） | 新增独立服务，桌面端保持瘦客户端，勿往本仓库塞服务端代码 |
| 引擎数量 > 3 且调度策略复杂化 | 引入引擎注册表 + 配置化调度 |

---

## 7. 一页纸结论

1. **不换架构，只切热点**：模块化单体是这个规模下的正确答案，钱要花在 App.svelte 和 lock_detector.rs 两个热点上
2. **先护栏后动刀**：CI + 核心策略单测先行，重构才有安全网
3. **trait 是唯一的扩展点投资**：LockEngine trait 对冲"新引擎"需求，其余一律等真实需求信号
4. **错误码是国际化和前端体验的前置条件**：便宜且收益持续，尽早做
5. **保持现有纪律**：spawn_blocking 约定、RAII、pending state 兜底、PID 复用防护——这些是项目最值钱的部分，重构时原样保留
