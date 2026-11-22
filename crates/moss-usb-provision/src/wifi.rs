//! 本机 Wi-Fi 读取：给 USB 配网的“新的 Wi-Fi”一栏填 SSID（2026-11-22 用户决定，技术书 n103）。
//!
//! 只读电脑当前连接的网络名和附近网络列表（信号、频段、是否加密），**不读密码**，结果不落盘、不写诊断。
//! macOS 14 起与 Windows 11 24H2 起，系统要求应用有“定位”权限才给出 Wi-Fi 名称：
//! macOS 须先在主线程调用 [`prepare_location_permission`] 弹出系统授权框，没授权时结果带提示而不报错。
//! Android 不编译 USB 配网（内置语音仅桌面端），这里只有 macOS / Windows / Linux 实现。

use std::collections::BTreeMap;

use moss_voice::CenterError;
use serde::{Deserialize, Serialize};

/// 板子能否连上取决于频段：小智常见芯片（ESP32 / S3 / C3 / C6）只支持 2.4 GHz，ESP32-C5 另支持 5 GHz。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum WifiBand {
    #[serde(rename = "2.4g")]
    G2_4,
    #[serde(rename = "5g")]
    G5,
    #[serde(rename = "6g")]
    G6,
}

impl WifiBand {
    /// 按信道中心频率（MHz）归类；不在 Wi-Fi 频段内的返回 `None`。
    pub fn from_mhz(mhz: u32) -> Option<Self> {
        match mhz {
            2400..=2500 => Some(Self::G2_4),
            4900..=5924 => Some(Self::G5),
            5925..=7125 => Some(Self::G6),
            _ => None,
        }
    }
}

/// 系统定位权限（只用来解释“为什么读不到名字”，MOSS 不读取位置）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WifiPermission {
    /// 已授权，或本系统读 Wi-Fi 名称不需要定位权限
    Granted,
    /// 还没问过用户（macOS 已在主线程弹出授权框）
    NotDetermined,
    Denied,
    /// 系统级“定位服务”整体关闭
    ServicesOff,
}

/// 一条底层观测：同一 SSID 在不同频段 / 不同 AP 会出现多次，由 [`merge_observations`] 合并。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WifiObservation {
    pub ssid: String,
    /// 0–100
    pub signal: Option<u8>,
    pub band: Option<WifiBand>,
    pub secure: Option<bool>,
    /// 仅支持 WPA3（不含 WPA2 / 开放）：部分小智 ESP32 固件只兼容 WPA2，界面据此提前提醒
    pub wpa3_only: Option<bool>,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WifiNetwork {
    pub ssid: String,
    /// 所有频段里最强的信号
    pub signal: Option<u8>,
    pub bands: Vec<WifiBand>,
    /// 每个频段各自最强的信号：双频同名网络里 2.4 GHz 往往比 5 GHz 弱，界面按板子支持的频段显示
    pub band_signals: BTreeMap<WifiBand, u8>,
    pub secure: Option<bool>,
    /// 仅支持 WPA3（不含 WPA2 过渡模式）；无法判断时为 `None`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wpa3_only: Option<bool>,
    pub current: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WifiReport {
    pub platform: &'static str,
    pub permission: WifiPermission,
    pub current: Option<WifiNetwork>,
    pub networks: Vec<WifiNetwork>,
    pub hint: Option<String>,
}

/// dBm 换算成 0–100（与 Windows 信号质量同一线性刻度：-100 dBm → 0，-50 dBm 及以上 → 100）。
pub fn rssi_to_percent(dbm: i64) -> u8 {
    ((dbm + 100) * 2).clamp(0, 100) as u8
}

/// 小智固件按 32 字节上限保存 SSID；隐藏网络（空名）与超长名字无法写入，直接丢弃。
fn usable_ssid(ssid: &str) -> bool {
    !ssid.trim().is_empty() && ssid.len() <= 32
}

/// 按 SSID 合并：信号取最强、频段取并集、任一 AP 加密即算加密、任一条是当前连接即算当前。
/// 排序：当前连接在前，其余按信号从强到弱，同信号按名字。
pub fn merge_observations(observations: Vec<WifiObservation>) -> Vec<WifiNetwork> {
    let mut merged: BTreeMap<String, WifiNetwork> = BTreeMap::new();
    for obs in observations {
        if !usable_ssid(&obs.ssid) {
            continue;
        }
        let entry = merged
            .entry(obs.ssid.clone())
            .or_insert_with(|| WifiNetwork {
                ssid: obs.ssid.clone(),
                signal: None,
                bands: Vec::new(),
                band_signals: BTreeMap::new(),
                secure: None,
                wpa3_only: None,
                current: false,
            });
        entry.signal = match (entry.signal, obs.signal) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        if let Some(band) = obs.band
            && !entry.bands.contains(&band)
        {
            entry.bands.push(band);
            entry.bands.sort();
        }
        if let (Some(band), Some(signal)) = (obs.band, obs.signal) {
            let slot = entry.band_signals.entry(band).or_insert(signal);
            *slot = (*slot).max(signal);
        }
        entry.secure = match (entry.secure, obs.secure) {
            (Some(a), Some(b)) => Some(a || b),
            (a, b) => a.or(b),
        };
        // 同名多 AP 只要任一 AP 兼容 WPA2 / 开放（false），板子就能连，不算“仅 WPA3”
        entry.wpa3_only = match (entry.wpa3_only, obs.wpa3_only) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), _) | (_, Some(true)) => Some(true),
            (None, None) => None,
        };
        entry.current |= obs.current;
    }
    let mut list: Vec<WifiNetwork> = merged.into_values().collect();
    list.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then(b.signal.unwrap_or(0).cmp(&a.signal.unwrap_or(0)))
            .then(a.ssid.cmp(&b.ssid))
    });
    list
}

fn permission_hint(platform: &str, permission: WifiPermission) -> Option<String> {
    // 界面把这段话放进“去系统设置打开 / 手动输入”弹窗，所以只说原因和去哪里开
    let text = match (platform, permission) {
        (_, WifiPermission::Granted) => return None,
        ("macos", WifiPermission::NotDetermined) => {
            "macOS 规定应用要有“定位服务”权限才能读到 Wi-Fi 名称（MOSS 不读取、不保存位置）。系统应已弹出授权框，点“允许”即可；没看到授权框，就到“系统设置 › 隐私与安全性 › 定位服务”里打开 MOSS。"
        }
        ("macos", WifiPermission::ServicesOff) => {
            "macOS 的“定位服务”总开关是关着的，系统因此不给出 Wi-Fi 名称（MOSS 不读取、不保存位置）。到“系统设置 › 隐私与安全性 › 定位服务”打开总开关并允许 MOSS 后就能读到。"
        }
        ("macos", _) => {
            "MOSS 没有“定位服务”权限，macOS 因此不给出 Wi-Fi 名称（MOSS 不读取、不保存位置）。到“系统设置 › 隐私与安全性 › 定位服务”里打开 MOSS 后就能读到。"
        }
        ("windows", _) => {
            "Windows 要求开位置权限才给出 Wi-Fi 名称（MOSS 不读取、不保存位置）。到“设置 › 隐私和安全性 › 位置”打开“位置服务”和“允许桌面应用访问你的位置”后就能读到。"
        }
        _ => "系统没有给出 Wi-Fi 名称，请直接手动输入。",
    };
    Some(text.to_owned())
}

fn build_report(
    platform: &'static str,
    permission: WifiPermission,
    observations: Vec<WifiObservation>,
    scanned: bool,
) -> WifiReport {
    let networks = merge_observations(observations);
    let current = networks.iter().find(|n| n.current).cloned();
    let hint = permission_hint(platform, permission).or_else(|| {
        if current.is_none() && !scanned {
            Some("这台电脑现在没有连 Wi-Fi（或用的是有线网），可以点“扫描附近”或手动输入。".into())
        } else if networks.is_empty() && scanned {
            Some("没有扫描到附近的 Wi-Fi，请确认电脑的 Wi-Fi 已打开，或直接手动输入。".into())
        } else {
            None
        }
    });
    WifiReport {
        platform,
        permission,
        current,
        // 只查当前连接时不返回列表，免得界面把它当成扫描结果
        networks: if scanned { networks } else { Vec::new() },
        hint,
    }
}

fn unavailable(message: impl Into<String>) -> CenterError {
    CenterError::new("wifi_unavailable", 503, message)
}

/// 电脑的 Wi-Fi 开关是关的：界面据此提供“打开 Wi-Fi 设置”。
#[cfg_attr(
    not(any(target_os = "macos", windows, target_os = "linux")),
    allow(dead_code)
)]
fn radio_off() -> CenterError {
    CenterError::new(
        "wifi_radio_off",
        503,
        "电脑的 Wi-Fi 是关着的，打开 Wi-Fi 后才能读取附近的网络名称。",
    )
}

/// 界面“去系统设置打开”要跳到哪一页。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WifiSettingsTarget {
    /// 定位 / 位置隐私页（macOS 定位服务、Windows 位置）
    Location,
    /// Wi-Fi 开关所在的网络设置页
    Wifi,
}

/// 各系统的设置页地址，只用这几个固定值（不接受界面传入任意地址）。
fn settings_uri(platform: &str, target: WifiSettingsTarget) -> Option<&'static str> {
    match (platform, target) {
        ("macos", WifiSettingsTarget::Location) => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_LocationServices")
        }
        ("macos", WifiSettingsTarget::Wifi) => {
            Some("x-apple.systempreferences:com.apple.wifi-settings-extension")
        }
        ("windows", WifiSettingsTarget::Location) => Some("ms-settings:privacy-location"),
        ("windows", WifiSettingsTarget::Wifi) => Some("ms-settings:network-wifi"),
        _ => None,
    }
}

/// 打开系统设置里的定位权限页或 Wi-Fi 设置页（用户在弹窗里点“去系统设置打开”时调用）。
pub fn open_settings(target: WifiSettingsTarget) -> Result<(), CenterError> {
    let uri = settings_uri(imp::PLATFORM, target).ok_or_else(|| {
        unavailable(
            "这个系统没有可以直接跳转的设置页，请在系统的网络或隐私设置里操作，或直接手动输入",
        )
    })?;
    launch_settings(uri, target)
}

/// 打不开设置页时告诉用户手动去哪里开。
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn manual_settings_path(platform: &str, target: WifiSettingsTarget) -> &'static str {
    match (platform, target) {
        ("macos", WifiSettingsTarget::Location) => "“系统设置 › 隐私与安全性 › 定位服务”",
        ("macos", WifiSettingsTarget::Wifi) => "“系统设置 › Wi-Fi”",
        ("windows", WifiSettingsTarget::Location) => "“设置 › 隐私和安全性 › 位置”",
        _ => "“设置 › 网络和 Internet › WLAN”",
    }
}

#[cfg(target_os = "macos")]
fn launch_settings(uri: &str, target: WifiSettingsTarget) -> Result<(), CenterError> {
    let failed = || {
        unavailable(format!(
            "打不开系统设置，请手动打开{}",
            manual_settings_path("macos", target)
        ))
    };
    let status = std::process::Command::new("/usr/bin/open")
        .arg(uri)
        .status()
        .map_err(|_| failed())?;
    if status.success() {
        Ok(())
    } else {
        Err(failed())
    }
}

#[cfg(windows)]
fn launch_settings(uri: &str, target: WifiSettingsTarget) -> Result<(), CenterError> {
    // explorer.exe 打开 ms-settings: 地址后即返回，且退出码恒为 1，不按退出码判断
    std::process::Command::new("explorer.exe")
        .arg(uri)
        .spawn()
        .map(|_| ())
        .map_err(|_| {
            unavailable(format!(
                "打不开 Windows 设置，请手动打开{}",
                manual_settings_path("windows", target)
            ))
        })
}

#[cfg(not(any(target_os = "macos", windows)))]
fn launch_settings(_uri: &str, _target: WifiSettingsTarget) -> Result<(), CenterError> {
    Err(unavailable("这个系统没有可以直接跳转的设置页"))
}

/// 电脑当前连接的 Wi-Fi（不扫描，通常一秒内返回）。
pub fn current() -> Result<WifiReport, CenterError> {
    let (permission, observations) = imp::current()?;
    Ok(build_report(imp::PLATFORM, permission, observations, false))
}

/// 扫描附近的 Wi-Fi（含当前连接），通常要几秒。
pub fn scan() -> Result<WifiReport, CenterError> {
    let (permission, observations) = imp::scan()?;
    Ok(build_report(imp::PLATFORM, permission, observations, true))
}

/// macOS：在**主线程**调用，还没问过用户时弹出系统定位授权框（管理器留在主线程上，授权框才不会被收走）。
/// 其他系统什么也不做。
pub fn prepare_location_permission() {
    #[cfg(target_os = "macos")]
    imp::prepare_location_permission();
}

// ---------------------------------------------------------------------------------------------
// macOS：CoreWLAN + CoreLocation
// ---------------------------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2_core_location::{CLAuthorizationStatus, CLLocationManager};
    use objc2_core_wlan::{
        CWChannel, CWChannelBand, CWInterface, CWNetwork, CWSecurity, CWWiFiClient,
    };

    use super::{
        CenterError, WifiBand, WifiObservation, WifiPermission, radio_off, rssi_to_percent,
        unavailable,
    };

    pub const PLATFORM: &str = "macos";

    thread_local! {
        // 只在主线程上使用：授权框显示期间管理器必须一直存活
        static MANAGER: RefCell<Option<Retained<CLLocationManager>>> = const { RefCell::new(None) };
    }

    pub fn prepare_location_permission() {
        MANAGER.with(|cell| {
            let mut slot = cell.borrow_mut();
            // SAFETY: CoreLocation 类方法与实例方法均无前置条件；本函数约定在主线程调用
            unsafe {
                if !CLLocationManager::locationServicesEnabled_class() {
                    return;
                }
                let manager = slot.get_or_insert_with(|| CLLocationManager::new());
                if manager.authorizationStatus() == CLAuthorizationStatus::NotDetermined {
                    manager.requestWhenInUseAuthorization();
                }
            }
        });
    }

    fn permission() -> WifiPermission {
        // SAFETY: 只读授权状态，CLLocationManager 可在任意线程创建
        unsafe {
            if !CLLocationManager::locationServicesEnabled_class() {
                return WifiPermission::ServicesOff;
            }
            let status = CLLocationManager::new().authorizationStatus();
            if status == CLAuthorizationStatus::AuthorizedAlways
                || status == CLAuthorizationStatus::AuthorizedWhenInUse
            {
                WifiPermission::Granted
            } else if status == CLAuthorizationStatus::NotDetermined {
                WifiPermission::NotDetermined
            } else {
                WifiPermission::Denied
            }
        }
    }

    fn interface() -> Result<Retained<CWInterface>, CenterError> {
        // SAFETY: CoreWLAN 共享客户端，无前置条件
        let iface = unsafe { CWWiFiClient::sharedWiFiClient().interface() }
            .ok_or_else(|| unavailable("这台 Mac 没有可用的无线网卡"))?;
        // SAFETY: 同上
        if !unsafe { iface.powerOn() } {
            return Err(radio_off());
        }
        Ok(iface)
    }

    fn band(channel: Option<Retained<CWChannel>>) -> Option<WifiBand> {
        // SAFETY: 只读属性
        let band = unsafe { channel?.channelBand() };
        match band {
            CWChannelBand::Band2GHz => Some(WifiBand::G2_4),
            CWChannelBand::Band5GHz => Some(WifiBand::G5),
            CWChannelBand::Band6GHz => Some(WifiBand::G6),
            _ => None,
        }
    }

    fn current_observation(iface: &CWInterface) -> Option<WifiObservation> {
        // SAFETY: 只读属性；没授权或没连接时 ssid 为空
        unsafe {
            let ssid = iface.ssid()?.to_string();
            Some(WifiObservation {
                ssid,
                signal: Some(rssi_to_percent(iface.rssiValue() as i64)),
                band: band(iface.wlanChannel()),
                secure: None,
                wpa3_only: None,
                current: true,
            })
        }
    }

    fn network_observation(net: &CWNetwork) -> Option<WifiObservation> {
        // SAFETY: 只读属性
        unsafe {
            let ssid = net.ssid()?.to_string();
            let is_open = net.supportsSecurity(CWSecurity::None);
            let supports_wpa3 = net.supportsSecurity(CWSecurity::WPA3Personal)
                || net.supportsSecurity(CWSecurity::WPA3Enterprise);
            let supports_compat = is_open
                || net.supportsSecurity(CWSecurity::WPA2Personal)
                || net.supportsSecurity(CWSecurity::WPA3Transition)
                || net.supportsSecurity(CWSecurity::WPAPersonal)
                || net.supportsSecurity(CWSecurity::WPAPersonalMixed)
                || net.supportsSecurity(CWSecurity::WPA2Enterprise)
                || net.supportsSecurity(CWSecurity::WPAEnterprise)
                || net.supportsSecurity(CWSecurity::WPAEnterpriseMixed)
                || net.supportsSecurity(CWSecurity::WEP);
            let wpa3_only = if supports_wpa3 && !supports_compat {
                Some(true)
            } else if supports_compat {
                Some(false)
            } else {
                None
            };
            Some(WifiObservation {
                ssid,
                signal: Some(rssi_to_percent(net.rssiValue() as i64)),
                band: band(net.wlanChannel()),
                secure: Some(!is_open),
                wpa3_only,
                current: false,
            })
        }
    }

    pub fn current() -> Result<(WifiPermission, Vec<WifiObservation>), CenterError> {
        let permission = permission();
        let iface = interface()?;
        if permission != WifiPermission::Granted {
            return Ok((permission, Vec::new()));
        }
        Ok((
            permission,
            current_observation(&iface).into_iter().collect(),
        ))
    }

    pub fn scan() -> Result<(WifiPermission, Vec<WifiObservation>), CenterError> {
        let permission = permission();
        let iface = interface()?;
        // 没授权时扫描结果里没有名字，白等几秒没有意义
        if permission != WifiPermission::Granted {
            return Ok((permission, Vec::new()));
        }
        let mut list: Vec<WifiObservation> = current_observation(&iface).into_iter().collect();
        // SAFETY: 同步扫描（阻塞几秒，调用方在阻塞线程池里）；失败时退回系统缓存的上次结果
        let networks = unsafe {
            iface
                .scanForNetworksWithName_error(None)
                .ok()
                .or_else(|| iface.cachedScanResults())
        };
        if let Some(set) = networks {
            list.extend(set.iter().filter_map(|net| network_observation(&net)));
        }
        Ok((permission, list))
    }
}

// ---------------------------------------------------------------------------------------------
// Windows：wlanapi（WlanScan + 可用网络列表 + BSS 列表取频段）
// ---------------------------------------------------------------------------------------------
#[cfg(windows)]
mod imp {
    use std::collections::HashSet;
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::NetworkManagement::WiFi::{
        DOT11_BSS_TYPE, DOT11_SSID, L2_NOTIFICATION_DATA, WLAN_AVAILABLE_NETWORK,
        WLAN_AVAILABLE_NETWORK_LIST, WLAN_BSS_ENTRY, WLAN_BSS_LIST, WLAN_CONNECTION_ATTRIBUTES,
        WLAN_INTERFACE_INFO, WLAN_INTERFACE_INFO_LIST, WLAN_NOTIFICATION_SOURCE_ACM,
        WLAN_NOTIFICATION_SOURCE_NONE, WLAN_RADIO_STATE, WlanCloseHandle, WlanEnumInterfaces,
        WlanFreeMemory, WlanGetAvailableNetworkList, WlanGetNetworkBssList, WlanOpenHandle,
        WlanQueryInterface, WlanRegisterNotification, WlanScan, dot11_BSS_type_any,
        dot11_radio_state_off, wlan_interface_state_connected, wlan_intf_opcode_current_connection,
        wlan_intf_opcode_radio_state, wlan_notification_acm_scan_complete,
        wlan_notification_acm_scan_fail,
    };
    use windows_sys::core::GUID;

    use super::{
        CenterError, WifiBand, WifiObservation, WifiPermission, radio_off, rsn_ie_is_wpa3_only,
        unavailable,
    };

    pub const PLATFORM: &str = "windows";
    /// WLAN AutoConfig 服务没运行（台式机没装无线网卡时常见）
    const ERROR_SERVICE_NOT_ACTIVE: u32 = 1062;
    /// WlanScan 最长等待兜底（多数网卡 0.8～1.5 秒内会发 scan_complete 通知提前结束）
    const SCAN_WAIT: Duration = Duration::from_millis(3500);

    fn guid_key(g: &GUID) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&g.data1.to_le_bytes());
        out[4..6].copy_from_slice(&g.data2.to_le_bytes());
        out[6..8].copy_from_slice(&g.data3.to_le_bytes());
        out[8..16].copy_from_slice(&g.data4);
        out
    }

    struct ScanWaitState {
        pending: Mutex<HashSet<[u8; 16]>>,
        cv: Condvar,
    }

    unsafe extern "system" fn scan_notify_cb(data: *mut L2_NOTIFICATION_DATA, ctx: *mut c_void) {
        if data.is_null() || ctx.is_null() {
            return;
        }
        // SAFETY: data 由 wlanapi 在回调期间保证有效，ctx 指向 ScanWaitState 且在注销回调前一直存活
        let (source, code, key) = unsafe {
            let d = &*data;
            (
                d.NotificationSource,
                d.NotificationCode,
                guid_key(&d.InterfaceGuid),
            )
        };
        if source != WLAN_NOTIFICATION_SOURCE_ACM
            || (code != wlan_notification_acm_scan_complete as u32
                && code != wlan_notification_acm_scan_fail as u32)
        {
            return;
        }
        // SAFETY: 同上；不 unwrap，避免跨 FFI panic
        let state = unsafe { &*(ctx as *const ScanWaitState) };
        if let Ok(mut pending) = state.pending.lock() {
            pending.remove(&key);
            if pending.is_empty() {
                state.cv.notify_all();
            }
        }
    }

    struct ScanNotifyGuard {
        handle: HANDLE,
        registered: bool,
    }

    impl Drop for ScanNotifyGuard {
        fn drop(&mut self) {
            if self.registered {
                // SAFETY: 注销回调会同步等待在途回调退出，保证先于 ScanWaitState 释放
                unsafe {
                    WlanRegisterNotification(
                        self.handle,
                        WLAN_NOTIFICATION_SOURCE_NONE,
                        0,
                        None,
                        null(),
                        null(),
                        null_mut(),
                    );
                }
            }
        }
    }

    struct BssSummary {
        entry: WLAN_BSS_ENTRY,
        wpa3_only: Option<bool>,
    }

    struct Client(HANDLE);

    impl Drop for Client {
        fn drop(&mut self) {
            // SAFETY: 句柄来自 WlanOpenHandle，只关闭一次
            unsafe { WlanCloseHandle(self.0, null()) };
        }
    }

    /// wlanapi 分配的内存，离开作用域时 WlanFreeMemory。
    struct WlanMem<T>(*mut T);

    impl<T> Drop for WlanMem<T> {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: 指针来自 wlanapi 的输出参数
                unsafe { WlanFreeMemory(self.0 as *const c_void) };
            }
        }
    }

    enum Denied {
        Permission,
        Other(CenterError),
    }

    fn api_error(what: &str, code: u32) -> Denied {
        if code == ERROR_ACCESS_DENIED {
            return Denied::Permission;
        }
        Denied::Other(unavailable(format!(
            "读取本机 Wi-Fi 失败（{what}，错误码 {code}）"
        )))
    }

    fn ssid_text(ssid: &DOT11_SSID) -> Option<String> {
        let len = (ssid.uSSIDLength as usize).min(ssid.ucSSID.len());
        String::from_utf8(ssid.ucSSID[..len].to_vec()).ok()
    }

    fn open() -> Result<(Client, Vec<GUID>), CenterError> {
        let mut version = 0u32;
        let mut handle: HANDLE = null_mut();
        // SAFETY: 输出参数都指向本地变量
        let code = unsafe { WlanOpenHandle(2, null(), &mut version, &mut handle) };
        if code == ERROR_SERVICE_NOT_ACTIVE {
            return Err(unavailable(
                "这台电脑的 WLAN 服务没有运行（可能没有无线网卡），请直接手动输入",
            ));
        }
        if code != ERROR_SUCCESS {
            return Err(unavailable(format!(
                "打开本机 Wi-Fi 接口失败（错误码 {code}）"
            )));
        }
        let client = Client(handle);
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = null_mut();
        // SAFETY: 句柄有效，输出指针由 WlanMem 释放
        let code = unsafe { WlanEnumInterfaces(client.0, null(), &mut list) };
        let list = WlanMem(list);
        if code != ERROR_SUCCESS || list.0.is_null() {
            return Err(unavailable(format!(
                "列出本机无线网卡失败（错误码 {code}）"
            )));
        }
        // SAFETY: 变长数组，长度为 dwNumberOfItems
        let guids = unsafe {
            let count = (*list.0).dwNumberOfItems as usize;
            let first = std::ptr::addr_of!((*list.0).InterfaceInfo) as *const WLAN_INTERFACE_INFO;
            (0..count)
                .map(|i| (*first.add(i)).InterfaceGuid)
                .collect::<Vec<_>>()
        };
        if guids.is_empty() {
            return Err(unavailable("这台电脑没有可用的无线网卡，请直接手动输入"));
        }
        if guids.iter().all(|guid| radio_is_off(&client, guid)) {
            return Err(radio_off());
        }
        Ok((client, guids))
    }

    /// 网卡所有射频都被软件开关（飞行模式 / Wi-Fi 开关）或硬件开关关掉；读不到状态时按“开着”处理。
    fn radio_is_off(client: &Client, guid: &GUID) -> bool {
        let mut size = 0u32;
        let mut data: *mut c_void = null_mut();
        // SAFETY: 输出参数指向本地变量，数据由 WlanMem 释放
        let code = unsafe {
            WlanQueryInterface(
                client.0,
                guid,
                wlan_intf_opcode_radio_state,
                null(),
                &mut size,
                &mut data,
                null_mut(),
            )
        };
        let data = WlanMem(data as *mut WLAN_RADIO_STATE);
        if code != ERROR_SUCCESS || data.0.is_null() {
            return false;
        }
        // SAFETY: 成功时 data 指向 WLAN_RADIO_STATE（定长数组，dwNumberOfPhys 不超过 64）
        let state = unsafe { &*data.0 };
        let count = (state.dwNumberOfPhys as usize).min(state.PhyRadioState.len());
        count > 0
            && state.PhyRadioState[..count].iter().all(|phy| {
                phy.dot11SoftwareRadioState == dot11_radio_state_off
                    || phy.dot11HardwareRadioState == dot11_radio_state_off
            })
    }

    fn current_on(
        client: &Client,
        guid: &GUID,
    ) -> Result<Option<(DOT11_SSID, WifiObservation)>, Denied> {
        let mut size = 0u32;
        let mut data: *mut c_void = null_mut();
        // SAFETY: 输出参数指向本地变量，数据由 WlanMem 释放
        let code = unsafe {
            WlanQueryInterface(
                client.0,
                guid,
                wlan_intf_opcode_current_connection,
                null(),
                &mut size,
                &mut data,
                null_mut(),
            )
        };
        let data = WlanMem(data as *mut WLAN_CONNECTION_ATTRIBUTES);
        // 没连接时返回 ERROR_INVALID_STATE，按“没有当前网络”处理
        if code == ERROR_ACCESS_DENIED {
            return Err(Denied::Permission);
        }
        if code != ERROR_SUCCESS || data.0.is_null() {
            return Ok(None);
        }
        // SAFETY: 成功时 data 指向 WLAN_CONNECTION_ATTRIBUTES
        let attrs = unsafe { &*data.0 };
        if attrs.isState != wlan_interface_state_connected {
            return Ok(None);
        }
        let assoc = &attrs.wlanAssociationAttributes;
        let Some(ssid) = ssid_text(&assoc.dot11Ssid) else {
            return Ok(None);
        };
        let matched_bss = bss_entries(
            client,
            guid,
            Some((
                &assoc.dot11Ssid,
                assoc.dot11BssType,
                attrs.wlanSecurityAttributes.bSecurityEnabled,
            )),
        )
        .ok()
        .and_then(|entries| {
            entries
                .into_iter()
                .find(|item| item.entry.dot11Bssid == assoc.dot11Bssid)
        });
        let band = matched_bss
            .as_ref()
            .and_then(|item| WifiBand::from_mhz(item.entry.ulChCenterFrequency / 1000));
        let wpa3_only = matched_bss.and_then(|item| item.wpa3_only);
        Ok(Some((
            assoc.dot11Ssid,
            WifiObservation {
                ssid,
                signal: Some(assoc.wlanSignalQuality.min(100) as u8),
                band,
                secure: Some(attrs.wlanSecurityAttributes.bSecurityEnabled != 0),
                wpa3_only,
                current: true,
            },
        )))
    }

    fn bss_entries(
        client: &Client,
        guid: &GUID,
        ssid: Option<(&DOT11_SSID, DOT11_BSS_TYPE, i32)>,
    ) -> Result<Vec<BssSummary>, Denied> {
        let mut list: *mut WLAN_BSS_LIST = null_mut();
        // SAFETY: 输出指针由 WlanMem 释放；指定 SSID 时 dot11BssType 与 bSecurityEnabled 须传入具体网络类型及加密状态，否则返回空表
        let code = unsafe {
            WlanGetNetworkBssList(
                client.0,
                guid,
                ssid.map_or(null(), |(s, _, _)| s as *const DOT11_SSID),
                ssid.map_or(dot11_BSS_type_any, |(_, bss_type, _)| bss_type),
                ssid.map_or(0, |(_, _, security_enabled)| security_enabled),
                null(),
                &mut list,
            )
        };
        let list = WlanMem(list);
        if code != ERROR_SUCCESS || list.0.is_null() {
            return Err(api_error("BSS 列表", code));
        }
        // SAFETY: 所有指针偏移均按 dwTotalSize 做边界裁剪后再解引用
        Ok(unsafe {
            let total_size = (*list.0).dwTotalSize as usize;
            let base = list.0 as *const u8;
            let count = (*list.0).dwNumberOfItems as usize;
            let first = std::ptr::addr_of!((*list.0).wlanBssEntries) as *const WLAN_BSS_ENTRY;
            let entries_off = (first as usize).saturating_sub(base as usize);
            let max_count =
                total_size.saturating_sub(entries_off) / std::mem::size_of::<WLAN_BSS_ENTRY>();
            let safe_count = count.min(max_count);
            (0..safe_count)
                .map(|i| {
                    let ptr = first.add(i);
                    let entry = *ptr;
                    let entry_off = (ptr as usize).saturating_sub(base as usize);
                    let wpa3_only = entry_off
                        .checked_add(entry.ulIeOffset as usize)
                        .and_then(|start| {
                            let end = start.checked_add(entry.ulIeSize as usize)?;
                            (end <= total_size && entry.ulIeSize > 0).then(|| {
                                std::slice::from_raw_parts(base.add(start), entry.ulIeSize as usize)
                            })
                        })
                        .and_then(rsn_ie_is_wpa3_only);
                    BssSummary { entry, wpa3_only }
                })
                .collect()
        })
    }

    fn available(client: &Client, guid: &GUID) -> Result<Vec<WLAN_AVAILABLE_NETWORK>, Denied> {
        let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = null_mut();
        // SAFETY: 输出指针由 WlanMem 释放
        let code = unsafe { WlanGetAvailableNetworkList(client.0, guid, 0, null(), &mut list) };
        let list = WlanMem(list);
        if code != ERROR_SUCCESS || list.0.is_null() {
            return Err(api_error("可用网络列表", code));
        }
        // SAFETY: 变长数组，长度为 dwNumberOfItems
        Ok(unsafe {
            let count = (*list.0).dwNumberOfItems as usize;
            let first = std::ptr::addr_of!((*list.0).Network) as *const WLAN_AVAILABLE_NETWORK;
            (0..count).map(|i| *first.add(i)).collect()
        })
    }

    fn finish(
        result: Result<Vec<WifiObservation>, Denied>,
    ) -> Result<(WifiPermission, Vec<WifiObservation>), CenterError> {
        match result {
            Ok(list) => Ok((WifiPermission::Granted, list)),
            Err(Denied::Permission) => Ok((WifiPermission::Denied, Vec::new())),
            Err(Denied::Other(err)) => Err(err),
        }
    }

    pub fn current() -> Result<(WifiPermission, Vec<WifiObservation>), CenterError> {
        let (client, guids) = open()?;
        finish((|| {
            let mut list = Vec::new();
            for guid in &guids {
                if let Some((_, obs)) = current_on(&client, guid)? {
                    list.push(obs);
                }
            }
            Ok(list)
        })())
    }

    pub fn scan() -> Result<(WifiPermission, Vec<WifiObservation>), CenterError> {
        let (client, guids) = open()?;
        // 优先监听 wlanapi 扫描完成通知，扫描一完成立即返回，不再每次硬等 3.5 秒；注册失败再退回固定等待
        let wait_state = Box::new(ScanWaitState {
            pending: Mutex::new(guids.iter().map(guid_key).collect()),
            cv: Condvar::new(),
        });
        let ctx_ptr = &*wait_state as *const ScanWaitState as *const c_void;
        // SAFETY: wait_state 在堆上地址稳定，且 _notify_guard 声明在后保证先注销回调再释放 wait_state
        let reg_code = unsafe {
            WlanRegisterNotification(
                client.0,
                WLAN_NOTIFICATION_SOURCE_ACM,
                0,
                Some(scan_notify_cb),
                ctx_ptr,
                null(),
                null_mut(),
            )
        };
        let _notify_guard = ScanNotifyGuard {
            handle: client.0,
            registered: reg_code == ERROR_SUCCESS,
        };
        let mut any_scan_started = false;
        for guid in &guids {
            // SAFETY: 句柄与 GUID 有效；只发起扫描，结果通过通知或超时后读取
            let code = unsafe { WlanScan(client.0, guid, null(), null(), null()) };
            if code == ERROR_SUCCESS {
                any_scan_started = true;
            } else if let Ok(mut pending) = wait_state.pending.lock() {
                pending.remove(&guid_key(guid));
            }
        }
        if any_scan_started {
            if _notify_guard.registered {
                let deadline = Instant::now() + SCAN_WAIT;
                if let Ok(mut pending) = wait_state.pending.lock() {
                    while !pending.is_empty() {
                        let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                            break;
                        };
                        let Ok((guard, timeout)) = wait_state.cv.wait_timeout(pending, rem) else {
                            break;
                        };
                        pending = guard;
                        if timeout.timed_out() {
                            break;
                        }
                    }
                }
            } else {
                std::thread::sleep(SCAN_WAIT);
            }
        }
        drop(_notify_guard);
        drop(wait_state);
        finish((|| {
            let mut list = Vec::new();
            for guid in &guids {
                if let Some((_, obs)) = current_on(&client, guid)? {
                    list.push(obs);
                }
                for net in available(&client, guid)? {
                    let Some(ssid) = ssid_text(&net.dot11Ssid) else {
                        continue;
                    };
                    list.push(WifiObservation {
                        ssid,
                        signal: Some(net.wlanSignalQuality.min(100) as u8),
                        band: None,
                        secure: Some(net.bSecurityEnabled != 0),
                        wpa3_only: None,
                        current: false,
                    });
                }
                // 可用网络列表不带频率与准确 RSN AKM，频段与仅 WPA3 判定从 BSS 列表补齐
                if let Ok(entries) = bss_entries(&client, guid, None) {
                    for item in entries {
                        let Some(ssid) = ssid_text(&item.entry.dot11Ssid) else {
                            continue;
                        };
                        list.push(WifiObservation {
                            ssid,
                            signal: Some(item.entry.uLinkQuality.min(100) as u8),
                            band: WifiBand::from_mhz(item.entry.ulChCenterFrequency / 1000),
                            secure: None,
                            wpa3_only: item.wpa3_only,
                            current: false,
                        });
                    }
                }
            }
            Ok(list)
        })())
    }
}

// ---------------------------------------------------------------------------------------------
// Linux：NetworkManager 的 nmcli（简洁模式输出不随系统语言变化）
// ---------------------------------------------------------------------------------------------
#[cfg(target_os = "linux")]
mod imp {
    use super::{
        CenterError, WifiObservation, WifiPermission, parse_nmcli, radio_off, unavailable,
    };

    pub const PLATFORM: &str = "linux";

    fn run(rescan: &str) -> Result<(WifiPermission, Vec<WifiObservation>), CenterError> {
        let output = std::process::Command::new("nmcli")
            .args([
                "-t",
                "-e",
                "yes",
                "-f",
                "IN-USE,SSID,SIGNAL,FREQ,SECURITY",
                "device",
                "wifi",
                "list",
                "--rescan",
                rescan,
            ])
            .env("LC_ALL", "C")
            .output()
            .map_err(|_| {
                unavailable("没找到 nmcli（NetworkManager），无法读取本机 Wi-Fi，请直接手动输入")
            })?;
        if !output.status.success() {
            return Err(unavailable(
                "nmcli 读取 Wi-Fi 失败，请确认无线网卡已打开，或直接手动输入",
            ));
        }
        let list = parse_nmcli(&String::from_utf8_lossy(&output.stdout));
        if list.is_empty() && wifi_radio_disabled() {
            return Err(radio_off());
        }
        Ok((WifiPermission::Granted, list))
    }

    fn wifi_radio_disabled() -> bool {
        std::process::Command::new("nmcli")
            .args(["-t", "radio", "wifi"])
            .env("LC_ALL", "C")
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "disabled")
    }

    pub fn current() -> Result<(WifiPermission, Vec<WifiObservation>), CenterError> {
        let (permission, list) = run("no")?;
        Ok((permission, list.into_iter().filter(|o| o.current).collect()))
    }

    pub fn scan() -> Result<(WifiPermission, Vec<WifiObservation>), CenterError> {
        run("yes")
    }
}

// 其他系统（USB 配网只在桌面端编译，这里只是让 crate 在别的目标上也能编过）
#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod imp {
    use super::{CenterError, WifiObservation, WifiPermission, unavailable};

    pub const PLATFORM: &str = "other";

    pub fn current() -> Result<(WifiPermission, Vec<WifiObservation>), CenterError> {
        Err(unavailable("这个系统上读取不了本机 Wi-Fi，请直接手动输入"))
    }

    pub fn scan() -> Result<(WifiPermission, Vec<WifiObservation>), CenterError> {
        current()
    }
}

/// 解析 802.11 Beacon / Probe Response 原始 IE 字节流中的 RSN（Tag 48）AKM 套件，
/// 识别是否“仅 WPA3（只宣告 SAE / FT-SAE，不含 WPA2-PSK / FT-PSK）”。
/// 混合模式（WPA2/WPA3 Transition）同时宣告 PSK 与 SAE，返回 `Some(false)`，不误报。
#[cfg_attr(not(windows), allow(dead_code))]
fn rsn_ie_is_wpa3_only(ies: &[u8]) -> Option<bool> {
    const IEEE_OUI: [u8; 3] = [0x00, 0x0F, 0xAC];
    let mut pos = 0usize;
    let mut saw_rsn = false;
    let mut has_sae = false;
    let mut has_wpa2_psk = false;
    while pos + 2 <= ies.len() {
        let id = ies[pos];
        let len = ies[pos + 1] as usize;
        let body_start = pos + 2;
        let body_end = body_start.checked_add(len)?;
        if body_end > ies.len() {
            break;
        }
        let body = &ies[body_start..body_end];
        pos = body_end;
        if id != 48 || body.len() < 8 {
            continue;
        }
        // RSN version (2B) == 1，Group Cipher (4B)，Pairwise Count (2B)
        if u16::from_le_bytes([body[0], body[1]]) != 1 {
            continue;
        }
        let pairwise_count = u16::from_le_bytes([body[6], body[7]]) as usize;
        let Some(akm_count_off) = pairwise_count.checked_mul(4).and_then(|n| n.checked_add(8))
        else {
            continue;
        };
        if akm_count_off + 2 > body.len() {
            continue;
        }
        let akm_count = u16::from_le_bytes([body[akm_count_off], body[akm_count_off + 1]]) as usize;
        let akm_list_off = akm_count_off + 2;
        let Some(akm_end) = akm_count
            .checked_mul(4)
            .and_then(|n| n.checked_add(akm_list_off))
        else {
            continue;
        };
        if akm_end > body.len() {
            continue;
        }
        saw_rsn = true;
        let (akm_chunks, _) = body[akm_list_off..akm_end].as_chunks::<4>();
        for chunk in akm_chunks {
            if chunk[..3] != IEEE_OUI {
                continue;
            }
            match chunk[3] {
                // IEEE 802.11 OUI 00-0F-AC：2 = WPA2-PSK，4 = FT-PSK（802.11r），6 = PSK-SHA256
                2 | 4 | 6 => has_wpa2_psk = true,
                // 8 = SAE (WPA3-Personal)，9 = FT-SAE，24 = SAE-EXT-KEY
                8 | 9 | 24 => has_sae = true,
                _ => {}
            }
        }
    }
    if !saw_rsn {
        return None;
    }
    Some(has_sae && !has_wpa2_psk)
}

/// 解析 `nmcli -t -e yes -f IN-USE,SSID,SIGNAL,FREQ,SECURITY device wifi list` 的输出：
/// 字段以 `:` 分隔，字段内的 `:` 与 `\` 转义为 `\:`、`\\`。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_nmcli(text: &str) -> Vec<WifiObservation> {
    fn split_escaped(line: &str) -> Vec<String> {
        let mut fields = vec![String::new()];
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        fields.last_mut().expect("非空").push(next);
                    }
                }
                ':' => fields.push(String::new()),
                other => fields.last_mut().expect("非空").push(other),
            }
        }
        fields
    }
    text.lines()
        .filter_map(|line| {
            let fields = split_escaped(line);
            let [in_use, ssid, signal, freq, security] = fields.as_slice() else {
                return None;
            };
            let mhz = freq
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u32>().ok());
            let security = security.trim();
            let is_open = security.is_empty() || security == "--";
            let upper = security.to_ascii_uppercase();
            let has_wpa3 = upper.contains("WPA3") || upper.contains("SAE");
            let has_compat = is_open
                || upper.contains("WPA2")
                || upper.contains("WPA1")
                || upper.contains("WEP");
            let wpa3_only = if has_wpa3 && !has_compat {
                Some(true)
            } else if has_compat {
                Some(false)
            } else {
                None
            };
            Some(WifiObservation {
                ssid: ssid.clone(),
                signal: signal.trim().parse::<u8>().ok().map(|v| v.min(100)),
                band: mhz.and_then(WifiBand::from_mhz),
                secure: Some(!is_open),
                wpa3_only,
                current: in_use.trim() == "*",
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(ssid: &str, signal: u8, band: WifiBand, secure: bool, current: bool) -> WifiObservation {
        WifiObservation {
            ssid: ssid.into(),
            signal: Some(signal),
            band: Some(band),
            secure: Some(secure),
            wpa3_only: None,
            current,
        }
    }

    #[test]
    fn band_from_frequency() {
        assert_eq!(WifiBand::from_mhz(2412), Some(WifiBand::G2_4));
        assert_eq!(WifiBand::from_mhz(2484), Some(WifiBand::G2_4));
        assert_eq!(WifiBand::from_mhz(5180), Some(WifiBand::G5));
        assert_eq!(WifiBand::from_mhz(5885), Some(WifiBand::G5));
        assert_eq!(WifiBand::from_mhz(5955), Some(WifiBand::G6));
        assert_eq!(WifiBand::from_mhz(60_000), None);
    }

    #[test]
    fn rssi_percent_scale() {
        assert_eq!(rssi_to_percent(-30), 100);
        assert_eq!(rssi_to_percent(-50), 100);
        assert_eq!(rssi_to_percent(-75), 50);
        assert_eq!(rssi_to_percent(-100), 0);
        assert_eq!(rssi_to_percent(-120), 0);
    }

    #[test]
    fn merge_dedups_by_ssid_and_sorts_current_first() {
        let merged = merge_observations(vec![
            obs("Studio", 40, WifiBand::G5, true, false),
            obs("Home", 60, WifiBand::G5, true, true),
            obs("Studio", 90, WifiBand::G2_4, true, false),
            obs("Cafe", 70, WifiBand::G2_4, false, false),
            obs("Home", 30, WifiBand::G2_4, true, false),
        ]);
        let names: Vec<_> = merged.iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(names, ["Home", "Studio", "Cafe"]);
        assert_eq!(merged[0].bands, [WifiBand::G2_4, WifiBand::G5]);
        assert_eq!(merged[0].signal, Some(60));
        assert_eq!(merged[0].band_signals[&WifiBand::G2_4], 30);
        assert_eq!(merged[0].band_signals[&WifiBand::G5], 60);
        assert!(merged[0].current);
        assert_eq!(merged[1].signal, Some(90));
        assert_eq!(merged[2].secure, Some(false));
    }

    #[test]
    fn merge_drops_hidden_and_too_long_ssids() {
        let too_long = "x".repeat(33);
        let merged = merge_observations(vec![
            obs("", 80, WifiBand::G2_4, true, false),
            obs("   ", 80, WifiBand::G2_4, true, false),
            obs(&too_long, 80, WifiBand::G2_4, true, false),
            obs(&"y".repeat(32), 10, WifiBand::G2_4, true, false),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].ssid.len(), 32);
    }

    #[test]
    fn merge_secure_is_true_when_any_ap_is_secure() {
        let mut open_entry = obs("Mixed", 50, WifiBand::G2_4, false, false);
        open_entry.secure = None;
        let merged = merge_observations(vec![
            open_entry,
            obs("Mixed", 20, WifiBand::G5, true, false),
        ]);
        assert_eq!(merged[0].secure, Some(true));
    }

    #[test]
    fn parses_nmcli_terse_output_with_escapes() {
        let text = "*:Home\\:5G:78:5180 MHz:WPA2\n \
                    :Cafe:55:2437 MHz:\n \
                    :Back\\\\slash:40:2412 MHz:--\n \
                    ::90:2462 MHz:WPA2\n\
                    garbage line\n";
        let parsed = parse_nmcli(text);
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0].ssid, "Home:5G");
        assert!(parsed[0].current);
        assert_eq!(parsed[0].band, Some(WifiBand::G5));
        assert_eq!(parsed[0].secure, Some(true));
        assert_eq!(parsed[1].ssid, "Cafe");
        assert_eq!(parsed[1].secure, Some(false));
        assert!(!parsed[1].current);
        assert_eq!(parsed[2].ssid, "Back\\slash");
        assert_eq!(parsed[2].secure, Some(false));
        // 隐藏网络在解析层保留，合并时丢弃
        assert_eq!(merge_observations(parsed).len(), 3);
    }

    #[test]
    fn report_hints_for_permission_and_empty_results() {
        let denied = build_report("windows", WifiPermission::Denied, Vec::new(), true);
        assert!(denied.hint.as_deref().is_some_and(|h| h.contains("位置")));
        let pending = build_report("macos", WifiPermission::NotDetermined, Vec::new(), false);
        assert!(
            pending
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("定位服务"))
        );

        let none_connected = build_report("linux", WifiPermission::Granted, Vec::new(), false);
        assert!(
            none_connected
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("没有连 Wi-Fi"))
        );

        let empty_scan = build_report("linux", WifiPermission::Granted, Vec::new(), true);
        assert!(
            empty_scan
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("没有扫描到"))
        );

        let ok = build_report(
            "linux",
            WifiPermission::Granted,
            vec![obs("Home", 70, WifiBand::G2_4, true, true)],
            false,
        );
        assert!(ok.hint.is_none());
        assert_eq!(ok.current.as_ref().map(|c| c.ssid.as_str()), Some("Home"));
        // 只查当前连接时不返回列表
        assert!(ok.networks.is_empty());
    }

    #[test]
    fn settings_uris_are_fixed_per_platform() {
        assert_eq!(
            settings_uri("macos", WifiSettingsTarget::Location),
            Some(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_LocationServices"
            )
        );
        assert_eq!(
            settings_uri("windows", WifiSettingsTarget::Location),
            Some("ms-settings:privacy-location")
        );
        assert_eq!(
            settings_uri("windows", WifiSettingsTarget::Wifi),
            Some("ms-settings:network-wifi")
        );
        assert!(settings_uri("macos", WifiSettingsTarget::Wifi).is_some());
        assert_eq!(settings_uri("linux", WifiSettingsTarget::Location), None);
        assert!(manual_settings_path("macos", WifiSettingsTarget::Wifi).contains("Wi-Fi"));
        assert!(manual_settings_path("windows", WifiSettingsTarget::Location).contains("位置"));
        let target: WifiSettingsTarget = serde_json::from_value(serde_json::json!("wifi")).unwrap();
        assert_eq!(target, WifiSettingsTarget::Wifi);
        assert!(
            serde_json::from_value::<WifiSettingsTarget>(serde_json::json!("x-apple.foo")).is_err()
        );
    }

    #[test]
    fn report_serializes_band_and_permission_names() {
        let report = build_report(
            "macos",
            WifiPermission::Granted,
            vec![
                obs("Home", 70, WifiBand::G2_4, true, true),
                obs("Home", 70, WifiBand::G5, true, false),
            ],
            true,
        );
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["permission"], "granted");
        assert_eq!(value["current"]["bands"], serde_json::json!(["2.4g", "5g"]));
        assert_eq!(value["networks"][0]["ssid"], "Home");
        assert_eq!(
            value["networks"][0]["band_signals"],
            serde_json::json!({"2.4g": 70, "5g": 70})
        );
        assert!(value["networks"][0].get("wpa3_only").is_none());
    }

    fn build_rsn_ie(akm_types: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        // version = 1
        body.extend_from_slice(&1u16.to_le_bytes());
        // group cipher = CCMP (00-0F-AC:04)
        body.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]);
        // 1 pairwise cipher = CCMP
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]);
        // AKM suites
        body.extend_from_slice(&(akm_types.len() as u16).to_le_bytes());
        for &suite in akm_types {
            body.extend_from_slice(&[0x00, 0x0F, 0xAC, suite]);
        }
        let mut ie = vec![48u8, body.len() as u8];
        ie.extend_from_slice(&body);
        ie
    }

    #[test]
    fn rsn_ie_distinguishes_wpa3_only_from_wpa2_and_transition() {
        // 前置一个普通 SSID IE（Tag 0），紧跟 RSN IE（Tag 48）
        let mut wpa3_only = vec![0, 4, b'T', b'e', b's', b't'];
        wpa3_only.extend_from_slice(&build_rsn_ie(&[8]));
        assert_eq!(rsn_ie_is_wpa3_only(&wpa3_only), Some(true));

        let mixed = build_rsn_ie(&[2, 8]);
        assert_eq!(rsn_ie_is_wpa3_only(&mixed), Some(false));

        // 启用 802.11r 快速漫游的 WPA2/WPA3 过渡模式（FT-PSK 4 + FT-SAE 9）不得误报为纯 WPA3
        let ft_mixed = build_rsn_ie(&[4, 9]);
        assert_eq!(rsn_ie_is_wpa3_only(&ft_mixed), Some(false));

        let wpa2_only = build_rsn_ie(&[2]);
        assert_eq!(rsn_ie_is_wpa3_only(&wpa2_only), Some(false));

        // 截断或无 RSN IE 返回 None 且不崩溃
        assert_eq!(rsn_ie_is_wpa3_only(&[0, 3, b'a', b'b', b'c']), None);
        assert_eq!(rsn_ie_is_wpa3_only(&[48, 10, 1, 0]), None);
    }

    #[test]
    fn wpa3_only_parsing_and_merge_behavior() {
        let nm = parse_nmcli(
            ":Pure3:80:2412 MHz:WPA3\n\
             :Mixed:75:2437 MHz:WPA2 WPA3\n\
             :Plain2:70:2462 MHz:WPA2\n\
             :OpenNet:65:2412 MHz:--\n",
        );
        assert_eq!(nm[0].wpa3_only, Some(true));
        assert_eq!(nm[1].wpa3_only, Some(false));
        assert_eq!(nm[2].wpa3_only, Some(false));
        assert_eq!(nm[3].wpa3_only, Some(false));

        let mut pure = obs("Corp", 60, WifiBand::G2_4, true, false);
        pure.wpa3_only = Some(true);
        let unknown = obs("Corp", 40, WifiBand::G2_4, true, false);
        assert_eq!(
            merge_observations(vec![pure.clone(), unknown])[0].wpa3_only,
            Some(true)
        );

        let mut compat = obs("Corp", 50, WifiBand::G2_4, true, false);
        compat.wpa3_only = Some(false);
        assert_eq!(
            merge_observations(vec![pure, compat])[0].wpa3_only,
            Some(false)
        );
    }
}
