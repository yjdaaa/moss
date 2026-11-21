//! 本机密钥表 `app_secrets` 与 Home Assistant 令牌。

use rusqlite::{OptionalExtension, params};

use super::{RuntimeDb, SchemaError};

impl RuntimeDb {
    /// NV15 desktop-owned HA token store, separate from voice JSON settings.
    /// The caller never receives the bearer token, only its presence and
    /// monotonic revision. Token changes atomically invalidate HA approvals.
    /// Read ONLY the optimistic revision for confirmed safe-template recovery.
    /// Unlike home_token_status, this does not decrypt an old DPAPI blob,
    /// so a foreign Windows-user copy can be safely revoked and replaced.
    /// Never use this as proof a token is configured or valid.
    pub fn home_token_recovery_revision(&self) -> Result<u64, SchemaError> {
        self.get_secret("voice.homeassistant.token_revision")?
            .map(|raw| {
                raw.parse::<u64>()
                    .ok()
                    .filter(|v| *v <= 9_007_199_254_740_991)
                    .ok_or_else(|| SchemaError::InvalidRecord("HA 令牌版本损坏".into()))
            })
            .transpose()
            .map(|revision| revision.unwrap_or(0))
    }

    pub fn home_token_status(&self) -> Result<(bool, u64), SchemaError> {
        let revision = self.home_token_recovery_revision()?;
        let configured = match self.get_secret("voice.homeassistant.token")? {
            None => false,
            Some(value) if crate::home_secret::is_sealed(&value) => {
                // A malformed envelope fails closed, not a configured usable token.
                crate::home_secret::open(&value)
                    .map_err(|_| SchemaError::InvalidRecord("HA 令牌加密数据无效".into()))?;
                true
            }
            Some(_) => {
                return Err(SchemaError::InvalidRecord(
                    "检测到旧版明文 HA 凭据，需安全迁移".into(),
                ));
            }
        };
        Ok((configured, revision))
    }

    pub fn update_home_token_if_revision(
        &self,
        expected_revision: u64,
        replacement: Option<&str>,
        now: f64,
    ) -> Result<Option<u64>, SchemaError> {
        if expected_revision > 9_007_199_254_740_990 || !now.is_finite() || now < 0.0 {
            return Err(SchemaError::InvalidRecord("HA 令牌版本或时间无效".into()));
        }
        if let Some(token) = replacement
            && (token.len() < 20
                || token.len() > 8192
                || token
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b.is_ascii_control()))
        {
            return Err(SchemaError::InvalidRecord("HA 访问令牌格式无效".into()));
        }
        let sealed = replacement
            .map(|token| {
                crate::home_secret::seal(token).map_err(|_| {
                    SchemaError::InvalidRecord("无法使用系统安全存储保护 HA 令牌".into())
                })
            })
            .transpose()?;
        // BEGIN IMMEDIATE acquires the SQLite write reservation *before* the
        // revision read. With multiple connections, the loser waits then sees
        // the incremented revision (conflict) instead of SQLITE_BUSY_SNAPSHOT.
        let tx = rusqlite::Transaction::new_unchecked(
            &self.connection,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT value FROM app_secrets WHERE name='voice.homeassistant.token_revision'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let current = raw
            .map(|raw| {
                raw.parse::<u64>()
                    .ok()
                    .filter(|v| *v <= 9_007_199_254_740_991)
                    .ok_or_else(|| SchemaError::InvalidRecord("HA 令牌版本损坏".into()))
            })
            .transpose()?
            .unwrap_or(0);
        if current != expected_revision {
            return Ok(None);
        }
        let next = current
            .checked_add(1)
            .filter(|v| *v <= 9_007_199_254_740_991)
            .ok_or_else(|| SchemaError::InvalidRecord("HA 令牌版本上限".into()))?;
        match replacement {
            Some(_) => {
                let sealed = sealed
                    .as_ref()
                    .ok_or_else(|| SchemaError::InvalidRecord("HA 令牌密封失败".into()))?;
                tx.execute("INSERT INTO app_secrets(name,value,updated_at) VALUES('voice.homeassistant.token',?1,?2) ON CONFLICT(name) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",params![sealed,now])?;
            }
            None => {
                tx.execute(
                    "DELETE FROM app_secrets WHERE name='voice.homeassistant.token'",
                    [],
                )?;
            }
        }
        tx.execute("INSERT INTO app_secrets(name,value,updated_at) VALUES('voice.homeassistant.token_revision',?1,?2) ON CONFLICT(name) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",params![next.to_string(),now])?;
        // Both voice-read and HomeControl grants were bound to the old
        // credential revision. Delete them in the same transaction as the
        // token rotation; re-entering the old value never restores authority.
        tx.execute("DELETE FROM app_secrets WHERE name IN ('voice.homeassistant.voice_read_consent','voice.homeassistant.voice_control_consent')", [])?;
        // A token can be replaced without changing the entity policy, but all
        // outstanding HomeControl approvals must be made stale immediately.
        tx.execute("UPDATE approvals SET status='expired',updated_at=?1 WHERE kind='home_control' AND status IN ('pending','approved')",params![now])?;
        tx.commit()?;
        Ok(Some(next))
    }

    /// Only the trusted internal HA connector may request decrypted credentials.
    /// Do not expose this through the Tauri command or general settings APIs.
    pub fn home_token_for_connection(&self) -> Result<Option<String>, SchemaError> {
        let Some(sealed) = self.get_secret("voice.homeassistant.token")? else {
            return Ok(None);
        };
        crate::home_secret::open(&sealed)
            .map(Some)
            .map_err(|_| SchemaError::InvalidRecord("Home Assistant 令牌无法解密或已损坏".into()))
    }

    /// One-time upgrade for an existing plaintext HA token saved by the older
    /// prototype. Runs in one SQLite transaction and bumps the credential
    /// revision, expiring pending/approved HomeControl approvals. Existing
    /// external/old backups must be protected or deleted separately by owner.
    pub fn migrate_legacy_home_token(&self, now: f64) -> Result<bool, SchemaError> {
        if !now.is_finite() || now < 0.0 {
            return Err(SchemaError::InvalidRecord("令牌迁移时间无效".into()));
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.connection,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let original: Option<String> = tx
            .query_row(
                "SELECT value FROM app_secrets WHERE name='voice.homeassistant.token'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(original) = original else {
            return Ok(false);
        };
        if crate::home_secret::is_sealed(&original) {
            // A sealed blob copied from another Windows principal may be
            // impossible to decrypt. Never fail the *entire database startup*:
            // home_token_status/for_connection must still reject the blob,
            // and ONLY explicitly confirmed credential-revoking migration may
            // clear it with an expected revision under BEGIN IMMEDIATE.
            // Do not unseal, reencrypt or grant any HomeControl permission.
            return Ok(false);
        }
        if original.len() < 20
            || original.len() > 8192
            || original
                .bytes()
                .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err(SchemaError::InvalidRecord(
                "旧版明文令牌格式不合法，拒绝迁移".into(),
            ));
        }
        let cipher = crate::home_secret::seal(&original)
            .map_err(|_| SchemaError::InvalidRecord("DPAPI 旧凭据迁移失败".into()))?;
        let old: Option<String> = tx
            .query_row(
                "SELECT value FROM app_secrets WHERE name='voice.homeassistant.token_revision'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let current = old
            .as_deref()
            .unwrap_or("0")
            .parse::<u64>()
            .ok()
            .filter(|v| *v < 9_007_199_254_740_991)
            .ok_or_else(|| SchemaError::InvalidRecord("旧 HA 凭据版本损坏".into()))?;
        tx.execute(
            "UPDATE app_secrets SET value=?1,updated_at=?2 WHERE name='voice.homeassistant.token'",
            params![cipher, now],
        )?;
        tx.execute("INSERT INTO app_secrets(name,value,updated_at) VALUES('voice.homeassistant.token_revision',?1,?2) ON CONFLICT(name) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",params![(current+1).to_string(),now])?;
        tx.execute("DELETE FROM app_secrets WHERE name IN ('voice.homeassistant.voice_read_consent','voice.homeassistant.voice_control_consent')", [])?;
        tx.execute("UPDATE approvals SET status='expired',updated_at=?1 WHERE kind='home_control' AND status IN ('pending','approved')",params![now])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn get_secret(&self, name: &str) -> Result<Option<String>, SchemaError> {
        let mut stmt = self
            .connection
            .prepare_cached("SELECT value FROM app_secrets WHERE name=?1")?;
        stmt.query_row([name], |row| row.get(0))
            .optional()
            .map_err(SchemaError::from)
    }

    pub fn set_secret(&self, name: &str, value: &str, now: f64) -> Result<(), SchemaError> {
        let mut stmt = self.connection.prepare_cached(
            "INSERT INTO app_secrets(name,value,updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(name) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
        )?;
        stmt.execute(params![name, value, now])?;
        Ok(())
    }

    pub fn delete_secret(&self, name: &str) -> Result<bool, SchemaError> {
        let mut stmt = self
            .connection
            .prepare_cached("DELETE FROM app_secrets WHERE name=?1")?;
        Ok(stmt.execute([name])? > 0)
    }

    /// 在一个事务内写入（`Some`）或删除（`None`）多项密钥，任一项失败整批不生效。
    /// 返回每项修改前的值，调用方后续步骤（如写 `settings.json`）失败时交给 [`Self::restore_secrets`] 还原。
    pub fn apply_secret_changes(
        &self,
        changes: &[(String, Option<String>)],
        now: f64,
    ) -> Result<Vec<(String, Option<String>)>, SchemaError> {
        let tx = self.connection.unchecked_transaction()?;
        let mut previous = Vec::with_capacity(changes.len());
        for (name, value) in changes {
            let old: Option<String> = tx
                .query_row(
                    "SELECT value FROM app_secrets WHERE name=?1",
                    [name],
                    |row| row.get(0),
                )
                .optional()?;
            match value {
                Some(value) => tx.execute(
                    "INSERT INTO app_secrets(name,value,updated_at) VALUES(?1,?2,?3)
                     ON CONFLICT(name) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
                    params![name, value, now],
                )?,
                None => tx.execute("DELETE FROM app_secrets WHERE name=?1", [name])?,
            };
            previous.push((name.clone(), old));
        }
        tx.commit()?;
        Ok(previous)
    }

    /// 按 [`Self::apply_secret_changes`] 返回的原值还原（逆序应用，同名多次修改也回到批次前的值）。
    pub fn restore_secrets(
        &self,
        previous: &[(String, Option<String>)],
        now: f64,
    ) -> Result<(), SchemaError> {
        let reversed: Vec<_> = previous.iter().rev().cloned().collect();
        self.apply_secret_changes(&reversed, now).map(|_| ())
    }
}
