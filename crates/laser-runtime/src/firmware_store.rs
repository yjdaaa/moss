//! NV17 固件 OTA 的持久化（schema 13，2026-10-15）：导入的固件清单与逐台推送记录。
//! 推送只对一台设备、到期失效；“已升级”只由设备之后上报的版本得出，下载本身不算刷写成功。
use crate::db::{RuntimeDb, SchemaError};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FirmwareArtifactRow {
    pub sha256: String,
    pub version: String,
    pub project: String,
    pub size: i64,
    pub created_at: f64,
}

/// 按下载令牌查到的推送。
#[derive(Debug, Clone, PartialEq)]
pub struct FirmwareOfferRow {
    pub delivery_id: String,
    pub device_id: String,
    pub artifact_sha256: String,
    pub firmware_version: String,
    pub download_status: String,
    pub revision: i64,
}

fn invalid(message: &str) -> SchemaError {
    SchemaError::InvalidRecord(message.into())
}
fn sha_ok(sha: &str) -> bool {
    sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}
/// 推送仍有效：未上报新版本、未到期、未取消（取消即清空令牌）。
const ACTIVE: &str = "device_report_status='not_reported' AND download_status IN ('pending','downloading','unknown') AND token IS NOT NULL AND expires_at > ?";
/// 下载令牌仅在待下载或下载中有效，允许同一设备在中断后重试。
const RETRYABLE_DOWNLOAD: &str = "device_report_status='not_reported' AND download_status IN ('pending','downloading') AND token IS NOT NULL AND expires_at > ?";

impl RuntimeDb {
    /// 登记导入的固件（同一 SHA-256 重复导入不变）。
    pub fn add_firmware_artifact(
        &self,
        sha256: &str,
        version: &str,
        project: &str,
        size: i64,
        now: f64,
    ) -> Result<(), SchemaError> {
        if !sha_ok(sha256)
            || version.is_empty()
            || version.len() > 32
            || project.len() > 32
            || size <= 0
            || !now.is_finite()
        {
            return Err(invalid("固件信息无效"));
        }
        self.connection.execute(
            "INSERT OR IGNORE INTO voice_firmware_artifacts(sha256,version,project,size,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![sha256.to_ascii_lowercase(), version, project, size, now],
        )?;
        Ok(())
    }

    pub fn list_firmware_artifacts(&self) -> Result<Vec<FirmwareArtifactRow>, SchemaError> {
        let mut statement = self.connection.prepare(
            "SELECT sha256,version,project,size,created_at FROM voice_firmware_artifacts ORDER BY created_at DESC LIMIT 64",
        )?;
        let rows = statement.query_map([], |r| {
            Ok(FirmwareArtifactRow {
                sha256: r.get(0)?,
                version: r.get(1)?,
                project: r.get(2)?,
                size: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 删除固件；还有有效推送在用时拒绝（返回 false）。
    pub fn delete_firmware_artifact(&self, sha256: &str, now: f64) -> Result<bool, SchemaError> {
        let in_use: i64 = self.connection.query_row(
            &format!(
                "SELECT COUNT(*) FROM voice_firmware_delivery WHERE artifact_sha256=?1 AND {}",
                ACTIVE.replace('?', "?2")
            ),
            params![sha256, now],
            |r| r.get(0),
        )?;
        if in_use > 0 {
            return Ok(false);
        }
        Ok(self.connection.execute(
            "DELETE FROM voice_firmware_artifacts WHERE sha256=?1",
            params![sha256],
        )? == 1)
    }

    /// 新建一台设备的推送；同一设备还没开始下载的旧推送被替换。
    pub fn create_firmware_offer(
        &self,
        delivery_id: &str,
        device_id: &str,
        sha256: &str,
        version: &str,
        token: &str,
        expires_at: f64,
        now: f64,
    ) -> Result<(), SchemaError> {
        if delivery_id.is_empty()
            || device_id.is_empty()
            || !sha_ok(sha256)
            || token.len() != 32
            || !expires_at.is_finite()
            || expires_at <= now
        {
            return Err(invalid("固件推送参数无效"));
        }
        let tx = self.connection.unchecked_transaction()?;
        let artifact: Option<String> = tx
            .query_row(
                "SELECT version FROM voice_firmware_artifacts WHERE sha256=?1",
                params![sha256],
                |r| r.get(0),
            )
            .optional()?;
        if artifact.as_deref() != Some(version) {
            return Err(invalid("找不到这个固件，或版本不符"));
        }
        tx.execute(
            "DELETE FROM voice_firmware_delivery WHERE device_id=?1 AND download_status='pending' AND device_report_status='not_reported'",
            params![device_id],
        )?;
        tx.execute(
            "INSERT INTO voice_firmware_delivery(delivery_id,device_id,artifact_sha256,download_status,device_report_status,revision,updated_at,firmware_version,token,expires_at)
             VALUES(?1,?2,?3,'pending','not_reported',0,?4,?5,?6,?7)",
            params![delivery_id, device_id, sha256, now, version, token, expires_at],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 设备请求 OTA 时：这台设备是否有有效推送，返回 (版本, 令牌)。
    pub fn active_firmware_offer(
        &self,
        device_id: &str,
        now: f64,
    ) -> Result<Option<(String, String)>, SchemaError> {
        Ok(self.connection.query_row(
            &format!("SELECT firmware_version,token FROM voice_firmware_delivery WHERE device_id=?1 AND {} ORDER BY updated_at DESC, rowid DESC LIMIT 1", ACTIVE.replace('?', "?2")),
            params![device_id, now],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?)
    }

    /// 设备请求 OTA 或上报版本时：取该设备当前最新有效推送的完整行与下载令牌。
    pub fn active_firmware_offer_row(
        &self,
        device_id: &str,
        now: f64,
    ) -> Result<Option<(FirmwareOfferRow, String)>, SchemaError> {
        Ok(self.connection.query_row(
            &format!("SELECT delivery_id,device_id,artifact_sha256,firmware_version,download_status,revision,token FROM voice_firmware_delivery WHERE device_id=?1 AND {} ORDER BY updated_at DESC, rowid DESC LIMIT 1", ACTIVE.replace('?', "?2")),
            params![device_id, now],
            |r| Ok((FirmwareOfferRow { delivery_id: r.get(0)?, device_id: r.get(1)?, artifact_sha256: r.get(2)?, firmware_version: r.get(3)?, download_status: r.get(4)?, revision: r.get(5)? }, r.get(6)?)),
        ).optional()?)
    }

    /// 按设备绑定的下载令牌查询可首次下载或重试的有效推送。
    pub fn firmware_offer_by_token(
        &self,
        token: &str,
        now: f64,
    ) -> Result<Option<FirmwareOfferRow>, SchemaError> {
        if token.len() != 32 {
            return Ok(None);
        }
        Ok(self.connection.query_row(
            &format!("SELECT delivery_id,device_id,artifact_sha256,firmware_version,download_status,revision FROM voice_firmware_delivery WHERE token=?1 AND {}", RETRYABLE_DOWNLOAD.replace('?', "?2")),
            params![token, now],
            |r| Ok(FirmwareOfferRow { delivery_id: r.get(0)?, device_id: r.get(1)?, artifact_sha256: r.get(2)?, firmware_version: r.get(3)?, download_status: r.get(4)?, revision: r.get(5)? }),
        ).optional()?)
    }

    /// 绑定推送单号与镜像 SHA-256 核销设备上报的新版本：只更新匹配的这一笔推送，不误伤同版本不同镜像的其他推送。
    pub fn firmware_delivery_reported(
        &self,
        delivery_id: &str,
        device_id: &str,
        artifact_sha256: &str,
        version: &str,
        now: f64,
    ) -> Result<bool, SchemaError> {
        if delivery_id.is_empty()
            || device_id.is_empty()
            || !sha_ok(artifact_sha256)
            || version.is_empty()
            || !now.is_finite()
        {
            return Err(invalid("固件上报参数无效"));
        }
        Ok(self.connection.execute(
            "UPDATE voice_firmware_delivery SET device_report_status='reported',
               download_status=CASE WHEN download_status IN ('downloading','unknown') THEN 'downloaded' ELSE download_status END,
               token=NULL, revision=revision+1, updated_at=?5
             WHERE delivery_id=?1 AND device_id=?2 AND artifact_sha256=?3 AND firmware_version=?4
               AND device_report_status='not_reported' AND token IS NOT NULL",
            params![delivery_id, device_id, artifact_sha256, version, now],
        )? == 1)
    }

    /// 设备上报的版本等于有效推送的版本：记为已上报（已升级），令牌作废（最多更新最近一条）。
    pub fn firmware_device_reported(
        &self,
        device_id: &str,
        version: &str,
        now: f64,
    ) -> Result<bool, SchemaError> {
        Ok(self.connection.execute(
            "UPDATE voice_firmware_delivery SET device_report_status='reported',
               download_status=CASE WHEN download_status IN ('downloading','unknown') THEN 'downloaded' ELSE download_status END,
               token=NULL, revision=revision+1, updated_at=?3
             WHERE delivery_id = (
               SELECT delivery_id FROM voice_firmware_delivery
               WHERE device_id=?1 AND firmware_version=?2 AND device_report_status='not_reported' AND token IS NOT NULL
               ORDER BY updated_at DESC, rowid DESC LIMIT 1
             )",
            params![device_id, version, now],
        )? > 0)
    }

    /// 取消推送：还没开始下载的直接删除；已开始下载的作废令牌（设备可能正在刷写，记录保留）。
    pub fn cancel_firmware_offer(&self, delivery_id: &str, now: f64) -> Result<bool, SchemaError> {
        let deleted = self.connection.execute(
            "DELETE FROM voice_firmware_delivery WHERE delivery_id=?1 AND download_status='pending' AND device_report_status='not_reported'",
            params![delivery_id],
        )?;
        if deleted == 1 {
            return Ok(true);
        }
        Ok(self.connection.execute(
            "UPDATE voice_firmware_delivery SET token=NULL, expires_at=?2, revision=revision+1, updated_at=?2
             WHERE delivery_id=?1 AND device_report_status='not_reported' AND token IS NOT NULL",
            params![delivery_id, now],
        )? == 1)
    }

    /// 检查是否存在尚未撤销且未过期的激活态 `downloading` 固件下载任务，避免已取消或过期记录长期误锁 USB 配网。
    pub fn has_active_firmware_download(&self, now: f64) -> Result<bool, SchemaError> {
        let active: i64 = self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM voice_firmware_delivery
                WHERE device_report_status='not_reported'
                  AND download_status='downloading'
                  AND token IS NOT NULL
                  AND expires_at > ?1
            )",
            params![now],
            |row| row.get(0),
        )?;
        Ok(active != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_lifecycle_is_per_device_expires_and_completes_only_on_report() {
        let db = RuntimeDb::open_memory().unwrap();
        let sha = "a".repeat(64);
        let token = "b".repeat(32);
        db.add_firmware_artifact(&sha, "1.9.0", "xiaozhi", 2048, 1.0)
            .unwrap();
        assert!(
            db.create_firmware_offer("d1", "dev", &sha, "1.8.0", &token, 100.0, 1.0)
                .is_err(),
            "版本必须与固件一致"
        );
        db.create_firmware_offer("d1", "dev", &sha, "1.9.0", &token, 100.0, 1.0)
            .unwrap();
        assert_eq!(
            db.active_firmware_offer("dev", 2.0).unwrap(),
            Some(("1.9.0".into(), token.clone()))
        );
        assert_eq!(
            db.active_firmware_offer("other", 2.0).unwrap(),
            None,
            "只对这台设备"
        );
        assert_eq!(
            db.active_firmware_offer("dev", 100.0).unwrap(),
            None,
            "到期失效"
        );
        assert!(
            !db.delete_firmware_artifact(&sha, 2.0).unwrap(),
            "有效推送在用时不能删固件"
        );

        let offer = db.firmware_offer_by_token(&token, 2.0).unwrap().unwrap();
        assert_eq!(
            (offer.device_id.as_str(), offer.download_status.as_str()),
            ("dev", "pending")
        );
        assert!(
            db.update_firmware_download_if_revision("d1", offer.revision, "downloading", 3.0)
                .unwrap()
        );
        assert!(
            !db.firmware_device_reported("dev", "1.8.0", 4.0).unwrap(),
            "旧版本上报不算升级"
        );
        assert!(db.firmware_device_reported("dev", "1.9.0", 5.0).unwrap());
        assert_eq!(
            db.firmware_offer_by_token(&token, 6.0).unwrap(),
            None,
            "升级后令牌作废"
        );
        let record = &db.list_firmware_deliveries().unwrap()[0];
        assert_eq!(
            (
                record.download_status.as_str(),
                record.device_report_status.as_str(),
                record.firmware_version.as_str()
            ),
            ("downloaded", "reported", "1.9.0")
        );
        assert!(db.delete_firmware_artifact(&sha, 7.0).unwrap());
    }

    #[test]
    fn cancel_deletes_pending_and_invalidates_started_download() {
        let db = RuntimeDb::open_memory().unwrap();
        let sha = "c".repeat(64);
        db.add_firmware_artifact(&sha, "2.0", "", 2048, 1.0)
            .unwrap();
        db.create_firmware_offer("p", "dev", &sha, "2.0", &"1".repeat(32), 100.0, 1.0)
            .unwrap();
        // 同一设备新推送替换还没开始下载的旧推送
        db.create_firmware_offer("q", "dev", &sha, "2.0", &"2".repeat(32), 100.0, 2.0)
            .unwrap();
        assert_eq!(db.list_firmware_deliveries().unwrap().len(), 1);
        assert!(db.cancel_firmware_offer("q", 3.0).unwrap());
        assert!(db.list_firmware_deliveries().unwrap().is_empty());

        db.create_firmware_offer("r", "dev", &sha, "2.0", &"3".repeat(32), 100.0, 4.0)
            .unwrap();
        assert!(
            db.update_firmware_download_if_revision("r", 0, "downloading", 5.0)
                .unwrap()
        );
        assert!(db.has_active_firmware_download(5.0).unwrap());
        assert!(db.cancel_firmware_offer("r", 6.0).unwrap());
        assert!(!db.has_active_firmware_download(7.0).unwrap());
        assert_eq!(db.active_firmware_offer("dev", 7.0).unwrap(), None);
        assert_eq!(
            db.list_firmware_deliveries().unwrap()[0].download_status,
            "downloading",
            "已开始下载的记录保留"
        );
    }

    #[test]
    fn has_active_firmware_download_ignores_pending_cancelled_and_expired_rows() {
        let db = RuntimeDb::open_memory().unwrap();
        assert!(!db.has_active_firmware_download(10.0).unwrap());

        let sha = "d".repeat(64);
        db.add_firmware_artifact(&sha, "2.1", "", 2048, 1.0)
            .unwrap();
        db.create_firmware_offer("pend", "dev1", &sha, "2.1", &"a".repeat(32), 100.0, 1.0)
            .unwrap();
        assert!(
            !db.has_active_firmware_download(10.0).unwrap(),
            "尚未开始下载的 pending 任务不应阻断 USB 配网"
        );

        assert!(
            db.update_firmware_download_if_revision("pend", 0, "downloading", 5.0)
                .unwrap()
        );
        assert!(db.has_active_firmware_download(99.0).unwrap());
        assert!(
            !db.has_active_firmware_download(100.0).unwrap(),
            "到达过期时间时不再视为活跃下载"
        );
        assert!(db.cancel_firmware_offer("pend", 12.0).unwrap());
        assert!(
            !db.has_active_firmware_download(12.0).unwrap(),
            "撤销 token=NULL 后即便 download_status 仍为 downloading 也不得阻塞"
        );
    }

    #[test]
    fn interrupted_download_can_retry_and_report_binds_delivery_and_sha256() {
        let db = RuntimeDb::open_memory().unwrap();
        let sha_a = "a".repeat(64);
        let sha_b = "b".repeat(64);
        let tok_a = "1".repeat(32);
        let tok_b = "2".repeat(32);
        db.add_firmware_artifact(&sha_a, "2.1.0", "", 2048, 1.0)
            .unwrap();
        db.add_firmware_artifact(&sha_b, "2.1.0", "", 2048, 1.0)
            .unwrap();

        db.create_firmware_offer("del-a", "dev1", &sha_a, "2.1.0", &tok_a, 100.0, 10.0)
            .unwrap();
        assert_eq!(
            db.firmware_offer_by_token(&tok_a, 11.0)
                .unwrap()
                .map(|r| r.delivery_id),
            Some("del-a".to_owned())
        );
        // 中断后同一设备可再次取得同一推送，修订保持不变
        assert!(
            db.update_firmware_download_if_revision("del-a", 0, "downloading", 12.0)
                .unwrap()
        );
        let retry = db.firmware_offer_by_token(&tok_a, 12.0).unwrap().unwrap();
        assert_eq!(retry.download_status, "downloading");
        assert_eq!(retry.revision, 1);
        assert_eq!(retry.artifact_sha256, sha_a);

        // 再创建同版本不同镜像的 pending 推送（del-a 因已是 downloading 仍留在表里）
        db.create_firmware_offer("del-b", "dev1", &sha_b, "2.1.0", &tok_b, 100.0, 20.0)
            .unwrap();
        assert_eq!(
            db.active_firmware_offer_row("dev1", 21.0)
                .unwrap()
                .map(|(r, _)| r.delivery_id),
            Some("del-b".to_owned())
        );

        // 绑定错配 sha256 或错配 delivery_id 时不误更新任何记录
        assert!(
            !db.firmware_delivery_reported("del-b", "dev1", &sha_a, "2.1.0", 22.0)
                .unwrap()
        );
        assert!(
            !db.firmware_delivery_reported("del-a", "dev1", &sha_b, "2.1.0", 22.0)
                .unwrap()
        );

        // 只上报 del-b / sha_b：只关闭 del-b，同版本的 del-a 仍保持未上报
        assert!(
            db.firmware_delivery_reported("del-b", "dev1", &sha_b, "2.1.0", 23.0)
                .unwrap()
        );
        let rows = db.list_firmware_deliveries().unwrap();
        let del_a = rows.iter().find(|r| r.delivery_id == "del-a").unwrap();
        let del_b = rows.iter().find(|r| r.delivery_id == "del-b").unwrap();
        assert_eq!(del_b.device_report_status, "reported");
        assert_eq!(del_b.revision, 1);
        assert!(
            db.firmware_offer_by_token(&tok_b, 23.0).unwrap().is_none(),
            "上报后 del-b 原下载令牌不可再拉取推送"
        );
        assert_eq!(
            db.active_firmware_offer_row("dev1", 23.0)
                .unwrap()
                .map(|(r, token)| (r.delivery_id, token)),
            Some(("del-a".to_owned(), tok_a)),
            "del-b 关闭后不再作为活跃推送，只剩 del-a 保持原令牌有效"
        );
        assert!(
            !db.firmware_delivery_reported("del-b", "dev1", &sha_b, "2.1.0", 24.0)
                .unwrap(),
            "已上报并清空令牌的 del-b 不可重复核销"
        );
        assert_eq!(del_a.device_report_status, "not_reported");
        assert_eq!(del_a.download_status, "downloading");
        assert!(
            db.delete_firmware_artifact(&sha_b, 24.0).unwrap(),
            "del-b 关闭后对应镜像不再被活跃推送占用"
        );
        assert!(
            !db.delete_firmware_artifact(&sha_a, 24.0).unwrap(),
            "仍处于未上报 downloading 的 del-a 继续锁定镜像"
        );
    }
}
