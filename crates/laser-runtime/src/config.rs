use std::env;

#[derive(Debug, Clone, PartialEq)]
pub struct LaserSettings {
    pub default_connection_mode: String,
    pub engraving_mode: String,
    pub web_host: String,
    pub web_port: u16,
    pub material_params_file: Option<String>,
}

impl LaserSettings {
    pub fn from_env() -> Result<Self, String> {
        Self::from_env_with_runtime_dir(None)
    }

    pub fn from_env_with_runtime_dir(runtime_dir: Option<&std::path::Path>) -> Result<Self, String> {
        let _ = dotenvy::dotenv();
        let port_from_file = runtime_dir
            .map(|d| d.join("settings.json"))
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|v| v.get("web_port").and_then(|p| p.as_u64()))
            .map(|p| p as u16);

        let web_port = if let Ok(val) = env::var("LASER_WEB_PORT") {
            val.parse::<u16>().map_err(|_| "LASER_WEB_PORT must be a valid port".to_owned())?
        } else if let Some(p) = port_from_file {
            p
        } else {
            9666
        };
        let mode = env::var("LASER_DEFAULT_CONNECTION_MODE").unwrap_or_else(|_| "network".into());
        if !matches!(mode.as_str(), "network" | "serial") {
            return Err("LASER_DEFAULT_CONNECTION_MODE must be network or serial".into());
        }
        let engraving = env::var("LASER_ENGRAVING_MODE").unwrap_or_else(|_| "raster".into());
        if !matches!(engraving.as_str(), "raster" | "outline") {
            return Err("LASER_ENGRAVING_MODE must be raster or outline".into());
        }
        Ok(Self {
            default_connection_mode: mode,
            engraving_mode: engraving,
            web_host: env::var("LASER_WEB_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            web_port,
            material_params_file: env::var("LASER_MATERIAL_PARAMS_FILE").ok(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_match_env_contract() {
        let settings = LaserSettings::from_env().unwrap();
        assert_eq!(settings.web_port, 9666);
        assert!(matches!(
            settings.default_connection_mode.as_str(),
            "network" | "serial"
        ));
    }
}
