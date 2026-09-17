# 租户入租契约 · QuestLog（待办租户）

> 版本：2026-09-17-v1（经理机起草，待总控审查适配）
> 模板：templates/租户入租契约模板.md v1
> 方案源：经理机仓库 docs/待办租户_采集机凌晨自动处理方案_审阅修订版_20260916.md（G0/G0.1 已完成，G1–G4 已在经理机实现并测试）

## 一、本租户是什么

- 租户名：`questlog` · 平台注册状态：待 NodeManifest 登记（executor 施工项）
- 业务边界：把经理机（Tailscale `manager`）QuestLog 桌面清单贴中用户显式 opt-in 的**只读研究类待办**，在夜间窗口经平台队列执行（zcode headless 只读），产出结构化研究报告并回执给经理机对账写回。**不做**：修改任何项目文件、写代码、交易、外部消息、不可逆动作。

## 二、你能用什么（平台接口）

- 数据目录：`/Users/Shared/CollectorData/tenants/questlog/`（0700）
  - `repos/`——输入快照的只读 checkout（按 base_sha 分离检出，`git status` 必须保持 clean）
  - `reports/<task_key>/report.md`——报告落盘（随平台 03:00 异机备份；请登记 backup-catalog）
- 任务通道：队列/执行器（python 执行器 + 白名单脚本 `questlog-dispatch` / `questlog-research`）；`side_effect_class="read_only"`，无需审批面
- 告警：平台统一 Bark 分级（任务 FAILED/ORPHANED 走平台默认分级）
- 每日巡检：平台日报自动覆盖本租户产物新鲜度

## 三、你不能做什么（红线）——逐条确认 + 申请薄壳

1. 不碰其他租户 ✓（脚本仅访问 `/Users/Shared/CollectorData/tenants/questlog/` 与平台只读 API/DB）
2. 不直接 SSH 写采集机 ✓（本租户所有采集机侧变更走本工作包→总控→executor 流程）
3. 不自建 launchd ✓（调度用平台 scheduler 注册 schedule）
4. 不自动交易/不写 A 资产/不把模型结论转不可逆动作 ✓（只读研究）
5. 密钥只引用不落明文 ✓（本租户 v1 无新增密钥；zcode 凭据用采集机既有配置，不复制）

**申请授予的薄壳（唯一的跨机访问面）**：本租户脚本经 SSH 只读访问**经理机**：
- 读：`ssh manager ls/scp` 经理机 `~/questlog-runtime/todo-outbox/*.json`（投递消息）、git fetch 经理机输入仓库（按 base_sha，只读）
- 写（仅两处）：`scp` 写经理机 `~/questlog-runtime/todo-receipts/<task_key>.json`（回执）；`ssh manager mv` 把已回执的 outbox 文件移入 `todo-outbox/archived/`
- 通道：`ssh -o BatchMode=yes manager`（fleet 既有别名，密钥既有）；全部为固定路径 + 已校验的 64-hex task_key，自由文本永不进 shell 参数（经文件传输）

## 四、开发→落地流程

已按流程执行：经理机专项会话完成方案（含决策清单/UNKNOWN/验收/回滚，见 WP-GX1）→ 本契约 + 施工包提交总控审查 → executor 施工 → 核验 → shadow 观察期 → 销项。

## 五、本租户特定事实

- **通道路径**：投递=经理机 `todo-outbox/`（task_key 命名 JSON，schema 见 `questlog.py`）；回执=经理机 `todo-receipts/<task_key>.json`（schema：version/task_key/attempt_id/exit_ok/timed_out/budget_exceeded/changes_detected/report_stdout/received_at）
- **幂等键**：`UNIQUE(tenant="questlog", idempotency_key=task_key)`——同键重投递被平台 schema 层吸收；回执后 outbox 条目归档
- **时窗**：派发仅在 19:00–21:00 UTC（= 03:00–06:00 +08:00，与经理机 sweep 窗口一致）；回执半程不受窗口限制（对账恢复语义）
- **预算**：每任务 wall-clock 上限来自投递消息 timeout_seconds（钳制 60–900s）；`max_attempts=1`
- **zcode 执行方式（决策点 D1）**：v1 shadow 用桩执行器（`QUESTLOG_RESEARCH_STUB=1`，固定 NEEDS_REVIEW 报告）打通全链；zcode headless 实接为后续独立施工项（需先解决采集机 CLI 安装/链接，手册 F-2）
- **经理机对账端**：回执由经理机 QuestLog app 的 finalization 状态机消费（已实现并测试），验收规则为原生报告验证器（不信任模型自报 DONE）
- **观察项**：跨机 SSH 失败率；outbox 积压；task_key 碰撞（应恒为零）
