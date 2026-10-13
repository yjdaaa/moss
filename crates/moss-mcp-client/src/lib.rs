//! NV9 客户端与连接管理：stdio / Streamable HTTP、命名空间、权限和有界发现。
//! laser-host 接入桌面配置、持久审批、审计及语音聚合；客户端默认审批器永不放行 Ask。
//! 超时或断线后的工具调用结果可能未知，因此绝不自动重放 tools/call。
mod client;
mod config;
mod manager;
pub use client::*;
pub use config::*;
pub use manager::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientError {
    InvalidConfig,
    InvalidName,
    DuplicateName,
    InvalidSecretRef,
    InsecureCredentials,
    MissingSecret,
    Disabled,
    Unavailable,
    ConnectionFailed,
    DiscoveryFailed,
    CatalogTooLarge,
    InvalidArguments,
    ApprovalFailed,
    OutcomeUnknown,
    ResponseTooLarge,
    OutcomeNotRecorded,
}
impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidConfig => "第三方 MCP 配置无效",
            Self::InvalidName => "第三方 MCP 名称不合法或工具别名超过 64 字符",
            Self::DuplicateName => "第三方 MCP 名称或工具别名重复",
            Self::InvalidSecretRef => "第三方 MCP 密钥引用或字段不合法",
            Self::InsecureCredentials => "第三方 MCP 凭据只可通过 HTTPS 或本机回环传送",
            Self::MissingSecret => "第三方 MCP 凭据未配置",
            Self::Disabled => "第三方 MCP 服务或工具已停用",
            Self::Unavailable => "第三方 MCP 服务当前不可用",
            Self::ConnectionFailed => "第三方 MCP 连接或握手失败，请检查服务与凭据",
            Self::DiscoveryFailed => "第三方 MCP 工具发现失败",
            Self::CatalogTooLarge => "第三方 MCP 工具清单超过数量或大小限制",
            Self::InvalidArguments => "第三方 MCP 参数必须为有界 JSON 对象",
            Self::ApprovalFailed => "第三方 MCP 审批失败或授权与当前调用不匹配",
            Self::OutcomeUnknown => {
                "第三方 MCP 调用超时或断线，执行结果未知；为避免重复操作，未自动重试"
            }
            Self::OutcomeNotRecorded => {
                "第三方操作可能已执行，但本机结果或审计保存失败；请核实，不要重复操作"
            }
            Self::ResponseTooLarge => "第三方 MCP 已返回结果，但内容超过大小限制；未重试操作",
        })
    }
}
impl std::error::Error for ClientError {}
