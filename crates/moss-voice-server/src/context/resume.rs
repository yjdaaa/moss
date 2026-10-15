//! NV19-07 read-only cross-session project resume. One summary per project/workspace with four
//! fixed sections (decisions, to-dos, evidence, unverified). Items may cite records as
//! `[approval:…]`, `[job:…]`, `[agent_run:…]` or `[test:…]`; on resume every citation is checked
//! again against the host's current records. Resuming only reports: a result-unknown run, an old
//! approval or a stopped job is never retried, re-approved or replayed, and there is no code
//! path here that executes anything (the NV13 Runner stays disabled by its own gate).
use super::service::{ContextService, ServiceError};
use super::{check_content, AccessContext, Category, Channel, ContextError, ContextMemory, MemoryDraft, RecordState, ScopeKey, ScopeKind, Source, SourceKind, Verification};

const HEADER: &str = "【项目恢复摘要】";
const SECTIONS: [&str; 4] = ["决策：", "待办：", "证据：", "未验证："];
const MAX_ITEMS: usize = 8;
const ITEM_CHARS: usize = 120;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResumeNote {
    pub decisions: Vec<String>,
    pub todos: Vec<String>,
    pub evidence: Vec<String>,
    pub unverified: Vec<String>,
}

fn clean(item: &str) -> String {
    item.split_whitespace().collect::<Vec<_>>().join(" ").replace('；', "，")
}

impl ResumeNote {
    fn sections(&self) -> [&Vec<String>; 4] {
        [&self.decisions, &self.todos, &self.evidence, &self.unverified]
    }

    fn validate(&self) -> Result<(), ContextError> {
        let items = self.sections();
        if items.iter().all(|list| list.is_empty()) || items.iter().any(|list| list.len() > MAX_ITEMS || list.iter().any(|i| i.trim().is_empty() || i.chars().count() > ITEM_CHARS)) {
            return Err(ContextError::ContentInvalid);
        }
        Ok(())
    }

    pub fn to_content(&self) -> String {
        let mut lines = vec![HEADER.to_owned()];
        for (name, list) in SECTIONS.iter().zip(self.sections()) {
            if !list.is_empty() {
                lines.push(format!("{name}{}", list.iter().map(|i| clean(i)).collect::<Vec<_>>().join("；")));
            }
        }
        lines.join("\n")
    }

    /// `None` unless the content is a resume summary (starts with the header line).
    pub fn parse(content: &str) -> Option<Self> {
        let mut lines = content.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let mut note = Self::default();
        for line in lines {
            for (index, name) in SECTIONS.iter().enumerate() {
                if let Some(text) = line.strip_prefix(name) {
                    let items: Vec<String> = text.split('；').map(str::trim).filter(|i| !i.is_empty()).map(str::to_owned).collect();
                    match index {
                        0 => note.decisions = items,
                        1 => note.todos = items,
                        2 => note.evidence = items,
                        _ => note.unverified = items,
                    }
                }
            }
        }
        Some(note)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    Approval,
    Job,
    AgentRun,
    Test,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Reference {
    pub kind: RefKind,
    pub id: String,
}

/// `[kind:id]` citations inside an item; malformed ones are ignored, never guessed.
pub fn references(item: &str) -> Vec<Reference> {
    let mut found = Vec::new();
    for part in item.split('[').skip(1) {
        let Some((body, _)) = part.split_once(']') else { continue };
        let Some((kind, id)) = body.split_once(':') else { continue };
        let kind = match kind {
            "approval" => RefKind::Approval,
            "job" => RefKind::Job,
            "agent_run" => RefKind::AgentRun,
            "test" => RefKind::Test,
            _ => continue,
        };
        if !id.is_empty() && id.len() <= 96 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.') {
            let reference = Reference { kind, id: id.into() };
            if !found.contains(&reference) {
                found.push(reference);
            }
        }
    }
    found
}

/// Current state of a cited record as the host reads it right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefState {
    Done,
    Failed,
    InProgress,
    /// Sent or started, outcome not known (e.g. NV13 `result_unknown`, interrupted sending).
    ResultUnknown,
    /// Rejected, expired or cancelled approval.
    Revoked,
    /// Cancelled or stopped job / run.
    Stopped,
    Missing,
    /// The host has no record type it can check this against.
    Unverifiable,
}

impl RefState {
    fn label(self) -> &'static str {
        match self {
            Self::Done => "已完成",
            Self::Failed => "失败",
            Self::InProgress => "仍在进行或待处理，不会自动批准或执行",
            Self::ResultUnknown => "结果未知，不会自动重试，须到现场确认",
            Self::Revoked => "已撤销或过期，不会重放",
            Self::Stopped => "已停止，不会重新启动",
            Self::Missing => "找不到记录",
            Self::Unverifiable => "无法核实",
        }
    }
}

/// Read-only view of the host's records. There is intentionally no method that acts.
pub trait ResumeFacts {
    fn state(&self, reference: &Reference) -> RefState;
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckedItem {
    pub text: String,
    pub states: Vec<(Reference, RefState)>,
    /// Why this item is in “unverified” although it was saved elsewhere.
    pub moved_from_evidence: bool,
}

impl CheckedItem {
    pub fn attention(&self) -> bool {
        self.states.iter().any(|(_, state)| *state != RefState::Done)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResumeView {
    pub memory_id: String,
    pub revision: i64,
    pub updated_at: f64,
    pub decisions: Vec<CheckedItem>,
    pub todos: Vec<CheckedItem>,
    pub evidence: Vec<CheckedItem>,
    pub unverified: Vec<CheckedItem>,
}

fn check(items: &[String], facts: &dyn ResumeFacts) -> Vec<CheckedItem> {
    items
        .iter()
        .map(|text| CheckedItem { text: text.clone(), states: references(text).into_iter().map(|r| { let s = facts.state(&r); (r, s) }).collect(), moved_from_evidence: false })
        .collect()
}

fn resume_rows(rows: Vec<ContextMemory>, now: f64) -> Option<(ContextMemory, ResumeNote)> {
    rows.into_iter()
        // Only the user's own confirmed entry counts: an extracted candidate that merely starts
        // with the header (e.g. a model imitating the format) is never taken as the summary.
        .filter(|row| {
            row.category == Category::TaskContext && row.source.kind() == SourceKind::UserEdit && matches!(row.state(now), RecordState::Active(Verification::Confirmed))
        })
        .filter_map(|row| ResumeNote::parse(&row.content).map(|note| (row, note)))
        .max_by(|a, b| a.0.updated_at.total_cmp(&b.0.updated_at))
}

impl ContextService {
    fn resume_scope(access: &AccessContext, scope: &ScopeKey) -> Result<(), ServiceError> {
        if access.channel() != Channel::DesktopPrivate {
            return Err(ContextError::AccessDenied.into());
        }
        if !matches!(scope.kind(), ScopeKind::Project | ScopeKind::Workspace) {
            return Err(ContextError::CategoryNotInScope.into());
        }
        Ok(())
    }

    /// Save the project's summary (private window). The first save needs `expected = None`;
    /// later saves must name the current record and revision, so two windows cannot silently
    /// overwrite each other.
    pub fn save_resume(&self, access: &AccessContext, scope: &ScopeKey, note: &ResumeNote, expected: Option<(&str, i64)>, now: f64) -> Result<ContextMemory, ServiceError> {
        Self::resume_scope(access, scope)?;
        note.validate()?;
        let content = note.to_content();
        check_content(&content)?;
        let existing = resume_rows(self.list(access, scope, now)?, now);
        match (expected, existing) {
            (None, None) => self.create(access, MemoryDraft {
                scope: scope.clone(),
                category: Category::TaskContext,
                content,
                verification: Verification::Confirmed,
                source: Source::new(SourceKind::UserEdit, None)?,
                applicability: None,
                expires_at: None,
            }, now),
            (Some((id, revision)), Some((row, _))) if row.id == id => self.edit(access, id, revision, &content, None, None, now),
            _ => Err(ServiceError::Conflict),
        }
    }

    /// Re-check every citation against the host's current records and report. Evidence whose
    /// citations no longer hold (failed, unknown, revoked, missing, unverifiable) is moved to
    /// “unverified”. Nothing is executed, approved or retried.
    pub fn resume(&self, access: &AccessContext, scope: &ScopeKey, facts: &dyn ResumeFacts, now: f64) -> Result<Option<ResumeView>, ServiceError> {
        Self::resume_scope(access, scope)?;
        let Some((row, note)) = resume_rows(self.list(access, scope, now)?, now) else { return Ok(None) };
        let mut evidence = Vec::new();
        let mut unverified = check(&note.unverified, facts);
        for item in check(&note.evidence, facts) {
            if item.attention() {
                unverified.push(CheckedItem { moved_from_evidence: true, ..item });
            } else {
                evidence.push(item);
            }
        }
        Ok(Some(ResumeView {
            memory_id: row.id,
            revision: row.revision,
            updated_at: row.updated_at,
            decisions: check(&note.decisions, facts),
            todos: check(&note.todos, facts),
            evidence,
            unverified,
        }))
    }
}

fn kind_name(kind: RefKind) -> &'static str {
    match kind {
        RefKind::Approval => "审批",
        RefKind::Job => "加工任务",
        RefKind::AgentRun => "Agent 运行（只读记录）",
        RefKind::Test => "测试",
    }
}

pub fn format_resume(view: &ResumeView) -> String {
    let mut out = vec!["【项目恢复：上次会话的记录，仅供参考；开始前须重新核对代码、数据库与设备的实际状态；结果未知、已撤销或已停止的任务不会自动重试或重放】".to_owned()];
    for (name, items) in [("决策", &view.decisions), ("待办", &view.todos), ("证据", &view.evidence), ("未验证", &view.unverified)] {
        out.push(format!("{name}："));
        if items.is_empty() {
            out.push("- （无）".into());
        }
        for item in items {
            let mut line = format!("- {}", item.text);
            for (reference, state) in &item.states {
                line.push_str(&format!("（{} {}：{}）", kind_name(reference.kind), reference.id, state.label()));
            }
            if item.moved_from_evidence {
                line.push_str("（原记为证据，现在核实不了）");
            }
            out.push(line);
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests;
