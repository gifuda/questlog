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
