use std::net::TcpStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::{ConnectionSpec, DeviceTransport, RealtimeCommand, open_transport};

/// 局域网内通过 mDNS 发现的控制器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredController {
    /// 主机名（不含 `.local`），FluidNC 默认 `fluidnc`
    pub hostname: String,
    pub ip: String,
    /// telnet 端口（`_telnet._tcp`），未广播时为 None
    pub telnet_port: Option<u16>,
    /// HTTP/WebUI 端口（`_http._tcp`），未广播时为 None
    pub http_port: Option<u16>,
}

/// 通过 mDNS 浏览 `_telnet._tcp` 与 `_http._tcp` 在指定时长内收集局域网控制器。
/// FluidNC 会把两种服务都注册在 `<hostname>.local`（默认 `fluidnc.local`）。
/// 只读操作，不向任何设备发送命令；没有多播权限或无网卡时返回空列表而不报错。
pub fn discover_controllers(timeout: Duration) -> Vec<DiscoveredController> {
    discover_controllers_with(timeout, |_| {})
}

/// 同 [`discover_controllers`]，每解析出一个地址（或补上一个端口）就立即回调，便于边发现边确认（2026-11-09）。
pub fn discover_controllers_with(
    timeout: Duration,
    mut on_found: impl FnMut(&DiscoveredController),
) -> Vec<DiscoveredController> {
    use mdns_sd::{ServiceDaemon, ServiceEvent};
    use std::collections::BTreeMap;

    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let receivers: Vec<(&str, _)> = ["_telnet._tcp.local.", "_http._tcp.local."]
        .into_iter()
        .filter_map(|svc| daemon.browse(svc).ok().map(|rx| (svc, rx)))
        .collect();
    if receivers.is_empty() {
        let _ = daemon.shutdown();
        return Vec::new();
    }
    let mut found: BTreeMap<(String, String), DiscoveredController> = BTreeMap::new();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let mut idle = true;
        for (svc, rx) in &receivers {
            while let Ok(event) = rx.try_recv() {
                idle = false;
                if let ServiceEvent::ServiceResolved(info) = event {
                    let hostname = info
                        .get_hostname()
                        .trim_end_matches('.')
                        .trim_end_matches(".local")
                        .to_string();
                    for addr in info.get_addresses() {
                        let ip = addr.to_string();
                        // IPv6 链路本地地址对上位机 TCP 连接价值不大，优先记录 IPv4
                        if ip.contains(':')
                            && found
                                .keys()
                                .any(|(h, i)| h == &hostname && !i.contains(':'))
                        {
                            continue;
                        }
                        let entry =
                            found
                                .entry((hostname.clone(), ip.clone()))
                                .or_insert_with(|| DiscoveredController {
                                    hostname: hostname.clone(),
                                    ip: ip.clone(),
                                    telnet_port: None,
                                    http_port: None,
                                });
                        if svc.starts_with("_telnet") {
                            entry.telnet_port = Some(info.get_port());
                        } else {
                            entry.http_port = Some(info.get_port());
                        }
                        on_found(entry);
                    }
                }
            }
        }
        if idle {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    for (svc, _) in &receivers {
        let _ = daemon.stop_browse(svc);
    }
    let _ = daemon.shutdown();
    let mut list: Vec<DiscoveredController> = found.into_values().collect();
    // IPv4 排前面；同一主机同时有 IPv4 与 IPv6 时只保留 IPv4
    list.sort_by_key(|c| (c.ip.contains(':'), c.hostname.clone(), c.ip.clone()));
    let hosts_with_v4: std::collections::HashSet<String> = list
        .iter()
        .filter(|c| !c.ip.contains(':'))
        .map(|c| c.hostname.clone())
        .collect();
    list.retain(|c| !c.ip.contains(':') || !hosts_with_v4.contains(&c.hostname));
    list
}

/// 局域网扫描时识别出的控制器信息（2026-11-09，只读识别）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControllerIdentity {
    /// 固件名与版本，如 `FluidNC v3.7.8`、`Grbl 1.1h`；只回了状态报告时为 None
    pub firmware: Option<String>,
    /// 状态报告里的机器状态，如 `Idle`、`Run`、`Alarm`
    pub state: Option<String>,
    /// FluidNC `$I` 的 `[MSG:Machine: <配置名>]`，多台机器时用来区分
    pub machine: Option<String>,
}

/// `FluidNC` 后面紧跟的版本号（`v4.1.1`），去掉结尾的 `:` / `]`。
fn fluidnc_version(text: &str) -> Option<String> {
    let rest = &text[text.find("FluidNC")? + "FluidNC".len()..];
    let version = rest
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches([':', ']']);
    Some(if version.is_empty() {
        "FluidNC".to_owned()
    } else {
        format!("FluidNC {version}")
    })
}

/// `[VER:...]` / 启动横幅 → 可读的固件名（2026-11-09 按 FluidNC 源码格式核对）。
/// - FluidNC 4.x `$I`：`[VER:4.1 FluidNC v4.1.1 (esp32-wifi) :<build>`（没有右括号）；3.x：`[VER:3.7.8 FluidNC v3.7.8:]`
/// - 经典 GRBL：`[VER:1.1h.20190825:]`
/// - 启动横幅：`Grbl 4.1 [FluidNC v4.1.1 (esp32-wifi) '$' for help]`、`Grbl 1.1h ['$' for help]`
fn firmware_from_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("[VER:") {
        if rest.contains("FluidNC") {
            return fluidnc_version(rest);
        }
        let version = rest.split([':', ' ', ']']).next().unwrap_or("");
        let short = version.split('.').take(2).collect::<Vec<_>>().join(".");
        return Some(format!(
            "Grbl {}",
            if short.is_empty() { version } else { &short }
        ));
    }
    if trimmed.starts_with("Grbl ") || trimmed.starts_with("GrblHAL ") {
        if trimmed.contains("[FluidNC") {
            return fluidnc_version(trimmed);
        }
        return trimmed
            .split(" [")
            .next()
            .map(|banner| banner.trim().to_owned());
    }
    None
}

/// 只读识别 GRBL / FluidNC：发实时状态查询 `?` 与 `$I`（读固件信息），不发任何会让机器运动或出光的命令。
/// 只有回了状态报告 `<State|...>`、`[VER:...]` 或 GRBL 启动横幅才算雕刻机控制器；单回一个 `ok` 不算。
pub fn identify_grbl(
    transport: &dyn DeviceTransport,
    timeout: Duration,
) -> Option<ControllerIdentity> {
    let _ = transport.write_realtime(RealtimeCommand::StatusQuery);
    let _ = transport.write_line("$I");
    let started = Instant::now();
    let deadline = started + timeout;
    // GRBL / FluidNC 回 `?` 只要几毫秒：这么久一行都没有就不是（扫描时不在别的设备上白等满时限）
    let first_reply_deadline = started + timeout.min(Duration::from_millis(700));
    let mut identity = ControllerIdentity::default();
    let mut grbl = false;
    let mut saw_ok = false;
    let mut any_line = false;
    while Instant::now() < deadline {
        if !any_line && Instant::now() >= first_reply_deadline {
            break;
        }
        match transport.read_line() {
            Ok(Some(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                any_line = true;
                // 还没认出是 GRBL 时先回了别的协议（`login:`、SSH / HTTP 横幅等）：立即判定，不等时限
                let grbl_like = line.starts_with('<')
                    || line.starts_with('[')
                    || line.starts_with("Grbl")
                    || line.eq_ignore_ascii_case("ok")
                    || line.starts_with("error:")
                    || line.starts_with("ALARM:");
                if !grbl && !grbl_like {
                    return None;
                }
                if line.starts_with('<') && line.ends_with('>') && line.contains('|') {
                    let state = line[1..].split(['|', ':', '>']).next().unwrap_or("").trim();
                    if !state.is_empty() {
                        identity.state = Some(state.to_owned());
                        grbl = true;
                    }
                } else if let Some(firmware) = firmware_from_line(line) {
                    identity.firmware = Some(firmware);
                    grbl = true;
                } else if let Some(name) = line
                    .strip_prefix("[MSG:Machine:")
                    .map(|rest| rest.trim_end_matches(']').trim())
                    .filter(|name| !name.is_empty())
                {
                    identity.machine = Some(name.to_owned());
                } else if line.eq_ignore_ascii_case("ok") {
                    saw_ok = true;
                }
                // `$I` 以 ok 结束（VER / OPT / Machine 都在它之前），收齐即返回
                if identity.state.is_some() && saw_ok {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }
    grbl.then_some(identity)
}

/// 连上 `spec` 做一次只读识别；连不上或不是 GRBL 时返回原因。
pub fn identify_controller(
    spec: &ConnectionSpec,
    connect_timeout: Duration,
    reply_timeout: Duration,
) -> Result<ControllerIdentity, String> {
    let transport = open_transport(spec, connect_timeout)?;
    identify_grbl(transport.as_ref(), reply_timeout)
        .ok_or_else(|| "没有按 GRBL / FluidNC 回应".to_owned())
}

/// 本机局域网网卡所在 IPv4 私有网段（各取本机所在的 /24）里开着 `port` 的地址（不含本机）。
/// 只做 TCP 连接尝试，不发数据。直接枚举网卡而不是看出口路由：开着代理 TUN 时出口是虚拟网卡。
pub fn scan_subnet_port(port: u16, connect_timeout: Duration) -> Vec<std::net::Ipv4Addr> {
    scan_subnet_port_with(port, connect_timeout, &|_| {})
}

/// 同 [`scan_subnet_port`]，每连上一个地址就立即回调（2026-11-09：边扫边确认）。
pub fn scan_subnet_port_with(
    port: u16,
    connect_timeout: Duration,
    on_open: &(dyn Fn(std::net::Ipv4Addr) + Sync),
) -> Vec<std::net::Ipv4Addr> {
    let locals = local_lan_ipv4();
    let mut hosts: Vec<std::net::Ipv4Addr> = Vec::new();
    for local in &locals {
        for host in subnet_hosts(*local) {
            if !hosts.contains(&host) && !locals.contains(&host) {
                hosts.push(host);
            }
        }
    }
    scan_hosts_port_with(hosts, port, connect_timeout, on_open)
}

/// 本机局域网网卡的私有 IPv4 地址：有广播地址（Wi-Fi / 有线网），跳过回环与代理 / VPN 的点对点虚拟网卡。
pub fn local_lan_ipv4() -> Vec<std::net::Ipv4Addr> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for interface in interfaces {
        let if_addrs::IfAddr::V4(v4) = &interface.addr else {
            continue;
        };
        let name = interface.name.to_ascii_lowercase();
        let virtual_tunnel = ["utun", "tun", "tap", "ppp", "wg", "ipsec", "gif", "stf"]
            .iter()
            .any(|prefix| name.starts_with(prefix));
        if v4.ip.is_loopback() || !v4.ip.is_private() || v4.broadcast.is_none() || virtual_tunnel {
            continue;
        }
        if !found.contains(&v4.ip) {
            found.push(v4.ip);
        }
    }
    found
}

/// `local` 所在 /24 的其余 253 个主机地址。
pub fn subnet_hosts(local: std::net::Ipv4Addr) -> Vec<std::net::Ipv4Addr> {
    let [a, b, c, d] = local.octets();
    (1..=254u8)
        .filter(|host| *host != d)
        .map(|host| std::net::Ipv4Addr::new(a, b, c, host))
        .collect()
}

/// 并行尝试连接一批地址的 `port`，返回连得上的。
pub fn scan_hosts_port(
    hosts: Vec<std::net::Ipv4Addr>,
    port: u16,
    connect_timeout: Duration,
) -> Vec<std::net::Ipv4Addr> {
    scan_hosts_port_with(hosts, port, connect_timeout, &|_| {})
}

/// 固定工作线程池并发探测一批地址的 `port`（上限 32 线程，避免按 IP 建线程触发系统线程耗尽），连上即回调。
pub fn scan_hosts_port_with(
    hosts: Vec<std::net::Ipv4Addr>,
    port: u16,
    connect_timeout: Duration,
    on_open: &(dyn Fn(std::net::Ipv4Addr) + Sync),
) -> Vec<std::net::Ipv4Addr> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    const MAX_SCAN_WORKERS: usize = 32;
    if hosts.is_empty() {
        return Vec::new();
    }
    let open = Mutex::new(Vec::new());
    let next_index = AtomicUsize::new(0);
    let worker_count = hosts.len().min(MAX_SCAN_WORKERS);
    let probe_loop = |hosts: &[std::net::Ipv4Addr],
                      next_index: &AtomicUsize,
                      open: &Mutex<Vec<std::net::Ipv4Addr>>| {
        loop {
            let idx = next_index.fetch_add(1, Ordering::Relaxed);
            let Some(&host) = hosts.get(idx) else {
                break;
            };
            let addr = std::net::SocketAddr::from((host, port));
            if TcpStream::connect_timeout(&addr, connect_timeout).is_ok() {
                on_open(host);
                if let Ok(mut found) = open.lock() {
                    found.push(host);
                }
            }
        }
    };
    std::thread::scope(|scope| {
        let mut spawned = 0usize;
        for _ in 0..worker_count {
            let hosts_ref = hosts.as_slice();
            let next_ref = &next_index;
            let open_ref = &open;
            if std::thread::Builder::new()
                .name("moss-lan-scan".into())
                .spawn_scoped(scope, move || probe_loop(hosts_ref, next_ref, open_ref))
                .is_ok()
            {
                spawned += 1;
            }
        }
        if spawned == 0 {
            probe_loop(hosts.as_slice(), &next_index, &open);
        }
    });
    let mut found = open.into_inner().unwrap_or_default();
    found.sort();
    found
}

#[cfg(test)]
mod tests;
