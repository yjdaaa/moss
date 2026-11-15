use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendPreflight {
    pub status: String,
    pub expected_sha256: String,
    pub actual_sha256: Option<String>,
    pub gcode_path: Option<std::path::PathBuf>,
    pub workflow_snapshot_valid: bool,
    pub calibration_sha_valid: bool,
}

pub fn authorize_send(preflight: &SendPreflight) -> Result<(), String> {
    if preflight.status != "pending" {
        return Err("job 不处于 pending，拒绝发送".into());
    }
    if preflight.expected_sha256.len() != 64
        || !preflight
            .expected_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("缺少有效的 expected_gcode_sha256，拒绝发送".into());
    }

    let actual_sha = if let Some(path) = &preflight.gcode_path {
        if !path.is_file() {
            return Err("G-code 文件不存在，拒绝发送".into());
        }
        let bytes = std::fs::read(path).map_err(|e| format!("读取 G-code 文件失败: {e}"))?;
        hex::encode(Sha256::digest(&bytes))
    } else if let Some(sha) = &preflight.actual_sha256 {
        sha.clone()
    } else {
        return Err("缺少待校验的 G-code 来源，拒绝发送".into());
    };

    if !preflight.expected_sha256.eq_ignore_ascii_case(&actual_sha) {
        return Err("G-code SHA-256 与发送前重算值不一致，拒绝发送".into());
    }
    if !preflight.workflow_snapshot_valid && !preflight.calibration_sha_valid {
        return Err("缺少有效 workflow 快照或 calibration SHA，拒绝发送".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_preflight_verifies_three_gates_strictly() {
        let temp = tempfile::tempdir().unwrap();
        let gcode_file = temp.path().join("cut.gcode");
        std::fs::write(&gcode_file, b"G1 X10 Y10").unwrap();
        let correct_sha = hex::encode(Sha256::digest(b"G1 X10 Y10"));

        let valid = SendPreflight {
            status: "pending".into(),
            expected_sha256: correct_sha.clone(),
            actual_sha256: None,
            gcode_path: Some(gcode_file.clone()),
            workflow_snapshot_valid: true,
            calibration_sha_valid: false,
        };
        assert!(authorize_send(&valid).is_ok());

        // Gate 1: Non-pending
        let mut bad_status = valid.clone();
        bad_status.status = "running".into();
        assert!(authorize_send(&bad_status).is_err());

        // Gate 2: SHA mismatch
        let mut bad_sha = valid.clone();
        bad_sha.expected_sha256 = "0".repeat(64);
        assert!(authorize_send(&bad_sha).is_err());

        // Gate 3: Neither workflow nor calibration
        let mut bad_auth = valid.clone();
        bad_auth.workflow_snapshot_valid = false;
        bad_auth.calibration_sha_valid = false;
        assert!(authorize_send(&bad_auth).is_err());
    }
}
