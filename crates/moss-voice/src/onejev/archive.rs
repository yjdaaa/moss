//! 推理引擎压缩包（tar.gz / zip）的安全解压，含符号链接还原与取消检查。

use super::*;

// 只有 Unix 解压会还原符号链接（见 extract_tar_gz），Windows 上不编译这个函数
#[cfg(unix)]
fn safe_intra_dir_symlink_target(target: &Path) -> bool {
    use std::path::Component;
    let mut comps = target.components();
    matches!(comps.next(), Some(Component::Normal(_))) && comps.next().is_none()
}

pub(super) fn extract_tar_gz(
    archive_file: &Path,
    dest: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let file = fs::File::open(archive_file).map_err(|e| format!("打开 tar.gz 失败：{e}"))?;
    let gz = flate2::read::GzDecoder::new(BufReader::new(file));
    let mut archive = tar::Archive::new(gz);
    let entries = archive
        .entries()
        .map_err(|_| "tar.gz 压缩包损坏".to_owned())?;
    for entry in entries {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".into());
        }
        let mut entry = entry.map_err(|_| "tar.gz 条目损坏".to_owned())?;
        let path = entry
            .path()
            .map_err(|_| "tar.gz 路径无效".to_owned())?
            .into_owned();
        if !safe_relative(&path) {
            return Err(format!("压缩包含不安全路径：{}", path.display()));
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_file() || entry_type.is_dir() {
            if !entry
                .unpack_in(dest)
                .map_err(|e| format!("解压 tar.gz 失败：{e}"))?
            {
                return Err(format!("压缩包含越界路径：{}", path.display()));
            }
        } else if entry_type.is_symlink() {
            #[cfg(unix)]
            {
                if let Ok(Some(link_target)) = entry.link_name() {
                    let link_target = link_target.into_owned();
                    if safe_intra_dir_symlink_target(&link_target) {
                        let full_dest = dest.join(&path);
                        if let Some(parent) = full_dest.parent() {
                            fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
                        }
                        let _ = fs::remove_file(&full_dest);
                        std::os::unix::fs::symlink(&link_target, &full_dest)
                            .map_err(|e| format!("创建动态库符号链接失败：{e}"))?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// 轻量纯 Rust ZIP 解压（仅接受相对安全路径、Stored(0) 与 Deflate(8) 普通文件/目录，拒绝路径穿越）。
pub(super) fn extract_zip(
    archive_file: &Path,
    dest: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    extract_zip_with_symlinks(archive_file, dest, cancel, false)
}

pub(crate) fn extract_zip_with_symlinks(
    archive_file: &Path,
    dest: &Path,
    cancel: &AtomicBool,
    preserve_symlinks: bool,
) -> Result<(), String> {
    let bytes = fs::read(archive_file).map_err(|e| format!("读取 zip 失败：{e}"))?;
    extract_zip_bytes_with_options(&bytes, dest, cancel, preserve_symlinks)
}

#[cfg(test)]
pub(super) fn extract_zip_bytes(
    bytes: &[u8],
    dest: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    extract_zip_bytes_with_options(bytes, dest, cancel, false)
}

fn write_zip_payload<W: Write>(
    bytes: &[u8],
    local_offset: usize,
    method: u16,
    comp_size: usize,
    uncomp_size: usize,
    out: &mut W,
) -> Result<(), String> {
    if local_offset + 30 > bytes.len() || &bytes[local_offset..local_offset + 4] != b"PK\x03\x04" {
        return Err("zip 本地文件头损坏".into());
    }
    let l_fn_len =
        u16::from_le_bytes([bytes[local_offset + 26], bytes[local_offset + 27]]) as usize;
    let l_ex_len =
        u16::from_le_bytes([bytes[local_offset + 28], bytes[local_offset + 29]]) as usize;
    let data_start = local_offset + 30 + l_fn_len + l_ex_len;
    let data_end = data_start.saturating_add(comp_size);
    if data_end > bytes.len() {
        return Err("zip 压缩数据越界".into());
    }
    let slice = &bytes[data_start..data_end];
    let written = match method {
        0 => {
            out.write_all(slice)
                .map_err(|e| format!("写入文件失败：{e}"))?;
            slice.len()
        }
        8 => {
            let mut decoder = flate2::read::DeflateDecoder::new(Cursor::new(slice));
            std::io::copy(&mut decoder, out).map_err(|e| format!("解压 deflate 数据失败：{e}"))?
                as usize
        }
        other => return Err(format!("不支持的 zip 压缩算法：{other}")),
    };
    if written != uncomp_size {
        return Err(format!("zip 解压大小不匹配（{written} != {uncomp_size}）"));
    }
    Ok(())
}

pub(crate) fn extract_zip_bytes_with_options(
    bytes: &[u8],
    dest: &Path,
    cancel: &AtomicBool,
    preserve_symlinks: bool,
) -> Result<(), String> {
    if bytes.len() < 22 {
        return Err("zip 文件过短".into());
    }
    let min_eocd = bytes.len().saturating_sub(65_557);
    let mut eocd_pos = None;
    for pos in (min_eocd..=bytes.len() - 22).rev() {
        if &bytes[pos..pos + 4] == b"PK\x05\x06" {
            let comment_len = u16::from_le_bytes([bytes[pos + 20], bytes[pos + 21]]) as usize;
            if pos + 22 + comment_len == bytes.len() {
                eocd_pos = Some(pos);
                break;
            }
        }
    }
    let eocd = eocd_pos.ok_or_else(|| "无效的 zip 尾部目录".to_owned())?;
    let total_entries = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]) as usize;
    let cd_offset = u32::from_le_bytes([
        bytes[eocd + 16],
        bytes[eocd + 17],
        bytes[eocd + 18],
        bytes[eocd + 19],
    ]) as usize;
    let mut cd = cd_offset;
    for _ in 0..total_entries {
        if cancel.load(Ordering::SeqCst) {
            return Err("已取消".into());
        }
        if cd + 46 > bytes.len() || &bytes[cd..cd + 4] != b"PK\x01\x02" {
            return Err("zip 中央目录损坏".into());
        }
        let method = u16::from_le_bytes([bytes[cd + 10], bytes[cd + 11]]);
        let comp_size = u32::from_le_bytes([
            bytes[cd + 20],
            bytes[cd + 21],
            bytes[cd + 22],
            bytes[cd + 23],
        ]) as usize;
        let uncomp_size = u32::from_le_bytes([
            bytes[cd + 24],
            bytes[cd + 25],
            bytes[cd + 26],
            bytes[cd + 27],
        ]) as usize;
        let fn_len = u16::from_le_bytes([bytes[cd + 28], bytes[cd + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[cd + 30], bytes[cd + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[cd + 32], bytes[cd + 33]]) as usize;
        let ext_attr = u32::from_le_bytes([
            bytes[cd + 38],
            bytes[cd + 39],
            bytes[cd + 40],
            bytes[cd + 41],
        ]);
        let local_offset = u32::from_le_bytes([
            bytes[cd + 42],
            bytes[cd + 43],
            bytes[cd + 44],
            bytes[cd + 45],
        ]) as usize;
        let header_end = cd + 46 + fn_len + extra_len + comment_len;
        if header_end > bytes.len() {
            return Err("zip 条目越界".into());
        }
        let raw_name = std::str::from_utf8(&bytes[cd + 46..cd + 46 + fn_len])
            .map_err(|_| "zip 文件名编码无效".to_owned())?;
        cd = header_end;

        let normalized = raw_name.replace('\\', "/");
        let is_dir = normalized.ends_with('/');
        let clean = normalized.trim_end_matches('/');
        if clean.is_empty() {
            continue;
        }
        let rel = Path::new(clean);
        if !safe_relative(rel) {
            return Err(format!("zip 包含不安全路径：{raw_name}"));
        }
        // Unix mode bits in upper 16 bits of external attributes
        let unix_mode = (ext_attr >> 16) & 0o170000;
        let out_path = dest.join(rel);
        if unix_mode == 0o120000 {
            if !preserve_symlinks {
                continue;
            }
            if uncomp_size == 0 || uncomp_size > 1024 {
                return Err(format!("zip 符号链接长度异常：{raw_name}"));
            }
            let mut raw_target = Vec::with_capacity(uncomp_size);
            write_zip_payload(
                bytes,
                local_offset,
                method,
                comp_size,
                uncomp_size,
                &mut raw_target,
            )?;
            #[cfg(unix)]
            {
                let target_str = std::str::from_utf8(&raw_target)
                    .map_err(|_| format!("zip 符号链接目标编码无效：{raw_name}"))?;
                let target_path = Path::new(target_str);
                if !safe_intra_dir_symlink_target(target_path) {
                    return Err(format!(
                        "zip 包含不安全符号链接：{raw_name} -> {target_str}"
                    ));
                }
                if let Some(parent) = out_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
                }
                let _ = fs::remove_file(&out_path);
                std::os::unix::fs::symlink(target_path, &out_path)
                    .map_err(|e| format!("创建动态库符号链接失败：{e}"))?;
            }
            #[cfg(not(unix))]
            {
                let _ = (raw_target, out_path);
            }
            continue;
        }
        if is_dir {
            fs::create_dir_all(&out_path).map_err(|e| format!("创建目录失败：{e}"))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建父目录失败：{e}"))?;
        }
        let mut out_file =
            fs::File::create(&out_path).map_err(|e| format!("写入解压文件失败：{e}"))?;
        write_zip_payload(
            bytes,
            local_offset,
            method,
            comp_size,
            uncomp_size,
            &mut out_file,
        )?;
    }
    Ok(())
}
