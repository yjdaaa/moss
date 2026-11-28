//! 按连接目标保存的“跟机器走”后端档案与全局走边框形状（2026-11-28，技术书 r78）。
//!
//! - `laser_machine_profiles`：按 `target_key`（与前端 `connectionTargetKey` 同格式：`serial:<port>:<baud>` /
//!   `network:<host>:<port>`）分桶持久化到运行目录 `settings.json`，最多保留 16 个目标（按 `updated_at` 淘汰最旧）。
//!   存放用户手填且“跟机器走”的配置：手填行程（`manual_travel`）、手填运动参数（`manual_motion`）、
//!   双向扫描偏移校准（`scan_offset_mm` / `scan_offset_table`）。
//! - `laser_framing_shape`：全局走边框形状（`"box"` 默认 / `"hull"` 沿图形外轮廓），工作台、画板、
//!   AI 预览窗与语音 / MCP 按预览走边框共用同一份设置。
//! - **安全不变量（r65 / r68 / r78）**：手填行程只决定界面与 AI 生成/问答默认取值；发送前仍按当场从固件
//!   读回的行程执行 `check_job_fits_machine` 复核，手填值绝不绕过发送前固件检查。

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

pub(crate) const MACHINE_PROFILES_KEY: &str = "laser_machine_profiles";
pub(crate) const FRAMING_SHAPE_KEY: &str = "laser_framing_shape";
pub(crate) const MAX_MACHINE_PROFILES: usize = 16;

const MAX_XY_TRAVEL_MM: f64 = 2000.0;
const MAX_Z_TRAVEL_MM: f64 = 500.0;
const MAX_ACCEL_MM_S2: f64 = 100_000.0;
const MAX_RATE_MM_PER_MIN: f64 = 1_000_000.0;
const MAX_JUNCTION_DEVIATION_MM: f64 = 10.0;
const MAX_SCAN_OFFSET_MM: f64 = 2.0;

/// 连接规范 → 与前端 `connectionTargetKey` 一致的目标键：
/// - 串口：`serial:<port>:<baud>`
/// - 网络：`network:<host>:<port>`
pub(crate) fn connection_target_key(spec: &laser_device::ConnectionSpec) -> String {
    match spec {
        laser_device::ConnectionSpec::Serial { port, baud_rate } => {
            format!("serial:{}:{baud_rate}", port.trim())
        }
        laser_device::ConnectionSpec::Tcp { host, port } => {
            format!("network:{}:{port}", host.trim())
        }
    }
}

/// 校验客户端提交的 `target_key`（只接受 `serial:<port>:<baud>` 或 `network:<host>:<port>`）。
pub(crate) fn validate_target_key(raw: &str) -> Result<String, String> {
    let key = raw.trim();
    if key.is_empty() || key.len() > 256 || key.chars().any(char::is_control) {
        return Err("target_key 不能为空且不能包含控制字符".into());
    }
    if let Some(rest) = key.strip_prefix("serial:") {
        let Some((port, baud)) = rest.rsplit_once(':') else {
            return Err("serial target_key 格式须为 serial:<port>:<baud>".into());
        };
        if port.trim().is_empty() || baud.parse::<u32>().ok().filter(|&b| b > 0).is_none() {
            return Err("serial target_key 格式须为 serial:<port>:<baud>".into());
        }
        return Ok(format!(
            "serial:{}:{}",
            port.trim(),
            baud.parse::<u32>().unwrap()
        ));
    }
    if let Some(rest) = key.strip_prefix("network:") {
        let Some((host, port)) = rest.rsplit_once(':') else {
            return Err("network target_key 格式须为 network:<host>:<port>".into());
        };
        if host.trim().is_empty() || port.parse::<u16>().ok().filter(|&p| p > 0).is_none() {
            return Err("network target_key 格式须为 network:<host>:<port>".into());
        }
        return Ok(format!(
            "network:{}:{}",
            host.trim(),
            port.parse::<u16>().unwrap()
        ));
    }
    Err("target_key 必须以 serial: 或 network: 开头".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FramingShape {
    #[default]
    Box,
    Hull,
}

impl FramingShape {
    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "box" => Ok(Self::Box),
            "hull" => Ok(Self::Hull),
            other => Err(format!(
                "laser_framing_shape 只能是 box / hull，收到 {other}"
            )),
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Box => "box",
            Self::Hull => "hull",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ManualTravel {
    pub(crate) x_travel_mm: Option<f64>,
    pub(crate) y_travel_mm: Option<f64>,
    pub(crate) z_travel_mm: Option<f64>,
}

impl ManualTravel {
    pub(crate) fn is_empty(&self) -> bool {
        self.x_travel_mm.is_none() && self.y_travel_mm.is_none() && self.z_travel_mm.is_none()
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut map = Map::new();
        if let Some(v) = self.x_travel_mm {
            map.insert("x_travel_mm".into(), json!(v));
        }
        if let Some(v) = self.y_travel_mm {
            map.insert("y_travel_mm".into(), json!(v));
        }
        if let Some(v) = self.z_travel_mm {
            map.insert("z_travel_mm".into(), json!(v));
        }
        Value::Object(map)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ManualMotion {
    pub(crate) x_accel_mm_s2: Option<f64>,
    pub(crate) y_accel_mm_s2: Option<f64>,
    pub(crate) x_max_rate_mm_per_min: Option<f64>,
    pub(crate) y_max_rate_mm_per_min: Option<f64>,
    pub(crate) junction_deviation_mm: Option<f64>,
}

impl ManualMotion {
    pub(crate) fn is_empty(&self) -> bool {
        self.x_accel_mm_s2.is_none()
            && self.y_accel_mm_s2.is_none()
            && self.x_max_rate_mm_per_min.is_none()
            && self.y_max_rate_mm_per_min.is_none()
            && self.junction_deviation_mm.is_none()
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut map = Map::new();
        if let Some(v) = self.x_accel_mm_s2 {
            map.insert("x_accel_mm_s2".into(), json!(v));
        }
        if let Some(v) = self.y_accel_mm_s2 {
            map.insert("y_accel_mm_s2".into(), json!(v));
        }
        if let Some(v) = self.x_max_rate_mm_per_min {
            map.insert("x_max_rate_mm_per_min".into(), json!(v));
        }
        if let Some(v) = self.y_max_rate_mm_per_min {
            map.insert("y_max_rate_mm_per_min".into(), json!(v));
        }
        if let Some(v) = self.junction_deviation_mm {
            map.insert("junction_deviation_mm".into(), json!(v));
        }
        Value::Object(map)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct MachineProfile {
    pub(crate) manual_travel: ManualTravel,
    pub(crate) manual_motion: ManualMotion,
    pub(crate) scan_offset_mm: Option<f64>,
    pub(crate) scan_offset_table: Vec<[f64; 2]>,
    pub(crate) updated_at: u64,
}

impl MachineProfile {
    pub(crate) fn is_empty(&self) -> bool {
        self.manual_travel.is_empty()
            && self.manual_motion.is_empty()
            && self.scan_offset_mm.is_none()
            && self.scan_offset_table.is_empty()
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut map = Map::new();
        if !self.manual_travel.is_empty() {
            map.insert("manual_travel".into(), self.manual_travel.to_json());
        }
        if !self.manual_motion.is_empty() {
            map.insert("manual_motion".into(), self.manual_motion.to_json());
        }
        if let Some(offset) = self.scan_offset_mm {
            map.insert("scan_offset_mm".into(), json!(offset));
        }
        if !self.scan_offset_table.is_empty() {
            map.insert("scan_offset_table".into(), json!(self.scan_offset_table));
        }
        map.insert("updated_at".into(), json!(self.updated_at));
        Value::Object(map)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct MachineProfileStore {
    pub(crate) targets: BTreeMap<String, MachineProfile>,
}

fn parse_pos_limit(value: &Value, field: &str, max: f64) -> Result<Option<f64>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let num = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse::<f64>().ok()));
    match num {
        Some(v) if v.is_finite() && v > 0.0 && v <= max => Ok(Some(v)),
        _ => Err(format!("{field} 必须是 (0, {max}] 之间的有限数字")),
    }
}

fn parse_scan_offset_field(value: &Value) -> Result<Option<f64>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let num = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse::<f64>().ok()));
    match num {
        Some(v) if v.is_finite() && v.abs() <= MAX_SCAN_OFFSET_MM => {
            if v.abs() <= 1e-9 {
                Ok(None)
            } else {
                Ok(Some(v))
            }
        }
        _ => Err(format!(
            "scan_offset_mm 须在 -{MAX_SCAN_OFFSET_MM} ~ {MAX_SCAN_OFFSET_MM} mm"
        )),
    }
}

fn apply_travel_patch(travel: &mut ManualTravel, value: &Value) -> Result<(), String> {
    if value.is_null() {
        *travel = ManualTravel::default();
        return Ok(());
    }
    let Some(map) = value.as_object() else {
        return Err("manual_travel 必须是对象或 null".into());
    };
    if let Some(v) = map.get("x_travel_mm").or_else(|| map.get("x")) {
        travel.x_travel_mm = parse_pos_limit(v, "manual_travel.x_travel_mm", MAX_XY_TRAVEL_MM)?;
    }
    if let Some(v) = map.get("y_travel_mm").or_else(|| map.get("y")) {
        travel.y_travel_mm = parse_pos_limit(v, "manual_travel.y_travel_mm", MAX_XY_TRAVEL_MM)?;
    }
    if let Some(v) = map.get("z_travel_mm").or_else(|| map.get("z")) {
        travel.z_travel_mm = parse_pos_limit(v, "manual_travel.z_travel_mm", MAX_Z_TRAVEL_MM)?;
    }
    Ok(())
}

fn apply_motion_patch(motion: &mut ManualMotion, value: &Value) -> Result<(), String> {
    if value.is_null() {
        *motion = ManualMotion::default();
        return Ok(());
    }
    let Some(map) = value.as_object() else {
        return Err("manual_motion 必须是对象或 null".into());
    };
    if let Some(v) = map.get("acceleration_mm_s2") {
        let accel = parse_pos_limit(v, "manual_motion.acceleration_mm_s2", MAX_ACCEL_MM_S2)?;
        motion.x_accel_mm_s2 = accel;
        motion.y_accel_mm_s2 = accel;
    }
    if let Some(v) = map.get("x_accel_mm_s2") {
        motion.x_accel_mm_s2 = parse_pos_limit(v, "manual_motion.x_accel_mm_s2", MAX_ACCEL_MM_S2)?;
    }
    if let Some(v) = map.get("y_accel_mm_s2") {
        motion.y_accel_mm_s2 = parse_pos_limit(v, "manual_motion.y_accel_mm_s2", MAX_ACCEL_MM_S2)?;
    }
    if let Some(v) = map.get("x_max_rate_mm_per_min") {
        motion.x_max_rate_mm_per_min = parse_pos_limit(
            v,
            "manual_motion.x_max_rate_mm_per_min",
            MAX_RATE_MM_PER_MIN,
        )?;
    }
    if let Some(v) = map.get("y_max_rate_mm_per_min") {
        motion.y_max_rate_mm_per_min = parse_pos_limit(
            v,
            "manual_motion.y_max_rate_mm_per_min",
            MAX_RATE_MM_PER_MIN,
        )?;
    }
    if let Some(v) = map.get("junction_deviation_mm") {
        motion.junction_deviation_mm = parse_pos_limit(
            v,
            "manual_motion.junction_deviation_mm",
            MAX_JUNCTION_DEVIATION_MM,
        )?;
    }
    Ok(())
}

fn next_updated_at(targets: &BTreeMap<String, MachineProfile>) -> u64 {
    let mut ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1);
    for entry in targets.values() {
        if entry.updated_at >= ts {
            ts = entry.updated_at.saturating_add(1);
        }
    }
    ts
}

fn prune_targets(targets: &mut BTreeMap<String, MachineProfile>) {
    while targets.len() > MAX_MACHINE_PROFILES {
        let Some(oldest_key) = targets
            .iter()
            .min_by(|(ka, va), (kb, vb)| va.updated_at.cmp(&vb.updated_at).then_with(|| ka.cmp(kb)))
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        targets.remove(&oldest_key);
    }
}

/// 宽松解析单个目标档案（用于从 `settings.json` 加载或旧浏览器本地数据一次性迁移）。
fn parse_profile_lenient(raw: &Value) -> Option<MachineProfile> {
    let map = raw.as_object()?;
    let mut profile = MachineProfile {
        updated_at: map.get("updated_at").and_then(Value::as_u64).unwrap_or(0),
        ..Default::default()
    };
    if let Some(travel_val) = map.get("manual_travel").and_then(Value::as_object) {
        if let Some(v) = travel_val
            .get("x_travel_mm")
            .or_else(|| travel_val.get("x"))
            && let Ok(parsed) = parse_pos_limit(v, "x_travel_mm", MAX_XY_TRAVEL_MM)
        {
            profile.manual_travel.x_travel_mm = parsed;
        }
        if let Some(v) = travel_val
            .get("y_travel_mm")
            .or_else(|| travel_val.get("y"))
            && let Ok(parsed) = parse_pos_limit(v, "y_travel_mm", MAX_XY_TRAVEL_MM)
        {
            profile.manual_travel.y_travel_mm = parsed;
        }
        if let Some(v) = travel_val
            .get("z_travel_mm")
            .or_else(|| travel_val.get("z"))
            && let Ok(parsed) = parse_pos_limit(v, "z_travel_mm", MAX_Z_TRAVEL_MM)
        {
            profile.manual_travel.z_travel_mm = parsed;
        }
    }
    if let Some(motion_val) = map.get("manual_motion").and_then(Value::as_object) {
        if let Some(v) = motion_val.get("acceleration_mm_s2")
            && let Ok(Some(accel)) = parse_pos_limit(v, "acceleration_mm_s2", MAX_ACCEL_MM_S2)
        {
            profile.manual_motion.x_accel_mm_s2 = Some(accel);
            profile.manual_motion.y_accel_mm_s2 = Some(accel);
        }
        if let Some(v) = motion_val.get("x_accel_mm_s2")
            && let Ok(parsed) = parse_pos_limit(v, "x_accel_mm_s2", MAX_ACCEL_MM_S2)
        {
            profile.manual_motion.x_accel_mm_s2 = parsed;
        }
        if let Some(v) = motion_val.get("y_accel_mm_s2")
            && let Ok(parsed) = parse_pos_limit(v, "y_accel_mm_s2", MAX_ACCEL_MM_S2)
        {
            profile.manual_motion.y_accel_mm_s2 = parsed;
        }
        if let Some(v) = motion_val.get("x_max_rate_mm_per_min")
            && let Ok(parsed) = parse_pos_limit(v, "x_max_rate_mm_per_min", MAX_RATE_MM_PER_MIN)
        {
            profile.manual_motion.x_max_rate_mm_per_min = parsed;
        }
        if let Some(v) = motion_val.get("y_max_rate_mm_per_min")
            && let Ok(parsed) = parse_pos_limit(v, "y_max_rate_mm_per_min", MAX_RATE_MM_PER_MIN)
        {
            profile.manual_motion.y_max_rate_mm_per_min = parsed;
        }
        if let Some(v) = motion_val.get("junction_deviation_mm")
            && let Ok(parsed) =
                parse_pos_limit(v, "junction_deviation_mm", MAX_JUNCTION_DEVIATION_MM)
        {
            profile.manual_motion.junction_deviation_mm = parsed;
        }
    }
    if let Some(v) = map.get("scan_offset_mm")
        && let Ok(parsed) = parse_scan_offset_field(v)
    {
        profile.scan_offset_mm = parsed;
    }
    if let Some(v) = map.get("scan_offset_table")
        && let Ok(Some(rows)) = crate::generation::parse_scan_offset_table(Some(v))
    {
        profile.scan_offset_table = rows.into_iter().map(|(s, o)| [s, o]).collect();
    }
    (!profile.is_empty()).then_some(profile)
}

impl MachineProfileStore {
    /// 从 `settings.json` 的 `laser_machine_profiles` 加载（旧文件无该键或损坏项时静默跳过）。
    pub(crate) fn from_settings_value(value: Option<&Value>) -> Self {
        let Some(targets_obj) = value
            .and_then(Value::as_object)
            .and_then(|root| root.get("targets"))
            .and_then(Value::as_object)
        else {
            return Self::default();
        };
        let mut targets = BTreeMap::new();
        for (raw_key, raw_entry) in targets_obj {
            let Ok(key) = validate_target_key(raw_key) else {
                continue;
            };
            if let Some(profile) = parse_profile_lenient(raw_entry) {
                targets.insert(key, profile);
            }
        }
        prune_targets(&mut targets);
        Self { targets }
    }

    pub(crate) fn to_json(&self) -> Value {
        let mut targets = Map::new();
        for (key, profile) in &self.targets {
            if !profile.is_empty() {
                targets.insert(key.clone(), profile.to_json());
            }
        }
        json!({ "targets": targets })
    }

    pub(crate) fn get(&self, target_key: &str) -> Option<&MachineProfile> {
        self.targets.get(target_key)
    }

    /// 严格校验并增量更新单个目标的档案；某目标所有字段都清空时自动删除该目标条目。
    pub(crate) fn apply_patch(&mut self, patch: &Map<String, Value>) -> Result<(), String> {
        let Some(raw_key) = patch.get("target_key").and_then(Value::as_str) else {
            return Err("machine_profile_patch 缺少 target_key".into());
        };
        let key = validate_target_key(raw_key)?;
        if patch.get("clear").and_then(Value::as_bool) == Some(true) {
            self.targets.remove(&key);
            return Ok(());
        }
        let mut entry = self.targets.get(&key).cloned().unwrap_or_default();
        if let Some(v) = patch.get("manual_travel") {
            apply_travel_patch(&mut entry.manual_travel, v)?;
        }
        if let Some(v) = patch.get("manual_motion") {
            apply_motion_patch(&mut entry.manual_motion, v)?;
        }
        if let Some(v) = patch.get("scan_offset_mm") {
            entry.scan_offset_mm = parse_scan_offset_field(v)?;
        }
        if let Some(v) = patch.get("scan_offset_table") {
            let rows = crate::generation::parse_scan_offset_table(Some(v))?;
            entry.scan_offset_table = rows
                .unwrap_or_default()
                .into_iter()
                .map(|(s, o)| [s, o])
                .collect();
        }
        if entry.is_empty() {
            self.targets.remove(&key);
        } else {
            entry.updated_at = next_updated_at(&self.targets);
            self.targets.insert(key, entry);
            prune_targets(&mut self.targets);
        }
        Ok(())
    }

    /// 一次性迁移前端历史手填值：后端已有值的目标 / 子字段严格保留不覆盖，且绝不挤掉后端已有目标。
    pub(crate) fn merge_migration(&mut self, migrate: &Map<String, Value>) -> Result<bool, String> {
        let Some(targets_val) = migrate.get("targets") else {
            return Ok(false);
        };
        let Some(targets_obj) = targets_val.as_object() else {
            return Err("machine_profiles_migrate.targets 必须是对象".into());
        };
        let mut incoming: Vec<(String, MachineProfile)> = Vec::new();
        for (raw_key, raw_entry) in targets_obj {
            let Ok(key) = validate_target_key(raw_key) else {
                continue;
            };
            if let Some(profile) = parse_profile_lenient(raw_entry) {
                incoming.push((key, profile));
            }
        }
        // 新近更新的旧目标优先迁入（满 16 个时不淘汰后端已有目标）
        incoming.sort_by(|a, b| {
            b.1.updated_at
                .cmp(&a.1.updated_at)
                .then_with(|| a.0.cmp(&b.0))
        });

        let mut changed = false;
        for (key, mut src) in incoming {
            if let Some(existing) = self.targets.get_mut(&key) {
                let mut entry_changed = false;
                if existing.manual_travel.x_travel_mm.is_none()
                    && src.manual_travel.x_travel_mm.is_some()
                {
                    existing.manual_travel.x_travel_mm = src.manual_travel.x_travel_mm;
                    entry_changed = true;
                }
                if existing.manual_travel.y_travel_mm.is_none()
                    && src.manual_travel.y_travel_mm.is_some()
                {
                    existing.manual_travel.y_travel_mm = src.manual_travel.y_travel_mm;
                    entry_changed = true;
                }
                if existing.manual_travel.z_travel_mm.is_none()
                    && src.manual_travel.z_travel_mm.is_some()
                {
                    existing.manual_travel.z_travel_mm = src.manual_travel.z_travel_mm;
                    entry_changed = true;
                }
                if existing.manual_motion.x_accel_mm_s2.is_none()
                    && src.manual_motion.x_accel_mm_s2.is_some()
                {
                    existing.manual_motion.x_accel_mm_s2 = src.manual_motion.x_accel_mm_s2;
                    entry_changed = true;
                }
                if existing.manual_motion.y_accel_mm_s2.is_none()
                    && src.manual_motion.y_accel_mm_s2.is_some()
                {
                    existing.manual_motion.y_accel_mm_s2 = src.manual_motion.y_accel_mm_s2;
                    entry_changed = true;
                }
                if existing.manual_motion.x_max_rate_mm_per_min.is_none()
                    && src.manual_motion.x_max_rate_mm_per_min.is_some()
                {
                    existing.manual_motion.x_max_rate_mm_per_min =
                        src.manual_motion.x_max_rate_mm_per_min;
                    entry_changed = true;
                }
                if existing.manual_motion.y_max_rate_mm_per_min.is_none()
                    && src.manual_motion.y_max_rate_mm_per_min.is_some()
                {
                    existing.manual_motion.y_max_rate_mm_per_min =
                        src.manual_motion.y_max_rate_mm_per_min;
                    entry_changed = true;
                }
                if existing.manual_motion.junction_deviation_mm.is_none()
                    && src.manual_motion.junction_deviation_mm.is_some()
                {
                    existing.manual_motion.junction_deviation_mm =
                        src.manual_motion.junction_deviation_mm;
                    entry_changed = true;
                }
                if existing.scan_offset_mm.is_none()
                    && existing.scan_offset_table.is_empty()
                    && (src.scan_offset_mm.is_some() || !src.scan_offset_table.is_empty())
                {
                    existing.scan_offset_mm = src.scan_offset_mm;
                    existing.scan_offset_table = src.scan_offset_table;
                    entry_changed = true;
                }
                if entry_changed {
                    changed = true;
                }
            } else if self.targets.len() < MAX_MACHINE_PROFILES {
                if src.updated_at == 0 {
                    src.updated_at = next_updated_at(&self.targets);
                }
                self.targets.insert(key, src);
                changed = true;
            }
        }
        Ok(changed)
    }
}

impl crate::ApiService {
    pub(crate) fn machine_profiles_json(&self) -> Value {
        self.machine_profiles
            .read()
            .map(|store| store.to_json())
            .unwrap_or_else(|_| json!({ "targets": {} }))
    }

    pub(crate) fn machine_profile_for_key(&self, target_key: &str) -> Option<MachineProfile> {
        self.machine_profiles
            .read()
            .ok()
            .and_then(|store| store.get(target_key).cloned())
    }

    pub(crate) fn framing_shape(&self) -> FramingShape {
        self.framing_shape
            .read()
            .ok()
            .and_then(|slot| *slot)
            .unwrap_or(FramingShape::Box)
    }

    pub(crate) fn framing_shape_configured(&self) -> bool {
        self.framing_shape
            .read()
            .ok()
            .is_some_and(|slot| slot.is_some())
    }
}
