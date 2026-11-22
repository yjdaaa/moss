/** 无弹窗内联小智 USB 一键配网卡片（Wi-Fi 配置 / 高级选项双标签）的纯逻辑、校验与请求构造。 */

export const MAX_TX_POWER_OPTIONS = [
  { value: 0, label: "默认（自动）" },
  { value: 8, label: "2 dBm" },
  { value: 20, label: "5 dBm" },
  { value: 28, label: "7 dBm" },
  { value: 34, label: "8.5 dBm" },
  { value: 44, label: "11 dBm" },
  { value: 52, label: "13 dBm" },
  { value: 56, label: "14 dBm" },
  { value: 60, label: "15 dBm" },
  { value: 66, label: "16.5 dBm" },
  { value: 72, label: "18 dBm" },
  { value: 80, label: "20 dBm" },
];

const encoder = new TextEncoder();

export function utf8ByteLength(value) {
  return encoder.encode(String(value || "")).length;
}

export const ESPRESSIF_USB_VID = 0x303a;

export function pickDefaultUsbPort(ports) {
  if (!Array.isArray(ports) || ports.length === 0) return "";
  const usable = ports.filter((p) => !p.is_laser_port);
  // 乐鑫原生 USB 口优先：CH340 / CP210x 这类转换芯片激光控制器也常用
  const preferred =
    usable.find((p) => p.vid === ESPRESSIF_USB_VID) || usable.find((p) => p.is_likely_esp);
  return (preferred || usable[0] || {}).port_name || "";
}

/** 选中的串口不是乐鑫原生 USB：读取前要提醒用户确认插着的是小智（读取会让该口上的板子重启）。 */
export function needsBoardIdentityWarning(port) {
  return Boolean(port) && port.vid !== ESPRESSIF_USB_VID;
}

/**
 * 与小智固件所用 esp-wifi-connect 的 `wifi_config::IsValidOtaUrl` 同一套规则（后端 `nvs::is_valid_ota_url` 一致）：
 * 固件会忽略不合规的地址并退回官方默认 OTA。
 */
export function isValidOtaUrl(url) {
  const text = String(url ?? "");
  if (!text) return true;
  if (utf8ByteLength(text) > 255) return false;
  const lower = text.toLowerCase();
  let rest;
  if (lower.startsWith("http://")) rest = lower.slice(7);
  else if (lower.startsWith("https://")) rest = lower.slice(8);
  else return false;
  for (let i = 0; i < text.length; i += 1) {
    const code = text.charCodeAt(i);
    if (code <= 0x20 || code >= 0x7f || text[i] === "\\") return false;
    if (text[i] === "%" && !/^[0-9a-fA-F]{2}$/.test(text.slice(i + 1, i + 3))) return false;
  }
  const authority = rest.split(/[/?#]/)[0];
  const hostPort = authority.slice(authority.lastIndexOf("@") + 1);
  let host = hostPort;
  let port = null;
  if (hostPort.startsWith("[")) {
    const end = hostPort.indexOf("]");
    if (end < 0) return false;
    host = hostPort.slice(1, end);
    const tail = hostPort.slice(end + 1);
    if (tail.startsWith(":")) port = tail.slice(1);
  } else if (hostPort.includes(":")) {
    const at = hostPort.lastIndexOf(":");
    host = hostPort.slice(0, at);
    port = hostPort.slice(at + 1);
  }
  if (!host) return false;
  if (port === null) return true;
  if (!/^\d+$/.test(port)) return false;
  const number = Number(port);
  return number >= 1 && number <= 65535;
}

/** 板型默认（设备上没有这个键）用空字符串表示；写入时擦除该键（后端 -1）。 */
function minutesField(value) {
  return Number.isInteger(value) ? String(value) : "";
}

export function createDraftFromInspect(inspectData, serviceStatus) {
  const nvs = inspectData?.nvs || {};
  const wifiSlots = Array.isArray(nvs.wifi_slots) ? nvs.wifi_slots : [];
  const existingOta = typeof nvs.ota_url === "string" ? nvs.ota_url : "";
  const suggestedOta = serviceStatus?.ota_ip_url || serviceStatus?.ota_url || "";
  const advanced = {
    maxTxPower: Number.isInteger(nvs.max_tx_power) ? nvs.max_tx_power : 0,
    rememberBssid: Boolean(nvs.remember_bssid),
    // 设备上没有 sleep_mode 键时固件按“开启”处理（power_save_timer.cc / sleep_timer.cc 默认 true）
    sleepMode: typeof nvs.sleep_mode === "boolean" ? nvs.sleep_mode : true,
    // 空字符串 = 板型默认时间（固件内部 -1）；"0" = 关掉这一步
    sleepDimMin: minutesField(nvs.sleep_dim_min),
    sleepOffMin: minutesField(nvs.sleep_off_min),
  };
  return {
    savedSlots: wifiSlots.map((slot) => ({
      slot: slot.slot,
      ssid: slot.ssid,
      has_password: Boolean(slot.has_password),
      channel: slot.channel ?? null,
    })),
    originalSlotIds: wifiSlots.map((slot) => slot.slot),
    newSsid: "",
    newPassword: "",
    otaUrl: existingOta || suggestedOta,
    ...advanced,
    // 读取时的高级选项：写入时只提交和它不同的项（与官方配网页一致），不顺手改掉没动过的设置
    advancedOriginal: advanced,
    // 默认不勾（项目规则：新的授权能力由用户显式开启）
    autoAllow: false,
    unrevoke: false,
  };
}

/** 等待分钟输入框的值 → 后端字段：空 = -1（擦除键回到板型默认），否则整数分钟。 */
function minutesForPatch(text) {
  const trimmed = String(text ?? "").trim();
  return trimmed === "" ? -1 : Number(trimmed);
}

export function pinSavedWifiSlot(savedSlots, index) {
  if (!Array.isArray(savedSlots) || index <= 0 || index >= savedSlots.length) {
    return savedSlots || [];
  }
  const next = [...savedSlots];
  const [picked] = next.splice(index, 1);
  next.unshift(picked);
  return next;
}

export function removeSavedWifiSlot(savedSlots, index) {
  if (!Array.isArray(savedSlots) || index < 0 || index >= savedSlots.length) {
    return savedSlots || [];
  }
  return savedSlots.filter((_, idx) => idx !== index);
}

/**
 * 本机 Wi-Fi 一键填写 / 扫描附近（2026-11-22，技术书 n103）。后端只给 SSID、信号、频段与是否加密，不给密码。
 * 小智常见芯片（ESP32 / S3 / C3 / C6）只支持 2.4 GHz，ESP32-C5 另支持 5 GHz。
 */
export function boardSupports5g(chip) {
  return /c5/i.test(String(chip || ""));
}

const WIFI_BAND_LABELS = { "2.4g": "2.4 GHz", "5g": "5 GHz", "6g": "6 GHz" };

export function wifiBandLabel(band) {
  return WIFI_BAND_LABELS[band] || String(band || "");
}

/** 板子能连的频段：ESP32-C5 双频（2.4 / 5 GHz），其余小智常见芯片只有 2.4 GHz；还没读板子时按 2.4 GHz。 */
export function boardWifiBands(chip) {
  return boardSupports5g(chip) ? ["2.4g", "5g"] : ["2.4g"];
}

/**
 * 按板子筛扫描结果（2026-11-22 用户要求“根据对应的板子来显示”）：只留板子能连的频段，
 * 频段标签与信号也只按这些频段算（双频同名网络里 2.4 GHz 往往更弱）；系统没给频率的网络保留。
 * 返回 `{ shown, hiddenCount }`，排序为电脑当前在前、再按信号强弱。
 */
export function filterScannedWifiForBoard(networks, chip) {
  const allowed = boardWifiBands(chip);
  const shown = [];
  let hiddenCount = 0;
  for (const network of Array.isArray(networks) ? networks : []) {
    const bands = Array.isArray(network?.bands) ? network.bands : [];
    if (bands.length === 0) {
      shown.push({ ...network, bands: [] });
      continue;
    }
    const usable = bands.filter((band) => allowed.includes(band));
    if (usable.length === 0) {
      hiddenCount += 1;
      continue;
    }
    const perBand = usable
      .map((band) => network.band_signals?.[band])
      .filter((value) => Number.isInteger(value));
    shown.push({
      ...network,
      bands: usable,
      signal: perBand.length ? Math.max(...perBand) : network.signal,
    });
  }
  shown.sort(
    (a, b) =>
      Number(Boolean(b.current)) - Number(Boolean(a.current)) ||
      (b.signal ?? -1) - (a.signal ?? -1),
  );
  return { shown, hiddenCount };
}

/** 扫描列表表头：说明按哪块板子筛的。 */
export function scanFilterLabel(chip, hiddenCount) {
  const bands = boardWifiBands(chip).map(wifiBandLabel).join(" / ");
  const base = chip
    ? `按板子 ${chip} 只显示 ${bands}`
    : `还没读取板子，先只显示 ${bands}（读取后按芯片调整）`;
  return hiddenCount > 0 ? `${base}，已隐藏 ${hiddenCount} 个板子连不上的网络` : base;
}

/** 一键填入电脑当前 Wi-Fi 后的提示：电脑连的频段板子不支持或网络为仅 WPA3 时提前提醒。 */
export function currentWifiFillNotice(network, chip) {
  if (!network?.ssid) return "";
  const bands = Array.isArray(network.bands) ? network.bands : [];
  const allowed = boardWifiBands(chip);
  const wpa3Tail = network.wpa3_only
    ? " 另外该网络为“仅 WPA3”，部分小智 ESP32 固件只兼容 WPA2，如连不上请到路由器改为 WPA2-PSK 或 WPA2/WPA3 混合。"
    : "";
  if (bands.length > 0 && !bands.some((band) => allowed.includes(band))) {
    const boardText = chip
      ? `这块板子（${chip}）只支持 ${allowed.map(wifiBandLabel).join(" / ")}`
      : "小智常见芯片只支持 2.4 GHz（ESP32-C5 另支持 5 GHz）";
    return `已填入电脑当前连接的“${network.ssid}”。电脑连的是 ${bands
      .map(wifiBandLabel)
      .join(" / ")}，${boardText}：路由器各频段同名就没问题，否则请改填板子能连的网络名（可点“扫描附近”查看）。密码请手动填写。${wpa3Tail}`;
  }
  return `已填入电脑当前连接的“${network.ssid}”，密码请手动填写${
    network.secure === false ? "（这是开放网络，可留空）" : ""
  }。${wpa3Tail}`;
}

/** 选中扫描列表中的 Wi-Fi 后的提示：纯 WPA3 网络提前预警。 */
export function pickedWifiNotice(network) {
  const ssid = String(network?.ssid || "");
  if (!ssid) return { tone: "ok", text: "" };
  if (network.wpa3_only) {
    return {
      tone: "warn",
      text: `已选“${ssid}”（仅 WPA3）：部分小智 ESP32 固件只兼容 WPA2，如连不上请到路由器把 2.4 GHz 改为 WPA2-PSK 或 WPA2/WPA3 混合；请填写它的密码。`,
    };
  }
  return {
    tone: "ok",
    text: `已选“${ssid}”，${
      network.secure === false ? "这是开放网络，密码可留空" : "请填写它的密码"
    }。`,
  };
}

/**
 * 读本机 Wi-Fi 遇到系统层面的阻碍时，弹窗要说什么、“去系统设置打开”跳哪一页（2026-11-22 用户要求弹窗）。
 * 没授权定位 → 定位权限页；电脑 Wi-Fi 开关关着（`wifi_radio_off`）→ Wi-Fi 设置页；其余返回 null。
 */
export function hostWifiProblem(result, error) {
  if (error?.code === "wifi_radio_off") {
    return {
      target: "wifi",
      permission: "",
      title: "电脑的 Wi-Fi 没打开",
      message: error.message || "电脑的 Wi-Fi 是关着的，打开 Wi-Fi 后才能读取附近的网络名称。",
    };
  }
  const permission = result?.permission;
  if (permission && permission !== "granted") {
    return {
      target: "location",
      permission,
      title: "需要定位权限才能读取 Wi-Fi 名称",
      message: result.hint || "系统要求应用有定位权限才给出 Wi-Fi 名称（MOSS 不读取、不保存位置）。",
    };
  }
  return null;
}

/** 只有 macOS / Windows 有可以直接跳转的系统设置页。 */
export function canOpenWifiSettings(os) {
  return os === "macos" || os === "windows";
}

/** 选中一个网络填进“新的 Wi-Fi”：换了网络名就清掉之前填的密码，免得把别的网络的密码写进去。 */
export function applyPickedWifi(draft, network) {
  const ssid = String(network?.ssid || "");
  if (!ssid) return draft;
  return {
    ...draft,
    newSsid: ssid,
    newPassword: ssid === draft.newSsid ? draft.newPassword : "",
  };
}

export function validateUsbProvisionDraft(draft, { includeNewWifi = true } = {}) {
  const ssid = String(draft?.newSsid || "").trim();
  const password = String(draft?.newPassword || "");

  if (includeNewWifi && ssid) {
    if (utf8ByteLength(ssid) > 32) {
      return "Wi-Fi 名称（SSID）UTF-8 长度不能超过 32 字节";
    }
    if (utf8ByteLength(password) > 64) {
      return "Wi-Fi 密码 UTF-8 长度不能超过 64 字节";
    }
  } else if (includeNewWifi && password && !ssid) {
    return "填写 Wi-Fi 密码时请同时填写 Wi-Fi 名称（SSID）";
  }

  const ota = String(draft?.otaUrl || "").trim();
  if (ota) {
    if (utf8ByteLength(ota) > 255) {
      return "自定义 OTA 地址不能超过 255 字节";
    }
    if (!isValidOtaUrl(ota)) {
      return "OTA 地址不合规：须以 http:// 或 https:// 开头、带主机名，不能含空格、中文、反斜杠或不完整的 % 编码（小智固件会忽略这样的地址）";
    }
  }

  const dim = minutesForPatch(draft?.sleepDimMin);
  const off = minutesForPatch(draft?.sleepOffMin);
  if (dim !== -1 && (!Number.isInteger(dim) || dim < 0 || dim > 1440)) {
    return "变暗等待时间必须为 0～1440 分钟的整数（留空 = 板型默认）";
  }
  if (off !== -1 && (!Number.isInteger(off) || off < 0 || off > 1440)) {
    return "关屏等待时间必须为 0～1440 分钟的整数（留空 = 板型默认）";
  }
  if (dim > 0 && off > 0 && off <= dim) {
    return "关屏等待时间（分钟）必须大于变暗等待时间";
  }

  return "";
}

function parseUrlInfo(urlText) {
  const trimmed = String(urlText || "").trim();
  if (!trimmed) return null;
  try {
    const parsed = new URL(trimmed);
    const port = parsed.port
      ? Number(parsed.port)
      : parsed.protocol === "https:"
        ? 443
        : parsed.protocol === "http:"
          ? 80
          : 0;
    return {
      protocol: parsed.protocol,
      hostname: parsed.hostname.toLowerCase(),
      port,
      pathname: parsed.pathname,
    };
  } catch {
    return null;
  }
}

export function isRecognizedXiaozhiInspect(inspectData) {
  if (!inspectData || typeof inspectData !== "object") return false;
  if (inspectData.partition?.has_xiaozhi_markers) return true;
  const namespaces = Array.isArray(inspectData.nvs?.namespaces)
    ? inspectData.nvs.namespaces
    : [];
  return namespaces.includes("board") || namespaces.includes("websocket");
}

/** OTA 地址比较用的规范形式：协议与主机不分大小写、默认端口省略、路径末尾斜杠不计。 */
function normalizeOtaForCompare(urlText) {
  const trimmed = String(urlText || "").trim();
  if (!trimmed) return "";
  try {
    const parsed = new URL(trimmed);
    return `${parsed.protocol}//${parsed.host}${parsed.pathname.replace(/\/+$/, "")}${parsed.search}`.toLowerCase();
  } catch {
    return trimmed.toLowerCase();
  }
}

/** MOSS 内置服务当前能用的 OTA 地址（本机局域网各网卡 IP 与 moss.local）。 */
export function mossOtaUrls(serviceStatus) {
  return [
    serviceStatus?.ota_ip_url,
    serviceStatus?.ota_url,
    ...(Array.isArray(serviceStatus?.ota_ip_urls) ? serviceStatus.ota_ip_urls : []),
  ].filter((url) => typeof url === "string" && url.trim());
}

/** 高级选项里“填入本机地址”快捷按钮列表：列出所有真实局域网 IP（有线/默认出网优先）与 moss.local。 */
export function mossOtaQuickFillOptions(serviceStatus) {
  const mdns = typeof serviceStatus?.ota_url === "string" ? serviceStatus.ota_url.trim() : "";
  const rawIps = [
    serviceStatus?.ota_ip_url,
    ...(Array.isArray(serviceStatus?.ota_ip_urls) ? serviceStatus.ota_ip_urls : []),
  ].filter((url) => typeof url === "string" && url.trim());
  const seen = new Set();
  const ipUrls = [];
  for (const url of rawIps) {
    const trimmed = url.trim();
    const key = normalizeOtaForCompare(trimmed);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    ipUrls.push(trimmed);
  }
  const options = ipUrls.map((url, idx) => {
    const host = parseUrlInfo(url)?.hostname || url;
    const label =
      ipUrls.length === 1
        ? "填入本机局域网 IP"
        : idx === 0
          ? `填入 ${host}（首选）`
          : `填入 ${host}`;
    return { url, label, kind: "ip" };
  });
  const mdnsKey = normalizeOtaForCompare(mdns);
  if (mdns && mdnsKey && !seen.has(mdnsKey)) {
    const mdnsHost = parseUrlInfo(mdns)?.hostname || "";
    const label = mdnsHost.endsWith(".local") ? `填入 ${mdnsHost} 地址` : "填入 MOSS 默认地址";
    options.push({ url: mdns, label, kind: "mdns" });
  }
  return options;
}

export const USB_CHECKIN_TIMEOUT_MS = 20_000;
export const USB_CHECKIN_POLL_MS = 2_000;

/** 读/写 Flash 过程中的分阶段提示，避免握手重试或回读校验期间用户误以为卡死。 */
export function usbOperationStageHint(busy, elapsedSec = 0) {
  const sec = Math.max(0, Math.floor(Number(elapsedSec) || 0));
  if (busy === "inspect") {
    if (sec < 3) return "正在握手小智芯片并进入下载模式…";
    if (sec < 8) {
      return "正在读取分区表与 NVS 配置（若板子未自动进下载模式，可按住 BOOT 键后重试）…";
    }
    return `仍在等待芯片响应（已等待 ${sec} 秒）；请确认数据线支持传输数据，或按住 BOOT 键重试。`;
  }
  if (busy === "write") {
    if (sec < 2) return "正在连接芯片并核对 MAC 与分区表…";
    if (sec < 6) return "正在写入 NVS 分区并执行芯片侧 MD5 回读校验…";
    return `正在完成回读校验并重启小智（已等待 ${sec} 秒），期间请勿拔掉 USB 线…`;
  }
  return "";
}

/**
 * 写入完成后核对这块板子是否已连上 Wi-Fi 并向 MOSS 报到。
 * 区分：已在线（online）、OTA 已报到待允许（pending）、已撤销（revoked）、OTA 已报到正连语音（ota_seen）、
 * 仅完成 OTA 后超时（checked_in）、服务未启动（service_stopped）、P4 手动核对（p4_manual）、等待中（waiting）与超时（timeout）。
 */
export function evaluateBoardCheckin(serviceStatus, tracker) {
  if (!tracker || !tracker.mac) return null;
  if (serviceStatus?.running === false) {
    return {
      phase: "service_stopped",
      tone: "warn",
      text: "MOSS 内置语音服务当前未运行，小智开机后无法报到；请先启动内置服务。",
    };
  }
  if (String(tracker.chip || "").toLowerCase() === "esp32p4") {
    return {
      phase: "p4_manual",
      tone: "info",
      text: "ESP32-P4 上报的 Wi-Fi MAC 与 USB 芯片 MAC 不同，请在上方「已连接设备」查看它是否上线。",
    };
  }
  const mac = String(tracker.mac).toLowerCase();
  const sessions = Array.isArray(serviceStatus?.sessions) ? serviceStatus.sessions : [];
  const preSessions = new Set(Array.isArray(tracker.preSessionIds) ? tracker.preSessionIds : []);
  const liveSession = sessions.find(
    (s) =>
      String(s?.device_id || "").toLowerCase() === mac &&
      (!s?.session_id || !preSessions.has(s.session_id)),
  );
  if (liveSession) {
    return {
      phase: "online",
      tone: "ok",
      text: "小智已连上 Wi-Fi 并上线 MOSS，现在可以直接对话了。",
    };
  }
  const devices = Array.isArray(serviceStatus?.devices) ? serviceStatus.devices : [];
  const device = devices.find((d) => String(d?.device_id || "").toLowerCase() === mac);
  const lastSeen = Number(device?.last_seen || 0);
  const baseline = Number(tracker.baselineLastSeen || 0);
  const hasRealIp = Boolean(String(device?.last_ip || "").trim());
  const freshOta = Boolean(device) && hasRealIp && lastSeen > baseline;
  if (freshOta) {
    if (device.status === "pending") {
      return {
        phase: "pending",
        tone: "warn",
        text: "小智已连上 Wi-Fi 并向 MOSS 报到！当前为“待允许”，请在上方「已连接设备」点允许。",
      };
    }
    if (device.status === "revoked") {
      return {
        phase: "revoked",
        tone: "warn",
        text: "小智已连上 Wi-Fi 并向 MOSS 报到，但当前为“已撤销”，请在上方「已连接设备」重新允许。",
      };
    }
    if (tracker.timedOut) {
      return {
        phase: "checked_in",
        tone: "ok",
        text: "小智已连上 Wi-Fi 并完成向 MOSS 报到（语音连接可能稍后就绪，可在上方「已连接设备」确认）。",
      };
    }
    return {
      phase: "ota_seen",
      tone: "info",
      text: "小智已连上 Wi-Fi 并向 MOSS 报到，正在等待语音连接就绪…",
    };
  }
  if (tracker.timedOut) {
    return {
      phase: "timeout",
      tone: "warn",
      text: "等待 20 秒仍未收到小智报到。请排查：Wi-Fi 密码是否输错、路由器 2.4 GHz 是否开启且未开 AP 隔离、电脑防火墙是否放行 MOSS，或按一下板子 RST 键重试。",
    };
  }
  return {
    phase: "waiting",
    tone: "info",
    text: "正在等待小智重启连网并向 MOSS 报到（约 5～15 秒）…",
  };
}

/**
 * 读出的板子 OTA 地址已不是 MOSS 当前的任何一个地址（电脑换了 IP、改了端口等）时返回
 * `{ boardUrl, mossUrl }`，界面据此提醒并给“换成 MOSS 当前地址”（2026-11-22 用户决定，默认不自动替换，
 * 因为也可能是故意指向别的服务器）。输入框已被改掉、板子没存过地址或服务没给出地址时返回 null。
 */
export function staleBoardOta(inspectData, draftOtaUrl, serviceStatus) {
  const boardUrl = typeof inspectData?.nvs?.ota_url === "string" ? inspectData.nvs.ota_url.trim() : "";
  if (!boardUrl || String(draftOtaUrl || "").trim() !== boardUrl) return null;
  const known = mossOtaUrls(serviceStatus);
  if (known.length === 0) return null;
  const target = normalizeOtaForCompare(boardUrl);
  if (known.some((url) => normalizeOtaForCompare(url) === target)) return null;
  return { boardUrl, mossUrl: known[0] };
}

export function deriveWsUrlForMoss(otaUrl, serviceStatus) {
  const trimmed = String(otaUrl || "").trim();
  const wsPort = Number(serviceStatus?.ws_port || 0);
  if (!trimmed || !wsPort) return "";
  try {
    const parsed = new URL(trimmed);
    const knownUrls = [
      serviceStatus?.ota_ip_url,
      serviceStatus?.ota_url,
      ...(Array.isArray(serviceStatus?.ota_ip_urls) ? serviceStatus.ota_ip_urls : []),
    ].filter(Boolean);
    const matchesExact = knownUrls.includes(trimmed);
    const knownHosts = new Set(["localhost", "127.0.0.1", "[::1]"]);
    for (const candidate of knownUrls) {
      const info = parseUrlInfo(candidate);
      if (info?.hostname) knownHosts.add(info.hostname);
    }
    const expectedHttpPort =
      Number(serviceStatus?.http_port || 0) ||
      parseUrlInfo(serviceStatus?.ota_ip_url)?.port ||
      parseUrlInfo(serviceStatus?.ota_url)?.port ||
      0;
    const isHttp = parsed.protocol === "http:";
    const isOtaPath =
      parsed.pathname === "/xiaozhi/ota/" || parsed.pathname === "/xiaozhi/ota";
    const parsedInfo = parseUrlInfo(trimmed);
    const matchesManualNic =
      isHttp &&
      isOtaPath &&
      expectedHttpPort > 0 &&
      parsedInfo?.port === expectedHttpPort &&
      knownHosts.has(parsed.hostname.toLowerCase());
    if (!matchesExact && !matchesManualNic) return "";
    const proto = parsed.protocol === "https:" ? "wss:" : "ws:";
    return `${proto}//${parsed.hostname}:${wsPort}/xiaozhi/v1/`;
  } catch {
    return "";
  }
}

export function buildUsbProvisionWritePayload({
  port,
  inspectData,
  draft,
  wsUrl = "",
}) {
  // 与官方配网页一致按原样保存 SSID（首尾空格也是名字的一部分），只有全空白才当作没填
  const rawSsid = String(draft.newSsid || "");
  const ssid = rawSsid.trim() ? rawSsid : "";
  const slotIds = (draft.savedSlots || []).map((s) => s.slot);
  const slotsChanged =
    slotIds.length !== (draft.originalSlotIds || []).length ||
    slotIds.some((id, idx) => id !== draft.originalSlotIds[idx]);
  const trimmedOta = String(draft.otaUrl || "").trim();

  const patch = { ota_url: trimmedOta };
  // 高级选项只提交改过的项：没动过的保持设备原值（尤其出厂没设过的休眠，不能被写成“关闭”）
  const original = draft.advancedOriginal || {};
  if (Number(draft.maxTxPower || 0) !== Number(original.maxTxPower || 0)) {
    patch.max_tx_power = Number(draft.maxTxPower || 0);
  }
  if (Boolean(draft.rememberBssid) !== Boolean(original.rememberBssid)) {
    patch.remember_bssid = Boolean(draft.rememberBssid);
  }
  if (Boolean(draft.sleepMode) !== Boolean(original.sleepMode ?? true)) {
    patch.sleep_mode = Boolean(draft.sleepMode);
  }
  if (String(draft.sleepDimMin ?? "").trim() !== String(original.sleepDimMin ?? "").trim()) {
    patch.sleep_dim_min = minutesForPatch(draft.sleepDimMin);
  }
  if (String(draft.sleepOffMin ?? "").trim() !== String(original.sleepOffMin ?? "").trim()) {
    patch.sleep_off_min = minutesForPatch(draft.sleepOffMin);
  }

  if (slotsChanged || ssid) {
    patch.retain_slots = slotIds;
  }
  if (ssid) {
    patch.new_wifi = {
      ssid,
      password: String(draft.newPassword || ""),
    };
  }
  const autoAllow = Boolean(trimmedOta && wsUrl && draft.autoAllow);
  // 清除 OTA、改填外部服务地址或取消自动放行时一并清掉旧 websocket:url，防止板子继续连回旧地址
  patch.ws_url = autoAllow ? wsUrl : "";

  const payload = {
    action: "write",
    port,
    expected_mac: inspectData?.device?.mac_address || "",
    confirmed: true,
    auto_allow: autoAllow,
    unrevoke: Boolean(autoAllow && draft.unrevoke),
    patch,
  };
  if (typeof inspectData?.known_device?.status_revision === "number") {
    payload.expected_status_revision = inspectData.known_device.status_revision;
  }
  return payload;
}

/** 读取到的设备在 MOSS 里是否被撤销过（此时自动允许需要单独确认“解除撤销”）。 */
export function isRevokedInMoss(inspectData) {
  return inspectData?.known_device?.status === "revoked";
}

export function formatUsbWriteConfirmText({ port, inspectData, payload }) {
  const chip = inspectData?.device?.chip || "ESP32";
  const mac = inspectData?.device?.mac_address || "未知 MAC";
  const patch = payload?.patch || {};
  const lines = [
    `目标串口：${port}`,
    `设备芯片：${chip}（MAC：${mac}）`,
  ];
  if (patch.new_wifi?.ssid) {
    lines.push(`新增并置顶 Wi-Fi：${patch.new_wifi.ssid}`);
  } else if (Array.isArray(patch.retain_slots)) {
    lines.push(`更新已保存 Wi-Fi 列表（保留 ${patch.retain_slots.length} 条，原密码不变）`);
  }
  if (patch.ota_url) {
    lines.push(`写入 OTA 地址：${patch.ota_url}`);
  } else {
    lines.push("OTA 地址：擦除自定义键，恢复固件默认");
  }
  if (payload?.auto_allow) {
    lines.push(
      payload.unrevoke
        ? "设备授权：这台设备在 MOSS 里被撤销过，将解除撤销并设为“已允许”"
        : "设备授权：写入预签发密钥并在 MOSS 自动设为“已允许”",
    );
  }
  const changed = [];
  if ("max_tx_power" in patch) changed.push("发射功率");
  if ("remember_bssid" in patch) changed.push("记住 BSSID");
  if ("sleep_mode" in patch || "sleep_dim_min" in patch || "sleep_off_min" in patch) {
    changed.push("休眠设置");
  }
  if (changed.length) lines.push(`同时修改：${changed.join("、")}`);
  lines.push("");
  lines.push("写入后会回读校验，失败会自动写回原配置；随后小智自动重启生效。确认继续写入板子 Flash 吗？");
  return lines.join("\n");
}
