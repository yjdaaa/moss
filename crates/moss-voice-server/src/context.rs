//! NV19-01 structured long-term context model: categories, scopes, sources, verification.
//! Pure data contracts only: no storage (NV19-02), extraction (NV19-03) or retrieval (NV19-04).
//! Records are untrusted *data*: they never become system instructions, approvals or tool
//! arguments. Trusted scopes are minted only from host-verified facts, never from model output.
use crate::memory::private_content;
use crate::person_context::PersonScope;

pub const MAX_CONTENT_CHARS: usize = 1000;
const MAX_SCOPE_ID: usize = 64;
const MAX_REF_TOKEN: usize = 96;
const MAX_APPLICABILITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextError {
    ScopeInvalid,
    CategoryNotInScope,
    AccessDenied,
    SourceNotAllowed,
    VerificationNotAllowed,
    ContentInvalid,
    ContentSensitive,
    SourceRefInvalid,
    ApplicabilityRequired,
    ApplicabilityInvalid,
    ExpiryInvalid,
    TransitionInvalid,
}

impl ContextError {
    pub fn code(self) -> &'static str {
        match self {
            Self::ScopeInvalid => "context_scope_invalid",
            Self::CategoryNotInScope => "context_category_not_in_scope",
            Self::AccessDenied => "context_access_denied",
            Self::SourceNotAllowed => "context_source_not_allowed",
            Self::VerificationNotAllowed => "context_verification_not_allowed",
            Self::ContentInvalid => "context_content_invalid",
            Self::ContentSensitive => "context_content_sensitive",
            Self::SourceRefInvalid => "context_source_ref_invalid",
            Self::ApplicabilityRequired => "context_applicability_required",
            Self::ApplicabilityInvalid => "context_applicability_invalid",
            Self::ExpiryInvalid => "context_expiry_invalid",
            Self::TransitionInvalid => "context_transition_invalid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Preference,
    DeviceFact,
    TechnicalNote,
    TaskContext,
    TroubleshootingExperience,
}

impl Category {
    pub const ALL: [Self; 5] = [Self::Preference, Self::DeviceFact, Self::TechnicalNote, Self::TaskContext, Self::TroubleshootingExperience];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::DeviceFact => "device_fact",
            Self::TechnicalNote => "technical_note",
            Self::TaskContext => "task_context",
            Self::TroubleshootingExperience => "troubleshooting_experience",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_str() == value)
    }

    /// Storage matrix: where a record of this category may live.
    pub fn allowed_scopes(self) -> &'static [ScopeKind] {
        match self {
            Self::Preference => &[ScopeKind::Device, ScopeKind::Person],
            Self::DeviceFact => &[ScopeKind::Device],
            Self::TechnicalNote | Self::TaskContext => &[ScopeKind::Project, ScopeKind::Workspace],
            Self::TroubleshootingExperience => &[ScopeKind::Device, ScopeKind::Workspace],
        }
    }

    /// Facts tied to hardware must say which model/firmware they apply to.
    pub fn needs_applicability(self) -> bool {
        matches!(self, Self::DeviceFact | Self::TroubleshootingExperience)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    Device,
    Person,
    Project,
    Workspace,
}

impl ScopeKind {
    pub const ALL: [Self; 4] = [Self::Device, Self::Person, Self::Project, Self::Workspace];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Person => "person",
            Self::Project => "project",
            Self::Workspace => "workspace",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_str() == value)
    }
}

fn valid_token(value: &str, max: usize, extra: &[u8]) -> bool {
    !value.is_empty() && value.len() <= max && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || extra.contains(&b))
}

/// A scope as stored in a row. Plain data: holding one grants nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScopeKey {
    kind: ScopeKind,
    id: String,
}

impl ScopeKey {
    pub fn new(kind: ScopeKind, id: &str) -> Result<Self, ContextError> {
        // Device ids are MAC-like (`aa:bb:..`); other ids are host-generated tokens.
        let extra: &[u8] = if kind == ScopeKind::Device { b":." } else { b"." };
        valid_token(id, MAX_SCOPE_ID, extra).then(|| Self { kind, id: id.into() }).ok_or(ContextError::ScopeInvalid)
    }

    pub fn parse(kind: &str, id: &str) -> Result<Self, ContextError> {
        Self::new(ScopeKind::parse(kind).ok_or(ContextError::ScopeInvalid)?, id)
    }

    pub fn kind(&self) -> ScopeKind {
        self.kind
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Where the host learned the scope. Model output, documents and Skills are never an origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeOrigin {
    /// Current allowed device and its grant generation.
    DeviceGrant,
    /// `PersonScope` from the identity resolver (requires accepted calibration; none in production).
    CalibratedSpeaker,
    /// Project/workspace explicitly chosen in the private desktop window.
    DesktopSelection,
}

/// Proof that the host verified this scope for the current request. Not deserializable,
/// no `Default`; the generation lets stores reject work started before revoke/delete/switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedScope {
    key: ScopeKey,
    origin: ScopeOrigin,
    generation: u64,
}

impl TrustedScope {
    pub fn device_grant(device_id: &str, grant_generation: u64) -> Result<Self, ContextError> {
        if grant_generation == 0 {
            return Err(ContextError::ScopeInvalid);
        }
        Ok(Self { key: ScopeKey::new(ScopeKind::Device, device_id)?, origin: ScopeOrigin::DeviceGrant, generation: grant_generation })
    }

    pub fn calibrated_person(scope: &PersonScope) -> Result<Self, ContextError> {
        let generation = u64::try_from(scope.revision()).map_err(|_| ContextError::ScopeInvalid)?;
        Ok(Self { key: ScopeKey::new(ScopeKind::Person, scope.id())?, origin: ScopeOrigin::CalibratedSpeaker, generation })
    }

    pub fn desktop_selection(kind: ScopeKind, id: &str, selection_generation: u64) -> Result<Self, ContextError> {
        if !matches!(kind, ScopeKind::Project | ScopeKind::Workspace) || selection_generation == 0 {
            return Err(ContextError::ScopeInvalid);
        }
        Ok(Self { key: ScopeKey::new(kind, id)?, origin: ScopeOrigin::DesktopSelection, generation: selection_generation })
    }

    pub fn key(&self) -> &ScopeKey {
        &self.key
    }

    pub fn origin(&self) -> ScopeOrigin {
        self.origin
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// Built-in voice session (and its background extraction).
    Voice,
    /// Packaged private desktop window (`voice-private`); never LAN or the main webview.
    DesktopPrivate,
}

/// Everything the current request may touch. Access is exact (kind + id) — no cross-device,
/// cross-person or cross-project reads, and no fallback from an unknown speaker to anyone else.
#[derive(Debug, Clone)]
pub struct AccessContext {
    channel: Channel,
    scopes: Vec<TrustedScope>,
}

impl AccessContext {
    /// Voice: the allowed device, optionally a calibrated person and a project/workspace the
    /// desktop attached to this device. Each slot must carry the matching kind and origin.
    pub fn voice(device: TrustedScope, person: Option<TrustedScope>, selection: Option<TrustedScope>) -> Result<Self, ContextError> {
        if device.origin != ScopeOrigin::DeviceGrant
            || person.as_ref().is_some_and(|p| p.origin != ScopeOrigin::CalibratedSpeaker)
            || selection.as_ref().is_some_and(|s| s.origin != ScopeOrigin::DesktopSelection)
        {
            return Err(ContextError::ScopeInvalid);
        }
        let mut scopes = vec![device];
        scopes.extend(person);
        scopes.extend(selection);
        Ok(Self { channel: Channel::Voice, scopes })
    }

    /// Private window: the scopes the host enumerated for the owner of this machine.
    pub fn desktop_private(scopes: Vec<TrustedScope>) -> Self {
        Self { channel: Channel::DesktopPrivate, scopes }
    }

    pub fn channel(&self) -> Channel {
        self.channel
    }

    pub fn scopes(&self) -> &[TrustedScope] {
        &self.scopes
    }

    /// First held scope of this kind (voice holds at most one per kind).
    pub fn scope_of(&self, kind: ScopeKind) -> Option<&TrustedScope> {
        self.scopes.iter().find(|scope| scope.key.kind == kind)
    }

    pub fn trusted(&self, key: &ScopeKey) -> Option<&TrustedScope> {
        self.scopes.iter().find(|scope| &scope.key == key)
    }

    pub fn can_read(&self, key: &ScopeKey) -> bool {
        self.trusted(key).is_some()
    }

    pub fn can_write(&self, key: &ScopeKey, source: SourceKind) -> bool {
        self.can_read(key) && source.channel() == self.channel
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// The user typed or edited it in the private window.
    UserEdit,
    /// Extracted after the user opted in, from committed and truncated conversation text.
    ConversationExtraction,
    /// Generated from an old NV10 per-device summary; never deletes or moves the original.
    LegacyNv10Import,
    /// A file/directory the user explicitly selected for indexing.
    DocumentIndex,
    /// A trusted test result supplied by the host (e.g. a passed verification run).
    TrustedTest,
    /// Read-only evidence from an NV13 AgentRun record; never re-runs anything.
    AgentRunRecord,
}

impl SourceKind {
    pub const ALL: [Self; 6] = [Self::UserEdit, Self::ConversationExtraction, Self::LegacyNv10Import, Self::DocumentIndex, Self::TrustedTest, Self::AgentRunRecord];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserEdit => "user_edit",
            Self::ConversationExtraction => "conversation_extraction",
            Self::LegacyNv10Import => "legacy_nv10_import",
            Self::DocumentIndex => "document_index",
            Self::TrustedTest => "trusted_test",
            Self::AgentRunRecord => "agent_run_record",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_str() == value)
    }

    pub fn channel(self) -> Channel {
        match self {
            Self::ConversationExtraction => Channel::Voice,
            _ => Channel::DesktopPrivate,
        }
    }

    /// Source contract: which categories each source may create.
    pub fn allowed_categories(self) -> &'static [Category] {
        use Category::*;
        match self {
            Self::UserEdit => &Category::ALL,
            // Device parameters, material tests and calibration stay in their own tables.
            Self::ConversationExtraction => &[Preference, TaskContext, TroubleshootingExperience],
            Self::LegacyNv10Import => &[Preference],
            Self::DocumentIndex => &[TechnicalNote],
            Self::TrustedTest => &[DeviceFact, TroubleshootingExperience],
            Self::AgentRunRecord => &[TaskContext],
        }
    }

    /// Only the user or a trusted test may create an already-confirmed record.
    pub fn may_create_confirmed(self) -> bool {
        matches!(self, Self::UserEdit | Self::TrustedTest)
    }

    /// Prefix of the internal reference; `None` means no reference is stored.
    fn ref_prefix(self) -> Option<&'static str> {
        match self {
            Self::UserEdit => None,
            Self::ConversationExtraction => Some("extract"),
            Self::LegacyNv10Import => Some("nv10"),
            Self::DocumentIndex => Some("doc"),
            Self::TrustedTest => Some("test"),
            Self::AgentRunRecord => Some("agent_run"),
        }
    }
}

/// `<prefix>:<token>` internal reference (extraction run id, document hash, test id...).
/// Never a URL, filesystem path or anything carrying credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    kind: SourceKind,
    reference: Option<String>,
}

impl Source {
    pub fn new(kind: SourceKind, reference: Option<&str>) -> Result<Self, ContextError> {
        let reference = match (kind.ref_prefix(), reference) {
            (None, None) => None,
            (Some(prefix), Some(value)) => {
                let token = value.strip_prefix(prefix).and_then(|rest| rest.strip_prefix(':')).ok_or(ContextError::SourceRefInvalid)?;
                if !valid_token(token, MAX_REF_TOKEN, b".") || private_content(value) {
                    return Err(ContextError::SourceRefInvalid);
                }
                Some(value.to_owned())
            }
            _ => return Err(ContextError::SourceRefInvalid),
        };
        Ok(Self { kind, reference })
    }

    pub fn kind(&self) -> SourceKind {
        self.kind
    }

    pub fn reference(&self) -> Option<&str> {
        self.reference.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    Candidate,
    Confirmed,
    Invalidated,
}

impl Verification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Confirmed => "confirmed",
            Self::Invalidated => "invalidated",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [Self::Candidate, Self::Confirmed, Self::Invalidated].into_iter().find(|item| item.as_str() == value)
    }
}

/// What justifies promoting a candidate. Model agreement is not evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence {
    UserConfirmed,
    TrustedTest(Source),
}

/// Allowed state changes. Invalidated is terminal: a corrected fact becomes a new record.
pub fn transition(from: Verification, to: Verification, evidence: Option<&Evidence>) -> Result<(), ContextError> {
    use Verification::*;
    let ok = match (from, to) {
        (Candidate, Confirmed) => match evidence {
            Some(Evidence::UserConfirmed) => true,
            Some(Evidence::TrustedTest(source)) => source.kind() == SourceKind::TrustedTest && source.reference().is_some(),
            None => false,
        },
        (Candidate | Confirmed, Invalidated) => true,
        _ => false,
    };
    ok.then_some(()).ok_or(ContextError::TransitionInvalid)
}

/// Hardware a fact applies to. Unknown current firmware never matches a pinned firmware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applicability {
    device_model: String,
    firmware: Option<String>,
}

impl Applicability {
    pub fn new(device_model: &str, firmware: Option<&str>) -> Result<Self, ContextError> {
        let clean = |value: &str| {
            let value = value.trim();
            (!value.is_empty() && value.chars().count() <= MAX_APPLICABILITY && !value.chars().any(char::is_control)).then(|| value.to_owned())
        };
        let device_model = clean(device_model).ok_or(ContextError::ApplicabilityInvalid)?;
        let firmware = match firmware {
            Some(value) => Some(clean(value).ok_or(ContextError::ApplicabilityInvalid)?),
            None => None,
        };
        Ok(Self { device_model, firmware })
    }

    pub fn device_model(&self) -> &str {
        &self.device_model
    }

    pub fn firmware(&self) -> Option<&str> {
        self.firmware.as_deref()
    }

    pub fn matches(&self, model: &str, firmware: Option<&str>) -> bool {
        self.device_model.eq_ignore_ascii_case(model.trim())
            && self.firmware.as_deref().is_none_or(|pinned| firmware.is_some_and(|current| pinned.eq_ignore_ascii_case(current.trim())))
    }
}

/// G-code lines and GRBL `$n=` settings belong to job/device tables, never to memory.
/// Anywhere in a line, not only at its start: a G/M code word followed later by an axis,
/// power or feed word, or a GRBL `$<n>=` setting.
fn looks_like_machine_data(text: &str) -> bool {
    text.lines().any(|line| {
        // ASCII words, split on anything else (spaces, CJK, punctuation).
        let words: Vec<&str> = line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-')).filter(|w| !w.is_empty()).collect();
        let code = |w: &str| w.len() >= 2 && w.len() <= 4 && w.starts_with(['G', 'g', 'M', 'm']) && w[1..].bytes().all(|b| b.is_ascii_digit());
        let parameter = |w: &str| {
            let mut chars = w.chars();
            chars.next().is_some_and(|c| "XYZSFxyzsf".contains(c)) && chars.next().is_some_and(|c| c.is_ascii_digit() || c == '-' || c == '.')
        };
        let gcode = words.iter().position(|w| code(w)).is_some_and(|at| words[at + 1..].iter().any(|w| parameter(w)));
        let setting = line.match_indices('$').any(|(at, _)| {
            let rest = &line[at + 1..];
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            digits > 0 && rest[digits..].starts_with('=')
        });
        gcode || setting
    })
}

fn check_content(content: &str) -> Result<(), ContextError> {
    let trimmed = content.trim();
    if trimmed.is_empty() || content.chars().count() > MAX_CONTENT_CHARS || content.chars().any(|c| c.is_control() && c != '\n') {
        return Err(ContextError::ContentInvalid);
    }
    if private_content(content) || looks_like_machine_data(content) {
        return Err(ContextError::ContentSensitive);
    }
    Ok(())
}

/// A record about to be created. `validate` enforces every NV19-01 contract at once.
#[derive(Debug, Clone)]
pub struct MemoryDraft {
    pub scope: ScopeKey,
    pub category: Category,
    pub content: String,
    pub verification: Verification,
    pub source: Source,
    pub applicability: Option<Applicability>,
    pub expires_at: Option<f64>,
}

impl MemoryDraft {
    pub fn validate(&self, access: &AccessContext, now: f64) -> Result<(), ContextError> {
        if !self.category.allowed_scopes().contains(&self.scope.kind()) {
            return Err(ContextError::CategoryNotInScope);
        }
        if !access.can_write(&self.scope, self.source.kind()) {
            return Err(ContextError::AccessDenied);
        }
        if !self.source.kind().allowed_categories().contains(&self.category) {
            return Err(ContextError::SourceNotAllowed);
        }
        match self.verification {
            Verification::Candidate => {}
            Verification::Confirmed if self.source.kind().may_create_confirmed() => {}
            _ => return Err(ContextError::VerificationNotAllowed),
        }
        check_content(&self.content)?;
        if self.category.needs_applicability() && self.applicability.is_none() {
            return Err(ContextError::ApplicabilityRequired);
        }
        if self.expires_at.is_some_and(|at| !at.is_finite() || at <= now) {
            return Err(ContextError::ExpiryInvalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordState {
    Active(Verification),
    Expired,
    Invalidated,
    Deleted,
}

/// A stored record (NV19-02 maps rows to this).
#[derive(Debug, Clone)]
pub struct ContextMemory {
    pub id: String,
    pub scope: ScopeKey,
    pub category: Category,
    pub content: String,
    pub verification: Verification,
    pub source: Source,
    pub applicability: Option<Applicability>,
    pub revision: i64,
    pub created_at: f64,
    pub updated_at: f64,
    pub expires_at: Option<f64>,
    pub deleted_at: Option<f64>,
}

/// A row as stored (column strings), for the host's store adapter.
#[derive(Debug, Clone)]
pub struct StoredMemory<'a> {
    pub id: &'a str,
    pub scope_kind: &'a str,
    pub scope_id: &'a str,
    pub category: &'a str,
    pub content: &'a str,
    pub verification: &'a str,
    pub source_kind: &'a str,
    pub source_ref: Option<&'a str>,
    pub device_model: Option<&'a str>,
    pub firmware: Option<&'a str>,
    pub revision: i64,
    pub created_at: f64,
    pub updated_at: f64,
    pub expires_at: Option<f64>,
    pub deleted_at: Option<f64>,
}

impl ContextMemory {
    /// Rebuild a record from stored strings. Unknown names or malformed references fail
    /// closed instead of being guessed. Tombstones keep their empty content.
    pub fn from_stored(row: StoredMemory<'_>) -> Result<Self, ContextError> {
        let applicability = match row.device_model {
            Some(model) => Some(Applicability::new(model, row.firmware)?),
            None if row.firmware.is_some() => return Err(ContextError::ApplicabilityInvalid),
            None => None,
        };
        Ok(Self {
            id: row.id.into(),
            scope: ScopeKey::parse(row.scope_kind, row.scope_id)?,
            category: Category::parse(row.category).ok_or(ContextError::ContentInvalid)?,
            content: row.content.into(),
            verification: Verification::parse(row.verification).ok_or(ContextError::ContentInvalid)?,
            source: Source::new(SourceKind::parse(row.source_kind).ok_or(ContextError::SourceRefInvalid)?, row.source_ref)?,
            applicability,
            revision: row.revision,
            created_at: row.created_at,
            updated_at: row.updated_at,
            expires_at: row.expires_at,
            deleted_at: row.deleted_at,
        })
    }

    pub fn state(&self, now: f64) -> RecordState {
        if self.deleted_at.is_some() {
            RecordState::Deleted
        } else if self.verification == Verification::Invalidated {
            RecordState::Invalidated
        } else if self.expires_at.is_some_and(|at| at <= now) {
            RecordState::Expired
        } else {
            RecordState::Active(self.verification)
        }
    }

    /// Visible to this request at all (candidates included, labelled as such by the caller).
    pub fn readable(&self, access: &AccessContext, now: f64) -> bool {
        access.can_read(&self.scope) && matches!(self.state(now), RecordState::Active(_))
    }

    /// May be presented as a fact: confirmed, active, and — for hardware-bound categories —
    /// matching the current device model/firmware. Still read-only data, never an instruction.
    pub fn usable_as_fact(&self, access: &AccessContext, now: f64, device: Option<(&str, Option<&str>)>) -> bool {
        self.readable(access, now)
            && self.verification == Verification::Confirmed
            && (!self.category.needs_applicability()
                || self.applicability.as_ref().zip(device).is_some_and(|(rule, (model, firmware))| rule.matches(model, firmware)))
    }
}

pub mod experience;
pub mod extract;
pub mod knowledge;
pub mod resume;
pub mod retrieve;
pub mod service;

#[cfg(test)]
pub(crate) mod fake_store;
#[cfg(test)]
mod tests;
