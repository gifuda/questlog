use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ItemType {
    Knowhow,
    Todo,
    Idea,
}

impl ItemType {
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "knowhow" => Ok(Self::Knowhow),
            "todo" => Ok(Self::Todo),
            "idea" => Ok(Self::Idea),
            _ => Err(format!("invalid type: {s}")),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Knowhow => "knowhow",
            Self::Todo => "todo",
            Self::Idea => "idea",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    Active,
    Completed,
    Archived,
    Converted,
}

impl ItemStatus {
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "archived" => Ok(Self::Archived),
            "converted" => Ok(Self::Converted),
            _ => Err(format!("invalid status: {s}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    High,
    Medium,
    Low,
}

impl Priority {
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "high" => Ok(Self::High),
            "medium" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            _ => Err(format!("invalid priority: {s}")),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

/// Structured outcome of the last automated (collector-tenant) attempt.
/// Machine-written by the trusted write-back path only; never inferred from
/// free text. Outcome values are a closed set (see the 待办租户 plan §5.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutoResult {
    /// accepted_report | cannot_auto | needs_review | execution_failed |
    /// timed_out | result_pending_writeback | stale | cancelled | preflight_blocked
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_level: Option<String>,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: ItemType,
    pub title: String,
    pub content: String,
    pub suggestion: String,
    pub tags: Vec<String>,
    pub status: ItemStatus,
    pub priority: Priority,
    pub source_session: String,
    /// True when the user copied the reference to process it in a conversation.
    #[serde(default)]
    pub processing: bool,
    /// Nightly auto-process opt-in (collector tenant pipeline). Explicit user
    /// authorization only — defaults to false, never inferred by a model.
    /// NOTE: 不得加 skip_serializing_if——update_todo 的补丁循环只写序列化
    /// 出的键，跳过 false 会导致"开"写进去、"关"永远写不进去。
    #[serde(default)]
    pub auto_process: bool,
    /// Bumped on every explicit (re)authorization. A closed result never
    /// applies to a later revision: turning the opt-in off and back on must
    /// not reuse the old result.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub auto_process_revision: u32,
    /// Structured result of the last automated attempt, written by the
    /// trusted write-back path only (generic UI/MCP updates cannot set it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_result: Option<AutoResult>,
    /// Written only when a CAS write-back actually completed active→completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_completed_at: Option<String>,
    /// User confirmation of a nightly-completed result (the review gate:
    /// auto-completed cards wait in a dedicated holding section until the
    /// user acknowledges, then join the regular done area).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_ack_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodosFile {
    /// Display metadata, not a safety invariant. Legacy files written before
    /// this field existed carry no `version` — the strict reader must accept
    /// them (default 1) instead of blocking the whole tenant (G5 drill
    /// finding: real boards lack it, only the salvage path tolerated that).
    #[serde(default = "default_todos_version")]
    pub version: u32,
    pub project: String,
    pub items: Vec<TodoItem>,
    /// Monotonic revision counter for CAS concurrency control.
    /// Old files without this field default to 0 (serde default).
    #[serde(default)]
    pub revision: u64,
}

fn default_todos_version() -> u32 {
    1
}

impl Default for TodosFile {
    fn default() -> Self {
        Self {
            version: 1,
            project: String::new(),
            items: vec![],
            revision: 0,
        }
    }
}

// ════════════════════════════════════════════════════════════
// 方案追踪模块 (Epic / Step) — 独立于待办，存储在 .epics.json
// ════════════════════════════════════════════════════════════

/// 方案状态机。
/// draft → active → verifying → observing → done
/// 旁支状态：blocked（被依赖阻塞）、paused（暂停）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EpicStatus {
    Draft,
    Active,
    Verifying,
    Observing,
    Done,
    Blocked,
    Paused,
}

impl EpicStatus {
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "draft" => Ok(Self::Draft),
            "active" => Ok(Self::Active),
            "verifying" => Ok(Self::Verifying),
            "observing" => Ok(Self::Observing),
            "done" => Ok(Self::Done),
            "blocked" => Ok(Self::Blocked),
            "paused" => Ok(Self::Paused),
            _ => Err(format!("invalid epic status: {s}")),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Verifying => "verifying",
            Self::Observing => "observing",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Paused => "paused",
        }
    }

    /// Short label shown as badge text in UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Draft => "草稿",
            Self::Active => "进行中",
            Self::Verifying => "验证中",
            Self::Observing => "观察窗",
            Self::Done => "已完成",
            Self::Blocked => "阻塞",
            Self::Paused => "暂停",
        }
    }
}

/// 步骤完成度阶段，映射到百分比。
/// pending=0, designed=25, coded=50, verified=75, observed=100
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum StepStage {
    Pending,
    Designed,
    Coded,
    Verified,
    Observed,
}

impl StepStage {
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "pending" => Ok(Self::Pending),
            "designed" => Ok(Self::Designed),
            "coded" => Ok(Self::Coded),
            "verified" => Ok(Self::Verified),
            "observed" => Ok(Self::Observed),
            _ => Err(format!("invalid step stage: {s}")),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Designed => "designed",
            Self::Coded => "coded",
            Self::Verified => "verified",
            Self::Observed => "observed",
        }
    }

    pub fn pct(&self) -> u32 {
        match self {
            Self::Pending => 0,
            Self::Designed => 25,
            Self::Coded => 50,
            Self::Verified => 75,
            Self::Observed => 100,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Pending => "未启动",
            Self::Designed => "已设计",
            Self::Coded => "已实施",
            Self::Verified => "已验证",
            Self::Observed => "已观察",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Pending => "☐",
            Self::Designed => "◐",
            Self::Coded => "◑",
            Self::Verified => "◒",
            Self::Observed => "☑",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EpicLinks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 方案（Epic）— 一个跨数周的改进路线项。
/// 层级：顶级方案（parent_id=None）→ 子方案（parent_id=Some）→ 步骤。
/// 最多三层：子方案不能再有子方案。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Epic {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub status: EpicStatus,
    /// 自由文本优先级标签，通常 P0|P1|P2|P3，但允许任意值。
    #[serde(default = "default_priority")]
    pub priority: String,
    /// 父方案 id。None = 顶级方案。Some = 子方案（挂在父方案下，不显示在顶层列表）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// 依赖的其他 epic id 列表。
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// 被哪些 epic 依赖（由 epic_store::derive_blocks 自动派生，不手动写）。
    #[serde(default)]
    pub blocks: Vec<String>,
    #[serde(default)]
    pub links: EpicLinks,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_active: Option<u32>,
    /// Runner 自动执行控制。
    /// None → 继承项目级开关
    /// Some(true) → 项目开启时自动执行
    /// Some(false) → 永远不自动执行
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_execute: Option<bool>,
    /// 验收条件（自由文本，由创建方案的对话显式填写）。人类可读，代码不解析。
    #[serde(default)]
    pub acceptance: String,
    /// 收尾原因。progress<100% 提前标 done 时强制必填。
    #[serde(default)]
    pub close_reason: String,
    pub created_at: String,
    pub updated_at: String,
}

fn default_priority() -> String {
    "P1".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StepLinks {
    /// 可选：软关联一条待办的 id（引用，非搬运）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todo_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// 步骤（Step）— 方案的子项，有 5 阶段进度。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub epic_id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub order: u32,
    /// 显式依赖的其他步骤 id（可跨 order）。空 = 按 order 串行推进。
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub stage: StepStage,
    #[serde(default)]
    pub links: StepLinks,
    /// TaskPackage 摘要（seal 后回写）。只存可移植信息。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<StepPackageSummary>,
    pub created_at: String,
    pub updated_at: String,
}

/// Step 上存储的 TaskPackage 摘要（可移植，不含绝对路径或 CLI session）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepPackageSummary {
    pub package_id: String,
    pub plan_revision: u32,
    pub package_hash: String,
    pub objective: String,
    pub executor: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpicsFile {
    pub version: u32,
    pub project: String,
    pub epics: Vec<Epic>,
    pub steps: Vec<Step>,
    /// Monotonic revision counter for CAS concurrency control.
    /// Old files without this field default to 0 (serde default).
    #[serde(default)]
    pub revision: u64,
}

impl Default for EpicsFile {
    fn default() -> Self {
        Self {
            version: 1,
            project: String::new(),
            epics: vec![],
            steps: vec![],
            revision: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── StepStage::pct — the progress-percent mapping. Frontend progress bars
    //    depend on these exact values; a regression here shifts every plan's %. ──

    #[test]
    fn auto_process_false_serializes_explicitly() {
        // 回归钉：auto_process 曾带 skip_serializing_if=is_false，而
        // update_todo 的补丁循环只写序列化出的键——导致"开"能写入、
        // "关"永远写不进文件（用户视角：夜间自动开了就取消不了）。
        let item = TodoItem {
            id: "t".into(),
            item_type: ItemType::Todo,
            title: "t".into(),
            content: String::new(),
            suggestion: String::new(),
            tags: vec![],
            status: ItemStatus::Active,
            priority: Priority::Medium,
            source_session: String::new(),
            processing: false,
            auto_process: false,
            auto_process_revision: 0,
            auto_result: None,
            auto_completed_at: None,
            auto_ack_at: None,
            created_at: "2026-09-24T00:00:00Z".into(),
            updated_at: "2026-09-24T00:00:00Z".into(),
        };
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v.get("auto_process"), Some(&serde_json::json!(false)));
    }

    #[test]
    fn step_stage_pct_values() {
        assert_eq!(StepStage::Pending.pct(), 0);
        assert_eq!(StepStage::Designed.pct(), 25);
        assert_eq!(StepStage::Coded.pct(), 50);
        assert_eq!(StepStage::Verified.pct(), 75);
        assert_eq!(StepStage::Observed.pct(), 100);
    }

    // ── Enum from_str / as_str round-trips ──

    #[test]
    fn item_type_round_trip() {
        for s in ["knowhow", "todo", "idea"] {
            let t = ItemType::from_str(s).unwrap();
            assert_eq!(t.as_str(), s);
        }
        assert!(ItemType::from_str("bug").is_err());
    }

    #[test]
    fn item_status_round_trip() {
        for s in ["active", "completed", "archived", "converted"] {
            let v = ItemStatus::from_str(s).unwrap();
            assert_eq!(v, ItemStatus::from_str(s).unwrap());
        }
        assert!(ItemStatus::from_str("closed").is_err());
    }

    #[test]
    fn priority_round_trip() {
        for s in ["high", "medium", "low"] {
            let p = Priority::from_str(s).unwrap();
            assert_eq!(p.as_str(), s);
        }
        assert!(Priority::from_str("urgent").is_err());
    }

    #[test]
    fn epic_status_round_trip() {
        for s in [
            "draft",
            "active",
            "verifying",
            "observing",
            "done",
            "blocked",
            "paused",
        ] {
            let e = EpicStatus::from_str(s).unwrap();
            assert_eq!(e.as_str(), s);
        }
        assert!(EpicStatus::from_str("canceled").is_err());
    }

    #[test]
    fn step_stage_round_trip() {
        for s in ["pending", "designed", "coded", "verified", "observed"] {
            let st = StepStage::from_str(s).unwrap();
            assert_eq!(st.as_str(), s);
        }
        assert!(StepStage::from_str("started").is_err());
    }

    // ── serde: field names and casing must stay stable (frontend reads them) ──

    #[test]
    fn todo_item_serializes_type_field_lowercase() {
        // `#[serde(rename = "type")]` + `rename_all = "lowercase"` — the JSON
        // key is "type" (not "item_type"), and the value is lowercase.
        let item = TodoItem {
            id: "x".into(),
            item_type: ItemType::Todo,
            title: "t".into(),
            content: "c".into(),
            suggestion: "s".into(),
            tags: vec![],
            status: ItemStatus::Active,
            priority: Priority::Medium,
            source_session: "".into(),
            processing: false,
            auto_process: false,
            auto_process_revision: 0,
            auto_result: None,
            auto_completed_at: None,
            auto_ack_at: None,
            created_at: "2026".into(),
            updated_at: "2026".into(),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["type"], "todo");
        assert_eq!(json["status"], "active");
        assert_eq!(json["priority"], "medium");
        // Round-trip back
        let back: TodoItem = serde_json::from_value(json).unwrap();
        assert_eq!(back.item_type, ItemType::Todo);
    }

    #[test]
    fn epic_priority_defaults_to_p1_when_missing() {
        // Epic with no "priority" field should fall back to default_priority() = "P1".
        let json = r#"{
            "id": "e1",
            "title": "T",
            "status": "draft",
            "created_at": "2026",
            "updated_at": "2026"
        }"#;
        let epic: Epic = serde_json::from_str(json).unwrap();
        assert_eq!(epic.priority, "P1");
        assert_eq!(epic.depends_on, Vec::<String>::new());
        assert_eq!(epic.blocks, Vec::<String>::new());
    }

    #[test]
    fn epic_parent_id_optional_and_skipped_when_none() {
        let epic = Epic {
            id: "e1".into(),
            title: "T".into(),
            description: "".into(),
            status: EpicStatus::Draft,
            priority: "P1".into(),
            parent_id: None,
            depends_on: vec![],
            blocks: vec![],
            links: EpicLinks::default(),
            max_active: None,
            auto_execute: None,
            acceptance: "".into(),
            close_reason: "".into(),
            created_at: "2026".into(),
            updated_at: "2026".into(),
        };
        let json = serde_json::to_string(&epic).unwrap();
        // skip_serializing_if = "Option::is_none" → "parent_id" absent
        assert!(!json.contains("parent_id"));
        assert!(!json.contains("max_active"));
    }
}
