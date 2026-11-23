//! 小智 USB 一键配网的桌面端接线：并发锁、激光串口占用判断、设备令牌密钥与诊断记录。
//! 读写 NVS、回读校验与失败恢复都在 `moss-usb-provision` crate。

use super::devices::ensure_device_token_secret;
use super::*;

/// 激光雕刻机当前（或默认）使用的串口；网络连接时为空。
fn active_laser_serial_port(host: &laser_host::HostApplication) -> Option<String> {
    if let Some(conn) = host.api_service.verified_connection() {
        let fields = conn.get("fields")?;
        let mode = fields.get("connection_mode").and_then(Value::as_str)?;
        if mode != "serial" {
            return None;
        }
        return fields
            .get("port")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    let default_mode = std::env::var("LASER_DEFAULT_CONNECTION_MODE").ok();
    if default_mode
        .as_deref()
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("network"))
    {
        return None;
    }
    std::env::var("GRBL_DEFAULT_PORT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.starts_with('<') && !s.eq_ignore_ascii_case("COMX"))
}

impl BuiltinVoice {
    pub fn usb_provision(&self, input: &Value) -> Result<Value, CenterError> {
        // 读本机 Wi-Fi 与枚举串口列表都不打开串口，不占配网锁：避免窗口聚焦自动刷新串口与读/写板子互相撞锁
        let req = moss_usb_provision::parse_usb_provision_request(input)?;
        if let Some(result) = moss_usb_provision::wifi_query(&req) {
            return result;
        }
        let _guard = if matches!(req, moss_usb_provision::UsbProvisionRequest::Ports {}) {
            None
        } else {
            Some(self.usb_provision_lock.try_lock().map_err(|_| {
                CenterError::new(
                    "usb_provision_busy",
                    409,
                    "正在读写小智板子 Flash，请等当前操作完成后再试",
                )
            })?)
        };
        let laser_port = active_laser_serial_port(&self.host);
        let laser_busy = self.host.api_service.device_work_in_progress();
        let temp_dir = self.host.runtime_root.path.join("usb-nvs-tmp");
        let db = &self.host.db;
        let device_secret = || {
            let guard = db.lock().map_err(|_| "数据库锁失败".to_owned())?;
            ensure_device_token_secret(&guard)
        };

        let (result, write_audit) = moss_usb_provision::provision(
            input,
            laser_port.as_deref(),
            laser_busy,
            db,
            &device_secret,
            temp_dir,
        )?;

        if let Some((chip_info, ota_written, auto_allowed)) = write_audit {
            let mut rec = EventRecord::info(
                DiagCategory::Device,
                format!(
                    "已通过 USB 向小智（{} · {}）写入 NVS 配置并重启",
                    chip_info.chip, chip_info.mac_address
                ),
            );
            let mut detail_parts = Vec::new();
            if let Some(ota) = ota_written {
                detail_parts.push(format!(
                    "OTA 地址：{}",
                    moss_voice_providers::scrub_diagnostic_text(&ota)
                ));
            }
            if auto_allowed {
                detail_parts.push("已预签发设备密钥并在 MOSS 设为“已允许”".to_string());
            }
            if !detail_parts.is_empty() {
                rec = rec.hint(detail_parts.join("；"));
            }
            self.diagnostics
                .push_from(moss_voice_server::DiagSource::Moss, rec);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_provision_ports_does_not_contend_with_flash_lock_while_inspect_returns_busy() {
        let temp = tempfile::tempdir().unwrap();
        let host = Arc::new(
            laser_host::HostApplication::init_with_runtime_dir(Some(temp.path())).unwrap(),
        );
        let builtin = init(Some(host)).0.unwrap();

        let _held = builtin.usb_provision_lock.lock().unwrap();

        let ports_res = builtin
            .usb_provision(&serde_json::json!({ "action": "ports" }))
            .expect("持有配网锁时枚举串口列表仍应成功，不应报 usb_provision_busy");
        assert!(ports_res["ports"].is_array(), "{ports_res}");

        let inspect_err = builtin
            .usb_provision(&serde_json::json!({ "action": "inspect", "port": "COM3" }))
            .expect_err("持有配网锁时读取板子仍须返回 usb_provision_busy");
        assert_eq!(inspect_err.code, "usb_provision_busy");
        assert_eq!(inspect_err.status, 409);
    }
}
