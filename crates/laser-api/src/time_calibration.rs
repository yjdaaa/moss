//! 工时自校准（r40，2026-11-16）：加工正常完成后比较“实际用时 / 预估用时”，按机器目标保留最近
//! [`MAX_SAMPLES`] 次比值，取中位数（限制在 [`FACTOR_RANGE`]）作为校准系数，乘到之后预览的工时估算上。
//!
//! - 只收正常完成的整件作业：取消、失败、急停、断点续雕、SD 脱机（含上传与校验时间）、实际用时不足
//!   [`MIN_ACTUAL_SECONDS`] 秒或没有预估值的一律不计；单次比值超出 [`RATIO_SANITY`] 视为异常不计。
//! - 至少 [`MIN_SAMPLES`] 次才开始校准；原始估算保留在 `raw_estimated_seconds`，`estimated_seconds` /
//!   `estimated_minutes` 是校准后的值（各处显示的“预计用时”随之变准），`laser_seconds` 等分项保持原始值。
//! - 存在运行目录 `settings.json` 的 `laser_time_calibration`：`{目标: {ratios: [...], updated_at}}`，
//!   目标即发送时的连接描述（串口 `端口@波特率` / 网络 `主机:端口`），最多 [`MAX_TARGETS`] 台。
use super::*;

pub(crate) const SETTINGS_KEY: &str = "laser_time_calibration";
pub(crate) const MAX_SAMPLES: usize = 10;
pub(crate) const MIN_SAMPLES: usize = 2;
pub(crate) const MIN_ACTUAL_SECONDS: f64 = 20.0;
pub(crate) const FACTOR_RANGE: (f64, f64) = (0.5, 3.0);
pub(crate) const RATIO_SANITY: (f64, f64) = (0.2, 5.0);
pub(crate) const MAX_TARGETS: usize = 8;

/// 一次作业结束时的用时信息。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JobTiming<'a> {
    pub status: &'a str,
    pub resumed: bool,
    pub sd_offline: bool,
    pub actual_seconds: f64,
    pub estimated_seconds: Option<f64>,
}

/// 这次作业能否作为校准样本：能则返回“实际 / 预估”比值，否则返回不计的原因代码。
pub(crate) fn sample_ratio(timing: &JobTiming<'_>) -> Result<f64, &'static str> {
    if timing.status != "completed" {
        return Err("not_completed");
    }
    if timing.resumed {
        return Err("resumed");
    }
    if timing.sd_offline {
        return Err("sd_offline");
    }
    if !timing.actual_seconds.is_finite() || timing.actual_seconds < MIN_ACTUAL_SECONDS {
        return Err("too_short");
    }
    let Some(estimated) = timing
        .estimated_seconds
        .filter(|v| v.is_finite() && *v > 0.0)
    else {
        return Err("no_estimate");
    };
    let ratio = timing.actual_seconds / estimated;
    if !(RATIO_SANITY.0..=RATIO_SANITY.1).contains(&ratio) {
        return Err("outlier");
    }
    Ok(ratio)
}

/// 最近比值的中位数，限制在 [`FACTOR_RANGE`]；样本不足 [`MIN_SAMPLES`] 时为 `None`。
pub(crate) fn factor_from(ratios: &[f64]) -> Option<f64> {
    let mut sorted: Vec<f64> = ratios.iter().copied().filter(|r| r.is_finite()).collect();
    if sorted.len() < MIN_SAMPLES {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    let median = if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    };
    Some(median.clamp(FACTOR_RANGE.0, FACTOR_RANGE.1))
}

fn entry_ratios(entry: &serde_json::Value) -> Vec<f64> {
    entry
        .get("ratios")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_f64)
                .filter(|r| r.is_finite() && (RATIO_SANITY.0..=RATIO_SANITY.1).contains(r))
                .collect()
        })
        .unwrap_or_default()
}

/// 把一个比值记进 `settings.json` 的映射（就地修改）：同一目标只留最近 [`MAX_SAMPLES`] 次，
/// 目标超过 [`MAX_TARGETS`] 台时去掉最久没更新的。返回该目标现在的样本数。
pub(crate) fn record_ratio(
    settings: &mut serde_json::Map<String, serde_json::Value>,
    target: &str,
    ratio: f64,
    now: u64,
) -> usize {
    let mut store = settings
        .get(SETTINGS_KEY)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let mut ratios = store.get(target).map(entry_ratios).unwrap_or_default();
    ratios.push((ratio * 10_000.0).round() / 10_000.0);
    if ratios.len() > MAX_SAMPLES {
        ratios.drain(..ratios.len() - MAX_SAMPLES);
    }
    let samples = ratios.len();
    store.insert(
        target.to_owned(),
        serde_json::json!({"ratios": ratios, "updated_at": now}),
    );
    while store.len() > MAX_TARGETS {
        let oldest = store
            .iter()
            .min_by_key(|(_, v)| v.get("updated_at").and_then(|t| t.as_u64()).unwrap_or(0))
            .map(|(k, _)| k.clone());
        match oldest {
            Some(key) => {
                store.remove(&key);
            }
            None => break,
        }
    }
    settings.insert(SETTINGS_KEY.into(), serde_json::Value::Object(store));
    samples
}

/// 某台机器当前的校准：`(系数, 样本数)`；没有记录或样本不足时为 `None`。
pub(crate) fn calibration_for(
    settings: &serde_json::Map<String, serde_json::Value>,
    target: &str,
) -> Option<(f64, usize)> {
    let ratios = settings
        .get(SETTINGS_KEY)
        .and_then(|v| v.get(target))
        .map(entry_ratios)?;
    factor_from(&ratios).map(|factor| (factor, ratios.len()))
}

/// 最近更新的目标（预览时不知道要发给哪台机器、也没有记住的连接时用它）。
pub(crate) fn latest_target(
    settings: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    settings
        .get(SETTINGS_KEY)?
        .as_object()?
        .iter()
        .max_by_key(|(_, v)| v.get("updated_at").and_then(|t| t.as_u64()).unwrap_or(0))
        .map(|(k, _)| k.clone())
}

/// 把校准系数写进结果里的 `time_estimate`（就地修改）：保留原始值 `raw_estimated_seconds`，
/// `estimated_seconds` / `estimated_minutes` 改为校准后的值，另加 `calibration`。
/// 已经校准过的（带 `raw_estimated_seconds`）按原始值重算，不会叠乘。
pub(crate) fn apply_to_estimate(estimate: &mut serde_json::Value, factor: f64, samples: usize) {
    let Some(map) = estimate.as_object_mut() else {
        return;
    };
    let Some(raw) = map
        .get("raw_estimated_seconds")
        .or_else(|| map.get("estimated_seconds"))
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite() && *v >= 0.0)
    else {
        return;
    };
    let seconds = raw * factor;
    map.insert("raw_estimated_seconds".into(), serde_json::json!(raw));
    map.insert(
        "estimated_seconds".into(),
        serde_json::json!((seconds * 10.0).round() / 10.0),
    );
    map.insert(
        "estimated_minutes".into(),
        serde_json::json!((seconds / 60.0 * 100.0).round() / 100.0),
    );
    map.insert(
        "calibration".into(),
        serde_json::json!({
            "factor": (factor * 1000.0).round() / 1000.0,
            "samples": samples,
            "source": "recent_jobs",
        }),
    );
}

/// 记录里预览时的原始工时估算（workflow 在 `artifacts.time_estimate`，标定记录在顶层 `time_estimate`）。
pub(crate) fn record_raw_estimate(payload: &serde_json::Value) -> Option<f64> {
    [
        &payload["artifacts"]["time_estimate"],
        &payload["time_estimate"],
    ]
    .into_iter()
    .find_map(|estimate| {
        estimate
            .get("raw_estimated_seconds")
            .or_else(|| estimate.get("estimated_seconds"))
            .and_then(|v| v.as_f64())
    })
    .filter(|v| v.is_finite() && *v > 0.0)
}

impl ApiService {
    /// 预览时用哪台机器的校准：请求里给了连接就用它，否则用记住的连接，再否则用最近校准过的那台。
    fn time_calibration_target(
        &self,
        request: &serde_json::Map<String, serde_json::Value>,
        settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<String> {
        let has = |keys: &[&str]| {
            keys.iter().any(|key| {
                request
                    .get(*key)
                    .and_then(|v| v.as_str())
                    .is_some_and(|v| !v.trim().is_empty())
            })
        };
        let serial = request
            .get("connection_mode")
            .or_else(|| request.get("preferred_mode"))
            .and_then(|v| v.as_str())
            .is_some_and(|m| m.trim().eq_ignore_ascii_case("serial"));
        // 串口必须给了端口（不去枚举系统串口），网络必须给了主机
        let explicit = if serial {
            has(&["port", "serial_port"])
        } else {
            has(&["network_host", "host"])
        };
        if explicit && let Ok(spec) = self.connection_spec_from_request(request) {
            return Some(spec.describe());
        }
        if let Some(fields) = self
            .verified_connection()
            .and_then(|v| v.get("fields").and_then(|f| f.as_object()).cloned())
            && let Ok(spec) = self.connection_spec_from_request(&fields)
        {
            return Some(spec.describe());
        }
        latest_target(settings)
    }

    /// 给新生成结果里的 `time_estimate` 套上该机器的工时校准（没有足够样本时不改动）。
    pub(crate) fn apply_time_calibration(
        &self,
        estimate: Option<&mut serde_json::Value>,
        request: &serde_json::Map<String, serde_json::Value>,
    ) {
        let Some(estimate) = estimate else {
            return;
        };
        if self.runtime_path.is_none() {
            return;
        }
        let settings = read_runtime_settings(self.runtime_path.as_deref());
        if !settings.contains_key(SETTINGS_KEY) {
            return;
        }
        let Some(target) = self.time_calibration_target(request, &settings) else {
            return;
        };
        if let Some((factor, samples)) = calibration_for(&settings, &target) {
            apply_to_estimate(estimate, factor, samples);
        }
    }

    /// `GET /api/time-calibration`：各台机器的校准系数与样本数（只读，不含比值明细以外的数据）。
    pub(crate) fn time_calibration_status(&self) -> serde_json::Value {
        let settings = read_runtime_settings(self.runtime_path.as_deref());
        let targets: Vec<serde_json::Value> = settings
            .get(SETTINGS_KEY)
            .and_then(|v| v.as_object())
            .map(|store| {
                store
                    .iter()
                    .map(|(target, entry)| {
                        let ratios = entry_ratios(entry);
                        let factor = factor_from(&ratios);
                        serde_json::json!({
                            "target": target,
                            "samples": ratios.len(),
                            "factor": factor.map(|f| (f * 1000.0).round() / 1000.0),
                            "active": factor.is_some(),
                            "updated_at": entry.get("updated_at").cloned().unwrap_or(serde_json::Value::Null),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        serde_json::json!({
            "targets": targets,
            "min_samples": MIN_SAMPLES,
            "max_samples": MAX_SAMPLES,
            "min_actual_seconds": MIN_ACTUAL_SECONDS,
            "factor_range": [FACTOR_RANGE.0, FACTOR_RANGE.1],
        })
    }

    /// `POST /api/time-calibration {action:"reset", target?}`：清除某台（不给 target 时全部）机器的校准。
    pub(crate) fn reset_time_calibration(&self, target: Option<&str>) -> Result<(), String> {
        update_runtime_settings(self.runtime_path.as_deref(), |settings| match target {
            Some(target) => {
                if let Some(store) = settings
                    .get_mut(SETTINGS_KEY)
                    .and_then(|v| v.as_object_mut())
                {
                    store.remove(target);
                }
            }
            None => {
                settings.remove(SETTINGS_KEY);
            }
        })
    }
}

/// 发送线程在作业结束时调用：符合条件就记一次样本，返回写进 `send_result.time_calibration` 的说明。
pub(crate) fn record_job_timing(
    runtime_path: Option<&std::path::Path>,
    target: &str,
    timing: &JobTiming<'_>,
) -> serde_json::Value {
    let ratio = match sample_ratio(timing) {
        Ok(ratio) => ratio,
        Err(reason) => return serde_json::json!({"recorded": false, "reason": reason}),
    };
    if runtime_path.is_none() {
        return serde_json::json!({"recorded": false, "reason": "no_runtime_dir"});
    }
    let mut samples = 0;
    let mut factor = None;
    let result = update_runtime_settings(runtime_path, |settings| {
        samples = record_ratio(settings, target, ratio, unix_seconds());
        factor = calibration_for(settings, target).map(|(f, _)| f);
    });
    match result {
        Ok(()) => serde_json::json!({
            "recorded": true,
            "ratio": (ratio * 1000.0).round() / 1000.0,
            "samples": samples,
            "factor": factor.map(|f| (f * 1000.0).round() / 1000.0),
        }),
        Err(_) => serde_json::json!({"recorded": false, "reason": "settings_write_failed"}),
    }
}
