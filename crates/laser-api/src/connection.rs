use super::*;

const SAVED_MOTION_KEYS: &[&str] = &[
    "x_max_rate_mm_per_min",
    "y_max_rate_mm_per_min",
    "x_accel_mm_s2",
    "y_accel_mm_s2",
    "junction_deviation_mm",
];

/// n73 握手成功的连接 → 记录：`fields` 同时带两套别名（串口 `port`/`baudrate`，网络 `host`/`network_host`、
/// `telnet_port`/`network_telnet_port`），各工具按自己认的字段名取用；
/// r66 同时持久化当次握手的固件 X/Y 行程与运动参数（`machine_limits`），供省略 `work_area_*` / `machine_motion` 的 AI/语音/MCP 生成路由取用。
fn verified_connection_value(
    spec: &laser_device::ConnectionSpec,
    at: u64,
    machine_limits: Option<&serde_json::Value>,
) -> serde_json::Value {
    let saved_limits = machine_limits
        .and_then(|lim| {
            let pos = |key: &str| {
                lim.get(key)
                    .and_then(serde_json::Value::as_f64)
                    .filter(|v| v.is_finite() && *v > 0.0)
            };
            let x = pos("x_travel_mm");
            let y = pos("y_travel_mm");
            let mut map = serde_json::Map::new();
            if x.is_some() || y.is_some() {
                map.insert(
                    "x_travel_mm".into(),
                    x.map_or(serde_json::Value::Null, |v| serde_json::json!(v)),
                );
                map.insert(
                    "y_travel_mm".into(),
                    y.map_or(serde_json::Value::Null, |v| serde_json::json!(v)),
                );
            }
            for key in SAVED_MOTION_KEYS {
                if let Some(val) = pos(key) {
                    map.insert((*key).into(), serde_json::json!(val));
                }
            }
            (!map.is_empty()).then_some(serde_json::Value::Object(map))
        })
        .unwrap_or(serde_json::Value::Null);
    match spec {
        laser_device::ConnectionSpec::Serial { port, baud_rate } => serde_json::json!({
            "mode": "serial",
            "label": format!("串口 {port}（{baud_rate}）"),
            "verified_at": at,
            "fields": {"connection_mode": "serial", "port": port, "baudrate": baud_rate},
            "machine_limits": saved_limits,
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
            "machine_limits": saved_limits,
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
        self.remember_verified_connection_with_limits(spec, None);
    }

    /// r66 记住握手成功的连接及其固件 X/Y 行程（未报行程时清空旧行程记录，避免换机残留）。
    pub fn remember_verified_connection_with_limits(
        &self,
        spec: &laser_device::ConnectionSpec,
        machine_limits: Option<&serde_json::Value>,
    ) {
        let value = verified_connection_value(spec, unix_seconds(), machine_limits);
        if let Ok(mut slot) = self.verified_connection.write() {
            *slot = Some(value.clone());
        }
        let _ = update_runtime_settings(self.runtime_path.as_deref(), |current| {
            current.insert("laser_verified_connection".into(), value);
        });
    }

    /// 判断请求中显式携带的连接目标是否与 `verified_connection` 一致；请求未显式带连接目标时视为匹配。
    fn request_matches_verified_connection(
        &self,
        request: &serde_json::Map<String, serde_json::Value>,
        record: &serde_json::Value,
    ) -> bool {
        let has = |keys: &[&str]| {
            keys.iter().any(|key| {
                request
                    .get(*key)
                    .and_then(|v| v.as_str())
                    .is_some_and(|v| !v.trim().is_empty())
            })
        };
        let mode = request
            .get("connection_mode")
            .or_else(|| request.get("preferred_mode"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let explicit = match mode {
            Some(m) if m.eq_ignore_ascii_case("serial") => has(&["port", "serial_port"]),
            Some(_) => has(&["network_host", "host"]),
            None => has(&["port", "serial_port", "network_host", "host"]),
        };
        if !explicit {
            return mode.is_none();
        }
        let Ok(req_spec) = self.connection_spec_from_request(request) else {
            return false;
        };
        let Some(fields) = record.get("fields").and_then(|f| f.as_object()) else {
            return false;
        };
        let Ok(ver_spec) = self.connection_spec_from_request(fields) else {
            return false;
        };
        req_spec.describe() == ver_spec.describe()
    }

    /// r78 当前记住连接的规范目标键（与前端 `connectionTargetKey` 对齐）。
    pub(crate) fn verified_connection_target_key(&self) -> Option<String> {
        let record = self.verified_connection()?;
        let fields = record.get("fields")?.as_object()?;
        let spec = self.connection_spec_from_request(fields).ok()?;
        Some(crate::machine_profiles::connection_target_key(&spec))
    }

    /// r78 解析本次请求对应的连接目标键：
    /// 1. 请求显式给了串口 / 网络地址 → 按该目标；
    /// 2. 请求未显式给连接目标 → 优先用最近一次握手记住的目标（`laser_verified_connection`），
    ///    否则回退 `.env` 默认配置目标；均无时返回 `None`（绝不猜测其他机器档案）。
    pub(crate) fn resolve_target_key_for_request(
        &self,
        request: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<String> {
        let has = |keys: &[&str]| {
            keys.iter().any(|key| {
                request
                    .get(*key)
                    .and_then(|v| v.as_str())
                    .is_some_and(|v| !v.trim().is_empty())
            })
        };
        let mode = request
            .get("connection_mode")
            .or_else(|| request.get("preferred_mode"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let explicit = match mode {
            Some(m) if m.eq_ignore_ascii_case("serial") => has(&["port", "serial_port"]),
            Some(_) => has(&["network_host", "host"]),
            None => has(&["port", "serial_port", "network_host", "host"]),
        };
        if explicit {
            return self
                .connection_spec_from_request(request)
                .ok()
                .map(|spec| crate::machine_profiles::connection_target_key(&spec));
        }
        if mode.is_none()
            && let Some(key) = self.verified_connection_target_key()
        {
            return Some(key);
        }
        self.connection_spec_from_request(request)
            .ok()
            .map(|spec| crate::machine_profiles::connection_target_key(&spec))
    }

    /// 仅读取最近一次工作台握手记下的**固件** X/Y 行程（不含手填；供能力摘要忙态回退区分来源）。
    pub(crate) fn remembered_firmware_work_area(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> (Option<f64>, Option<f64>) {
        let Some(record) = self
            .verified_connection()
            .filter(|rec| self.request_matches_verified_connection(object, rec))
        else {
            return (None, None);
        };
        let lim = &record["machine_limits"];
        (
            lim.get("x_travel_mm").and_then(serde_json::Value::as_f64),
            lim.get("y_travel_mm").and_then(serde_json::Value::as_f64),
        )
    }

    /// r66 / r78 读取当前目标的默认 X/Y 行程：逐轴按“手填（后端档案） → 固件（最近握手）”取值。
    pub(crate) fn remembered_work_area(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> (Option<f64>, Option<f64>) {
        let profile = self
            .resolve_target_key_for_request(object)
            .and_then(|key| self.machine_profile_for_key(&key));
        let (fw_x, fw_y) = self.remembered_firmware_work_area(object);
        let man_x = profile.as_ref().and_then(|p| p.manual_travel.x_travel_mm);
        let man_y = profile.as_ref().and_then(|p| p.manual_travel.y_travel_mm);
        (man_x.or(fw_x), man_y.or(fw_y))
    }

    /// r66 / r78 生成请求缺省补齐：按“请求显式值 → 手填（后端机器档案） → 固件（最近握手） → 默认”
    /// 逐项补入 `work_area_*`、`machine_motion`（含加速度来源标记）与 `fill_options` 双向扫描偏移。
    pub(crate) fn with_remembered_work_area<'a>(
        &self,
        object: &'a serde_json::Map<String, serde_json::Value>,
    ) -> std::borrow::Cow<'a, serde_json::Map<String, serde_json::Value>> {
        let profile = self
            .resolve_target_key_for_request(object)
            .and_then(|key| self.machine_profile_for_key(&key));
        let verified_limits = self
            .verified_connection()
            .filter(|rec| self.request_matches_verified_connection(object, rec))
            .map(|rec| rec["machine_limits"].clone());
        if profile.is_none() && verified_limits.is_none() {
            return std::borrow::Cow::Borrowed(object);
        }
        let missing = |key: &str| matches!(object.get(key), None | Some(serde_json::Value::Null));

        // 1. 行程：请求显式 > 后端手填档案 > 固件握手
        let fw_w = verified_limits
            .as_ref()
            .and_then(|lim| lim.get("x_travel_mm"))
            .and_then(serde_json::Value::as_f64);
        let fw_h = verified_limits
            .as_ref()
            .and_then(|lim| lim.get("y_travel_mm"))
            .and_then(serde_json::Value::as_f64);
        let man_w = profile.as_ref().and_then(|p| p.manual_travel.x_travel_mm);
        let man_h = profile.as_ref().and_then(|p| p.manual_travel.y_travel_mm);
        let fill_w = missing("work_area_width_mm")
            .then_some(man_w.or(fw_w))
            .flatten();
        let fill_h = missing("work_area_height_mm")
            .then_some(man_h.or(fw_h))
            .flatten();

        // 2. 运动参数（工时估算 / 自动过扫描 / 剩余时间）：请求显式 > 后端手填运动 > 固件握手运动
        let mut merged_motion: Option<serde_json::Map<String, serde_json::Value>> = None;
        let mut accel_origin: Option<&'static str> = None;
        let motion_allows_fill = matches!(
            object.get("machine_motion"),
            None | Some(serde_json::Value::Null | serde_json::Value::Object(_))
        );
        if motion_allows_fill {
            let explicit_motion = object
                .get("machine_motion")
                .and_then(serde_json::Value::as_object);
            let explicit_has = |key: &str| {
                explicit_motion
                    .and_then(|m| m.get(key))
                    .is_some_and(|v| !v.is_null())
            };
            let mut next_motion = explicit_motion.cloned().unwrap_or_default();
            let mut motion_changed = false;

            let fw_pos = |key: &str| {
                verified_limits
                    .as_ref()
                    .and_then(|lim| lim.get(key))
                    .and_then(serde_json::Value::as_f64)
                    .filter(|v| v.is_finite() && *v > 0.0)
            };
            let manual_motion = profile.as_ref().map(|p| &p.manual_motion);

            // 加速度（X/Y 成组优先级：若请求未显式传任一轴，再按手填 → 固件补齐）
            if explicit_has("x_accel_mm_s2") || explicit_has("y_accel_mm_s2") {
                accel_origin = Some("request");
            } else {
                let man_ax = manual_motion.and_then(|m| m.x_accel_mm_s2);
                let man_ay = manual_motion.and_then(|m| m.y_accel_mm_s2);
                if man_ax.is_some() || man_ay.is_some() {
                    if let Some(v) = man_ax {
                        next_motion.insert("x_accel_mm_s2".into(), serde_json::json!(v));
                    }
                    if let Some(v) = man_ay {
                        next_motion.insert("y_accel_mm_s2".into(), serde_json::json!(v));
                    }
                    accel_origin = Some("manual");
                    motion_changed = true;
                } else {
                    let fw_ax = fw_pos("x_accel_mm_s2");
                    let fw_ay = fw_pos("y_accel_mm_s2");
                    if fw_ax.is_some() || fw_ay.is_some() {
                        if let Some(v) = fw_ax {
                            next_motion.insert("x_accel_mm_s2".into(), serde_json::json!(v));
                        }
                        if let Some(v) = fw_ay {
                            next_motion.insert("y_accel_mm_s2".into(), serde_json::json!(v));
                        }
                        accel_origin = Some("firmware");
                        motion_changed = true;
                    }
                }
            }

            let other_keys: [(&str, Option<f64>); 3] = [
                (
                    "x_max_rate_mm_per_min",
                    manual_motion.and_then(|m| m.x_max_rate_mm_per_min),
                ),
                (
                    "y_max_rate_mm_per_min",
                    manual_motion.and_then(|m| m.y_max_rate_mm_per_min),
                ),
                (
                    "junction_deviation_mm",
                    manual_motion.and_then(|m| m.junction_deviation_mm),
                ),
            ];
            for (key, man_val) in other_keys {
                if !explicit_has(key)
                    && let Some(val) = man_val.or_else(|| fw_pos(key))
                {
                    next_motion.insert(key.into(), serde_json::json!(val));
                    motion_changed = true;
                }
            }
            if motion_changed {
                merged_motion = Some(next_motion);
            }
        }

        // 3. 双向扫描偏移校准：请求未显式传 scan_offset_mm / scan_offset_table 且未显式关闭双向时，补入后端档案
        let mut merged_fill: Option<serde_json::Map<String, serde_json::Value>> = None;
        if let Some(ref prof) = profile
            && (prof.scan_offset_mm.is_some() || !prof.scan_offset_table.is_empty())
            && matches!(
                object.get("fill_options"),
                None | Some(serde_json::Value::Null | serde_json::Value::Object(_))
            )
        {
            let explicit_fill = object
                .get("fill_options")
                .and_then(serde_json::Value::as_object);
            let explicit_bidirectional_off = explicit_fill
                .and_then(|m| m.get("bidirectional"))
                .and_then(serde_json::Value::as_bool)
                == Some(false);
            let explicit_offset_given = explicit_fill.is_some_and(|m| {
                m.get("scan_offset_mm").is_some_and(|v| !v.is_null())
                    || m.get("scan_offset_table").is_some_and(|v| !v.is_null())
            });
            if !explicit_bidirectional_off && !explicit_offset_given {
                let mut next_fill = explicit_fill.cloned().unwrap_or_default();
                if !prof.scan_offset_table.is_empty() {
                    next_fill.insert(
                        "scan_offset_table".into(),
                        serde_json::json!(prof.scan_offset_table),
                    );
                } else if let Some(offset) = prof.scan_offset_mm {
                    next_fill.insert("scan_offset_mm".into(), serde_json::json!(offset));
                }
                next_fill.insert(
                    "_scan_offset_origin".into(),
                    serde_json::Value::String("machine_profile".into()),
                );
                merged_fill = Some(next_fill);
            }
        }

        if fill_w.is_none()
            && fill_h.is_none()
            && merged_motion.is_none()
            && merged_fill.is_none()
            && accel_origin.is_none()
        {
            return std::borrow::Cow::Borrowed(object);
        }
        let mut cloned = object.clone();
        if let Some(w) = fill_w {
            cloned.insert("work_area_width_mm".into(), serde_json::json!(w));
        }
        if let Some(h) = fill_h {
            cloned.insert("work_area_height_mm".into(), serde_json::json!(h));
        }
        if let Some(motion) = merged_motion {
            cloned.insert("machine_motion".into(), serde_json::Value::Object(motion));
        }
        if let Some(origin) = accel_origin {
            cloned.insert(
                "_acceleration_origin".into(),
                serde_json::Value::String(origin.into()),
            );
        }
        if let Some(fill) = merged_fill {
            cloned.insert("fill_options".into(), serde_json::Value::Object(fill));
        }
        std::borrow::Cow::Owned(cloned)
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
        let mut device_identity = None;
        let mut probed_live = false;
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
                        identity: None,
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
                device_identity = report.identity.clone();
                probed_live = true;
                summary = report.summary.clone();
                break;
            }
        }
        let connected = serial_connected || network_connected;
        let soft_limits_enabled = machine_limits
            .as_ref()
            .and_then(|lim| lim.soft_limits_enabled);
        let homing_enabled = machine_limits.as_ref().and_then(|lim| lim.homing_enabled);
        let capabilities = connected.then(|| {
            let explicit_x = crate::generation::parse_optional_work_area_override(
                object.get("work_area_width_mm"),
                "work_area_width_mm",
            )
            .ok()
            .flatten();
            let explicit_y = crate::generation::parse_optional_work_area_override(
                object.get("work_area_height_mm"),
                "work_area_height_mm",
            )
            .ok()
            .flatten();
            let profile_target = verified
                .as_ref()
                .map(crate::machine_profiles::connection_target_key)
                .or_else(|| self.resolve_target_key_for_request(object));
            let profile = profile_target
                .as_deref()
                .and_then(|key| self.machine_profile_for_key(key));
            let manual_x =
                explicit_x.or_else(|| profile.as_ref().and_then(|p| p.manual_travel.x_travel_mm));
            let manual_y =
                explicit_y.or_else(|| profile.as_ref().and_then(|p| p.manual_travel.y_travel_mm));
            let remembered_fw = if probed_live {
                (None, None)
            } else {
                self.remembered_firmware_work_area(object)
            };
            laser_device::build_machine_capabilities(
                manual_x,
                manual_y,
                machine_limits.as_ref(),
                remembered_fw,
                &firmware_settings,
                machine_features.as_ref(),
                device_identity.as_ref(),
            )
        });
        let capabilities_json = capabilities.as_ref().map(machine_capabilities_json);
        let capabilities_summary = capabilities
            .as_ref()
            .map(laser_device::MachineCapabilities::summary_zh);
        let homing_json = |h: &Option<laser_device::AxisHoming>| {
            h.as_ref().map(|cfg| {
                serde_json::json!({
                    "cycle": cfg.cycle,
                    "mpos_mm": cfg.mpos_mm,
                    "positive_direction": cfg.positive_direction,
                })
            })
        };
        let result = serde_json::json!({
            "preferred_mode": preferred,
            "selected_mode": selected,
            "fallback_used": selected.is_some_and(|m| m != preferred),
            "attempts": attempts,
            "serial_connected": serial_connected,
            "network_connected": network_connected,
            "connected": connected,
            "firmware_settings": firmware_settings,
            "soft_limits_enabled": soft_limits_enabled,
            "homing_enabled": homing_enabled,
            "capabilities": capabilities_json,
            "capabilities_summary": capabilities_summary,
            "identity": device_identity.map(|id| serde_json::json!({
                "firmware": id.firmware,
                "version": id.version,
                "machine_name": id.machine_name,
            })),
            "machine_limits": machine_limits.map(|lim| serde_json::json!({
                "x_travel_mm": lim.x_travel_mm,
                "y_travel_mm": lim.y_travel_mm,
                "z_travel_mm": lim.z_travel_mm,
                "x_max_rate_mm_per_min": lim.x_max_rate_mm_per_min,
                "y_max_rate_mm_per_min": lim.y_max_rate_mm_per_min,
                "x_accel_mm_s2": lim.x_accel_mm_s2,
                "y_accel_mm_s2": lim.y_accel_mm_s2,
                "junction_deviation_mm": lim.junction_deviation_mm,
                "soft_limits_enabled": lim.soft_limits_enabled,
                "homing_enabled": lim.homing_enabled,
                "homing_dir_mask": lim.homing_dir_mask,
            })),
            "features": machine_features.map(|feat| serde_json::json!({
                "axes": feat.axes,
                "has_air_assist": feat.has_air_assist,
                "has_door": feat.has_door,
                "has_estop_or_fault_pin": feat.has_estop_or_fault_pin,
                "speed_map": feat.speed_map,
                "x_soft_limits": feat.x_soft_limits,
                "y_soft_limits": feat.y_soft_limits,
                "x_homing": homing_json(&feat.x_homing),
                "y_homing": homing_json(&feat.y_homing),
                "z_homing": homing_json(&feat.z_homing),
            })),
            "timeout_seconds": timeout_secs,
            "summary": summary,
        });
        (result, verified)
    }
}

/// 将 `MachineCapabilities` 转为结构化 JSON（供 `/api/connection/check` 返回）。
fn machine_capabilities_json(caps: &laser_device::MachineCapabilities) -> serde_json::Value {
    serde_json::json!({
        "effective_travel_mm": {
            "x": caps.x_travel.mm,
            "x_source": caps.x_travel.source.as_str(),
            "x_source_label": caps.x_travel.source.label_zh(),
            "y": caps.y_travel.mm,
            "y_source": caps.y_travel.source.as_str(),
            "y_source_label": caps.y_travel.source.label_zh(),
        },
        "firmware_travel_mm": {
            "x": caps.firmware_x_travel_mm,
            "y": caps.firmware_y_travel_mm,
        },
        "homing_enabled": caps.homing_enabled,
        "homing_label": caps.homing_label(),
        "soft_limits_enabled": caps.soft_limits_enabled,
        "soft_limits_label": caps.soft_limits_label(),
        "spindle_max_s": caps.spindle_max_s,
        "spindle_max_s_label": caps.spindle_max_s_label(),
        "laser_mode_enabled": caps.laser_mode_enabled,
        "laser_mode_label": caps.laser_mode_label(),
        "firmware_type": caps.firmware_type_label(),
        "summary": caps.summary_zh(),
    })
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
