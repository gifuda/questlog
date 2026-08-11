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

**发现 4（未修复，暂停点）：活跃项目工作区常态不干净，与 git_clean 硬门禁不兼容**
- ymos 自动机持续回写 tracked 文件（writeback_review.json、s1-observation-log.jsonl），且**有意提交**（"data(自动): 自动机产出同步" 是其常规提交模式）。
- .gitignore 无法解决已跟踪文件的持续修改；等待同步窗口不可靠。
- 用户决策（2026-08-11）：**暂停 Canary，记录发现**。S2-S5 待收口决策后继续。

### 暂停状态

- S1 shadow 观察：✅ 完成（三层防护确认）
- S1.1 git_clean 修复：✅ 完成（已合并 fb45c03）
- S1.2 MCP project_dir 修复：✅ 完成（已合并 3d6d028）
- S2 read-only：⏸ 暂停（ymos 工作区常态脏 vs git_clean 门禁不兼容）
- S3/S4/S5：⏸ 待 S2 解锁

### 待决策项（下次收口）

1. git_clean 门禁是否改为"按 preconditions 声明检查"（不声明 git_clean 的包不检查）——让 Canary 包可跳过该前置
2. 或为真实活跃项目提供"允许运行时回写窗口"策略
3. 或 Canary 改用影子 clone 仓库（git clone 而非 worktree），完全避开主仓脏状态
