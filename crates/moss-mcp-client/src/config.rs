use crate::ClientError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub const MAX_TOOLS: usize = 64;
/// The desktop can inspect a larger bounded catalog and disable tools; only MAX_TOOLS are exposed.
pub const MAX_DISCOVERED_TOOLS: usize = 256;
pub const MAX_DESCRIPTION_CHARS: usize = 200;
pub const MAX_SERVERS: usize = 8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Disabled,
    #[default]
    Ask,
    /// 仅表示用户明确将此工具视为可信只读工具；不根据服务端 annotations 自动提升。
    Allow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransportConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        /// 仅存 app_secrets 引用，不存环境变量值。
        #[serde(default)]
        env_refs: BTreeMap<String, String>,
    },
    StreamableHttp {
        url: String,
        /// 仅存 app_secrets 引用，不存 Header 值。
        #[serde(default)]
        header_refs: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    pub transport: TransportConfig,
    /// 键是服务器原始工具名；缺省总是 Ask。
    #[serde(default)]
    pub policies: BTreeMap<String, Permission>,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub fn tool_alias(server: &str, tool: &str) -> Result<String, ClientError> {
    if !identifier(server)
        || server.eq_ignore_ascii_case("device")
        || server.contains("__")
        || server.len() > 32
        || !identifier(tool)
    {
        return Err(ClientError::InvalidName);
    }
    let alias = format!("{server}__{tool}");
    if alias.len() > 64 {
        return Err(ClientError::InvalidName);
    }
    Ok(alias)
}

/// 引用必须属于该服务器；第三方配置不能借用 MOSS 的设备 token / 模型密钥。
pub fn secret_ref(server: &str, kind: &str, key: &str) -> String {
    format!("voice.mcp.{server}.{kind}.{key}")
}

/// Versioned references make file/config and credential updates crash-safe: old configs keep old keys.
pub fn valid_secret_ref(server: &str, kind: &str, key: &str, reference: &str) -> bool {
    let base = secret_ref(server, kind, key);
    reference == base
        || reference
            .strip_prefix(&(base + ".v-"))
            .is_some_and(|suffix| {
                suffix.len() == 32
                    && suffix
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
}

impl ServerConfig {
    pub fn permission(&self, tool: &str) -> Permission {
        if !self.enabled {
            Permission::Disabled
        } else {
            self.policies.get(tool).copied().unwrap_or_default()
        }
    }

    pub fn validate(&self) -> Result<(), ClientError> {
        tool_alias(&self.name, "test")?;
        if self.policies.len() > 256 {
            return Err(ClientError::InvalidConfig);
        }
        for tool in self.policies.keys() {
            tool_alias(&self.name, tool)?;
        }
        match &self.transport {
            TransportConfig::Stdio {
                command,
                args,
                env_refs,
            } => {
                if command.trim().is_empty()
                    || command.len() > 1024
                    || command.contains('\0')
                    || args.len() > 64
                    || args
                        .iter()
                        .any(|arg| arg.len() > 8192 || arg.contains('\0'))
                    || env_refs.len() > 32
                {
                    return Err(ClientError::InvalidConfig);
                }
                for (key, reference) in env_refs {
                    if !identifier(key)
                        || key.contains('-')
                        || key.len() > 128
                        || !valid_secret_ref(&self.name, "env", key, reference)
                    {
                        return Err(ClientError::InvalidSecretRef);
                    }
                }
            }
            TransportConfig::StreamableHttp { url, header_refs } => {
                let parsed = url::Url::parse(url).map_err(|_| ClientError::InvalidConfig)?;
                if url.len() > 2048
                    || !matches!(parsed.scheme(), "http" | "https")
                    || parsed.host_str().is_none()
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.query().is_some()
                    || parsed.fragment().is_some()
                    || header_refs.len() > 32
                {
                    return Err(ClientError::InvalidConfig);
                }
                let loopback = parsed.host_str().is_some_and(|host| {
                    host == "localhost"
                        || host
                            .trim_matches(['[', ']'])
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback())
                });
                if !header_refs.is_empty() && parsed.scheme() != "https" && !loopback {
                    return Err(ClientError::InsecureCredentials);
                }
                for (key, reference) in header_refs {
                    if key.len() > 128
                        || key != &key.to_ascii_lowercase()
                        || reqwest::header::HeaderName::from_bytes(key.as_bytes()).is_err()
                        || matches!(
                            key.as_str(),
                            "host"
                                | "content-type"
                                | "content-length"
                                | "accept"
                                | "connection"
                                | "transfer-encoding"
                        )
                        || key.starts_with("mcp-")
                        || key.starts_with("sec-")
                        || !valid_secret_ref(&self.name, "header", key, reference)
                    {
                        return Err(ClientError::InvalidSecretRef);
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn validate_servers(servers: &[ServerConfig]) -> Result<(), ClientError> {
    if servers.len() > MAX_SERVERS {
        return Err(ClientError::InvalidConfig);
    }
    let mut names = HashSet::new();
    for server in servers {
        server.validate()?;
        if !names.insert(server.name.to_ascii_lowercase()) {
            return Err(ClientError::DuplicateName);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn server() -> ServerConfig {
        serde_json::from_value(
            serde_json::json!({"name":"notes","transport":{"type":"stdio","command":"fake"}}),
        )
        .unwrap()
    }
    #[test]
    fn defaults_fail_closed_and_aliases_cannot_collide() {
        let mut config = server();
        assert_eq!(config.permission("read"), Permission::Disabled);
        config.enabled = true;
        assert_eq!(config.permission("read"), Permission::Ask);
        assert_eq!(tool_alias("notes", "read").unwrap(), "notes__read");
        for name in ["", "a__b", "a.b", "a/b", "中文"] {
            assert!(tool_alias(name, "read").is_err());
        }
        assert!(tool_alias("notes", &"x".repeat(60)).is_err());
        assert!(validate_servers(&[config.clone(), config]).is_err());
    }
    #[test]
    fn foreign_secret_refs_and_credential_urls_are_rejected() {
        let mut config = server();
        config.transport = TransportConfig::Stdio {
            command: "fake".into(),
            args: vec![],
            env_refs: BTreeMap::from([("API_KEY".into(), "voice.device_token_secret".into())]),
        };
        assert_eq!(config.validate(), Err(ClientError::InvalidSecretRef));
        for address in [
            "https://u:p@example.test/mcp",
            "https://example.test/mcp?token=x",
            "https://example.test/mcp#x",
            "file:///tmp/x",
        ] {
            config.transport = TransportConfig::StreamableHttp {
                url: address.into(),
                header_refs: BTreeMap::new(),
            };
            assert!(config.validate().is_err());
        }
        config.transport = TransportConfig::StreamableHttp {
            url: "http://example.test/mcp".into(),
            header_refs: BTreeMap::from([(
                "authorization".into(),
                secret_ref("notes", "header", "authorization"),
            )]),
        };
        assert_eq!(config.validate(), Err(ClientError::InsecureCredentials));
        let text = serde_json::to_string(&config).unwrap();
        assert!(!text.contains("Bearer"));
    }
}
