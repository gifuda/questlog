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
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodosFile {
    pub version: u32,
    pub project: String,
    pub items: Vec<TodoItem>,
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
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpicsFile {
    pub version: u32,
    pub project: String,
    pub epics: Vec<Epic>,
    pub steps: Vec<Step>,
}

impl Default for EpicsFile {
    fn default() -> Self {
        Self {
            version: 1,
            project: String::new(),
            epics: vec![],
            steps: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── StepStage::pct — the progress-percent mapping. Frontend progress bars
    //    depend on these exact values; a regression here shifts every plan's %. ──

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
