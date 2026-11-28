//! MCP 服务层：JSON-RPC 处理、工具分发与小智 WebSocket 桥接。
//!
//! - `HostBackend`：把需要持久化/真实设备作业的工具转发给 moss-host 的 HTTP 处理器
//!   （生产由 `laser-host` 进程内接线，测试注入假后端）；
//! - 无后端可用时，涉及 workflow / 标定 / 材料库的工具明确返回失败，绝不伪装成功；
//! - `run_bridge`：以 WebSocket 客户端连接 `MCP_ENDPOINT`（小智/MOSS 控制台生成），
//!   逐帧处理 JSON-RPC，断线指数退避重连（1s → 60s）。每个接入点一条桥（NV8 多接入点），
//!   定时发心跳，长时间收不到数据即判定断线重连；
//! - 工具开关（NV8）：后端报告为关闭的工具不出现在 `tools/list`，调用时也拒绝（停止类工具不可关闭）；
//!   每次调用按结果计入统计（`McpTelemetrySnapshot::tool_stats`）。

use std::sync::Arc;

pub use laser_contracts::{McpTelemetry, McpTelemetrySnapshot, global_mcp_telemetry};

mod bridge;
mod dispatch;
mod rpc;
mod schema;
#[cfg(test)]
mod tests;

pub use bridge::{
    BridgeStats, MAX_EXTRA_ENDPOINTS, endpoint_from_env, endpoint_from_value, endpoint_identity,
    redact_endpoint, run_bridge,
};
pub use dispatch::{
    DESKTOP_PREVIEW_HINT, dispatch_tool_call, dispatch_tool_call_with, format_mcp_response,
};
pub use rpc::{handle_json_rpc, handle_json_rpc_with};
pub use schema::{tools_list_for, tools_list_json, tools_list_with};

pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const SERVER_NAME: &str = "YOKO_MCP_SERVER";

/// moss-host 后端调用面：`method` + `route` + JSON 请求体 → (HTTP 状态码, JSON 响应体)。
pub trait HostBackend: Send + Sync {
    fn request(
        &self,
        method: &str,
        route: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, serde_json::Value), String>;

    /// 严格模式（技术书 §6.2）。默认关闭；`laser-host` 的进程内后端返回 ApiService 的实际开关。
    fn strict_mode(&self) -> bool {
        false
    }

    /// 记一条审计（技术书 §7.3：R4 拒绝、严格模式拦截）。默认不记录；`laser-host` 写入本地数据库。
    /// 拒绝本身不依赖审计是否写入成功。
    fn audit(&self, _entry: &laser_contracts::AuditEntry) -> Result<(), String> {
        Ok(())
    }

    /// 审计记录里的调用方标签。接入点为 `mcp`；内置语音的后端返回 `voice:<设备>`（NV4）。
    fn audit_client(&self) -> String {
        "mcp".into()
    }

    /// 严格模式关闭时是否仍拒绝 R4 与未分级工具（命令执行、打开应用、快捷键等）。接入点为否（行为不变）；
    /// 内置语音为是：放宽的只是运动与出光的确认方式，桌面控制类工具始终不对内置语音开放。
    fn always_deny_r4(&self) -> bool {
        false
    }

    /// 用户是否在 MOSS 中关闭了这个工具（NV8）。关闭的工具不列出、不可调用；默认全部打开。
    fn tool_disabled(&self, _name: &str) -> bool {
        false
    }

    /// n73 工作台最近一次握手成功的连接字段（`port`/`baudrate` 或 `host`/`network_host`/`telnet_port`…）。
    /// 工具调用没给任何连接参数时补进去；默认没有（行为不变，仍按 `.env` 兜底）。
    fn default_connection(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        None
    }

    /// MCP 遥测实例（默认复用进程级全局单例；宿主或测试可注入隔离实例避免并行测试串味）。
    fn mcp_telemetry(&self) -> &McpTelemetry {
        global_mcp_telemetry()
    }
}

pub(crate) fn telemetry_for(backend: Option<&dyn HostBackend>) -> &McpTelemetry {
    match backend {
        Some(backend) => backend.mcp_telemetry(),
        None => global_mcp_telemetry(),
    }
}

pub type SharedBackend = Arc<dyn HostBackend>;

pub use laser_contracts::{ALWAYS_ENABLED_TOOLS, tool_can_be_disabled};
