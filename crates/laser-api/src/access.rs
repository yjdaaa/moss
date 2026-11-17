use super::*;

impl ApiService {
    /// 系统设置与 Jev 密钥查看/测试门禁：禁止语音、MCP 桥、网关与局域网会话调用；桌面会话启用后仅允许受信任桌面端调用。
    pub(crate) fn require_operator_settings_access(
        &self,
        ctx: &laser_contracts::CallerContext,
    ) -> Option<Vec<u8>> {
        if matches!(
            ctx,
            laser_contracts::CallerContext::Voice { .. }
                | laser_contracts::CallerContext::McpBridge
                | laser_contracts::CallerContext::Approval { .. }
                | laser_contracts::CallerContext::Gateway { .. }
                | laser_contracts::CallerContext::Lan { .. }
        ) || (self.desktop_session.get().is_some()
            && !matches!(ctx, laser_contracts::CallerContext::Desktop))
        {
            return Some(http_json(
                403,
                failure(
                    "该设置只能在 MOSS 桌面端操作",
                    Some(serde_json::json!({"error_code": "desktop_session_required"})),
                ),
            ));
        }
        None
    }
}

/// 浏览器来源白名单（技术书 §6.5）：Tauri 2 内嵌页。
const TAURI_ORIGINS: [&str; 2] = ["http://tauri.localhost", "tauri://localhost"];
/// 前端开发 / 预览端口（`frontend/package.json` 的 5177、5178，Vite 默认 5173）；仅回环请求时允许。
const DEV_LOOPBACK_ORIGINS: [&str; 6] = [
    "http://127.0.0.1:5173",
    "http://localhost:5173",
    "http://127.0.0.1:5177",
    "http://localhost:5177",
    "http://127.0.0.1:5178",
    "http://localhost:5178",
];
/// 判断浏览器 `Origin` 是否允许跨源访问。精确匹配 scheme + host + port，不做前缀或大小写归一。
///
/// 允许：Tauri 内嵌页；与请求 `Host` 同源（MOSS 自己提供的页面，含局域网访问）；
/// 回环请求下的前端开发端口；`extra`（`MOSS_ALLOWED_ORIGINS`）。
pub fn is_allowed_origin(
    origin: &str,
    host_header: Option<&str>,
    loopback: bool,
    extra: &[String],
) -> bool {
    if origin.is_empty() || origin != origin.trim() || origin == "null" {
        return false;
    }
    if TAURI_ORIGINS.contains(&origin) {
        return true;
    }
    if loopback && DEV_LOOPBACK_ORIGINS.contains(&origin) {
        return true;
    }
    if extra.iter().any(|item| item == origin) {
        return true;
    }
    if let Some(host) = host_header.map(str::trim).filter(|host| !host.is_empty())
        && origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
            .is_some_and(|rest| rest == host)
    {
        return true;
    }
    false
}

/// 回环连接的 `Host` 白名单（防 DNS 重绑定，技术书 §6.5）。
///
/// 允许：IPv4 / 方括号 IPv6 字面量、`localhost`（不区分大小写），以及 `extra_origins`
/// （`MOSS_ALLOWED_ORIGINS`）里出现的主机名。端口可有可无，但必须是数字。
/// 只对回环连接生效：非回环连接本来就要 Bearer 鉴权，局域网主机名访问不受影响。
pub fn is_allowed_loopback_host(host: &str, extra_origins: &[String]) -> bool {
    let Some(name) = host_name_without_port(host.trim()) else {
        return false;
    };
    if name.eq_ignore_ascii_case("localhost") || name.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    if let Some(inner) = name
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    }
    extra_origins.iter().any(|origin| {
        origin
            .split_once("://")
            .and_then(|(_, rest)| host_name_without_port(rest))
            .is_some_and(|extra| extra.eq_ignore_ascii_case(name))
    })
}

/// 去掉 `Host` 中的端口；格式不合法（空、端口非数字、未加方括号的 IPv6）时返回 `None`。
fn host_name_without_port(host: &str) -> Option<&str> {
    if host.is_empty() {
        return None;
    }
    let (name, port) = if host.starts_with('[') {
        let end = host.find(']')?;
        let rest = &host[end + 1..];
        let port = match rest {
            "" => None,
            _ => Some(rest.strip_prefix(':')?),
        };
        (&host[..=end], port)
    } else {
        match host.split_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (host, None),
        }
    };
    if name.is_empty() || name == "[]" {
        return None;
    }
    if let Some(port) = port
        && (port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    Some(name)
}

/// 局域网（手机）连接不允许的“参考数据”写操作（技术书 §9.8）：材料库增删改与导入、校准单元格存为推荐参数、系统设置、
/// Jev 密钥测试与查看。雕刻机操作（连接、预览、发送、走边框、急停、安全动作）与只读查询不受影响。
pub fn is_reference_data_write(method: &str, route: &str, body: &[u8]) -> bool {
    if method != "POST" {
        return false;
    }
    match route {
        // select-cell 会把校准单元格保存为材料推荐参数
        // time-calibration 只有清除（reset）一种写操作（r40）
        "/api/settings"
        | "/api/jev/test"
        | "/api/jev/reveal"
        | "/api/material-lab/select-cell"
        | "/api/time-calibration"
        // 雕刻路线全局开关（r45）与文字对刀起点（r48）
        | "/api/engrave-route"
        | "/api/text-origin" => true,
        "/api/material-lab/manage" => {
            let action = serde_json::from_slice::<serde_json::Value>(body)
                .ok()
                .and_then(|value| {
                    value
                        .get("action")
                        .and_then(|a| a.as_str())
                        .map(str::to_owned)
                });
            // 只有导出是只读；解析失败或缺 action 一律按写入拒绝
            action.as_deref() != Some("export")
        }
        _ => false,
    }
}

/// 本机用于局域网通信的 IPv4 地址：对外“连接”一个 UDP 套接字（不发送任何数据包）后读本地地址。
/// 没有网络或只有回环时返回空。
pub(crate) fn primary_lan_ipv4() -> Option<std::net::Ipv4Addr> {
    let is_usable_lan_v4 = |ip: std::net::Ipv4Addr| -> bool {
        if ip.is_loopback() || ip.is_unspecified() || ip.is_link_local() || ip.is_broadcast() {
            return false;
        }
        let o = ip.octets();
        // 排除 198.18.0.0/15 (Clash/Sing-box TUN) 与 100.64.0.0/10 (Tailscale CGNAT)
        !(o[0] == 198 && (o[1] == 18 || o[1] == 19)) && !(o[0] == 100 && (o[1] & 0xc0) == 64)
    };
    for probe in [
        "192.0.2.1:9",
        "10.255.255.254:9",
        "192.168.1.1:9",
        "172.16.0.1:9",
    ] {
        if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0")
            && socket.connect(probe).is_ok()
            && let Ok(addr) = socket.local_addr()
            && let std::net::IpAddr::V4(ip) = addr.ip()
            && is_usable_lan_v4(ip)
        {
            return Some(ip);
        }
    }
    None
}

/// 构造调用方上下文：进程内调用方显式给出的优先；否则回环 → `Loopback`，局域网配对 → `Lan`。
pub(crate) fn caller_context_for(
    ctx_override: Option<laser_contracts::CallerContext>,
    loopback: bool,
    pairing_id: Option<String>,
) -> laser_contracts::CallerContext {
    if let Some(ctx) = ctx_override {
        return ctx;
    }
    match (loopback, pairing_id) {
        (false, Some(pairing_id)) => laser_contracts::CallerContext::Lan { pairing_id },
        // 非回环且未鉴权只可能访问公开 GET 路由，不触发任何需要身份的动作
        (false, None) => laser_contracts::CallerContext::Lan {
            pairing_id: String::new(),
        },
        (true, _) => laser_contracts::CallerContext::Loopback,
    }
}

/// 请求体里的身份类字段一律删除（技术书 §6.4 规则 1，验收 A-36）：身份只由入口构造，不能自报。
const IDENTITY_FIELDS: [&str; 6] = [
    "caller_context",
    "caller",
    "_caller",
    "desktop_session",
    "approval_id",
    "gateway_session",
];

pub(crate) fn strip_identity_fields(object: &mut serde_json::Map<String, serde_json::Value>) {
    for key in IDENTITY_FIELDS {
        object.remove(key);
    }
}
