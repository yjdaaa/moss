//! 创作画板持久化（#24）：在应用数据目录（`RuntimeRoot/draw_state.json`）保存画布、
//! 上次预览指针、表单配置与防重发指纹，避免桌面端启动端口回退导致按 origin 隔离的
//! `localStorage` 丢失旧画布和预览。

use std::collections::BTreeMap;

use serde_json::{Map, Value};

const DRAW_STATE_FILE: &str = "draw_state.json";
const MAX_SCENE_BYTES: usize = 6 * 1024 * 1024;
const MAX_LAST_PREVIEW_BYTES: usize = 16 * 1024;
const MAX_FORM_KEYS: usize = 64;
const FORM_KEYS: &[&str] = &[
    "material",
    "thickness_mm",
    "width_mm",
    "height_mm",
    "image_strategy",
    "vector_style",
    "image_mode",
    "line_interval_scale",
    "halftone_cell_mm",
    "halftone_angle_deg",
    "gray_min_power_percent",
    "vector_simplify_factor",
];
const MAX_FORM_VALUE_BYTES: usize = 4096;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DrawStateRecord {
    pub scene: Option<String>,
    pub last_preview: Option<String>,
    pub form: BTreeMap<String, String>,
    pub last_accepted_fingerprint: Option<String>,
}

impl DrawStateRecord {
    fn from_storage_json(value: Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "画板状态文件格式必须是 JSON 对象".to_owned())?;
        let mut record = Self::default();
        if let Some(v) = object.get("scene")
            && !v.is_null()
        {
            record.scene = Some(
                v.as_str()
                    .ok_or_else(|| "画板状态中的 scene 必须是字符串".to_owned())?
                    .to_owned(),
            );
        }
        if let Some(v) = object.get("last_preview")
            && !v.is_null()
        {
            record.last_preview = Some(
                v.as_str()
                    .ok_or_else(|| "画板状态中的 last_preview 必须是字符串".to_owned())?
                    .to_owned(),
            );
        }
        if let Some(v) = object.get("form")
            && !v.is_null()
        {
            let form_obj = v
                .as_object()
                .ok_or_else(|| "画板状态中的 form 必须是对象".to_owned())?;
            for (k, val) in form_obj {
                if !FORM_KEYS.contains(&k.as_str()) {
                    continue;
                }
                let text = val
                    .as_str()
                    .ok_or_else(|| format!("画板状态中的 form.{k} 必须是字符串"))?;
                record.form.insert(k.clone(), text.to_owned());
            }
        }
        if let Some(v) = object.get("last_accepted_fingerprint")
            && !v.is_null()
        {
            record.last_accepted_fingerprint = Some(
                v.as_str()
                    .ok_or_else(|| {
                        "画板状态中的 last_accepted_fingerprint 必须是字符串".to_owned()
                    })?
                    .to_owned(),
            );
        }
        Ok(record)
    }

    fn to_storage_json(&self) -> Value {
        let mut obj = Map::new();
        if let Some(scene) = &self.scene {
            obj.insert("scene".into(), Value::String(scene.clone()));
        }
        if let Some(last_preview) = &self.last_preview {
            obj.insert("last_preview".into(), Value::String(last_preview.clone()));
        }
        if !self.form.is_empty() {
            let mut form_obj = Map::new();
            for (k, v) in &self.form {
                form_obj.insert(k.clone(), Value::String(v.clone()));
            }
            obj.insert("form".into(), Value::Object(form_obj));
        }
        if let Some(fp) = &self.last_accepted_fingerprint {
            obj.insert(
                "last_accepted_fingerprint".into(),
                Value::String(fp.clone()),
            );
        }
        Value::Object(obj)
    }

    pub(crate) fn to_response_json(&self, exists: bool) -> Value {
        serde_json::json!({
            "exists": exists,
            "scene": self.scene,
            "last_preview": self.last_preview,
            "form": self.form,
            "last_accepted_fingerprint": self.last_accepted_fingerprint,
        })
    }

    pub(crate) fn apply_patch(&mut self, patch: &Map<String, Value>) -> Result<(), String> {
        if let Some(value) = patch.get("scene") {
            let text = value
                .as_str()
                .ok_or_else(|| "scene 必须是字符串".to_owned())?;
            if text.len() > MAX_SCENE_BYTES {
                return Err("scene 内容超出大小限制".to_owned());
            }
            self.scene = Some(text.to_owned());
        }
        if let Some(value) = patch.get("last_preview") {
            let text = value
                .as_str()
                .ok_or_else(|| "last_preview 必须是字符串".to_owned())?;
            if text.len() > MAX_LAST_PREVIEW_BYTES {
                return Err("last_preview 内容超出大小限制".to_owned());
            }
            self.last_preview = Some(text.to_owned());
        }
        if let Some(value) = patch.get("form") {
            let obj = value
                .as_object()
                .ok_or_else(|| "form 必须是键值对对象".to_owned())?;
            if obj.len() > MAX_FORM_KEYS {
                return Err("form 字段数量超出上限".to_owned());
            }
            let mut next_entries = Vec::with_capacity(obj.len());
            for (k, v) in obj {
                if k.is_empty()
                    || k.len() > 64
                    || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    return Err(format!("form 键名无效: {k}"));
                }
                if !FORM_KEYS.contains(&k.as_str()) {
                    continue;
                }
                let text = v.as_str().ok_or_else(|| format!("form.{k} 必须是字符串"))?;
                if text.len() > MAX_FORM_VALUE_BYTES {
                    return Err(format!("form.{k} 超出长度限制"));
                }
                next_entries.push((k.clone(), text.to_owned()));
            }
            for (k, v) in next_entries {
                self.form.insert(k, v);
            }
            if self.form.len() > MAX_FORM_KEYS {
                return Err("累计 form 字段数量超出上限".to_owned());
            }
        }
        if let Some(value) = patch.get("last_accepted_fingerprint") {
            let text = value
                .as_str()
                .ok_or_else(|| "last_accepted_fingerprint 必须是字符串".to_owned())?;
            let normalized = text.trim().to_ascii_lowercase();
            if !normalized.is_empty()
                && (normalized.len() != 64 || !normalized.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return Err("last_accepted_fingerprint 必须是 64 位十六进制摘要或空串".to_owned());
            }
            self.last_accepted_fingerprint = Some(normalized);
        }
        Ok(())
    }
}

fn read_file_state(runtime_path: &std::path::Path) -> Result<(DrawStateRecord, bool), String> {
    let target = runtime_path.join(DRAW_STATE_FILE);
    let parsed = match std::fs::read(&target) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| error.to_string())
            .and_then(DrawStateRecord::from_storage_json),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok((DrawStateRecord::default(), false));
        }
        Err(err) => return Err(format!("读取画板状态失败：{err}")),
    };
    match parsed {
        Ok(record) => Ok((record, true)),
        Err(_) => {
            // 保留坏文件供排查，下一次保存可从空状态继续。
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            std::fs::rename(
                &target,
                runtime_path.join(format!("{DRAW_STATE_FILE}.corrupt-{stamp}")),
            )
            .map_err(|err| format!("备份损坏画板状态失败：{err}"))?;
            Ok((DrawStateRecord::default(), false))
        }
    }
}

fn write_file_state(
    runtime_path: &std::path::Path,
    record: &DrawStateRecord,
) -> Result<(), String> {
    std::fs::create_dir_all(runtime_path).map_err(|error| format!("创建运行目录失败：{error}"))?;
    let target = runtime_path.join(DRAW_STATE_FILE);
    let data = serde_json::to_vec(&record.to_storage_json()).map_err(|error| error.to_string())?;
    let temp = runtime_path.join(format!("{DRAW_STATE_FILE}.api-tmp-{}", std::process::id()));
    std::fs::write(&temp, data)
        .and_then(|()| std::fs::rename(&temp, &target))
        .map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            format!("写入画板状态失败：{error}")
        })
}

impl crate::ApiService {
    pub(crate) fn read_draw_state(&self) -> Result<(DrawStateRecord, bool), String> {
        let _guard = self
            .draw_state_lock
            .lock()
            .map_err(|_| "画板状态锁失败".to_owned())?;
        if let Some(runtime_path) = self.runtime_path.as_deref() {
            read_file_state(runtime_path)
        } else {
            let guard = self
                .draw_state_memory
                .lock()
                .map_err(|_| "画板内存状态锁失败".to_owned())?;
            let exists = guard.scene.is_some()
                || guard.last_preview.is_some()
                || !guard.form.is_empty()
                || guard.last_accepted_fingerprint.is_some();
            Ok((guard.clone(), exists))
        }
    }

    pub(crate) fn update_draw_state(
        &self,
        patch: &Map<String, Value>,
    ) -> Result<(DrawStateRecord, bool), (u16, String)> {
        let _guard = self
            .draw_state_lock
            .lock()
            .map_err(|_| (500, "画板状态锁失败".to_owned()))?;
        if let Some(runtime_path) = self.runtime_path.as_deref() {
            let (mut current, _) = read_file_state(runtime_path).map_err(|e| (500, e))?;
            current.apply_patch(patch).map_err(|e| (400, e))?;
            write_file_state(runtime_path, &current).map_err(|e| (500, e))?;
            Ok((current, true))
        } else {
            let mut guard = self
                .draw_state_memory
                .lock()
                .map_err(|_| (500, "画板内存状态锁失败".to_owned()))?;
            let mut next = guard.clone();
            next.apply_patch(patch).map_err(|e| (400, e))?;
            *guard = next.clone();
            Ok((next, true))
        }
    }
}
