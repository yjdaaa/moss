//! 设备 token：`base64url_nopad(HMAC-SHA256(secret, "client_id|device_id|ts")) + "." + ts`。
//!
//! 格式与 xiaozhi-esp32-server `core/auth.py`（`7a260ee`，MIT）一致：token 只含签名与签发时间，
//! 不含明文身份；校验时用连接请求头里的 `Client-Id` 与 `Device-Id` 重算签名。默认有效期 30 天。
//! 固件在 WebSocket 请求头里以 `Authorization: Bearer <token>` 发送。

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const DEFAULT_EXPIRE_SECONDS: u64 = 30 * 24 * 60 * 60;

#[derive(Clone)]
pub struct TokenSigner {
    secret: Vec<u8>,
    expire_seconds: u64,
}

// 密钥不出现在调试输出中
impl std::fmt::Debug for TokenSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSigner").field("expire_seconds", &self.expire_seconds).finish_non_exhaustive()
    }
}

impl TokenSigner {
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self { secret: secret.into(), expire_seconds: DEFAULT_EXPIRE_SECONDS }
    }

    pub fn with_expire_seconds(mut self, seconds: u64) -> Self {
        self.expire_seconds = seconds.max(1);
        self
    }

    fn sign(&self, content: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC 接受任意长度密钥");
        mac.update(content.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    /// 为设备签发 token；`now` 为 Unix 秒。
    pub fn issue(&self, client_id: &str, device_id: &str, now: u64) -> String {
        format!("{}.{now}", self.sign(&format!("{client_id}|{device_id}|{now}")))
    }

    /// 校验 token（可带 `Bearer ` 前缀）。签名不符、过期、格式错误都返回 false。
    pub fn verify(&self, token: &str, client_id: &str, device_id: &str, now: u64) -> bool {
        let token = token.trim();
        let token = token.strip_prefix("Bearer ").unwrap_or(token).trim();
        let Some((signature, ts)) = token.rsplit_once('.') else { return false };
        let Ok(ts) = ts.parse::<u64>() else { return false };
        if ts > now.saturating_add(300) || now.saturating_sub(ts) > self.expire_seconds {
            return false;
        }
        let Ok(provided) = URL_SAFE_NO_PAD.decode(signature) else { return false };
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC 接受任意长度密钥");
        mac.update(format!("{client_id}|{device_id}|{ts}").as_bytes());
        // verify_slice 为常量时间比较
        mac.verify_slice(&provided).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_790_000_000;

    #[test]
    fn issue_and_verify() {
        let signer = TokenSigner::new(b"secret-for-test".to_vec());
        let token = signer.issue("client-1", "aa:bb:cc:dd:ee:ff", NOW);
        assert!(signer.verify(&token, "client-1", "aa:bb:cc:dd:ee:ff", NOW + 10));
        assert!(signer.verify(&format!("Bearer {token}"), "client-1", "aa:bb:cc:dd:ee:ff", NOW));
        // 身份不符、密钥不同、篡改、过期、格式错误
        assert!(!signer.verify(&token, "client-2", "aa:bb:cc:dd:ee:ff", NOW));
        assert!(!signer.verify(&token, "client-1", "aa:bb:cc:dd:ee:00", NOW));
        assert!(!TokenSigner::new(b"other".to_vec()).verify(&token, "client-1", "aa:bb:cc:dd:ee:ff", NOW));
        let tampered = token.replacen('.', "x.", 1);
        assert!(!signer.verify(&tampered, "client-1", "aa:bb:cc:dd:ee:ff", NOW));
        assert!(!signer.verify(&token, "client-1", "aa:bb:cc:dd:ee:ff", NOW + DEFAULT_EXPIRE_SECONDS + 1));
        assert!(!signer.verify("", "client-1", "aa:bb:cc:dd:ee:ff", NOW));
        assert!(!signer.verify("abc.notanumber", "client-1", "aa:bb:cc:dd:ee:ff", NOW));
        // 签发时间在未来（超过 5 分钟容差）
        let future = signer.issue("client-1", "aa:bb:cc:dd:ee:ff", NOW + 3600);
        assert!(!signer.verify(&future, "client-1", "aa:bb:cc:dd:ee:ff", NOW));
        assert!(!format!("{signer:?}").contains("secret-for-test"));
    }

    /// 与 xiaozhi-esp32-server `AuthManager._sign` 的输出一致：
    /// python -c "import hmac,hashlib,base64;print(base64.urlsafe_b64encode(hmac.new(b'k',b'c|d|1',hashlib.sha256).digest()).decode().rstrip('='))"
    #[test]
    fn signature_matches_python_reference() {
        let signer = TokenSigner::new(b"k".to_vec());
        assert_eq!(signer.issue("c", "d", 1), format!("{}.1", PY_REFERENCE));
    }

    const PY_REFERENCE: &str = "5QX18MAc5apnxYzict8RsICvoqMir104_9IhMlrQcdc";
}
