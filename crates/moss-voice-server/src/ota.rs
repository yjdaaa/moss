//! OTA 接口：`GET/POST /xiaozhi/ota/`（技术书 §2.1、§5.10）。
//!
//! 设备开机时请求一次：登记设备；已允许的设备拿到 WebSocket 地址与 token，未允许的设备拿到地址但 token 为空，
//! 连接后会收到“请在电脑上允许”的提示。WebSocket 地址的主机名取自设备请求 OTA 时用的地址
//! （`Host` 请求头），保证设备能用同一个地址连回来。

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
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
/// 固定 mDNS 局域网主机名：小智设备填入 `http://moss.local:8003/xiaozhi/ota/` 后，换 Wi-Fi 无需重改地址。
pub const MDNS_HOST_LABEL: &str = "moss.local";

/// 判断 IPv4 地址或对应网卡是否属于代理 TUN、VPN 隧道或虚拟机桥接网卡。
pub fn is_virtual_or_tunnel_ipv4(if_name: &str, ip: Ipv4Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_link_local() || ip.is_broadcast() {
        return true;
    }
    let octets = ip.octets();
    // 198.18.0.0/15：Clash / Mihomo / Sing-box 等代理软件 TUN 虚拟网卡地址段
    if octets[0] == 198 && (octets[1] == 18 || octets[1] == 19) {
        return true;
    }
    // 100.64.0.0/10：Tailscale / ZeroTier 等 CGNAT 异地组网虚拟地址段
    if octets[0] == 100 && (octets[1] & 0xc0) == 64 {
        return true;
    }
    let lower = if_name.to_ascii_lowercase();
    const VIRTUAL_KEYWORDS: [&str; 18] = [
        "vmware",
        "vmnet",
        "vbox",
        "virtual",
        "vethernet",
        "wsl",
        "hyper-v",
        "docker",
        "tailscale",
        "zerotier",
        "clash",
        "mihomo",
        "sing-box",
        "wintun",
        "wireguard",
        "loopback",
        "npcap",
        "utun",
    ];
    if VIRTUAL_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
        return true;
    }
    false
}

fn interface_priority(if_name: &str, ip: Ipv4Addr) -> (u8, u8, u8) {
    let lower = if_name.to_ascii_lowercase();
    let name_rank = if lower.contains("wlan")
        || lower.contains("wi-fi")
        || lower.contains("wifi")
        || lower == "en0"
    {
        0
    } else if lower.contains("eth") || lower.contains("以太网") || lower.starts_with("en") {
        1
    } else {
        2
    };
    let octets = ip.octets();
    // Windows 移动热点固定为 192.168.137.1，属于真实可用热点网关；其余 .1 结尾多为未命名的本地虚拟网桥
    let is_windows_hotspot = octets == [192, 168, 137, 1];
    let host_rank = if octets[3] != 1 || is_windows_hotspot { 0 } else { 1 };
    let private_rank = if ip.is_private() { 0 } else { 1 };
    (name_rank, host_rank, private_rank)
}

/// 枚举本机所有真实局域网 IPv4 地址（已排除 Clash TUN、Tailscale、VMware 等虚拟网卡，按 Wi-Fi/有线优先级排序）。
pub fn lan_ipv4_addrs() -> Vec<Ipv4Addr> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut candidates: Vec<(String, Ipv4Addr)> = interfaces
        .into_iter()
        .filter(|iface| !iface.is_loopback())
        .filter_map(|iface| match iface.ip() {
            IpAddr::V4(v4) if !is_virtual_or_tunnel_ipv4(&iface.name, v4) => Some((iface.name, v4)),
            _ => None,
        })
        .collect();
    candidates.sort_by_key(|(name, ip)| (interface_priority(name, *ip), ip.octets()));
    let mut result = Vec::with_capacity(candidates.len());
    for (_, ip) in candidates {
        if !result.contains(&ip) {
            result.push(ip);
        }
    }
    result
}

/// 本机用于局域网通信的首选地址（只用于选路，不发送任何数据；自动跳过 198.18.0.1 等虚拟网卡）。
pub fn local_ip() -> Option<IpAddr> {
    if let Some(ip) = lan_ipv4_addrs().into_iter().next() {
        return Some(IpAddr::V4(ip));
    }
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if !is_virtual_or_tunnel_ipv4("", v4) => Some(IpAddr::V4(v4)),
        _ => None,
    }
}

/// 针对来访设备 `peer` 选择与其同网段的本机真实网卡地址。
pub fn local_ip_for_peer(peer: IpAddr) -> Option<IpAddr> {
    if !peer.is_loopback() && !peer.is_unspecified() {
        if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
            if socket.connect(SocketAddr::new(peer, 80)).is_ok() {
                if let Ok(local) = socket.local_addr() {
                    match local.ip() {
                        IpAddr::V4(v4) if !is_virtual_or_tunnel_ipv4("", v4) => {
                            return Some(IpAddr::V4(v4));
                        }
                        IpAddr::V6(v6) if !v6.is_loopback() && !v6.is_unspecified() => {
                            return Some(IpAddr::V6(v6));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    local_ip()
}

/// 在局域网物理网卡上启动 `moss.local.` mDNS 广播，使小智固定填写 `http://moss.local:8003/xiaozhi/ota/` 即可跨 Wi-Fi 自动解析。
pub(crate) fn start_mdns_responder(http_port: u16, ws_port: u16) -> Option<mdns_sd::ServiceDaemon> {
    let ips = lan_ipv4_addrs();
    if ips.is_empty() {
        return None;
    }
    let daemon = mdns_sd::ServiceDaemon::new().ok()?;
    let _ = daemon.disable_interface(mdns_sd::IfKind::All);
    for ip in &ips {
        let _ = daemon.enable_interface(IpAddr::V4(*ip));
    }
    let ip_addrs: Vec<IpAddr> = ips.iter().copied().map(IpAddr::V4).collect();
    let host_fqdn = format!("{MDNS_HOST_LABEL}.");
    if let Ok(http_info) = mdns_sd::ServiceInfo::new(
        "_http._tcp.local.",
        "moss-xiaozhi-ota",
        &host_fqdn,
        &ip_addrs[..],
        http_port,
        None,
    ) {
        let _ = daemon.register(http_info);
    }
    let _ = ws_port;
    Some(daemon)
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
    // 登记、授权核对与快照都走本地 SQLite（同步锁）：放到阻塞线程，不占 tokio 工作线程。
    let registry = shared.registry.clone();
    let (device_id, client_id, report) = (info.device_id.clone(), info.client_id.clone(), body);
    let registered = tokio::task::spawn_blocking(move || {
        let status = registry.seen(&visit)?;
        // 2026-11-02：把这次上报的完整 JSON 存成设备快照（固件版本、芯片、flash/psram、MAC、
        // 分区表等）；字段随固件版本变化时不改表结构。
        // 2026-11-05：OTA 请求不带 token，`seen` 对“同 Device-Id、不同 Client-Id”的来访仍返回已绑定记录的状态，
        // 所以只在设备已允许且 Client-Id 与绑定记录一致（`authorize_http`，纯读）时才写快照，
        // 冒用 Device-Id 的局域网主机不能改写桌面端「设备信息」。
        let bound = status == DeviceStatus::Allowed
            && registry.authorize_http(&device_id, &client_id).ok().flatten().is_some();
        if bound {
            registry.device_snapshot(&device_id, crate::SNAPSHOT_INFO, &report);
        }
        Ok::<_, String>((status, bound))
    })
    .await
    .unwrap_or_else(|error| Err(format!("登记任务失败：{error}")));
    let (status, bound) = match registered {
        Ok(registered) => registered,
        Err(error) => {
            shared.state.event(format!("OTA 登记设备失败：{error}"));
            return (StatusCode::INTERNAL_SERVER_ERROR, "设备登记失败").into_response();
        }
    };
    let raw_host = header("host").as_deref().and_then(host_without_port);
    let host = match raw_host.as_deref() {
        Some(h) if h.eq_ignore_ascii_case("localhost") || h == "0.0.0.0" => local_ip_for_peer(peer.ip())
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "127.0.0.1".into()),
        Some(h) if h.to_ascii_lowercase().ends_with(".local") => {
            if !peer.ip().is_loopback() {
                local_ip_for_peer(peer.ip())
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| h.to_owned())
            } else {
                h.to_owned()
            }
        }
        Some(h) => h.to_owned(),
        None => local_ip_for_peer(peer.ip())
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "127.0.0.1".into()),
    };
    let token = if status == DeviceStatus::Allowed {
        shared.signer.issue(&info.client_id, &info.device_id, unix_now())
    } else {
        String::new()
    };
    // NV17：只对已允许的设备核对上报版本、下发用户确认过的推送。
    // 2026-11-05：同快照，要求 Client-Id 与绑定记录一致，冒用者上报的版本不会把推送记为已升级。
    let firmware = match (&shared.config.firmware, bound) {
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
    use std::net::Ipv4Addr;
    use super::{host_without_port, is_virtual_or_tunnel_ipv4};

    #[test]
    fn host_header_parsing() {
        assert_eq!(host_without_port("192.168.1.5:8003").as_deref(), Some("192.168.1.5"));
        assert_eq!(host_without_port("moss.local:8003").as_deref(), Some("moss.local"));
        assert_eq!(host_without_port("moss-pc").as_deref(), Some("moss-pc"));
        assert_eq!(host_without_port("[fe80::1]:8003").as_deref(), Some("[fe80::1]"));
        assert_eq!(host_without_port(""), None);
        assert_eq!(host_without_port("evil/host:80"), None);
    }

    #[test]
    fn virtual_and_tunnel_interfaces_are_filtered_out() {
        // Clash / Sing-box TUN 虚拟网卡 (198.18.0.0/15) 必须被过滤
        assert!(is_virtual_or_tunnel_ipv4("Meta", Ipv4Addr::new(198, 18, 0, 1)));
        assert!(is_virtual_or_tunnel_ipv4("utun3", Ipv4Addr::new(198, 19, 1, 1)));
        // Tailscale CGNAT (100.64.0.0/10) 必须被过滤
        assert!(is_virtual_or_tunnel_ipv4("Tailscale", Ipv4Addr::new(100, 101, 50, 60)));
        // VMware 虚拟网卡必须被过滤
        assert!(is_virtual_or_tunnel_ipv4("VMware Network Adapter VMnet8", Ipv4Addr::new(192, 168, 51, 1)));
        // 真实 WLAN 局域网地址保留
        assert!(!is_virtual_or_tunnel_ipv4("WLAN", Ipv4Addr::new(10, 12, 19, 12)));
        assert!(!is_virtual_or_tunnel_ipv4("Wi-Fi", Ipv4Addr::new(192, 168, 1, 105)));
    }
}
