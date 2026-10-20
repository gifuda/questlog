/**
 * 服务器监控 (Infra Board) 类型 — 与家庭服务器 agent-base
 * snapshot-local.json 契约 v1 对齐（2026-10-19 实况核对）。
 * 纯只读：这些类型只用于展示，app 不写任何数据侧文件。
 *
 * 宽松防御型：数据源键动态（queue 键集合不固定）、全字段可缺/可 null，
 * 视图层一律按可选消费，绝不因字段缺失崩溃。契约 v1 未记载的额外键
 * （llm_quota 下的 qingtie/lifeos/ztasks 等）不建模不展示——防 v2 变动
 * 连带破坏视图。
 */

export type InfraSnapshotStatus = "ok" | "missing" | "parse_error";

/** get_infra_snapshot 命令返回的状态信封（照 get_arb_board 模式）。 */
export interface InfraSnapshotEnvelope {
  status: InfraSnapshotStatus;
  /** 快照文件绝对路径（missing 态展示用） */
  snapshot_path?: string;
  /** parse_error 态的错误描述 */
  error?: string;
  /** ok 态时 snapshot-local.json 原文透传（fetched_at 随 body 带出），前端防御式再解析 */
  data?: string;
}

/** 单窗口额度（5h 窗 / 周窗同构）。 */
export interface InfraQuotaWindow {
  /** 剩余百分比 0..100；null = 未知 */
  remaining_pct?: number | null;
  reset_at?: number | null;
  /** 本地时区重置时刻文案（如「周二 02:18」），11px 小字直显 */
  reset_local?: string | null;
}

/** LLM 供应商额度项（llm_quota.providers[]）。 */
export interface InfraProvider {
  id?: string;
  name?: string | null;
  /** 实况 "live"；异常时配合 error 展示 */
  state?: string | null;
  five_hour?: InfraQuotaWindow | null;
  weekly?: InfraQuotaWindow | null;
  error?: string | null;
  as_of?: number | null;
}

/** 底座快照段（snapshot.base）。queue/health 键动态，全字段可缺。 */
export interface InfraBase {
  /** 实况仅 dead_letter；契约自注「键动态」——视图动态枚举，不写死三键 */
  queue?: Record<string, number>;
  /** schema v2：排队明细（id/title/harness/enqueued_at，全字段可缺） */
  queued_items?: {
    id?: number;
    title?: string;
    harness?: string;
    enqueued_at?: string;
  }[];
  /** schema v2：在办明细（started_at 计时） */
  running_items?: {
    id?: number;
    title?: string;
    harness?: string;
    started_at?: string;
  }[];
  recent?: {
    id?: number;
    /** 运行中为 null（尚未结束） */
    secs?: number | null;
    status?: string;
    harness?: string;
  }[];
  dsh_today?: number | null;
  usage_14d?: {
    day?: string;
    ok?: number;
    fail?: number;
    secs?: number;
    in_tok?: number;
    harness?: string;
  }[];
  usage_today?: { harness?: string; tasks?: number; input_tokens?: number }[];
  /** checked_at / status 为元数据键，*_detail 与主键配对，枚举时跳过 */
  health?: Record<string, string>;
}

/** snapshot-local.json 解析后的防御视图（额外键忽略）。 */
export interface InfraSnapshotFile {
  snapshot?: {
    base?: InfraBase;
    /** NAS 帧龄（秒）；null = 无数据（F8 前置逻辑按最坏假设处理） */
    nas_frame_age_secs?: number | null;
    generated_at?: string | null;
  };
  llm_quota?: {
    updated_local?: string | null;
    providers?: InfraProvider[];
  };
  /** 落盘时刻（ISO 8601 UTC），新鲜度判定基准 */
  fetched_at?: string | null;
}
