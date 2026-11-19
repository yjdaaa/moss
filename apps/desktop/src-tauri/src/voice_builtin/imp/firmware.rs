//! 固件 OTA 宿主（下载绑定设备地址并校验哈希）与固件导入、按台推送管理命令。

use super::*;

/// NV17 固件 OTA（2026-10-15）：推送记录在 SQLite，固件文件在运行目录 `firmware/<sha256>.bin`。
pub(super) struct DesktopFirmwareHost {
    pub(super) db: Arc<Mutex<laser_runtime::RuntimeDb>>,
    pub(super) dir: std::path::PathBuf,
}
/// 推送有效期：设备在这段时间内开机检查更新才会拿到。
const FIRMWARE_OFFER_SECONDS: f64 = 24.0 * 3600.0;
fn random_hex(len: usize) -> Result<String, CenterError> {
    let mut bytes = vec![0u8; len / 2];
    getrandom::fill(&mut bytes).map_err(|_| internal("随机数生成失败"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
impl moss_voice_server::firmware::FirmwareHost for DesktopFirmwareHost {
    fn offer(
        &self,
        device_id: &str,
        current: &moss_voice_server::firmware::DeviceFirmware<'_>,
    ) -> Option<moss_voice_server::firmware::FirmwarePush> {
        use moss_voice_server::firmware::{FirmwarePush, PushDecision};
        let resolved = self.decide_offer(device_id, current)?;
        match resolved.decision {
            PushDecision::Send { force } => Some(FirmwarePush {
                version: resolved.version,
                token: resolved.token,
                force,
            }),
            PushDecision::AlreadyRunning | PushDecision::Skip => None,
        }
    }
    fn fetch(&self, token: &str, peer: &str) -> Option<Vec<u8>> {
        self.fetch_with_after_read(token, peer, || {})
    }
    fn reported(&self, device_id: &str, current: &moss_voice_server::firmware::DeviceFirmware<'_>) {
        // 设备上跑的就是推送的这一份（按 ELF 哈希；旧固件没有哈希时按版本号）且绑定推送单号与镜像哈希才记为已升级
        if let Some(resolved) = self.decide_offer(device_id, current)
            && resolved.decision == moss_voice_server::firmware::PushDecision::AlreadyRunning
            && let Ok(db) = self.db.lock()
        {
            let _ = db.firmware_delivery_reported(
                &resolved.delivery_id,
                device_id,
                &resolved.artifact_sha256,
                &resolved.version,
                now(),
            );
        }
    }
}

struct OfferDecision {
    delivery_id: String,
    version: String,
    token: String,
    artifact_sha256: String,
    decision: moss_voice_server::firmware::PushDecision,
}

impl DesktopFirmwareHost {
    fn fetch_with_after_read(
        &self,
        token: &str,
        peer: &str,
        after_read: impl FnOnce(),
    ) -> Option<Vec<u8>> {
        let offer = {
            let db = self.db.lock().ok()?;
            let offer = db.firmware_offer_by_token(token, now()).ok()??;
            // 令牌只给这台设备：下载方必须是这台已允许设备最近一次请求 OTA 的地址
            let device = db.voice_device(&offer.device_id).ok()??;
            if !device.is_allowed() || device.last_ip != peer {
                return None;
            }
            offer
        };
        let bytes = std::fs::read(self.dir.join(format!("{}.bin", offer.artifact_sha256))).ok()?;
        if sha256_hex(&bytes) != offer.artifact_sha256 {
            return None;
        }
        after_read();
        // 放锁读盘后复验授权、令牌、修订和镜像；仅首次下载执行 pending → downloading CAS。
        let db = self.db.lock().ok()?;
        let current_offer = db.firmware_offer_by_token(token, now()).ok()??;
        if current_offer.delivery_id != offer.delivery_id
            || current_offer.revision != offer.revision
            || current_offer.download_status != offer.download_status
            || current_offer.device_id != offer.device_id
            || current_offer.artifact_sha256 != offer.artifact_sha256
        {
            return None;
        }
        let device = db.voice_device(&current_offer.device_id).ok()??;
        if !device.is_allowed() || device.last_ip != peer {
            return None;
        }
        if current_offer.download_status == "pending"
            && !db
                .update_firmware_download_if_revision(
                    &current_offer.delivery_id,
                    current_offer.revision,
                    "downloading",
                    now(),
                )
                .ok()?
        {
            return None;
        }
        Some(bytes)
    }

    /// 这台设备的有效推送（单号、版本、令牌、镜像哈希）以及对它上报的当前固件该怎么办。
    fn decide_offer(
        &self,
        device_id: &str,
        current: &moss_voice_server::firmware::DeviceFirmware<'_>,
    ) -> Option<OfferDecision> {
        let (row, token) = {
            let db = self.db.lock().ok()?;
            db.active_firmware_offer_row(device_id, now()).ok()??
        };
        let never_downloaded = row.download_status == "pending";
        // 只读文件开头的应用描述，不把整份固件读进来
        let push_elf = std::fs::File::open(self.dir.join(format!("{}.bin", row.artifact_sha256)))
            .ok()
            .and_then(|mut file| {
                use std::io::Read;
                let mut head = [0u8; 208];
                file.read_exact(&mut head).ok()?;
                moss_voice_server::firmware::image_elf_sha256(&head)
            });
        let decision = moss_voice_server::firmware::decide_push(
            &row.firmware_version,
            push_elf.as_deref(),
            current,
            never_downloaded,
        );
        Some(OfferDecision {
            delivery_id: row.delivery_id,
            version: row.firmware_version,
            token,
            artifact_sha256: row.artifact_sha256,
            decision,
        })
    }
}

impl BuiltinVoice {
    pub(super) fn firmware_dir(&self) -> std::path::PathBuf {
        self.host.runtime_root.path.join("firmware")
    }

    pub fn firmware_status(&self) -> Result<Value, CenterError> {
        let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
        let records = db
            .list_firmware_deliveries()
            .map_err(|_| internal("读取固件记录失败"))?;
        let artifacts = db
            .list_firmware_artifacts()
            .map_err(|_| internal("读取固件清单失败"))?;
        let devices=db.voice_devices().map_err(|_|internal("读取设备失败"))?.into_iter().filter(|d|d.is_allowed())
                .map(|d|serde_json::json!({"device_id":d.device_id,"name":d.name,"model":d.model,"firmware_version":d.firmware_version,"last_seen":d.last_seen}))
                .collect::<Vec<_>>();
        Ok(
            serde_json::json!({"enabled":true,"records":records,"artifacts":artifacts,"devices":devices,"now":now(),"offer_hours":FIRMWARE_OFFER_SECONDS/3600.0}),
        )
    }

    /// 导入用户选中的固件：必须是 ESP32 应用镜像，版本号与项目名取自固件头。
    pub fn firmware_import(&self, path: &std::path::Path) -> Result<Value, CenterError> {
        let invalid = |message: &str| CenterError::new("firmware_invalid", 400, message);
        let meta = std::fs::metadata(path).map_err(|_| invalid("无法读取所选文件"))?;
        if !meta.is_file() || meta.len() as usize > moss_voice_server::firmware::FIRMWARE_MAX_BYTES
        {
            return Err(invalid("请选择 16 MB 以内的固件文件"));
        }
        let bytes = std::fs::read(path).map_err(|_| invalid("无法读取所选文件"))?;
        let image = moss_voice_server::firmware::parse_app_image(&bytes).map_err(invalid)?;
        let sha = sha256_hex(&bytes);
        let dir = self.firmware_dir();
        std::fs::create_dir_all(&dir).map_err(|e| internal(format!("创建固件目录失败：{e}")))?;
        let target = dir.join(format!("{sha}.bin"));
        if !target.exists() {
            let temp = dir.join(format!(".{sha}.part"));
            std::fs::write(&temp, &bytes)
                .and_then(|()| std::fs::rename(&temp, &target))
                .map_err(|e| {
                    let _ = std::fs::remove_file(&temp);
                    internal(format!("保存固件失败：{e}"))
                })?;
        }
        let project: String = image.project.chars().take(32).collect();
        self.host
            .db
            .lock()
            .map_err(|_| internal("数据库锁失败"))?
            .add_firmware_artifact(&sha, &image.version, &project, bytes.len() as i64, now())
            .map_err(|_| internal("登记固件失败"))?;
        Ok(
            serde_json::json!({"sha256":sha,"version":image.version,"project":project,"size":bytes.len()}),
        )
    }

    /// 推送 / 取消 / 删除固件。推送必须逐台、明确确认，且固件版本比设备当前版本新（否则设备不会升级）。
    pub fn firmware_manage(&self, input: &Value) -> Result<Value, CenterError> {
        let invalid = || CenterError::new("firmware_invalid", 400, "固件操作格式无效");
        let object = input.as_object().ok_or_else(invalid)?;
        let action = input["action"].as_str().ok_or_else(invalid)?;
        let allowed: &[&str] = match action {
            "offer" => &["action", "device_id", "sha256", "confirmed"],
            "cancel" => &["action", "delivery_id"],
            "delete" => &["action", "sha256", "confirmed"],
            _ => return Err(invalid()),
        };
        if object.len() != allowed.len() || object.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(invalid());
        }
        let db = self.host.db.lock().map_err(|_| internal("数据库锁失败"))?;
        match action {
            "offer" => {
                if input["confirmed"].as_bool() != Some(true) {
                    return Err(invalid());
                }
                let device_id = input["device_id"].as_str().ok_or_else(invalid)?;
                let sha = input["sha256"].as_str().ok_or_else(invalid)?;
                let device = db
                    .voice_device(device_id)
                    .map_err(|_| internal("读取设备失败"))?
                    .filter(|d| d.is_allowed())
                    .ok_or_else(|| {
                        CenterError::new("firmware_device_invalid", 409, "只能推送给已允许的设备")
                    })?;
                let artifact = db
                    .list_firmware_artifacts()
                    .map_err(|_| internal("读取固件清单失败"))?
                    .into_iter()
                    .find(|a| a.sha256 == sha)
                    .ok_or_else(|| CenterError::new("firmware_missing", 404, "找不到这个固件"))?;
                // 版本号不更新也可以推：固件自带 ELF 哈希时，设备下次请求 OTA 会按内容比对，
                // 不是同一份就强制安装（同版本号重新编译、降级）。没有哈希可比时才要求版本号更新。
                let comparable = std::fs::File::open(
                    self.host
                        .runtime_root
                        .path
                        .join("firmware")
                        .join(format!("{sha}.bin")),
                )
                .ok()
                .and_then(|mut file| {
                    use std::io::Read;
                    let mut head = [0u8; 208];
                    file.read_exact(&mut head).ok()?;
                    moss_voice_server::firmware::image_elf_sha256(&head)
                })
                .is_some();
                if !comparable
                    && !moss_voice_server::firmware::is_newer(
                        &artifact.version,
                        &device.firmware_version,
                    )
                {
                    return Err(CenterError::new(
                        "firmware_not_newer",
                        409,
                        "固件版本不比设备当前版本新，且固件里没有可比对的 ELF 哈希，设备不会升级",
                    ));
                }
                let (delivery, token) = (format!("fw-{}", random_hex(16)?), random_hex(32)?);
                db.create_firmware_offer(
                    &delivery,
                    device_id,
                    sha,
                    &artifact.version,
                    &token,
                    now() + FIRMWARE_OFFER_SECONDS,
                    now(),
                )
                .map_err(|_| internal("创建推送失败"))?;
            }
            "cancel" => {
                let id = input["delivery_id"].as_str().ok_or_else(invalid)?;
                if !db
                    .cancel_firmware_offer(id, now())
                    .map_err(|_| internal("取消推送失败"))?
                {
                    return Err(CenterError::new(
                        "firmware_conflict",
                        409,
                        "这条推送已完成或已取消",
                    ));
                }
            }
            "delete" => {
                if input["confirmed"].as_bool() != Some(true) {
                    return Err(invalid());
                }
                let sha = input["sha256"].as_str().ok_or_else(invalid)?;
                if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(invalid());
                }
                if !db
                    .delete_firmware_artifact(sha, now())
                    .map_err(|_| internal("删除固件失败"))?
                {
                    return Err(CenterError::new(
                        "firmware_in_use",
                        409,
                        "还有推送在用这个固件，请先取消推送",
                    ));
                }
                let _ = std::fs::remove_file(self.firmware_dir().join(format!("{sha}.bin")));
            }
            _ => {}
        }
        drop(db);
        self.firmware_status()
    }
}

#[cfg(test)]
mod firmware_host_tests {
    use super::*;
    use moss_voice_server::firmware::{DeviceFirmware, FirmwareHost, FirmwarePush};
    fn running<'a>(version: &'a str, elf: Option<&'a str>) -> DeviceFirmware<'a> {
        DeviceFirmware {
            version,
            elf_sha256: elf,
        }
    }

    /// 同版本号重新编译：设备上报的 ELF 哈希与推送的固件不同就带 force 下发一次；
    /// 设备刷完上报同一个哈希才记为已升级（只是版本号相同不算）。
    #[test]
    fn same_version_rebuild_is_forced_once_and_confirmed_by_elf_hash() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap()));
        let mut bytes = vec![0u8; 2048];
        bytes[0] = 0xE9;
        bytes[176..208].copy_from_slice(&[0xAB; 32]);
        let (new_elf, old_elf) = ("ab".repeat(32), "cd".repeat(32));
        let sha = sha256_hex(&bytes);
        std::fs::write(dir.path().join(format!("{sha}.bin")), &bytes).unwrap();
        {
            let db = db.lock().unwrap();
            db.voice_device_seen("dev", "client", "board", "1.1.0", "192.168.1.9", now())
                .unwrap();
            db.set_voice_device_status("dev", laser_runtime::VOICE_DEVICE_ALLOWED, now())
                .unwrap();
            db.add_firmware_artifact(&sha, "1.1.0", "", 2048, now())
                .unwrap();
            db.create_firmware_offer(
                "fw-1",
                "dev",
                &sha,
                "1.1.0",
                &"a".repeat(32),
                now() + 60.0,
                now(),
            )
            .unwrap();
        }
        let host = DesktopFirmwareHost {
            db: db.clone(),
            dir: dir.path().to_path_buf(),
        };
        let status = || {
            db.lock().unwrap().list_firmware_deliveries().unwrap()[0]
                .device_report_status
                .clone()
        };
        // 版本号相同但内容不同：不算已升级，强制下发
        host.reported("dev", &running("1.1.0", Some(&old_elf)));
        assert_eq!(status(), "not_reported", "版本号相同但不是这一份");
        assert_eq!(
            host.offer("dev", &running("1.1.0", Some(&old_elf))),
            Some(FirmwarePush {
                version: "1.1.0".into(),
                token: "a".repeat(32),
                force: true
            })
        );
        // 开始下载过之后不再强制（刷写失败不会反复重刷）
        host.fetch(&"a".repeat(32), "192.168.1.9").unwrap();
        assert_eq!(host.offer("dev", &running("1.1.0", Some(&old_elf))), None);
        // 设备刷完上报同一个 ELF 哈希：记为已升级
        host.reported("dev", &running("1.1.0", Some(&new_elf)));
        assert_eq!(status(), "reported");
    }

    /// 下载必须来自这台已允许设备最近请求 OTA 的地址，文件按 SHA-256 核对，第一次下载记为“已开始下载”。
    #[test]
    fn download_is_bound_to_device_ip_and_verified_by_hash() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap()));
        let bytes = vec![0xE9u8; 2048];
        let sha = sha256_hex(&bytes);
        std::fs::write(dir.path().join(format!("{sha}.bin")), &bytes).unwrap();
        {
            let db = db.lock().unwrap();
            db.voice_device_seen("dev", "client", "board", "1.0.0", "192.168.1.9", now())
                .unwrap();
            db.set_voice_device_status("dev", laser_runtime::VOICE_DEVICE_ALLOWED, now())
                .unwrap();
            db.add_firmware_artifact(&sha, "1.1.0", "", 2048, now())
                .unwrap();
            db.create_firmware_offer(
                "fw-1",
                "dev",
                &sha,
                "1.1.0",
                &"a".repeat(32),
                now() + 60.0,
                now(),
            )
            .unwrap();
        }
        let host = DesktopFirmwareHost {
            db: db.clone(),
            dir: dir.path().to_path_buf(),
        };
        assert_eq!(
            host.offer("dev", &running("1.0.0", None)),
            Some(FirmwarePush {
                version: "1.1.0".into(),
                token: "a".repeat(32),
                force: false
            })
        );
        assert_eq!(
            host.offer("dev", &running("1.1.0", None)),
            None,
            "设备不上报 ELF 哈希时只比版本号：不比设备新就不下发"
        );
        host.reported("dev", &running("1.1.0", None));
        assert_eq!(
            db.lock().unwrap().list_firmware_deliveries().unwrap()[0].device_report_status,
            "not_reported",
            "尚未开始下载时即使版本号相同也不误记为已升级"
        );
        assert!(
            host.fetch(&"a".repeat(32), "192.168.1.50").is_none(),
            "别的地址拿不到"
        );
        assert_eq!(
            host.fetch(&"a".repeat(32), "192.168.1.9").unwrap().len(),
            2048
        );
        assert_eq!(
            db.lock().unwrap().list_firmware_deliveries().unwrap()[0].download_status,
            "downloading"
        );
        std::fs::write(dir.path().join(format!("{sha}.bin")), b"tampered").unwrap();
        assert!(
            host.fetch(&"a".repeat(32), "192.168.1.9").is_none(),
            "文件被改动就不发"
        );
        host.reported("dev", &running("1.1.0", None));
        assert_eq!(
            db.lock().unwrap().list_firmware_deliveries().unwrap()[0].device_report_status,
            "reported"
        );
        assert!(
            host.offer("dev", &running("1.0.0", None)).is_none(),
            "升级完成后不再下发"
        );
    }

    #[test]
    fn interrupted_fetch_retries_but_mid_read_revoke_or_cancel_still_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap()));
        let mut bytes_a = vec![0u8; 2048];
        bytes_a[0] = 0xE9;
        bytes_a[176..208].copy_from_slice(&[0x11; 32]);
        let mut bytes_b = vec![0u8; 2048];
        bytes_b[0] = 0xE9;
        bytes_b[176..208].copy_from_slice(&[0x22; 32]);
        let (sha_a, sha_b) = (sha256_hex(&bytes_a), sha256_hex(&bytes_b));
        let (elf_a, elf_b) = ("11".repeat(32), "22".repeat(32));
        std::fs::write(dir.path().join(format!("{sha_a}.bin")), &bytes_a).unwrap();
        std::fs::write(dir.path().join(format!("{sha_b}.bin")), &bytes_b).unwrap();

        let (tok_a, tok_b) = ("a".repeat(32), "b".repeat(32));
        {
            let guard = db.lock().unwrap();
            guard
                .voice_device_seen("dev", "client", "board", "1.0.0", "192.168.1.9", now())
                .unwrap();
            guard
                .set_voice_device_status("dev", laser_runtime::VOICE_DEVICE_ALLOWED, now())
                .unwrap();
            guard
                .add_firmware_artifact(&sha_a, "1.1.0", "", 2048, now())
                .unwrap();
            guard
                .add_firmware_artifact(&sha_b, "1.1.0", "", 2048, now())
                .unwrap();
            guard
                .create_firmware_offer("fw-a", "dev", &sha_a, "1.1.0", &tok_a, now() + 60.0, now())
                .unwrap();
        }
        let host = DesktopFirmwareHost {
            db: db.clone(),
            dir: dir.path().to_path_buf(),
        };

        // 1. 首次下载后模拟中断，重查 OTA 后可重新下载且不重复推进 revision
        assert_eq!(host.fetch(&tok_a, "192.168.1.9").unwrap().len(), 2048);
        let current = moss_voice_server::firmware::DeviceFirmware {
            version: "1.0",
            elf_sha256: None,
        };
        let offered = host
            .offer("dev", &current)
            .expect("设备重启后应继续收到同一推送");
        assert_eq!(offered.token, tok_a);
        assert!(
            host.fetch(&tok_a, "192.0.2.20").is_none(),
            "重试仍须匹配设备来源"
        );
        assert_eq!(host.fetch(&tok_a, "192.168.1.9").unwrap(), bytes_a);
        assert_eq!(
            host.db
                .lock()
                .unwrap()
                .firmware_offer_by_token(&tok_a, now())
                .unwrap()
                .unwrap()
                .revision,
            1
        );

        // 2. 新建同版本镜像 fw-b（fw-a 仍是 downloading），读盘放锁期间并发撤销设备 -> 拒发且 fw-b 维持 pending
        let tok_c = "c".repeat(32);
        db.lock()
            .unwrap()
            .create_firmware_offer(
                "fw-b",
                "dev",
                &sha_b,
                "1.1.0",
                &tok_b,
                now() + 60.0,
                now() + 1.0,
            )
            .unwrap();
        assert!(
            host.fetch_with_after_read(&tok_b, "192.168.1.9", || {
                db.lock()
                    .unwrap()
                    .set_voice_device_status(
                        "dev",
                        laser_runtime::VOICE_DEVICE_REVOKED,
                        now() + 2.0,
                    )
                    .unwrap();
            })
            .is_none(),
            "读盘期间设备被撤销时不得返回镜像"
        );
        // 恢复允许，再测读盘放锁期间 CAS 被并发取消/抢占 -> 拒发
        db.lock()
            .unwrap()
            .set_voice_device_status("dev", laser_runtime::VOICE_DEVICE_ALLOWED, now() + 3.0)
            .unwrap();
        assert!(
            host.fetch_with_after_read(&tok_b, "192.168.1.9", || {
                db.lock()
                    .unwrap()
                    .cancel_firmware_offer("fw-b", now() + 4.0)
                    .unwrap();
            })
            .is_none(),
            "读盘期间推送被取消时不得返回镜像"
        );

        // 3. 重新下发 fw-c（镜像 B，同版本 1.1.0），设备先报旧镜像 elf_a 不得错关 fw-c，报 elf_b 只关 fw-c 且不误改 fw-a
        db.lock()
            .unwrap()
            .create_firmware_offer(
                "fw-c",
                "dev",
                &sha_b,
                "1.1.0",
                &tok_c,
                now() + 60.0,
                now() + 5.0,
            )
            .unwrap();
        host.reported("dev", &running("1.1.0", Some(&elf_a)));
        let rows_mid = db.lock().unwrap().list_firmware_deliveries().unwrap();
        let fw_c_mid = rows_mid.iter().find(|r| r.delivery_id == "fw-c").unwrap();
        assert_eq!(fw_c_mid.device_report_status, "not_reported");

        assert_eq!(host.fetch(&tok_c, "192.168.1.9").unwrap().len(), 2048);
        host.reported("dev", &running("1.1.0", Some(&elf_b)));
        let rows_after = db.lock().unwrap().list_firmware_deliveries().unwrap();
        let fw_a_after = rows_after.iter().find(|r| r.delivery_id == "fw-a").unwrap();
        let fw_c_after = rows_after.iter().find(|r| r.delivery_id == "fw-c").unwrap();
        assert_eq!(fw_c_after.device_report_status, "reported");
        assert_eq!(fw_c_after.download_status, "downloaded");
        assert_eq!(fw_c_after.revision, 2);
        assert!(
            host.fetch(&tok_c, "192.168.1.9").is_none(),
            "上报完成后原下载令牌不得再下载固件"
        );
        {
            let guard = db.lock().unwrap();
            assert!(
                guard
                    .firmware_offer_by_token(&tok_c, now())
                    .unwrap()
                    .is_none(),
                "上报完成后按原令牌查不到有效推送"
            );
            assert_eq!(
                guard
                    .active_firmware_offer_row("dev", now())
                    .unwrap()
                    .map(|(r, token)| (r.delivery_id, token)),
                Some(("fw-a".to_owned(), tok_a)),
                "fw-c 已关闭，仅 fw-a 仍保持未上报有效状态"
            );
            assert!(
                !guard
                    .firmware_delivery_reported("fw-c", "dev", &sha_b, "1.1.0", now())
                    .unwrap(),
                "令牌作废且已上报的 fw-c 不得重复核销"
            );
        }
        assert_eq!(fw_a_after.device_report_status, "not_reported");
    }
}
