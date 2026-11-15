//! 小智硬件 USB 串口一键配网、写 OTA 地址与预签发设备密钥（读取/重构 ESP-IDF NVS v2）。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use espflash::connection::reset::{ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use moss_xiaozhi_proto::TokenSigner;
use moss_xiaozhi_proto::nvs::{
    NvsError, NvsStore, PARTITION_TABLE_OFFSET, PARTITION_TABLE_READ_SIZE, PartitionSummary,
    XiaozhiNvsPatch, XiaozhiNvsView, apply_xiaozhi_nvs_patch, format_uuid_v4, inspect_xiaozhi_nvs,
    parse_partition_table,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use serialport::{SerialPortType, UsbPortInfo};

use super::devices::ensure_device_token_secret;
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct UsbSerialPortItem {
    pub port_name: String,
    pub description: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub serial_number: Option<String>,
    pub is_usb: bool,
    pub is_laser_port: bool,
    pub is_likely_esp: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct UsbChipInfo {
    pub chip: String,
    pub mac_address: String,
    pub flash_size: String,
    /// Flash 容量（字节）；读不到时为空，此时不做“NVS 超出容量”检查
    #[serde(default)]
    pub flash_bytes: Option<u64>,
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct KnownDeviceView {
    pub device_id: String,
    pub name: String,
    pub status: String,
    pub model: String,
    pub firmware_version: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum UsbProvisionRequest {
    // 必须写成结构化空变体，serde 内部标记枚举下的单元变体不会触发 deny_unknown_fields
    Ports {},
    Inspect {
        port: String,
    },
    Write {
        port: String,
        #[serde(default)]
        expected_mac: Option<String>,
        #[serde(default)]
        confirmed: bool,
        /// 默认关（项目规则：新的授权能力由用户显式开启）
        #[serde(default)]
        auto_allow: bool,
        /// 这台板子在 MOSS 里被撤销过时，自动允许还必须显式确认“解除撤销”
        #[serde(default)]
        unrevoke: bool,
        #[serde(default)]
        patch: XiaozhiNvsPatch,
    },
}

fn parse_usb_provision_request(input: &Value) -> Result<UsbProvisionRequest, CenterError> {
    serde_json::from_value(input.clone()).map_err(|err| {
        CenterError::new(
            "usb_input_invalid",
            400,
            format!("USB 配网请求格式无效：{err}"),
        )
    })
}

pub(super) trait UsbFlasherSession {
    fn info(&self) -> UsbChipInfo;
    fn read_region(&mut self, offset: u32, size: u32) -> Result<Vec<u8>, CenterError>;
    /// 擦除并写入；结束后芯片停在下载程序里，不复位（以便回读校验）
    fn write_region(&mut self, offset: u32, data: &[u8]) -> Result<(), CenterError>;
    /// 芯片侧对一段 Flash 计算的 MD5（下载程序的 `FLASH_MD5` 指令）
    fn flash_md5(&mut self, offset: u32, size: u32) -> Result<[u8; 16], CenterError>;
    fn finish_reset(&mut self) -> Result<(), CenterError>;
}

/// 写入 NVS 并用芯片侧 MD5 回读校验；写入出错或校验不符时把读出的原始内容写回去并再校验一次。
/// 芯片在整个过程中停在下载程序里，由调用方最后复位。
pub(super) fn write_verified<S: UsbFlasherSession>(
    session: &mut S,
    offset: u32,
    image: &[u8],
    original: &[u8],
) -> Result<(), CenterError> {
    let attempt = |session: &mut S, data: &[u8]| -> Result<(), CenterError> {
        session.write_region(offset, data)?;
        let expected: [u8; 16] = <md5::Md5 as md5::Digest>::digest(data).into();
        if session.flash_md5(offset, data.len() as u32)? != expected {
            return Err(CenterError::new(
                "usb_flash_verify_failed",
                502,
                "写入后回读校验不一致",
            ));
        }
        Ok(())
    };
    let Err(first) = attempt(session, image) else {
        return Ok(());
    };
    match attempt(session, original) {
        Ok(()) => Err(CenterError::new(
            "usb_flash_write_rolled_back",
            502,
            format!(
                "写入没有成功（{}），已把板子原来的配置写回并校验无误，板子保持原样；请检查数据线后重试",
                first.message
            ),
        )),
        Err(restore) => Err(CenterError::new(
            "usb_flash_restore_failed",
            502,
            format!(
                "写入没有成功（{}），把原配置写回也失败了（{}）：板子的 Wi-Fi 等配置可能已损坏，开机时固件会清空 NVS。请换一根数据线、按住 BOOT 键后重新配网",
                first.message, restore.message
            ),
        )),
    }
}

struct TempFileGuard(PathBuf);

fn zero_and_remove_temp_file(path: &std::path::Path) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return;
    }
    let len = (meta.len() as usize).min(256 * 1024);
    if len > 0
        && let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(path)
    {
        use std::io::{Seek, SeekFrom, Write};
        let _ = file.seek(SeekFrom::Start(0));
        let zeros = vec![0u8; len];
        let _ = file.write_all(&zeros);
        let _ = file.sync_all();
    }
    let _ = std::fs::remove_file(path);
}

pub(super) fn sweep_usb_temp_dir(temp_dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(temp_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("usb-read-") && name_str.ends_with(".bin") {
            zero_and_remove_temp_file(&entry.path());
        }
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        zero_and_remove_temp_file(&self.0);
    }
}

struct RealEspflashSession {
    flasher: Flasher,
    info: UsbChipInfo,
    temp_dir: PathBuf,
}

impl RealEspflashSession {
    fn open(port_name: &str, temp_dir: PathBuf) -> Result<Self, CenterError> {
        let port_info = find_usb_port_info(port_name);
        let connect_with = |before: ResetBeforeOperation| -> Result<Flasher, CenterError> {
            let serial = serialport::new(port_name, 115_200)
                .flow_control(serialport::FlowControl::None)
                .open_native()
                .map_err(|err| {
                    CenterError::new(
                        "usb_serial_open_failed",
                        400,
                        format!("打不开串口 {port_name}：{err}（请确认没有其他串口监视器占用）"),
                    )
                })?;
            Flasher::connect(
                serial,
                port_info.clone(),
                None,
                true, // read_flash 需要加载 RAM stub
                false,
                false,
                None,
                // 写完停在下载程序里：先回读校验，失败还要写回原配置，最后由 finish_reset 复位
                ResetAfterOperation::NoResetNoStub,
                before,
            )
            .map_err(|err| {
                CenterError::new(
                    "usb_esp_connect_failed",
                    502,
                    format!(
                        "未能握手小智芯片（{err}）；若板子未自动进下载，请按住 BOOT 键再点重试"
                    ),
                )
            })
        };

        let mut flasher = match connect_with(ResetBeforeOperation::DefaultReset) {
            Ok(f) => f,
            Err(_) => connect_with(ResetBeforeOperation::UsbReset)?,
        };
        let raw_info = flasher.device_info().map_err(|err| {
            CenterError::new(
                "usb_chip_info_failed",
                502,
                format!("读取芯片信息失败：{err}"),
            )
        })?;
        let info = UsbChipInfo {
            chip: raw_info.chip.to_string(),
            mac_address: raw_info.mac_address.trim().to_ascii_lowercase(),
            flash_size: raw_info.flash_size.to_string(),
            flash_bytes: Some(u64::from(raw_info.flash_size.size())),
            revision: raw_info.revision.map(|(maj, min)| format!("v{maj}.{min}")),
        };
        Ok(Self {
            flasher,
            info,
            temp_dir,
        })
    }
}

impl UsbFlasherSession for RealEspflashSession {
    fn info(&self) -> UsbChipInfo {
        self.info.clone()
    }

    fn read_region(&mut self, offset: u32, size: u32) -> Result<Vec<u8>, CenterError> {
        std::fs::create_dir_all(&self.temp_dir).map_err(|err| {
            CenterError::new(
                "usb_temp_dir_failed",
                500,
                format!("创建 USB 配网临时目录失败：{err}"),
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&self.temp_dir, std::fs::Permissions::from_mode(0o700));
        }
        sweep_usb_temp_dir(&self.temp_dir);
        let mut nonce = [0u8; 8];
        let _ = getrandom::fill(&mut nonce);
        let tag: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let temp_path = self.temp_dir.join(format!("usb-read-{tag}.bin"));
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            opts.open(&temp_path).map_err(|err| {
                CenterError::new(
                    "usb_temp_dir_failed",
                    500,
                    format!("预创建 USB 临时镜像失败：{err}"),
                )
            })?;
        }
        let _cleanup = TempFileGuard(temp_path.clone());

        self.flasher
            .read_flash(offset, size, 0x1000, 64, temp_path.clone())
            .map_err(|err| {
                CenterError::new(
                    "usb_flash_read_failed",
                    502,
                    format!("从 Flash 0x{offset:x} 读取 {size} 字节失败：{err}"),
                )
            })?;
        std::fs::read(&temp_path).map_err(|err| {
            CenterError::new(
                "usb_flash_read_failed",
                500,
                format!("读取 Flash 临时缓冲失败：{err}"),
            )
        })
    }

    fn write_region(&mut self, offset: u32, data: &[u8]) -> Result<(), CenterError> {
        self.flasher
            .write_bin_to_flash(offset, data, None)
            .map_err(|err| {
                CenterError::new(
                    "usb_flash_write_failed",
                    502,
                    format!("写入 NVS 分区（0x{offset:x}）失败：{err}"),
                )
            })
    }

    fn flash_md5(&mut self, offset: u32, size: u32) -> Result<[u8; 16], CenterError> {
        self.flasher
            .checksum_md5(offset, size)
            .map(u128::to_be_bytes)
            .map_err(|err| {
                CenterError::new(
                    "usb_flash_verify_failed",
                    502,
                    format!("回读校验 NVS 分区（0x{offset:x}）失败：{err}"),
                )
            })
    }

    fn finish_reset(&mut self) -> Result<(), CenterError> {
        self.flasher.connection().reset().map_err(|err| {
            CenterError::new(
                "usb_chip_reset_failed",
                502,
                format!("重置板子退出下载模式失败，请手动按一下 RST 键：{err}"),
            )
        })
    }
}

fn find_usb_port_info(port_name: &str) -> UsbPortInfo {
    if let Ok(ports) = serialport::available_ports() {
        for p in ports {
            if port_matches(&p.port_name, port_name)
                && let SerialPortType::UsbPort(info) = p.port_type
            {
                return info;
            }
        }
    }
    UsbPortInfo {
        vid: 0,
        pid: 0,
        serial_number: None,
        manufacturer: None,
        product: None,
    }
}

fn normalize_port_cmp(name: &str) -> String {
    let trimmed = name.trim();
    if cfg!(target_os = "windows") {
        trimmed
            .strip_prefix(r"\\.\")
            .unwrap_or(trimmed)
            .to_ascii_uppercase()
    } else if let Some(rest) = trimmed.strip_prefix("/dev/tty.") {
        // macOS 同一个 USB 串口有 /dev/tty.X 与 /dev/cu.X 两个名字：激光机可能用 tty.X 连着，
        // 配网列表只显示 cu.X，两者必须认作同一个口，才能拦住对激光控制器的复位
        format!("/dev/cu.{rest}")
    } else {
        trimmed.to_string()
    }
}

pub(super) fn port_matches(a: &str, b: &str) -> bool {
    let na = normalize_port_cmp(a);
    let nb = normalize_port_cmp(b);
    !na.is_empty() && na == nb
}

fn is_likely_esp_usb(port_name: &str, vid: Option<u16>, desc: &str) -> bool {
    if matches!(
        vid,
        Some(0x303A) | Some(0x10C4) | Some(0x1A86) | Some(0x0403) | Some(0x067B)
    ) {
        return true;
    }
    let lower_name = port_name.to_ascii_lowercase();
    let lower_desc = desc.to_ascii_lowercase();
    lower_name.contains("usbmodem")
        || lower_name.contains("usbserial")
        || lower_name.contains("ttyacm")
        || lower_name.contains("ttyusb")
        || lower_desc.contains("esp")
        || lower_desc.contains("jtag")
        || lower_desc.contains("cp210")
        || lower_desc.contains("ch34")
        || lower_desc.contains("ch910")
}

pub(super) fn collect_serial_ports(
    raw_ports: Vec<serialport::SerialPortInfo>,
    active_laser_port: Option<&str>,
) -> Vec<UsbSerialPortItem> {
    let cu_names: HashSet<String> = raw_ports
        .iter()
        .filter_map(|p| p.port_name.strip_prefix("/dev/cu.").map(str::to_string))
        .collect();

    let mut items = Vec::new();
    for p in raw_ports {
        let lower = p.port_name.to_ascii_lowercase();
        if lower.contains("bluetooth-incoming-port") || lower.contains("debug-console") {
            continue;
        }
        // macOS 上同时出现 /dev/cu.* 和 /dev/tty.* 时只保留 /dev/cu.*，避免打开 tty.* 卡在载波检测
        if let Some(rest) = p.port_name.strip_prefix("/dev/tty.")
            && cu_names.contains(rest)
        {
            continue;
        }

        let (vid, pid, serial_number, description, is_usb) = match p.port_type {
            SerialPortType::UsbPort(info) => {
                let mut parts = Vec::new();
                if let Some(prod) = info
                    .product
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    parts.push(prod.to_string());
                } else if let Some(mfg) = info
                    .manufacturer
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    parts.push(mfg.to_string());
                }
                parts.push(format!("{:04X}:{:04X}", info.vid, info.pid));
                (
                    Some(info.vid),
                    Some(info.pid),
                    info.serial_number,
                    parts.join(" · "),
                    true,
                )
            }
            SerialPortType::PciPort => (None, None, None, "PCI 串口".to_string(), false),
            SerialPortType::BluetoothPort => continue,
            SerialPortType::Unknown => (None, None, None, "串口设备".to_string(), false),
        };

        let is_laser_port =
            active_laser_port.is_some_and(|laser| port_matches(&p.port_name, laser));
        let is_likely_esp = is_likely_esp_usb(&p.port_name, vid, &description);
        items.push(UsbSerialPortItem {
            port_name: p.port_name,
            description,
            vid,
            pid,
            serial_number,
            is_usb,
            is_laser_port,
            is_likely_esp,
        });
    }

    // 乐鑫原生 USB（VID 0x303A，小智常见的 ESP32-S3 直连口）排最前；CH340 / CP210x 这类转换芯片
    // 激光控制器也常用，排在后面，避免默认选中激光机
    let espressif = |item: &UsbSerialPortItem| item.vid == Some(0x303A);
    items.sort_by(|a, b| {
        espressif(b)
            .cmp(&espressif(a))
            .then_with(|| b.is_likely_esp.cmp(&a.is_likely_esp))
            .then_with(|| a.is_laser_port.cmp(&b.is_laser_port))
            .then_with(|| b.is_usb.cmp(&a.is_usb))
            .then_with(|| a.port_name.cmp(&b.port_name))
    });
    items
}

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

fn any_firmware_downloading(db: &Mutex<laser_runtime::RuntimeDb>) -> bool {
    db.lock()
        .ok()
        .and_then(|guard| guard.has_active_firmware_download(now()).ok())
        .unwrap_or(false)
}

fn preflight_usb_port(
    port_name: &str,
    laser_port: Option<&str>,
    laser_busy: bool,
    available_ports: &[UsbSerialPortItem],
) -> Result<(), CenterError> {
    let trimmed = port_name.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return Err(CenterError::new(
            "usb_port_invalid",
            400,
            "请先选择一个有效的 USB 串口",
        ));
    }
    if laser_busy {
        return Err(CenterError::new(
            "usb_blocked_laser_busy",
            409,
            "激光雕刻机正在执行任务，为避免串口与供电干扰已禁止 USB 配网",
        ));
    }
    if let Some(laser) = laser_port
        && port_matches(trimmed, laser)
    {
        return Err(CenterError::new(
            "usb_blocked_laser_port",
            409,
            format!("串口 {trimmed} 当前正作为激光雕刻机连接端口使用，已拒绝读写"),
        ));
    }
    if !available_ports
        .iter()
        .any(|item| port_matches(&item.port_name, trimmed))
    {
        return Err(CenterError::new(
            "usb_port_not_found",
            404,
            format!("找不到串口 {trimmed}，请重新插拔数据线并刷新端口列表"),
        ));
    }
    Ok(())
}

fn nvs_err(err: NvsError) -> CenterError {
    match err {
        NvsError::EncryptedNvs(m) => CenterError::new("usb_nvs_encrypted", 409, m),
        NvsError::WrongDevicePartition(m) => CenterError::new("usb_wrong_device", 409, m),
        NvsError::Validation(m) => CenterError::new("usb_nvs_validation", 400, m),
        NvsError::CapacityExceeded(m) => CenterError::new("usb_nvs_full", 400, m),
        NvsError::InvalidPartitionTable(m) | NvsError::InvalidNvs(m) => {
            CenterError::new("usb_nvs_invalid", 422, m)
        }
    }
}

type NvsRead = (
    PartitionSummary,
    moss_xiaozhi_proto::nvs::PartitionEntry,
    NvsStore,
    Vec<u8>,
);

/// 读分区表与 NVS 分区；最后一项是 NVS 原始字节（写入失败时用来恢复）。
fn read_nvs_from_session<S: UsbFlasherSession>(session: &mut S) -> Result<NvsRead, CenterError> {
    let flash_bytes = session.info().flash_bytes;
    let part_raw = session.read_region(PARTITION_TABLE_OFFSET, PARTITION_TABLE_READ_SIZE)?;
    let summary = parse_partition_table(&part_raw).map_err(nvs_err)?;
    let nvs_part = summary
        .require_writable_nvs_within(flash_bytes)
        .map_err(nvs_err)?
        .clone();
    let nvs_raw = session.read_region(nvs_part.offset, nvs_part.size)?;
    let store = NvsStore::parse(&nvs_raw).map_err(nvs_err)?;
    Ok((summary, nvs_part, store, nvs_raw))
}

fn lookup_known_device(
    db: &Mutex<laser_runtime::RuntimeDb>,
    mac_lower: &str,
) -> Option<KnownDeviceView> {
    let guard = db.lock().ok()?;
    let rec = guard.voice_device(mac_lower).ok()??;
    Some(KnownDeviceView {
        device_id: rec.device_id,
        name: rec.name,
        status: rec.status,
        model: rec.model,
        firmware_version: rec.firmware_version,
    })
}

fn ensure_client_uuid(
    store: &NvsStore,
    db: &Mutex<laser_runtime::RuntimeDb>,
    mac_lower: &str,
) -> Result<String, CenterError> {
    if let Ok(guard) = db.lock()
        && let Ok(Some(rec)) = guard.voice_device(mac_lower)
        && !rec.client_id.trim().is_empty()
    {
        return Ok(rec.client_id);
    }
    if let Some(existing) = store
        .get_str("board", "uuid")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(existing.to_string());
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|err| {
        CenterError::new(
            "usb_uuid_gen_failed",
            500,
            format!("生成设备 UUID 失败：{err}"),
        )
    })?;
    Ok(format_uuid_v4(bytes))
}

fn execute_inspect<S: UsbFlasherSession>(
    port_name: &str,
    mut session: S,
    db: &Mutex<laser_runtime::RuntimeDb>,
) -> Result<Value, CenterError> {
    let info = session.info();
    let res = read_nvs_from_session(&mut session);
    let reset_res = session.finish_reset();
    let (summary, nvs_part, store, _) = res?;
    reset_res?;

    let view: XiaozhiNvsView = inspect_xiaozhi_nvs(&store);
    let known = lookup_known_device(db, &info.mac_address);
    Ok(json!({
        "port": port_name,
        "device": info,
        "partition": {
            "label": nvs_part.label,
            "offset": format!("0x{:x}", nvs_part.offset),
            "size": nvs_part.size,
            "all_labels": summary.all_labels,
            "has_xiaozhi_markers": summary.has_xiaozhi_markers,
        },
        "nvs": view,
        "known_device": known,
    }))
}

fn execute_write<S: UsbFlasherSession>(
    port_name: &str,
    expected_mac: Option<&str>,
    confirmed: bool,
    auto_allow: bool,
    unrevoke: bool,
    base_patch: &XiaozhiNvsPatch,
    mut session: S,
    db: &Mutex<laser_runtime::RuntimeDb>,
) -> Result<(Value, UsbChipInfo, Option<String>, bool), CenterError> {
    if !confirmed {
        return Err(CenterError::new(
            "usb_confirm_required",
            400,
            "写入 Flash 前必须完成界面二次确认",
        ));
    }
    let expected_mac = expected_mac
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            CenterError::new(
                "usb_expected_mac_required",
                400,
                "请先读取板子信息，核对芯片 MAC 后再写入",
            )
        })?;

    let info = session.info();
    if !info.mac_address.eq_ignore_ascii_case(expected_mac) {
        let _ = session.finish_reset();
        return Err(CenterError::new(
            "usb_mac_mismatch",
            409,
            format!(
                "当前串口设备 MAC（{}）与读取确认时（{}）不一致，已中止写入",
                info.mac_address, expected_mac
            ),
        ));
    }

    let read_res = read_nvs_from_session(&mut session);
    let (summary, nvs_part, mut store, nvs_raw) = match read_res {
        Ok(ok) => ok,
        Err(err) => {
            let _ = session.finish_reset();
            return Err(err);
        }
    };

    let is_xiaozhi = summary.has_xiaozhi_markers
        || store.has_namespace("board")
        || store.has_namespace("websocket");
    if !is_xiaozhi {
        let _ = session.finish_reset();
        return Err(CenterError::new(
            "usb_wrong_device",
            409,
            "未检测到小智固件分区特征（缺少 model/assets 分区及小智配置），为防误刷其他开发板已中止写入",
        ));
    }

    let client_id = match ensure_client_uuid(&store, db, &info.mac_address) {
        Ok(id) => id,
        Err(err) => {
            let _ = session.finish_reset();
            return Err(err);
        }
    };
    let mut patch = base_patch.clone();
    patch.board_uuid = Some(client_id.clone());

    let clearing_ota = patch
        .ota_url
        .as_deref()
        .is_some_and(|url| url.trim().is_empty());
    let clearing_ws = patch
        .ws_url
        .as_deref()
        .is_some_and(|url| url.trim().is_empty());
    // ESP32-P4 上报的设备号是协处理器的 Wi-Fi MAC，与这里读到的芯片 MAC 不同（固件 `system_info.cc`）：
    // 按读到的 MAC 放行会多出一条对不上的“已允许”记录，真设备仍是待允许。这类板子不自动放行。
    let mac_mismatch_chip = info.chip.eq_ignore_ascii_case("esp32p4");
    let known_status = lookup_known_device(db, &info.mac_address).map(|d| d.status);
    if auto_allow
        && !mac_mismatch_chip
        && known_status.as_deref() == Some(laser_runtime::VOICE_DEVICE_REVOKED)
        && !unrevoke
    {
        let _ = session.finish_reset();
        return Err(CenterError::new(
            "usb_device_revoked",
            409,
            "这台小智在 MOSS 里被撤销过：要自动允许，请在确认时勾选“解除撤销”；不想放行就取消勾选“自动允许”后再写入",
        ));
    }
    let bind_moss = auto_allow && !mac_mismatch_chip && !clearing_ota && !clearing_ws;
    if bind_moss {
        let ts_sec = now().max(0.0) as u64;
        let secret_res = {
            let guard = db
                .lock()
                .map_err(|_| CenterError::new("db_lock_failed", 500, "数据库锁失败"))?;
            ensure_device_token_secret(&guard)
        };
        let secret = match secret_res {
            Ok(s) => s,
            Err(msg) => {
                let _ = session.finish_reset();
                return Err(CenterError::new("usb_token_failed", 500, msg));
            }
        };
        let token = TokenSigner::new(secret).issue(&client_id, &info.mac_address, ts_sec);
        patch.ws_token = Some(token);
    } else {
        // 关闭自动允许、改填外部地址或恢复默认 OTA 时清除旧 websocket 凭据，防止重启连回旧服务
        patch.ws_token = Some(String::new());
        if patch.ws_url.is_none() || clearing_ota {
            patch.ws_url = Some(String::new());
        }
    }

    if let Err(err) = apply_xiaozhi_nvs_patch(&mut store, &patch).map_err(nvs_err) {
        let _ = session.finish_reset();
        return Err(err);
    }

    let image = match store.build(nvs_part.size as usize).map_err(nvs_err) {
        Ok(img) => img,
        Err(err) => {
            let _ = session.finish_reset();
            return Err(err);
        }
    };

    if let Err(err) = write_verified(&mut session, nvs_part.offset, &image, &nvs_raw) {
        let _ = session.finish_reset();
        return Err(err);
    }
    // 数据已写入并校验无误：复位失败只影响重启，不算写入失败（照常登记，提示用户手动按 RST）
    let reset_warning = session.finish_reset().err().map(|err| err.message);

    if bind_moss {
        let ts = now();
        let guard = db
            .lock()
            .map_err(|_| CenterError::new("db_lock_failed", 500, "数据库锁失败"))?;
        guard
            .bind_usb_voice_device(&info.mac_address, &client_id, &info.chip, ts)
            .map_err(|err| CenterError::new("usb_db_failed", 500, db_error(err)))?;
    }

    let view = inspect_xiaozhi_nvs(&store);
    let known = lookup_known_device(db, &info.mac_address);
    let ota_written = view.ota_url.clone();
    let payload = json!({
        "ok": true,
        "port": port_name,
        "device": info,
        "partition": {
            "label": nvs_part.label,
            "offset": format!("0x{:x}", nvs_part.offset),
            "size": nvs_part.size,
            "all_labels": summary.all_labels,
            "has_xiaozhi_markers": summary.has_xiaozhi_markers,
        },
        "nvs": view,
        "known_device": known,
        "auto_allowed": bind_moss,
        "auto_allow_skipped": (auto_allow && mac_mismatch_chip).then_some(
            "ESP32-P4 板子上报的设备号与芯片 MAC 不同，没有自动放行：板子连上 MOSS 后请在设备列表里手动允许",
        ),
        "reset_warning": reset_warning,
    });
    Ok((payload, info, ota_written, bind_moss))
}

pub(super) fn execute_usb_provision_with<S, F>(
    req: &UsbProvisionRequest,
    raw_ports: Vec<serialport::SerialPortInfo>,
    laser_port: Option<&str>,
    laser_busy: bool,
    db: &Mutex<laser_runtime::RuntimeDb>,
    connect_fn: F,
) -> Result<(Value, Option<(UsbChipInfo, Option<String>, bool)>), CenterError>
where
    S: UsbFlasherSession,
    F: FnOnce(&str) -> Result<S, CenterError>,
{
    let ports = collect_serial_ports(raw_ports, laser_port);
    match req {
        UsbProvisionRequest::Ports {} => {
            Ok((json!({ "ports": ports, "laser_busy": laser_busy }), None))
        }
        UsbProvisionRequest::Inspect { port } => {
            let port_name = port.trim();
            preflight_usb_port(port_name, laser_port, laser_busy, &ports)?;
            if any_firmware_downloading(db) {
                return Err(CenterError::new(
                    "usb_blocked_firmware_busy",
                    409,
                    "有小智设备正在下载升级固件，为防中断刷写已暂时禁止 USB 配网",
                ));
            }
            let session = connect_fn(port_name)?;
            let val = execute_inspect(port_name, session, db)?;
            Ok((val, None))
        }
        UsbProvisionRequest::Write {
            port,
            expected_mac,
            confirmed,
            auto_allow,
            unrevoke,
            patch,
        } => {
            let port_name = port.trim();
            preflight_usb_port(port_name, laser_port, laser_busy, &ports)?;
            if any_firmware_downloading(db) {
                return Err(CenterError::new(
                    "usb_blocked_firmware_busy",
                    409,
                    "有小智设备正在下载升级固件，为防中断刷写已暂时禁止 USB 配网",
                ));
            }
            let session = connect_fn(port_name)?;
            let (val, info, ota_written, bound) = execute_write(
                port_name,
                expected_mac.as_deref(),
                *confirmed,
                *auto_allow,
                *unrevoke,
                patch,
                session,
                db,
            )?;
            Ok((val, Some((info, ota_written, bound))))
        }
    }
}

impl BuiltinVoice {
    pub fn usb_provision(&self, input: &Value) -> Result<Value, CenterError> {
        let _guard = self.usb_provision_lock.try_lock().map_err(|_| {
            CenterError::new(
                "usb_provision_busy",
                409,
                "正在读写小智板子 Flash，请等当前操作完成后再试",
            )
        })?;
        let req = parse_usb_provision_request(input)?;
        let raw_ports = serialport::available_ports().map_err(|err| {
            CenterError::new(
                "usb_ports_enum_failed",
                500,
                format!("枚举本机串口失败：{err}"),
            )
        })?;
        let laser_port = active_laser_serial_port(&self.host);
        let laser_busy = self.host.api_service.device_work_in_progress();
        let temp_dir = self.host.runtime_root.path.join("usb-nvs-tmp");

        let (result, write_audit) = execute_usb_provision_with(
            &req,
            raw_ports,
            laser_port.as_deref(),
            laser_busy,
            &self.host.db,
            |port_name| RealEspflashSession::open(port_name, temp_dir),
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
    use moss_xiaozhi_proto::nvs::NvsValue;

    struct MockSession {
        info: UsbChipInfo,
        part_table: Vec<u8>,
        nvs_offset: u32,
        nvs_bytes: Vec<u8>,
        reset_called: bool,
        writes: usize,
    }

    impl UsbFlasherSession for MockSession {
        fn info(&self) -> UsbChipInfo {
            self.info.clone()
        }

        fn read_region(&mut self, offset: u32, size: u32) -> Result<Vec<u8>, CenterError> {
            if offset == PARTITION_TABLE_OFFSET {
                return Ok(self.part_table[..size as usize].to_vec());
            }
            if offset == self.nvs_offset {
                assert_eq!(size as usize, self.nvs_bytes.len());
                return Ok(self.nvs_bytes.clone());
            }
            Err(CenterError::new(
                "mock_bad_offset",
                400,
                "unexpected read offset",
            ))
        }

        fn write_region(&mut self, offset: u32, data: &[u8]) -> Result<(), CenterError> {
            assert_eq!(offset, self.nvs_offset);
            assert_eq!(data.len(), self.nvs_bytes.len());
            self.nvs_bytes = data.to_vec();
            self.writes += 1;
            Ok(())
        }

        fn flash_md5(&mut self, offset: u32, size: u32) -> Result<[u8; 16], CenterError> {
            assert_eq!(offset, self.nvs_offset);
            assert_eq!(size as usize, self.nvs_bytes.len());
            Ok(<md5::Md5 as md5::Digest>::digest(&self.nvs_bytes).into())
        }

        fn finish_reset(&mut self) -> Result<(), CenterError> {
            self.reset_called = true;
            Ok(())
        }
    }

    fn make_part_table(entries: &[(u8, u8, u32, u32, &str, u32)]) -> Vec<u8> {
        let mut raw = vec![0xFFu8; PARTITION_TABLE_READ_SIZE as usize];
        for (idx, (pt, st, off, sz, lbl, flags)) in entries.iter().enumerate() {
            let dst = &mut raw[idx * 32..(idx + 1) * 32];
            dst.fill(0);
            dst[0..2].copy_from_slice(&[0xAA, 0x50]);
            dst[2] = *pt;
            dst[3] = *st;
            dst[4..8].copy_from_slice(&off.to_le_bytes());
            dst[8..12].copy_from_slice(&sz.to_le_bytes());
            let lb = lbl.as_bytes();
            let n = lb.len().min(16);
            dst[12..12 + n].copy_from_slice(&lb[..n]);
            dst[28..32].copy_from_slice(&flags.to_le_bytes());
        }
        raw
    }

    fn fake_ports() -> Vec<serialport::SerialPortInfo> {
        vec![
            serialport::SerialPortInfo {
                port_name: "/dev/cu.usbmodem14201".into(),
                port_type: SerialPortType::UsbPort(UsbPortInfo {
                    vid: 0x303A,
                    pid: 0x1001,
                    serial_number: Some("SN1".into()),
                    manufacturer: Some("Espressif".into()),
                    product: Some("USB JTAG/serial debug unit".into()),
                }),
            },
            serialport::SerialPortInfo {
                port_name: "/dev/tty.usbmodem14201".into(),
                port_type: SerialPortType::UsbPort(UsbPortInfo {
                    vid: 0x303A,
                    pid: 0x1001,
                    serial_number: None,
                    manufacturer: None,
                    product: None,
                }),
            },
            serialport::SerialPortInfo {
                port_name: "/dev/cu.usbserial-LASER".into(),
                port_type: SerialPortType::UsbPort(UsbPortInfo {
                    vid: 0x1A86,
                    pid: 0x7523,
                    serial_number: None,
                    manufacturer: None,
                    product: Some("USB-SERIAL CH340".into()),
                }),
            },
        ]
    }

    #[test]
    fn ports_filters_macos_tty_duplicate_and_marks_laser_port() {
        let items = collect_serial_ports(fake_ports(), Some("/dev/cu.usbserial-LASER"));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].port_name, "/dev/cu.usbmodem14201");
        assert!(!items[0].is_laser_port);
        assert!(items[0].is_likely_esp);
        assert_eq!(items[1].port_name, "/dev/cu.usbserial-LASER");
        assert!(items[1].is_laser_port);
    }

    #[test]
    fn inspect_and_write_enforce_safety_gates_and_auto_allow_device() {
        let db = Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap());
        let part_table = make_part_table(&[
            (0x01, 0x02, 0x9000, 0x6000, "nvs", 0),
            (0x00, 0x10, 0x20000, 0x200000, "ota_0", 0),
            (0x01, 0x82, 0x600000, 0x400000, "assets", 0),
        ]);
        let mut initial_store = NvsStore::default();
        initial_store
            .set("wifi", "ssid", NvsValue::Str("OldHome".into()))
            .unwrap();
        initial_store
            .set("wifi", "password", NvsValue::Str("keep-this-secret".into()))
            .unwrap();
        initial_store
            .set("misc", "volume", NvsValue::U8(65))
            .unwrap();
        let initial_nvs = initial_store.build(0x6000).unwrap();

        let chip = UsbChipInfo {
            chip: "esp32s3".into(),
            mac_address: "dc:da:0c:12:34:56".into(),
            flash_size: "16MB".into(),
            flash_bytes: Some(16 << 20),
            revision: Some("v0.2".into()),
        };

        // 1. Refuse laser port or laser busy.
        let req_inspect: UsbProvisionRequest = serde_json::from_value(json!({
            "action": "inspect",
            "port": "/dev/cu.usbserial-LASER",
        }))
        .unwrap();
        let err = execute_usb_provision_with(
            &req_inspect,
            fake_ports(),
            Some("/dev/cu.usbserial-LASER"),
            false,
            &db,
            |_| -> Result<MockSession, CenterError> { unreachable!("must reject before opening") },
        )
        .unwrap_err();
        assert_eq!(err.code, "usb_blocked_laser_port");

        let req_busy: UsbProvisionRequest = serde_json::from_value(json!({
            "action": "inspect",
            "port": "/dev/cu.usbmodem14201",
        }))
        .unwrap();
        let err_busy = execute_usb_provision_with(
            &req_busy,
            fake_ports(),
            None,
            true,
            &db,
            |_| -> Result<MockSession, CenterError> { unreachable!("must reject when laser busy") },
        )
        .unwrap_err();
        assert_eq!(err_busy.code, "usb_blocked_laser_busy");

        // 2. Inspect hides Wi-Fi plaintext password.
        let (inspect_val, _) =
            execute_usb_provision_with(&req_busy, fake_ports(), None, false, &db, |_| {
                Ok(MockSession {
                    info: chip.clone(),
                    part_table: part_table.clone(),
                    nvs_offset: 0x9000,
                    nvs_bytes: initial_nvs.clone(),
                    reset_called: false,
                    writes: 0,
                })
            })
            .unwrap();
        let dumped = inspect_val.to_string();
        assert!(!dumped.contains("keep-this-secret"));
        assert_eq!(inspect_val["nvs"]["wifi_slots"][0]["ssid"], "OldHome");
        assert_eq!(inspect_val["nvs"]["wifi_slots"][0]["has_password"], true);

        // 3. Write requires confirmed + matching MAC, preserves old password & unrelated keys,
        //    writes websocket:token verifiable with DB secret, and auto-allows in RuntimeDb.
        let bad_mac_req: UsbProvisionRequest = serde_json::from_value(json!({
            "action": "write",
            "port": "/dev/cu.usbmodem14201",
            "expected_mac": "00:00:00:00:00:01",
            "confirmed": true,
            "patch": { "ota_url": "http://192.168.1.20:9666/xiaozhi/ota/" }
        }))
        .unwrap();
        let err_mac =
            execute_usb_provision_with(&bad_mac_req, fake_ports(), None, false, &db, |_| {
                Ok(MockSession {
                    info: chip.clone(),
                    part_table: part_table.clone(),
                    nvs_offset: 0x9000,
                    nvs_bytes: initial_nvs.clone(),
                    reset_called: false,
                    writes: 0,
                })
            })
            .unwrap_err();
        assert_eq!(err_mac.code, "usb_mac_mismatch");

        let mut shared_flash = initial_nvs.clone();
        let write_req: UsbProvisionRequest = serde_json::from_value(json!({
            "action": "write",
            "port": "/dev/cu.usbmodem14201",
            "expected_mac": "DC:DA:0C:12:34:56",
            "confirmed": true,
            "auto_allow": true,
            "patch": {
                "new_wifi": { "ssid": "Studio5G", "password": "studio-pw" },
                "ota_url": "http://192.168.1.20:9666/xiaozhi/ota/",
                "max_tx_power": 52,
                "remember_bssid": true
            }
        }))
        .unwrap();
        let (write_val, audit) =
            execute_usb_provision_with(&write_req, fake_ports(), None, false, &db, |_| {
                Ok(MockSession {
                    info: chip.clone(),
                    part_table: part_table.clone(),
                    nvs_offset: 0x9000,
                    nvs_bytes: shared_flash.clone(),
                    reset_called: false,
                    writes: 0,
                })
            })
            .unwrap();
        assert!(audit.is_some());
        assert_eq!(write_val["auto_allowed"], true);
        assert_eq!(write_val["known_device"]["status"], "allowed");

        // Re-verify built bytes directly from a second MockSession capturing write output.
        let written_bytes = {
            let mut holder = Vec::new();
            struct CapturingSession<'a> {
                inner: MockSession,
                out: &'a mut Vec<u8>,
            }
            impl UsbFlasherSession for CapturingSession<'_> {
                fn info(&self) -> UsbChipInfo {
                    self.inner.info()
                }
                fn read_region(&mut self, offset: u32, size: u32) -> Result<Vec<u8>, CenterError> {
                    self.inner.read_region(offset, size)
                }
                fn write_region(&mut self, offset: u32, data: &[u8]) -> Result<(), CenterError> {
                    self.inner.write_region(offset, data)?;
                    *self.out = data.to_vec();
                    Ok(())
                }
                fn flash_md5(&mut self, offset: u32, size: u32) -> Result<[u8; 16], CenterError> {
                    self.inner.flash_md5(offset, size)
                }
                fn finish_reset(&mut self) -> Result<(), CenterError> {
                    self.inner.finish_reset()
                }
            }
            execute_usb_provision_with(&write_req, fake_ports(), None, false, &db, |_| {
                Ok(CapturingSession {
                    inner: MockSession {
                        info: chip.clone(),
                        part_table: part_table.clone(),
                        nvs_offset: 0x9000,
                        nvs_bytes: shared_flash.clone(),
                        reset_called: false,
                        writes: 0,
                    },
                    out: &mut holder,
                })
            })
            .unwrap();
            holder
        };
        shared_flash = written_bytes;
        let final_store = NvsStore::parse(&shared_flash).unwrap();
        assert_eq!(final_store.get_str("wifi", "ssid"), Some("Studio5G"));
        assert_eq!(final_store.get_str("wifi", "ssid1"), Some("OldHome"));
        assert_eq!(
            final_store.get_str("wifi", "password1"),
            Some("keep-this-secret")
        );
        assert_eq!(
            final_store.get_str("wifi", "ota_url"),
            Some("http://192.168.1.20:9666/xiaozhi/ota/")
        );
        assert_eq!(final_store.get_u8("misc", "volume"), Some(65));

        let client_id = final_store.get_str("board", "uuid").unwrap();
        let token = final_store.get_str("websocket", "token").unwrap();
        let secret = db
            .lock()
            .unwrap()
            .get_secret(TOKEN_SECRET_NAME)
            .unwrap()
            .unwrap()
            .into_bytes();
        assert!(TokenSigner::new(secret.clone()).verify(
            token,
            client_id,
            "dc:da:0c:12:34:56",
            now() as u64 + 60
        ));

        // 4. Reflashed board with blank NVS & mismatched old UUID rebinds to DB's bound client_id.
        let mut reflashed_store = NvsStore::default();
        reflashed_store
            .set(
                "board",
                "uuid",
                NvsValue::Str("00000000-0000-4000-8000-999999999999".into()),
            )
            .unwrap();
        let reflashed_nvs = reflashed_store.build(0x6000).unwrap();
        let mut holder2 = Vec::new();
        struct Capturing<'a> {
            inner: MockSession,
            out: &'a mut Vec<u8>,
        }
        impl UsbFlasherSession for Capturing<'_> {
            fn info(&self) -> UsbChipInfo {
                self.inner.info()
            }
            fn read_region(&mut self, offset: u32, size: u32) -> Result<Vec<u8>, CenterError> {
                self.inner.read_region(offset, size)
            }
            fn write_region(&mut self, offset: u32, data: &[u8]) -> Result<(), CenterError> {
                self.inner.write_region(offset, data)?;
                *self.out = data.to_vec();
                Ok(())
            }
            fn flash_md5(&mut self, offset: u32, size: u32) -> Result<[u8; 16], CenterError> {
                self.inner.flash_md5(offset, size)
            }
            fn finish_reset(&mut self) -> Result<(), CenterError> {
                self.inner.finish_reset()
            }
        }
        execute_usb_provision_with(&write_req, fake_ports(), None, false, &db, |_| {
            Ok(Capturing {
                inner: MockSession {
                    info: chip.clone(),
                    part_table: part_table.clone(),
                    nvs_offset: 0x9000,
                    nvs_bytes: reflashed_nvs,
                    reset_called: false,
                    writes: 0,
                },
                out: &mut holder2,
            })
        })
        .unwrap();
        let rebound_store = NvsStore::parse(&holder2).unwrap();
        assert_eq!(rebound_store.get_str("board", "uuid"), Some(client_id));
        let rebound_token = rebound_store.get_str("websocket", "token").unwrap();
        assert!(TokenSigner::new(secret).verify(
            rebound_token,
            client_id,
            "dc:da:0c:12:34:56",
            now() as u64 + 60
        ));
    }

    #[test]
    fn usb_provision_request_rejects_unknown_fields_and_invalid_actions() {
        assert!(matches!(
            parse_usb_provision_request(&json!({ "action": "ports" })),
            Ok(UsbProvisionRequest::Ports {})
        ));
        for bad in [
            json!({ "action": "ports", "extra": 1 }),
            json!({ "action": "ports", "confirmed": true }),
            json!({ "action": "inspect", "port": "/dev/cu.usbmodem14201", "confirmed": true }),
            json!({ "action": "inspect" }),
            json!({ "action": "unknown" }),
            json!({
                "action": "write",
                "port": "/dev/cu.usbmodem14201",
                "unknown": true
            }),
            json!({
                "action": "write",
                "port": "/dev/cu.usbmodem14201",
                "patch": { "unknown_patch_key": 1 }
            }),
            json!({
                "action": "write",
                "port": "/dev/cu.usbmodem14201",
                "patch": {
                    "new_wifi": { "ssid": "Home", "password": "pw", "psk": "extra" }
                }
            }),
        ] {
            let err = parse_usb_provision_request(&bad).unwrap_err();
            assert_eq!(err.code, "usb_input_invalid", "payload={bad}");
        }
    }

    #[test]
    fn write_blocks_generic_non_xiaozhi_boards_and_downloading_firmware_and_clears_ws_on_opt_out() {
        let db = Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap());
        let chip = UsbChipInfo {
            chip: "esp32s3".into(),
            mac_address: "aa:11:22:33:44:55".into(),
            flash_size: "16MB".into(),
            flash_bytes: Some(16 << 20),
            revision: None,
        };

        // 1. 仅有 nvs + ota_0、无 assets/model 且 NVS 无小智命名空间的通用板拒绝写入
        let generic_table = make_part_table(&[
            (0x01, 0x02, 0x9000, 0x6000, "nvs", 0),
            (0x00, 0x10, 0x20000, 0x200000, "ota_0", 0),
        ]);
        let empty_nvs = NvsStore::default().build(0x6000).unwrap();
        let req_generic: UsbProvisionRequest = serde_json::from_value(json!({
            "action": "write",
            "port": "/dev/cu.usbmodem14201",
            "expected_mac": "aa:11:22:33:44:55",
            "confirmed": true,
            "patch": { "ota_url": "http://192.168.1.20:9666/xiaozhi/ota/" }
        }))
        .unwrap();
        let wrong =
            execute_usb_provision_with(&req_generic, fake_ports(), None, false, &db, |_| {
                Ok(MockSession {
                    info: chip.clone(),
                    part_table: generic_table.clone(),
                    nvs_offset: 0x9000,
                    nvs_bytes: empty_nvs.clone(),
                    reset_called: false,
                    writes: 0,
                })
            })
            .unwrap_err();
        assert_eq!(wrong.code, "usb_wrong_device");

        // 2. auto_allow=false 或清空 ota_url 时擦除残留的 websocket:token 与 websocket:url
        let xiaozhi_table = make_part_table(&[
            (0x01, 0x02, 0x9000, 0x6000, "nvs", 0),
            (0x01, 0x82, 0x600000, 0x400000, "assets", 0),
        ]);
        let mut stale_store = NvsStore::default();
        stale_store
            .set("board", "uuid", NvsValue::Str("uuid-keep".into()))
            .unwrap();
        stale_store
            .set(
                "websocket",
                "url",
                NvsValue::Str("ws://old.example:8000/xiaozhi/v1/".into()),
            )
            .unwrap();
        stale_store
            .set("websocket", "token", NvsValue::Str("stale-tok".into()))
            .unwrap();
        let stale_bytes = stale_store.build(0x6000).unwrap();
        // 先在库里记一笔真实板型且 client_id="" 的记录，验证重绑 + 保留真实 model
        {
            let guard = db.lock().unwrap();
            guard
                .voice_device_seen(
                    "aa:11:22:33:44:55",
                    "",
                    "bread-compact-wifi-lcd",
                    "2.1.0",
                    "",
                    10.0,
                )
                .unwrap();
        }
        let mut captured = Vec::new();
        struct Cap<'a> {
            inner: MockSession,
            out: &'a mut Vec<u8>,
        }
        impl UsbFlasherSession for Cap<'_> {
            fn info(&self) -> UsbChipInfo {
                self.inner.info()
            }
            fn read_region(&mut self, offset: u32, size: u32) -> Result<Vec<u8>, CenterError> {
                self.inner.read_region(offset, size)
            }
            fn write_region(&mut self, offset: u32, data: &[u8]) -> Result<(), CenterError> {
                self.inner.write_region(offset, data)?;
                *self.out = data.to_vec();
                Ok(())
            }
            fn flash_md5(&mut self, offset: u32, size: u32) -> Result<[u8; 16], CenterError> {
                self.inner.flash_md5(offset, size)
            }
            fn finish_reset(&mut self) -> Result<(), CenterError> {
                self.inner.finish_reset()
            }
        }
        let req_opt_out: UsbProvisionRequest = serde_json::from_value(json!({
            "action": "write",
            "port": "/dev/cu.usbmodem14201",
            "expected_mac": "aa:11:22:33:44:55",
            "confirmed": true,
            "auto_allow": false,
            "patch": { "ota_url": "https://api.tenclass.net/xiaozhi/ota/" }
        }))
        .unwrap();
        let (opt_out_val, _) =
            execute_usb_provision_with(&req_opt_out, fake_ports(), None, false, &db, |_| {
                Ok(Cap {
                    inner: MockSession {
                        info: chip.clone(),
                        part_table: xiaozhi_table.clone(),
                        nvs_offset: 0x9000,
                        nvs_bytes: stale_bytes.clone(),
                        reset_called: false,
                        writes: 0,
                    },
                    out: &mut captured,
                })
            })
            .unwrap();
        assert_eq!(opt_out_val["auto_allowed"], false);
        let parsed_out = NvsStore::parse(&captured).unwrap();
        assert_eq!(parsed_out.get_str("websocket", "token"), None);
        assert_eq!(parsed_out.get_str("websocket", "url"), None);

        // 再用 auto_allow=true 写入：把库里空 client_id 重绑成 uuid-keep，且保留真实板型
        let req_bind: UsbProvisionRequest = serde_json::from_value(json!({
            "action": "write",
            "port": "/dev/cu.usbmodem14201",
            "expected_mac": "aa:11:22:33:44:55",
            "confirmed": true,
            "auto_allow": true,
            "patch": { "ota_url": "http://192.168.1.20:9666/xiaozhi/ota/" }
        }))
        .unwrap();
        let (bound_val, _) =
            execute_usb_provision_with(&req_bind, fake_ports(), None, false, &db, |_| {
                Ok(MockSession {
                    info: chip.clone(),
                    part_table: xiaozhi_table.clone(),
                    nvs_offset: 0x9000,
                    nvs_bytes: captured.clone(),
                    reset_called: false,
                    writes: 0,
                })
            })
            .unwrap();
        assert_eq!(bound_val["auto_allowed"], true);
        assert_eq!(bound_val["known_device"]["model"], "bread-compact-wifi-lcd");
        assert_eq!(bound_val["known_device"]["status"], "allowed");
        assert_eq!(
            db.lock()
                .unwrap()
                .voice_device("aa:11:22:33:44:55")
                .unwrap()
                .unwrap()
                .client_id,
            "uuid-keep"
        );

        // 3. 当某台小智固件正在 downloading 时拦截 USB 读写
        {
            let guard = db.lock().unwrap();
            let sha = "c".repeat(64);
            guard
                .add_firmware_artifact(&sha, "2.5.0", "xiaozhi", 1024, 1.0)
                .unwrap();
            guard
                .create_firmware_offer(
                    "deliv-1",
                    "aa:11:22:33:44:55",
                    &sha,
                    "2.5.0",
                    &"d".repeat(32),
                    9999999999.0,
                    1.0,
                )
                .unwrap();
            let offer = guard
                .firmware_offer_by_token(&"d".repeat(32), 2.0)
                .unwrap()
                .unwrap();
            assert!(
                guard
                    .update_firmware_download_if_revision(
                        "deliv-1",
                        offer.revision,
                        "downloading",
                        3.0
                    )
                    .unwrap()
            );
        }
        let req_inspect: UsbProvisionRequest = serde_json::from_value(json!({
            "action": "inspect",
            "port": "/dev/cu.usbmodem14201",
        }))
        .unwrap();
        let fw_err = execute_usb_provision_with(
            &req_inspect,
            fake_ports(),
            None,
            false,
            &db,
            |_| -> Result<MockSession, CenterError> {
                unreachable!("downloading 时不得开串口")
            },
        )
        .unwrap_err();
        assert_eq!(fw_err.code, "usb_blocked_firmware_busy");

        // 4. 固件推送取消后（token=NULL 且 download_status 仍为 downloading）不再误锁 USB 配网
        assert!(
            db.lock()
                .unwrap()
                .cancel_firmware_offer("deliv-1", 4.0)
                .unwrap()
        );
        let after_cancel =
            execute_usb_provision_with(&req_inspect, fake_ports(), None, false, &db, |_| {
                Ok(MockSession {
                    info: chip.clone(),
                    part_table: xiaozhi_table.clone(),
                    nvs_offset: 0x9000,
                    nvs_bytes: captured.clone(),
                    reset_called: false,
                    writes: 0,
                })
            })
            .unwrap();
        assert_eq!(after_cancel.0["device"]["mac_address"], "aa:11:22:33:44:55");

        // 5. 过期但仍停留在 downloading 状态的记录同样不得误锁 USB 配网
        {
            let guard = db.lock().unwrap();
            let sha = "e".repeat(64);
            guard
                .add_firmware_artifact(&sha, "2.5.1", "xiaozhi", 1024, 1.0)
                .unwrap();
            guard
                .create_firmware_offer(
                    "deliv-expired",
                    "aa:11:22:33:44:55",
                    &sha,
                    "2.5.1",
                    &"f".repeat(32),
                    100.0,
                    1.0,
                )
                .unwrap();
            let offer = guard
                .firmware_offer_by_token(&"f".repeat(32), 2.0)
                .unwrap()
                .unwrap();
            assert!(
                guard
                    .update_firmware_download_if_revision(
                        "deliv-expired",
                        offer.revision,
                        "downloading",
                        3.0
                    )
                    .unwrap()
            );
        }
        assert!(!any_firmware_downloading(&db));

        // 6. 即使请求里误传 auto_allow=true，只要 ws_url 为空串就不得签发 token 或自动放行
        let mut stale_for_ext = NvsStore::default();
        stale_for_ext
            .set(
                "websocket",
                "url",
                NvsValue::Str("ws://192.168.1.20:9667/xiaozhi/v1/".into()),
            )
            .unwrap();
        stale_for_ext
            .set("websocket", "token", NvsValue::Str("moss-old-tok".into()))
            .unwrap();
        let stale_for_ext_bytes = stale_for_ext.build(0x6000).unwrap();
        {
            let guard = db.lock().unwrap();
            guard
                .voice_device_seen("11:22:33:44:55:66", "", "box", "2.1.0", "", 10.0)
                .unwrap();
        }
        let mut ext_captured = Vec::new();
        let ext_chip = UsbChipInfo {
            mac_address: "11:22:33:44:55:66".into(),
            ..chip.clone()
        };
        let req_ext_with_flag: UsbProvisionRequest = serde_json::from_value(json!({
            "action": "write",
            "port": "/dev/cu.usbmodem14201",
            "expected_mac": "11:22:33:44:55:66",
            "confirmed": true,
            "auto_allow": true,
            "patch": {
                "ota_url": "https://api.tenclass.net/xiaozhi/ota/",
                "ws_url": ""
            }
        }))
        .unwrap();
        let (ext_val, _) =
            execute_usb_provision_with(&req_ext_with_flag, fake_ports(), None, false, &db, |_| {
                Ok(Cap {
                    inner: MockSession {
                        info: ext_chip.clone(),
                        part_table: xiaozhi_table.clone(),
                        nvs_offset: 0x9000,
                        nvs_bytes: stale_for_ext_bytes.clone(),
                        reset_called: false,
                        writes: 0,
                    },
                    out: &mut ext_captured,
                })
            })
            .unwrap();
        assert_eq!(ext_val["auto_allowed"], false);
        let parsed_ext = NvsStore::parse(&ext_captured).unwrap();
        assert_eq!(parsed_ext.get_str("websocket", "token"), None);
        assert_eq!(parsed_ext.get_str("websocket", "url"), None);
        assert_eq!(
            db.lock()
                .unwrap()
                .voice_device("11:22:33:44:55:66")
                .unwrap()
                .unwrap()
                .status,
            "pending"
        );
    }

    impl<T: UsbFlasherSession> UsbFlasherSession for &mut T {
        fn info(&self) -> UsbChipInfo {
            (**self).info()
        }
        fn read_region(&mut self, offset: u32, size: u32) -> Result<Vec<u8>, CenterError> {
            (**self).read_region(offset, size)
        }
        fn write_region(&mut self, offset: u32, data: &[u8]) -> Result<(), CenterError> {
            (**self).write_region(offset, data)
        }
        fn flash_md5(&mut self, offset: u32, size: u32) -> Result<[u8; 16], CenterError> {
            (**self).flash_md5(offset, size)
        }
        fn finish_reset(&mut self) -> Result<(), CenterError> {
            (**self).finish_reset()
        }
    }

    /// 写入失败、校验不一致、复位失败等情况下板子的结局：模拟会话可以让前几次写入出错或写坏。
    struct FlakySession {
        inner: MockSession,
        /// 前 N 次写入先写进一半垃圾再报错（模拟拔线）
        fail_writes: usize,
        /// 前 N 次写入“成功”但实际写进去的内容有一个字节不对
        corrupt_writes: usize,
        fail_reset: bool,
        resets: usize,
    }

    impl UsbFlasherSession for FlakySession {
        fn info(&self) -> UsbChipInfo {
            self.inner.info()
        }
        fn read_region(&mut self, offset: u32, size: u32) -> Result<Vec<u8>, CenterError> {
            self.inner.read_region(offset, size)
        }
        fn write_region(&mut self, offset: u32, data: &[u8]) -> Result<(), CenterError> {
            if self.fail_writes > 0 {
                self.fail_writes -= 1;
                let half = data.len() / 2;
                self.inner.nvs_bytes[..half].fill(0x5A);
                return Err(CenterError::new("usb_flash_write_failed", 502, "串口断开"));
            }
            self.inner.write_region(offset, data)?;
            if self.corrupt_writes > 0 {
                self.corrupt_writes -= 1;
                self.inner.nvs_bytes[100] ^= 0xFF;
            }
            Ok(())
        }
        fn flash_md5(&mut self, offset: u32, size: u32) -> Result<[u8; 16], CenterError> {
            self.inner.flash_md5(offset, size)
        }
        fn finish_reset(&mut self) -> Result<(), CenterError> {
            self.resets += 1;
            if self.fail_reset {
                return Err(CenterError::new("usb_chip_reset_failed", 502, "复位失败"));
            }
            self.inner.finish_reset()
        }
    }

    fn xiaozhi_board(chip: &str) -> (UsbChipInfo, Vec<u8>, Vec<u8>) {
        let info = UsbChipInfo {
            chip: chip.into(),
            mac_address: "aa:bb:cc:00:11:22".into(),
            flash_size: "16MB".into(),
            flash_bytes: Some(16 << 20),
            revision: None,
        };
        let table = make_part_table(&[
            (0x01, 0x02, 0x9000, 0x6000, "nvs", 0),
            (0x00, 0x10, 0x20000, 0x200000, "ota_0", 0),
            (0x01, 0x82, 0x600000, 0x400000, "assets", 0),
        ]);
        let mut store = NvsStore::default();
        store
            .set("wifi", "ssid", NvsValue::Str("OldHome".into()))
            .unwrap();
        store
            .set("wifi", "password", NvsValue::Str("old-pass".into()))
            .unwrap();
        (info, table, store.build(0x6000).unwrap())
    }

    fn flaky(
        chip: &str,
        fail_writes: usize,
        corrupt_writes: usize,
        fail_reset: bool,
    ) -> FlakySession {
        let (info, table, nvs) = xiaozhi_board(chip);
        FlakySession {
            inner: MockSession {
                info,
                part_table: table,
                nvs_offset: 0x9000,
                nvs_bytes: nvs,
                reset_called: false,
                writes: 0,
            },
            fail_writes,
            corrupt_writes,
            fail_reset,
            resets: 0,
        }
    }

    fn write_request(auto_allow: bool, unrevoke: bool) -> UsbProvisionRequest {
        serde_json::from_value(json!({
            "action": "write",
            "port": "/dev/cu.usbmodem14201",
            "expected_mac": "aa:bb:cc:00:11:22",
            "confirmed": true,
            "auto_allow": auto_allow,
            "unrevoke": unrevoke,
            "patch": {
                "new_wifi": { "ssid": "NewHome", "password": "new-pass" },
                "ota_url": "http://192.168.1.20:9666/xiaozhi/ota/"
            }
        }))
        .unwrap()
    }

    /// 写入中途出错或回读校验不一致：把读出来的原配置写回并校验，板子保持原样，最后照常复位、不登记设备。
    #[test]
    fn failed_or_corrupted_write_restores_the_original_nvs_and_resets() {
        for (fail_writes, corrupt_writes) in [(1, 0), (0, 1)] {
            let db = Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap());
            let mut session = flaky("esp32s3", fail_writes, corrupt_writes, false);
            let original = session.inner.nvs_bytes.clone();
            let result = execute_write(
                "/dev/cu.usbmodem14201",
                Some("aa:bb:cc:00:11:22"),
                true,
                true,
                false,
                &XiaozhiNvsPatch {
                    new_wifi: Some(moss_xiaozhi_proto::nvs::NewWifiPatch {
                        ssid: "NewHome".into(),
                        password: "new-pass".into(),
                    }),
                    ..Default::default()
                },
                &mut session,
                &db,
            );
            let err = result.unwrap_err();
            assert_eq!(
                err.code, "usb_flash_write_rolled_back",
                "{fail_writes}/{corrupt_writes}: {}",
                err.message
            );
            assert_eq!(session.inner.nvs_bytes, original, "原配置逐字节写回");
            assert!(session.resets >= 1, "失败也要复位，不把板子留在下载模式");
            assert!(
                db.lock()
                    .unwrap()
                    .voice_device("aa:bb:cc:00:11:22")
                    .unwrap()
                    .is_none(),
                "写入没成功就不登记设备"
            );
        }
        // 写回原配置也失败：如实告诉用户配置可能已损坏
        let db = Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap());
        let mut session = flaky("esp32s3", 2, 0, false);
        let err = execute_write(
            "/dev/cu.usbmodem14201",
            Some("aa:bb:cc:00:11:22"),
            true,
            false,
            false,
            &XiaozhiNvsPatch::default(),
            &mut session,
            &db,
        )
        .unwrap_err();
        assert_eq!(err.code, "usb_flash_restore_failed");
        assert!(session.resets >= 1);
    }

    /// 数据已写入并校验无误、只是复位出错：算写入成功（照常登记），并提示手动按 RST。
    #[test]
    fn reset_failure_after_a_verified_write_is_a_warning_not_a_failure() {
        let db = Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap());
        let mut session = flaky("esp32s3", 0, 0, true);
        let (payload, ..) = execute_write(
            "/dev/cu.usbmodem14201",
            Some("aa:bb:cc:00:11:22"),
            true,
            true,
            false,
            &XiaozhiNvsPatch::default(),
            &mut session,
            &db,
        )
        .unwrap();
        assert_eq!(payload["ok"], true);
        assert!(
            payload["reset_warning"]
                .as_str()
                .is_some_and(|w| !w.is_empty())
        );
        assert_eq!(payload["auto_allowed"], true);
        let rec = db
            .lock()
            .unwrap()
            .voice_device("aa:bb:cc:00:11:22")
            .unwrap()
            .unwrap();
        assert_eq!(rec.status, laser_runtime::VOICE_DEVICE_ALLOWED);
    }

    /// ESP32-P4 上报的设备号不是芯片 MAC：不自动放行，也不预签绑在错误设备号上的密钥。
    #[test]
    fn esp32p4_boards_are_not_auto_allowed_by_chip_mac() {
        let db = Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap());
        let mut session = flaky("esp32p4", 0, 0, false);
        let (payload, ..) = execute_write(
            "/dev/cu.usbmodem14201",
            Some("aa:bb:cc:00:11:22"),
            true,
            true,
            false,
            &XiaozhiNvsPatch::default(),
            &mut session,
            &db,
        )
        .unwrap();
        assert_eq!(payload["auto_allowed"], false);
        assert!(payload["auto_allow_skipped"].as_str().is_some());
        assert!(
            db.lock()
                .unwrap()
                .voice_device("aa:bb:cc:00:11:22")
                .unwrap()
                .is_none()
        );
        let store = NvsStore::parse(&session.inner.nvs_bytes).unwrap();
        assert!(
            store
                .get_str("websocket", "token")
                .is_none_or(str::is_empty)
        );
    }

    /// 自动允许默认关；撤销过的设备要自动允许，必须显式确认解除撤销。
    #[test]
    fn auto_allow_defaults_off_and_revoked_devices_need_explicit_unrevoke() {
        let parsed: UsbProvisionRequest = serde_json::from_value(json!({
            "action": "write",
            "port": "/dev/cu.usbmodem14201"
        }))
        .unwrap();
        assert!(matches!(
            parsed,
            UsbProvisionRequest::Write {
                auto_allow: false,
                unrevoke: false,
                ..
            }
        ));

        let db = Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap());
        {
            let guard = db.lock().unwrap();
            guard
                .voice_device_seen(
                    "aa:bb:cc:00:11:22",
                    "old-client",
                    "board",
                    "2.0.0",
                    "192.168.1.9",
                    1.0,
                )
                .unwrap();
            guard
                .set_voice_device_status(
                    "aa:bb:cc:00:11:22",
                    laser_runtime::VOICE_DEVICE_REVOKED,
                    2.0,
                )
                .unwrap();
        }
        let run = |unrevoke: bool| {
            execute_usb_provision_with(
                &write_request(true, unrevoke),
                fake_ports(),
                None,
                false,
                &db,
                |_| Ok(flaky("esp32s3", 0, 0, false)),
            )
        };
        let err = run(false).unwrap_err();
        assert_eq!(err.code, "usb_device_revoked");
        assert_eq!(
            db.lock()
                .unwrap()
                .voice_device("aa:bb:cc:00:11:22")
                .unwrap()
                .unwrap()
                .status,
            laser_runtime::VOICE_DEVICE_REVOKED,
            "没确认就保持撤销"
        );
        run(true).unwrap();
        assert_eq!(
            db.lock()
                .unwrap()
                .voice_device("aa:bb:cc:00:11:22")
                .unwrap()
                .unwrap()
                .status,
            laser_runtime::VOICE_DEVICE_ALLOWED
        );
    }

    /// macOS 上激光机用 /dev/tty.X 连着时，配网列表里的 /dev/cu.X 是同一个口，必须拦住。
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn macos_tty_and_cu_names_of_the_same_port_are_one_port() {
        assert!(port_matches(
            "/dev/tty.usbserial-LASER",
            "/dev/cu.usbserial-LASER"
        ));
        let items = collect_serial_ports(fake_ports(), Some("/dev/tty.usbserial-LASER"));
        assert!(
            items
                .iter()
                .any(|item| item.port_name == "/dev/cu.usbserial-LASER" && item.is_laser_port)
        );
        let db = Mutex::new(laser_runtime::RuntimeDb::open_memory().unwrap());
        let inspect: UsbProvisionRequest = serde_json::from_value(
            json!({ "action": "inspect", "port": "/dev/cu.usbserial-LASER" }),
        )
        .unwrap();
        let err = execute_usb_provision_with(
            &inspect,
            fake_ports(),
            Some("/dev/tty.usbserial-LASER"),
            false,
            &db,
            |_| Ok(flaky("esp32s3", 0, 0, false)),
        )
        .unwrap_err();
        assert_eq!(err.code, "usb_blocked_laser_port");
    }

    #[test]
    fn temp_file_guard_and_sweep_clean_usb_read_images() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("usb-read-deadbeef.bin");
        let keep = dir.path().join("other.txt");
        std::fs::write(&f1, b"secret-wifi-pass").unwrap();
        std::fs::write(&keep, b"keep").unwrap();
        {
            let f2 = dir.path().join("usb-read-00112233.bin");
            std::fs::write(&f2, b"secret-2").unwrap();
            let _g = TempFileGuard(f2.clone());
            assert!(f2.exists());
        }
        assert!(!dir.path().join("usb-read-00112233.bin").exists());
        sweep_usb_temp_dir(dir.path());
        assert!(!f1.exists());
        assert!(keep.exists());
    }
}
