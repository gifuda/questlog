# target 构建缓存淘汰机制

> 2026-09-23 建立。此前 target/ 无任何淘汰策略，只涨不缩（2026-09-04 实测 9GB，
> 同月 23 日又涨回 3.7GB）。本机制是"构建缓存瘦身"方案（epic_521748a0）的收尾：
> staticlib 移除与分层 profile 解决"单次构建太大"，本机制解决"只涨不缩"。

## 组成

| 组件 | 位置 | 职责 |
|------|------|------|
| 淘汰脚本 | `scripts/target-eviction.sh` | 纯 bash 阶梯淘汰，零外部依赖（不需要 cargo 在 PATH） |
| 调度 | `~/Library/LaunchAgents/com.questlog.target-eviction.plist` | 每 24h 自动调用一次（StartInterval=86400） |
| 日志 | `~/questlog-runtime/logs/target-eviction.log` | 每次运行的大小与动作记录 |
| 验收路径守卫 | 脚本内 `case` 检查 | 只接受 `*/src-tauri/target` 路径，防误配 rm -rf |

## 阶梯策略

默认模式仅在总量超 **WARN 2.5GiB** 时启动；`--deep` 无视阈值强制执行（KEEP_DAYS 从 30 收紧到 7）：

1. 删 `target/{debug,release}/incremental` — 零风险，代价仅下次增量编译变全量
2. 清扫 `deps`/`build`/`.fingerprint`/`bundle` 中 mtime 超 KEEP_DAYS 的陈旧产物 — 被误删的仍会被 cargo 按需重编，最坏代价一次慢构建，不损坏状态
3. 清扫后仍超 **HARD 3.5GiB** → 整删 `target/debug`（release 交付产物不动）
4. 仍超 HARD → 记 CRITICAL + macOS 通知，**不自动 cargo clean**（抹 release = 下次交付构建全量重编，留给人工决策）

并发保护：mkdir 锁（`~/questlog-runtime/target-eviction.lock`），超 2h 的残留锁自动接管。

## 手动用法

```bash
scripts/target-eviction.sh --dry-run        # 预演（只报告不删除）
scripts/target-eviction.sh --deep           # 首轮/定期深度清理
scripts/target-eviction.sh                  # 与 launchd 相同的默认模式
```

停用调度：`launchctl bootout gui/$(id -u)/com.questlog.target-eviction`（plist 文件保留，重启用 `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.questlog.target-eviction.plist`）。

## 已知权衡

- 按时 mtime 清扫会误删"仍在被引用但很久没重编"的依赖产物（cargo-sweep 同款行为），
  触发一次按需重编。阈值 30 天 + 仅超 WARN 才清扫，把这种代价压到很低频率。
- 清扫避开正在进行的构建：锁只防脚本自身并发，不感知 cargo——每日一次的窗口撞上
  正在跑的 cargo 时，最坏是该次构建报"找不到产物"，重跑即恢复。

## 观察期自动追踪（S5）

淘汰机制上线后进入 7 天观察期（2026-09-23 至 2026-09-30，契约见方案 epic_521748a0
的 acceptance 字段）。自动追踪不依赖对话/自动化名额，由第二个系统级后台代理承载：

- **代理**：`com.questlog.target-observation`（每日 12:10）→ `scripts/target-observation-check.sh`
- **三项核验**（与 acceptance 一致）：① 淘汰调度 26h 内有运行记录（宽限覆盖 Mac 睡眠延迟）② 26h 内无 CRITICAL ③ target ≤2.5GiB
- **记录**：结果追加 `~/questlog-runtime/logs/target-eviction-observation.md`，幂等防双记
- **通知**：FAIL 即时告警；观察期满（09-30 PASS）发汇总通知并自动退役（bootout 自己）
- **边界**：只读检查 + 追加记录，绝不改面板——S1-S5 推进 observed 仍由用户确认

运维：`--check-only` 只跑检查不记录；停用 `launchctl bootout gui/$(id -u)/com.questlog.target-observation`。

## 首轮效果（2026-09-23）

target 3.7G → 574M（debug 434M + release 139M，均为 9 月新构建的分层 profile 精益缓存），
项目总量 4.0G → 790M。launchd kickstart 实测：runs=1、exit 0、日志 healthy。
