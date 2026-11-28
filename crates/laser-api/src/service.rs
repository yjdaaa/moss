use super::*;

/// n72 精简工具清单里的工具（与 `laser_mcp::COMPACT_LASER_TOOLS` 加电脑控制类工具一致；
/// laser-api 不依赖 laser-mcp，`laser-host` 的测试核对两边一致）。
pub fn laser_mcp_compact_tool(name: &str) -> bool {
    matches!(
        name,
        "check_laser_connection_tool"
            | "laser_safe_action_tool"
            | "laser_workflow_tool"
            | "material_params_tool"
            | "refine_laser_params_from_feedback_tool"
            | "regenerate_text_laser_task_tool"
            | "run_calibration_grid_tool"
            | "select_calibration_cell_tool"
            | "command_execution_tool"
            | "open_app_tool"
    )
}

impl ApiService {
    pub fn new(
        settings: laser_runtime::LaserSettings,
        materials: laser_contracts::MaterialLibrary,
    ) -> Self {
        Self {
            settings,
            materials,
            upload_root: None,
            preview_roots: Vec::new(),
            auth: None,
            workflows: std::sync::Arc::new(
                std::sync::Mutex::new(std::collections::BTreeMap::new()),
            ),
            calibrations: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::BTreeMap::new(),
            )),
            store: None,
            runtime_db: None,
            read_db: None,
            generated_root: None,
            actual_port: std::sync::Arc::new(std::sync::atomic::AtomicU16::new(0)),
            runtime_path: None,
            process_trace: None,
            active_jobs: Default::default(),
            uncertain_jobs: Default::default(),
            launching_workflows: std::sync::Arc::new(std::sync::Mutex::new(
                LaunchReservationState::default(),
            )),
            target_open_locks: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            short_io_targets: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            material_write_lock: std::sync::Mutex::new(()),
            feedback_adoption: std::sync::Mutex::new(()),
            draw_state_lock: std::sync::Mutex::new(()),
            draw_state_memory: std::sync::Mutex::new(Default::default()),
            transport_factory: default_transport_factory(),
            extra_allowed_origins: Vec::new(),
            serial_port_lister: std::sync::Arc::new(enumerate_system_serial_ports),
            controller_candidates: std::sync::Arc::new(find_controller_candidates),
            event_sink: Default::default(),
            discovery_reply_timeout: std::time::Duration::from_millis(1500),
            discovery_in_flight: std::sync::atomic::AtomicBool::new(false),
            strict_mode: std::sync::atomic::AtomicBool::new(false),
            approvals: None,
            desktop_session: std::sync::OnceLock::new(),
            agent_runs: std::sync::OnceLock::new(),
            external_tools: std::sync::OnceLock::new(),
            disabled_tools: std::sync::RwLock::new(std::collections::BTreeSet::new()),
            full_tool_list: std::sync::atomic::AtomicBool::new(false),
            verified_connection: std::sync::RwLock::new(None),
            voice_sessions: Default::default(),
            shutdown_requested: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            mcp_telemetry: laser_contracts::global_mcp_telemetry_arc(),
            voice_telemetry: laser_contracts::global_voice_telemetry_arc(),
            dry_run: std::sync::Arc::new(laser_device::DryRunState::default()),
        }
    }

    /// LB2 当前真机空跑模式快照（自动结算 60 分钟到期）。
    pub fn dry_run_snapshot(&self) -> laser_device::DryRunSnapshot {
        self.dry_run.snapshot_now()
    }

    pub fn is_dry_run_active(&self) -> bool {
        self.dry_run.active_policy_now().is_some()
    }

    /// 注入隔离的 MCP 遥测实例（默认使用进程级全局单例）。
    pub fn with_mcp_telemetry(
        mut self,
        telemetry: std::sync::Arc<laser_contracts::McpTelemetry>,
    ) -> Self {
        self.mcp_telemetry = telemetry;
        self
    }

    /// 注入隔离的语音会话遥测实例（默认使用进程级全局单例）。
    pub fn with_voice_telemetry(
        mut self,
        telemetry: std::sync::Arc<laser_contracts::VoiceTelemetry>,
    ) -> Self {
        self.voice_telemetry = telemetry;
        self
    }

    pub fn mcp_telemetry(&self) -> &std::sync::Arc<laser_contracts::McpTelemetry> {
        &self.mcp_telemetry
    }

    pub fn voice_telemetry(&self) -> &std::sync::Arc<laser_contracts::VoiceTelemetry> {
        &self.voice_telemetry
    }

    /// 严格模式开关（来自 `MOSS_FUSION_STRICT`，见 [`laser_runtime::config::fusion_strict_from_env`]）。
    pub fn with_strict_mode(self, strict: bool) -> Self {
        self.strict_mode
            .store(strict, std::sync::atomic::Ordering::SeqCst);
        self
    }

    pub fn strict_mode(&self) -> bool {
        self.strict_mode.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 运行中切换严格模式（语音中心的严格模式开关，写 `.env` 后即时生效，n27）。
    pub fn set_strict_mode(&self, strict: bool) {
        self.strict_mode
            .store(strict, std::sync::atomic::Ordering::SeqCst);
    }

    /// 用户是否关闭了工具 `name`（NV8）。
    pub fn tool_disabled(&self, name: &str) -> bool {
        laser_contracts::tool_can_be_disabled(name)
            && self
                .disabled_tools
                .read()
                .map(|set| set.contains(name))
                .unwrap_or(false)
    }

    /// 当前关闭的工具（按名称排序）。
    pub fn disabled_tools(&self) -> Vec<String> {
        self.disabled_tools
            .read()
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// n72 是否给接入点与内置语音完整工具清单（默认否：精简清单）。
    pub fn full_tool_list(&self) -> bool {
        self.full_tool_list
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// n72 运行中切换完整 / 精简工具清单（语音中心写入 `settings.json` 后调用）。
    pub fn set_full_tool_list(&self, full: bool) {
        self.full_tool_list
            .store(full, std::sync::atomic::Ordering::Release);
    }

    /// 运行中替换关闭的工具列表（语音中心写入 `settings.json` 后调用）。不能关闭的工具与空名会被忽略。
    pub fn set_disabled_tools<I: IntoIterator<Item = String>>(&self, names: I) {
        let names = names
            .into_iter()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty() && laser_contracts::tool_can_be_disabled(name))
            .collect();
        if let Ok(mut set) = self.disabled_tools.write() {
            *set = names;
        }
    }

    /// 替换审批服务（测试注入固定时钟用）；生产环境由 [`Self::with_runtime_db`] 自动创建。
    pub fn with_approval_service(
        mut self,
        service: std::sync::Arc<approval::ApprovalService>,
    ) -> Self {
        self.approvals = Some(service);
        self
    }

    pub fn approvals(&self) -> Option<&std::sync::Arc<approval::ApprovalService>> {
        self.approvals.as_ref()
    }

    /// 进程启动时调用：所有 pending / approved 审批一律过期（技术书 §5.5、A-32）。
    pub fn recover_approvals_after_restart(&self) -> usize {
        match self
            .approvals
            .as_ref()
            .map(|service| service.recover_after_restart())
        {
            Some(Ok(count)) => count,
            Some(Err(error)) => {
                eprintln!("[laser-api] 启动时处理遗留审批失败: {}", error.message());
                0
            }
            None => 0,
        }
    }

    /// 替换局域网扫描的候选地址来源（测试用本机假设备，不扫真实网络）。
    pub fn with_controller_candidates(mut self, finder: ControllerCandidateFinder) -> Self {
        self.controller_candidates = finder;
        self
    }

    /// 调整局域网扫描等每台设备回应的时限（测试在满负载下放宽；收齐回应即返回，不会因此变慢）。
    pub fn with_discovery_reply_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.discovery_reply_timeout = timeout;
        self
    }

    pub fn with_serial_port_lister(mut self, lister: SerialPortLister) -> Self {
        self.serial_port_lister = lister;
        self
    }

    /// 额外允许的浏览器来源（来自 `MOSS_ALLOWED_ORIGINS`，见 [`laser_runtime::config::parse_allowed_origins`]）。
    pub fn with_allowed_origins(mut self, origins: Vec<String>) -> Self {
        self.extra_allowed_origins = origins;
        self
    }

    pub fn with_transport_factory(mut self, factory: TransportFactory) -> Self {
        self.transport_factory = factory;
        self
    }

    /// 绑定宿主退出的原子关停标记。置位后拒收新作业，急停和软复位仍可建连。
    pub fn with_shutdown_flag(
        mut self,
        flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.shutdown_requested = flag;
        self
    }

    pub fn request_shutdown(&self) {
        self.shutdown_requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// LC：构造空跑传输层兜底拦截告警处理器（锁存当前请求来源标签与过程记录器，跨后台发送线程生效）。
    pub(crate) fn make_dry_run_block_handler(&self) -> laser_device::DryRunBlockHandler {
        let trace = self.process_trace().cloned();
        let bench_source = crate::process_trace::current_bench_source();
        let event_sink = self.event_sink.clone();
        std::sync::Arc::new(move |hit: &laser_device::DryRunBlockedCommand| {
            let source_label = bench_source.map(|s| s.source_key()).unwrap_or("local");
            eprintln!(
                "[laser-api][dry-run][source={source_label}] 传输层兜底拦截 {}（已替换为 {}）：{}",
                hit.code,
                hit.blocked_comment(),
                hit.original_line
            );
            crate::process_trace::trace_dry_run_blocked_with_meta(
                trace.as_ref(),
                hit,
                bench_source,
            );
            emit_laser_event(
                &event_sink,
                LaserEvent {
                    level: LaserEventLevel::Warn,
                    text: format!(
                        "空跑传输层兜底拦截高风险指令 {}（已替换为无运动注释）",
                        hit.code
                    ),
                    hint: None,
                    detail: Some(format!(
                        "error_code={} code={} source={source_label} replacement={}",
                        hit.error_code(),
                        hit.code,
                        hit.blocked_comment()
                    )),
                },
            );
        })
    }

    pub(crate) fn open_device_transport(
        &self,
        spec: &laser_device::ConnectionSpec,
    ) -> Result<Box<dyn laser_device::DeviceTransport>, String> {
        if self.is_shutdown_requested() {
            return Err("服务正在关闭，已拒绝建立新的设备连接".to_owned());
        }
        let transport = (self.transport_factory)(spec)?;
        if self.is_shutdown_requested() {
            // 新连接尚未写过指令，直接释放，避免打断控制器自己的 SD 任务。
            return Err("建立设备连接期间服务进入关闭流程，已拒绝继续使用新连接".to_owned());
        }
        Ok(Box::new(
            laser_device::DryRunTransport::wrap(transport, self.dry_run.clone())
                .with_block_handler(self.make_dry_run_block_handler()),
        ))
    }

    pub fn with_runtime_path(mut self, runtime_path: std::path::PathBuf) -> Self {
        let runtime_settings = read_runtime_settings(Some(&runtime_path));
        // LB2：空跑开关本身每次启动固定默认关闭；仅恢复用户上次保存的速度上限（校验 300..=5000）
        if let Some(saved_feed) = runtime_settings
            .get("dry_run_max_feed_mm_min")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
        {
            let _ = self.dry_run.set_max_feed(saved_feed);
        }
        if let Some(saved) = runtime_settings
            .get("laser_verified_connection")
            .filter(|value| valid_verified_connection(value))
            && let Ok(mut slot) = self.verified_connection.write()
        {
            *slot = Some(saved.clone());
        }
        self.set_full_tool_list(
            runtime_settings
                .get("voice")
                .and_then(|voice| voice.get("mcp_full_tool_list"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        );
        let disabled = runtime_settings
            .get("mcp_disabled_tools")
            .and_then(serde_json::Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.set_disabled_tools(disabled);
        self.process_trace = Some(laser_runtime::ProcessTrace::new(runtime_path.clone()));
        self.runtime_path = Some(runtime_path);
        self
    }

    /// 完整过程记录器（有运行目录时才有；开关关着时写入调用什么也不做）。
    pub fn process_trace(&self) -> Option<&laser_runtime::ProcessTrace> {
        self.process_trace.as_ref()
    }

    pub fn set_actual_port(&self, port: u16) {
        self.actual_port
            .store(port, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn get_actual_port(&self) -> u16 {
        let port = self.actual_port.load(std::sync::atomic::Ordering::SeqCst);
        if port == 0 {
            self.settings.web_port
        } else {
            port
        }
    }

    pub fn with_upload_root(mut self, upload_root: std::path::PathBuf) -> Self {
        self.upload_root = Some(upload_root);
        self
    }

    pub fn with_preview_roots(mut self, preview_roots: Vec<std::path::PathBuf>) -> Self {
        self.preview_roots = preview_roots;
        self
    }

    pub fn preview_roots(&self) -> &[std::path::PathBuf] {
        &self.preview_roots
    }

    pub fn with_auth(mut self, auth: std::sync::Arc<AuthManager>) -> Self {
        self.auth = Some(auth);
        self
    }

    pub fn with_store(
        mut self,
        store: std::sync::Arc<dyn laser_runtime::RuntimeStore + Send + Sync>,
    ) -> Self {
        self.store = Some(store);
        self
    }

    pub fn with_runtime_db(
        mut self,
        runtime_db: std::sync::Arc<std::sync::Mutex<laser_runtime::RuntimeDb>>,
    ) -> Self {
        let read_db = runtime_db
            .lock()
            .ok()
            .and_then(|guard| guard.open_readonly().ok().flatten())
            .map(|ro| std::sync::Arc::new(std::sync::Mutex::new(ro)));
        self.runtime_db = Some(runtime_db.clone());
        self.read_db = read_db.clone();
        if self.approvals.is_none() {
            self.approvals = Some(std::sync::Arc::new(
                approval::ApprovalService::new(runtime_db.clone()).with_read_db(read_db.clone()),
            ));
        }
        let split_store =
            std::sync::Arc::new(laser_runtime::SplitSqliteStore::new(runtime_db, read_db));
        self.with_store(split_store)
    }

    pub fn with_generated_root(mut self, generated_root: std::path::PathBuf) -> Self {
        self.generated_root = Some(generated_root);
        self
    }
}
