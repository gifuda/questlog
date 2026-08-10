# QuestLog 受控 AI 任务执行闭环方案

> **现行基线提示（2026-08-11）**：主体功能已经实施，本文保留为原始架构与施工历史；最终收口、执行器边界和放行标准以 `docs/QuestLog受控AI任务执行闭环_最终收口方案_20260811.md` 为准。尤其是 Codex 已调整为“只通过 MCP 推送方案，不承担自动执行”，本文中 Codex Executor / Codex canary 的旧设计不再作为现行要求。

> 日期：2026-08-10
>
> 状态：可施工方案，尚未实施
>
> 目标仓库：QuestLog（桌面清单贴）
>
> 本文边界：定义产品与工程施工包，不代表代码、部署或真实运行已完成

## 1. 结论摘要

QuestLog 应在现有开源产品上迭代，不另建第二个任务产品。

产品仍保留“待办清单、方案追踪”两个一级用户模块；CLI Runner 作为“方案追踪”的执行能力，显示在 Epic/Step 下的“执行记录”区域，不在第一阶段独立为一级模块。

目标闭环：

```text
人在 Codex / Claude Code / ZCode 等对话中形成方案与执行包
  → QuestLog MCP 结构化登记、校验、封包
  → QuestLog 方案追踪显示依赖、进度、授权和阻塞
  → 本地 Runner 领取已放行 Step，调用 Codex/ZCode CLI 执行
  → Runner 独立读取 Git diff/commit、测试和 verifier 证据
  → 通过则自动释放下一 Step
  → 未能处理、需要授权或方向不确定时才回到人
  → 人在任一 MCP 对话端补充决策，QuestLog 继续推进
```

核心价值不是“让 AI 永远自主运行”，而是把重复的复制、粘贴、查进度、开新会话、转交上下文、提醒下一步等低价值操作交给系统；人的注意力只用于目标调整、授权和重要决策。

## 2. 第一性原理

### 2.1 三类真相

| 真相 | 权威载体 | 说明 |
|---|---|---|
| 人批准了什么 | Epic、Step、任务包、授权记录 | 目标、边界、依赖、验收条件 |
| 代码实际变成什么 | Git 工作树、diff、commit | 不采信模型“已完成”的自然语言声明 |
| 是否真正完成 | verifier、测试、目标产物、观察记录 | commit 是必要证据，但不是生产完成的充分证据 |

### 2.2 两个自动决策

系统只需要自动回答两个问题：

1. 当前 Step 是否允许启动？
2. 当前 Step 的证据是否足以释放下一个 Step？

不能确定时一律停止，不让模型自行改变目标或扩大权限。

### 2.3 人机边界

自动执行范围：

- 读取项目和方案；
- 在隔离 worktree/branch 中编辑允许路径；
- 运行预先声明的测试和 verifier；
- 保存运行记录；
- 在满足证据门槛后推进阶段；
- 在批准策略允许时生成候选 commit。

默认需要人工授权：

- 改变已封包任务的目标、范围或验收条件；
- 修改前置依赖或跳过失败依赖；
- 合并主分支、push、创建 PR；
- 部署、重启服务、切换生产路径；
- 访问任务包未声明的目录或网络；
- 执行不可逆外部动作。

## 3. 当前代码事实与缺口

当前 QuestLog 已具备：

- MCP 工具：Todo、Epic、Step 的创建、查询和更新；
- `.epics.json`：方案、子方案、步骤、依赖、验收文本；
- 五阶段进度：`pending → designed → coded → verified → observed`；
- Epic 依赖自动 `blocked` 和释放；
- Step 可行动项计算；
- Tauri watcher：监听 `.todos.json`、`.epics.json` 并刷新桌面 UI；
- Stop Hook：发现会话声称完成但未登记时提醒。

当前缺口：

- 没有标准任务包和封包版本；
- 没有执行队列、租约、CLI Runner；
- 没有 Step 与 worktree/branch/base SHA/commit 的绑定；
- 没有统一的执行证据和失败状态；
- 没有“需要人工判断”的结构化回流；
- MCP Node 端和 Tauri Rust 端均可直接写 JSON，新增 Runner 后会出现第三写入方；
- 现有 `acceptance` 是人类可读自由文本，不能直接作为机器验收契约；
- 现有 Step 依赖要求前序达到 `observed`，不适合所有工程步骤。多数施工链应在前序 `verified` 后释放，只有稳定性依赖才要求 `observed`。

## 4. 产品信息架构

### 4.1 一级模块保持不变

```text
QuestLog
├── 待办清单
└── 方案追踪
    └── Epic
        └── Step
            ├── 任务包
            ├── 执行记录 Runs
            ├── Git / verifier 证据
            └── 授权 / 阻塞 / 人工反馈
```

Runner 在架构上是 QuestLog Core 的执行能力，在界面上从属于方案追踪。第一阶段不新增独立“CLI 控制台”一级导航，避免产生第二套任务状态。

未来只有当产品支持“不属于任何方案的长期定时作业”时，才考虑增加独立的“自动化作业”一级模块。

### 4.2 运行视图

方案卡增加：

- 自动执行开关：关闭 / 只读 / worktree 写入；
- 当前可执行 Step；
- 当前执行器和运行时间；
- branch、base SHA、candidate commit；
- 测试和 verifier 结论；
- 阻塞原因和需要人回答的问题；
- 暂停、取消、重试、转人工按钮。

全局可以增加一个由 Step 派生的“需要关注”收件箱，但它不是新的任务真相源，只聚合：

- `awaiting_approval`；
- `blocked`；
- 超过重试上限；
- Git 漂移；
- verifier 不通过。

## 5. 最小运行架构

```text
Codex / Claude Code / ZCode 对话
            │ MCP
            ▼
QuestLog MCP Server
  - 创建 Epic/Step
  - 生成并封包 TaskPackage
  - 提交人工决策
            │ 受控写入
            ▼
Plan Store + Local Runtime Store
            ▲
            │ 领取/回写
Tauri Runner（托盘进程内）
  - 选择 ready Step
  - 分配 worktree/branch
  - 启动 Codex/ZCode CLI
  - 解析 CLI 事件
  - 检查 Git 和 verifier
            │
            ▼
Git repository / tests / artifacts
```

第一阶段 Runner 放在现有 Tauri 托盘进程内：应用已经常驻、已有项目注册表和文件 watcher，可以避免再引入一个用户可见服务。QuestLog 未运行时不启动新任务；以后需要完全无界面运行时，再把相同 Runner Core 打包成 launchd sidecar。

MCP 请求不能同步等待长时间 CLI 任务。`enqueue` 只负责写入队列并立即返回，Runner 异步领取。

## 6. 数据与单写边界

### 6.1 Plan Store

`.epics.json` 继续保存可携带、可读、可版本化的方案事实：

- Epic、Step、依赖；
- 人类验收说明；
- TaskPackage 的当前封包 ID、revision 和 hash；
- 执行策略摘要；
- 最终阶段。

完整 TaskPackage 不写入 `.epics.json`：其中包含本机绝对路径、CLI session 和执行策略，只保存在 Runtime Store。Plan Store 仅保存可移植的 package ID、revision、hash 和人类可读摘要，避免把个人路径或运行细节提交到 Git。

### 6.2 Runtime Store

运行控制状态不写入项目目录的 `.epics.json`，避免高频运行事件与方案编辑互相覆盖。放入操作系统本地应用数据目录，例如：

```text
QuestLog local data/
└── runtime/
    ├── executions.json
    ├── events.jsonl
    ├── locks/
    └── outputs/<execution_id>/
```

保存：

- queue/running/blocked/failed/passed；
- 执行租约和心跳；
- CLI session/thread ID；
- stdout/stderr/JSONL 路径；
- worktree、branch、base SHA、candidate SHA；
- 测试和 verifier 证据；
- retry、timeout、取消原因；
- decision request 和人工回复。

### 6.3 写入规则

施工前置 P0：

- 非 ENOENT 读取错误必须 fail-closed；
- JSON 损坏不得回退为空集合后继续写；
- Node MCP、Tauri UI、Runner 使用同一 revision/CAS 协议；
- 写入必须有跨进程文件锁；
- 原子 rename 只解决半写，不解决 last-writer-wins；
- 冲突时重读、重新应用 patch，不能静默覆盖。

## 7. 标准 TaskPackage 契约

### 7.1 原则

对话模型不直接上传自由格式 Markdown 作为执行输入。模型调用结构化 MCP 工具填写意图，QuestLog 服务端根据 Schema、当前 Git 和方案状态生成 TaskPackage。

任务包只有经过 `seal_task_package` 后才能进入 `ready`。封包后内容不可修改；修改任一字段必须产生新 revision 和新 hash，并让未开始的旧包失效、运行中的旧包暂停等待裁决。

### 7.2 最小 Schema

```json
{
  "schema_version": 1,
  "package_id": "pkg_uuid",
  "epic_id": "epic_uuid",
  "step_id": "step_uuid",
  "plan_revision": 3,
  "source_session": "optional-session-id",
  "project_path": "/canonical/project/path",
  "repo_root": "/canonical/git/root",
  "base_sha": "full-git-sha",
  "objective": "单一、可验证的目标",
  "context_refs": ["doc/path"],
  "scope": {
    "allowed_paths": ["src/**", "test/**"],
    "forbidden_paths": ["secrets/**"],
    "max_changed_files": 6
  },
  "preconditions": [
    {"type": "step_stage", "step_id": "S1", "at_least": "verified"},
    {"type": "git_clean", "value": true}
  ],
  "executor": {
    "preferred": "codex",
    "fallbacks": ["zcode"],
    "sandbox": "workspace-write",
    "timeout_seconds": 3600,
    "max_attempts": 2
  },
  "acceptance": [
    {"type": "command", "command_id": "test:mcp"},
    {"type": "git_diff_scope"},
    {"type": "verifier", "verifier_id": "default-code-review"}
  ],
  "authority": {
    "auto_edit_worktree": true,
    "auto_commit_candidate": true,
    "auto_merge": false,
    "auto_push": false,
    "auto_deploy": false
  },
  "stop_conditions": [
    "scope_expanded",
    "base_sha_drift",
    "unapproved_external_effect",
    "repeated_failure"
  ],
  "rollback": {
    "strategy": "preserve_worktree_and_stop"
  },
  "sealed_at": "RFC3339",
  "package_hash": "sha256"
}
```

### 7.3 命令安全

TaskPackage 不直接接受任意 shell 字符串作为自动验收命令。`acceptance.command_id` 从项目级受信命令注册表解析，例如：

```text
test:mcp      → npm test（mcp-server）
build:mcp     → npm run build（mcp-server）
test:desktop  → cargo test（desktop-app/src-tauri）
build:web     → npm run build（desktop-app）
```

新增或修改命令注册表属于人工审批动作。

## 8. 对话端如何保证每次任务包合规

### 8.1 服务端硬门禁是主控制点

新增 MCP 工具：

```text
prepare_task_package(step_id, structured_fields)
validate_task_package(package_id)
seal_task_package(package_id)
enqueue_step(step_id)
list_decision_requests()
resolve_decision(request_id, answer)
```

规则：

- `prepare` 可返回缺失字段清单；
- `validate` 读取真实项目路径、Git、依赖和策略，不信任模型传入的 base SHA；
- `seal` 仅在所有必填字段和机器检查通过后成功；
- `enqueue` 只接受当前 Step 的有效 sealed package；
- 任何 MCP 客户端都走同一服务端校验，保证 Codex、Claude Code、ZCode 一致；
- 自然语言 prompt、README、AGENTS 只能引导，不能代替硬门禁。

### 8.2 Hook 是体验兜底

Codex/Claude Code 等客户端可分别接入其 Hook 能力：

- SessionStart：注入当前项目、活动 Epic、未解决 blocker；
- PreToolUse：拦截未封包的 `enqueue_step`、非法项目路径和越权状态变更；
- Stop：用户明确要求“生成可执行方案”但会话未产生 sealed package 时，提醒补齐；
- PostToolUse：记录 MCP 工具成功/失败，避免只根据对话关键词猜测；
- commit-msg：候选 commit 必须关联 Epic、Step、Package revision。

Hook 可被不同客户端关闭或不支持，因此只承担提前反馈和减少返工，不能作为系统权威门禁。

## 9. Step 依赖和阶段释放

### 9.1 计划阶段与运行状态分离

Step 阶段继续保留：

```text
pending → designed → coded → verified → observed
```

运行状态单独保存：

```text
draft_package
→ ready
→ queued
→ running
→ candidate
→ passed

旁路：awaiting_approval / blocked / failed / cancelled / superseded
```

映射规则：

- sealed package 完成：Step 可推进至 `designed`；
- 产生符合范围的候选 commit：Step 可推进至 `coded`；
- 针对 exact commit 的 acceptance 全通过：Step 推进至 `verified`；
- 观察条件满足或人工确认：Step 推进至 `observed`；
- CLI exit 0 但无预期 diff/commit/产物：不推进；
- 模型声明“完成”但机器证据不足：不推进。

### 9.2 依赖门槛

将 Step 依赖从字符串扩展为带门槛的对象：

```json
{
  "step_id": "S1",
  "required_stage": "verified"
}
```

兼容规则：旧字符串依赖按 `observed` 解释，避免升级后提前释放既有方案。新工程方案默认依赖门槛为 `verified`；明确依赖长期稳定性的步骤使用 `observed`。

## 10. Runner 行为

### 10.1 调度条件

Runner 仅领取同时满足以下条件的 Step：

- Epic 为 `active`；
- Step 有当前有效 sealed package；
- 所有依赖达到声明门槛；
- 自动执行开关允许；
- 没有未决人工授权；
- project/repo 路径已 canonicalize 且在注册表 allowlist；
- 没有同 project/worktree 的有效租约；
- base SHA 与封包时一致，或已按策略重新封包；
- 执行器能力探测通过。

默认每个项目同一时间只运行一个写任务；只读任务可并行。已有 `max_active` 字段可用于上限，但不能突破 worktree 和写租约约束。

### 10.2 Git 隔离

写任务默认：

1. 从 TaskPackage `base_sha` 创建专用 worktree/branch；
2. 确认初始工作树干净；
3. CLI 只获得该 worktree 的工作目录；
4. 执行结束后 Runner 检查 changed paths、文件数和禁止目录；
5. 运行 diff 范围检查和提交前安全检查；
6. 根据策略生成候选 commit；
7. 对 exact candidate commit 运行或确认 acceptance，失败则保留候选但不释放下游；
8. merge、push、deploy 不自动执行；
9. 失败时保留 worktree、diff、日志，不自动清理用户证据。

建议 commit trailer：

```text
QuestLog-Epic: <epic_id>
QuestLog-Step: <step_id>
QuestLog-Package: <package_id>@<plan_revision>
QuestLog-Execution: <execution_id>
```

### 10.3 Codex 执行器

第一执行器使用 Codex 非交互模式：

- `codex exec --json -C <worktree> --sandbox workspace-write <task>`；
- 解析 JSONL，记录 `thread.started`、turn 完成/失败、命令和文件变更事件；
- 保存 thread/session ID；
- 需要同一次执行续接时使用 `codex exec resume <session_id>`；
- 默认不使用 `danger-full-access` 或跳过审批/沙箱参数；
- CLI 事件用于观测，不替代 Git/verifier 最终裁决。

官方能力依据：

- Codex 非交互模式：<https://learn.chatgpt.com/docs/non-interactive-mode.md>
- Codex CLI：<https://learn.chatgpt.com/docs/developer-commands.md?surface=cli>
- Codex Hook：<https://learn.chatgpt.com/docs/hooks.md>

### 10.4 ZCode 与其他执行器

执行器实现统一接口：

```text
probe()                  能力与版本探测
start(task, cwd)         启动并返回 execution/session 标识
stream(execution_id)     读取结构化事件或文本
resume(execution_id)     可选；不支持则重新封包续跑
cancel(execution_id)     终止子进程
```

ZCode 精确命令行必须在目标机通过 `--help` 和真实 dry-run 探测后写入 adapter，不根据历史命令或文档猜测。无原生 resume 时，新运行读取同一 Step、worktree、Git 和事件记录继续，不要求人复制上下文。

## 11. 人工反馈与异常回流

Runner 遇到不能处理或不应自行决定的事项时，生成结构化 DecisionRequest：

```json
{
  "request_id": "decision_uuid",
  "epic_id": "E42",
  "step_id": "S3",
  "execution_id": "run_uuid",
  "kind": "approval_or_adjudication",
  "question": "需要人决定的唯一问题",
  "options": ["A", "B"],
  "evidence_refs": ["log", "diff", "commit"],
  "blocking": true
}
```

反馈入口：

- QuestLog UI 直接回答；
- 任意已接入 QuestLog MCP 的对话中说“处理待决问题”，调用 `list_decision_requests`；
- UI 点击“在 Codex/ZCode 中讨论”，Runner 带完整任务上下文启动或续接会话；
- `resolve_decision` 记录回答、回答人、时间和适用 revision，然后释放或重新封包。

第一阶段不承诺向任意已经打开的 GUI 对话强行注入消息。系统保证“无需复制上下文”，不保证“必须回到某个未受控的原窗口”。由 Runner 创建并记录的 Codex CLI session 可以原位 resume。

## 12. 失败、重试和熔断

自动重试只适用于：

- 短暂 CLI/网络失败；
- 无代码变更的启动失败；
- 明确可恢复的测试环境问题。

以下情况不得自动沿原方向连续重试：

- 代码已经产生但 verifier 失败；
- 任务范围扩大；
- base SHA 或方案 revision 漂移；
- 需要新权限；
- 两次尝试给出不同根因；
- 同一失败达到 `max_attempts`；
- 发现用户或其他会话的未归属变更。

熔断后状态为 `blocked`，保留 worktree 和证据，等待人决定“修订任务包、改执行器、转人工或取消”。

## 13. 分批施工任务包

### W0：存储安全与单写协议

**目的**：在新增第三写入方前，消除 JSON 误覆盖和损坏回退风险。

**候选文件**：

- `mcp-server/src/store.ts`
- `mcp-server/src/epic-store.ts`
- `desktop-app/src-tauri/src/store.rs`
- `desktop-app/src-tauri/src/epic_store.rs`
- 对应 Node/Rust 测试

**施工**：

- 统一 ENOENT 与其他错误语义；
- JSON 损坏 fail-closed，并保留恢复入口；
- 增加 `revision`；
- 增加跨进程锁、CAS 和冲突重试；
- Node/Rust 写入协议一致；
- 增加并发写、权限错误、损坏 JSON、冲突恢复测试。

**验收**：

- 100 轮并发更新不丢字段；
- 权限错误和 JSON 损坏时后续写入被拒绝；
- CAS 冲突可重试且不会覆盖新版本；
- 既有 `.todos.json/.epics.json` 无损迁移。

**回滚**：保留向后读取能力；关闭新写入器，恢复旧应用版本仍可读取旧字段。

### W1：TaskPackage Schema 与 MCP 硬门禁

**依赖**：W0。

**候选文件**：

- 新增 `mcp-server/src/task-package.ts`
- `mcp-server/src/index.ts`
- `mcp-server/src/epic-store.ts`
- 新增 TaskPackage 测试

**施工**：

- 实现 prepare/validate/seal/enqueue；
- 服务端解析 canonical project/repo；
- 从 Git 读取 base SHA；
- 生成 package hash 和 revision；
- package 变更使旧包 superseded；
- 缺字段返回结构化修复清单。

**验收**：

- 任意客户端都无法 enqueue 未封包任务；
- 模型伪造路径、base SHA 或权限字段被拒绝；
- sealed package 修改后 hash 和 revision 必变；
- 自由文本计划可以保存，但不能自动执行。

**回滚**：关闭 `execution_packages` feature flag，原 Epic/Step MCP 工具继续工作。

### W2：Runtime Store 与 Runner Core（shadow）

**依赖**：W0、W1。

**候选文件**：

- 新增 `desktop-app/src-tauri/src/execution_store.rs`
- 新增 `desktop-app/src-tauri/src/runner.rs`
- `desktop-app/src-tauri/src/models.rs`
- `desktop-app/src-tauri/src/lib.rs`
- Node 侧 Runtime Store 客户端

**施工**：

- 队列、租约、心跳、事件日志；
- Runner 仅 shadow 计算“会领取哪个 Step”，不启动 CLI；
- 实现 canonical path 和 project allowlist；
- 实现 pause/cancel/expired lease；
- Tauri/MCP 读写同一 runtime 协议。

**验收**：

- shadow 选择结果与 `actionable`、依赖门槛一致；
- 同一项目不产生两个写租约；
- 应用重启能恢复队列和识别陈旧租约；
- Runtime 损坏不影响 Plan Store，反之亦然。

**回滚**：`runner.enabled=false`，保留运行记录，停止领取新任务。

### W3：Git Worktree 与证据裁决

**依赖**：W2。

**候选文件**：

- 新增 `runner_git.rs`
- 受信命令注册表
- Git/verifier 集成测试

**施工**：

- base SHA、branch、worktree 创建与租约；
- changed-path 范围验证；
- 候选 commit 和 trailer；
- acceptance registry；
- candidate/verified/observed 阶段映射；
- 不自动 merge/push/deploy。

**验收**：

- 非允许路径变更必熔断；
- CLI exit 0 但无证据不推进；
- exact commit 验证通过才进入 verified；
- 主工作区用户未提交变更不被触碰；
- 失败 worktree 保留且可人工接管。

**回滚**：停止 Runner；删除行为不自动执行，worktree 由人确认后再清理。

### W4：Codex Executor MVP

**依赖**：W2、W3。

**候选文件**：

- 新增 `desktop-app/src-tauri/src/executors/mod.rs`
- 新增 `desktop-app/src-tauri/src/executors/codex.rs`
- Runner 集成测试与 mock JSONL fixture

**施工**：

- probe Codex CLI；
- 启动 `codex exec --json`；
- 解析 thread/turn/item/error；
- 保存输出、超时、取消；
- 支持受控 resume；
- 默认 workspace-write，拒绝危险 bypass 参数。

**验收**：

- mock 成功、失败、超时、取消、畸形 JSONL 全覆盖；
- 一个只读任务和一个隔离 worktree 写任务完成端到端；
- Codex 声称成功但 Git/验收失败时 QuestLog 不假绿；
- 人工取消后子进程退出、租约释放、证据保留。

**回滚**：关闭 `executor.codex.enabled`，Step 回到 ready/manual。

### W5：方案追踪执行 UI 与人工反馈

**依赖**：W1、W2；完整验收依赖 W4。

**候选文件**：

- `desktop-app/src/components/EpicBoard.tsx`
- `desktop-app/src/hooks/useEpics.ts`
- `desktop-app/src/types/epics.ts`
- Tauri commands
- i18n 文案与测试

**施工**：

- TaskPackage 摘要和 hash；
- 自动执行开关；
- Run 时间线；
- Git/verifier 证据；
- DecisionRequest 收件箱；
- pause/cancel/retry/resolve；
- “在执行器中讨论”入口。

**验收**：

- 用户无需复制任务正文即可启动、暂停、重试和回答 blocker；
- UI 状态来自 Runtime/Plan Store，不根据模型文字推断；
- 任何自动权限提升都必须经过显式 UI 授权。

**回滚**：隐藏 execution UI，不影响既有方案看板。

### W6：对话约束、Hook 与跨客户端适配

**依赖**：W1。

**候选文件**：

- MCP server instructions/tool descriptions
- `mcp-server/hooks/`
- `setup-mcp.sh`
- 客户端安装文档

**施工**：

- Codex SessionStart/PreToolUse/Stop Hook；
- Claude Code 对等 Hook；
- 其他客户端无 Hook 时保留 MCP 硬门禁；
- Hook 安装、信任、版本变更提示；
- 更新“生成可执行方案”的对话流程。

**验收**：

- Codex/Claude Code 漏填时能在会话内收到具体修复提示；
- 关闭所有 Hook 后，未封包任务仍无法入队；
- Hook 不扫描或上传秘密；
- 客户端不支持 Hook 时核心闭环仍工作。

**回滚**：禁用 Hook；服务端校验继续生效。

### W7：ZCode Adapter 与受控扩展

**依赖**：W4 端到端闭环稳定。

**施工**：

- 在真实目标机 probe ZCode CLI；
- 固化结构化输出、超时和取消协议；
- 不支持 resume 时用同一 worktree + 新 attempt；
- 执行器选择和 fallback 必须受 package 策略约束；
- 不自动在两个执行器间反复切换。

**验收**：

- ZCode 成功/失败/超时/取消可回写同一 Runtime Schema；
- 同一 Step 切换执行器不丢 Git 和历史证据；
- 执行器差异不改变 TaskPackage 和人工授权语义。

**回滚**：关闭 ZCode adapter，Codex/manual 不受影响。

## 14. 上线顺序与观察窗口

```text
W0 数据安全
  → W1 任务包硬门禁
  → W2 Runner shadow（只计算不执行）
  → W3 Git/verifier
  → W4 Codex 单项目 canary
  → W5 UI 闭环
  → W6 Hook 降低漏填
  → W7 ZCode 扩展
```

切换阶段：

1. `disabled`：只保留现有方案追踪；
2. `shadow`：显示“将要执行”，不启动 CLI；
3. `read_only_canary`：自动执行只读任务；
4. `worktree_canary`：单项目、单任务、隔离 worktree；
5. `bounded_auto`：仅批准的项目和任务包自动执行；
6. 保持 merge/push/deploy 人工授权，不默认进入全自动。

每次扩大范围前要求：

- 前一阶段无数据丢失；
- 无未归属 diff；
- 无假绿；
- cancel/重启/超时恢复通过；
- 用户确认人工时间确实下降，而不是把成本转移到排查自动化。

## 15. 总体验收标准

### 15.1 功能验收

- 人在任一已接入 MCP 的对话中创建一个 Epic 和多个 Step；
- QuestLog 自动指出任务包缺失字段并完成封包；
- 前置依赖满足后 Runner 自动领取；
- Codex CLI 在隔离 worktree 中执行；
- Runner 独立读取 Git 和 acceptance 证据；
- 通过后自动释放下一 Step；
- 遇到权限或方向问题生成 DecisionRequest；
- 人无需复制任务上下文即可回答并继续；
- 所有 Step 达到约定门槛后 Epic 完成。

### 15.2 安全验收

- 未封包、未授权、依赖不足的 Step 启动数为 0；
- 主工作区用户变更被覆盖数为 0；
- 非允许路径自动修改数为 0；
- 自动 merge/push/deploy 数为 0（默认策略）；
- JSON 损坏或并发冲突导致的数据丢失数为 0；
- CLI exit 0 导致假绿数为 0。

### 15.3 人效验收

以真实方案的至少 20 个 Step 为观察样本：

- 人工复制粘贴任务正文次数下降 ≥80%；
- 人工检查“任务是否完成”的次数下降 ≥70%；
- 已批准且无异常的 Step 自动派发率 ≥90%；
- 人工介入主要集中在授权、目标变化和多路径裁决；
- 平均每个 Step 的人工触点不超过 1 次；
- 所有人工触点都能追溯到结构化 DecisionRequest。

## 16. 全局回滚

任一阶段出现数据错误、越权、假绿或不可解释的自动推进时：

1. `runner.enabled=false`，立即停止领取新任务；
2. 正在运行的任务按策略取消或等待安全点退出；
3. 不删除 worktree、diff、日志和 Runtime 证据；
4. 所有 queued/running Step 转为 `paused`，不回退历史阶段；
5. `.epics.json` 保持向后兼容，旧 QuestLog 仍可查看方案；
6. 人工判定每个 candidate commit 的保留、重试或废弃；
7. 修复通过 shadow 后才重新开启。

## 17. 明确不做

第一阶段不做：

- 云端队列或多用户协作服务器；
- 向任意已打开的第三方 GUI 对话强制注入消息；
- 自动修改主分支；
- 自动 push、merge、发布或部署；
- 用模型自然语言自评替代 Git/verifier；
- 任意 shell 命令自动执行；
- 跨项目并发写入；
- 无上限的自动重试或自动换模型狂奔；
- 为 Runner 新建第二套方案/依赖/进度系统。

## 18. 开工决策

建议批准的第一施工批仅包含 W0 和 W1：

- 先保证现有 JSON 资产在多写入方下安全；
- 再建立 TaskPackage 服务端硬门禁；
- 暂不启动任何 CLI，不产生自动代码修改；
- W0/W1 验收通过后，才进入 W2 shadow。

这一顺序能先得到“对话生成标准任务包、QuestLog 展示和统筹”的直接价值，同时不把尚未验证的 Runner 接到真实代码写入链路。
