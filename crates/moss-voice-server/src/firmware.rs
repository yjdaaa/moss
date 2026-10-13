//! NV17 inert firmware import and offer eligibility fixture.
//! NOT registered to OTA routes; it cannot send a firmware offer or flash.
#![allow(dead_code)]
use getrandom::fill;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
const FILE_MAX: u64 = 16 * 1024 * 1024;
const TOTAL_MAX: u64 = 256 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirmwareError {
    InvalidInput,
    TooLarge,
    Conflict,
    Io,
    InvalidArtifact,
    Unauthorized,
}
#[derive(Clone, Debug)]
struct Entry {
    board: String,
    version: String,
    filename: String,
    hash: String,
    size: u64,
    enabled: bool,
    revision: u64,
}
struct ArtifactStore {
    root: PathBuf,
    records: HashMap<(String, String), Entry>,
    total: u64,
}
struct FakeGrant<'a> {
    board: &'a str,
    device: &'a str,
    client: &'a str,
    allowed: bool,
    reported_version: &'a str,
}
struct CleanStage(PathBuf);
impl Drop for CleanStage {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn board_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.trim() == s
        && !s.chars().any(char::is_control)
        && !s.contains(['/', '\\'])
}
fn version(v: &str) -> Option<String> {
    if v.is_empty() || v.len() > 64 {
        return None;
    }
    let mut digits = v
        .split('.')
        .map(|part| {
            if part.is_empty() || part.len() > 10 || !part.bytes().all(|x| x.is_ascii_digit()) {
                return None;
            }
            part.parse::<u32>().ok()
        })
        .collect::<Option<Vec<_>>>()?;
    if digits.is_empty() || digits.len() > 5 {
        return None;
    }
    while digits.len() > 1 && digits.last() == Some(&0) {
        digits.pop();
    }
    Some(
        digits
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("."),
    )
}
fn reparse(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}
impl ArtifactStore {
    fn new(path: &Path) -> Result<Self, FirmwareError> {
        if !path.is_absolute() {
            return Err(FirmwareError::InvalidInput);
        }
        let meta = fs::symlink_metadata(path).map_err(|_| FirmwareError::Io)?;
        if !meta.is_dir() || reparse(&meta) {
            return Err(FirmwareError::InvalidInput);
        }
        Ok(Self {
            root: path.canonicalize().map_err(|_| FirmwareError::Io)?,
            records: HashMap::new(),
            total: 0,
        })
    }
    /// Only trusted, already-open file handles may be read. Never accept a
    /// user-provided filename or HTTP URL for remote firmware requests.
    fn import_fake(
        &mut self,
        board: &str,
        release: &str,
        mut source: impl Read,
    ) -> Result<Entry, FirmwareError> {
        if !board_ok(board) {
            return Err(FirmwareError::InvalidInput);
        }
        let release = version(release).ok_or(FirmwareError::InvalidInput)?;
        let mut nonce = [0u8; 16];
        fill(&mut nonce).map_err(|_| FirmwareError::Io)?;
        let temp_path = self.root.join(format!(".nv17-{}.part", hex(&nonce)));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|_| FirmwareError::Io)?;
        let guard = CleanStage(temp_path);
        let mut digest = Sha256::new();
        let mut count = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = source.read(&mut buffer).map_err(|_| FirmwareError::Io)?;
            if n == 0 {
                break;
            }
            count = count.checked_add(n as u64).ok_or(FirmwareError::TooLarge)?;
            if count > FILE_MAX {
                return Err(FirmwareError::TooLarge);
            }
            file.write_all(&buffer[..n])
                .map_err(|_| FirmwareError::Io)?;
            digest.update(&buffer[..n]);
        }
        if count == 0 {
            return Err(FirmwareError::InvalidArtifact);
        }
        file.sync_all().map_err(|_| FirmwareError::Io)?;
        drop(file);
        let sha = hex(&digest.finalize());
        let key = (board.to_owned(), release.clone());
        if let Some(previous) = self.records.get(&key) {
            if previous.hash == sha && previous.size == count {
                return Ok(previous.clone());
            }
            return Err(FirmwareError::Conflict);
        }
        if self.records.len() >= 256 || self.total.saturating_add(count) > TOTAL_MAX {
            return Err(FirmwareError::TooLarge);
        }
        // Full digest of the original board/type prevents filename collisions.
        let board_hash = hex(&Sha256::digest(board.as_bytes()));
        let filename = format!("{}-{}-{}.bin", board_hash, release.replace('.', "~"), sha);
        // Hard-link is create-only. Windows rename could overwrite existing.
        fs::hard_link(&guard.0, self.root.join(&filename)).map_err(|_| FirmwareError::Conflict)?;
        let record = Entry {
            board: board.into(),
            version: release,
            filename,
            hash: sha,
            size: count,
            enabled: false,
            revision: 1,
        };
        self.total += count;
        self.records.insert(key, record.clone());
        Ok(record)
    }
    /// This mock has no desktop command to invoke confirmation in production.
    fn confirm_fake(
        &mut self,
        board: &str,
        release: &str,
        hash: &str,
        revision: u64,
    ) -> Result<Entry, FirmwareError> {
        let release = version(release).ok_or(FirmwareError::InvalidInput)?;
        let item = self
            .records
            .get_mut(&(board.to_owned(), release))
            .ok_or(FirmwareError::Unauthorized)?;
        if item.revision != revision || item.hash != hash {
            return Err(FirmwareError::Unauthorized);
        }
        item.enabled = true;
        item.revision += 1;
        Ok(item.clone())
    }
    fn disable_fake(&mut self, board: &str, release: &str) {
        if let Some(release) = version(release) {
            if let Some(item) = self.records.get_mut(&(board.into(), release)) {
                item.enabled = false;
                item.revision += 1;
            }
        }
    }
    fn offer_fake(&self, device: &FakeGrant<'_>) -> Option<Entry> {
        if !device.allowed
            || device.device.is_empty()
            || device.client.is_empty()
            || !board_ok(device.board)
        {
            return None;
        }
        // An invalid or unknown device version is NOT equivalent to 0.0.
        // Otherwise a bad/malicious OTA report could trigger an offer.
        let reported = version(device.reported_version)?;
        self.records
            .values()
            .filter(|record| {
                record.enabled
                    && record.board == device.board
                    && moss_xiaozhi_proto::is_newer_version(&record.version, &reported)
            })
            .max_by(|a, b| {
                if moss_xiaozhi_proto::is_newer_version(&a.version, &b.version) {
                    std::cmp::Ordering::Greater
                } else if a.version == b.version {
                    std::cmp::Ordering::Equal
                } else {
                    std::cmp::Ordering::Less
                }
            })
            .cloned()
    }
    /// Same open file is SHA-verified before yielding any bytes to caller.
    /// No actual network route uses this function. OS-level TOCTOU/path
    /// revalidation must be fully audited before production download support.
    fn read_fake(&self, record: &Entry, device: &FakeGrant<'_>) -> Result<Vec<u8>, FirmwareError> {
        let current = self
            .records
            .get(&(record.board.clone(), record.version.clone()))
            .ok_or(FirmwareError::Unauthorized)?;
        if !device.allowed
            || device.device.is_empty()
            || device.client.is_empty()
            || device.board != current.board
            || version(device.reported_version).is_none_or(|reported| {
                !moss_xiaozhi_proto::is_newer_version(&current.version, &reported)
            })
            || !current.enabled
            || current.revision != record.revision
            || current.hash != record.hash
        {
            return Err(FirmwareError::Unauthorized);
        }
        let path = self.root.join(&current.filename);
        let metadata = fs::symlink_metadata(&path).map_err(|_| FirmwareError::InvalidArtifact)?;
        if !metadata.is_file()
            || reparse(&metadata)
            || metadata.len() != current.size
            || metadata.len() > FILE_MAX
        {
            return Err(FirmwareError::InvalidArtifact);
        }
        let mut file = File::open(&path).map_err(|_| FirmwareError::InvalidArtifact)?;
        let mut bytes = Vec::new();
        (&mut file)
            .take(FILE_MAX + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| FirmwareError::InvalidArtifact)?;
        if bytes.len() as u64 != current.size || hex(&Sha256::digest(&bytes)) != current.hash {
            return Err(FirmwareError::InvalidArtifact);
        }
        Ok(bytes)
    }
}
/// NV17 synthetic observation journal: a successful firmware download is NOT
/// evidence that firmware was installed. Persisted status is deliberately
/// recovery-safe: an interrupted transfer becomes Unknown, never auto-retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryState { Pending, Downloading, Downloaded, Unknown }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReportState { NotReported, Reported }
#[derive(Debug, Clone)]
struct FakeDelivery {
    device: String,
    artifact_hash: String,
    download: DeliveryState,
    report: ReportState,
}
impl FakeDelivery {
    fn new(device: &str, artifact_hash: &str) -> Result<Self, FirmwareError> {
        if device.is_empty() || artifact_hash.len() != 64
            || !artifact_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(FirmwareError::InvalidInput);
        }
        Ok(Self { device: device.into(), artifact_hash: artifact_hash.into(),
            download: DeliveryState::Pending, report: ReportState::NotReported })
    }
    fn start(&mut self, device: &str) -> Result<(), FirmwareError> {
        if self.device != device || self.download != DeliveryState::Pending {
            return Err(FirmwareError::Unauthorized);
        }
        self.download = DeliveryState::Downloading;
        Ok(())
    }
    fn downloaded(&mut self, device: &str) -> Result<(), FirmwareError> {
        if self.device != device || self.download != DeliveryState::Downloading {
            return Err(FirmwareError::Unauthorized);
        }
        self.download = DeliveryState::Downloaded;
        Ok(())
    }
    fn interrupted(&mut self) {
        if self.download == DeliveryState::Downloading {
            self.download = DeliveryState::Unknown;
        }
    }
    fn report_installed(&mut self, device: &str, hash: &str) -> Result<(), FirmwareError> {
        // Download completion alone is not proof of device installation.
        if self.device != device || self.artifact_hash != hash
            || self.download != DeliveryState::Downloaded {
            return Err(FirmwareError::Unauthorized);
        }
        self.report = ReportState::Reported;
        Ok(())
    }
}
#[cfg(test)]
mod download_mock;
#[cfg(test)]
mod tests {
    use super::*;
    fn store() -> (tempfile::TempDir, ArtifactStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(tmp.path()).unwrap();
        (tmp, store)
    }
    fn grant<'a>(board: &'a str, ver: &'a str) -> FakeGrant<'a> {
        FakeGrant {
            board,
            device: "fake-device",
            client: "fake-client",
            allowed: true,
            reported_version: ver,
        }
    }
    #[test]
    fn nv17_download_completion_never_implies_device_installed_and_restart_is_unknown() {
        let sha = "a".repeat(64);
        let mut transfer = FakeDelivery::new("device-1", &sha).unwrap();
        assert_eq!(transfer.report, ReportState::NotReported);
        assert!(transfer.report_installed("device-1", &sha).is_err());
        transfer.start("device-1").unwrap();
        transfer.interrupted();
        assert_eq!(transfer.download, DeliveryState::Unknown);
        assert!(transfer.start("device-1").is_err(), "no implicit retry after interruption");
        assert!(transfer.report_installed("device-1", &sha).is_err());
        let mut fresh = FakeDelivery::new("device-2", &sha).unwrap();
        fresh.start("device-2").unwrap();
        fresh.downloaded("device-2").unwrap();
        assert_eq!(fresh.report, ReportState::NotReported);
        assert!(fresh.report_installed("device-1", &sha).is_err());
        assert!(fresh.report_installed("device-2", &"b".repeat(64)).is_err());
        fresh.report_installed("device-2", &sha).unwrap();
        assert_eq!(fresh.report, ReportState::Reported);
    }
    #[test]
    fn default_disabled_and_explicit_hash_revision_required() {
        let (_tmp, mut catalog) = store();
        let initial = catalog
            .import_fake("BOARD-A", "1.02.0", &b"FAKE_FIRMWARE_A"[..])
            .unwrap();
        assert_eq!(initial.version, "1.2");
        assert!(!initial.enabled);
        assert!(catalog.offer_fake(&grant("BOARD-A", "1")).is_none());
        assert_eq!(
            catalog.confirm_fake("BOARD-A", "1.2", "wrong", 1).err(),
            Some(FirmwareError::Unauthorized)
        );
        let confirmed = catalog
            .confirm_fake("BOARD-A", "1.2", &initial.hash, 1)
            .unwrap();
        assert!(catalog.offer_fake(&grant("BOARD-A", "1")).is_some());
        assert!(catalog.offer_fake(&grant("BOARD-A", "1.2")).is_none());
        assert!(catalog.offer_fake(&grant("OTHER", "1")).is_none());
        assert_eq!(
            catalog
                .read_fake(&confirmed, &grant("BOARD-A", "1"))
                .unwrap(),
            b"FAKE_FIRMWARE_A"
        );
        catalog.disable_fake("BOARD-A", "1.2");
        assert!(catalog.offer_fake(&grant("BOARD-A", "1")).is_none());
        assert_eq!(
            catalog.read_fake(&confirmed, &grant("BOARD-A", "1")).err(),
            Some(FirmwareError::Unauthorized)
        );
    }
    #[test]
    fn duplicate_padded_versions_and_conflicts_cannot_overwrite() {
        let (_tmp, mut catalog) = store();
        let original = catalog.import_fake("板型甲", "1.02.0", &b"A"[..]).unwrap();
        let same = catalog.import_fake("板型甲", "1.2", &b"A"[..]).unwrap();
        assert_eq!(original.hash, same.hash);
        assert_eq!(catalog.records.len(), 1);
        assert_eq!(
            catalog.import_fake("板型甲", "1.002", &b"B"[..]).err(),
            Some(FirmwareError::Conflict)
        );
        for (board, release) in [
            ("../bad", "1"),
            ("valid", "1..2"),
            ("valid", "1.2-rc1"),
            ("valid", "1/2"),
        ] {
            assert_eq!(
                catalog.import_fake(board, release, &b"x"[..]).err(),
                Some(FirmwareError::InvalidInput)
            );
        }
    }
    #[test]
    fn truncated_or_tampered_artifacts_and_oversized_import_rejected() {
        let (tmp, mut catalog) = store();
        let massive = vec![0u8; (FILE_MAX + 1) as usize];
        assert_eq!(
            catalog.import_fake("B", "1", &massive[..]).err(),
            Some(FirmwareError::TooLarge)
        );
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
        let imported = catalog
            .import_fake("B", "1", &b"VERIFIED_FAKE"[..])
            .unwrap();
        let enabled = catalog.confirm_fake("B", "1", &imported.hash, 1).unwrap();
        fs::write(tmp.path().join(&enabled.filename), b"tampered").unwrap();
        assert_eq!(
            catalog.read_fake(&enabled, &grant("B", "0")).err(),
            Some(FirmwareError::InvalidArtifact)
        );
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }
    #[test]
    fn fake_identity_revocation_and_stale_confirmation_denied() {
        let (_tmp, mut catalog) = store();
        let original = catalog.import_fake("B", "9", &b"FAKE"[..]).unwrap();
        let enabled = catalog.confirm_fake("B", "9", &original.hash, 1).unwrap();
        let mut device = grant("B", "1");
        device.allowed = false;
        assert!(catalog.offer_fake(&device).is_none());
        assert_eq!(
            catalog.read_fake(&enabled, &device).err(),
            Some(FirmwareError::Unauthorized)
        );
        device.allowed = true;
        device.client = "";
        assert!(catalog.offer_fake(&device).is_none());
        device.client = "fake-client";
        assert_eq!(
            catalog.read_fake(&original, &device).err(),
            Some(FirmwareError::Unauthorized)
        );
        assert!(catalog.offer_fake(&grant("B", "garbled")).is_none());
    }
}
