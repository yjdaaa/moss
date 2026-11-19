//! 本地音乐目录设置与启动时扫描。

use super::*;

/// 启动内置语音服务时尝试扫描本地音乐目录；换电脑或拔盘导致目录不可用时只返回提示，不阻断内置语音主服务启动。
pub(super) fn scan_music_for_start(
    config: &moss_voice::settings::BuiltinMusic,
    chat_ready: bool,
) -> (
    Option<Arc<moss_voice_server::music::MusicIndex>>,
    Option<String>,
) {
    if !config.enabled || !chat_ready {
        return (None, None);
    }
    match moss_voice_server::music::MusicIndex::scan(std::path::Path::new(&config.directory)) {
        Ok(found) => (Some(Arc::new(found)), None),
        Err(_) => {
            let warning = "本地音乐目录当前不可用（换电脑或移动盘拔出时请重新选择音乐文件夹），本次启动已跳过本地音乐".to_owned();
            eprintln!("[moss-desktop] {warning}");
            (None, Some(warning))
        }
    }
}

impl BuiltinVoice {
    /// NV14: main-window-only local music settings. Reading status does not
    /// scan the user's filesystem; saving with enabled=true explicitly
    /// validates only the requested local folder.
    pub fn music_status(&self) -> Value {
        let settings = self.settings();
        let warning = if settings.builtin_music.enabled {
            self.music_warning.lock().ok().and_then(|slot| slot.clone())
        } else {
            None
        };
        serde_json::json!({
            "settings":settings.builtin_music,
            "running":self.running(),
            "requires_chat":true,
            "warning":warning
        })
    }
    pub fn music_save(&self, input: &Value) -> Result<Value, CenterError> {
        use moss_voice::settings::BuiltinMusic;
        let invalid = |message: &str| CenterError::new("music_invalid", 400, message);
        let config: BuiltinMusic =
            serde_json::from_value(input.get("settings").cloned().unwrap_or(Value::Null))
                .map_err(|_| invalid("本地音乐配置格式无效"))?;
        if config.directory.len() > 1024 || config.directory.chars().any(char::is_control) {
            return Err(invalid("目录太长或含非法字符"));
        }
        let track_count = if config.enabled {
            if config.directory.trim().is_empty() {
                return Err(invalid("先明确选择本地音乐目录"));
            }
            moss_voice_server::music::MusicIndex::scan(std::path::Path::new(&config.directory))
                .map_err(|_| invalid("音乐目录无法读取、路径不安全或超过索引限制"))?
                .len()
        } else {
            0
        };
        moss_voice::settings::update_voice(&self.settings_file(), |settings| {
            settings.builtin_music = config;
            Ok(())
        })
        .map_err(|_| CenterError::new("music_save_failed", 500, "音乐配置写入失败"))?;
        if let Ok(mut slot) = self.music_warning.lock() {
            *slot = None;
        }
        let restarted = self.restart_if_running()?;
        let mut result = self.music_status();
        result["restarted"] = Value::Bool(restarted);
        result["track_count"] = serde_json::json!(track_count);
        Ok(result)
    }
}
