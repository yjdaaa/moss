//! 雕刻路线全局开关（2026-11-17 用户决定，技术书 r45）：矢量还是转图片。
//!
//! 一个值、存在运行目录 `settings.json` 的 `laser_engrave_route`（`"vector"` 默认 / `"image"`），
//! 工作台、创作画板与语音 / AI 生成的任务都读它，界面上的两个开关只是同一个值的两处入口，不会两边不同步：
//! - 文字任务（`/api/text-task/generate`，工作台与语音同一条）：请求没写 `engrave_route` 时按它；
//!   转图片时文字一律实心、渲染成位图逐行扫描，外框切割始终走矢量；
//! - 创作画板（`/api/draw/preview`）：请求没写 `mode` 时，矢量 → `vector`，转图片 → `raster`。
//!
//! `GET /api/engrave-route` 读，`POST /api/engrave-route {route}` 改（局域网只读，见
//! [`crate::access::is_reference_data_write`]）；语音与 AI 工具不能改。

use crate::settings::{read_runtime_settings, update_runtime_settings};

pub(crate) const SETTINGS_KEY: &str = "laser_engrave_route";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EngraveRoute {
    Vector,
    Image,
}

impl EngraveRoute {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "vector" => Ok(Self::Vector),
            "image" => Ok(Self::Image),
            other => Err(format!("engrave_route 只能是 vector / image，收到 {other}")),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Vector => "vector",
            Self::Image => "image",
        }
    }

    /// 请求里写了就按请求，否则按全局开关。
    pub(crate) fn from_request(
        object: &serde_json::Map<String, serde_json::Value>,
        default: Self,
    ) -> Result<Self, String> {
        match object.get("engrave_route") {
            None | Some(serde_json::Value::Null) => Ok(default),
            Some(serde_json::Value::String(value)) if value.trim().is_empty() => Ok(default),
            Some(serde_json::Value::String(value)) => Self::parse(value),
            Some(_) => Err("engrave_route 只能是 vector / image".into()),
        }
    }
}

impl crate::ApiService {
    /// 全局雕刻路线；没设置过或值无效时为矢量。
    pub(crate) fn engrave_route(&self) -> EngraveRoute {
        read_runtime_settings(self.runtime_path.as_deref())
            .get(SETTINGS_KEY)
            .and_then(|v| v.as_str())
            .and_then(|v| EngraveRoute::parse(v).ok())
            .unwrap_or(EngraveRoute::Vector)
    }

    pub(crate) fn engrave_route_json(&self) -> serde_json::Value {
        serde_json::json!({ "route": self.engrave_route().as_str() })
    }

    pub(crate) fn set_engrave_route(&self, route: EngraveRoute) -> Result<(), String> {
        update_runtime_settings(self.runtime_path.as_deref(), |settings| {
            settings.insert(
                SETTINGS_KEY.into(),
                serde_json::Value::String(route.as_str().into()),
            );
        })
    }
}
