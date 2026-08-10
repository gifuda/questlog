# QuestLog 受控 AI 任务执行闭环：单一 Goal 执行包

> 日期：2026-08-10
>
> 来源方案：`docs/QuestLog受控AI任务执行闭环方案_20260810.md`
>
> 用途：把全部施工内容合并成一个可直接交给 Goal 机制的结果型目标；文中的 G0-G4 仅是该 Goal 的内部阶段，不得分别创建为多个 Goal；本文件不代表任何代码已经实施

## 1. 使用方式

只创建一个总 Goal，由它在内部严格按 `G0 → G1 → G2 → G3 → G4` 顺序持续执行。G0-G4 是内部依赖阶段，不是五个独立 Goal；阶段切换不要求用户重复回复“继续”。

这个总 Goal 是“结果目标”，不是一个只写方案或只完成首个子步骤的待办。Goal 启动后，执行者必须持续完成范围内所有可逆、可验证、无需新授权的工作，包括代码、测试、文档、迁移、错误修正和最终复核。

允许暂停的情形只有：

- 需要用户授予新的权限或批准不可逆动作；
- 必需的外部软件、凭证、目标机或服务不可用；
- 工作区存在无法安全隔离的用户改动；
- 方案出现两条会实质改变产品方向的路径，需要人裁决；
- 同一阻塞已通过三次不同的安全方法尝试，仍无法推进。

普通测试失败、类型错误、代码缺陷、实现细节选择、文档同步、格式修复不属于暂停理由，应在同一 Goal 内继续处理。

## 2. 全局执行契约

以下约束适用于全部 Goal，不需要在各 Goal 中重复询问：

### 2.1 工作范围

- 目标仓库：`/Users/abo/编程架构包/桌面清单贴`；
- 以当前代码和测试为准，README/方案仅作上下文；
- 开工先读取仓库内适用的 `AGENTS.md`、项目规则和当前 Git 状态；
- 保留并绕开用户现有改动，不清理、不覆盖、不重置；
- 源码施工使用专用 branch/worktree；
- 可在专用分支提交候选 commit，但未经单独授权不得 merge、push、发版或部署；
- 不修改 QuestLog 已登记项目中的用户 `.todos.json/.epics.json` 实际数据，测试只能使用临时 fixture。

### 2.2 一次性完成要求

- 不因完成了设计、单个文件、单项测试或首个子步骤而结束 Goal；
- 每完成一个内部步骤，立即回看剩余工作和新暴露的依赖；
- 发现可在当前权限内修复的问题时直接修复并重跑验证；
- 可以调整内部实施顺序，但不能静默扩大 Goal 的产品边界；
- 对无需人判断的实现细节采用最小、兼容、可回滚方案；
- 若一个子项被外部条件阻塞，继续完成同一 Goal 内所有不依赖该条件的事项；
- 只有所有验收条件通过，才可把 Goal 标记为 complete。

### 2.3 证据和交付

每个 Goal 结束时必须给出：

- 实际改动文件清单和目的；
- 当前 branch、base commit、候选 commit；
- 执行过的测试、命令和结果；
- 未执行的验证及明确原因；
- 与验收条件逐项对应的证据；
- 遗留风险、回滚方式和后续 Goal 是否已解锁；
- 若阻塞，生成一个结构化 DecisionRequest，只提出真正需要人的最小问题。

### 2.4 禁止假完成

- 编译通过不等于集成通过；
- CLI exit 0 不等于任务完成；
- commit 存在不等于验证通过；
- mock 通过不等于真实 CLI 可用；
- 单次实跑不等于持续稳定；
- 不得为了标记 complete 而跳过测试、缩小验收或把失败改写成“后续优化”。

## 3. 单一 Goal 的内部阶段总览

| 内部阶段 | 结果 | 依赖 | 预计人工介入 |
|---|---|---|---|
| G0 | 存储安全、TaskPackage 与入队硬门禁完整落地 | 无 | 无，除非发现不可迁移的真实数据格式 |
| G1 | Codex CLI 受控自动执行闭环在隔离 fixture 项目跑通 | G0 | 无，不接真实用户项目 |
| G2 | 方案看板、人工异常回流、Hook 和跨客户端对话流程完整落地 | G1 | 无，默认权限不扩大 |
| G3 | ZCode adapter 和统一多执行器协议完成真实能力核验 | G2 | 仅当本机无 ZCode CLI/鉴权时需要外部条件 |
| G4 | 单项目 canary、观察指标和正式放行裁决 | G0-G3 | 必须人工批准 canary 和扩大范围 |

G0-G3 是当前权限内应尽可能一次性完成的主体工程。G4 仍属于同一个总 Goal，但涉及真实项目自动改动和观察期，必须在该内部阶段保留人工授权闸门。

## 4. G0：完成存储安全与标准任务包硬门禁

### 可直接使用的 Goal 表述

```text
目标：在 QuestLog 仓库中一次性完成“多写入方数据安全 + 标准 TaskPackage + MCP 入队硬门禁”，使 Codex、Claude Code、ZCode 等任意 MCP 客户端生成的方案步骤只有在结构、权限、Git 基线和验收契约全部合格后才能进入自动执行队列。本 Goal 不启动任何 CLI，也不自动修改真实用户项目。

持续执行要求：从当前代码事实出发完成设计、实现、Node/Rust 对齐、数据迁移、测试、文档和复核；普通测试失败或实现问题必须在本 Goal 内修复，不要在完成局部功能后停下。只有出现新权限、无法安全隔离的用户改动或不可迁移的真实数据格式时才请求人处理。

必须完成：
1. 修复 `.todos.json/.epics.json` 的错误语义：仅 ENOENT 可创建空数据；权限、IO 和 JSON 损坏必须 fail-closed，保留恢复证据，禁止回退为空后继续写。
2. 为 Node MCP、Tauri UI 和未来 Runner 设计并实现同一 revision/CAS、跨进程锁和冲突重试协议；原子 rename 继续保留，但不得把它当成并发控制。
3. 保持既有 version=1 数据可读，提供确定性迁移和回滚测试，不触碰真实用户项目数据。
4. 定义 TaskPackage v1 结构：epic/step/revision、canonical project/repo、真实 base SHA、目标、允许与禁止路径、前置条件、受信验收命令、执行器策略、权限、停止条件、回滚和 package hash。
5. 完整 TaskPackage 只保存在本机 Runtime Store；`.epics.json` 只保存可移植的 package ID、revision、hash 和摘要，禁止提交个人绝对路径或会话运行细节。
6. 实现 prepare_task_package、validate_task_package、seal_task_package、enqueue_step；服务端自己读取 canonical path、Git 和策略，不信任模型伪造的 base SHA 或权限。
7. 未 sealed、已 superseded、依赖不足、路径越界、权限扩大或 Schema 不完整的任务必须拒绝入队，并返回结构化缺失/错误清单。
8. 建立受信验收命令注册表；TaskPackage 不接受任意 shell 字符串作为自动验收命令。
9. 补齐 Node 和 Rust 的单元、并发、损坏恢复、CAS 冲突、路径逃逸、向后兼容和 MCP 契约测试。
10. 更新中文/英文文档，说明 TaskPackage、硬门禁、数据迁移和 feature flag；保持现有 Todo/Epic 功能兼容。

验收：
- 100 轮并发更新不丢字段；
- 权限错误或损坏 JSON 后的写入被拒绝；
- CAS 冲突不会覆盖新版本；
- 既有 fixture 无损迁移；
- 任意 MCP 客户端都无法入队未封包任务；
- 伪造 project_path/base_sha/authority 被拒绝；
- 关闭 execution_packages feature flag 后，原有方案追踪继续工作；
- 全部现有测试和新增测试通过，格式、lint、build 达到仓库现行标准。

禁止：不启动 codex/zcode CLI，不创建真实项目 worktree，不自动 commit 用户项目，不 merge/push/发布。

完成条件：上述验收全部有可复现证据，数据格式向后兼容，回滚路径已验证，且 G1 所需的 sealed package 与安全写入接口均可调用。
```

### G0 解锁物

- 安全 Plan Store；
- TaskPackage v1；
- `sealed → ready` 硬门禁；
- Runner 可消费的稳定接口；
- G1 可以在 fixture 项目中施工。

## 5. G1：完成 Codex 受控自动执行最小闭环

### 可直接使用的 Goal 表述

```text
目标：在 G0 已通过的基础上，一次性完成 QuestLog Runtime Store、Tauri Runner、Git worktree/verifier 和 Codex CLI executor，使一个 sealed Step 可以在隔离 fixture Git 项目中被自动领取、执行、生成候选 commit、按 exact commit 验证、回写证据并释放下一 Step；不得接入真实用户项目或自动 merge/push/deploy。

持续执行要求：按 shadow → read-only fixture → writable fixture 的内部顺序持续推进。每一阶段失败时修复后重跑，不要把“shadow 通过”“Codex 有输出”或“出现 commit”当作 Goal 完成。只有端到端证据闭环和故障场景全部通过才结束。

必须完成：
1. 在 Tauri 托盘进程内实现 Runtime Store、队列、事件日志、输出目录、租约、心跳、取消、超时和陈旧租约恢复；高频运行状态不写入 `.epics.json`。
2. 实现 Runner 调度：仅领取 active Epic、有效 sealed package、依赖门槛满足、自动策略允许、无未决授权、路径 allowlist 合格且无写租约冲突的 Step。
3. 将 Step 依赖扩展为可声明 required_stage；旧字符串依赖继续按 observed 解释，新工程依赖可在 verified 后释放。
4. 实现 shadow 模式：计算“将领取什么”但不启动 CLI；重启后选择结果必须确定且不重复领取。
5. 实现专用 worktree/branch、clean baseline、base SHA 漂移检查、允许/禁止路径、最大文件数和未归属变更熔断。
6. 实现 Runner 拥有的候选 commit 与 QuestLog Epic/Step/Package/Execution trailers；主工作区和用户未提交变更不得被触碰。
7. 实现受信 acceptance registry 和 exact candidate commit 验证；CLI exit 0、自然语言“完成”或仅有 commit 都不能直接推进 verified。
8. 实现 Codex probe、`codex exec --json` 启动、JSONL 事件解析、session ID 保存、超时、取消、畸形输出和受控 resume；默认 workspace-write，拒绝危险 bypass 参数。
9. 实现运行状态与 Step 阶段映射：sealed→designed、candidate commit→coded、exact commit 验证通过→verified；observed 不由本 Goal 自动伪造。
10. 覆盖成功、无 diff、越界 diff、测试失败、CLI 失败、CLI 超时、取消、应用重启、陈旧租约、base 漂移、JSONL 损坏和 verifier 假绿等测试。
11. 在临时 fixture Git 项目完成至少一条只读链和一条两 Step 写链的真实 Codex CLI 端到端运行；测试中不得使用真实用户项目。
12. 更新架构、运行状态、日志位置、取消与恢复文档。

验收：
- 一个 sealed Step 自动从 ready 走到 candidate/verified；
- verified 后按依赖门槛自动释放下一 Step；
- 模型声称成功但 Git/acceptance 不满足时不推进；
- 非允许路径变更、base 漂移和租约冲突都 fail-closed；
- cancel、timeout、应用重启后状态一致，证据和 worktree 保留；
- 主工作区零改动，自动 merge/push/deploy 为 0；
- mock 测试和至少两条 fixture 真实端到端链全部通过；
- runner.enabled=false 能立即停止领取新任务，原方案追踪不受影响。

禁止：不接真实用户项目，不自动观察为 observed，不扩大到 ZCode，不 merge/push/部署。

完成条件：Codex fixture 闭环达到 E3 级单目标实跑证据，失败和回滚路径均验证，G2 可以直接消费稳定的 Runtime/Decision 接口。
```

### G1 解锁物

- 不依赖人工复制粘贴的 Codex CLI 执行链；
- Git/verifier 驱动的自动放行；
- 可暂停、取消、恢复的本地 Runner；
- G2 可以构建用户可见反馈闭环。

## 6. G2：完成人工异常回流、执行 UI 与对话合规闭环

### 可直接使用的 Goal 表述

```text
目标：在 G1 运行内核稳定的基础上，一次性完成 QuestLog 方案追踪中的任务包、Runs、Git/verifier 证据、授权/阻塞界面，以及跨 MCP 对话的 DecisionRequest 回流和 Hook 兜底，使用户无需复制任务正文即可启动、暂停、重试、回答阻塞并继续原 Step。

持续执行要求：完成前后端、MCP 工具、UI、i18n、Hook、安装脚本、测试和文档；不要只做界面或只做 MCP。Hook 不得成为权威门禁，关闭 Hook 后服务端规则仍必须成立。

必须完成：
1. 在 Epic/Step 下展示 TaskPackage 摘要、revision/hash、执行策略、worktree/branch/base/candidate SHA、运行时间线、测试/verifier 证据和当前状态。
2. 增加关闭/只读/worktree 写入的自动执行开关，以及 pause/cancel/retry/转人工操作；权限扩大必须显式批准。
3. 定义并实现 DecisionRequest：唯一问题、类型、选项、证据引用、阻塞范围、适用 package revision、回答人和回答时间。
4. 实现 list_decision_requests、resolve_decision；人在任意 QuestLog MCP 客户端中都能查询并回答，不需要复制完整上下文。
5. UI 增加“需要关注”派生收件箱，只聚合 approval、blocked、retry exhausted、Git drift 和 verifier failed，不建立第二套任务数据。
6. 支持“在 Codex 中讨论/继续”：对 Runner 管理的 Codex session 使用受控 resume；不能向任意第三方 GUI 线程注入时明确降级为带完整上下文的新会话，不要求人手工复制。
7. 更新 MCP server instructions 和工具描述：用户要求可执行方案时，引导模型创建 Step、prepare、validate、seal；缺字段由服务端返回具体修复清单。
8. 实现 Codex SessionStart/PreToolUse/PostToolUse/Stop Hook：注入活动任务、拦截非法 enqueue、记录工具结果、提醒漏封包；Hook 变更需要信任提示。
9. 实现 Claude Code 对等兜底；其他客户端不支持 Hook 时仍由 MCP 服务端门禁保证一致性。
10. 将现有只按对话关键词猜进度的 Stop Hook 降为辅助，不允许它单独把 Step 推进到 verified/observed。
11. 更新 setup-mcp、README 中英文、隐私与故障排查文档；不在日志或 UI 暴露秘密、完整 prompt 或不必要的个人路径。
12. 覆盖 UI 状态、DecisionRequest 生命周期、MCP 跨会话处理、Hook 开/关、权限拒绝、resume 降级和 i18n 测试。

验收：
- 从任一 MCP 对话创建并封包任务后，QuestLog UI 完整显示且无需复制；
- blocked 后人只回答一个结构化问题即可继续同一 Step；
- Hook 全部禁用时，未封包和越权任务仍无法运行；
- UI 不能绕过服务端 authority 或直接伪造 verified/observed；
- Codex session 可 resume，不能 resume 时能以同一 execution 上下文安全新开；
- 原 Todo/方案看板行为和既有测试不回归；
- 全部新增测试、构建、lint 和文档检查通过。

禁止：不把 Runner 做成独立一级任务模块，不自动提升权限，不向任意已打开 GUI 对话强制注入消息，不接真实项目 canary。

完成条件：用户从“对话形成方案”到“AI 执行、异常回流、继续执行”的完整 fixture 体验不需要手工复制任务正文，且所有状态有 Plan/Git/verifier 证据。
```

### G2 解锁物

- 用户可见、低打扰的异常处理闭环；
- 对话端任务包合规体验；
- 不依赖 Hook 的跨客户端一致性；
- G3 可以只关注执行器差异。

## 7. G3：完成 ZCode Adapter 与统一多执行器协议

### 可直接使用的 Goal 表述

```text
目标：在 G2 已通过的基础上，一次性完成 ZCode CLI 真实能力探测、adapter、事件/输出适配、取消/超时/续接策略和统一多执行器测试，使同一个 sealed TaskPackage 可以在不改变权限、Git、验收和人工回流语义的前提下选择 Codex 或 ZCode 执行。

持续执行要求：先在目标机读取实际 ZCode CLI --help、版本和最小 dry-run，不根据历史路径或旧文档猜命令。若本机缺少 ZCode CLI、鉴权或所需运行环境，先完成所有与真实外部条件无关的接口、fixture、mock、UI 和文档工作，再生成一个最小 DecisionRequest 说明唯一缺失条件；不得因外部缺失而提前放弃可完成工作。

必须完成：
1. 定义 executor probe/start/stream/resume?/cancel 标准接口和 capability manifest。
2. 真实探测 ZCode 可执行文件、版本、非交互 prompt、结构化输出、退出码、超时、取消和 session/resume 能力。
3. 根据真实探测实现 ZCode adapter；无 resume 时使用同一 Step/worktree/Git/Runtime 生成新 attempt，历史证据不可覆盖。
4. Codex 与 ZCode 共用 TaskPackage、authority、worktree、Git/verifier、DecisionRequest 和状态机；adapter 不得自行解释完成或扩大权限。
5. executor fallback 只能按 package 声明执行；一次失败不得无限自动换模型，达到上限后 blocked。
6. 覆盖成功、失败、超时、取消、无结构化输出、无 resume、鉴权失败和执行器切换测试。
7. 在临时 fixture Git 项目完成一条真实 ZCode 只读链和一条允许写链；若外部条件缺失，将真实链明确保留为未通过，不得用 mock 冒充。
8. UI 显示执行器、attempt 历史和 capability；更新安装、探测、降级和故障排查文档。

验收：
- ZCode 能通过统一接口运行并回写同一 Runtime Schema；
- 执行器切换不丢 package、worktree、Git 和人工决策历史；
- ZCode 自报成功但 Git/verifier 不通过时不推进；
- 无 resume 时新 attempt 能继续而无需人复制上下文；
- 外部 CLI/鉴权缺失被准确报告为未完成的真实验证，不假绿；
- 关闭 executor.zcode.enabled 后 Codex/manual 不受影响。

禁止：不自动在执行器间反复切换，不改动任务目标，不接真实用户项目，不 merge/push/部署。

完成条件：真实 ZCode 端到端 fixture 通过；若唯一阻塞是外部 CLI/鉴权，则 Goal 状态保持 blocked 而非 complete，但所有不依赖外部条件的工程项必须已经完成并有证据。
```

### G3 解锁物

- 统一多执行器能力；
- ZCode 真实适配证据；
- G4 可以选择一个或多个已验证执行器进行 canary。

## 8. G4：完成单项目 Canary、持续观察与放行裁决

### 可直接使用的 Goal 表述

```text
目标：在 G0-G3 全部通过并获得用户明确 canary 授权后，在一个用户指定的非关键项目上完成 QuestLog 受控自动执行 canary、故障演练、人效测量和持续观察，形成是否扩大范围的证据裁决。未获得授权时不得启动真实项目自动写入。

持续执行要求：授权后按 shadow → read-only → 单 Step worktree write → 多 Step 串行的顺序推进；每阶段只有达到门槛才进入下一阶段。普通可修复缺陷应在本 Goal 内修复并重新开始相应观察窗口；涉及权限扩大、真实 merge/push/deploy 或范围扩大时必须再次获得人工批准。

必须完成：
1. 记录 canary 项目、base commit、允许路径、执行器、测试注册表、禁止动作、取消方式和观察期限。
2. shadow 阶段核对至少 10 个待执行决策，确保调度顺序、依赖和授权判断一致。
3. read-only 阶段验证 CLI、日志、DecisionRequest、超时、取消和重启恢复。
4. worktree write 阶段从单 Step 开始，逐步完成至少 20 个真实 Step；merge/push/deploy 继续人工控制。
5. 演练 CLI 失败、测试失败、越界 diff、base 漂移、重复领取、应用重启、取消和执行器不可用。
6. 记录人工复制粘贴次数、人工检查次数、自动派发率、每 Step 人工触点、blocked 原因和假绿/越权/数据丢失事件。
7. 若代码、规则、执行器或范围发生实质变化，重置对应观察窗口，不能沿用旧数据。
8. 输出扩大范围、继续观察或回滚的正式裁决；扩大范围必须由人批准。

放行门槛：
- 未封包、未授权、依赖不足任务启动数为 0；
- 主工作区用户变更覆盖数为 0；
- 非允许路径自动修改数为 0；
- 默认自动 merge/push/deploy 数为 0；
- 数据丢失和假绿数为 0；
- 人工复制粘贴任务正文次数下降至少 80%；
- 人工检查完成状态次数下降至少 70%；
- 已批准且无异常 Step 自动派发率至少 90%；
- 平均每个 Step 人工触点不超过 1 次；
- 所有人工触点都对应结构化 DecisionRequest；
- cancel、重启、失败和回滚演练全部通过。

禁止：没有明确授权不启动真实写入；不得为了达到指标删除失败样本；不得把单次成功、mock、exit 0 或 commit 当作持续稳定证明。

完成条件：至少 20 个真实 Step 的完整观察满足全部门槛，并由用户批准扩大范围；若证据不足，Goal 保持观察或 blocked，不能标 complete。
```

## 9. Goal 自动推进规则

为了减少用户逐步盯进度，执行 G0-G3 时采用以下内部推进规则：

```text
读取代码与规则
→ 建立当前基线
→ 完成设计
→ 实施全部范围
→ 运行针对性测试
→ 运行全量相关测试
→ 修复失败并重跑
→ 检查 diff 和越界
→ 更新文档/迁移/回滚
→ 复核验收证据
→ 仅在全部通过时 complete
```

不要求用户在“设计完成、代码完成、测试完成”之间重复回复“继续”。Goal 机制应自动持续推进，直到达到完成或真实阻塞终态。

## 10. 人只处理的四类事项

系统最终只应把以下事项交给人：

1. **授权**：是否允许真实项目写入、merge、push、部署或扩大权限；
2. **方向裁决**：两条路径会实质改变产品定位或数据契约；
3. **外部条件**：缺少 CLI、鉴权、目标机或服务；
4. **失败仲裁**：多次安全尝试后仍无法满足验收，需要取消、改目标或接受风险。

测试修复、类型错误、文档同步、重新运行、格式修正、生成任务包、创建 worktree、整理证据等重复工作不应再次要求人确认。

## 11. 推荐启动方式

只创建一个总 Goal。总 Goal 自动依次推进 G0-G3，不创建新的 Goal，也不要求用户在阶段之间确认。进入 G4 时，总 Goal 保持 active 并生成一次 canary 授权请求；获得用户明确指定项目和权限后继续完成观察，未获授权不得把 Goal 标记为 complete。

单一 Goal 可以跨越多个内部阶段，但不能因此合并权限边界：真实项目写入、merge、push、发布、部署和扩大范围仍分别受人工授权约束。
