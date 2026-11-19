use super::*;

#[derive(Debug, Clone)]
pub struct PairChallenge {
    pub code: String,
    pub expires_at: u64,
}

#[derive(Debug)]
struct PairState {
    code: String,
    expires_at: u64,
    used: bool,
    static_pin_hash: Option<String>,
    failures: HashMap<String, (u32, u64)>,
    locked_until: HashMap<String, u64>,
    tokens: HashMap<String, String>,
}

#[derive(Debug)]
pub struct AuthManager {
    state: std::sync::Mutex<PairState>,
    runtime_path: Option<std::path::PathBuf>,
}

impl AuthManager {
    pub fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(PairState {
                code: String::new(),
                expires_at: 0,
                used: false,
                static_pin_hash: None,
                failures: HashMap::new(),
                locked_until: HashMap::new(),
                tokens: HashMap::new(),
            }),
            runtime_path: None,
        }
    }

    /// 从运行目录 `settings.json` 恢复已配对设备 Token 哈希与自定义固定配对码哈希，
    /// 使手机配对后在 MOSS 重启或电脑重启后仍保持长久连接。
    pub fn with_runtime_path(runtime_path: std::path::PathBuf) -> Self {
        let saved = read_runtime_settings(Some(runtime_path.as_path()));
        let static_pin_hash = saved
            .get("lan_static_pin_hash")
            .and_then(|v| v.as_str())
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(|s| s.to_ascii_lowercase());
        let mut tokens = HashMap::new();
        if let Some(obj) = saved.get("lan_paired_tokens").and_then(|v| v.as_object()) {
            for (k, v) in obj.iter().take(64) {
                if k.len() == 64
                    && k.bytes().all(|b| b.is_ascii_hexdigit())
                    && let Some(id) = v.as_str()
                {
                    tokens.insert(k.to_ascii_lowercase(), id.to_owned());
                }
            }
        }
        Self {
            state: std::sync::Mutex::new(PairState {
                code: String::new(),
                expires_at: 0,
                used: false,
                static_pin_hash,
                failures: HashMap::new(),
                locked_until: HashMap::new(),
                tokens,
            }),
            runtime_path: Some(runtime_path),
        }
    }

    pub fn issue_code(&self, now: u64) -> Result<PairChallenge, String> {
        let mut random = [0u8; 4];
        getrandom::fill(&mut random).map_err(|error| error.to_string())?;
        let number = u32::from_le_bytes(random) % 1_000_000;
        let code = format!("{number:06}");
        let challenge = PairChallenge {
            code: code.clone(),
            expires_at: now + 300,
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| "auth state lock poisoned".to_string())?;
        state.code = code;
        state.expires_at = challenge.expires_at;
        state.used = false;
        state.failures.clear();
        state.locked_until.clear();
        Ok(challenge)
    }

    /// 设置或清除自定义固定 6 位数字配对码（仅持久化 SHA-256 哈希，不落盘明文）。
    pub fn set_static_pin(&self, pin: Option<&str>) -> Result<(), String> {
        let next_hash = match pin.map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(raw) => {
                if raw.len() != 6 || !raw.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("自定义配对码必须是 6 位数字".to_owned());
                }
                Some(hex::encode(Sha256::digest(raw.as_bytes())))
            }
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| "auth state lock poisoned".to_owned())?;
        if let Some(path) = self.runtime_path.as_deref() {
            let hash_clone = next_hash.clone();
            update_runtime_settings(Some(path), |current| match hash_clone {
                Some(h) => {
                    current.insert(
                        "lan_static_pin_hash".to_owned(),
                        serde_json::Value::String(h),
                    );
                }
                None => {
                    current.remove("lan_static_pin_hash");
                }
            })?;
        }
        state.static_pin_hash = next_hash;
        state.failures.clear();
        state.locked_until.clear();
        Ok(())
    }

    pub fn has_static_pin(&self) -> bool {
        self.state
            .lock()
            .map(|s| s.static_pin_hash.is_some())
            .unwrap_or(false)
    }

    pub fn paired_count(&self) -> usize {
        self.state.lock().map(|s| s.tokens.len()).unwrap_or(0)
    }

    /// 撤销所有已配对设备的长久连接 Token（在 state 锁内同步写盘，防止与并发配对互相覆盖）。
    pub fn revoke_all_tokens(&self) -> Result<usize, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "auth state lock poisoned".to_owned())?;
        if let Some(path) = self.runtime_path.as_deref() {
            update_runtime_settings(Some(path), |current| {
                current.remove("lan_paired_tokens");
            })?;
        }
        let count = state.tokens.len();
        state.tokens.clear();
        Ok(count)
    }

    pub fn pair(
        &self,
        source_ip: &str,
        code: &str,
        now: u64,
    ) -> Result<(String, String), AuthError> {
        let code = code.trim();
        let mut state = self.state.lock().map_err(|_| AuthError::Internal)?;
        if state
            .locked_until
            .get(source_ip)
            .is_some_and(|until| *until > now)
        {
            return Err(AuthError::Locked(state.locked_until[source_ip] - now));
        }
        let dynamic_valid =
            !state.used && now < state.expires_at && !state.code.is_empty() && code == state.code;
        let static_valid = code.len() == 6
            && code.bytes().all(|b| b.is_ascii_digit())
            && state
                .static_pin_hash
                .as_deref()
                .is_some_and(|expected| expected == hex::encode(Sha256::digest(code.as_bytes())));
        if !dynamic_valid && !static_valid {
            let entry = state
                .failures
                .entry(source_ip.to_string())
                .or_insert((0, now));
            entry.0 += 1;
            if entry.0 >= 5 {
                state.locked_until.insert(source_ip.to_string(), now + 900);
            }
            return Err(AuthError::Invalid);
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| AuthError::Internal)?;
        let token = base64::engine::general_purpose::STANDARD.encode(bytes);
        let token_hash = hex::encode(Sha256::digest(bytes));
        let pairing_id = format!("pair-{}", &token_hash[..12]);
        let mut next_tokens = state.tokens.clone();
        if next_tokens.len() >= 64
            && let Some(first_key) = next_tokens.keys().next().cloned()
        {
            next_tokens.remove(&first_key);
        }
        next_tokens.insert(token_hash, pairing_id.clone());
        // 在 state 锁内同步写盘并校验结果，确保内存与 settings.json 不会因并发撤销而分叉
        if let Some(path) = self.runtime_path.as_deref() {
            let map: serde_json::Map<String, serde_json::Value> = next_tokens
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect();
            update_runtime_settings(Some(path), |current| {
                current.insert(
                    "lan_paired_tokens".to_owned(),
                    serde_json::Value::Object(map),
                );
            })
            .map_err(|_| AuthError::Internal)?;
        }
        if dynamic_valid {
            state.used = true;
        }
        state.failures.remove(source_ip);
        state.locked_until.remove(source_ip);
        state.tokens = next_tokens;
        Ok((token, pairing_id))
    }

    pub fn authorize(&self, bearer: &str) -> Result<String, AuthError> {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(bearer)
            .map_err(|_| AuthError::Unauthorized)?;
        let hash = hex::encode(Sha256::digest(raw));
        self.state
            .lock()
            .map_err(|_| AuthError::Internal)?
            .tokens
            .get(&hash)
            .cloned()
            .ok_or(AuthError::Unauthorized)
    }
}

impl Default for AuthManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    Invalid,
    Locked(u64),
    Unauthorized,
    Internal,
}
