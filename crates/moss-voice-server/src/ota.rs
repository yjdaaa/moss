//! OTA 接口：`GET/POST /xiaozhi/ota/`（技术书 §2.1、§5.10）。
//!
//! 设备开机时请求一次：登记设备；已允许的设备拿到 WebSocket 地址与 token，未允许的设备拿到地址但 token 为空，
//! 连接后会收到“请在电脑上允许”的提示。WebSocket 地址的主机名取自设备请求 OTA 时用的地址
//! （`Host` 请求头），保证设备能用同一个地址连回来。

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;

use axum::Router;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use moss_xiaozhi_proto::{FirmwareOffer, OtaInfo, OtaReply};

use crate::{DeviceStatus, DeviceVisit, Shared, unix_now};

pub fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/xiaozhi/ota/", get(ota_get).post(ota_post))
        .route("/xiaozhi/ota", get(ota_get).post(ota_post))
        .with_state(shared)
}

async fn ota_get() -> &'static str {
    "MOSS 内置语音服务：OTA 接口运行中"
}

/// 本机用于对外通信的地址（不发送任何数据，只用于选路）。
fn local_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:80").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip()).filter(|ip| !ip.is_unspecified())
}

/// 从 `Host` 请求头取主机名（去掉端口，保留 IPv6 方括号）。
pub(crate) fn host_without_port(host: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split_once(']').map(|(inner, _)| format!("[{inner}]"));
    }
    let name = match host.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    };
    let valid = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    (valid && !name.is_empty()).then(|| name.to_owned())
}

fn timezone_offset_minutes() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60
}

async fn ota_post(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_owned);
    let info = match OtaInfo::from_request(header, &body) {
        Ok(info) => info,
        Err(error) => {
            shared.state.event(format!("OTA 请求被拒绝（{}）：{error}", peer.ip()));
            return (StatusCode::BAD_REQUEST, error).into_response();
        }
    };
    let visit = DeviceVisit {
        device_id: info.device_id.clone(),
        client_id: info.client_id.clone(),
        model: info.model.clone(),
        firmware_version: info.version.clone(),
        ip: peer.ip().to_string(),
    };
    let status = match shared.registry.seen(&visit) {
        Ok(status) => status,
        Err(error) => {
            shared.state.event(format!("OTA 登记设备失败：{error}"));
            return (StatusCode::INTERNAL_SERVER_ERROR, "设备登记失败").into_response();
        }
    };
    // 2026-11-02：把这次上报的完整 JSON 存成设备快照（固件版本、芯片、flash/psram、MAC、
    // 分区表等）。只在设备已登记后写入；字段随固件版本变化时不改表结构。
    shared
        .registry
        .device_snapshot(&info.device_id, crate::SNAPSHOT_INFO, &body);
    let host = header("host")
        .as_deref()
        .and_then(host_without_port)
        .filter(|host| host != "localhost" && host != "0.0.0.0")
        .or_else(|| local_ip().map(|ip| ip.to_string()))
        .unwrap_or_else(|| "127.0.0.1".into());
    let token = if status == DeviceStatus::Allowed {
        shared.signer.issue(&info.client_id, &info.device_id, unix_now())
    } else {
        String::new()
    };
    // NV17：只对已允许的设备核对上报版本、下发用户确认过的推送
    let firmware = match (&shared.config.firmware, status == DeviceStatus::Allowed) {
        (Some(host), true) => {
            let host = host.clone();
            let (device, version) = (info.device_id.clone(), info.version.clone());
            tokio::task::spawn_blocking(move || {
                host.reported(&device, &version);
                host.offer(&device, &version)
            })
            .await
            .ok()
            .flatten()
        }
        _ => None,
    };
    if let Some((version, _)) = &firmware {
        shared.state.event(format!("OTA：向设备 {} 下发固件 {version}（用户已确认推送）", info.device_id));
    }
    shared.state.event(format!(
        "OTA：设备 {}（{}，固件 {}）{}",
        info.device_id,
        info.model,
        info.version,
        match status {
            DeviceStatus::Allowed => "已允许，已下发连接地址与 token",
            DeviceStatus::Pending => "待允许：请在语音中心允许后重启设备",
            DeviceStatus::Revoked => "已撤销，不下发 token",
        }
    ));
    let reply = OtaReply {
        websocket_url: format!("ws://{host}:{}/xiaozhi/v1/", shared.ws_port),
        token,
        protocol_version: shared.config.protocol_version,
        now_ms: unix_now().saturating_mul(1000),
        timezone_offset_minutes: timezone_offset_minutes(),
        current_version: info.version,
        firmware: firmware.map(|(version, token)| FirmwareOffer {
            version,
            url: format!("http://{host}:{}/xiaozhi/firmware/{token}.bin", shared.http_port),
        }),
    };
    axum::Json(reply.to_json()).into_response()
}

#[cfg(test)]
mod tests {
    use super::host_without_port;

    #[test]
    fn host_header_parsing() {
        assert_eq!(host_without_port("192.168.1.5:8003").as_deref(), Some("192.168.1.5"));
        assert_eq!(host_without_port("moss-pc").as_deref(), Some("moss-pc"));
        assert_eq!(host_without_port("[fe80::1]:8003").as_deref(), Some("[fe80::1]"));
        assert_eq!(host_without_port(""), None);
        assert_eq!(host_without_port("evil/host:80"), None);
    }
}
