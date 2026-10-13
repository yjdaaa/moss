//! Secure, bounded, read-only local-file discovery and selection.
//! No network, file write, process spawn, model API, or music playback.
use getrandom::fill;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, Metadata},
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

const MAX_TRACKS: usize = 10_000;
const MAX_SCANNED_ENTRIES: usize = 50_000;
const MAX_DIRECTORIES: usize = 5_000;
const MAX_DEPTH: usize = 8;
const MAX_TRACK_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CANDIDATES: usize = 5;
const MAX_QUERY_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MusicError {
    InvalidRoot,
    Unavailable,
    UnsafePath,
    LimitExceeded,
    InvalidQuery,
    EntropyFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackCandidate {
    pub track_id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MusicChoice {
    Selected(TrackCandidate),
    Ambiguous(Vec<TrackCandidate>),
    Missing,
}

#[derive(Clone)]
struct IndexedTrack {
    candidate: TrackCandidate,
    relative: PathBuf,
    size: u64,
    mtime: SystemTime,
}

/// Paths are deliberately private; callers may expose ONLY TrackCandidate.
/// A stale index is never trusted for reading: resolve() checks it again.
pub struct MusicIndex {
    root: PathBuf,
    tracks: Vec<IndexedTrack>,
}

fn is_link_or_reparse(meta: &Metadata) -> bool {
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

/// Check every existing component, not just the canonicalized final target.
fn no_link_ancestry(path: &Path) -> Result<(), MusicError> {
    for component in path.ancestors() {
        let meta = fs::symlink_metadata(component).map_err(|_| MusicError::Unavailable)?;
        if is_link_or_reparse(&meta) {
            return Err(MusicError::UnsafePath);
        }
    }
    Ok(())
}

fn sensitive_catalog_root(root: &Path) -> bool {
    // Avoid accidentally making the entire volume, a user profile root,
    // an operating-system directory, or credentials into a music library.
    if root.parent().is_none() {
        return true;
    }
    let forbidden_ancestors = [
        "windows",
        "program files",
        "program files (x86)",
        "programdata",
        "system volume information",
        "$recycle.bin",
        ".git",
        ".ssh",
    ];
    if root
        .ancestors()
        .filter_map(Path::file_name)
        .any(|part| forbidden_ancestors.contains(&part.to_string_lossy().to_lowercase().as_str()))
    {
        return true;
    }
    let leaf = root
        .file_name()
        .and_then(|part| part.to_str())
        .unwrap_or("")
        .to_lowercase();
    if matches!(
        leaf.as_str(),
        "users" | "home" | "appdata" | "local" | "roaming" | "locallow" | "documents and settings"
    ) {
        return true;
    }
    root.parent()
        .and_then(Path::file_name)
        .and_then(|part| part.to_str())
        .is_some_and(|part| matches!(part.to_lowercase().as_str(), "users" | "home"))
}
/// Prevent indexing credentials hidden inside an otherwise allowed music root.
fn sensitive_descendant(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()).is_some_and(|name| {
        matches!(name.to_ascii_lowercase().as_str(),
            ".git" | ".ssh" | ".aws" | ".config" | ".gnupg" | "appdata" |
            "credentials" | "secrets" | "private" | "tokens" | "keys")
    })
}

fn valid_local_root(root: &Path) -> Result<PathBuf, MusicError> {
    if !root.is_absolute()
        || sensitive_catalog_root(root)
        || root
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(MusicError::InvalidRoot);
    }
    #[cfg(windows)]
    {
        // UNC, device paths and extended namespace prefixes are excluded:
        // music directories must be explicitly chosen local drive paths.
        let src = root.to_str().ok_or(MusicError::InvalidRoot)?;
        if src.starts_with(r"\\") || src.starts_with("//") {
            return Err(MusicError::InvalidRoot);
        }
    }
    no_link_ancestry(root)?;
    let canonical = root.canonicalize().map_err(|_| MusicError::Unavailable)?;
    if !canonical.is_dir() {
        return Err(MusicError::InvalidRoot);
    }
    no_link_ancestry(&canonical)?;
    if sensitive_catalog_root(&canonical) {
        return Err(MusicError::UnsafePath);
    }
    Ok(canonical)
}

/// OS-verified opened-handle path, never just the caller-supplied pathname.
/// On unreviewed operating systems fail closed rather than guessing.
fn actual_handle_path(file: &std::fs::File) -> Result<PathBuf, MusicError> {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
        let mut chars = vec![0u16; 32768];
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                chars.as_mut_ptr(),
                chars.len() as u32,
                0,
            )
        } as usize;
        if length == 0 || length >= chars.len() {
            return Err(MusicError::UnsafePath);
        }
        let actual = PathBuf::from(OsString::from_wide(&chars[..length]));
        return actual.canonicalize().map_err(|_| MusicError::UnsafePath);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let actual = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|_| MusicError::UnsafePath)?;
        return actual.canonicalize().map_err(|_| MusicError::UnsafePath);
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = file;
        Err(MusicError::UnsafePath)
    }
}

fn acceptable_name(name: &str) -> bool {
    !name.is_empty() && name.chars().count() <= 180 && !name.chars().any(char::is_control)
}

fn media_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "wav" | "flac" | "mp3" | "ogg"
            )
        })
}

fn track_id(relative: &Path) -> String {
    let hash = Sha256::digest(relative.as_os_str().as_encoded_bytes());
    hash[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn normalize(value: &str) -> String {
    value.trim().to_lowercase()
}

impl MusicIndex {
    /// The only public scan takes an explicit path (no implicit user-home
    /// probing), caps depth/file count/entry count, and rejects every symlink,
    /// Windows junction and other filesystem reparse point.
    pub fn scan(root: &Path) -> Result<Self, MusicError> {
        Self::scan_limits(root, MAX_TRACKS, MAX_SCANNED_ENTRIES, MAX_DIRECTORIES)
    }

    fn scan_limits(
        root: &Path,
        track_limit: usize,
        entry_limit: usize,
        dir_limit: usize,
    ) -> Result<Self, MusicError> {
        let root = valid_local_root(root)?;
        let mut pending = vec![(root.clone(), 0usize)];
        let mut seen_dirs = 0usize;
        let mut seen_entries = 0usize;
        let mut tracks: Vec<IndexedTrack> = Vec::new();
        let mut ids = HashSet::new();
        while let Some((dir, depth)) = pending.pop() {
            seen_dirs += 1;
            if seen_dirs > dir_limit {
                return Err(MusicError::LimitExceeded);
            }
            // Do not tolerate replaced directories during a scan.
            if is_link_or_reparse(&fs::symlink_metadata(&dir).map_err(|_| MusicError::Unavailable)?)
            {
                return Err(MusicError::UnsafePath);
            }
            for entry in fs::read_dir(&dir).map_err(|_| MusicError::Unavailable)? {
                seen_entries += 1;
                if seen_entries > entry_limit {
                    return Err(MusicError::LimitExceeded);
                }
                let entry = entry.map_err(|_| MusicError::Unavailable)?;
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path).map_err(|_| MusicError::Unavailable)?;
                if is_link_or_reparse(&metadata) {
                    continue;
                }
                if sensitive_descendant(&path) { continue; }
                if metadata.is_dir() {
                    if depth < MAX_DEPTH {
                        pending.push((path, depth + 1));
                    }
                    continue;
                }
                if !metadata.is_file()
                    || !media_extension(&path)
                    || metadata.len() == 0
                    || metadata.len() > MAX_TRACK_BYTES
                {
                    continue;
                }
                let title = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .ok_or(MusicError::UnsafePath)?;
                if !acceptable_name(title) {
                    continue;
                }
                let relative = path
                    .strip_prefix(&root)
                    .map_err(|_| MusicError::UnsafePath)?
                    .to_path_buf();
                let id = track_id(&relative);
                if !ids.insert(id.clone()) {
                    return Err(MusicError::UnsafePath);
                }
                tracks.push(IndexedTrack {
                    candidate: TrackCandidate {
                        track_id: id,
                        title: title.to_owned(),
                    },
                    relative,
                    size: metadata.len(),
                    mtime: metadata.modified().map_err(|_| MusicError::Unavailable)?,
                });
                if tracks.len() > track_limit {
                    return Err(MusicError::LimitExceeded);
                }
            }
        }
        tracks.sort_by(|left, right| {
            left.candidate
                .title
                .cmp(&right.candidate.title)
                .then_with(|| left.relative.cmp(&right.relative))
        });
        Ok(Self { root, tracks })
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Exactly one of query or random must be supplied. Exact filename
    /// matches win over contains matches. Ambiguity NEVER plays a random song.
    pub fn choose(&self, query: Option<&str>, random: bool) -> Result<MusicChoice, MusicError> {
        if random {
            if query.is_some() {
                return Err(MusicError::InvalidQuery);
            }
            if self.tracks.is_empty() {
                return Ok(MusicChoice::Missing);
            }
            let mut bytes = [0u8; 8];
            fill(&mut bytes).map_err(|_| MusicError::EntropyFailed)?;
            let position = (u64::from_le_bytes(bytes) % self.tracks.len() as u64) as usize;
            return Ok(MusicChoice::Selected(
                self.tracks[position].candidate.clone(),
            ));
        }
        let query = query.ok_or(MusicError::InvalidQuery)?;
        if query.is_empty()
            || query.chars().count() > MAX_QUERY_CHARS
            || query.chars().any(char::is_control)
        {
            return Err(MusicError::InvalidQuery);
        }
        let desired = normalize(query);
        if desired.is_empty() {
            return Err(MusicError::InvalidQuery);
        }
        let exact: Vec<_> = self
            .tracks
            .iter()
            .filter(|track| normalize(&track.candidate.title) == desired)
            .collect();
        let matches = if exact.is_empty() {
            self.tracks
                .iter()
                .filter(|track| normalize(&track.candidate.title).contains(&desired))
                .collect::<Vec<_>>()
        } else {
            exact
        };
        Ok(match matches.as_slice() {
            [] => MusicChoice::Missing,
            [single] => MusicChoice::Selected(single.candidate.clone()),
            multiple => MusicChoice::Ambiguous(
                multiple
                    .iter()
                    .take(MAX_CANDIDATES)
                    .map(|track| track.candidate.clone())
                    .collect(),
            ),
        })
    }

    /// Trusted decoder side ONLY. Revalidate relative path and metadata;
    /// caller must additionally hold an opened read-only file handle for
    /// handle-based checks to remove OS-level TOCTOU races (NV14-2).
    pub fn resolve(&self, id: &str) -> Result<PathBuf, MusicError> {
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(MusicError::InvalidQuery);
        }
        let track = self
            .tracks
            .iter()
            .find(|track| track.candidate.track_id == id)
            .ok_or(MusicError::Unavailable)?;
        // Internal root is already canonical (Windows may prefix it with
        // \\?\). Recheck it without reapplying the external UNC input guard.
        no_link_ancestry(&self.root)?;
        let now = self
            .root
            .canonicalize()
            .map_err(|_| MusicError::Unavailable)?;
        if now != self.root || !now.is_dir() {
            return Err(MusicError::UnsafePath);
        }
        let path = self.root.join(&track.relative);
        if track.relative.components().any(|part| match part {
            Component::Normal(name) => sensitive_descendant(Path::new(name)),
            _ => true,
        }) { return Err(MusicError::UnsafePath); }
        no_link_ancestry(&path)?;
        let canonical = path.canonicalize().map_err(|_| MusicError::Unavailable)?;
        if !canonical.starts_with(&self.root) {
            return Err(MusicError::UnsafePath);
        }
        let metadata = fs::symlink_metadata(&path).map_err(|_| MusicError::Unavailable)?;
        if !metadata.is_file()
            || is_link_or_reparse(&metadata)
            || metadata.len() != track.size
            || metadata.modified().map_err(|_| MusicError::Unavailable)? != track.mtime
        {
            return Err(MusicError::Unavailable);
        }
        Ok(canonical)
    }

    /// Open a selected track using an OS-verified handle. The caller-supplied
    /// ID is not a path; the opened file must still resolve under this root.
    pub fn open(&self, id: &str) -> Result<(std::fs::File, String), MusicError> {
        let canonical = self.resolve(id)?;
        let track = self
            .tracks
            .iter()
            .find(|item| item.candidate.track_id == id)
            .ok_or(MusicError::Unavailable)?;
        #[cfg(windows)]
        let opened = {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_FLAG_OPEN_REPARSE_POINT prevents a replaced final link
            // from quietly following another file during open.
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(0x0020_0000)
                .open(&canonical)
                .map_err(|_| MusicError::Unavailable)?
        };
        #[cfg(not(windows))]
        let opened = std::fs::File::open(&canonical).map_err(|_| MusicError::Unavailable)?;
        let final_path = actual_handle_path(&opened)?;
        if final_path != canonical || !final_path.starts_with(&self.root) {
            return Err(MusicError::UnsafePath);
        }
        let metadata = opened.metadata().map_err(|_| MusicError::Unavailable)?;
        if !metadata.is_file()
            || metadata.len() != track.size
            || metadata.modified().map_err(|_| MusicError::Unavailable)? != track.mtime
        {
            return Err(MusicError::Unavailable);
        }
        let extension = canonical
            .extension()
            .and_then(|ext| ext.to_str())
            .ok_or(MusicError::UnsafePath)?
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "wav" | "flac" | "mp3" | "ogg") {
            return Err(MusicError::UnsafePath);
        }
        Ok((opened, extension))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canary(root: &Path, path: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"synthetic-fixture").unwrap();
    }

    #[test]
    fn case_insensitive_unicode_exact_contains_and_ambiguity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        canary(root, "dir one/清晨 测试.MP3");
        canary(root, "dir two/清晨 测试.FLAC");
        canary(root, "dir three/晚间歌.wav");
        canary(root, "notes/notes.txt");
        let index = MusicIndex::scan(root).unwrap();
        assert_eq!(index.len(), 3);
        let MusicChoice::Ambiguous(candidates) = index.choose(Some("清晨 测试"), false).unwrap()
        else {
            panic!("same title must be ambiguous");
        };
        assert_eq!(candidates.len(), 2);
        assert_ne!(candidates[0].track_id, candidates[1].track_id);
        assert!(
            candidates
                .iter()
                .all(|c| !c.track_id.contains("dir") && !c.title.contains("dir"))
        );
        assert!(matches!(
            index.choose(Some("晚间"), false).unwrap(),
            MusicChoice::Selected(_)
        ));
        assert!(matches!(
            index.choose(Some("nothing"), false).unwrap(),
            MusicChoice::Missing
        ));
        assert!(matches!(
            index.choose(None, true).unwrap(),
            MusicChoice::Selected(_)
        ));
        assert_eq!(
            index.choose(Some("晚间"), true),
            Err(MusicError::InvalidQuery)
        );
        assert_eq!(index.choose(None, false), Err(MusicError::InvalidQuery));
        assert_eq!(
            index.choose(Some("\n"), false),
            Err(MusicError::InvalidQuery)
        );
        assert_eq!(
            index.choose(Some(&"a".repeat(201)), false),
            Err(MusicError::InvalidQuery)
        );
    }
    #[test]
    fn nv14_sensitive_descendant_directories_are_excluded_from_library() {
        let temp=tempfile::tempdir().unwrap();
        canary(temp.path(),"album/normal.mp3");
        for name in [".ssh", ".aws", "AppData", "credentials", "Secrets", ".git"] {
            canary(temp.path(), &format!("album/{name}/synthetic_secret.wav"));
        }
        let index=MusicIndex::scan(temp.path()).unwrap();
        assert_eq!(index.len(),1);
        assert!(matches!(index.choose(Some("normal"),false),Ok(MusicChoice::Selected(_))));
        assert_eq!(index.choose(Some("synthetic_secret"),false),Ok(MusicChoice::Missing));
    }
    #[test]
    fn stale_moved_modified_and_unknown_ids_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        canary(root, "静夜.ogg");
        let index = MusicIndex::scan(root).unwrap();
        let MusicChoice::Selected(selected) = index.choose(Some("静夜"), false).unwrap() else {
            panic!()
        };
        assert_eq!(
            index.resolve(&selected.track_id).unwrap(),
            root.join("静夜.ogg").canonicalize().unwrap()
        );
        fs::write(root.join("静夜.ogg"), b"changed file").unwrap();
        assert_eq!(
            index.resolve(&selected.track_id),
            Err(MusicError::Unavailable)
        );
        fs::rename(root.join("静夜.ogg"), root.join("other.ogg")).unwrap();
        assert!(index.resolve(&selected.track_id).is_err());
        assert_eq!(index.resolve("../elsewhere"), Err(MusicError::InvalidQuery));
    }
    #[test]
    fn recursion_and_track_file_scanning_budgets_are_hard_errors() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for name in ["1.mp3", "2.wav", "3.flac"] {
            canary(root, name);
        }
        assert!(matches!(
            MusicIndex::scan_limits(root, 2, 50, 50),
            Err(MusicError::LimitExceeded)
        ));
        assert!(matches!(
            MusicIndex::scan_limits(root, 50, 2, 50),
            Err(MusicError::LimitExceeded)
        ));
        for depth in 1..=9 {
            canary(root, &format!("{}/song.wav", vec!["sub"; depth].join("/")));
        }
        let index = MusicIndex::scan(root).unwrap();
        assert_eq!(index.len(), 3 + 8);
        assert!(
            !index
                .choose(Some("song"), false)
                .is_ok_and(|c| matches!(c, MusicChoice::Selected(_)))
        );
    }

    #[test]
    fn forbids_volume_system_and_profile_roots_before_fs_scan() {
        #[cfg(windows)]
        for root in [
            r"C:\",
            r"C:\Windows\Music",
            r"C:\Program Files\Music",
            r"C:\Users",
            r"C:\Users\someone",
            r"C:\Users\someone\AppData",
            r"C:\Users\someone\.ssh",
            r"C:\ProgramData\Other",
        ] {
            assert!(sensitive_catalog_root(Path::new(root)), "accepted {root}");
        }
        let temp = tempfile::tempdir().unwrap();
        assert!(!sensitive_catalog_root(temp.path()));
    }
    #[test]
    fn opened_file_handle_is_verified_and_stale_track_fails_closed() {
        use std::io::Read;
        let temp = tempfile::tempdir().unwrap();
        canary(temp.path(), "artist/song.wav");
        let index = MusicIndex::scan(temp.path()).unwrap();
        let MusicChoice::Selected(track) = index.choose(Some("song"), false).unwrap() else {
            panic!()
        };
        let (mut file, ext) = index.open(&track.track_id).unwrap();
        assert_eq!(ext, "wav");
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, b"synthetic-fixture");
        assert_eq!(
            index.open("../outside").err(),
            Some(MusicError::InvalidQuery)
        );
        fs::write(temp.path().join("artist/song.wav"), b"modified").unwrap();
        assert!(index.open(&track.track_id).is_err());
    }
    #[test]
    fn links_and_outside_roots_are_never_indexed_or_resolved() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        canary(outside.path(), "private.mp3");
        let root = temp.path();
        canary(root, "legitimate.mp3");
        assert!(matches!(
            MusicIndex::scan(Path::new("relative")),
            Err(MusicError::InvalidRoot)
        ));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path().join("private.mp3"), root.join("link.mp3"))
                .unwrap();
            std::os::unix::fs::symlink(outside.path(), root.join("bad-directory")).unwrap();
            assert!(matches!(
                MusicIndex::scan(&root.join("bad-directory")),
                Err(MusicError::UnsafePath)
            ));
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(
                outside.path().join("private.mp3"),
                root.join("link.mp3"),
            )
            .ok();
        }
        let index = MusicIndex::scan(root).unwrap();
        assert_eq!(index.len(), 1);
        assert!(matches!(
            index.choose(Some("private"), false).unwrap(),
            MusicChoice::Missing
        ));
    }
}
