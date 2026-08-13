# G4 Canary · S1 Shadow 观察记录

> 日期：2026-08-11
> 项目：ymos-投资系统（用户授权，G4 指定项目）
> 授权记录：epic_0d8618ab links_note（2026-08-11）

## 观察目标

验证 Runner 对 ymos 项目的识别与过滤行为，确认 shadow 模式下 ymos 不会被误领取、不会被触碰。**本阶段不实际执行任何任务。**

## 基线事实

| 项 | 值 |
|---|---|
| ymos 仓库 HEAD | `98452d90` |
| ymos 主仓状态 | **不干净**（有未提交用户工作：.epics.json、SOP 文档等 5+ 文件改动）→ 绝不能碰 |
| ymos epic/step | 16 epics / 90 steps（真实活跃项目） |
| runner-config 中 ymos | `{"enabled": false}`（默认安全） |
| questlog-runtime packages.json 中 ymos 包 | 0 个（无 ready 包） |
| ymos runtime 归属 | 共用 `~/questlog-runtime`（无独立 runtime） |

## 过滤链路验证（三层防护）

```
第一层：packages.json 过滤 —— ymos 无任何包（0 ready）→ shadow_pick 返回空
第二层：runner-config 项目开关 —— ymos disabled → 即使有 ready 包也被 poll_once 跳过
第三层：git_clean 前置 —— ymos 主仓不干净 → 即使封包也无法通过 validate（error）
```

Runner 对 ymos 的可见性 = 0，可执行性 = 0。**当前 ymos 绝对安全。**

## shadow_pick 行为确认

- `Runner::shadow_pick()`（runner.rs:916）只读取 ready 包列表，不启动任何 CLI、不创建 worktree、不写 executions
- 当前全局 ready 包 = 0（本会话最后 5 个包均已 passed/consumed）

## 观察结论

S1 通过。安全基线成立：**ymos 当前完全不可被 Runner 触碰**。进入 S2 前需要：

1. 在 ymos 项目下注册 Canary 专用 epic + step（只读任务）
2. 封包时 allowed_paths 限定为只读观察路径（如 `InfoFlow/**` 只读或特定文档）
3. sandbox 用 `read-only`（runner.rs:931 已支持 read-only sandbox）
4. 执行在隔离 worktree（从 HEAD 98452d9 创建），主仓任何文件不写入

## 下一步（S2 read-only 链）

- 在 ymos 注册一个只读 step：如"读取并总结 InfoFlow/ 下某个文档的结构"
- 封包：allowed_paths 限定只读路径，executor=zcode，sandbox=read-only
- 验证：ZCode 在 ymos worktree 中只读执行、无 diff 产生、候选 commit 不创建（NoDiff 处理）、验收通过

---

## 更新（2026-08-11，S1.1 后追加）

### Canary 暴露的 3 个真实发现（全部已处理或已记录）

**发现 1：ymos 误删除 60+ Brain/ 文件（事故，已复原）**
- 用户确认是误删除。`git checkout -- .` 全部复原，Brain/ 目录完整还原（SOP×6、references 全量、ymos-diagnosis、复盘档案）。
- 教训：**未经用户确认前，绝不对真实项目工作区执行恢复/清理/提交操作**。发现大量删除时要立即停下询问。

**发现 2：git_clean 门禁把 untracked 判为脏（已修复）**
- ymos 有 57 个有意保留的 untracked 工作产物（_Manager 方案、.zcode/plans）——这是活跃项目常态。
- 修复：Node `isGitClean` + Rust `is_worktree_clean` 忽略 `??` 行，只检查 tracked 修改（commit fb45c03，封包执行，4 验收全过）。
- ymos .gitignore 补充 QuestLog 产物（.task-packages/、.epics.json.bak*），已提交（ymos commit 1b5f687）。

**发现 3：MCP epicCwd 忽略 project_dir 参数（已修复）**
- 全部 10 个 epic 工具声明 project_dir 但路由时完全忽略，跨项目注册静默落到 cwd 项目（Canary S2 epic 被写到桌面清单贴）。
- 修复：epicCwd 优先使用显式 project_dir（commit 3d6d028，回归测试 9/9 通过）。

**发现 4（已解决）：活跃项目工作区常态不干净，与 git_clean 硬门禁不兼容**
- ymos 自动机持续回写 tracked 文件（writeback_review.json、s1-observation-log.jsonl），且**有意提交**（"data(自动): 自动机产出同步" 是其常规提交模式）。
- .gitignore 无法解决已跟踪文件的持续修改；等待同步窗口不可靠。
- 架构修正（2026-08-11，用户拍板）：**标准执行包是独立载体（.task-packages/），代码修改发生在隔离 worktree（从 base_sha 创建，天然干净）；主工作区脏不脏与执行无关**。据此：
  - `prepareTaskPackage` 不再自动注入 `git_clean` precondition（默认 `[]`）
  - `validateTaskPackage` 只在包**显式声明** `git_clean` precondition 时才检查主工作区
  - Rust `validate_before_claim` 移除主工作区 `is_worktree_clean` 检查；base_sha drift 检查保留（语义正确性，非"干净"检查）
  - 快照记录（EnvironmentSnapshot 的 git_clean 字段）保留为信息性数据，非门禁
  - 回归测试反转："脏树默认可 seal" + "声明 git_clean 才拒绝"

### 暂停状态

- S1 shadow 观察：✅ 完成（三层防护确认）
- S1.1 git_clean 修复：✅ 完成（已合并 fb45c03）
- S1.2 MCP project_dir 修复：✅ 完成（已合并 3d6d028）
- S1.3 git_clean 架构修正（声明式 precondition）：✅ 完成（移除主工作区门禁）
- S2 read-only：✅ 解锁（ymos 工作区常态脏不再阻塞；待实际执行）
- S3/S4/S5：⏸ 待 S2 完成

### 待决策项（下次收口）

1. ~~git_clean 门禁改为"按 preconditions 声明检查"~~ ✅ 已执行（S1.3）
2. 或为真实活跃项目提供"允许运行时回写窗口"策略 —— 已不再需要（门禁已移除）
3. 或 Canary 改用影子 clone 仓库 —— 已不再需要（worktree 天然隔离）

---

## 更新（2026-08-11，S2 read-only 链完成后追加）

### S2 执行记录（真实 read-only 包，全链路通过）

**包**：`pkg_f1dfe3dd-5e5a-4460-997d-f1c7e51326ed`（step_188b19f9 "S2-1 只读总结 Brain/SOP_初始调研.md"）
**链**：prepare（新 dist，声明式 git_clean）→ 手动调 sandbox=read-only → validate **valid=true（ymos 脏工作区不阻塞）** → seal rev2 → enqueue runner_eligible=true → **Runner 领取 → ZCode 在 worktree 只读执行（完整中文总结，exit 0）→ 无 diff → NoDiff → package passed**

**证据**：
| 项 | 值 |
|---|---|
| execution | exec_78fba69b status=**passed**，无候选 commit |
| bundle outcome | **no_diff**（stdout 含完整 SOP 总结，outcome.json 落盘） |
| step stage | ymos .epics.json step_188b19f9 → **verified**（Runner 自动推进） |
| ymos 主工作区 | 零触碰（与执行前基线一致，无新增改动；Brain/ 未动） |
| worktree | 临时目录自动清理（temp_dir 下，随执行结束移除） |

**首次尝试暴露缺陷 → 修复**（pkg_4e6e7456 被旧 Runner 判 blocked）：
- **发现 5：NoDiff 语义缺陷**——Runner 把 NoDiff 一律视为"未产出候选"的失败（blocked + DecisionRequest）；但 **read-only 任务无 diff 是预期成功**（产出是 stdout 报告本身）。
- 修复（runner.rs poll_once NoDiff 分支）：`sandbox == "read-only"` 时 NoDiff → Passed（finish Passed + package passed + step Verified，无候选 commit）；`workspace-write` 维持原 blocked 语义。
- **回归测试**：`e2e_bridge.rs` 新增 `bridge_readonly_package_no_diff_counts_as_passed`（真实 ZCode 只读执行 → 断言 Passed + bundle outcome=no_diff + package passed + step verified + 主仓 untouched）。全量 cargo test 127+3 通过。
- **附带修复 e2e 基建**：① 测试直接写 runner-config.json 替代 set_project_enabled（避免启动后台 Runner 线程与显式 poll_once 竞争抢包）；② 两个 e2e 测试同进程并行竞争 QUESTLOG_RUNTIME env → 文件级 RUNTIME_LOCK 串行化。
- **部署**：重建 release bundle + 重启 App（新 Runner 逻辑生效，旧逻辑验证了 S1.3 前行为）。

### S2 结论

✅ **S2 read-only 链通过**：真实活跃项目（ymos，工作区常态脏）在 S1.3 修正后可以封包、被领取、在隔离 worktree 只读执行、NoDiff 正确判为成功、step 自动推进。**主工作区全程零触碰**——Canary 安全边界保持。

### 下一步（S3 单 Step write）

- 在 ymos 注册一个 write step（如"向某个明确允许的文件追加一行记录"）
- 封包：sandbox=workspace-write，allowed_paths 限定该文件，executor=zcode
- 验证：ZCode 在 worktree 写入 → 候选 commit 创建（QuestLog trailers）→ 验收通过 → 主仓零触碰、worktree 清理

---

## 更新（2026-08-11，S3 单 Step write 完成后追加）

### S3 执行记录（真实 write 包，全链路通过）

**包**：`pkg_6a7eae9a-dcee-43fc-a073-a612116dd282`（step_a99ef02f "S3-1 单 Step write 探针"）
**链**：prepare → validate（ymos 脏工作区不阻塞）→ seal rev2 → enqueue runner_eligible=true → **Runner 领取 → ZCode 在隔离 worktree 创建 `Canary_write_probe.md` → diff → 候选 commit → verify:candidate 验收 → passed**

**证据**：
| 项 | 值 |
|---|---|
| execution | exec_f04ddf4f status=**passed**，candidate_sha=`d9a0ac5069` |
| 候选 commit | `d9a0ac5 QuestLog auto-execution`（ymos-manager，**仅新增 Canary_write_probe.md 3 行**，scope 精确） |
| verifier | verify:candidate 在 worktree 跑 `git diff --check HEAD^ HEAD`，passed |
| bundle outcome | **passed**（verifier-results.json 绑定候选 SHA） |
| step stage | ymos .epics.json step_a99ef02f → **verified**（Runner 自动推进） |
| ymos 主仓 | **零触碰**：Canary_write_probe.md 不存在于主仓，git status 无 probe 痕迹；HEAD 未动（候选 commit 只在 worktree 分支 questlog/exec_*） |

### 附带修复（decision 残留尾巴）

- **发现 6：read-only NoDiff 成功但残留无意义 DecisionRequest**——seal_bundle 的 decision 生成对 NoDiff 无条件生成（retry/revise/cancel），与 poll_once 的 read-only NoDiff→Passed 判定不一致。
- 修复：decision 生成处同样按 `pkg.sandbox == "read-only"` 跳过 NoDiff 决策；CliFailed/Timeout 维持原逻辑。全量 cargo test 127+3 通过，重建 release bundle + 重启 App。
- 2 个历史 DecisionRequest（dec_d08fae07 / dec_2c620714）已 resolve cancel（对应包均已 passed/废弃）。

### S3 结论

✅ **S3 单 Step write 通过**：真实项目（ymos）中 write 链完整跑通——worktree 写入 → 候选 commit（仅限定文件）→ 验收 → passed → step 自动推进 verified。**主仓全程零触碰，候选 commit 不 merge**。

### 下一步（S4 多 Step 串行）

- 在 ymos Canary epic 下注册 2-3 个 write step（各自独立的探针文件，有 order 依赖）
- 验证：顺序领取、前一步 passed 后下一步才被领取（依赖 gate）、各自独立候选 commit、主仓全程零触碰

---

## 更新（2026-08-11，S4 多 Step 串行完成后追加）

### S4 执行记录（3 个 write step 串行，全链路通过）

**步骤**：S4-1 Canary_probe_1.md → S4-2 Canary_probe_2.md → S4-3 Canary_probe_3.md（同 epic，隐式 order 串行）

**派发方式**：逐个派发——前一步到达 passed 终态后才 enqueue 下一步（避免全量 ready 时后步被 poll 提前领取触发 PreflightBlocked 噪音决策）。

**证据**：
| 步骤 | 包 | execution | 候选 commit | step stage |
|---|---|---|---|---|
| S4-1 | pkg_526e02f9 | exec_7a1e3042 passed | `bb801d95`（仅 Canary_probe_1.md +2） | verified |
| S4-2 | pkg_12e082af | exec_931dc001 passed | `92068784`（仅 Canary_probe_2.md +2） | verified |
| S4-3 | pkg_a78e04e6 | exec_66204ceb passed | `885134b4`（仅 Canary_probe_3.md +2） | verified |

- 全部 base_sha=1b5f6876（ymos HEAD），全部 enqueue runner_eligible=true、blockers=[]，verify:candidate 验收通过
- **主仓零触碰**：3 个 probe 文件均不存在于主仓，HEAD 未动；候选 commit 全部留在独立 worktree 分支

**确认的隔离属性**：每步 worktree 从 base_sha 创建，**前一步的候选 commit 对新 worktree 不可见**——每步候选严格独立（这正是"不 merge、零污染"的设计；若后续需要步骤间承接，须先合并候选或重设 base）。

**依赖 gate 语义（代码确认，runner.rs:1101-1134）**：
- 隐式 order 前置（order < step.order）→ 前置 ≥ **coded** 才可领取（自动链可跑通）
- 显式 depends_on → 前置 ≥ **observed** 才可领取（跨会话人工确认）

### S4 结论

✅ **S4 多 Step 串行通过**：3 个 write 步骤顺序完成、各自独立候选 commit、自动推进 verified、主仓全程零触碰。串行派发 + 依赖 gate 语义清晰可复现。

### S6（追加）：开关即授权 — sealed 包可直接领取（2026-08-13）

消灭"开开关仍待命"的最后一块拼图。改动前：sealed 包必须由对话端 `enqueue_step` 翻成 ready，Runner 才领取；App 里的方案 ▶/⏸/⇆ 开关只写授权字段，不触发。改动后：

- `read_ready_packages` 领取过滤器放宽为 sealed+ready——**开关 ON + 包 sealed = 下轮 poll 直接领取**（validate_before_claim 全量校验不变，含 hash/base_sha/依赖/authority）
- **quiet-skip 双 gate**（claim 前）：manual 执行器包永远不 claim（tracking-only）；依赖未就绪（order 前置<coded、显式 depends_on<observed、step_stage precondition 未达）→ 安静等待保持 sealed，不再被 claim 后误标 blocked
- **授权收紧（对抗审查发现）**：`is_epic_auto_execute` 的 `auto_execute=None` 分支从"继承项目开关"改为 fail-closed——项目开关 ON 不再批量放行所有 sealed 包，必须给方案显式 ▶。MCP `verify_pickup_readiness` 同步镜像
- **状态显示拆分**：heartbeat `ready_packages`（已入队）与 `sealed_packages`（已封包待领取）分开，UI 新增"📦 已封包·N"档位
- 测试：140 Rust + 145 MCP 全绿；新增 sealed 领取/manual 跳过/依赖等待/fail-closed 四组测试

**注意**：上线时 questlog-runtime 有 16 个存量 sealed 包（3 个 manual）——双开关 OFF + fail-closed 双重保护下不会误跑；真实 canary 验证（开一个开关跑通一个 sealed 包）留待用户授权后执行。

### 下一步（S5 观察裁决与演练）

- 汇总 S2-S4 全链路证据，形成 Canary 裁决（扩大范围 vs 保持单项目）
- 观察期约定：G4 收口前在 ymos 持续观察，确认无副作用
- 可选项：多 Step 并行（同 epic 无依赖步骤同时 in-flight）作为 S4 的补充验证

---

## 更新（2026-08-11，S5 观察裁决与演练完成后追加）

### S5 并行演练（跨 epic 并发包）

**设计**：Runner 单线程串行处理包；同 epic 有隐式 order 依赖无法并行。用**两个独立 epic**（S5-P1/P2）各 1 个 write step，back-to-back enqueue。

**证据**：
- pkg_2fd85ee7（P1）→ exec_161d3666 passed，候选 `a6dbcb80`
- pkg_7140f759（P2）→ exec_23cf4c9e passed，候选 `d691b34c`
- **串行顺序确认**：P2 的 execution 在 P1 完成后才创建（created_at 相差 8s = 一个 poll 间隔）——无抢包、无 in-flight 冲突、无跨包污染
- 主仓零触碰（HEAD 1b5f6876 不变，probe 文件不存在）

### 发现 7：执行后 worktree 不清理（已修复）

- **现象**：S2-S5 每次执行在 temp 目录留完整 checkout + git worktree 注册（本次 Canary 累积 8 个）。
- **修复**：poll_once 在执行结果处理完成后统一调 `remove_worktree`（git worktree remove --force + 目录删除，失败 warn 不阻断）；e2e 测试断言执行后 `git worktree list` 无 questlog 残留。
- **上线验证**：S5-v read-only 包执行后 worktree 残留 = **0**。
- 8 个历史残留已手动清理。

### 决策盘点

- DecisionRequest 共 15 条历史请求，**全部已 resolve**（pending=0）；本 Canary 产生的 2 条已 cancel（对应包均已 passed/废弃）。无未决人工决策。

### S5 观察裁决（Canary 全链结论）

**S2-S5 共 8 个真实包（1 read-only + 7 write）在 ymos 全部 passed，主仓全程零触碰（HEAD 保持 1b5f6876，无 probe 文件、无 merge、无污染）。**

**裁决建议**：
1. **保持单项目观察**：暂不扩大执行范围。ymos 作为 Canary 项目继续观察（观察期建议 3 天，确认自动机/人工工作流无副作用）。
2. **扩大范围的前置条件**（观察期通过后）：
   - 新项目必须注册在 `~/.todo-board.json`（路由正确性已验证）
   - 新项目的 epic 需 `auto_execute` 显式开启 + runner-config 项目开关开启
   - 每项目首次执行前先做 read-only 包验证
3. **系统修复收益**（本 Canary 阶段发现并修复 4 项）：git_clean 声明式架构修正（S1.3）、read-only NoDiff=passed（发现 5）、NoDiff 决策残留（发现 6）、worktree 自动清理（发现 7）——均有回归测试。

**遗留说明**：
- 隔离属性：后续步骤 worktree 从 base_sha 创建，前步候选不可见（步骤间承接需先合并候选或重设 base）——有意设计，已确认
- 依赖 gate：隐式 order ≥ coded（自动链）、显式 depends_on ≥ observed（人工确认）——语义分界清晰

---

## 更新（2026-08-13，AGENTS.md 施工纪律机制验证）

### 结论：机制天然存在，选 A（接受模型自主判断）

**验证方法**：封中性 objective 施工包（automation/_questlog_review_probe/ 新建 2 个探针 py，不提审查），执行后查证。

**证据**（exec_1a1f56e7，候选 66b2f23e，passed）：
- headless 施工会话自动加载 worktree 内的 ymos AGENTS.md——stdout 输出"改动分类：2 文件新建隔离探针 stub，零消费方、无现有链路影响，未触发 agent 序列"（"改动分类/agent 序列"概念仅存在于 AGENTS.md 矩阵）
- 模型自觉跑 python3 边界测试（verifier 行为内化）
- 候选精确（只含 2 新文件 +31 行）、主仓零触碰

**机制链**：worktree（git 检出 AGENTS.md）→ headless ZCode 自动加载 → 按矩阵自主判断 → subagent 走 ~/.zcode/agents（用户 MiniMax-M3 指定，DB 实锤 zcode-adversarial-auditor 等走 custom provider）。

**用户决策（选 A）**：接受模型按矩阵精神判断（系统性改动才触发全套审查，隔离 stub 等低风险不浪费 token）。Runner 不自建审查编排——AGENTS.md 是单一纪律源。

### 执行器模型选择（同期上线）

- App 方案页开关行新增 🤖 模型选择器：读 ~/.zcode/cli/config.json 的 provider/model，选中值存 runner-config.executor_model
- Runner 建 worktree 时注入项目级 .zcode/config.json（model + 完整 provider def；实验确认 CLI 0.16.3 无 --model 参数、项目 config 不合并全局 provider 表、.zcode/config.json 需加入 scope 排除）
- 思考档只读展示（CLI 0.16.3 无运行时切换；defaultVariant 不被解析）
- 端到端验证：GLM-5.2 注入 → 日志确认 → read-only 执行 passed；用户已在 UI 实际切换过 DeepSeek
