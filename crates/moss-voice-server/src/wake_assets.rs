//! 小智设备唤醒词与画风资源包的内存暂存与 HTTP 分块下载路由。
//!
//! 纯打包、校验与指纹逻辑位于 `moss_xiaozhi_display::wake_pack`，此处原样重导出并补充
//! 一次性下载令牌与 Axum 分块流路由。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

pub use moss_xiaozhi_display::wake_pack::*;

/// 一台设备待下载的资源包。
struct Staged {
    device_id: String,
    bytes: std::sync::Arc<Vec<u8>>,
    expires: Instant,
}

/// 服务端内存里的待下载资源包：令牌随机、只对一台设备、30 分钟过期；同一台设备新推送会顶掉旧的。
#[derive(Default)]
pub struct AssetsStore {
    staged: Mutex<HashMap<String, Staged>>,
}

impl AssetsStore {
    pub fn stage(&self, device_id: &str, bytes: Vec<u8>) -> Result<String, String> {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).map_err(|_| "无法生成下载令牌".to_owned())?;
        let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let mut map = self
            .staged
            .lock()
            .map_err(|_| "资源包表锁失败".to_owned())?;
        let now = Instant::now();
        map.retain(|_, s| s.expires > now && s.device_id != device_id);
        map.insert(
            token.clone(),
            Staged {
                device_id: device_id.to_owned(),
                bytes: std::sync::Arc::new(bytes),
                expires: now + STAGED_TTL,
            },
        );
        Ok(token)
    }

    /// 按令牌取资源包：返回 (设备编号, 内容)。
    pub fn fetch(&self, token: &str) -> Option<(String, std::sync::Arc<Vec<u8>>)> {
        if !crate::firmware::token_ok(token) {
            return None;
        }
        let map = self.staged.lock().ok()?;
        map.get(token)
            .filter(|s| s.expires > Instant::now())
            .map(|s| (s.device_id.clone(), s.bytes.clone()))
    }

    /// 移除指定设备的内存待下载唤醒词 / 画风资源包（n134）。
    pub fn clear_device(&self, device_id: &str) {
        if let Ok(mut map) = self.staged.lock() {
            map.retain(|_, s| s.device_id != device_id);
        }
    }
}

/// 资源包分块下发：连接用完释放时，所有分块都已交给连接才通知宿主“设备下载完了”
/// （带 Content-Length 时连接发够字节就不再拉流，所以在释放时判断；中途断开时还有分块没交出）。
struct DownloadChunks {
    bytes: std::sync::Arc<Vec<u8>>,
    at: usize,
    done: Option<(std::sync::Arc<crate::Shared>, String)>,
}

const DOWNLOAD_CHUNK: usize = 64 * 1024;

impl Iterator for DownloadChunks {
    type Item = Result<axum::body::Bytes, std::convert::Infallible>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.at >= self.bytes.len() {
            return None;
        }
        let end = (self.at + DOWNLOAD_CHUNK).min(self.bytes.len());
        let chunk = axum::body::Bytes::copy_from_slice(&self.bytes[self.at..end]);
        self.at = end;
        Some(Ok(chunk))
    }
}

impl Drop for DownloadChunks {
    fn drop(&mut self) {
        let Some((shared, device)) = self.done.take() else {
            return;
        };
        if self.at < self.bytes.len() {
            shared.state.event(
                crate::EventRecord::warn(
                    crate::DiagCategory::Firmware,
                    format!(
                        "唤醒词资源包：设备 {device} 下载中断（{} / {} 字节）",
                        self.at,
                        self.bytes.len()
                    ),
                )
                .hint("设备网络不稳或中途断电：等它重新连上后在“设备 › 唤醒词”重新推送"),
            );
            return;
        }
        shared.state.event(
            crate::EventRecord::info(
                crate::DiagCategory::Firmware,
                format!(
                    "唤醒词资源包：设备 {device} 已下载完 {} 字节",
                    self.bytes.len()
                ),
            )
            .hint("设备写入资源分区后会自动重启，之后开机就按新资源包显示"),
        );
        let registry = shared.registry.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn_blocking(move || registry.assets_downloaded(&device));
        } else {
            registry.assets_downloaded(&device);
        }
    }
}

pub(crate) fn router(shared: std::sync::Arc<crate::Shared>) -> axum::Router {
    use axum::extract::{ConnectInfo, Path, State};
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    async fn download(
        State(shared): State<std::sync::Arc<crate::Shared>>,
        ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
        Path(file): Path<String>,
    ) -> axum::response::Response {
        let token = file.strip_suffix(".bin").unwrap_or(&file);
        match shared.assets_store.fetch(token) {
            Some((device, bytes)) => {
                shared.state.event(crate::EventRecord::info(
                    crate::DiagCategory::Firmware,
                    format!(
                        "唤醒词资源包：设备 {device}（{}）开始下载 {} 字节",
                        peer.ip(),
                        bytes.len()
                    ),
                ));
                // 分块下发：最后一块交给连接后再拉一次流时才算下载完，中途断开不会走到这里
                let total = bytes.len();
                let chunks = DownloadChunks {
                    bytes,
                    at: 0,
                    done: Some((shared.clone(), device)),
                };
                (
                    [
                        (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
                        // 固件按 Content-Length 判断分区放不放得下，分块下发也要给准确长度
                        (header::CONTENT_LENGTH, total.to_string()),
                    ],
                    axum::body::Body::from_stream(futures_util::stream::iter(chunks)),
                )
                    .into_response()
            }
            None => {
                shared.state.event(
                    crate::EventRecord::warn(
                        crate::DiagCategory::Firmware,
                        format!("唤醒词资源包下载被拒绝（{}）：令牌无效或已过期", peer.ip()),
                    )
                    .hint("资源包只保留 30 分钟：在“唤醒词与音量”里重新推送"),
                );
                StatusCode::NOT_FOUND.into_response()
            }
        }
    }
    axum::Router::new()
        .route("/xiaozhi/assets/{file}", axum::routing::get(download))
        .with_state(shared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn n61_staged_package_is_one_device_one_token_and_expires() {
        let store = AssetsStore::default();
        let first = store.stage("dev-a", vec![1, 2, 3]).unwrap();
        assert!(crate::firmware::token_ok(&first));
        assert_eq!(
            store.fetch(&first).map(|(d, b)| (d, b.as_ref().clone())),
            Some(("dev-a".into(), vec![1, 2, 3]))
        );
        let other = store.stage("dev-b", vec![9]).unwrap();
        let second = store.stage("dev-a", vec![4]).unwrap();
        assert!(
            store.fetch(&first).is_none(),
            "同一台设备的新推送顶掉旧令牌"
        );
        assert!(store.fetch(&second).is_some() && store.fetch(&other).is_some());
        assert!(store.fetch("../etc/passwd").is_none() && store.fetch(&"0".repeat(32)).is_none());
        store
            .staged
            .lock()
            .unwrap()
            .get_mut(&second)
            .unwrap()
            .expires = Instant::now() - Duration::from_secs(1);
        assert!(store.fetch(&second).is_none(), "过期后不再给");
    }
}
