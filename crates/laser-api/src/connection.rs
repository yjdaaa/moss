use super::*;

/// n73 握手成功的连接 → 记录：`fields` 同时带两套别名（串口 `port`/`baudrate`，网络 `host`/`network_host`、
/// `telnet_port`/`network_telnet_port`），各工具按自己认的字段名取用。
fn verified_connection_value(spec: &laser_device::ConnectionSpec, at: u64) -> serde_json::Value {
    match spec {
        laser_device::ConnectionSpec::Serial { port, baud_rate } => serde_json::json!({
            "mode": "serial",
            "label": format!("串口 {port}（{baud_rate}）"),
            "verified_at": at,
            "fields": {"connection_mode": "serial", "port": port, "baudrate": baud_rate},
        }),
        laser_device::ConnectionSpec::Tcp { host, port } => serde_json::json!({
            "mode": "network",
            "label": format!("网络 {host}:{port}"),
            "verified_at": at,
            "fields": {
                "connection_mode": "network",
                "host": host, "network_host": host,
                "telnet_port": port, "network_telnet_port": port,
            },
        }),
    }
}

/// 设置文件里读回的记录必须是上面这种形状，否则忽略。
pub(crate) fn valid_verified_connection(value: &serde_json::Value) -> bool {
    let fields = &value["fields"];
    match fields["connection_mode"].as_str() {
        Some("serial") => fields["port"]
            .as_str()
            .is_some_and(|p| !p.trim().is_empty()),
        Some("network") => fields["network_host"]
            .as_str()
            .is_some_and(|h| !h.trim().is_empty()),
        _ => false,
    }
}

/// 串口枚举函数类型（见 [`ApiService::with_serial_port_lister`]）。
pub type SerialPortLister = std::sync::Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// 局域网扫描的候选地址（2026-11-09）：mDNS 广播到的设备 + 本网段开着 23 端口的设备。
/// 候选只是“可能是”，必须再经 [`laser_device::identify_controller`] 只读识别出 GRBL / FluidNC 才算找到。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerCandidate {
    pub ip: String,
    pub telnet_port: u16,
    /// mDNS 主机名（不含 `.local`）；网段扫描到的为空
    pub hostname: String,
    pub http_port: Option<u16>,
    /// `mdns` / `scan` / `mdns+scan`
    pub source: &'static str,
}

/// 候选地址收集函数类型（见 [`ApiService::with_controller_candidates`]；测试注入本机假设备，不扫真实网络）。
/// 每找到一个候选就调用一次 `emit`，扫描接口随即开始确认（边找边验）；函数返回即表示找完。
pub type ControllerCandidateFinder =
    std::sync::Arc<dyn Fn(&(dyn Fn(ControllerCandidate) + Sync)) + Send + Sync>;

/// 默认候选：mDNS 浏览（1 秒）与本网段 23 端口扫描同时进行，各自找到一个就交出一个。
pub(crate) fn find_controller_candidates(emit: &(dyn Fn(ControllerCandidate) + Sync)) {
    std::thread::scope(|scope| {
        scope.spawn(|| {
            laser_device::discover_controllers_with(
                std::time::Duration::from_millis(1000),
                |found| {
                    if !found.ip.contains(':') {
                        emit(ControllerCandidate {
                            ip: found.ip.clone(),
                            telnet_port: found.telnet_port.unwrap_or(23),
                            hostname: found.hostname.clone(),
                            http_port: found.http_port,
                            source: "mdns",
                        });
                    }
                },
            );
        });
        laser_device::scan_subnet_port_with(23, std::time::Duration::from_millis(300), &|ip| {
            emit(ControllerCandidate {
                ip: ip.to_string(),
                telnet_port: 23,
                hostname: String::new(),
                http_port: None,
                source: "scan",
            });
        });
    });
}

impl ApiService {
    /// n73 工作台最近一次握手成功的连接：`{mode, label, verified_at, fields}`，`fields` 是要补进工具参数的连接字段。
    pub fn verified_connection(&self) -> Option<serde_json::Value> {
        self.verified_connection
            .read()
            .ok()
            .and_then(|slot| slot.clone())
    }

    /// n73 记住一次握手成功的连接，并写进 `settings.json`（写不进去只影响重启后是否还记得）。
    pub fn remember_verified_connection(&self, spec: &laser_device::ConnectionSpec) {
        let value = verified_connection_value(spec, unix_seconds());
        if let Ok(mut slot) = self.verified_connection.write() {
            *slot = Some(value.clone());
        }
        let _ = update_runtime_settings(self.runtime_path.as_deref(), |current| {
            current.insert("laser_verified_connection".into(), value);
        });
    }

    /// 局域网扫描（2026-11-09 用户要求）：收集候选后逐台只读识别（`?` + `$I`），只返回按 GRBL / FluidNC
    /// 回应的设备；正在加工或待确认停机的目标不另开连接打扰，单独列出。
    pub(crate) fn discover_verified_controllers(&self) -> serde_json::Value {
        use std::sync::atomic::Ordering;
        const MAX_PROBE_WORKERS: usize = 8;
        if self.is_shutdown_requested() {
            return serde_json::json!({
                "controllers": [],
                "busy": [],
                "checked": 0,
                "not_controllers": 0,
                "hint": "服务正在关闭，已拒绝局域网设备扫描",
            });
        }
        struct DiscoveryInFlightGuard<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for DiscoveryInFlightGuard<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        if self
            .discovery_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return serde_json::json!({
                "controllers": [],
                "busy": [],
                "checked": 0,
                "not_controllers": 0,
                "hint": "已有局域网设备扫描正在进行，请稍后重试",
            });
        }
        let _discovery_guard = DiscoveryInFlightGuard(&self.discovery_in_flight);

        let busy_workflow_for = |target: &str| -> Option<String> {
            if let Ok(jobs) = self.active_jobs.lock()
                && let Some(job) = jobs.values().find(|j| j.target == target)
            {
                return Some(job.workflow_id.clone());
            }
            if let Ok(uncertain) = self.uncertain_jobs.lock()
                && let Some(job) = uncertain.values().find(|j| j.target == target)
            {
                return Some(job.workflow_id.clone());
            }
            if let Ok(short_io) = self.short_io_targets.lock()
                && short_io.contains_key(target)
            {
                return Some("probing".to_owned());
            }
            None
        };

        enum ProbeOutcome {
            Done(String, Result<laser_device::ControllerIdentity, String>),
            Skipped {
                ip: String,
                telnet_port: u16,
                workflow_id: String,
            },
        }

        // 2026-11-09：边找边验——候选到达后交给固定小线程池（上限 8 线程）确认，防止并发请求打爆 OS 线程数
        let (candidate_tx, candidate_rx) = std::sync::mpsc::channel::<ControllerCandidate>();
        let finder = self.controller_candidates.clone();
        let finder_thread = std::thread::Builder::new()
            .name("moss-lan-finder".into())
            .spawn(move || {
                finder(&move |candidate| {
                    let _ = candidate_tx.send(candidate);
                });
            });
        let (work_tx, work_rx) = std::sync::mpsc::channel::<(String, ControllerCandidate)>();
        let work_rx = std::sync::Arc::new(std::sync::Mutex::new(work_rx));
        let (result_tx, result_rx) = std::sync::mpsc::channel::<ProbeOutcome>();
        let reply_timeout = self.discovery_reply_timeout;
        let mut workers = Vec::new();
        for _ in 0..MAX_PROBE_WORKERS {
            let work_rx = work_rx.clone();
            let result_tx = result_tx.clone();
            let busy = DiscoveryBusyTables {
                open_locks: self.target_open_locks.clone(),
                active_jobs: self.active_jobs.clone(),
                uncertain_jobs: self.uncertain_jobs.clone(),
                short_io_targets: self.short_io_targets.clone(),
            };
            let shutdown_flag = self.shutdown_requested.clone();
            if let Ok(handle) = std::thread::Builder::new()
                .name("moss-lan-probe".into())
                .spawn(move || {
                    loop {
                        let next = work_rx.lock().ok().and_then(|rx| rx.recv().ok());
                        let Some((key, candidate)) = next else {
                            break;
                        };
                        if shutdown_flag.load(Ordering::Acquire) {
                            break;
                        }
                        match probe_discovery_candidate(&busy, &candidate, reply_timeout) {
                            Err(workflow_id) => {
                                let _ = result_tx.send(ProbeOutcome::Skipped {
                                    ip: candidate.ip,
                                    telnet_port: candidate.telnet_port,
                                    workflow_id,
                                });
                            }
                            Ok(result) => {
                                let _ = result_tx.send(ProbeOutcome::Done(key, result));
                            }
                        }
                    }
                })
            {
                workers.push(handle);
            }
        }
        let mut seen: std::collections::BTreeMap<String, ControllerCandidate> = Default::default();
        let mut skipped = Vec::new();
        let mut inline_work = Vec::new();
        match finder_thread {
            Ok(handle) => {
                for candidate in candidate_rx {
                    let key = discovery_target(&candidate).describe();
                    if let Some(known) = seen.get_mut(&key) {
                        if known.hostname.is_empty() {
                            known.hostname = candidate.hostname.clone();
                        }
                        known.http_port = known.http_port.or(candidate.http_port);
                        if known.source != candidate.source {
                            known.source = "mdns+scan";
                        }
                        continue;
                    }
                    seen.insert(key.clone(), candidate.clone());
                    if let Some(workflow) = busy_workflow_for(&key) {
                        skipped.push(serde_json::json!({
                            "ip": candidate.ip, "telnet_port": candidate.telnet_port, "workflow_id": workflow,
                        }));
                        continue;
                    }
                    if workers.is_empty() {
                        inline_work.push((key, candidate));
                    } else {
                        let _ = work_tx.send((key, candidate));
                    }
                }
                let _ = handle.join();
            }
            Err(_) => {
                // 极端线程紧张时回退到当前线程收集候选，不触发 panic
                let collected = std::sync::Mutex::new(Vec::new());
                self.controller_candidates.as_ref()(&|candidate| {
                    if let Ok(mut list) = collected.lock() {
                        list.push(candidate);
                    }
                });
                for candidate in collected.into_inner().unwrap_or_default() {
                    let key = discovery_target(&candidate).describe();
                    if let Some(known) = seen.get_mut(&key) {
                        if known.hostname.is_empty() {
                            known.hostname = candidate.hostname.clone();
                        }
                        known.http_port = known.http_port.or(candidate.http_port);
                        if known.source != candidate.source {
                            known.source = "mdns+scan";
                        }
                        continue;
                    }
                    seen.insert(key.clone(), candidate.clone());
                    if let Some(workflow) = busy_workflow_for(&key) {
                        skipped.push(serde_json::json!({
                            "ip": candidate.ip, "telnet_port": candidate.telnet_port, "workflow_id": workflow,
                        }));
                        continue;
                    }
                    if workers.is_empty() {
                        inline_work.push((key, candidate));
                    } else {
                        let _ = work_tx.send((key, candidate));
                    }
                }
            }
        }
        drop(work_tx);
        for (key, candidate) in inline_work {
            if self.is_shutdown_requested() {
                break;
            }
            let busy = DiscoveryBusyTables {
                open_locks: self.target_open_locks.clone(),
                active_jobs: self.active_jobs.clone(),
                uncertain_jobs: self.uncertain_jobs.clone(),
                short_io_targets: self.short_io_targets.clone(),
            };
            match probe_discovery_candidate(&busy, &candidate, reply_timeout) {
                Err(workflow_id) => {
                    let _ = result_tx.send(ProbeOutcome::Skipped {
                        ip: candidate.ip,
                        telnet_port: candidate.telnet_port,
                        workflow_id,
                    });
                }
                Ok(result) => {
                    let _ = result_tx.send(ProbeOutcome::Done(key, result));
                }
            }
        }
        drop(result_tx);
        for worker in workers {
            let _ = worker.join();
        }
        let candidates: Vec<ControllerCandidate> = seen.values().cloned().collect();
        let mut probed: Vec<(
            ControllerCandidate,
            Result<laser_device::ControllerIdentity, String>,
        )> = Vec::new();
        for item in result_rx {
            match item {
                ProbeOutcome::Done(key, result) => {
                    if let Some(candidate) = seen.get(&key).cloned() {
                        probed.push((candidate, result));
                    }
                }
                ProbeOutcome::Skipped {
                    ip,
                    telnet_port,
                    workflow_id,
                } => skipped.push(serde_json::json!({
                    "ip": ip, "telnet_port": telnet_port, "workflow_id": workflow_id,
                })),
            }
        }
        let mut controllers = Vec::new();
        let mut not_controllers = 0usize;
        for (candidate, result) in probed {
            match result {
                Ok(identity) => controllers.push(serde_json::json!({
                    "ip": candidate.ip,
                    "telnet_port": candidate.telnet_port,
                    "hostname": candidate.hostname,
                    "http_port": candidate.http_port,
                    "source": candidate.source,
                    "firmware": identity.firmware,
                    "state": identity.state,
                    "machine": identity.machine,
                })),
                Err(_) => not_controllers += 1,
            }
        }
        let hint = if !controllers.is_empty() {
            String::new()
        } else if !skipped.is_empty() {
            "找到的雕刻机正在加工，未另开连接打扰；可直接用当前连接".to_owned()
        } else if candidates.is_empty() {
            let subnets: Vec<String> = laser_device::local_lan_ipv4()
                .iter()
                .map(|ip| {
                    let [a, b, c, _] = ip.octets();
                    format!("{a}.{b}.{c}.0/24")
                })
                .collect();
            if subnets.is_empty() {
                "没有找到可用的局域网网卡，也没收到 mDNS 广播：确认电脑已连上和雕刻机相同的网络"
                    .to_owned()
            } else {
                format!(
                    "在 {} 扫描了 23 端口并监听 mDNS 广播，没找到可能的控制器：确认雕刻机已开机、和电脑在同一网段（大网段只扫本机所在的这一段；FluidNC 可开启 $MDNS/Enable=ON，或直接填 IP）",
                    subnets.join("、")
                )
            }
        } else {
            format!(
                "检查了 {} 个地址，没有设备按 GRBL / FluidNC 回应",
                candidates.len()
            )
        };
        serde_json::json!({
            "controllers": controllers,
            "busy": skipped,
            "checked": candidates.len(),
            "not_controllers": not_controllers,
            "hint": hint,
        })
    }

    /// 从请求参数构造连接描述（串口留空时自动选择首个枚举到的系统串口）。
    pub(crate) fn connection_spec_from_request(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<laser_device::ConnectionSpec, String> {
        let mode = object
            .get("connection_mode")
            .or_else(|| object.get("preferred_mode"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(self.settings.default_connection_mode.as_str())
            .to_ascii_lowercase();
        if mode == "serial" {
            let mut port = object
                .get("port")
                .or_else(|| object.get("serial_port"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_owned();
            if port.is_empty() {
                port = (self.serial_port_lister)()
                    .into_iter()
                    .next()
                    .ok_or("未选择串口且未发现任何系统串口，请插入 USB 线并点击刷新")?;
            }
            let baud = parse_int(
                object.get("baudrate").or_else(|| object.get("baud")),
                "baudrate",
                115_200,
            )
            .map_err(|e| e.to_string())?;
            if !(1200..=4_000_000).contains(&baud) {
                return Err("baudrate 超出合法范围 (1200 ~ 4000000)".into());
            }
            Ok(laser_device::ConnectionSpec::Serial {
                port,
                baud_rate: baud as u32,
            })
        } else {
            let host = object
                .get("network_host")
                .or_else(|| object.get("host"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_owned();
            if host.is_empty() {
                return Err("请填写设备 host/IP".into());
            }
            let port = parse_int(
                object
                    .get("network_telnet_port")
                    .or_else(|| object.get("telnet_port")),
                "network_telnet_port",
                23,
            )
            .map_err(|e| e.to_string())?;
            if !(1..=65535).contains(&port) {
                return Err("network_telnet_port 超出合法范围".into());
            }
            Ok(laser_device::ConnectionSpec::Tcp {
                host,
                port: port as u16,
            })
        }
    }

    /// 握手探测：只发 `$$` 读取固件设置（不移动轴、不出光）。先探首选模式，
    /// 失败且另一模式参数可用时回退探测，输出与前端契约一致的 attempts/summary 结构。
    pub(crate) fn probe_connections(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> (serde_json::Value, Option<laser_device::ConnectionSpec>) {
        if self.is_shutdown_requested() {
            return (
                serde_json::json!({
                    "connected": false,
                    "error_code": "host_shutting_down",
                    "summary": "服务正在关闭，已拒绝建立连接探测",
                    "attempts": [],
                }),
                None,
            );
        }
        let mut verified: Option<laser_device::ConnectionSpec> = None;
        let preferred = object
            .get("connection_mode")
            .or_else(|| object.get("preferred_mode"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(self.settings.default_connection_mode.as_str())
            .to_ascii_lowercase();
        let preferred = if preferred == "serial" {
            "serial"
        } else {
            "network"
        };
        let other = if preferred == "network" {
            "serial"
        } else {
            "network"
        };
        let timeout_secs = parse_float(object.get("network_timeout"), "network_timeout", 3.0)
            .unwrap_or(3.0)
            .clamp(0.5, 10.0);
        let timeout = std::time::Duration::from_secs_f64(timeout_secs);

        let mut attempts = Vec::new();
        let mut selected: Option<&str> = None;
        let mut serial_connected = false;
        let mut network_connected = false;
        let mut firmware_settings: Vec<String> = Vec::new();
        let mut machine_limits = None;
        let mut machine_features = None;
        let mut summary = String::new();

        let allow_fallback = parse_bool(
            object
                .get("auto_fallback")
                .or_else(|| object.get("allow_fallback")),
            true,
        );
        let modes: &[&str] = if allow_fallback {
            &[preferred, other]
        } else {
            std::slice::from_ref(&preferred)
        };

        for (index, &mode) in modes.iter().enumerate() {
            let mut request = object.clone();
            request.insert(
                "connection_mode".into(),
                serde_json::Value::String(mode.to_owned()),
            );
            let spec = match self.connection_spec_from_request(&request) {
                Ok(spec) => spec,
                Err(error) => {
                    // 回退模式参数缺失不算错误，只记录跳过原因
                    attempts.push(serde_json::json!({
                        "mode": mode, "target": "", "connected": false, "laser_mode_ok": false,
                        "summary": format!("{}: {error}", if index == 0 { "参数无效" } else { "未尝试" })
                    }));
                    if index == 0 {
                        summary = format!(
                            "{} 参数无效: {error}",
                            if mode == "network" {
                                "网络"
                            } else {
                                "串口"
                            }
                        );
                    }
                    continue;
                }
            };
            let target = spec.describe();
            enum ProbeOpenOutcome {
                Active(String),
                Uncertain(String),
                Busy,
                Opened(
                    std::sync::Arc<dyn laser_device::DeviceTransport>,
                    ShortIoGuard,
                ),
                OpenFailed(String),
            }
            // 统一锁序：target_open_mutex -> active_jobs -> uncertain_jobs -> short_io_targets；
            // 只读探测绝不剥离 UncertainJob 活句柄，打开连接后登记 ShortIoGuard 即释放打开锁，
            // 既防止并发双开串口/Telnet，又不阻断急停复用在途句柄写 0x18。
            let opened = {
                let open_mutex = target_open_mutex(&self.target_open_locks, &target);
                let _open_guard = open_mutex
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let active_wf = self.active_jobs.lock().ok().and_then(|jobs| {
                    jobs.values()
                        .find(|j| j.target == target)
                        .map(|j| j.workflow_id.clone())
                });
                if let Some(wf_id) = active_wf {
                    ProbeOpenOutcome::Active(wf_id)
                } else {
                    let uncertain_wf = self.uncertain_jobs.lock().ok().and_then(|uncertain| {
                        uncertain
                            .values()
                            .find(|j| j.target == target)
                            .map(|j| j.workflow_id.clone())
                    });
                    if let Some(wf_id) = uncertain_wf {
                        ProbeOpenOutcome::Uncertain(wf_id)
                    } else if self
                        .short_io_targets
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .contains_key(&target)
                    {
                        ProbeOpenOutcome::Busy
                    } else {
                        match laser_device::open_transport(&spec, timeout) {
                            Ok(t) => {
                                let transport_arc: std::sync::Arc<
                                    dyn laser_device::DeviceTransport,
                                > = std::sync::Arc::from(t);
                                let guard = register_short_io_target(
                                    &self.short_io_targets,
                                    &target,
                                    transport_arc.clone(),
                                );
                                ProbeOpenOutcome::Opened(transport_arc, guard)
                            }
                            Err(e) => ProbeOpenOutcome::OpenFailed(e),
                        }
                    }
                }
            };
            let report = match opened {
                ProbeOpenOutcome::Active(wf_id) => {
                    let busy_summary = format!("设备正在执行任务 {wf_id}");
                    attempts.push(serde_json::json!({
                        "mode": mode,
                        "target": target,
                        "connected": true,
                        "laser_mode_ok": true,
                        "summary": busy_summary,
                    }));
                    if mode == "serial" {
                        serial_connected = true;
                    } else {
                        network_connected = true;
                    }
                    selected = Some(mode);
                    summary = busy_summary;
                    break;
                }
                ProbeOpenOutcome::Uncertain(wf_id) => {
                    let pending_summary = format!(
                        "设备存在待确认停止的任务 {wf_id}，请先核实硬件停机或按急停后再探测"
                    );
                    attempts.push(serde_json::json!({
                        "mode": mode,
                        "target": target,
                        "connected": false,
                        "laser_mode_ok": false,
                        "summary": pending_summary,
                    }));
                    if index == 0 || summary.is_empty() {
                        summary = pending_summary;
                    }
                    continue;
                }
                ProbeOpenOutcome::Busy => {
                    let busy_summary =
                        format!("设备 {target} 正在执行另一次连接探测或状态查询，请稍后重试");
                    attempts.push(serde_json::json!({
                        "mode": mode,
                        "target": target,
                        "connected": false,
                        "laser_mode_ok": false,
                        "summary": busy_summary,
                    }));
                    if index == 0 || summary.is_empty() {
                        summary = busy_summary;
                    }
                    continue;
                }
                ProbeOpenOutcome::OpenFailed(error) => {
                    let label = if mode == "network" {
                        "网络"
                    } else {
                        "串口"
                    };
                    laser_device::ProbeReport {
                        mode: spec.mode(),
                        target: target.clone(),
                        connected: false,
                        laser_mode_ok: false,
                        firmware_settings: Vec::new(),
                        machine_limits: None,
                        features: None,
                        summary: format!("{label} 连接失败: {error}"),
                    }
                }
                ProbeOpenOutcome::Opened(transport_arc, _short_io_guard) => {
                    // probe_opened_device 持有 Box<Arc<...>> 并在返回前释放，随后 _short_io_guard
                    // 在锁内释放最后一个 Arc（关闭串口/TCP fd），保证无并发双开窗口。
                    laser_device::probe_opened_device(
                        &spec,
                        Box::new(transport_arc) as Box<dyn laser_device::DeviceTransport>,
                        timeout,
                    )
                }
            };
            attempts.push(serde_json::json!({
                "mode": mode,
                "target": report.target,
                "connected": report.connected,
                "laser_mode_ok": report.laser_mode_ok,
                "summary": report.summary,
            }));
            if index == 0 || summary.is_empty() {
                summary = report.summary.clone();
            }
            if report.connected && report.laser_mode_ok {
                verified = Some(spec.clone());
                if mode == "serial" {
                    serial_connected = true;
                } else {
                    network_connected = true;
                }
                selected = Some(mode);
                firmware_settings = report.firmware_settings.clone();
                machine_limits = report.machine_limits.clone();
                machine_features = report.features.clone();
                summary = report.summary.clone();
                break;
            }
        }
        let result = serde_json::json!({
            "preferred_mode": preferred,
            "selected_mode": selected,
            "fallback_used": selected.is_some_and(|m| m != preferred),
            "attempts": attempts,
            "serial_connected": serial_connected,
            "network_connected": network_connected,
            "connected": serial_connected || network_connected,
            "firmware_settings": firmware_settings,
            "machine_limits": machine_limits.map(|lim| serde_json::json!({
                "x_travel_mm": lim.x_travel_mm,
                "y_travel_mm": lim.y_travel_mm,
                "z_travel_mm": lim.z_travel_mm,
                "x_max_rate_mm_per_min": lim.x_max_rate_mm_per_min,
                "y_max_rate_mm_per_min": lim.y_max_rate_mm_per_min,
                "x_accel_mm_s2": lim.x_accel_mm_s2,
                "y_accel_mm_s2": lim.y_accel_mm_s2,
                "junction_deviation_mm": lim.junction_deviation_mm,
            })),
            "features": machine_features.map(|feat| serde_json::json!({
                "axes": feat.axes,
                "has_air_assist": feat.has_air_assist,
                "has_door": feat.has_door,
                "has_estop_or_fault_pin": feat.has_estop_or_fault_pin,
                "speed_map": feat.speed_map,
            })),
            "timeout_seconds": timeout_secs,
            "summary": summary,
        });
        (result, verified)
    }
}

/// 终态或发送中的 workflow 不允许再次下发（规格 §8：未确认/已终止任务绝不自动重发）。
#[cfg(windows)]
pub(crate) fn enumerate_system_serial_ports() -> Vec<String> {
    laser_device::available_serial_ports()
}

#[cfg(not(windows))]
pub(crate) fn enumerate_system_serial_ports() -> Vec<String> {
    let mut ports = laser_device::available_serial_ports();
    if let Ok(entries) = std::fs::read_dir("/dev") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("ttyUSB") || name.starts_with("ttyACM") || name.starts_with("cu.") {
                let full = format!("/dev/{name}");
                if !ports.contains(&full) {
                    ports.push(full);
                }
            }
        }
    }
    ports.sort();
    ports
}

/// 局域网候选对应的 Telnet 连接；键一律用 `describe()`（IPv6 带方括号），与任务登记表的目标同格式。
fn discovery_target(candidate: &ControllerCandidate) -> laser_device::ConnectionSpec {
    laser_device::ConnectionSpec::Tcp {
        host: candidate.ip.clone(),
        port: candidate.telnet_port,
    }
}

/// 扫描确认时要避让的占用表（与连接探测、发送、安全动作共用）。
pub(crate) struct DiscoveryBusyTables {
    pub(crate) open_locks: crate::jobs::TargetOpenLocks,
    pub(crate) active_jobs: ActiveJobs,
    pub(crate) uncertain_jobs: crate::jobs::UncertainJobs,
    pub(crate) short_io_targets: crate::jobs::ShortIoTargets,
}

/// 确认一个局域网候选是不是 GRBL / FluidNC。与连接探测同一锁序：持 `target_open_mutex` 复查
/// 活动任务 → 待确认任务 → 短连接占用，都空闲才开连接并登记 `ShortIoGuard`，随即释放打开锁再问询，
/// 杜绝和并发启动的发送或探测双开同一 Telnet 端口。设备被占用时返回 `Err(任务编号 / "probing")`。
pub(crate) fn probe_discovery_candidate(
    busy: &DiscoveryBusyTables,
    candidate: &ControllerCandidate,
    reply_timeout: std::time::Duration,
) -> Result<Result<laser_device::ControllerIdentity, String>, String> {
    let spec = discovery_target(candidate);
    let target = spec.describe();
    let (transport, short_io) = {
        let open_mutex = target_open_mutex(&busy.open_locks, &target);
        let _open_guard = open_mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(workflow_id) = busy.active_jobs.lock().ok().and_then(|jobs| {
            jobs.values()
                .find(|j| j.target == target)
                .map(|j| j.workflow_id.clone())
        }) {
            return Err(workflow_id);
        }
        if let Some(workflow_id) = busy.uncertain_jobs.lock().ok().and_then(|uncertain| {
            uncertain
                .values()
                .find(|j| j.target == target)
                .map(|j| j.workflow_id.clone())
        }) {
            return Err(workflow_id);
        }
        if busy
            .short_io_targets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(&target)
        {
            return Err("probing".to_owned());
        }
        let transport: std::sync::Arc<dyn laser_device::DeviceTransport> =
            match laser_device::open_transport(&spec, std::time::Duration::from_millis(800)) {
                Ok(transport) => std::sync::Arc::from(transport),
                Err(error) => return Ok(Err(error)),
            };
        let guard = register_short_io_target(&busy.short_io_targets, &target, transport.clone());
        (transport, guard)
    };
    let identity = laser_device::identify_grbl(transport.as_ref(), reply_timeout)
        .ok_or_else(|| "没有按 GRBL / FluidNC 回应".to_owned());
    // 先放掉本地句柄，再由守卫在占用表锁内释放最后一个引用，fd 关闭先于下一请求通过占位校验
    drop(transport);
    drop(short_io);
    Ok(identity)
}
