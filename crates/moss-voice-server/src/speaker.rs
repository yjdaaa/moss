//! NV16 synthetic speaker-vector policy; no microphone, model, storage or
//! production authorization is connected. All real speaker gates fail closed.
#![allow(dead_code)]
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const DIM_MIN: usize = 64;
const DIM_MAX: usize = 1024;
const MAX_PEOPLE: usize = 64;
const MAX_ACTIVE: usize = 2;
const MAX_QUEUED: usize = 16;
const MAX_WAIT: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpeakerError {
    InvalidSample,
    BadDimension,
    InvalidIdentity,
    ModelMismatch,
    ConflictingClips,
    Duplicate,
    Capacity,
}
#[derive(Clone)]
struct FakeClip {
    vector: Vec<f32>,
    voiced_ms: u32,
    clipped: bool,
    multi_voice: bool,
}
struct Template {
    id: String,
    name: String,
    embedding: Vec<f32>,
}
#[derive(Debug, PartialEq)]
enum IdentityHint {
    Unknown,
    SyntheticMatch { id: String, score: f32, margin: f32 },
}
struct SpeakerMatcher {
    model: String,
    dimension: usize,
    people: Vec<Template>,
    /// Never true outside controlled test calibration. No auth path exists.
    calibrated: bool,
    debug_threshold: f32,
    debug_margin: f32,
}
fn model_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 96
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}
fn id_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 64
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn name_ok(v: &str) -> bool {
    !v.trim().is_empty()
        && v.trim() == v
        && v.chars().count() <= 64
        && !v.chars().any(char::is_control)
}
fn normalized(values: &[f32], dim: usize) -> Result<Vec<f32>, SpeakerError> {
    if values.len() != dim || !(DIM_MIN..=DIM_MAX).contains(&dim) {
        return Err(SpeakerError::BadDimension);
    }
    let mut sum = 0.0f64;
    for &value in values {
        if !value.is_finite() {
            return Err(SpeakerError::InvalidSample);
        }
        sum += f64::from(value).powi(2);
    }
    if !sum.is_finite() || sum <= 1e-16 {
        return Err(SpeakerError::InvalidSample);
    }
    let norm = sum.sqrt();
    Ok(values
        .iter()
        .map(|v| (f64::from(*v) / norm) as f32)
        .collect())
}
fn validate(clip: &FakeClip, dim: usize) -> Result<Vec<f32>, SpeakerError> {
    if !(2_000..=10_000).contains(&clip.voiced_ms) || clip.clipped || clip.multi_voice {
        return Err(SpeakerError::InvalidSample);
    }
    normalized(&clip.vector, dim)
}
fn similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x * y)
        .sum::<f32>()
        .clamp(-1.0, 1.0)
}
impl SpeakerMatcher {
    fn disabled(model: &str, dimension: usize) -> Result<Self, SpeakerError> {
        if !model_ok(model) {
            return Err(SpeakerError::ModelMismatch);
        }
        if !(DIM_MIN..=DIM_MAX).contains(&dimension) {
            return Err(SpeakerError::BadDimension);
        }
        Ok(Self {
            model: model.into(),
            dimension,
            people: vec![],
            calibrated: false,
            debug_threshold: 0.70,
            debug_margin: 0.05,
        })
    }
    fn enroll_fake(
        &mut self,
        id: &str,
        name: &str,
        model: &str,
        clips: &[FakeClip],
    ) -> Result<(), SpeakerError> {
        if !id_ok(id) || !name_ok(name) {
            return Err(SpeakerError::InvalidIdentity);
        }
        if self.model != model {
            return Err(SpeakerError::ModelMismatch);
        }
        if clips.len() != 3 {
            return Err(SpeakerError::InvalidSample);
        }
        if self.people.len() >= MAX_PEOPLE {
            return Err(SpeakerError::Capacity);
        }
        if self.people.iter().any(|p| p.id == id || p.name == name) {
            return Err(SpeakerError::Duplicate);
        }
        let embeddings = clips
            .iter()
            .map(|c| validate(c, self.dimension))
            .collect::<Result<Vec<_>, _>>()?;
        // Every enrollment utterance must agree, not merely a mean in which
        // two contradictory samples could cancel the third.
        for i in 0..3 {
            for j in i + 1..3 {
                if similarity(&embeddings[i], &embeddings[j]) < 0.70 {
                    return Err(SpeakerError::ConflictingClips);
                }
            }
        }
        let avg = (0..self.dimension)
            .map(|k| (embeddings[0][k] + embeddings[1][k] + embeddings[2][k]) / 3.0)
            .collect::<Vec<_>>();
        let embedding = normalized(&avg, self.dimension)?;
        self.people.push(Template {
            id: id.into(),
            name: name.into(),
            embedding,
        });
        Ok(())
    }
    fn recognize_fake(
        &self,
        model: &str,
        generation: u64,
        current: u64,
        clip: &FakeClip,
    ) -> IdentityHint {
        if !self.calibrated
            || model != self.model
            || generation != current
            || self.people.is_empty()
        {
            return IdentityHint::Unknown;
        }
        let Ok(vector) = validate(clip, self.dimension) else {
            return IdentityHint::Unknown;
        };
        let mut candidates = self
            .people
            .iter()
            .map(|p| (p, similarity(&vector, &p.embedding)))
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
        let (best, top) = candidates[0];
        let runner_up = candidates.get(1).map(|c| c.1).unwrap_or(-1.0);
        let margin = (top - runner_up).max(0.0);
        if top < self.debug_threshold || margin < self.debug_margin {
            return IdentityHint::Unknown;
        }
        IdentityHint::SyntheticMatch {
            id: best.id.clone(),
            score: top,
            margin,
        }
    }
    fn remove_fake(&mut self, id: &str) {
        self.people.retain(|entry| entry.id != id);
        self.calibrated = false;
    }
    /// No match grants physical, financial, home-control or laser authority.
    fn may_authorize_action(&self, _hint: &IdentityHint) -> bool {
        false
    }
}

#[derive(Default)]
struct QuotaState {
    active: HashSet<String>,
    queued: usize,
}
#[derive(Clone, Default)]
struct SpeakerQuota {
    state: Arc<Mutex<QuotaState>>,
}
struct Slot {
    device: String,
    state: Arc<Mutex<QuotaState>>,
}
impl Drop for Slot {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.active.remove(&self.device);
        }
    }
}
impl SpeakerQuota {
    fn acquire(&self, device: &str) -> Result<Slot, SpeakerError> {
        if !id_ok(device) {
            return Err(SpeakerError::InvalidIdentity);
        }
        let mut state = self.state.lock().map_err(|_| SpeakerError::Capacity)?;
        if state.active.len() >= MAX_ACTIVE || state.active.contains(device) {
            return Err(SpeakerError::Capacity);
        }
        state.active.insert(device.into());
        Ok(Slot {
            device: device.into(),
            state: self.state.clone(),
        })
    }
    fn enqueue(&self) -> Result<(), SpeakerError> {
        let mut state = self.state.lock().map_err(|_| SpeakerError::Capacity)?;
        if state.queued >= MAX_QUEUED {
            return Err(SpeakerError::Capacity);
        }
        state.queued += 1;
        Ok(())
    }
    fn dequeue(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.queued = state.queued.saturating_sub(1);
        }
    }
}
fn timeout_unknown(started: Instant) -> bool {
    started.elapsed() >= MAX_WAIT
}

/// NV16 inert owner-isolated memory fixture. Caller-provided speaker names or
/// model text are never accepted as authority; only a matcher-owned id may
/// access a person summary. No production speaker memory or IO is wired.
#[derive(Default)]
struct SyntheticPersonMemory {
    summaries: std::collections::HashMap<String, (u64, String)>,
}
impl SyntheticPersonMemory {
    fn read(&self, matcher: &SpeakerMatcher, hint: &IdentityHint) -> Option<&str> {
        let IdentityHint::SyntheticMatch { id, .. } = hint else {
            return None;
        };
        if !matcher.calibrated || !matcher.people.iter().any(|p| p.id == *id) {
            return None;
        }
        self.summaries.get(id).map(|(_, summary)| summary.as_str())
    }
    fn save(
        &mut self,
        matcher: &SpeakerMatcher,
        hint: &IdentityHint,
        expected_revision: u64,
        summary: &str,
    ) -> Result<u64, SpeakerError> {
        if !matches!(hint, IdentityHint::SyntheticMatch { .. })
            || summary.len() > 4_096
            || summary.chars().any(char::is_control)
            || self.read(matcher, hint).is_none() && expected_revision != 0
        {
            return Err(SpeakerError::InvalidIdentity);
        }
        let IdentityHint::SyntheticMatch { id, .. } = hint else {
            unreachable!()
        };
        if !matcher.calibrated || !matcher.people.iter().any(|p| p.id == *id) {
            return Err(SpeakerError::InvalidIdentity);
        }
        let current = self
            .summaries
            .get(id)
            .map(|(revision, _)| *revision)
            .unwrap_or(0);
        if current != expected_revision {
            return Err(SpeakerError::Duplicate);
        }
        let next = current.checked_add(1).ok_or(SpeakerError::Capacity)?;
        self.summaries
            .insert(id.clone(), (next, summary.to_owned()));
        Ok(next)
    }
    fn delete(&mut self, id: &str) {
        self.summaries.remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn clip(index: usize) -> FakeClip {
        let mut vector = vec![0.0f32; 64];
        vector[index] = 1.0;
        FakeClip {
            vector,
            voiced_ms: 3_000,
            clipped: false,
            multi_voice: false,
        }
    }
    fn clips(index: usize) -> Vec<FakeClip> {
        vec![clip(index); 3]
    }
    #[test]
    fn person_memory_isolated_by_matcher_owned_identity_and_revision() {
        let mut matcher = SpeakerMatcher::disabled("synthetic", 64).unwrap();
        matcher
            .enroll_fake("person-a", "甲", "synthetic", &clips(0))
            .unwrap();
        matcher
            .enroll_fake("person-b", "乙", "synthetic", &clips(1))
            .unwrap();
        let a_untrusted = IdentityHint::SyntheticMatch {
            id: "person-a".into(),
            score: 1.,
            margin: 1.,
        };
        let mut memory = SyntheticPersonMemory::default();
        assert!(memory.save(&matcher, &a_untrusted, 0, "private").is_err());
        matcher.calibrated = true; // synthetic fixture only; never released
        let a = matcher.recognize_fake("synthetic", 1, 1, &clip(0));
        let b = matcher.recognize_fake("synthetic", 1, 1, &clip(1));
        assert_eq!(memory.save(&matcher, &a, 0, "person-a-only"), Ok(1));
        assert_eq!(memory.read(&matcher, &a), Some("person-a-only"));
        assert_eq!(memory.read(&matcher, &b), None);
        assert_eq!(memory.read(&matcher, &IdentityHint::Unknown), None);
        assert_eq!(
            memory.save(&matcher, &a, 0, "stale"),
            Err(SpeakerError::Duplicate)
        );
        assert_eq!(memory.save(&matcher, &a, 1, "updated"), Ok(2));
        matcher.remove_fake("person-a");
        memory.delete("person-a");
        assert_eq!(memory.read(&matcher, &a), None);
        assert_eq!(memory.read(&matcher, &b), None);
        assert!(!matcher.may_authorize_action(&a));
    }
    #[test]
    fn no_calibration_means_unknown_and_no_action_authorization() {
        let mut matcher = SpeakerMatcher::disabled("test-model-v1", 64).unwrap();
        matcher
            .enroll_fake("person-a", "甲", "test-model-v1", &clips(0))
            .unwrap();
        assert_eq!(
            matcher.recognize_fake("test-model-v1", 1, 1, &clip(0)),
            IdentityHint::Unknown
        );
        matcher.calibrated = true; // strictly cfg(test) fake calibration
        let hint = matcher.recognize_fake("test-model-v1", 1, 1, &clip(0));
        assert!(matches!(hint,IdentityHint::SyntheticMatch{ref id,..} if id=="person-a"));
        assert!(!matcher.may_authorize_action(&hint));
        matcher.remove_fake("person-a");
        assert_eq!(
            matcher.recognize_fake("test-model-v1", 1, 1, &clip(0)),
            IdentityHint::Unknown
        );
    }
    #[test]
    fn invalid_vector_audio_metadata_and_inconsistent_enrollment_rejected() {
        let mut matcher = SpeakerMatcher::disabled("test-model-v1", 64).unwrap();
        for bad in [
            FakeClip {
                vector: vec![0.0; 64],
                ..clip(0)
            },
            FakeClip {
                vector: vec![f32::NAN; 64],
                ..clip(0)
            },
            FakeClip {
                vector: vec![f32::INFINITY; 64],
                ..clip(0)
            },
            FakeClip {
                vector: vec![1.0; 63],
                ..clip(0)
            },
            FakeClip {
                voiced_ms: 1000,
                ..clip(0)
            },
            FakeClip {
                voiced_ms: 11000,
                ..clip(0)
            },
            FakeClip {
                clipped: true,
                ..clip(0)
            },
            FakeClip {
                multi_voice: true,
                ..clip(0)
            },
        ] {
            let mut input = clips(0);
            input[0] = bad;
            assert!(
                matcher
                    .enroll_fake("a", "A", "test-model-v1", &input)
                    .is_err()
            );
        }
        let mut opposing = clips(0);
        opposing[1].vector[0] = -1.0;
        assert_eq!(
            matcher.enroll_fake("a", "A", "test-model-v1", &opposing),
            Err(SpeakerError::ConflictingClips)
        );
        assert!(matcher.people.is_empty());
    }
    #[test]
    fn stale_turn_wrong_model_top_two_ambiguity_and_id_validation() {
        let mut matcher = SpeakerMatcher::disabled("test-model-v1", 64).unwrap();
        matcher
            .enroll_fake("a", "A", "test-model-v1", &clips(0))
            .unwrap();
        matcher
            .enroll_fake("b", "B", "test-model-v1", &clips(1))
            .unwrap();
        matcher.calibrated = true;
        assert_eq!(
            matcher.recognize_fake("other", 7, 7, &clip(0)),
            IdentityHint::Unknown
        );
        assert_eq!(
            matcher.recognize_fake("test-model-v1", 6, 7, &clip(0)),
            IdentityHint::Unknown
        );
        let mut near = clip(0);
        near.vector[0] = 0.71;
        near.vector[1] = 0.70;
        assert_eq!(
            matcher.recognize_fake("test-model-v1", 7, 7, &near),
            IdentityHint::Unknown
        );
        assert_eq!(
            matcher.enroll_fake("a", "dup", "test-model-v1", &clips(2)),
            Err(SpeakerError::Duplicate)
        );
        assert_eq!(
            matcher.enroll_fake("c", "C", "evil-model", &clips(2)),
            Err(SpeakerError::ModelMismatch)
        );
        assert_eq!(
            matcher.enroll_fake("../../", "C", "test-model-v1", &clips(2)),
            Err(SpeakerError::InvalidIdentity)
        );
        assert!(SpeakerMatcher::disabled("unsafe\nmodel", 64).is_err());
    }
    #[test]
    fn concurrency_queue_and_timeout_bound() {
        let quota = SpeakerQuota::default();
        let first = quota.acquire("d1").unwrap();
        assert!(quota.acquire("d1").is_err());
        let second = quota.acquire("d2").unwrap();
        assert!(quota.acquire("d3").is_err());
        drop(first);
        let third = quota.acquire("d3").unwrap();
        drop(second);
        drop(third);
        for _ in 0..MAX_QUEUED {
            quota.enqueue().unwrap();
        }
        assert_eq!(quota.enqueue().err(), Some(SpeakerError::Capacity));
        quota.dequeue();
        quota.enqueue().unwrap();
        assert!(timeout_unknown(Instant::now() - Duration::from_secs(2)));
        assert!(!timeout_unknown(Instant::now()));
    }
}
