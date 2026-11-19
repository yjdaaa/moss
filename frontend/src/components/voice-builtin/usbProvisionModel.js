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
