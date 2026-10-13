//! Optional bounded, read-only legacy HA backup risk classification.
//! Does not extract secrets or claim that an unrecognized file is safe.
use std::{fs::File,io::Read,path::Path};
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum BackupRisk{SuspectedLegacyCredential,EncryptedCredentialMaterial,UnknownTreatAsSensitive}
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub struct BackupAudit{pub risk:BackupRisk,pub inspected_bytes:u64}
const LIMIT:u64=64*1024*1024;
const MARKERS:[&[u8];4]=[b"voice.homeassistant.token",b"long_lived_access_token",b"access_token",b"dpapi-user-v1:"];
pub fn audit_selected_file(path:&Path)->Result<BackupAudit,&'static str>{
    // Refuse network/relative paths and redirected parent directories. Never scan a root.
    if !path.is_absolute() || path.components().any(|part| matches!(part,std::path::Component::ParentDir)) {
        return Err("unsafe path");
    }
    #[cfg(windows)]
    {
        use std::path::{Component,Prefix};
        use std::os::windows::fs::MetadataExt;
        if !matches!(path.components().next(),Some(Component::Prefix(prefix)) if matches!(prefix.kind(),Prefix::Disk(_)|Prefix::VerbatimDisk(_))) {
            return Err("unsafe path prefix");
        }
        let mut current=std::path::PathBuf::new();
        for component in path.components() {
            current.push(component.as_os_str());
            let metadata=std::fs::symlink_metadata(&current).map_err(|_|"unsafe path component")?;
            if metadata.is_symlink() || metadata.file_attributes() & 0x400 != 0 {
                return Err("redirected path component");
            }
        }
    }
    let info=std::fs::symlink_metadata(path).map_err(|_|"file metadata failed")?;
    if !info.file_type().is_file()||info.len()==0||info.len()>LIMIT{return Err("invalid bounded regular file");}
    let mut f=File::open(path).map_err(|_|"file opening failed")?;
    let mut buffer=[0u8;16384];let mut tail=Vec::new();let mut size=0u64;let mut seen=[false;4];
    loop{
        let n=f.read(&mut buffer).map_err(|_|"file read failed")?;
        if n==0{break;}
        size+=n as u64;if size>LIMIT{return Err("file too large");}
        let mut chunk=std::mem::take(&mut tail);
        chunk.extend_from_slice(&buffer[..n]);
        for (i,marker) in MARKERS.iter().enumerate(){
            seen[i]|=chunk.windows(marker.len()).any(|w|w.eq_ignore_ascii_case(marker));
        }
        let keep=MARKERS.iter().map(|m|m.len()).max().unwrap_or(1)-1;
        tail=chunk[chunk.len().saturating_sub(keep)..].to_vec();
    }
    if size!=info.len(){return Err("file changed during inspection");}
    let risk=if seen[0]||seen[1]||seen[2]{BackupRisk::SuspectedLegacyCredential}
    else if seen[3]{BackupRisk::EncryptedCredentialMaterial}
    else{BackupRisk::UnknownTreatAsSensitive};
    Ok(BackupAudit{risk,inspected_bytes:size})
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test] fn nv15_legacy_file_audit_has_no_secret_output_and_detects_chunk_boundaries(){
        let temp=tempfile::tempdir().unwrap();let f=temp.path().join("synthetic");
        std::fs::write(&f,format!("{}voice.homeassistant.token{}", "x".repeat(16376),"synthetic_token_no_real_credentials")).unwrap();
        assert_eq!(audit_selected_file(&f).unwrap().risk,BackupRisk::SuspectedLegacyCredential);
        let other=temp.path().join("cipher");std::fs::write(&other,b"dpapi-user-v1:fake").unwrap();
        assert_eq!(audit_selected_file(&other).unwrap().risk,BackupRisk::EncryptedCredentialMaterial);
        std::fs::write(&other,b"unknown data").unwrap();
        assert_eq!(audit_selected_file(&other).unwrap().risk,BackupRisk::UnknownTreatAsSensitive);
    }
    #[test] fn nv15_audit_refuses_relative_and_parent_traversal(){
        assert!(audit_selected_file(Path::new("synthetic-backup.json")).is_err());
        let temp=tempfile::tempdir().unwrap();
        assert!(audit_selected_file(&temp.path().join(".." ).join("backup.json")).is_err());
    }
    #[cfg(unix)]
    #[test] fn nv15_audit_refuses_symlink(){
        use std::os::unix::fs::symlink;
        let temp=tempfile::tempdir().unwrap();
        let target=temp.path().join("target");std::fs::write(&target,b"fake").unwrap();
        let link=temp.path().join("link");symlink(&target,&link).unwrap();
        assert!(audit_selected_file(&link).is_err());
    }
    #[test] fn nv15_legacy_file_audit_rejects_directory_and_oversize(){
        let temp=tempfile::tempdir().unwrap();assert!(audit_selected_file(temp.path()).is_err());
        let file=temp.path().join("oversize");let f=File::create(&file).unwrap();f.set_len(LIMIT+1).unwrap();
        assert!(audit_selected_file(&file).is_err());
    }
}
