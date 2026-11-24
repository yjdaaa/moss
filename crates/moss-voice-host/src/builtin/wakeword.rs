//! 设备唤醒词、全屏画风资源包与全量字库的检测、导入与推送。

use super::*;

pub(super) type BasePackFpCache = (
    Option<(u64, Option<std::time::SystemTime>)>,
    moss_voice_server::wake_assets::BasePackFingerprint,
);

/// 自动保持全屏画风：这台该不该自动推送——没装好（含版本过旧、换画风、底包或字库有更新）、没有在等下载、一天内没自动推过。
/// `expected_marker` 为按该设备屏幕规格、字库与当前底包计算的完整指纹标记（`moss-screen-2:<style>:<16hex>`）。
pub(super) fn screen_pack_auto_due(
    pref: Option<&moss_voice::settings::DeviceHardwarePrefs>,
    expected_marker: &str,
    now: u64,
) -> bool {
    const DAY: u64 = 24 * 60 * 60;
    let Some(pref) = pref else {
        return true;
    };
    if moss_voice_server::screen_pack::compare_pack_marker(
        pref.screen_pack.as_deref(),
        expected_marker,
    ) == moss_voice_server::screen_pack::PackMatchState::Installed
    {
        return false;
    }
    if pref.screen_pack_pending.is_some()
        && now.saturating_sub(pref.screen_pack_pending_at.unwrap_or(0))
            <= moss_voice_server::wake_assets::STAGED_TTL.as_secs()
    {
        return false;
    }
    pref.screen_pack_auto_at
        .is_none_or(|at| now.saturating_sub(at) >= DAY)
}

fn snapshot_font_and_screen(
    dev_row: Option<&laser_runtime::VoiceDeviceRecord>,
) -> (Option<(u32, u8)>, Option<(u32, u32)>, bool) {
    let Some(dev_row) = dev_row else {
        return (None, None, false);
    };
    let snap_font = dev_row
        .hello_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| {
            let size = u32::try_from(v["text_font"]["size"].as_u64()?).ok()?;
            let bpp = u8::try_from(v["text_font"]["bpp"].as_u64()?).ok()?;
            Some((size, bpp))
        });
    let (snap_screen, snap_mono) = dev_row
        .info_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .map(|v| {
            let w = v["display"]["width"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok());
            let h = v["display"]["height"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok());
            let mono = v["display"]["monochrome"].as_bool().unwrap_or(false);
            (w.zip(h), mono)
        })
        .unwrap_or((None, false));
    (snap_font, snap_screen, snap_mono)
}

/// 统一计算某台设备当前期望的画风/字库资源包标记（`wakeword_status` 与 `screen_pack_auto_tick` 共用）。
pub(super) fn expected_device_screen_pack_marker(
    style: moss_voice_server::ScreenCardStyle,
    live_font: Option<(u32, u8)>,
    live_screen: Option<(u32, u32)>,
    live_mono: bool,
    dev_row: Option<&laser_runtime::VoiceDeviceRecord>,
    base_fp: moss_voice_server::wake_assets::BasePackFingerprint,
    installed_marker: Option<&str>,
) -> String {
    let (snap_font, snap_screen, snap_mono) = snapshot_font_and_screen(dev_row);
    let preferred_font = live_font.or(snap_font);
    let eff_screen = live_screen.or(snap_screen);
    let eff_mono = live_mono || snap_mono;
    let pack_screen = eff_screen.unwrap_or((320, 240));
    let fallback_font =
        moss_voice_server::glyph_font::select_pack_font(preferred_font, eff_screen, eff_mono);
    let primary = moss_voice_server::screen_pack::pack_marker_for(
        style,
        pack_screen,
        Some(fallback_font),
        base_fp,
    )
    .unwrap_or_else(|_| moss_voice_server::screen_pack::pack_marker(style));
    // 兼容超大屏打包因 30_4 超过 8 MB 分区自动回退到 20_4 的情形（含早期固件不报 hello.text_font、
    // 常连模式重启后尚未唤醒导致 hello_json 快照仍留 30_4、或固件上报非标准字号）：
    // 若已装标记与当前画风 + 屏幕尺寸 + 底包 + 20_4 指纹精确吻合，则视为最新，避免重启后误判过期循环重刷
    if fallback_font.size == 30
        && !base_fp.has_text_font
        && let Some(inst) = installed_marker
        && inst != primary
        && let Ok(fallback_20) = moss_voice_server::screen_pack::pack_marker_for(
            style,
            pack_screen,
            Some(moss_voice_server::glyph_font::PROFILES[2]),
            base_fp,
        )
        && inst == fallback_20
    {
        return fallback_20;
    }
    primary
}

/// 默认规格（`320×240` + `20_4` + 无底包）下当前画风应装的标记（供单元测试直接断言）。
#[cfg(test)]
pub(super) fn current_screen_pack_marker(settings: &moss_voice::VoiceSettings) -> String {
    moss_voice_server::screen_pack::pack_marker(moss_voice_server::ScreenCardStyle::from_setting(
        &settings.builtin_local.screen_card_style,
    ))
}

impl BuiltinVoice {
    pub(super) fn full_font_dir(&self) -> std::path::PathBuf {
        let spec = local_models::spec(local_models::XIAOZHI_NOTO_V1_FULL_ID)
            .expect("xiaozhi_noto_v1_full spec");
        local_models::install_path(
            self.models.root_for(local_models::XIAOZHI_NOTO_V1_FULL_ID),
            spec,
        )
    }

    pub(super) fn sync_running_full_font_dir(&self) {
        let dir = self.full_font_dir();
        if let Ok(guard) = self.server.lock()
            && let Some(server) = guard.as_ref()
        {
            server.set_full_font_dir(Some(dir));
        }
    }

    /// n111（2026-11-24 用户决定）：补字库不再是可选模型，界面不列出也不能取消；
    /// 删掉 n83 留下的“用户取消 / 删除后不再自动下载”标记，让它照常自动补齐。
    fn clear_legacy_full_font_dismissed(&self) {
        for root in self.models.all_roots() {
            let _ = std::fs::remove_file(root.join(".noto_v1_full_dismissed"));
        }
    }

    /// n83：内置语音服务启动后，若小智官方全量补字库（`noto-v1-full`，16.6 MB）尚未安装，
    /// 自动在后台校验下载并解压；未下完或断网时完全不阻塞服务启动，下载就位后热生效，没下成下次启动再试。
    /// n111 起不再理会旧的“已取消”标记（界面已不列出补字库）。
    #[cfg_attr(test, allow(dead_code))]
    pub(super) fn ensure_full_font_ready_with_base(&self, base_override: Option<&str>) -> bool {
        if self
            .models
            .path_if_installed(local_models::XIAOZHI_NOTO_V1_FULL_ID)
            .is_some()
        {
            return false;
        }
        self.clear_legacy_full_font_dismissed();
        let mirror = self.settings().builtin_local.mirror;
        let base = base_override
            .or(mirror.as_deref())
            .unwrap_or(local_models::DEFAULT_XIAOZHI_FONTS_BASE);
        self.models
            .download(local_models::XIAOZHI_NOTO_V1_FULL_ID, base)
            .is_ok()
    }

    pub(super) fn wakeword_dir(&self) -> std::path::PathBuf {
        self.host.runtime_root.path.join("wakeword")
    }

    /// n61：当前保存的自定义唤醒词；没保存过或文件损坏时用“你好 MOSS”。
    fn wakeword_word(&self) -> moss_voice_server::wake_assets::WakeWord {
        use moss_voice_server::wake_assets::{DEFAULT_THRESHOLD, WakeWord};
        std::fs::read(self.wakeword_dir().join("settings.json"))
            .ok()
            .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
            .and_then(|v| {
                WakeWord::new(
                    v["text"].as_str()?,
                    v["command"].as_str()?,
                    v["threshold"].as_f64()?,
                )
                .ok()
            })
            .unwrap_or_else(|| {
                WakeWord::new("你好 MOSS", "ni hao mo si", DEFAULT_THRESHOLD)
                    .expect("default wake word is valid")
            })
    }

    fn wakeword_base(&self) -> Option<Vec<u8>> {
        std::fs::read(self.wakeword_dir().join("base-assets.bin")).ok()
    }

    pub(super) fn base_pack_fingerprint(
        &self,
    ) -> moss_voice_server::wake_assets::BasePackFingerprint {
        let path = self.wakeword_dir().join("base-assets.bin");
        let stamp = std::fs::metadata(&path)
            .ok()
            .map(|m| (m.len(), m.modified().ok()));
        if let Ok(guard) = self.wakeword_base_fp_cache.lock()
            && let Some((cached_stamp, fp)) = *guard
            && cached_stamp == stamp
        {
            return fp;
        }
        let (bytes, cacheable) = match stamp {
            Some(_) => match std::fs::read(&path) {
                Ok(raw) => (Some(raw), true),
                Err(_) => (None, false),
            },
            None => (None, true),
        };
        let fp = moss_voice_server::wake_assets::fingerprint_base_pack(bytes.as_deref());
        if let Ok(mut guard) = self.wakeword_base_fp_cache.lock() {
            *guard = cacheable.then_some((stamp, fp));
        }
        fp
    }

    pub(super) fn reset_screen_pack_auto_throttle(&self) {
        let _ = moss_voice::settings::update_voice(&self.settings_file(), |s| {
            for entry in s.builtin_device_prefs.values_mut() {
                entry.screen_pack_auto_at = None;
            }
            Ok(())
        });
    }

    fn wakeword_write(&self, name: &str, bytes: &[u8]) -> Result<(), CenterError> {
        let dir = self.wakeword_dir();
        std::fs::create_dir_all(&dir).map_err(|e| internal(format!("创建唤醒词目录失败：{e}")))?;
        let temp = dir.join(format!(".{name}.part"));
        std::fs::write(&temp, bytes)
            .and_then(|()| std::fs::rename(&temp, dir.join(name)))
            .map_err(|e| {
                let _ = std::fs::remove_file(&temp);
                internal(format!("保存失败：{e}"))
            })
    }

    /// n61 设备唤醒词：当前设置、基础资源包摘要、在线且已允许的设备及其固件是否支持网络更新资源包。
    pub fn wakeword_status(&self) -> Result<Value, CenterError> {
        let word = self.wakeword_word();
        let base = self
            .wakeword_base()
            .and_then(|b| moss_voice_server::wake_assets::describe_base(&b).ok());
        let base_fp = self.base_pack_fingerprint();
        let allowed: std::collections::HashMap<String, laser_runtime::VoiceDeviceRecord> = self
            .host
            .db
            .lock()
            .map_err(|_| internal("数据库锁失败"))?
            .voice_devices()
            .map_err(|_| internal("读取设备失败"))?
            .into_iter()
            .filter(|d| d.is_allowed())
            .map(|d| (d.device_id.clone(), d))
            .collect();
        let sessions = {
            let guard = self.server.lock().map_err(|_| internal("语音服务锁失败"))?;
            if let Some(server) = guard.as_ref() {
                let current = server.active_device_sessions();
                if current.iter().any(|r| {
                    allowed.contains_key(&r.device_id)
                        && r.device_mcp_enabled
                        && !r.device_tools_ready
                }) {
                    server.sync_device_tools();
                }
                current
            } else {
                Vec::new()
            }
        };
        let settings = moss_voice::settings::load(&self.settings_file());
        let style = moss_voice_server::ScreenCardStyle::from_setting(
            &settings.builtin_local.screen_card_style,
        );
        let prefs = settings.builtin_device_prefs;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let devices = sessions
                .iter()
                .filter(|r| allowed.contains_key(&r.device_id))
                .map(|r| {
                    let dev_row = allowed.get(&r.device_id);
                    let screen = (r.screen_monochrome != Some(true))
                        .then_some(r.screen_width.zip(r.screen_height))
                        .flatten();
                    let pref = prefs.get(&r.device_id);
                    let installed_marker = pref.and_then(|p| p.screen_pack.as_deref());
                    let expected_marker = expected_device_screen_pack_marker(
                        style,
                        r.text_font_size.zip(r.text_font_bpp),
                        r.screen_width.zip(r.screen_height),
                        r.screen_monochrome == Some(true),
                        dev_row,
                        base_fp,
                        installed_marker,
                    );
                    let match_state = moss_voice_server::screen_pack::compare_pack_marker(
                        installed_marker,
                        &expected_marker,
                    );
                    let installed = installed_marker
                        .is_some_and(moss_voice_server::screen_pack::marker_installed);
                    let pending = pref.and_then(|p| p.screen_pack_pending.as_deref());
                    let pending_at = pref.and_then(|p| p.screen_pack_pending_at).unwrap_or(0);
                    let expired = pending.is_some()
                        && now.saturating_sub(pending_at)
                            > moss_voice_server::wake_assets::STAGED_TTL.as_secs();
                    // 2026-11-09 / n84：画风包状态以“设备整包下载完”为准，按版本前缀 + 内容指纹细分为
                    // installed / style_outdated / assets_outdated / font_outdated / none
                    let screen_pack_state = match (pending, expired) {
                        (Some(_), true) => "download_expired",
                        (Some(_), false) => "pending_download",
                        (None, _) => match match_state {
                            moss_voice_server::screen_pack::PackMatchState::Installed => {
                                "installed"
                            }
                            moss_voice_server::screen_pack::PackMatchState::StyleOutdated => {
                                "style_outdated"
                            }
                            moss_voice_server::screen_pack::PackMatchState::AssetsOutdated => {
                                "assets_outdated"
                            }
                            moss_voice_server::screen_pack::PackMatchState::FontOutdated => {
                                "font_outdated"
                            }
                            moss_voice_server::screen_pack::PackMatchState::None => "none",
                        },
                    };
                    serde_json::json!({
                        "device_id":r.device_id,"name":dev_row.map(|d| &d.name),
                        "supported":r.device_tools.iter().any(|t|t=="self.assets.set_download_url"),
                        "tools_loading":r.device_mcp_enabled && !r.device_tools_ready,
                        "screen_pack_size":screen.map(|(w,h)|[w,h]),
                        "screen_pack":installed,
                        "screen_pack_state":screen_pack_state,
                        "screen_pack_style":installed_marker
                            .filter(|_| installed)
                            .and_then(moss_voice_server::screen_pack::parse_pack_marker)
                            .map(|p| p.style),
                        "screen_pack_pending_with_pack":pending.map(|p| p != moss_voice::settings::SCREEN_PACK_NONE),
                        "wake_target":pref.and_then(|p| p.wake_target.clone()),
                    })
                })
                .collect::<Vec<_>>();
        Ok(serde_json::json!({
            "word":{"text":word.text,"command":word.command,"threshold":word.threshold},
            "base":base,"devices":devices,
            "running":self.server.lock().map_err(|_|internal("语音服务锁失败"))?.is_some(),
        }))
    }

    /// 导入基础资源包（官方网页生成器导出的 assets.bin）：推送时保留它的字体、表情和背景，只换唤醒词。
    pub fn wakeword_import_base(&self, path: &std::path::Path) -> Result<Value, CenterError> {
        use moss_voice_server::wake_assets::{
            ASSETS_MAX_BYTES, describe_base, fingerprint_base_pack,
        };
        let invalid = |message: &str| CenterError::new("wakeword_invalid", 400, message);
        let meta = std::fs::metadata(path).map_err(|_| invalid("无法读取所选文件"))?;
        if !meta.is_file() || meta.len() as usize > ASSETS_MAX_BYTES {
            return Err(invalid("请选择 8 MB 以内的资源包（assets.bin）"));
        }
        let bytes = std::fs::read(path).map_err(|_| invalid("无法读取所选文件"))?;
        let summary = describe_base(&bytes).map_err(|e| invalid(&e))?;
        let old_fp = self.base_pack_fingerprint();
        let new_fp = fingerprint_base_pack(Some(&bytes));
        self.wakeword_write("base-assets.bin", &bytes)?;
        let stamp = std::fs::metadata(self.wakeword_dir().join("base-assets.bin"))
            .ok()
            .map(|m| (m.len(), m.modified().ok()));
        if let Ok(mut guard) = self.wakeword_base_fp_cache.lock() {
            *guard = Some((stamp, new_fp));
        }
        if new_fp != old_fp {
            self.reset_screen_pack_auto_throttle();
        }
        Ok(summary)
    }

    /// 保存唤醒词 / 移除基础资源包 / 逐台确认推送（自定义唤醒词或恢复“你好小智”）。
    /// 自动保持全屏画风（2026-11-09 用户决定，开关默认关）：开着时，挑一台空闲、彩屏、支持在线更新资源包、
    /// 还没装好画风包（或版本/画风/底包/字库与期望不一致）的在线小智自动推送，沿用它上次的唤醒词。加工中、有待批准请求时不推；
    /// 每台一天最多自动推一次（失败也不反复重试），结果写进“记录与诊断”（来源 MOSS 软件）。返回推送了哪台。
    pub(super) fn screen_pack_auto_tick(&self, now: u64) -> Option<String> {
        let settings = self.settings();
        if !settings.builtin_local.screen_pack_auto_keep {
            return None;
        }
        if self.host.api_service.device_work_in_progress()
            || self
                .host
                .api_service
                .pending_approval_display_summary()
                .is_some()
        {
            return None;
        }
        let sessions = self
            .server
            .lock()
            .ok()?
            .as_ref()
            .map(VoiceServer::active_device_sessions)?;
        let allowed: std::collections::HashMap<String, laser_runtime::VoiceDeviceRecord> = self
            .host
            .db
            .lock()
            .ok()?
            .voice_devices()
            .ok()?
            .into_iter()
            .filter(|d| d.is_allowed())
            .map(|d| (d.device_id.clone(), d))
            .collect();
        let style = moss_voice_server::ScreenCardStyle::from_setting(
            &settings.builtin_local.screen_card_style,
        );
        let base_fp = self.base_pack_fingerprint();
        let candidate = sessions.into_iter().find(|r| {
            let Some(dev_row) = allowed.get(&r.device_id) else {
                return false;
            };
            if r.state != "idle"
                || !r
                    .device_tools
                    .iter()
                    .any(|t| t == "self.assets.set_download_url")
                || r.screen_monochrome == Some(true)
                || r.screen_width.zip(r.screen_height).is_none()
            {
                return false;
            }
            let pref = settings.builtin_device_prefs.get(&r.device_id);
            let expected_marker = expected_device_screen_pack_marker(
                style,
                r.text_font_size.zip(r.text_font_bpp),
                r.screen_width.zip(r.screen_height),
                false,
                Some(dev_row),
                base_fp,
                pref.and_then(|p| p.screen_pack.as_deref()),
            );
            screen_pack_auto_due(pref, &expected_marker, now)
        })?;
        let device_id = candidate.device_id;
        let name = allowed
            .get(&device_id)
            .map(|d| d.name.as_str())
            .filter(|name| !name.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| device_id.clone());
        let target = match settings
            .builtin_device_prefs
            .get(&device_id)
            .and_then(|p| p.wake_target.as_deref())
        {
            Some("custom") => "custom",
            _ => "default",
        };
        // 先记下这次尝试，失败也不会每 20 秒重推、反复重启设备
        let _ = moss_voice::settings::update_voice(&self.settings_file(), |s| {
            s.builtin_device_prefs
                .entry(device_id.clone())
                .or_default()
                .screen_pack_auto_at = Some(now);
            Ok(())
        });
        let record = match self.push_device_assets(&device_id, target, true) {
            Ok(size) => EventRecord::info(
                DiagCategory::Firmware,
                format!(
                    "自动保持全屏画风：已给 {name} 推送全屏画风（{:.1} MB），它会重启并下载",
                    size as f64 / 1_048_576.0
                ),
            )
            .hint("下载完设备列表显示“已刷全屏画风”，之后上电就是全屏画面；期间不要断电"),
            Err(error) => EventRecord::warn(
                DiagCategory::Firmware,
                format!("自动保持全屏画风：给 {name} 推送没成功：{}", error.message),
            )
            .hint("一天后会再自动试一次；也可以在“设备 › 唤醒词”手动一键刷入"),
        };
        self.diagnostics
            .push_from(moss_voice_server::DiagSource::Moss, record);
        Some(device_id)
    }

    /// 给一台已允许的在线小智推送资源包（唤醒词，可带全屏画风）：生成、下发下载地址并让它重启，
    /// 设置里记为“等下载”。手动推送（逐台确认）与“自动保持全屏画风”共用；返回资源包字节数。
    fn push_device_assets(
        &self,
        device_id: &str,
        target_name: &str,
        with_screen: bool,
    ) -> Result<usize, CenterError> {
        use moss_voice_server::wake_assets::WakeTarget;
        let target = match target_name {
            "custom" => WakeTarget::Custom(self.wakeword_word()),
            _ => WakeTarget::Default,
        };
        let Some(dev_row) = self
            .host
            .db
            .lock()
            .map_err(|_| internal("数据库锁失败"))?
            .voice_device(device_id)
            .map_err(|_| internal("读取设备失败"))?
            .filter(|d| d.is_allowed())
        else {
            return Err(CenterError::new(
                "wakeword_device_invalid",
                409,
                "只能推送给已允许的设备",
            ));
        };
        let (snap_font, snap_screen, snap_mono) = snapshot_font_and_screen(Some(&dev_row));
        let (live_font, live_screen, live_mono, pack_size, style) = {
            let server = self.server.lock().map_err(|_| internal("语音服务锁失败"))?;
            let server = server.as_ref().ok_or_else(|| {
                CenterError::new("voice_builtin_stopped", 409, "内置语音服务未运行")
            })?;
            let (lf, ls, lm) = server.device_font_context(device_id);
            (
                lf,
                ls,
                lm,
                server.screen_pack_size(device_id),
                server.screen_card_style(),
            )
        };
        let fallback_font = moss_voice_server::glyph_font::select_pack_font(
            live_font.or(snap_font),
            live_screen.or(snap_screen),
            live_mono || snap_mono,
        );
        // n70 全屏画风：按这台设备的屏幕分辨率和当前画风生成，随唤醒词一起写进资源分区
        let screen = if with_screen {
            let size = pack_size.ok_or_else(|| {
                    CenterError::new(
                        "screen_pack_unsupported",
                        409,
                        "这台设备没有上报彩屏分辨率（或是单色屏），不能刷全屏画风；可以取消勾选只推唤醒词",
                    )
                })?;
            let extras = moss_voice_server::screen_pack::build_screen_extras(style, size)
                .map_err(internal)?;
            Some((extras, style, size))
        } else {
            None
        };
        let base_bytes = self.wakeword_base();
        let (package, used_fallback_font) =
            moss_voice_server::wake_assets::build_package_with_screen_and_font_info(
                base_bytes.as_deref(),
                &target,
                screen.as_ref().map(|(extras, _, _)| extras),
                Some(fallback_font),
            )
            .map_err(|e| CenterError::new("wakeword_invalid", 400, e))?;
        // 2026-11-09 / n84：先记为“已推送、等下载”，标记附带实际打包字库与底包内容指纹；
        // 设备整包下载完（`assets_downloaded`）才算已装，不带画风的包下载完清掉已装标记。
        let marker = if let Some((_, style, screen_size)) = &screen {
            let base_fp =
                moss_voice_server::wake_assets::fingerprint_base_pack(base_bytes.as_deref());
            moss_voice_server::screen_pack::pack_marker_for(
                *style,
                *screen_size,
                used_fallback_font,
                base_fp,
            )
            .map_err(internal)?
        } else {
            moss_voice::settings::SCREEN_PACK_NONE.to_owned()
        };
        let size = package.len();
        {
            let server = self.server.lock().map_err(|_| internal("语音服务锁失败"))?;
            let server = server.as_ref().ok_or_else(|| {
                CenterError::new("voice_builtin_stopped", 409, "内置语音服务未运行")
            })?;
            server
                .install_wake_assets(device_id, package)
                .map_err(|e| CenterError::new("wakeword_device_unsupported", 409, e))?;
        }
        let wake_target = Some(target_name.to_owned());
        moss_voice::settings::update_voice(&self.settings_file(), |s| {
            let entry = s
                .builtin_device_prefs
                .entry(device_id.to_owned())
                .or_default();
            entry.screen_pack_pending = Some(marker.clone());
            entry.screen_pack_pending_at = Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
            entry.wake_target = wake_target.clone();
            Ok(())
        })
        .map_err(|msg| CenterError::new("voice_settings_write_failed", 500, msg))?;
        Ok(size)
    }

    pub fn wakeword_manage(&self, input: &Value) -> Result<Value, CenterError> {
        use moss_voice_server::wake_assets::{WakeTarget, WakeWord};
        let invalid = || CenterError::new("wakeword_invalid", 400, "唤醒词操作格式无效");
        let object = input.as_object().ok_or_else(invalid)?;
        let action = input["action"].as_str().ok_or_else(invalid)?;
        let allowed: &[&str] = match action {
            "save" => &["action", "text", "command", "threshold"],
            "clear_base" => &["action", "confirmed"],
            "push" => &["action", "device_id", "target", "confirmed", "screen_pack"],
            _ => return Err(invalid()),
        };
        // n70：push 的 screen_pack 可省略（省略即不带全屏画风），其余字段必须齐全
        let optional = usize::from(action == "push" && !object.contains_key("screen_pack"));
        if object.len() + optional != allowed.len()
            || object.keys().any(|k| !allowed.contains(&k.as_str()))
        {
            return Err(invalid());
        }
        match action {
            "save" => {
                let word = WakeWord::new(
                    input["text"].as_str().ok_or_else(invalid)?,
                    input["command"].as_str().ok_or_else(invalid)?,
                    input["threshold"].as_f64().ok_or_else(invalid)?,
                )
                .map_err(|e| CenterError::new("wakeword_invalid", 400, e))?;
                let raw=serde_json::to_vec(&serde_json::json!({"text":word.text,"command":word.command,"threshold":word.threshold})).map_err(|_|internal("保存唤醒词失败"))?;
                self.wakeword_write("settings.json", &raw)?;
            }
            "clear_base" => {
                if input["confirmed"].as_bool() != Some(true) {
                    return Err(invalid());
                }
                let old_fp = self.base_pack_fingerprint();
                let none_fp = moss_voice_server::wake_assets::fingerprint_base_pack(None);
                match std::fs::remove_file(self.wakeword_dir().join("base-assets.bin")) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(internal(format!("移除基础资源包失败：{e}"))),
                }
                if let Ok(mut guard) = self.wakeword_base_fp_cache.lock() {
                    *guard = Some((None, none_fp));
                }
                if old_fp != none_fp {
                    self.reset_screen_pack_auto_throttle();
                }
            }
            "push" => {
                if input["confirmed"].as_bool() != Some(true) {
                    return Err(invalid());
                }
                let device_id = input["device_id"].as_str().ok_or_else(invalid)?;
                let target = match input["target"].as_str() {
                    Some("custom") => WakeTarget::Custom(self.wakeword_word()),
                    Some("default") => WakeTarget::Default,
                    _ => return Err(invalid()),
                };
                let with_screen = match input.get("screen_pack") {
                    None => false,
                    Some(Value::Bool(b)) => *b,
                    Some(_) => return Err(invalid()),
                };
                let target_name = if matches!(target, WakeTarget::Default) {
                    "default"
                } else {
                    "custom"
                };
                let size = self.push_device_assets(device_id, target_name, with_screen)?;
                let mut status = self.wakeword_status()?;
                status["pushed"] = serde_json::json!({"device_id":device_id,"size":size,"target":input["target"],"screen_pack":with_screen});
                return Ok(status);
            }
            _ => {}
        }
        self.wakeword_status()
    }
}
