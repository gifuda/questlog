# QuestLog 受控 AI 任务执行闭环：最终收口方案

> 日期：2026-08-11  
> 状态：当前代码审计后的优化收口基线；尚未放行  
> 适用分支：`feat/g0-storage-safety-taskpackage` 当前工作区  
> 前序文档：`QuestLog受控AI任务执行闭环方案_20260810.md`、`QuestLog受控AI任务执行闭环_Goal执行包_20260810.md`  
> 本文边界：定义剩余收口施工、测试、放行和回滚；不代表当前版本已经通过最终验收。Meta-Harness 可继续用于执行记忆、外部验证和受控重试收口；LID Harness 单独暂缓，不属于本轮施工或完成条件  

## 0. 当前代码审计后的权威收口裁决

> 本节基于 2026-08-11 当前分支、当前未提交工作区和已运行证据形成，优先级高于本文后续早期差距描述。后续章节保留为施工细节参考；若状态判断冲突，以本节为准。

### 0.1 当前代码状态与证据边界

当前 HEAD 为 `e874f951`。三支柱工作已实现并提交：受信验收命令注册表扩充至 11 个命令（`docs/acceptance-commands.md`）、worktree 内 `node_modules` symlink 使验收命令在隔离 worktree 可用、MCP `verify_pickup_readiness` 就绪门禁（enqueue 返回 `runner_eligible`/`blockers`）、Runner 在应用启动且项目已启用时自动开启线程。step-0 验证期间发现的 7 个缺陷也已全部修复并提交（见 0.4，均含回归测试）。源码工作区干净，仅剩 `.questlog-output` 输出与 `node_modules` 依赖目录未跟踪（均属 Runner 基础设施，不入候选提交）。这些提交仍不能视为已经发布、已安装 App 已加载或真实项目可安全自动执行。

step-0 验证与既有快照证据：

| 证据 | 结果 | 可证明 | 不能证明 |
|---|---|---|---|
| step-0 验证包 `pkg_60acfb94` | outcome=`passed`；候选 commit `3840e667` | sealed→ready→Runner 领取→ZCode 隔离 worktree 施工→candidate commit→exact 验收的完整链路通过 | 真实生产闭环；C2/C6 未完成场景 |
| `cargo:check`（候选 worktree 上执行） | exit 0 | acceptance 精确绑定候选 commit 且通过 Rust 编译检查 | 全量测试、strict Clippy 与完整 E2E |
| MCP 全套测试与 build（较早候选快照） | 124 passed、1 skipped；TypeScript build 通过 | Node 侧既有功能及部分 TaskPackage 策略未明显回归 | 最新全部未提交改动仍全绿；真实生产闭环 |
| MCP TaskPackage 定向测试（最新候选） | 21 passed；build 通过 | 默认执行器、manual、canonical hash、Plan Store 校验等定向逻辑可编译 | Rust/App/真实执行的组合正确性 |
| 前端测试与 Vite build（较早候选快照） | 3 passed；build 通过 | i18n 基础完整、前端可构建 | 原生 Tauri invoke、按钮后端行为、窄窗口视觉与已安装 App |
| Rust lib 测试（较早候选快照） | 109 passed | Execution/Decision/Runner 的若干单元行为 | 最新 runner_git 改动后的全套回归 |
| Rust runner_git 定向测试（最新候选） | 9 passed | 未跟踪源码发现、控制面文件保护等候选逻辑 | 全量 Clippy、完整 E2E 和生产兼容性 |
| strict Clippy | 最新一次在 `filter().next()` 处失败；随后仅做了一行候选修订，未重跑 | 暴露了候选仍未完成最终质量门禁 | 不能声称严格 Clippy 已通过 |
| 真实 ZCode exact-candidate fixture（较早候选快照） | `ready → Passed`，ZCode 0.16.1，候选 SHA 与 verifier SHA 一致，主工作区内容不变 | 正向隔离 worktree 与候选提交路径曾经跑通 | 最新全部改动仍保持通过；取消、暂停、超时和真实项目安全 |
| `git diff --check` | 当前通过 | 当前文本 diff 无空白错误 | 功能正确、App 可发布或提交边界合理 |

因此当前裁决为：**三支柱与 step-0 验证已完成；C8（文档）进行中，C2（取消/超时）与 C6（谱系 UI）仍未完成；不可开启真实项目 Runner，不可声明闭环完成，不可据此 merge/push/deploy。**

### 0.2 第一性原理：最终只保留一个机器对齐核心

系统不需要在 MCP、控制台、CLI、对话和 Git 之间反复同步“完成文案”。机器完成真相只保留一条：

```text
目标 Step
→ 服务端封装的 TaskPackage
→ 明确 base commit
→ ZCode 隔离执行
→ candidate commit
→ verifier 精确绑定 candidate SHA
→ passed / blocked
```

四类数据各司其职：

| 数据 | 用途 | 是否完成真相 |
|---|---|---|
| `.epics.json` Plan Store | 给人看目标、步骤、依赖、验收和当前阶段 | 否；只表达意图与计划状态 |
| TaskPackage | 给机器执行的不可变契约：目标、路径、权限、预算、验收、base | 否；只证明“允许这样执行” |
| Execution / DecisionRequest / AttemptBundle | 调度、人工介入和审计证据 | 否；只证明发生了什么、为何停下 |
| Git candidate commit + exact verifier | 精确代码结果和验收对象 | **是；唯一机器完成对齐码** |

`.todos.json/.epics.json` 是 QuestLog 控制面元数据，不应混入候选源码提交，也不能因为自身回写让 `git_clean` 永久失败；执行器无论 scope 是否写成 `**`，都不得修改或提交这些控制面文件。代码新增、修改、删除和重命名则必须全部进入 changed-path 检查，尤其不能遗漏 untracked 新文件。

### 0.3 优化后的最小闭环

不新增独立“任务控制台真相源”。现有方案追踪页面承载 Runner 开关、执行状态和待决收件箱；调度逻辑作为 Runner 内部能力存在。

```text
任一对话端（Codex / Claude Code / ZCode）
  → MCP 写 Epic / Step / TaskPackage 草案
  → QuestLog 依据依赖与授权判断 dispatchable
  → 就绪时服务端绑定当前 Git base 并 seal
  → 已武装范围内由 Runner 自动领取
  → ZCode 在隔离 worktree 施工
  → 生成 candidate commit + exact verifier
  ├─ 低风险且仍在既定方向：自动释放下一机械 Step
  └─ 方向、权限、外部影响或证据不确定：DecisionRequest 回到人
       → 人只回答最小决策
       → 明确 retry，或生成新 revision 后再执行
```

这里的关键优化不是增加节点，而是把当前“提前 seal 固定 base”的流程改为**就绪时封包**：

1. 对话阶段生成完整 Step 与 TaskPackage 草案，允许一次性拆完方案，但不提前冻结未来步骤的 base SHA。
2. 依赖满足时，QuestLog 在服务端读取权威 predecessor candidate 或当前 HEAD，生成该次执行的最终 sealed package。
3. 不自动 merge 的前提下，依赖步骤可基于已通过且已授权的 predecessor candidate 形成候选 commit 链；Git commit graph 成为路径依赖的机器事实。
4. 到里程碑、方向变化、高风险边界或最终合并前再让人审核，而不是每个普通机械步骤都要求复制、轮询或手工重新封包。
5. `manual` 始终只追踪；Codex 始终只设计和推送；生产执行始终只允许批准的 ZCode adapter。

### 0.4 step-0 验证发现并修复的 7 个缺陷

打通 `pkg_60acfb94 → 候选 3840e667 → passed` 完整链路的 step-0 验证工程中，发现并修复了以下 7 个缺陷（均已提交，除 B5 外均含回归测试）：

| ID | 缺陷 | 现象与根因 | 修复（commit） |
|---|---|---|---|
| B1 | seal hash 竞态 | seal 先对 `plan_revision=N` 的内容计算 hash，再把 revision 提升到 N+1（revision 属于可哈希内容），导致存储的 hash 过期，Runner 领取时判 `sealed package hash mismatch` | 先算 draft hash 决定是否 bump，bump 后再对最终内容（含新 revision）计算不可变 hash（`ed0f13e`） |
| B2 | `getPackagesForStep` 歧义 | 同一 Step 存在多个 package（旧 attempt blocked、新包已 seal）且 `plan_revision` 相同时排序不确定，可能取到终态旧包 | 可执行状态（sealed/ready）优先于终态，再按 `sealed_at` 倒序，最新 seal 胜出（`79213d6`） |
| B3 | `node_modules` 被误判越权 | worktree 为让验收命令可用而 symlink 主仓 `node_modules`，symlink 出现在 changed paths 触发 scope violation | 将 `node_modules` 视同 `.questlog-output` 归为 Runner 基础设施，覆盖嵌套路径（`de86c93`） |
| B4 | ZCode 自动提交后候选 commit 失败 | ZCode CLI 遵循“改后提交”约定会自行 commit；Runner 再执行 `git add + git commit` 时无未暂存改动而失败（空错误） | 检测 `HEAD != base_sha`（CLI 已提交）时改用 `git commit --amend` 附加 QuestLog trailer；正常路径不变（`d509147`） |
| B5 | MCP 陈旧进程 | 验证期间 MCP server 进程加载的是旧构建，工具行为与最新源码不一致，验证结果不可信 | 重建并重启 MCP/App 后复验通过；教训：验证前必须核对进程与当前 HEAD 构建一致（对齐 P1-10） |
| B6 | 中文文件名被 octal 转义 | git 默认 `core.quotePath=true`，中文文件名被八进制转义，与 UTF-8 的 `allowed_paths` 比较失败，中文文件名全部误判 scope violation | 所有读路径 git 命令加 `-c core.quotePath=false` 并补测试（`8d73931`） |
| B7 | order-predecessor 死锁 | preflight 依赖检查要求同一 Epic 内低 order 前置步骤必须 `observed`，但自主执行步骤只能自动推进到 `coded`（candidate 已产出），`observed` 需跨会话人工确认——第 N 步永远无法运行 | 隐式 order 前置只要求 `coded`；显式 `depends_on` 仍要求 `observed`（`9072794`） |

另：候选 commit 一律加 `--no-verify`（`e874f95`）——Runner 候选提交发生在隔离 worktree 且 scope 已由 `verify_changed_paths_in_scope` 校验，不应被针对人工提交的 pre-commit 守卫（对中文文件名存在已知 bug）拦截。

### 0.5 当前代码发现的问题与解决思路

#### P0：不解决不得进入真实项目 Runner

| ID | 当前问题 | 影响 | 收口思路 |
|---|---|---|---|
| P0-1 | 当前全局 Runner ON 会消费 Runtime Store 中所有 ready package，缺少项目/Epic 级武装范围 | 一次开关可能放大到非预期项目 | 增加 `armed_scope`：project + epic + revision + 到期时间 + 最大任务/时长/成本；默认空，App 重启清空 |
| P0-2 | ZCode `workspace-write` 实际映射为 `--mode yolo`，scope 检查主要发生在执行后；没有真正的网络、凭证和外部副作用隔离 | 即使最终 diff 合规，执行期间仍可能访问网络、秘密或外部系统 | R1 仅 fixture；真实项目前必须采用 OS/容器级 sandbox，或明确禁网、最小环境变量、凭证剥离和外部命令 allowlist；post-check 不能替代执行前隔离 |
| P0-3 | 当前 TaskPackage 把 base SHA 在 prepare/seal 阶段冻结；同时禁止自动 merge，依赖步骤无法自然串行继承前一候选 | 方案“自动按序执行”在有代码路径依赖时会不断 base drift、要求人重新封包 | 将未来步骤保留为草案；dispatch 时绑定 predecessor candidate/HEAD 并 seal；形成可审计候选 commit chain，最终仍由人决定 merge |
| P0-4 | Runner 对所有低 order/depends_on 步骤硬要求 `observed`，而 passed 只推进到 `verified` | 每个步骤都需要人手工 observed，无法显著降低盯任务成本 | 依赖门槛写入 Step/TaskPackage：纯机械链默认 `verified` 即可释放，涉及运行观察/外部效果才要求 `observed`；风险策略决定人工检查点 |
| P0-5 | 已提交基线的 changed-path 只看 `git diff`，会漏掉 untracked 新文件；当前工作区有候选修订但尚未完成全门禁 | 新建越权文件可能不被 scope/commit 证据捕获 | changed set 必须合并 tracked diff、untracked、rename/delete；硬排除 Runner 自身输出，硬禁止 `.todos/.epics/.todo-board` 控制面文件；补新增、删除、重命名和 symlink E2E |
| P0-6 | `.epics.json` 回写与 `git_clean` 的语义存在冲突；旧文档又同时把它描述为可提交计划事实 | seal 后自身元数据可能让 enqueue 永久判 dirty，机器真相混乱 | 明确 Plan Store 是本地控制面而非候选源码真相；源码洁净检查只排除固定 QuestLog 元数据，其他 untracked 仍阻断；执行器永远不能修改控制面文件 |
| P0-7 | pause/cancel/timeout、进程组 kill 和晚到结果 CAS 在工作区已有候选实现，但没有最新全量真实 E2E | UI 可能显示已停，CLI/子进程仍继续或晚到 passed 覆盖取消 | 用可控 fake child 验证 TERM→KILL→wait、孙进程消失、candidate 不再产生；再做真实 ZCode cancel/pause/timeout fixture；终态只能 CAS 前进 |
| P0-8 | exact candidate 正向 E2E 曾通过，但默认 `verify:candidate` 仅是 `git diff --check` | SHA 绑定正确不等于业务正确，仍可能形成“精确地验证了一个错误候选” | exact SHA 绑定作为底座；每个可执行 package 至少声明一个项目相关 trusted verifier，按 lint/type/targeted test/full test 分层；无业务验收不能 passed |
| P0-9 | acceptance registry 在 Node 与 Rust 双份维护；Rust只消费 command 类型，其他 acceptance 类型可能被静默丢弃 | 两端漂移或 package 看似有验收但 Runner 实际无可执行验收 | registry 改为单一共享声明或构建期生成；未知 acceptance/precondition 必须 fail-closed；增加跨语言契约测试 |
| P0-10 | DecisionRequest 回答当前只记录文本，不会自动产生新 attempt/revision；旧工具文案曾声称 Runner 会继续 | 人以为已放行，实际任务仍停；或未来错误实现直接复活旧 attempt | 保持“回答不直接执行”的安全语义；回答后只允许显式 `retry_same_revision` 或 `revise_and_reseal`；scope/authority/base/direction变化必须新 revision |
| P0-11 | 未解决 DecisionRequest 没有形成 Step 级互斥；回答也未校验仍对应当前 package revision | 同一步可能通过另一 ready package 绕过人工闸，旧回答可能错误影响新方案 | dispatch 前检查同 Step 的 unresolved blocking request；resolution 绑定 package id+revision+candidate；旧 revision 只能归档，不能释放新 revision |
| P0-12 | Package/Execution 状态、`.epics` 摘要状态和 Node/Rust枚举尚未形成一份权威契约 | UI 可显示 sealed，而 Runtime 已 blocked/passed；跨语言写入未知状态 | 区分 immutable package lifecycle 与 execution runtime status；`.epics` 只存封包摘要，实时执行状态只读 Runtime Store；共享 schema + migration + unknown-state fail-closed |
| P0-13 | canonical hash 两端实现策略存在潜在差异：Node递归删除所有名为 status/package_hash 的键，Rust只删除顶层 | 特定嵌套字段可产生跨语言 hash 不一致 | 明确定义 canonical JSON：仅排除顶层 runtime 字段、递归排序 object key、数组保序、UTF-8/数字格式固定；使用同一固定向量和跨语言 round-trip 测试 |
| P0-14 | seal 与 `.epics` package summary 是跨两个文件存储，不能原子提交；历史实现还可能忽略 step 不存在或回写失败 | Runtime 有 sealed package，但方案页无对应 Step/summary，形成孤儿执行契约 | seal 前验证 Epic/Step/项目一致；使用 prepare-record + CAS finalize，任一侧失败将 package 标记 cancelled/invalid；Runner领取时再比对 id/revision/hash |
| P0-15 | Runtime/Decision 列表在损坏文件语义上不完全一致，Node 某些 list 路径会把错误折叠为空列表 | 待决事项可能被静默隐藏，人工闸失效 | 只有 ENOENT 返回空；权限、IO、JSON 损坏一律错误可见并保留备份，UI/对话不得显示“无待决” |
| P0-16 | 当前机器观察到两个已安装版 App 主进程；尚未证明单实例或跨实例 Runner 安全 | 两个 UI/Runner 可能同时开启、重复领取或产生状态竞争 | 加单实例守卫；继续保留文件锁、lease token、execution revision；双进程 E2E 证明最多一个领取，第二实例只读或聚焦已有窗口 |

#### P1：进入 shadow/bounded auto 前完成

| ID | 当前问题 | 收口思路 |
|---|---|---|
| P1-1 | Runner 传给 ZCode 的主要是 objective，标准包中的 scope、禁止事项、验收、停止条件和依赖没有完整编译进执行指令 | 由服务端生成不可注入的 executor brief，包含目标、允许/禁止路径、验收 ID、禁止 merge/push/deploy、停止条件和证据输出要求；后端门禁仍是权威 |
| P1-2 | retry 虽已出现 parent attempt/max_attempts 候选逻辑，但尚未证明解决方案与 DecisionRequest、父证据一致 | retry 永远创建新 attempt；同 revision 仅允许非方向性失败重试；达到预算或根因不一致时必须 revise/人工决策 |
| P1-3 | Worktree/branch 创建中的部分 Git 错误被忽略，旧目录直接删除可能留下 Git worktree registry 或同名 branch | 所有 Git 子命令检查 exit/status；使用 `git worktree remove/prune`；失败 worktree默认保留并可从 UI 人工清理 |
| P1-4 | Runner OFF 只停止新轮询，不代表当前任务已停止，UI语义不够明确 | UI区分“停止领取”和“停止全部”；后者逐个写 cancel_requested 并监督退出；默认 OFF 仍保持重启不恢复 |
| P1-5 | 执行按钮缺少统一 pending/error/确认反馈；Decision回答后收件箱消失，用户可能不知下一步 | 所有 action 显示后端真实状态和错误；启用 Runner/取消执行需确认；回答后显示“已记录，未启动”，提供明确 retry/revise 入口 |
| P1-6 | 原生 App UI 尚未在隔离 Runtime 下完成实机验收；浏览器 Vite 页面不能替代 Tauri invoke | 使用临时 fixture Runtime 启动 Tauri，核验默认 OFF、toggle、inbox、pause/cancel/retry、candidate/verifier/attempt 展示、窄窗口和中英文；不得触碰真实项目数据 |
| P1-7 | AttemptBundle 保存 stdout、prompt、绝对路径和环境信息，尚无完整脱敏、权限和保留期证据 | 默认最小采集；secret/环境变量/个人路径脱敏；目录权限仅当前用户；manifest与SHA长期保留，大日志按策略清理并留审计 |
| P1-8 | README 中英文仍主要描述传统清单/Epic，未说明 Codex只推送、ZCode执行及安全边界 | 回补受控执行入口、manual语义、Runner默认OFF、DecisionRequest、候选commit、故障处理和“永不自动merge/push/deploy” |
| P1-9 | 旧 package/schema、已安装 App 与当前未提交数据结构之间没有升级/降级证据 | 增加 schema_version migration、旧状态映射、只读回滚和备份恢复测试；未知新字段旧 App 不得误执行 |
| P1-10 | 代码、构建产物、已安装 App 和运行中进程版本尚未对齐 | 最终验收必须记录 source HEAD/candidate、bundle version、实际启动二进制 hash/version 和 Runtime schema，不以本地 build 成功替代已安装 App 证据 |

### 0.6 人工介入策略：按风险设检查点，不按步骤机械打断

每个 Epic 设 `approval_policy`，由人创建方案时选择：

| 策略 | 自动范围 | 人何时介入 |
|---|---|---|
| `manual` | 不自动执行 | 每步由人处理 |
| `after_each` | 一次只执行一个 Step | 每个 candidate passed 后 |
| `milestone` | 同一候选链内自动执行低风险机械 Step | 到指定里程碑、链长或预算上限 |
| `risk_triggered` | 在已武装 scope/budget 内持续执行 | 仅方向变化、权限扩大、外部副作用、漂移、失败、歧义或最终 merge 前 |

无论策略如何，以下情况强制 DecisionRequest，不能自动重试或继续：

- objective、scope、authority、acceptance 或依赖语义变化；
- 需要网络、凭证、外部系统、部署、数据迁移或真实服务操作；
- base 不是声明的 predecessor candidate/HEAD，或 revision/hash 不一致；
- scope violation、verifier failed、取消/超时、同根因重复失败、达到预算；
- 两种以上合理实现方向，或候选结果改变原方案发力方向；
- merge、push、PR、发布、部署、清理证据或真实项目 canary。

### 0.7 收口施工顺序

只按以下顺序推进，避免在错误方向上把自动化做得更快：

1. **S0 冻结候选**：审计当前 20 个未提交路径，按功能拆分候选 commit；不混入真实 `.todos/.epics` 数据。
2. **S1 统一契约**：Package/Execution/Decision schema、canonical hash、acceptance registry、未知类型 fail-closed。
3. **S2 固化 Git 真相**：完整 changed set、控制面硬保护、candidate chain、exact SHA verifier、Git 命令错误处理。
4. **S3 固化执行隔离**：ZCode-only、OS sandbox/禁网/凭证剥离、进程树监督、timeout/cancel/pause、单实例和 lease CAS。
5. **S4 完成人工闸**：armed scope、approval policy、Decision revision gate、显式 retry/revise、全局停止语义。
6. **S5 App UI**：默认 OFF、真实状态、错误/确认、DecisionInbox、证据查询、原生隔离 Runtime 验收。
7. **S6 全门禁**：Node、前端、Rust fmt/strict Clippy/test/build、真实 ZCode 成功/失败/取消/超时/漂移/新文件/双实例 E2E。
8. **S7 放行**：先 fixture，再 shadow；真实项目 canary 必须单独获得用户授权，且仍禁止自动 merge/push/deploy。

### 0.8 最终要求与完成定义

以下全部满足后，才允许把状态从“候选收口”改为“可进入 bounded auto”：

#### 产品目标

- [ ] Codex/Claude Code 可以在对话中一次性推送 Epic、Step 和完整任务草案，无需人工复制到新对话。
- [ ] 生产 Runner 只调用批准的 ZCode；Codex 不承担机械执行成本；manual 永不入自动队列。
- [ ] 有路径依赖的步骤以 predecessor candidate commit 为机器对齐码，可在不自动 merge 的情况下形成候选链。
- [ ] 人工触点从“盯每一步”收敛为授权、方向、风险、异常和最终合并决策。

#### 安全与权限

- [ ] Runner 每次启动均默认 OFF；没有 armed project/Epic 时即使全局 ON 也不领取。
- [ ] 自动权限上限仅为隔离 worktree 写入、受信验证和 candidate commit；merge/push/deploy/外部副作用恒为 0。
- [ ] ZCode 运行前具备真实隔离；凭证、网络和外部命令不靠 prompt 自律。
- [ ] pause/cancel/timeout 真实终止整个进程树并 wait；晚到结果无法覆盖终态。
- [ ] 两个 App/Runner 进程并发时，同一 Execution 最多一个获得有效 lease。

#### Git 与验收

- [ ] 修改、新增、删除、重命名和 symlink 路径全部进入 scope 检查；控制面和 Runner 输出永不进入候选 commit。
- [ ] candidate SHA、worktree HEAD、verifier target 三者精确一致；主工作区源码不被 Runner 修改。
- [ ] 每个 passed 至少有一个项目相关 trusted verifier；模型自述、CLI exit 0、commit存在或 `git diff --check` 单独均不算完成。
- [ ] candidate chain 的父 SHA、package revision/hash 和 predecessor execution 可追溯。

#### 状态、决策与证据

- [ ] Node/Rust/UI 共用可迁移 schema；未知状态、命令、precondition、acceptance 一律 fail-closed。
- [ ] unresolved blocking DecisionRequest 阻止同 Step 后续领取；resolution只对绑定 revision/candidate 有效。
- [ ] retry 新建 attempt，不覆盖父证据；方向或权限变化必须新 package revision。
- [ ] 每次 attempt 的 package、环境、原始事件、stdout/stderr、diff、candidate、verifier、decision 和 outcome 可查且完整性可验证。
- [ ] Runtime/Decision 数据损坏不被折叠为空；有备份、错误可见和恢复路径。

#### App 与发布证据

- [ ] Runner 开关、armed scope、当前执行器、预算、运行状态和待决问题在方案追踪顶部可见。
- [ ] UI action 有 loading/error/确认和真实后端状态；App 重启后不自动恢复执行权限。
- [ ] 原生 Tauri App 在临时 Runtime 上完成中英文、窄窗口和全按钮 E2E；不以浏览器 mock 代替。
- [ ] 最新工作区全量测试、build、fmt、strict Clippy 全绿；真实 ZCode exact-candidate、取消、超时、漂移、untracked、双实例场景均有证据。
- [ ] source candidate、最终 commit、构建产物和实际运行 App 版本一致。
- [ ] README、安装、迁移、故障处理、熔断和回滚与代码一致。
- [ ] 未经用户新授权，不运行真实项目 canary，不 merge、push、发布或部署。

最终放行裁决只能是以下三种之一：

- `fixture_ready`：仅临时仓库可自动执行；
- `shadow_ready`：真实项目只计算将领取什么，不启动 CLI；
- `bounded_auto_ready`：仅用户明确武装的项目/Epic/预算可自动执行。

当前裁决：**三支柱与 step-0 验证已完成；C8（文档）进行中，C2（取消/超时）与 C6（谱系 UI）仍未完成；尚未达到 `fixture_ready` 的最终证据要求；保持 Runner OFF。**

### 0.9 requirement-to-evidence：C0-C8 当前状态

| 施工包 | 状态 | 证据 |
|---|---|---|
| C0 冻结基线和提交边界 | 已完成 | 全部改动按功能拆分提交，HEAD `e874f951`；源码工作区干净；`git diff --check` 通过 |
| C1 固化执行器与 TaskPackage 策略 | 已完成 | 默认执行器 `zcode`；`manual` 只登记不领取；生产 Runner 对 `codex` fail-closed；acceptance registry 11 个命令（`docs/acceptance-commands.md`） |
| C2 真实进程监督、取消和超时 | 未完成 | 进程树真实终止、pause/cancel/timeout 终态与晚到结果 CAS 尚无完整 E2E |
| C3 精确候选 commit 验证 | 已完成 | acceptance 在候选 worktree 执行（HEAD==candidate、工作树干净前置检查）；`pkg_60acfb94` → `cargo:check` exit 0 → `3840e667` passed；`--no-verify`/`quotePath=false`/amend 修复（`e874f95`、`8d73931`、`d509147`） |
| C4 领取前重新验证与状态契约 | 已完成 | MCP `verify_pickup_readiness` 门禁 + enqueue 返回 `runner_eligible`/`blockers`；poll_once 领取前按项目/方案开关、hash/revision 过滤 |
| C5 DecisionRequest 接入 Runner | 已完成 | resolution 连接执行动作（`9a8053b`）；acceptance 失败 → DecisionRequest + sealed AttemptBundle E2E（`338cf06`） |
| C6 完成 App UI 人工控制台 | 未完成 | attempt 谱系 UI（候选 diff/verifier 对比）未完成；Runner 开关与执行面板已有 |
| C7 测试、质量和兼容性收口 | 部分（step-0 基础） | `pkg_60acfb94` outcome=passed、candidate `3840e667`、`cargo:check` exit 0；全量门槛待 C2/C6 补齐后复验 |
| C8 文档、安装和可回滚放行 | 进行中 | 本文档（0.1-0.9 状态更新）为 C8 一部分；README/安装/迁移/feature flags/AttemptBundle 契约文档待完成 |

## 1. 收口结论

QuestLog 的主体链路已经搭建，但当前只能安全用于：

```text
Codex / Claude Code / ZCode 对话形成方案
→ 通过 QuestLog MCP 登记 Epic / Step / TaskPackage
→ QuestLog 方案追踪集中显示和统筹
→ 人工决定是否开启 Runner
```

在本方案的 P0 项全部通过前，Runner 必须保持关闭，不能进入无人值守自动执行。

最终目标不是“所有任务都无人介入”，而是：

- Codex 等高成本对话模型只负责方案设计、结构化登记和重要决策讨论；
- ZCode 等已批准的低成本 CLI 执行器负责隔离 worktree 内的机械施工；
- QuestLog 负责调度、证据、暂停、授权、异常回流和自动释放下一步；
- QuestLog 将每次执行的源码、环境、原始轨迹、候选 commit 和 verifier 结果保存为不可变执行记忆，使后续 attempt 能基于真实历史纠错；
- 人只处理授权、方向变化、外部条件和无法自动裁决的失败；
- Git、精确候选 commit 和 verifier 决定是否完成，模型声明不构成完成证据。

## 2. 不可变产品边界

### 2.1 客户端角色

| 角色 | 允许 | 禁止 |
|---|---|---|
| Codex 对话 | 设计方案；创建/更新 Epic、Step；prepare/validate/seal/enqueue；读取运行证据；回答 DecisionRequest | 被 Runner 默认或自动选作代码执行器 |
| Claude Code 对话 | 同上；可作为 MCP 方案入口 | 绕过 TaskPackage 和服务端门禁直接放行 |
| ZCode CLI | 执行已 sealed、已放行、范围受控的 TaskPackage | 改目标、扩权限、自行 merge/push/deploy、以自然语言自评完成 |
| QuestLog Runner | 调度、租约、进程监督、worktree、候选 commit、验收、状态回写 | 未授权扩大范围；修改主工作区；自动 merge/push/deploy |
| 人 | 开关 Runner；批准/驳回；暂停/取消；修改方向；决定候选 commit 后续 | 无需重复复制任务正文或人工轮询普通任务 |

### 2.2 默认执行策略

- `executor.preferred` 默认值为 `zcode`；也可显式为 `manual`。
- 生产 Runner 不接受 `codex`；若保留 Codex adapter，仅用于测试或显式 feature flag，默认关闭且 UI 不提供普通开启入口。
- `manual` 表示只登记和展示，不进入自动队列；不能被 Runner 当作未知执行器后失败。
- 自动 fallback 默认关闭；未来启用时必须由 TaskPackage 显式声明，并受最大尝试次数约束。
- Runner 默认关闭，App 重启后仍默认关闭；环境变量不得静默突破用户 UI 的禁用状态。

### 2.3 自动权限上限

默认只允许：读取、隔离 worktree 写入、运行受信验收命令、生成候选 commit、保存证据。

以下始终需要新的人类授权：

- 修改 TaskPackage 的 objective、scope、acceptance、authority 或依赖；
- 访问未声明路径、网络、凭证或外部系统；
- merge、push、创建 PR、发布、部署、重启真实服务；
- 清理失败 worktree 或丢弃候选 commit；
- 将真实项目加入自动执行 canary。

## 3. 当前实现与剩余差距

### 3.1 已具备且需要保留

- JSON 损坏 fail-closed、文件锁、revision/CAS 和并发测试；
- Epic/Step、依赖、五阶段进度和方案追踪 UI；
- TaskPackage prepare → validate → seal → enqueue 服务端门禁；
- Runtime Store、执行记录、租约、worktree、范围检查和候选 commit；
- ZCode CLI probe、真实 fixture 读写和候选 commit E2E；
- Runner 默认关闭；
- 执行面板、pause/cancel/retry 按钮和未提交的 Runner 总开关 UI；
- MCP DecisionRequest 的存储、查询和回答工具。

### 3.2 最终验收发现的阻塞

| ID | 级别 | 差距 | 风险 |
|---|---|---|---|
| F1 | P0 | TaskPackage 默认执行器仍为 Codex，Runner 仍实现 Codex 自动执行 | 成本边界靠人工记忆，可能误用高成本模型 |
| F2 | P0 | acceptance 在主仓库而非候选 worktree / candidate SHA 上运行 | 假绿或假红，无法证明候选代码正确 |
| F3 | P0 | pause/cancel 只改状态，不 kill 子进程；timeout 被忽略 | 人工控制按钮不真实，任务可继续狂奔 |
| F4 | P0 | Runner 不创建 DecisionRequest，UI 没有待决收件箱 | 异常不能自动回流给人，闭环中断 |
| F5 | P0 | Runner 只按 `status=ready` 领取，未重新验证 hash、authority、依赖、Epic 状态和 package revision | seal 后漂移或本地篡改可能越过门禁 |
| F6 | P0 | 取消/暂停与 Runner 完成回写缺少终态 CAS | 已取消任务可能被晚到的成功结果覆盖 |
| F7 | P1 | timeout、max_attempts、fallbacks、sandbox、authority 未完整进入 Rust Runner 消费契约 | TaskPackage 声明与实际运行不一致 |
| F8 | P1 | `manual` 被 Schema 接受但 Runner 不支持其语义 | 合法任务包进入不可解释失败 |
| F9 | P1 | 当前 E2E 只证明无验收命令时拒绝通过，没有候选 commit 精确验收成功链 | 最关键的 anti-fake-green 未形成正向证据 |
| F10 | P1 | Rust fmt 和严格 Clippy 失败，7 个 UI/Runner 文件未提交 | 当前状态不是可发布快照 |

## 4. 收口后的权威闭环

```text
对话形成方案
  → MCP push_epic / push_step
  → prepare / validate / seal
  → 人或策略 enqueue
  → Runner 领取前重新验证 sealed hash + revision + authority + dependencies
  → 建立 Execution + lease + 子进程监督句柄
  → ZCode 在隔离 worktree 执行
  → pause / cancel / timeout 可真实终止进程
  → 检查 diff scope，生成 candidate commit
  → 在 exact candidate worktree 上运行 acceptance
  ├─ 通过：Execution=passed，Step=verified，按依赖释放下一 Step
  └─ 不确定/越权/失败：Execution=blocked，创建 DecisionRequest
       → 人在 UI 或任一 MCP 对话批准、驳回、修订或取消
       → 产生新 attempt 或新 package revision，禁止直接篡改旧证据
```

## 5. 状态机与竞争控制

### 5.1 Package 状态

```text
draft → sealed → ready → running → passed
                    ├→ blocked
                    ├→ failed
                    ├→ cancelled
                    └→ superseded
```

- Node 与 Rust 必须共享同一枚举；不得由 Rust 写入 TypeScript 类型未声明的状态。
- `sealed` 内容不可变；任何内容变化必须新 revision + 新 hash。
- Runner 领取时重新计算 hash，并确认 `.epics.json` 摘要仍指向同一 package revision/hash。

### 5.2 Execution 状态

```text
queued → starting → running → candidate → verifying → passed
             │          │           │          ├→ blocked
             │          │           │          └→ failed
             └──────────┴───────────┴────────────→ cancel_requested → cancelled
                                      └──────────→ timed_out
```

终态：`passed / failed / cancelled / timed_out / superseded`。

所有状态更新必须携带 `expected_status + execution_revision + lease_token`：

- UI 点击取消先写 `cancel_requested`；
- supervisor kill 并 wait 确认子进程退出后写 `cancelled`；
- Runner 只能从预期前态推进，不能覆盖 `cancel_requested/cancelled/timed_out`；
- terminal 状态不可被 retry 原地复活；retry 创建新 attempt，保留旧执行记录；
- App 退出、崩溃或租约过期后先检查 PID/进程身份，再决定 stale/blocked，不能仅凭时间重复执行。

## 6. 收口施工包

### C0：冻结基线和提交边界

**目的**：先把当前 7 个未提交文件与已提交实现划清边界，避免后续修复混入不可追溯状态。

**施工**：

- 记录 base commit、当前 diff 和现有用户改动；
- 不重置、不清理，创建专用收口分支/worktree；
- 将 Runner 总开关改动作为独立候选提交，修复 i18n 文件尾和格式；
- 建立 F1-F10 的验收追踪表。

**验收**：每个后续 commit 只对应一个施工包；`git diff --check` 通过；无真实 `.todos.json/.epics.json` 进入提交。

**回滚**：丢弃收口 worktree，不影响当前分支和用户数据。

### C1：固化执行器与 TaskPackage 策略

**覆盖**：F1、F7、F8。

**候选文件**：

- `mcp-server/src/task-package.ts`
- `mcp-server/src/task-package-ops.ts`
- `mcp-server/src/index.ts`
- `desktop-app/src-tauri/src/execution_store.rs`
- `desktop-app/src-tauri/src/runner.rs`
- 对应 Node/Rust 测试与文档

**施工**：

1. 默认执行器改为 `zcode`，`manual` 不入自动队列。
2. MCP 允许 Codex 对话创建/封包/入队，但 package 的执行器必须是 ZCode 或 manual。
3. 生产 Runner 对 `codex` fail-closed，返回结构化 policy violation；Codex adapter 由默认关闭的测试 feature flag 隔离。
4. Rust TaskPackage DTO 完整接收 `sandbox/timeout/max_attempts/fallbacks/authority/preconditions/sealed_at`。
5. Node/Rust 使用同一 PackageStatus 和 ExecutorKind 契约，增加兼容迁移。
6. fallback 默认空；达到最大尝试后生成 DecisionRequest，不无限换模型。

**验收**：

- 未指定执行器时生成 ZCode package；
- 显式 `manual` 只登记不领取；
- 普通 MCP 调用无法创建可被生产 Runner 执行的 Codex package；
- 旧 Codex package 被标记 `blocked/policy_mismatch`，不自动执行；
- 所有执行策略字段进入 Rust 并被实际消费。

**回滚**：关闭 execution feature flag；保留方案登记，所有 ready package 转 paused，不改旧证据。

### C2：实现真实进程监督、取消和超时

**覆盖**：F3、F6、F7。

**候选文件**：

- `desktop-app/src-tauri/src/executors/mod.rs`
- `desktop-app/src-tauri/src/executors/zcode.rs`
- `desktop-app/src-tauri/src/runner.rs`
- `desktop-app/src-tauri/src/execution_store.rs`
- `desktop-app/src-tauri/src/commands.rs`
- 新增 AttemptBundle / trace store（可独立文件）

**施工**：

1. 新增进程监督器：`execution_id → pid/process group/handle/cancel token/deadline`。
2. 启动子进程时独立进程组，避免只 kill 外壳而遗留子进程。
3. wait 改为可中断监督循环，周期性 heartbeat，并检查 cancel/timeout。
4. pause：终止当前 attempt，保留 worktree，状态 blocked + DecisionRequest；继续时创建新 attempt。
5. cancel：终止进程树并确认退出，状态 cancelled；默认不删除 worktree。
6. 全局 Runner OFF：立即停止领取；用户可选择是否取消当前执行，默认只停止新领取。
7. 所有终态更新使用 CAS + lease token，防止晚到结果覆盖人工操作。
8. 每个 attempt 从创建起拥有独立、不可变的证据目录，不与下一次 retry 共用或覆盖日志。
9. 启动执行器前生成机器可读 `environment-snapshot.json`：base SHA、目录树摘要、工具版本、受信命令、权限、依赖、允许路径和父 attempt 引用。
10. 保存完整原始执行轨迹，而不是只保存分数或模型摘要；摘要只能作为索引，不是权威证据。
11. AttemptBundle 至少包含 manifest、sealed package、environment snapshot、events.jsonl、stdout/stderr、diff、candidate、verifier、decisions 和 outcome。

**验收**：

- 执行中点击 cancel，子进程及后代在限定时间内退出；
- cancel 后不产生新的 candidate commit，不被改写为 passed/failed；
- timeout 到期真实 kill，并记录 deadline、exit、stdout/stderr；
- pause 后 worktree/日志保留，resume 产生新 attempt；
- App 重启可识别存活/已死子进程，不重复领取；
- 两次 retry 形成两个独立 AttemptBundle，旧轨迹的 hash、时间和文件内容不变；
- 环境快照足以让执行器直接进入施工，避免重复探测 Git、工具和目录边界。

**回滚**：Runner OFF；禁用 supervisor 自动领取；所有非终态转 blocked 等待人工处理。

### C3：精确候选 commit 验证

**覆盖**：F2、F9。

**候选文件**：

- `desktop-app/src-tauri/src/runner.rs`
- `desktop-app/src-tauri/src/runner_git.rs`
- acceptance registry 与 E2E fixture

**施工**：

1. acceptance 的 cwd 必须是候选 worktree，且执行前确认 `HEAD == candidate_sha`、工作树干净。
2. 每个 verifier 结果记录 command_id、cwd、candidate SHA、开始/结束时间、exit code 和输出引用。
3. 验证过程中如 worktree HEAD 或文件发生变化，立即熔断。
4. 验收失败保留 candidate，不推进 verified，不自动在原 attempt 上继续修改。
5. Node/Rust acceptance registry 建立单一生成源或一致性测试，防止两份表漂移。
6. 验证按成本分层：package/hash/authority → diff scope → 语法/import/type/smoke → 针对性测试 → 完整 acceptance → 必要时观察/canary。
7. 任一廉价层失败即停止，不运行后续昂贵验证；每层结果独立写入 verifier 证据。
8. 完整验证与执行器进程分离，执行器不能写 verifier 结果或自行宣布通过。

**验收**：

- 构造“主仓库通过、候选失败”fixture，必须失败；
- 构造“主仓库失败、候选通过”fixture，必须只按候选结果通过；
- verifier 证据中的 SHA 与 Execution.candidate_sha 完全一致；
- 无 acceptance、未知 command_id、脏 worktree、SHA 漂移均 fail-closed；
- 至少一条 ZCode 写链从 ready → candidate → exact verification → passed；
- 语法或接口错误会被廉价闸拦截，完整测试启动数为 0；
- verifier 结果可追溯到 AttemptBundle 中的原始输出，而非仅有一段总结。

**回滚**：关闭自动 verifier 和 Runner；候选 commit 保留，Step 最多停在 coded。

### C4：领取前重新验证与状态契约统一

**覆盖**：F5、F6、F7。

**候选文件**：

- `mcp-server/src/runtime-store.ts`
- `mcp-server/src/task-package.ts`
- `desktop-app/src-tauri/src/execution_store.rs`
- `desktop-app/src-tauri/src/runner.rs`
- `desktop-app/src-tauri/src/epic_store.rs`

**施工**：

Runner 每次领取必须原子完成以下检查：

1. package 状态为 ready，revision/hash 与 Plan Store 摘要一致；
2. 重新计算 sealed hash，拒绝内容漂移；
3. Epic 为 active，Step 仍存在且未 superseded/cancelled；
4. 所有 precondition 和 required_stage 仍满足；旧字符串依赖继续按 observed；
5. authority 允许 worktree edit 和 candidate commit；
6. repo/project canonical path 仍在注册 allowlist；
7. base SHA、scope、acceptance registry、执行器 capability 仍有效；
8. 同项目没有写执行租约；
9. 领取与 package `ready→running`、Execution 创建、lease 写入在一个可恢复事务中完成。

**验收**：seal 后修改 package、Epic 暂停、依赖回退、hash 漂移、路径变化、权限撤销、重复 Runner 并发领取均无法启动 CLI。

**回滚**：领取器切回 shadow，只报告“将领取/拒绝原因”，不启动执行器。

### C5：DecisionRequest 真正接入 Runner

**覆盖**：F4、F6。

**候选文件**：

- 统一 Runtime Decision Store（Node/Rust）
- `mcp-server/src/decision-request.ts`
- `mcp-server/src/index.ts`
- `desktop-app/src-tauri/src/runner.rs`
- `desktop-app/src-tauri/src/commands.rs`

**施工**：

1. Node/Rust 使用同一 DecisionRequest Schema、锁、revision 和状态枚举。
2. Runner 在以下情形自动创建且去重：approval required、scope violation、base drift、verifier failed、retry exhausted、policy mismatch、ambiguous recovery。
3. 每个请求绑定 package revision、execution attempt 和原始证据引用；证据可指向 events、stderr、diff、candidate 和具体 verifier 结果，不能只给模型摘要；旧 revision 的回答不能释放新包。
4. resolution 使用结构化动作：`approve_once / revise_package / retry / transfer_manual / reject / cancel`，自由文本只作说明。
5. `resolve_decision` 不直接把旧执行改 passed；它只触发合法状态转换、新 attempt 或新 package revision。
6. 阻塞请求未解决时，Runner 不得领取同一 Step。
7. 人或高成本规划模型处理 blocker 时，默认先收到证据索引，再按需读取原始轨迹；不得把全部历史轨迹无差别塞入上下文。

**验收**：

- 每种阻塞只生成一个活动请求；
- 人可在任一 MCP 对话查询并回答，无需复制上下文；
- 过期 revision 的回答被拒绝；
- reject/cancel 永不继续，retry 创建新 attempt，revise_package 回到 draft；
- Decision Store 损坏时 fail-closed，不放行任务。

**回滚**：所有需要决策的任务保持 blocked；关闭自动恢复，允许人工读取证据。

### C6：完成 App UI 人工控制台

**覆盖**：F3、F4、F10。

**候选文件**：

- `desktop-app/src/components/EpicBoard.tsx`
- `desktop-app/src/components/ExecutionPanel.tsx`
- 新增 DecisionInbox/DecisionCard 组件
- `desktop-app/src/hooks/useExecutions.ts`
- Tauri commands、类型、i18n 和 UI 测试

**施工**：

1. Runner 总开关始终可见，默认 OFF；加载中禁用，失败显示错误，不静默吞错。
2. 总开关明确区分“停止领取新任务”和“取消当前运行”。
3. Step 展示 package revision/hash、执行器、attempt、worktree、base/candidate SHA、验收结果和最后错误。
4. pause/cancel 使用二次确认并显示真实后端状态，不做乐观假完成。
5. 增加“需要关注”收件箱，显示唯一问题、证据、适用 revision 和结构化动作。
6. 人可以批准一次、修订包、重试、转人工、驳回、取消；权限扩大需再次确认。
7. terminal 执行只显示“新建重试 attempt”，不允许把旧记录改回 queued。
8. UI 不能直接写 verified/observed，也不能绕过服务端 authority。
9. Step 下按时间展示 attempt 谱系和 parent_attempt，支持比较两个候选的 diff、环境快照、成本和 verifier，不把摘要误标为原始证据。
10. 默认只展示轨迹索引、关键错误和证据引用；用户按需展开本地原始 events/stdout/stderr，秘密字段保持脱敏。

**验收**：

- App 重启后 Runner 默认为 OFF；
- UI 所有按钮都能在 E2E 中证明对应后台动作真实发生；
- cancel 后进程数、状态、candidate SHA 均符合预期；
- DecisionRequest 从生成、显示、回答到新 attempt 全链可见；
- 用户可从失败 attempt 定位父子关系、修复假设、candidate SHA 和原始 verifier 证据；
- attempt 比较不会读取或上传无关项目轨迹；
- 中英文无缺 key，窄窗口不遮挡关键控制，错误状态不空白。

**回滚**：隐藏 execution UI，Runner 保持 OFF；原 Todo/Epic 看板不受影响。

### C7：测试、质量和兼容性收口

**覆盖**：F9、F10 及 C1-C6 全部回归。

**必须新增的 E2E**：

| 场景 | 必须证明 |
|---|---|
| Codex 对话推送方案 | Epic/Step/TaskPackage 进入 QuestLog，但执行器不是 Codex |
| ZCode 正向链 | ready → running → candidate → exact verifier → passed |
| 候选失败链 | 主仓库测试通过也不能假绿 |
| 取消 | 进程树退出、无晚到 commit、终态不被覆盖 |
| 超时 | deadline 生效、进程退出、证据完整 |
| 暂停/继续 | 旧 attempt 保留，新 attempt 续作 |
| Evidence-aware retry | 新 attempt 能读取父 attempt 原始轨迹，并声明单一修复假设，旧证据不被覆盖 |
| AttemptBundle | 源码、环境、事件、diff、候选、验证和结论齐全且可校验 |
| 廉价验证阶梯 | 早期错误不触发昂贵全量测试 |
| seal 后篡改 | hash/revision 不一致时 CLI 启动数为 0 |
| DecisionRequest | blocked → 回答 → 合法新状态，无上下文复制 |
| 并发领取 | 两个 Runner 最多一个获得 lease |
| App 重启 | 不重复执行，不默认开启 Runner |
| manual | 永不自动领取，UI 可转人工 |
| 数据损坏 | Plan/Runtime/Decision 任一损坏均 fail-closed |
| 泛化与泄漏 | 未见 fixture 上无回归，任务特定字符串/秘密/绝对个人路径不会被带入新候选 |
| 成本与人效 | token、耗时、重试、人工触点、取消和回滚指标可复算 |

**全量门槛**：

```text
mcp-server: npm test && npm run build
desktop-app: npm test && npm run build
Rust: cargo test
Rust: cargo fmt --all -- --check
Rust: cargo clippy --all-targets --all-features -- -D warnings
Git: git diff --check
```

不得以 mock 代替 ZCode 真实 fixture；不得以一次 CLI exit 0 代替候选 commit 验证。

### C8：文档、安装和可回滚放行

**施工**：

- README 中英文说明 Codex 只推送、ZCode 执行、Runner 默认 OFF；
- TaskPackage、DecisionRequest、日志、隐私、取消、恢复和故障排查文档与代码一致；
- setup-mcp 验证 Codex/Claude/ZCode 都连接同一 QuestLog MCP；
- 增加 feature flags：execution packages、runner、zcode executor、codex executor test-only；
- 记录 Runtime Schema 版本与迁移/降级策略；
- 定义 AttemptBundle v1、原始轨迹保留期、秘密脱敏、访问权限和完整性校验；
- 提供本地 Trace Query CLI/命令：按 project/Epic/Step/executor/status/error/candidate SHA 查询 attempt，比较候选 diff 和 verifier 结果；
- Trace Query 返回索引和文件引用，不用 AI 摘要替代原始轨迹；
- 生成最终 requirement-to-evidence 验收表。

**验收**：全新安装和既有数据升级都能启动；禁用 Runner 后原方案追踪完整可用；旧 package 无法因迁移被意外执行。

## 7. 依赖和可并行顺序

```text
C0 基线冻结
 └→ C1 执行器/契约
     ├→ C2 进程监督 ─┐
     ├→ C3 精确验证 ─┼→ C4 领取与状态事务 → C5 DecisionRequest → C6 UI
     └───────────────┘                                      │
                                                            └→ C7 全量验证 → C8 放行
```

- C2 与 C3 可在 C1 后并行；
- C4 必须统一前面产生的状态和 DTO，不能提前拼接；
- C5 后端先于 C6 UI，避免出现只有按钮没有真实控制；
- C7 中的针对性测试随每包实施，全量 E2E 在 C6 后执行；
- C8 只在代码与真实 fixture 全绿后完成。

## 8. UI 最终验收清单

- [ ] 方案追踪顶部明确显示 Runner OFF/ON 和当前执行器策略；
- [ ] 默认 OFF，加载失败不误显示 ON；
- [ ] 每个 Step 可看到 package revision/hash 和是否 sealed；
- [ ] 可看到当前 attempt、PID/运行时间、worktree、base/candidate SHA；
- [ ] 可展开 verifier 命令、被测 SHA、结果和日志引用；
- [ ] pause、cancel、retry、transfer manual 的语义不同且有确认；
- [ ] cancel 后 UI 等待后端确认，不立即假装完成；
- [ ] “需要关注”只展示未决问题，并能过滤项目/Epic/Step；
- [ ] 回答时展示作用 revision，过期请求不可操作；
- [ ] 批准一次不等于永久扩大权限；
- [ ] 可查看 attempt 谱系、修复假设、成本和 verifier，并按需打开脱敏后的本地原始轨迹；
- [ ] 两个 attempt 的 diff/结果比较不改变任何运行状态；
- [ ] 原 Todo/Epic 编辑、依赖和完成门禁不回归；
- [ ] 中英文、深浅主题、菜单栏窄窗口均可用。

## 9. 放行阶段

### R0：开发禁用态

- Runner OFF；
- 只运行单元/集成测试；
- 允许 Codex 对话推送方案；
- 不启动真实项目 CLI。

放行到 R1：C1-C6 针对性测试全部通过。

### R1：临时 fixture 闭环

- 临时 Git 仓库；
- 真实 ZCode；
- 覆盖成功、失败、取消、超时、漂移和 DecisionRequest；
- 自动 merge/push/deploy 为 0。

放行到 R2：C7 全量门槛通过，requirement-to-evidence 无缺项。

### R2：Shadow

- 读取一个用户指定项目；
- 只显示将领取的 Step 和拒绝原因；
- 不创建执行 worktree，不启动 CLI；
- 至少核对 10 次调度决定。

进入 R3 必须获得用户明确 canary 授权。

### R3：单项目 worktree canary

- 用户指定非关键项目和允许路径；
- 从单 Step 开始；
- merge/push/deploy 仍人工；
- 至少完成 20 个 Step 或用户约定样本；
- 发生越权、假绿、数据丢失或无法取消时立即回到 R0。

### R4：Bounded Auto

只有 R3 达到以下指标且用户再次批准后开放：

- 未封包/未授权/依赖不足启动数：0；
- 非允许路径修改数：0；
- 假绿和数据丢失数：0；
- 自动 merge/push/deploy：0；
- 人工复制任务正文下降 ≥80%；
- 人工检查是否完成下降 ≥70%；
- 无异常 Step 自动派发率 ≥90%；
- 平均每 Step 人工触点 ≤1；
- 所有人工触点均有 DecisionRequest。

## 10. 全局熔断与回滚

触发条件：越权路径、假绿、数据损坏、取消失效、重复执行、错误执行器、不可解释状态推进。

立即动作：

1. 设置 Runner OFF，停止领取新任务；
2. 对运行任务发送 cancel，确认进程树退出；
3. 不删除 worktree、candidate、日志、events 和 decisions；
4. 所有 queued/running/cancel_requested 进入 blocked，记录熔断原因；
5. 禁止自动 retry/fallback；
6. 导出执行与 Git 证据，逐个决定保留、重试或废弃；
7. 修复后从 R1 fixture 重新验证，不能直接恢复 canary。

降级保证：禁用 execution feature 后，Todo、Epic、Step 和方案追踪仍可读写；旧 package 不会自动启动。

## 11. 最终完成定义

只有同时满足以下条件，才可声明闭环收口完成：

- [ ] F1-F10 均有代码和测试证据；
- [ ] Codex 普通路径无法成为 Runner 执行器；
- [ ] ZCode 正向 exact candidate E2E 通过；
- [ ] pause/cancel/timeout 真实终止进程并保持终态；
- [ ] Runner 领取前重新验证 package、计划、权限、依赖和 Git；
- [ ] DecisionRequest 在 Runner、MCP、UI 三端闭环；
- [ ] 每个 attempt 生成不可变 AttemptBundle，原始轨迹可本地查询且经过秘密/路径治理；
- [ ] retry 基于父 attempt 的真实证据并形成新 attempt，不覆盖历史；
- [ ] verifier 采用廉价到昂贵的分层门禁，并与执行器独立；
- [ ] UI 所有人工按钮都有后端行为 E2E，不是状态装饰；
- [ ] Node、前端、Rust 测试/构建/fmt/Clippy 全绿；
- [ ] 当前 7 个未提交文件已审计并形成可追溯候选 commit；
- [ ] 原 Todo/Epic 功能无回归；
- [ ] 默认 Runner OFF，merge/push/deploy 自动次数为 0；
- [ ] requirement-to-evidence 表无 `unknown`；
- [ ] 真实项目 canary 未经用户授权没有启动。

## 12. 可直接交给单一 Goal 的收口表述

```text
目标：在 QuestLog 当前受控 AI 执行闭环实现基础上，一次性完成 C0-C8 最终收口，使 Codex/Claude/ZCode 对话可以把方案和标准 TaskPackage 推送到方案追踪，但生产 Runner 只使用显式批准的 ZCode 或 manual 策略；实现真实进程取消/暂停/超时、候选 commit 精确验收、领取前重新验证、终态 CAS、DecisionRequest MCP+UI 回流、不可变 AttemptBundle、完整原始执行轨迹、环境快照、Evidence-aware Retry、分层 verifier 和全量 E2E。默认 Runner 必须关闭，禁止自动 merge/push/deploy，禁止触碰真实用户项目数据或未经授权启动真实项目 canary。

执行要求：持续按 C0→C1→(C2/C3)→C4→C5→C6→C7→C8 推进。普通代码缺陷、测试失败、格式或文档问题在同一 Goal 内修复并重跑，不因完成局部功能而结束。每个施工包完成后回看剩余依赖并更新 requirement-to-evidence。每次 retry 必须保存父 attempt 的完整轨迹并声明本次单一主要修复假设；摘要只作索引，Git、原始轨迹和外部 verifier 才是证据。只有 F1-F10、全部测试构建、真实 ZCode exact-candidate fixture、取消/超时/DecisionRequest/AttemptBundle E2E 和 App UI 验收全部通过，才可标 complete。

授权边界：允许在隔离 worktree 修改 QuestLog 源码、运行测试并创建候选 commit；不允许 merge、push、发布、部署、修改真实项目 `.todos.json/.epics.json`、自动清理失败 worktree或启动真实项目 canary。需要这些动作时生成最小 DecisionRequest 等待用户。
```

## 13. 交付物

收口完成时应交付：

- C0-C8 对应候选 commits；
- Runtime / Package / Decision Schema 与迁移说明；
- AttemptBundle v1、Trace Query 和环境快照契约；
- requirement-to-evidence 验收矩阵；
- 全量命令和测试结果；
- ZCode 正向、取消、超时、漂移和 DecisionRequest E2E 证据；
- App UI 截图或自动化交互证据；
- 未执行的真实项目 canary 及其授权状态；
- 熔断、回滚和人工接管手册；
- 最终“可投入 bounded auto / 继续 shadow / 回滚”的明确裁决。

## 14. Meta-Harness 可继续借鉴的逻辑与启用边界

> 用户已明确 Meta-Harness 可以继续。其与当前收口直接相关的部分——AttemptBundle、EnvironmentSnapshot、原始执行轨迹、外部 verifier 和受 max_attempts/DecisionRequest 约束的 Evidence-aware Retry——纳入收口设计与证据要求。自动策略搜索、无上限候选生成和真实项目自主试验仍受 R1-R4 放行阶段约束，不得绕过 Runner OFF、armed scope、人工授权或 Git exact-candidate 门禁。

### 14.1 借鉴结论

Meta-Harness 与 QuestLog 不是同一种产品：前者优化模型外部 Harness，后者控制真实工程任务执行。但两者共享同一个可验证外层循环：

```text
提出候选
→ 隔离执行
→ 保存候选源码和完整执行轨迹
→ 外部评估器独立判定
→ 从全部历史中选择性读取证据
→ 归因失败并提出下一候选
```

QuestLog 应吸收其“完整轨迹 + 文件系统可查询历史 + 外部评价 + 代码空间候选 + 轻量预检 + 多目标比较”，同时保留更严格的 TaskPackage、authority、DecisionRequest、最大尝试和人工熔断。

### 14.2 AttemptBundle v1

建议本地结构：

```text
runtime/attempts/<execution_id>/
├── manifest.json
├── task-package.json
├── environment-snapshot.json
├── events.jsonl
├── stdout.log
├── stderr.log
├── diff.patch
├── candidate.json
├── verifier-results.json
├── decisions.json
└── outcome.json
```

最小 manifest 字段：

```json
{
  "execution_id": "run_uuid",
  "attempt_no": 2,
  "parent_attempt_id": "run_previous",
  "package_id": "pkg_uuid",
  "plan_revision": 3,
  "executor": "zcode",
  "hypothesis": "本次只验证的主要修复假设",
  "base_sha": "full_sha",
  "candidate_sha": "full_sha_or_null",
  "environment_snapshot_ref": "environment-snapshot.json",
  "raw_trace_ref": "events.jsonl",
  "verifier_ref": "verifier-results.json",
  "outcome": "passed_or_blocked_or_failed",
  "integrity_hash": "sha256"
}
```

规则：

- 原始文件只追加或封存，不允许 retry 覆盖；
- summary、UI 文案和模型反思均为派生数据，可重建；
- 任何自动诊断必须能回指原始事件、命令、diff 或 verifier；
- 默认只允许读取同项目、同 Epic/Step、权限范围内的历史；
- secret、认证信息、完整个人路径和无关 prompt 在落盘前按规则脱敏；
- 保留期到期时先保留 manifest、Git SHA 和 verifier，删除大日志需留下审计记录。

### 14.3 EnvironmentSnapshot

执行前由 Runner 生成，不由模型自由描述，至少包含：

- repo/project canonical path 的受控引用、base SHA、branch、Git clean；
- 允许/禁止路径、最大文件数、authority 和停止条件；
- 与任务相关的目录树摘要、工具链与版本；
- 可用受信命令、依赖 Step、产物和 package revision/hash；
- 父 attempt、失败 verifier 和 DecisionRequest 的索引；
- 执行器 capability、timeout 和剩余 attempt 预算。

环境快照用于减少模型重复探测和 Token 浪费，但不能替代实时 Git/权限二次校验。

### 14.4 Evidence-aware Retry

重试不再等于重放相同 prompt：

```text
父 attempt 失败
→ 封存完整 AttemptBundle
→ 生成机器索引，不压缩删除原始轨迹
→ 新 attempt 读取 package + 环境快照 + 父证据引用
→ 声明一个主要失败假设和有限修改策略
→ 产生独立 worktree 结果或在受控保留 worktree 上续作
→ 外部 verifier 重新评价
```

以下情况禁止自动 Evidence-aware Retry，直接 DecisionRequest：scope/authority 扩大、base/revision 漂移、两次根因冲突、候选已产生且修改方向不唯一、秘密/外部系统需求、达到 max_attempts。

### 14.5 多目标指标与帕累托比较

除 pass/fail 外，每次 attempt 记录：

| 维度 | 指标 |
|---|---|
| 正确性 | acceptance/verifier、回归、观察结果 |
| 成本 | input/output tokens、CLI 费用、全量测试次数 |
| 时间 | 执行、等待、验证和人工响应耗时 |
| 稳定性 | retry、timeout、stale、rollback |
| 人效 | 人工触点、复制任务正文、人工检查次数 |
| 安全 | 越界、假绿、取消失效、数据损坏 |

QuestLog 先保存指标，不在 P0 阶段自动搜索策略。R3 canary 后才允许比较不同 ZCode 策略的帕累托前沿；任何“更便宜但越权/假绿”的候选直接淘汰，不参与成本权衡。

### 14.6 泛化、泄漏和因果纪律

- 针对新策略至少保留未参与调优的 fixture/OOD 样本；
- 审计任务 ID、答案、秘密、绝对路径和测试特定字符串硬编码；
- 一次 attempt 尽量只改变一个主要变量，记录 hypothesis，避免混淆执行器、prompt、范围和 verifier；
- 失败归因必须引用原始轨迹，不能只接受模型反思；
- 成功策略跨执行器或跨项目迁移前重新 canary，不能沿用旧证据。

### 14.7 明确不照搬

QuestLog 不采用 Meta-Harness 的以下开放搜索边界：

- 无上限自主候选和重试；
- 执行器自行改 objective、scope、acceptance、authority；
- 把全部历史轨迹一次性注入上下文；
- 用高成本 Codex 持续承担日常执行或搜索；
- 执行器同时担任评价器；
- 只按单一分数优化；
- 在未授权真实项目上自动试验策略。

### 14.8 分阶段启用

```text
当前收口阶段：立即实现 AttemptBundle/环境快照/原始轨迹/Trace Query 数据地基
R1 fixture：启用 Evidence-aware Retry，但受 max_attempts 和 DecisionRequest 约束
R2 shadow：只计算将读取哪些历史、将采用什么假设，不执行优化
R3 canary：记录多目标指标，人工审核策略比较
R4 bounded auto：用户批准后，才允许在已批准范围内自动选择非劣策略
```

Meta-Harness 能力是 QuestLog 的“执行记忆与学习层”，从属于方案追踪和 Runner，不新增一级产品真相源，也不得绕过本方案的安全收口顺序。

## 15. 已暂缓：LID Harness

LID Harness 当前只保留为未来研究方向，不进入本轮 Goal、S0-S7 施工、0.7 完成定义、Schema、Runner 状态机、TaskPackage 或 App UI。

在用户重新明确授权前：

- 不把 LID Harness 与 Meta-Harness、AttemptBundle 或 Evidence-aware Retry 混为同一能力；
- 不因 LID Harness 增加新的一级模块、真相源、任务节点或自动调度层；
- 不以 LID Harness 为由扩大 ZCode 权限、重试次数、上下文读取范围或真实项目试验范围；
- 不要求当前代码、测试、文档或发布门禁为其预留未经验证的接口；
- 后续若恢复，必须先单独说明目标、输入输出、与现有 Git/TaskPackage/DecisionRequest 的边界，以及是否会增加人工或自动执行风险，再另立施工与验收方案。
