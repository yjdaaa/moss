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
            | "shortcut_key_execution_tool"
            | "open_jsjds_website_tool"
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
            active_jobs: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            uncertain_jobs: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            launching_workflows: std::sync::Arc::new(std::sync::Mutex::new(
                LaunchReservationState::default(),
            )),
            target_open_locks: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            material_write_lock: std::sync::Mutex::new(()),
            feedback_adoption: std::sync::Mutex::new(()),
            transport_factory: default_transport_factory(),
            extra_allowed_origins: Vec::new(),
            serial_port_lister: std::sync::Arc::new(enumerate_system_serial_ports),
            controller_candidates: std::sync::Arc::new(find_controller_candidates),
            event_sink: Default::default(),
            discovery_reply_timeout: std::time::Duration::from_millis(1500),
            strict_mode: std::sync::atomic::AtomicBool::new(false),
            approvals: None,
            desktop_session: std::sync::OnceLock::new(),
            agent_runs: std::sync::OnceLock::new(),
            external_tools: std::sync::OnceLock::new(),
            disabled_tools: std::sync::RwLock::new(std::collections::BTreeSet::new()),
            full_tool_list: std::sync::atomic::AtomicBool::new(false),
            verified_connection: std::sync::RwLock::new(None),
            voice_sessions: Default::default(),
        }
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

    /// 运行中切换严格模式（语音中心切到自建链路时开启，切回官方时恢复 `.env` 的设置，技术书 §9.4.2）。
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

    pub fn with_runtime_path(mut self, runtime_path: std::path::PathBuf) -> Self {
        let runtime_settings = read_runtime_settings(Some(&runtime_path));
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
        self.runtime_path = Some(runtime_path);
        self
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
