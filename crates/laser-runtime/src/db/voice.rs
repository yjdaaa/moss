//! 内置语音：设备登记与快照、声纹、语音记忆、对话记录、固件交付记录。

use rusqlite::{OptionalExtension, params};

use super::{
    RuntimeDb, SchemaError, VOICE_CHAT_LOGS_MAX_ROWS, VOICE_DEVICE_ALLOWED, VOICE_DEVICE_PENDING,
    VOICE_DEVICE_REVOKED, VOICE_SNAPSHOT_HELLO, VOICE_SNAPSHOT_INFO, VOICE_SNAPSHOT_LISTEN,
    VOICE_SNAPSHOT_STATUS, VoiceChatLogRecord, VoiceDeviceRecord, VoiceFirmwareDeliveryRecord,
    VoiceMemoryRecord, VoiceSpeakerRecord, VoiceSpeakerTemplate,
};

const VOICE_SNAPSHOT_MAX_BYTES: usize = 8 * 1024;

const VOICE_DEVICE_COLUMNS: &str = "device_id, client_id, status, name, model, firmware_version, last_ip, first_seen, last_seen, decided_at, info_json, info_at, hello_json, hello_at, status_json, status_at, listen_json, listen_at, status_revision";

fn voice_device_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VoiceDeviceRecord> {
    Ok(VoiceDeviceRecord {
        device_id: row.get(0)?,
        client_id: row.get(1)?,
        status: row.get(2)?,
        name: row.get(3)?,
        model: row.get(4)?,
        firmware_version: row.get(5)?,
        last_ip: row.get(6)?,
        first_seen: row.get(7)?,
        last_seen: row.get(8)?,
        decided_at: row.get(9)?,
        info_json: row.get(10)?,
        info_at: row.get(11)?,
        hello_json: row.get(12)?,
        hello_at: row.get(13)?,
        status_json: row.get(14)?,
        status_at: row.get(15)?,
        listen_json: row.get(16)?,
        listen_at: row.get(17)?,
        status_revision: row.get(18)?,
    })
}

impl RuntimeDb {
    /// NV17 durable download journal. Startup recovery never treats an
    /// interrupted transfer as safely retryable or installed.
    pub fn recover_firmware_downloads(&self, now: f64) -> Result<usize, SchemaError> {
        let changed = self.connection.execute(
            "UPDATE voice_firmware_delivery SET download_status='unknown',revision=revision+1,updated_at=?1 WHERE download_status='downloading'",
            params![now],
        )?;
        Ok(changed)
    }

    /// Private desktop diagnostic only. A finished download is never a flash claim.
    pub fn list_firmware_deliveries(
        &self,
    ) -> Result<Vec<VoiceFirmwareDeliveryRecord>, SchemaError> {
        let mut statement=self.connection.prepare("SELECT delivery_id,device_id,artifact_sha256,download_status,device_report_status,revision,updated_at,firmware_version,expires_at FROM voice_firmware_delivery ORDER BY updated_at DESC,delivery_id LIMIT 200")?;
        let rows = statement.query_map([], |r| {
            Ok(VoiceFirmwareDeliveryRecord {
                delivery_id: r.get(0)?,
                device_id: r.get(1)?,
                artifact_sha256: r.get(2)?,
                download_status: r.get(3)?,
                device_report_status: r.get(4)?,
                revision: r.get(5)?,
                updated_at: r.get(6)?,
                firmware_version: r.get(7)?,
                expires_at: r.get(8)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// NV17 fail-closed conditional transition: callers cannot use completion
    /// of a download to assert that firmware was flashed. The device report
    /// must be separately authenticated by the upper layer.
    pub fn update_firmware_download_if_revision(
        &self,
        delivery_id: &str,
        expected_revision: i64,
        next: &str,
        now: f64,
    ) -> Result<bool, SchemaError> {
        if delivery_id.is_empty() || expected_revision < 0 || !now.is_finite() {
            return Err(SchemaError::InvalidRecord("固件记录或版本无效".into()));
        }
        let predecessor = match next {
            "downloading" => "pending",
            "downloaded" | "unknown" => "downloading",
            _ => return Err(SchemaError::InvalidRecord("不允许的下载状态迁移".into())),
        };
        Ok(self.connection.execute(
            "UPDATE voice_firmware_delivery SET download_status=?1, revision=revision+1, updated_at=?2 \
             WHERE delivery_id=?3 AND revision=?4 AND download_status=?5 AND device_report_status='not_reported'",
            params![next, now, delivery_id, expected_revision, predecessor],
        )? == 1)
    }

    /// NV16 internal enrollment from three independently extracted clips.
    /// IDs come from the host, never an LLM or a device's identity claim.
    pub fn enroll_voice_speaker(
        &self,
        id: &str,
        name: &str,
        model_id: &str,
        clips: &[Vec<f32>],
        now: f64,
    ) -> Result<VoiceSpeakerRecord, SchemaError> {
        let invalid = || SchemaError::InvalidRecord("声纹登记数据无效".into());
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || name.trim() != name
            || name.is_empty()
            || name.chars().count() > 64
            || name.chars().any(char::is_control)
            || model_id != "campplus_sv_zh_cn_16k_common"
            || clips.len() != 3
            || !now.is_finite()
        {
            return Err(invalid());
        }
        let normalize = |v: &[f32]| -> Result<Vec<f32>, SchemaError> {
            if v.len() != 192 || v.iter().any(|n| !n.is_finite()) {
                return Err(invalid());
            }
            let norm = v.iter().map(|n| f64::from(*n).powi(2)).sum::<f64>().sqrt();
            if !norm.is_finite() || norm <= 1e-8 {
                return Err(invalid());
            }
            Ok(v.iter().map(|n| (f64::from(*n) / norm) as f32).collect())
        };
        let vectors = clips
            .iter()
            .map(|v| normalize(v))
            .collect::<Result<Vec<_>, _>>()?;
        for i in 0..3 {
            for j in i + 1..3 {
                if vectors[i]
                    .iter()
                    .zip(&vectors[j])
                    .map(|(a, b)| a * b)
                    .sum::<f32>()
                    < 0.70
                {
                    return Err(invalid());
                }
            }
        }
        let mean = (0..192)
            .map(|i| vectors.iter().map(|v| v[i]).sum::<f32>() / 3.)
            .collect::<Vec<_>>();
        let embedding = normalize(&mean)?
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>();
        let tx = self.connection.unchecked_transaction()?;
        let (count, duplicate): (i64, i64) = tx.query_row(
            "SELECT count(*),coalesce(sum(name=?1),0) FROM voice_speakers",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if count >= 64 || duplicate > 0 {
            return Err(invalid());
        }
        tx.execute("INSERT INTO voice_speakers(id,name,model_id,dimension,embedding,revision,created_at) VALUES(?1,?2,?3,192,?4,1,?5)",params![id,name,model_id,embedding,now])?;
        tx.execute("INSERT INTO voice_person_memory(speaker_id,summary,revision,updated_at) VALUES(?1,'',0,?2)",params![id,now])?;
        tx.commit()?;
        Ok(VoiceSpeakerRecord {
            id: id.into(),
            name: name.into(),
            model_id: model_id.into(),
            dimension: 192,
            revision: 1,
            created_at: now,
        })
    }

    pub fn list_voice_speakers(&self) -> Result<Vec<VoiceSpeakerRecord>, SchemaError> {
        let mut statement=self.connection.prepare("SELECT id,name,model_id,dimension,revision,created_at FROM voice_speakers ORDER BY created_at,id LIMIT 64")?;
        let rows = statement.query_map([], |r| {
            Ok(VoiceSpeakerRecord {
                id: r.get(0)?,
                name: r.get(1)?,
                model_id: r.get(2)?,
                dimension: r.get(3)?,
                revision: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn native_voice_speaker_templates(&self) -> Result<Vec<VoiceSpeakerTemplate>, SchemaError> {
        let mut statement=self.connection.prepare("SELECT id,revision,embedding FROM voice_speakers WHERE model_id='campplus_sv_zh_cn_16k_common' AND dimension=192 ORDER BY id LIMIT 65")?;
        let rows = statement.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (id, revision, bytes) = row?;
            if bytes.len() != 768
                || revision < 1
                || id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(SchemaError::InvalidRecord("声纹记录损坏".into()));
            }
            let embedding = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|v| f32::from_le_bytes(*v))
                .collect::<Vec<_>>();
            let norm = embedding.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
            if !norm.is_finite() || norm <= 1e-16 || embedding.iter().any(|v| !v.is_finite()) {
                return Err(SchemaError::InvalidRecord("声纹向量损坏".into()));
            }
            result.push(VoiceSpeakerTemplate {
                id,
                revision,
                embedding,
            });
        }
        if result.len() > 64 {
            return Err(SchemaError::InvalidRecord("声纹库超过限额".into()));
        }
        Ok(result)
    }

    /// NV16 internal-only person memory storage. This API accepts a server-
    /// validated speaker ID, never a model-supplied identity claim.
    pub fn voice_person_summary(
        &self,
        speaker_id: &str,
    ) -> Result<Option<(String, i64)>, SchemaError> {
        self.connection.query_row(
            "SELECT m.summary,m.revision FROM voice_person_memory m JOIN voice_speakers s ON s.id=m.speaker_id WHERE s.id=?1",
            [speaker_id], |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional().map_err(Into::into)
    }

    pub fn update_voice_person_summary_if_revision(
        &self,
        speaker_id: &str,
        expected_revision: i64,
        summary: &str,
        now: f64,
    ) -> Result<bool, SchemaError> {
        if speaker_id.is_empty()
            || expected_revision < 0
            || !now.is_finite()
            || summary.chars().count() > 1500
            || summary.chars().any(char::is_control)
        {
            return Err(SchemaError::InvalidRecord("个人摘要或版本无效".into()));
        }
        // Do not create an identity/memory row on reads or failed writes.
        Ok(self.connection.execute(
            "UPDATE voice_person_memory SET summary=?3, revision=revision+1, updated_at=?4 \
             WHERE speaker_id=?1 AND revision=?2 AND EXISTS(SELECT 1 FROM voice_speakers WHERE id=?1)",
            params![speaker_id, expected_revision, summary, now],
        )? == 1)
    }

    /// Explicit internal delete removes the entire identity and its personal
    /// summary in the same transaction, even when FK enforcement is disabled.
    pub fn delete_voice_speaker_if_revision(
        &self,
        speaker_id: &str,
        expected_revision: i64,
    ) -> Result<bool, SchemaError> {
        if speaker_id.is_empty() || expected_revision < 1 {
            return Err(SchemaError::InvalidRecord("说话人版本无效".into()));
        }
        let tx = self.connection.unchecked_transaction()?;
        let removed = tx.execute(
            "DELETE FROM voice_speakers WHERE id=?1 AND revision=?2",
            params![speaker_id, expected_revision],
        )? == 1;
        if removed {
            tx.execute(
                "DELETE FROM voice_person_memory WHERE speaker_id=?1",
                [speaker_id],
            )?;
        }
        tx.commit()?;
        Ok(removed)
    }

    /// 首次开启记忆时创建空摘要；重复调用不覆盖已有内容或 revision。
    pub fn ensure_voice_memory(
        &self,
        device_id: &str,
        now: f64,
    ) -> Result<VoiceMemoryRecord, SchemaError> {
        self.connection.execute(
            "INSERT OR IGNORE INTO voice_memory(device_id,summary,revision,updated_at) VALUES(?1,'',0,?2)",
            params![device_id, now],
        )?;
        self.voice_memory(device_id)?
            .ok_or_else(|| SchemaError::InvalidRecord("记忆行创建失败".into()))
    }

    /// 不存在时返回 None；不能因为一次后台读就替设备悄悄开启记忆。
    pub fn voice_memory(&self, device_id: &str) -> Result<Option<VoiceMemoryRecord>, SchemaError> {
        self.connection
            .query_row(
                "SELECT device_id,summary,revision,updated_at FROM voice_memory WHERE device_id=?1",
                params![device_id],
                |row| {
                    Ok(VoiceMemoryRecord {
                        device_id: row.get(0)?,
                        summary: row.get(1)?,
                        revision: row.get(2)?,
                        updated_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// 所有记忆只读列表，不含原会话。
    pub fn voice_memories(&self) -> Result<Vec<VoiceMemoryRecord>, SchemaError> {
        let mut query = self.connection.prepare("SELECT device_id,summary,revision,updated_at FROM voice_memory ORDER BY updated_at DESC")?;
        let rows = query.query_map([], |row| {
            Ok(VoiceMemoryRecord {
                device_id: row.get(0)?,
                summary: row.get(1)?,
                revision: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 用于后台总结和用户编辑/清空：只有旧 revision 一致才改变内容。
    /// 返回 None 说明用户或另一会话已编辑，调用方不能无条件重试覆盖。
    pub fn update_voice_memory_if_revision(
        &self,
        device_id: &str,
        expected_revision: i64,
        summary: &str,
        now: f64,
    ) -> Result<Option<VoiceMemoryRecord>, SchemaError> {
        if summary.chars().count() > 1500 {
            return Err(SchemaError::InvalidRecord("语音记忆最多 1500 字符".into()));
        }
        let count = self.connection.execute(
            "UPDATE voice_memory SET summary=?3,revision=revision+1,updated_at=?4 WHERE device_id=?1 AND revision=?2",
            params![device_id, expected_revision, summary, now],
        )?;
        if count == 0 {
            return Ok(None);
        }
        self.voice_memory(device_id)
    }

    /// n81 追加一条已脱敏的语音对话记录，并定额保留最近 `VOICE_CHAT_LOGS_MAX_ROWS` 条。
    pub fn append_voice_chat_log(&self, entry: &VoiceChatLogRecord) -> Result<i64, SchemaError> {
        let now = (entry.at_ms as f64) / 1000.0;
        self.connection.execute(
            "INSERT INTO voice_chat_logs(
               at_ms, device_id, session_id, turn, raw_asr, corrected_asr, reply, spoken,
               tools_json, outcome, truncated, tools_dropped, created_at, updated_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
            params![
                entry.at_ms,
                entry.device_id,
                entry.session_id,
                entry.turn,
                entry.raw_asr,
                entry.corrected_asr,
                entry.reply,
                entry.spoken,
                entry.tools_json,
                entry.outcome,
                i64::from(entry.truncated),
                entry.tools_dropped,
                now,
            ],
        )?;
        let id = self.connection.last_insert_rowid();
        let _ = self.connection.execute(
            "DELETE FROM voice_chat_logs
             WHERE id NOT IN (SELECT id FROM voice_chat_logs ORDER BY at_ms DESC, id DESC LIMIT ?1)",
            params![VOICE_CHAT_LOGS_MAX_ROWS],
        );
        Ok(id)
    }

    /// n81 按时间倒序读取最近的语音对话记录（只给受信任主窗口显示）。
    pub fn list_voice_chat_logs(
        &self,
        limit: usize,
    ) -> Result<Vec<VoiceChatLogRecord>, SchemaError> {
        let bounded = (limit.clamp(1, VOICE_CHAT_LOGS_MAX_ROWS as usize)) as i64;
        let mut stmt = self.connection.prepare(
            "SELECT id, at_ms, device_id, session_id, turn, raw_asr, corrected_asr, reply, spoken,
                    tools_json, outcome, truncated, tools_dropped
             FROM voice_chat_logs
             ORDER BY at_ms DESC, id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![bounded], |row| {
            Ok(VoiceChatLogRecord {
                id: row.get(0)?,
                at_ms: row.get(1)?,
                device_id: row.get(2)?,
                session_id: row.get(3)?,
                turn: row.get(4)?,
                raw_asr: row.get(5)?,
                corrected_asr: row.get(6)?,
                reply: row.get(7)?,
                spoken: row.get(8)?,
                tools_json: row.get(9)?,
                outcome: row.get(10)?,
                truncated: row.get::<_, i64>(11)? != 0,
                tools_dropped: row.get(12)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// n81 清空全部语音对话记录。
    pub fn clear_voice_chat_logs(&self) -> Result<usize, SchemaError> {
        self.connection
            .execute("DELETE FROM voice_chat_logs", [])
            .map_err(Into::into)
    }

    /// 记录一次设备来访（OTA 或 WebSocket）。新设备登记为待允许；
    /// **`client_id` 与已登记的不同**时，已允许的设备退回待允许（防止他人冒用 MAC 取得 token），已撤销的保持撤销。
    pub fn voice_device_seen(
        &self,
        device_id: &str,
        client_id: &str,
        model: &str,
        firmware_version: &str,
        last_ip: &str,
        now: f64,
    ) -> Result<VoiceDeviceRecord, SchemaError> {
        let tx = self.connection.unchecked_transaction()?;
        let existing = tx
            .query_row(
                &format!("SELECT {VOICE_DEVICE_COLUMNS} FROM voice_devices WHERE device_id=?1"),
                [device_id],
                voice_device_from_row,
            )
            .optional()?;
        match existing {
            None => {
                // 未认证局域网设备最多保留 64 条 pending 记录，超限淘汰最旧 pending，防止无限膨胀拖慢主库锁。
                const MAX_PENDING_VOICE_DEVICES: i64 = 64;
                tx.execute(
                    "DELETE FROM voice_devices
                     WHERE status = ?1
                       AND device_id NOT IN (
                           SELECT device_id FROM voice_devices
                           WHERE status = ?1
                           ORDER BY last_seen DESC, rowid DESC
                           LIMIT ?2
                       )",
                    params![VOICE_DEVICE_PENDING, MAX_PENDING_VOICE_DEVICES - 1],
                )?;
                tx.execute(
                    "INSERT INTO voice_devices(device_id, client_id, status, model, firmware_version, last_ip, first_seen, last_seen, status_revision)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 1)",
                    params![device_id, client_id, VOICE_DEVICE_PENDING, model, firmware_version, last_ip, now],
                )?;
            }
            // 同名 Device-Id 的未认证来访不能替换已绑定的 Client-Id，
            // 更不能把已允许设备降级并撤销其正在执行的合法会话。
            Some(record) if record.client_id != client_id => {
                tx.commit()?;
                return Ok(record);
            }
            Some(record) => {
                let keep =
                    |new: &str, old: String| if new.is_empty() { old } else { new.to_owned() };
                tx.execute(
                    "UPDATE voice_devices SET client_id=?2, status=?3, model=?4, firmware_version=?5, last_ip=?6, last_seen=?7
                     WHERE device_id=?1",
                    params![
                        device_id,
                        client_id,
                        record.status,
                        keep(model, record.model),
                        keep(firmware_version, record.firmware_version),
                        keep(last_ip, record.last_ip),
                        now
                    ],
                )?;
            }
        }
        tx.execute("UPDATE approvals SET status='expired', updated_at=?2 WHERE source='voice' AND session_id=?1 AND status IN ('pending','approved') AND EXISTS (SELECT 1 FROM voice_devices WHERE device_id=?1 AND status<>'allowed')", params![device_id, now])?;
        let record = tx.query_row(
            &format!("SELECT {VOICE_DEVICE_COLUMNS} FROM voice_devices WHERE device_id=?1"),
            [device_id],
            voice_device_from_row,
        )?;
        tx.commit()?;
        Ok(record)
    }

    /// 物理 USB 配网后按芯片 MAC 重绑 `client_id` 并设为已允许，同时保留原本已知的板型（仅在空 `model` 时填芯片名）。
    /// 若设备在库中已为 `revoked`，且未勾 `unrevoke` 或库中 `status_revision` 与起始绑定修订 `expected_revision` 不一致，
    /// 事务内拒绝覆盖最新撤销状态，作废残留审批并返回 `Ok(None)`。
    pub fn bind_usb_voice_device(
        &self,
        device_id: &str,
        client_id: &str,
        fallback_model: &str,
        expected_revision: Option<i64>,
        unrevoke: bool,
        now: f64,
    ) -> Result<Option<VoiceDeviceRecord>, SchemaError> {
        if device_id.trim().is_empty() || client_id.trim().is_empty() {
            return Err(SchemaError::InvalidRecord(
                "设备 MAC 与 Client-Id 不能为空".into(),
            ));
        }
        let tx = self.connection.unchecked_transaction()?;
        let existing = tx
            .query_row(
                &format!("SELECT {VOICE_DEVICE_COLUMNS} FROM voice_devices WHERE device_id=?1"),
                [device_id],
                voice_device_from_row,
            )
            .optional()?;
        match existing {
            None => {
                if expected_revision.is_some() {
                    tx.commit()?;
                    return Ok(None);
                }
                tx.execute(
                    "INSERT INTO voice_devices(device_id, client_id, status, model, firmware_version, last_ip, first_seen, last_seen, decided_at, status_revision)
                     VALUES(?1, ?2, ?3, ?4, '', '', ?5, ?5, ?5, 1)",
                    params![
                        device_id,
                        client_id,
                        VOICE_DEVICE_ALLOWED,
                        fallback_model,
                        now
                    ],
                )?;
            }
            Some(record) => {
                if record.status == VOICE_DEVICE_REVOKED
                    && (!unrevoke || expected_revision != Some(record.status_revision))
                {
                    tx.execute(
                        "UPDATE approvals SET status='expired', updated_at=?2
                         WHERE source='voice' AND session_id=?1 AND status IN ('pending','approved')",
                        params![device_id, now],
                    )?;
                    tx.commit()?;
                    return Ok(None);
                }
                let rebinding = record.client_id != client_id;
                let next_model = if record.model.trim().is_empty() {
                    fallback_model.to_owned()
                } else {
                    record.model
                };
                let decided_at = if record.status == VOICE_DEVICE_ALLOWED && !rebinding {
                    record.decided_at.unwrap_or(now)
                } else {
                    now
                };
                let next_revision = if record.status != VOICE_DEVICE_ALLOWED || rebinding {
                    record.status_revision + 1
                } else {
                    record.status_revision
                };
                tx.execute(
                    "UPDATE voice_devices SET client_id=?2, status=?3, model=?4, last_seen=?5, decided_at=?6, status_revision=?7
                     WHERE device_id=?1",
                    params![
                        device_id,
                        client_id,
                        VOICE_DEVICE_ALLOWED,
                        next_model,
                        now,
                        decided_at,
                        next_revision
                    ],
                )?;
                if rebinding {
                    tx.execute(
                        "UPDATE approvals SET status='expired', updated_at=?2
                         WHERE source='voice' AND session_id=?1 AND status IN ('pending','approved')",
                        params![device_id, now],
                    )?;
                }
            }
        }
        let record = tx.query_row(
            &format!("SELECT {VOICE_DEVICE_COLUMNS} FROM voice_devices WHERE device_id=?1"),
            [device_id],
            voice_device_from_row,
        )?;
        tx.commit()?;
        Ok(Some(record))
    }

    /// 记录设备快照（schema 14）。未知种类或超长 JSON 返回 `false`，不写库也不报错。
    pub fn voice_device_snapshot(
        &self,
        device_id: &str,
        kind: &str,
        json: &str,
        now: f64,
    ) -> Result<bool, SchemaError> {
        let (column, at) = match kind {
            VOICE_SNAPSHOT_INFO => ("info_json", "info_at"),
            VOICE_SNAPSHOT_HELLO => ("hello_json", "hello_at"),
            VOICE_SNAPSHOT_STATUS => ("status_json", "status_at"),
            VOICE_SNAPSHOT_LISTEN => ("listen_json", "listen_at"),
            _ => return Ok(false),
        };
        if json.is_empty() || json.len() > VOICE_SNAPSHOT_MAX_BYTES {
            return Ok(false);
        }
        let changed = self.connection.execute(
            &format!("UPDATE voice_devices SET {column}=?2, {at}=?3 WHERE device_id=?1"),
            params![device_id, json, now],
        )?;
        Ok(changed > 0)
    }

    pub fn voice_device(&self, device_id: &str) -> Result<Option<VoiceDeviceRecord>, SchemaError> {
        Ok(self
            .connection
            .query_row(
                &format!("SELECT {VOICE_DEVICE_COLUMNS} FROM voice_devices WHERE device_id=?1"),
                [device_id],
                voice_device_from_row,
            )
            .optional()?)
    }

    /// 全部登记设备，最近来访的在前。
    pub fn voice_devices(&self) -> Result<Vec<VoiceDeviceRecord>, SchemaError> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {VOICE_DEVICE_COLUMNS} FROM voice_devices ORDER BY last_seen DESC"
        ))?;
        let rows = statement.query_map([], voice_device_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 桌面端允许或撤销设备；返回是否找到该设备。
    pub fn set_voice_device_status(
        &self,
        device_id: &str,
        status: &str,
        now: f64,
    ) -> Result<bool, SchemaError> {
        if ![
            VOICE_DEVICE_PENDING,
            VOICE_DEVICE_ALLOWED,
            VOICE_DEVICE_REVOKED,
        ]
        .contains(&status)
        {
            return Err(SchemaError::InvalidRecord(format!(
                "未知的设备状态: {status}"
            )));
        }
        let tx = self.connection.unchecked_transaction()?;
        let found = tx.execute(
            "UPDATE voice_devices SET status=?2, decided_at=?3, status_revision=status_revision+1 WHERE device_id=?1",
            params![device_id, status, now],
        )? > 0;
        if found && status != VOICE_DEVICE_ALLOWED {
            tx.execute("UPDATE approvals SET status='expired', updated_at=?2 WHERE source='voice' AND session_id=?1 AND status IN ('pending','approved')", params![device_id, now])?;
        }
        tx.commit()?;
        Ok(found)
    }

    /// 修改设备显示名（最长 32 字）。
    pub fn rename_voice_device(&self, device_id: &str, name: &str) -> Result<bool, SchemaError> {
        let name: String = name.trim().chars().take(32).collect();
        Ok(self.connection.execute(
            "UPDATE voice_devices SET name=?2 WHERE device_id=?1",
            params![device_id, name],
        )? > 0)
    }
}
