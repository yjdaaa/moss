import React, { useCallback, useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  ArrowUp,
  CheckCircle2,
  Eye,
  EyeOff,
  History,
  Laptop,
  Lock,
  LockOpen,
  Play,
  RefreshCw,
  ScanSearch,
  Sliders,
  Trash2,
  Usb,
  Wifi,
} from "lucide-react";
import { showChoice } from "../../appDialog.js";
import { detectClientOs } from "../../platform.js";
import { voiceApi } from "../../voiceApi.js";
import { voiceErrorText } from "../../voiceState.js";
import {
  MAX_TX_POWER_OPTIONS,
  USB_CHECKIN_POLL_MS,
  USB_CHECKIN_TIMEOUT_MS,
  applyPickedWifi,
  buildUsbProvisionWritePayload,
  canOpenWifiSettings,
  createDraftFromInspect,
  currentWifiFillNotice,
  deriveWsUrlForMoss,
  evaluateBoardCheckin,
  filterScannedWifiForBoard,
  hostWifiProblem,
  formatUsbWriteConfirmText,
  isRecognizedXiaozhiInspect,
  isRevokedInMoss,
  mossOtaQuickFillOptions,
  needsBoardIdentityWarning,
  pickDefaultUsbPort,
  pickedWifiNotice,
  pinSavedWifiSlot,
  removeSavedWifiSlot,
  utf8ByteLength,
  scanFilterLabel,
  staleBoardOta,
  usbOperationStageHint,
  validateUsbProvisionDraft,
  wifiBandLabel,
} from "./usbProvisionModel.js";

const ESPRESSIF_USB_VID = 0x303a;
const PORT_AUTOSCAN_MS = 4_000;

// 仅在当前进程内存中记住最近一次成功写入的 Wi-Fi，方便连续给多台小智配网；关闭或刷新即清，不落盘
let lastWrittenWifiSession = null;

export function clearLastWrittenWifiSession() {
  lastWrittenWifiSession = null;
}

const DEVICE_STATUS_LABELS = {
  allowed: "已允许",
  pending: "待允许",
  revoked: "已撤销",
};

/** 无弹窗内联小智 USB 一键配网写地址卡片（MOSS 曜石暗色双标签风格）。 */
export function XiaozhiUsbProvisionCard({ serviceStatus, onSetStatus }) {
  const [tab, setTab] = useState("wifi");
  const [ports, setPorts] = useState([]);
  const [laserBusy, setLaserBusy] = useState(false);
  const [selectedPort, setSelectedPort] = useState("");
  const [busy, setBusy] = useState("");
  const [busyElapsedSec, setBusyElapsedSec] = useState(0);
  const [errText, setErrText] = useState("");
  const [noticeText, setNoticeText] = useState("");
  const [inspectData, setInspectData] = useState(null);
  const [draft, setDraft] = useState(() => createDraftFromInspect(null, serviceStatus));
  const [showPassword, setShowPassword] = useState(false);
  const [pendingWrite, setPendingWrite] = useState(null);
  const [startingService, setStartingService] = useState(false);
  const [checkin, setCheckin] = useState(null);
  const [lastWifi, setLastWifi] = useState(() => lastWrittenWifiSession);
  // 本机 Wi-Fi 读取：不占后端配网锁，与读板子 / 写入互不阻塞，单独记忙碌状态
  const [wifiBusy, setWifiBusy] = useState("");
  const [wifiNotice, setWifiNotice] = useState(null);
  const [wifiScan, setWifiScan] = useState(null);
  const wifiSeqRef = useRef(0);
  const ssidInputRef = useRef(null);
  // 去系统设置后切回 MOSS 时要自动重查的动作（wifi_current / wifi_scan）
  const pendingRecheckRef = useRef("");
  const lastWifiActionRef = useRef("wifi_current");
  // macOS 第一次“还没问过”时系统会自己弹授权框，不再叠一个 MOSS 弹窗
  const systemPromptSeenRef = useRef(false);
  const queryHostWifiRef = useRef(null);
  const wifiBusyRef = useRef("");
  wifiBusyRef.current = wifiBusy;

  const mountedRef = useRef(true);
  const inFlightRef = useRef(false);
  const silentPortsInFlightRef = useRef(false);
  const requestSeqRef = useRef(0);
  const userPickedPortRef = useRef(false);
  const cardRef = useRef(null);
  const portSelectRef = useRef(null);
  const portsRef = useRef(ports);
  portsRef.current = ports;
  const busyRef = useRef(busy);
  busyRef.current = busy;
  const inspectDataRef = useRef(inspectData);
  inspectDataRef.current = inspectData;
  const selectedPortRef = useRef(selectedPort);
  selectedPortRef.current = selectedPort;
  const serviceStatusRef = useRef(serviceStatus);
  serviceStatusRef.current = serviceStatus;
  const otaTouchedRef = useRef(false);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      inFlightRef.current = false;
      silentPortsInFlightRef.current = false;
      requestSeqRef.current += 1;
    };
  }, []);

  // 读取 / 写入期间按秒更新分阶段提示
  useEffect(() => {
    if (busy !== "inspect" && busy !== "write") {
      setBusyElapsedSec(0);
      return;
    }
    setBusyElapsedSec(0);
    const timer = window.setInterval(() => {
      setBusyElapsedSec((prev) => prev + 1);
    }, 1000);
    return () => window.clearInterval(timer);
  }, [busy]);

  // 服务状态晚于卡片挂载返回时，若尚未读取板子且用户未手动改过 OTA，同步填入默认本机地址
  useEffect(() => {
    const suggested = serviceStatus?.ota_ip_url || serviceStatus?.ota_url || "";
    if (!suggested || inspectData || otaTouchedRef.current) return;
    setDraft((prev) => (prev.otaUrl ? prev : { ...prev, otaUrl: suggested }));
  }, [serviceStatus?.ota_ip_url, serviceStatus?.ota_url, inspectData]);

  const otaIpUrlsKey = Array.isArray(serviceStatus?.ota_ip_urls)
    ? serviceStatus.ota_ip_urls.join("|")
    : "";

  // 服务端端口或本机 OTA 地址变更后，撤销旧端口计算出的待确认写入状态
  useEffect(() => {
    setPendingWrite(null);
  }, [
    serviceStatus?.ws_port,
    serviceStatus?.http_port,
    serviceStatus?.ota_ip_url,
    serviceStatus?.ota_url,
    otaIpUrlsKey,
  ]);

  // 跳转触发 lazy 展开时，若首次聚焦被“扫描中禁用”的串口选框，待扫描完成解锁后把焦点交回串口选框
  useEffect(() => {
    if (!busy && ports.length > 0 && cardRef.current && document.activeElement === cardRef.current) {
      portSelectRef.current?.focus({ preventScroll: true });
    }
  }, [busy, ports.length]);

  const resetBoardDraft = useCallback(() => {
    otaTouchedRef.current = false;
    setInspectData(null);
    setPendingWrite(null);
    setCheckin(null);
    setNoticeText("");
    setDraft(createDraftFromInspect(null, serviceStatusRef.current));
  }, []);

  const loadPorts = useCallback(
    async ({ silent = false } = {}) => {
      if (silent) {
        if (inFlightRef.current || silentPortsInFlightRef.current) return;
        silentPortsInFlightRef.current = true;
      } else {
        if (inFlightRef.current) return;
        inFlightRef.current = true;
        setBusy("ports");
        setErrText("");
        setPendingWrite(null);
      }
      const seq = silent ? requestSeqRef.current : ++requestSeqRef.current;
      try {
        const res = await voiceApi.usbProvision({ action: "ports" });
        if (!mountedRef.current || requestSeqRef.current !== seq) return;
        if (silent && inFlightRef.current) return;
        const list = Array.isArray(res?.ports) ? res.ports : [];
        const prevPort = selectedPortRef.current;
        const prevPortNames = new Set(portsRef.current.map((p) => p.port_name));
        const prevPortItem = list.find((p) => p.port_name === prevPort && !p.is_laser_port);
        // 后台发现新插上的乐鑫原生 USB 口且用户未手动指定别的串口时，自动切过去
        const newlyPluggedEsp =
          silent &&
          !userPickedPortRef.current &&
          (!prevPortItem || prevPortItem.vid !== ESPRESSIF_USB_VID) &&
          list.some(
            (p) =>
              p.vid === ESPRESSIF_USB_VID &&
              !p.is_laser_port &&
              !prevPortNames.has(p.port_name),
          );
        const nextPort =
          prevPortItem && !newlyPluggedEsp ? prevPort : pickDefaultUsbPort(list);
        setPorts(list);
        setLaserBusy(Boolean(res?.laser_busy));
        setSelectedPort(nextPort);
        if (nextPort !== prevPort) {
          if (inspectDataRef.current) {
            resetBoardDraft();
          } else {
            // 尚未读取板子时切口或后台新插上串口，保留用户已先填好的 Wi-Fi / 密码 / OTA 草稿
            setPendingWrite(null);
            setCheckin(null);
          }
        }
      } catch (error) {
        if (silent || !mountedRef.current || requestSeqRef.current !== seq) return;
        setNoticeText("");
        setErrText(voiceErrorText(error));
      } finally {
        if (silent) {
          silentPortsInFlightRef.current = false;
        } else if (requestSeqRef.current === seq) {
          inFlightRef.current = false;
          if (mountedRef.current) setBusy("");
        }
      }
    },
    [resetBoardDraft],
  );

  useEffect(() => {
    void loadPorts();
  }, [loadPorts]);

  // 还没检测到任何串口且尚未读取板子时，每 4 秒后台扫一次，插上数据线自动出现，不用手点刷新
  useEffect(() => {
    if (ports.length > 0 || inspectData || busy) return;
    const timer = window.setInterval(() => {
      if (document.visibilityState === "hidden") return;
      void loadPorts({ silent: true });
    }, PORT_AUTOSCAN_MS);
    return () => window.clearInterval(timer);
  }, [ports.length, inspectData, busy, loadPorts]);

  const loadPortsRef = useRef(loadPorts);
  loadPortsRef.current = loadPorts;

  const updateDraft = useCallback((updater) => {
    setPendingWrite(null);
    setDraft(updater);
  }, []);

  const inspectBoard = async () => {
    if (!selectedPort || busy || inFlightRef.current) return;
    inFlightRef.current = true;
    const seq = ++requestSeqRef.current;
    setBusy("inspect");
    setErrText("");
    setNoticeText("");
    setPendingWrite(null);
    setCheckin(null);
    try {
      const res = await voiceApi.usbProvision({
        action: "inspect",
        port: selectedPort,
      });
      if (!mountedRef.current || requestSeqRef.current !== seq) return;
      otaTouchedRef.current = true;
      setInspectData(res);
      setDraft(createDraftFromInspect(res, serviceStatusRef.current));
      setNoticeText("已读取小智板子 NVS 配置，按原生配网结构展开（原 Wi-Fi 密码不回显）。");
    } catch (error) {
      if (!mountedRef.current || requestSeqRef.current !== seq) return;
      setNoticeText("");
      setErrText(voiceErrorText(error));
    } finally {
      if (requestSeqRef.current === seq) {
        inFlightRef.current = false;
        if (mountedRef.current) setBusy("");
      }
    }
  };

  const recognizedXiaozhi = isRecognizedXiaozhiInspect(inspectData);

  const requestWrite = () => {
    if (!inspectData || busy || laserBusy) return;
    if (!recognizedXiaozhi) {
      setPendingWrite(null);
      setNoticeText("");
      setErrText("该设备分区表与 NVS 中未检出小智固件特征，已阻止写入以防误刷其他开发板。");
      return;
    }
    const validationError = validateUsbProvisionDraft(draft);
    if (validationError) {
      setPendingWrite(null);
      setNoticeText("");
      setErrText(validationError);
      return;
    }
    const wsUrl = deriveWsUrlForMoss(draft.otaUrl, serviceStatus);
    const payload = buildUsbProvisionWritePayload({
      port: selectedPort,
      inspectData,
      draft,
      wsUrl,
    });
    const confirmLines = formatUsbWriteConfirmText({ port: selectedPort, inspectData, payload })
      .split("\n")
      .map((line) => line.trim())
      .filter((line) => line && !line.startsWith("写入后小智将自动重启生效"));
    setErrText("");
    setNoticeText("");
    setPendingWrite({ payload, confirmLines });
  };

  const confirmWrite = async () => {
    if (!pendingWrite || !inspectData || busy || laserBusy || inFlightRef.current) return;
    inFlightRef.current = true;
    const seq = ++requestSeqRef.current;
    const { payload } = pendingWrite;
    const preSessionIds = Array.isArray(serviceStatusRef.current?.sessions)
      ? serviceStatusRef.current.sessions.map((s) => s?.session_id).filter(Boolean)
      : [];
    setPendingWrite(null);
    setCheckin(null);
    setBusy("write");
    setErrText("");
    setNoticeText("");
    try {
      const res = await voiceApi.usbProvision(payload);
      if (!mountedRef.current || requestSeqRef.current !== seq) return;
      setInspectData(res);
      setDraft(createDraftFromInspect(res, serviceStatusRef.current));
      if (payload.patch?.new_wifi?.ssid) {
        lastWrittenWifiSession = {
          ssid: payload.patch.new_wifi.ssid,
          password: payload.patch.new_wifi.password || "",
        };
        setLastWifi(lastWrittenWifiSession);
      }
      const notes = ["已写入 NVS 并回读校验无误，小智板子正在重启，几秒后会连上 Wi-Fi 并向 MOSS 报到。"];
      if (res?.auto_allow_skipped) notes.push(res.auto_allow_skipped);
      if (res?.reset_warning) {
        notes.push(`配置已写好，但板子没能自动重启（${res.reset_warning}），请手动按一下 RST 键。`);
      }
      setNoticeText(notes.join(" "));
      let latestStatus = serviceStatusRef.current;
      let statusFetched = false;
      try {
        const nextStatus = await voiceApi.builtinStatus();
        if (mountedRef.current && requestSeqRef.current === seq && nextStatus) {
          latestStatus = nextStatus;
          statusFetched = true;
          if (typeof onSetStatus === "function") onSetStatus(nextStatus);
        }
      } catch {
        // 忽略后台状态刷新失败
      }
      if (!mountedRef.current || requestSeqRef.current !== seq) return;
      const mac = String(res?.device?.mac_address || "").toLowerCase();
      const pointsToMoss = Boolean(deriveWsUrlForMoss(payload.patch?.ota_url, latestStatus));
      if (mac && pointsToMoss) {
        // 合并写前与写后瞬间的在线会话：写入刚复位时服务端可能还留着旧 TCP 半开会话，不能误判为新上线
        const postSessionIds = Array.isArray(latestStatus?.sessions)
          ? latestStatus.sessions.map((s) => s?.session_id).filter(Boolean)
          : [];
        const mergedPreSessions = Array.from(new Set([...preSessionIds, ...postSessionIds]));
        const matched = Array.isArray(latestStatus?.devices)
          ? latestStatus.devices.find((d) => String(d?.device_id || "").toLowerCase() === mac)
          : null;
        // 写后状态拉取失败时用当前秒数抬高基线，避免老设备 bind_usb_voice_device 刷新的 last_seen 被误判为新 OTA 报到
        const baselineLastSeen = statusFetched
          ? Number(matched?.last_seen || 0)
          : Math.max(Number(matched?.last_seen || 0), Date.now() / 1000);
        setCheckin({
          mac,
          chip: res?.device?.chip || "",
          baselineLastSeen,
          preSessionIds: mergedPreSessions,
          retrySeq: 0,
          timedOut: false,
        });
      }
    } catch (error) {
      if (!mountedRef.current || requestSeqRef.current !== seq) return;
      setNoticeText("");
      setErrText(voiceErrorText(error));
    } finally {
      if (requestSeqRef.current === seq) {
        inFlightRef.current = false;
        if (mountedRef.current) setBusy("");
      }
    }
  };

  const startBuiltinService = async () => {
    if (startingService) return;
    setStartingService(true);
    setErrText("");
    try {
      const next = await voiceApi.builtinStart();
      if (!mountedRef.current) return;
      if (next && typeof onSetStatus === "function") onSetStatus(next);
      // 启动服务后如果刚才写过板子，清零超时并重新开启 20 秒报到跟踪
      setCheckin((prev) =>
        prev ? { ...prev, retrySeq: (prev.retrySeq || 0) + 1, timedOut: false } : prev,
      );
    } catch (error) {
      if (!mountedRef.current) return;
      setErrText(`启动内置语音服务失败：${voiceErrorText(error)}`);
    } finally {
      if (mountedRef.current) setStartingService(false);
    }
  };

  const focusSsidInput = () => {
    window.setTimeout(() => ssidInputRef.current?.focus(), 0);
  };

  // 权限 / 开关提示后面带“去系统设置打开”“重新检查”：用户先选了手动输入，之后想开随时能点
  const problemNotice = (problem, text) => ({
    tone: "warn",
    text,
    settingsTarget: canOpenWifiSettings(detectClientOs()) ? problem.target : "",
    recheck: true,
  });

  const openWifiSettings = async (target) => {
    try {
      await voiceApi.usbProvision({ action: "open_wifi_settings", target });
      if (!mountedRef.current) return;
      pendingRecheckRef.current = lastWifiActionRef.current;
      setWifiNotice({
        tone: "ok",
        text: "已打开系统设置。打开权限或 Wi-Fi 后切回 MOSS，会自动重新检查。",
        recheck: true,
      });
    } catch (error) {
      if (!mountedRef.current) return;
      setWifiNotice({ tone: "bad", text: voiceErrorText(error), recheck: true });
    }
  };

  /** 弹窗：去系统设置打开 / 手动输入。没有可跳转设置页的系统只给提示并聚焦输入框。 */
  const offerSystemSettings = async (problem) => {
    const manualText = `${problem.message} 现在可以先手动输入 Wi-Fi 名称。`;
    if (!canOpenWifiSettings(detectClientOs())) {
      setWifiNotice({ tone: "warn", text: manualText, recheck: true });
      focusSsidInput();
      return;
    }
    const choice = await showChoice(problem.message, {
      title: problem.title,
      tone: "warning",
      confirmText: "去系统设置打开",
      cancelText: "手动输入",
    });
    if (!mountedRef.current) return;
    if (choice === "confirm") {
      await openWifiSettings(problem.target);
      return;
    }
    setWifiNotice(
      problemNotice(problem, "已改为手动输入。之后想让 MOSS 自动读取，可以随时去系统设置打开。"),
    );
    if (choice === "cancel") focusSsidInput();
  };

  const queryHostWifi = async (action, { auto = false } = {}) => {
    if (wifiBusy) return;
    lastWifiActionRef.current = action;
    const seq = ++wifiSeqRef.current;
    setWifiBusy(action);
    setWifiNotice(null);
    let problem = null;
    try {
      const res = await voiceApi.usbProvision({ action });
      if (!mountedRef.current || wifiSeqRef.current !== seq) return;
      problem = hostWifiProblem(res, null);
      if (problem?.permission === "not_determined" && !systemPromptSeenRef.current) {
        // 系统刚弹出授权框：等用户点完切回 MOSS 自动重查
        systemPromptSeenRef.current = true;
        pendingRecheckRef.current = action;
        setWifiNotice(
          problemNotice(problem, "请在系统弹出的授权框里点“允许”，MOSS 会自动重新读取。"),
        );
        return;
      }
      if (!problem) {
        const chip = inspectData?.device?.chip;
        if (action === "wifi_current") {
          if (res?.current?.ssid) {
            updateDraft((prev) => applyPickedWifi(prev, res.current));
            setWifiNotice({ tone: "ok", text: currentWifiFillNotice(res.current, chip) });
          } else {
            setWifiNotice({
              tone: "warn",
              text: res?.hint || "没读到电脑当前连接的 Wi-Fi，请手动输入。",
            });
          }
        } else {
          const list = Array.isArray(res?.networks) ? res.networks : [];
          setWifiScan(list);
          if (res?.hint) setWifiNotice({ tone: list.length ? "ok" : "warn", text: res.hint });
        }
      }
    } catch (error) {
      if (!mountedRef.current || wifiSeqRef.current !== seq) return;
      problem = hostWifiProblem(null, error);
      if (!problem) setWifiNotice({ tone: "bad", text: voiceErrorText(error) });
    } finally {
      if (wifiSeqRef.current === seq && mountedRef.current) setWifiBusy("");
    }
    if (!problem || !mountedRef.current) return;
    if (auto) {
      // 自动重查仍不行：只留提示和入口，不反复弹窗打扰
      setWifiNotice(problemNotice(problem, `还没读到 Wi-Fi 名称。${problem.message}`));
    } else {
      await offerSystemSettings(problem);
    }
  };
  queryHostWifiRef.current = queryHostWifi;

  // 切回 MOSS（窗口重新获得焦点 / 页面重新可见）时：
  // 1) 若刚去系统设置开定位或 Wi-Fi，自动重查一次 Wi-Fi；
  // 2) 若尚未读取板子且空闲，静默刷新一次串口列表（插拔数据线不用手点刷新）。
  useEffect(() => {
    const onBack = () => {
      if (document.visibilityState === "hidden") return;
      const action = pendingRecheckRef.current;
      if (action) {
        // 正在读时先不消费“待重查”，留到下次切回再查
        if (wifiBusyRef.current) return;
        pendingRecheckRef.current = "";
        void queryHostWifiRef.current?.(action, { auto: true });
        return;
      }
      if (!busyRef.current && !inFlightRef.current && !inspectDataRef.current && !wifiBusyRef.current) {
        void loadPortsRef.current?.({ silent: true });
      }
    };
    window.addEventListener("focus", onBack);
    document.addEventListener("visibilitychange", onBack);
    return () => {
      window.removeEventListener("focus", onBack);
      document.removeEventListener("visibilitychange", onBack);
    };
  }, []);

  const checkinView = evaluateBoardCheckin(serviceStatus, checkin);
  const checkinPolling = Boolean(
    checkin && checkinView && (checkinView.phase === "waiting" || checkinView.phase === "ota_seen"),
  );
  const checkinRetrySeq = checkin?.retrySeq || 0;

  // 写入后跟踪小智重启连网并向 MOSS 报到（仅在服务运行后才起算 20 秒超时，每 2 秒轻查一次）
  useEffect(() => {
    if (!checkinPolling) return;
    let cancelled = false;
    let pollBusy = false;
    const pollOnce = async () => {
      if (cancelled || pollBusy || document.visibilityState === "hidden") return;
      pollBusy = true;
      try {
        const next = await voiceApi.builtinStatus({ lite: true });
        if (!cancelled && mountedRef.current && next && typeof onSetStatus === "function") {
          onSetStatus(next);
        }
      } catch {
        // 忽略偶发轮询失败，等下个节拍再查
      } finally {
        pollBusy = false;
      }
    };
    const timeoutId = window.setTimeout(() => {
      if (!cancelled && mountedRef.current) {
        setCheckin((prev) => (prev ? { ...prev, timedOut: true } : null));
      }
    }, USB_CHECKIN_TIMEOUT_MS);
    const intervalId = window.setInterval(() => {
      void pollOnce();
    }, USB_CHECKIN_POLL_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timeoutId);
      window.clearInterval(intervalId);
    };
  }, [checkinPolling, checkinRetrySeq, onSetStatus]);

  const retryCheckin = async () => {
    if (!checkin) return;
    setCheckin((prev) =>
      prev ? { ...prev, retrySeq: (prev.retrySeq || 0) + 1, timedOut: false } : null,
    );
    try {
      const next = await voiceApi.builtinStatus({ lite: true });
      if (mountedRef.current && next && typeof onSetStatus === "function") {
        onSetStatus(next);
      }
    } catch {
      // 轮询会继续重试
    }
  };

  const pickScannedWifi = (network) => {
    updateDraft((prev) => applyPickedWifi(prev, network));
    setWifiNotice(pickedWifiNotice(network));
  };

  const useLastWrittenWifi = () => {
    if (!lastWifi?.ssid) return;
    updateDraft((prev) => ({
      ...prev,
      newSsid: lastWifi.ssid,
      newPassword: lastWifi.password || "",
    }));
    setWifiNotice({
      tone: "ok",
      text: `已填入本次刚配过的“${lastWifi.ssid}”及其密码（仅保存在本次内存中，关闭 MOSS 即清除）。`,
    });
  };

  const clearLastWifi = () => {
    clearLastWrittenWifiSession();
    setLastWifi(null);
  };

  const resetSavedSlots = () => {
    if (!inspectData) return;
    const fresh = createDraftFromInspect(inspectData, serviceStatus);
    updateDraft((prev) => ({ ...prev, savedSlots: fresh.savedSlots }));
  };

  const renderWriteArea = (buttonId, buttonLabel) => {
    const writeStage = busy === "write" ? usbOperationStageHint("write", busyElapsedSec) : "";
    if (pendingWrite) {
      return (
        <div
          className="xz-confirm-box"
          id="xiaozhi-usb-confirm-box"
          role="region"
          aria-label="写入板子确认"
        >
          <div className="xz-confirm-title">
            <AlertTriangle size={15} aria-hidden="true" />
            <span>写入后小智将自动重启生效。确认写入板子 Flash 吗？</span>
          </div>
          <ul className="xz-confirm-lines">
            {pendingWrite.confirmLines.map((line, idx) => (
              <li key={idx}>{line}</li>
            ))}
          </ul>
          <div className="xz-confirm-actions">
            <button
              type="button"
              className="btn btn-hud-primary"
              disabled={Boolean(busy) || laserBusy || !inspectData || !recognizedXiaozhi}
              onClick={confirmWrite}
              id="xiaozhi-usb-confirm-write"
            >
              确认写入并重启
            </button>
            <button
              type="button"
              className="btn btn-ghost"
              disabled={Boolean(busy)}
              onClick={() => setPendingWrite(null)}
              id="xiaozhi-usb-cancel-write"
            >
              取消
            </button>
          </div>
        </div>
      );
    }
    return (
      <>
        <div className="wakeword-actions">
          <button
            type="button"
            className="btn btn-hud-primary"
            disabled={Boolean(busy) || !inspectData || !recognizedXiaozhi || laserBusy}
            onClick={requestWrite}
            id={buttonId}
          >
            {busy === "write" ? "正在写入板子 Flash…" : buttonLabel}
          </button>
          {!inspectData ? (
            <span className="voice-muted">请先在上方读取板子配置</span>
          ) : (
            !recognizedXiaozhi && (
              <span className="voice-muted">未检出小智固件特征，已禁止写入</span>
            )
          )}
        </div>
        {writeStage && (
          <div className="voice-hint xz-banner" id="xiaozhi-usb-progress" role="status">
            <RefreshCw size={14} className="voice-spin" aria-hidden="true" />{" "}
            <span>{writeStage}</span>
          </div>
        )}
      </>
    );
  };

  const slotsModified =
    draft.savedSlots.length !== draft.originalSlotIds.length ||
    draft.savedSlots.some((item, idx) => item.slot !== draft.originalSlotIds[idx]);
  const ssidBytes = utf8ByteLength(draft.newSsid.trim());
  const otaQuickOptions = mossOtaQuickFillOptions(serviceStatus);
  const serviceStopped = serviceStatus?.running === false;
  const inspectStage = busy === "inspect" ? usbOperationStageHint("inspect", busyElapsedSec) : "";
  const dimVal = String(draft.sleepDimMin ?? "").trim() === "" ? -1 : Number(draft.sleepDimMin);
  const offVal = String(draft.sleepOffMin ?? "").trim() === "" ? -1 : Number(draft.sleepOffMin);
  const revoked = isRevokedInMoss(inspectData);
  const staleOta = staleBoardOta(inspectData, draft.otaUrl, serviceStatus);
  const renderStaleOta = (id) =>
    staleOta ? (
      <div className="voice-hint voice-hint--warn xz-banner" id={id} role="status">
        <AlertTriangle size={14} />{" "}
        <span>
          板子里存的是 <code>{staleOta.boardUrl}</code>，MOSS 现在的地址是{" "}
          <code>{staleOta.mossUrl}</code>。照原样写入，板子可能连不上 MOSS（除非你是故意指向别的服务器）。
        </span>
        <button
          type="button"
          className="xz-chip-btn"
          disabled={Boolean(busy)}
          onClick={() => {
            otaTouchedRef.current = true;
            updateDraft((prev) => ({ ...prev, otaUrl: staleOta.mossUrl }));
          }}
        >
          换成 MOSS 当前地址
        </button>
      </div>
    ) : null;
  const boardChip = inspectData?.device?.chip || "";
  // 扫描结果按读到的芯片筛：非 C5 只显示 2.4 GHz，C5 显示 2.4 / 5 GHz
  const scanView =
    Array.isArray(wifiScan) && wifiScan.length > 0
      ? filterScannedWifiForBoard(wifiScan, boardChip)
      : null;
  const ssidEdgeSpaces = draft.newSsid.trim() !== "" && draft.newSsid !== draft.newSsid.trim();

  return (
    <div className="xiaozhi-usb-card" id="xiaozhi-usb-card" ref={cardRef} tabIndex={-1}>
      <p className="xiaozhi-usb-intro">
        用 USB 数据线连接小智（若无法自动进下载模式，先按住板子 BOOT 键再点“读取板子配置”），免开手机热点直写闪存 NVS 分区的 Wi-Fi、本机语音地址与设备密钥；原已保存的 Wi-Fi 密码不回显。
      </p>

      {serviceStopped && (
        <div
          className="voice-hint voice-hint--warn xz-banner"
          id="xiaozhi-usb-service-stopped"
          role="status"
        >
          <AlertTriangle size={14} />{" "}
          <span>MOSS 内置语音服务还没启动，现在写入后小智开机连不上 MOSS。</span>
          <button
            type="button"
            className="xz-chip-btn"
            id="xiaozhi-usb-start-service"
            disabled={startingService}
            onClick={startBuiltinService}
          >
            <Play size={12} aria-hidden="true" />{" "}
            {startingService ? "启动中…" : "启动内置服务"}
          </button>
        </div>
      )}
      {errText && (
        <div className="voice-hint voice-hint--bad xz-banner" role="alert">
          <AlertTriangle size={14} /> <span>{errText}</span>
        </div>
      )}
      {noticeText && (
        <div className="voice-hint voice-hint--ok xz-banner" role="status">
          <CheckCircle2 size={14} /> <span>{noticeText}</span>
        </div>
      )}
      {checkinView && (
        <div
          className={`voice-hint ${
            checkinView.tone === "ok"
              ? "voice-hint--ok"
              : checkinView.tone === "warn"
                ? "voice-hint--warn"
                : ""
          } xz-banner`}
          id="xiaozhi-usb-checkin"
          data-phase={checkinView.phase}
          role="status"
        >
          {checkinView.tone === "ok" ? (
            <CheckCircle2 size={14} aria-hidden="true" />
          ) : checkinView.tone === "warn" ? (
            <AlertTriangle size={14} aria-hidden="true" />
          ) : checkinView.phase === "waiting" || checkinView.phase === "ota_seen" ? (
            <RefreshCw size={14} className="voice-spin" aria-hidden="true" />
          ) : (
            <Usb size={14} aria-hidden="true" />
          )}{" "}
          <span>{checkinView.text}</span>
          {checkinView.phase === "service_stopped" && (
            <button
              type="button"
              className="xz-chip-btn"
              disabled={startingService}
              onClick={startBuiltinService}
            >
              <Play size={12} aria-hidden="true" />{" "}
              {startingService ? "启动中…" : "启动内置服务"}
            </button>
          )}
          {checkinView.phase === "timeout" && (
            <button
              type="button"
              className="xz-chip-btn"
              id="xiaozhi-usb-checkin-retry"
              onClick={retryCheckin}
            >
              <RefreshCw size={12} aria-hidden="true" /> 重新检查
            </button>
          )}
        </div>
      )}

      <div className="xiaozhi-usb-serial-bar">
        <div className="wakeword-step-head">
          <span className="wakeword-step-no" aria-hidden="true">1</span>
          <div>
            <h3>选择串口并读取板子</h3>
            <p>
              自动跳过当前激光端口；写入前先读取原分区，完整保留板子校准参数与已有配置，写后回读校验、失败自动写回原配置。读取和写入都会让板子重启一下。
            </p>
          </div>
        </div>

        <div className="xiaozhi-usb-serial-row">
          <select
            ref={portSelectRef}
            id="xiaozhi-usb-port-select"
            className="text-input"
            aria-label="USB 串口"
            value={selectedPort}
            disabled={Boolean(busy) || ports.length === 0}
            onChange={(e) => {
              userPickedPortRef.current = true;
              setSelectedPort(e.target.value);
              if (inspectDataRef.current) {
                resetBoardDraft();
              } else {
                setPendingWrite(null);
                setCheckin(null);
              }
              setErrText("");
            }}
          >
            {ports.length === 0 ? (
              <option value="">
                {busy === "ports" ? "正在扫描 USB 串口…" : "未检测到可用 USB 串口"}
              </option>
            ) : (
              ports.map((item) => (
                <option
                  key={item.port_name}
                  value={item.port_name}
                  disabled={item.is_laser_port}
                >
                  {item.port_name}
                  {item.description ? ` — ${item.description}` : ""}
                  {item.is_laser_port ? " [激光端口·已保护]" : ""}
                </option>
              ))
            )}
          </select>
          <button
            type="button"
            className="btn btn-ghost"
            disabled={Boolean(busy)}
            onClick={() => void loadPorts()}
            title="重新扫描串口"
            id="xiaozhi-usb-refresh"
          >
            <RefreshCw size={14} className={busy === "ports" ? "voice-spin" : ""} /> 刷新
          </button>
          <button
            type="button"
            className="btn btn-hud-primary"
            disabled={Boolean(busy) || !selectedPort || laserBusy}
            onClick={inspectBoard}
            id="xiaozhi-usb-inspect"
          >
            <Usb size={14} /> {busy === "inspect" ? "读取中…" : "读取板子配置"}
          </button>
        </div>

        {inspectStage && (
          <div className="voice-hint xz-banner" id="xiaozhi-usb-progress" role="status">
            <RefreshCw size={14} className="voice-spin" aria-hidden="true" />{" "}
            <span>{inspectStage}</span>
          </div>
        )}

        {needsBoardIdentityWarning(ports.find((p) => p.port_name === selectedPort)) && (
          <div className="voice-hint voice-hint--warn xz-banner" id="xiaozhi-usb-port-identity-warn">
            <AlertTriangle size={14} />{" "}
            <span>
              这个串口用的是 CH340 / CP210x 一类转换芯片，激光控制器也常用：点“读取”会让这个口上的板子重启一下，请确认插着的是小智而不是激光机。
            </span>
          </div>
        )}

        {laserBusy && (
          <div className="voice-hint voice-hint--warn xz-banner">
            <AlertTriangle size={14} /> <span>激光雕刻机正在执行任务，已暂停 USB 串口配网。</span>
          </div>
        )}

        {inspectData ? (
          <>
            <div className="xiaozhi-usb-board-chip">
              <span>
                <b>{inspectData.device?.chip}</b> · {inspectData.device?.flash_size} · MAC{" "}
                <code>{inspectData.device?.mac_address}</code>
              </span>
              <span>
                分区 <code>{inspectData.partition?.label}</code> ({inspectData.partition?.offset})
              </span>
              {inspectData.known_device && (
                <span className="ob-pill ob-pill--info xz-known-pill">
                  MOSS：{inspectData.known_device.name || inspectData.known_device.device_id}（
                  {DEVICE_STATUS_LABELS[inspectData.known_device.status] ||
                    inspectData.known_device.status}
                  ）
                </span>
              )}
            </div>
            {!recognizedXiaozhi && (
              <div
                className="voice-hint voice-hint--warn xz-banner"
                id="xiaozhi-usb-unrecognized-warn"
              >
                <AlertTriangle size={14} />{" "}
                <span>
                  该板卡分区表与 NVS 中未检出小智固件特征（无小智资源分区或 board / websocket
                  命名空间），已禁止写入以防误刷非小智开发板。
                </span>
              </div>
            )}
          </>
        ) : (
          <p className="voice-muted" style={{ margin: 0 }}>
            尚未读取板子配置；选好串口后点“读取板子配置”。
          </p>
        )}
      </div>

      <div className="xiaozhi-usb-config-step">
        <div className="xiaozhi-usb-step2-head">
          <div className="wakeword-step-head">
            <span className="wakeword-step-no" aria-hidden="true">2</span>
            <div>
              <h3>填写 Wi-Fi 与连接地址</h3>
              <p>支持调整已存 Wi-Fi 优先级或添加新网络，底部二次确认后写入并重启小智。</p>
            </div>
          </div>

          <div className="xz-tabs" role="tablist" aria-label="配网标签页">
            <button
              type="button"
              role="tab"
              id="xiaozhi-tab-wifi"
              aria-selected={tab === "wifi"}
              className={`xz-tab ${tab === "wifi" ? "is-active" : ""}`}
              onClick={() => setTab("wifi")}
            >
              <Wifi size={14} /> Wi-Fi 配置
            </button>
            <button
              type="button"
              role="tab"
              id="xiaozhi-tab-advanced"
              aria-selected={tab === "advanced"}
              className={`xz-tab ${tab === "advanced" ? "is-active" : ""}`}
              onClick={() => setTab("advanced")}
            >
              <Sliders size={14} /> 高级选项
            </button>
          </div>
        </div>

        {tab === "wifi" ? (
          <div className="xz-tab-body" role="tabpanel" aria-labelledby="xiaozhi-tab-wifi">
            <div className="xz-card">
              <div className="xz-card-head">
                <h4>已保存的 Wi-Fi</h4>
                {slotsModified && (
                  <button
                    type="button"
                    className="voice-link-btn"
                    disabled={Boolean(busy)}
                    onClick={resetSavedSlots}
                  >
                    恢复读取前顺序
                  </button>
                )}
              </div>
              {draft.savedSlots.length === 0 ? (
                <p className="voice-muted" style={{ margin: 0 }}>
                  {inspectData
                    ? "板子 NVS 里还没有保存过 Wi-Fi。"
                    : "点击上方“读取板子配置”后，这里会列出板子已保存的 Wi-Fi 列表。"}
                </p>
              ) : (
                <ul className="xz-saved-list">
                  {draft.savedSlots.map((slot, index) => (
                    <li key={`${slot.slot}-${slot.ssid}`} className="xz-saved-item">
                      <div className="xz-saved-info">
                        <span className="xz-saved-ssid">{slot.ssid}</span>
                        <div className="xz-saved-tags">
                          {index === 0 && <span className="ob-pill ob-pill--info">首选</span>}
                          <span className="ob-pill ob-pill--off">
                            {slot.has_password ? "已存密码" : "无密码"}
                          </span>
                        </div>
                      </div>
                      <div className="xz-saved-actions">
                        <button
                          type="button"
                          className="xz-icon-btn"
                          disabled={Boolean(busy) || index === 0}
                          onClick={() =>
                            updateDraft((prev) => ({
                              ...prev,
                              savedSlots: pinSavedWifiSlot(prev.savedSlots, index),
                            }))
                          }
                          title="设为首选（置顶）"
                          aria-label={`置顶 ${slot.ssid}`}
                        >
                          <ArrowUp size={14} />
                        </button>
                        <button
                          type="button"
                          className="xz-icon-btn xz-icon-btn--danger"
                          disabled={Boolean(busy)}
                          onClick={() =>
                            updateDraft((prev) => ({
                              ...prev,
                              savedSlots: removeSavedWifiSlot(prev.savedSlots, index),
                            }))
                          }
                          title="从列表移除"
                          aria-label={`删除 ${slot.ssid}`}
                        >
                          <Trash2 size={14} />
                        </button>
                      </div>
                    </li>
                  ))}
                </ul>
              )}
            </div>

            <div className="xz-card">
              <h4>新的 Wi-Fi</h4>
              <div className="xz-grid-2">
                <div className="xz-form-group">
                  <label htmlFor="xiaozhi-wifi-ssid">
                    Wi-Fi 名称 (SSID)
                    {draft.newSsid.trim() && (
                      <span className={`xz-byte-count ${ssidBytes > 32 ? "is-over" : ""}`}>
                        {ssidBytes}/32 B
                      </span>
                    )}
                  </label>
                  <input
                    id="xiaozhi-wifi-ssid"
                    ref={ssidInputRef}
                    className="text-input"
                    type="text"
                    value={draft.newSsid}
                    disabled={Boolean(busy)}
                    placeholder="不添新 Wi-Fi 可留空（仅改已有顺序或地址）"
                    onChange={(e) => updateDraft((prev) => ({ ...prev, newSsid: e.target.value }))}
                  />
                  {ssidEdgeSpaces && (
                    <p className="xz-field-err" id="xiaozhi-wifi-ssid-spaces">
                      名称首尾有空格，会按原样写入；不是路由器名字的一部分请删掉。
                    </p>
                  )}
                  <div className="xz-quick-fills">
                    <button
                      type="button"
                      className="xz-chip-btn"
                      id="xiaozhi-wifi-use-current"
                      disabled={Boolean(busy) || Boolean(wifiBusy)}
                      onClick={() => queryHostWifi("wifi_current")}
                      title="读取这台电脑当前连接的 Wi-Fi 名称（不读密码）"
                    >
                      <Laptop size={12} aria-hidden="true" />{" "}
                      {wifiBusy === "wifi_current" ? "读取中…" : "用本机当前 Wi-Fi"}
                    </button>
                    <button
                      type="button"
                      className="xz-chip-btn"
                      id="xiaozhi-wifi-scan"
                      disabled={Boolean(busy) || Boolean(wifiBusy)}
                      onClick={() => queryHostWifi("wifi_scan")}
                      title="用这台电脑扫描附近的 Wi-Fi"
                    >
                      <ScanSearch size={12} aria-hidden="true" />{" "}
                      {wifiBusy === "wifi_scan" ? "扫描中…" : "扫描附近"}
                    </button>
                    {lastWifi?.ssid && draft.newSsid !== lastWifi.ssid && (
                      <>
                        <button
                          type="button"
                          className="xz-chip-btn"
                          id="xiaozhi-wifi-use-last"
                          disabled={Boolean(busy)}
                          onClick={useLastWrittenWifi}
                          title="填入本次内存会话中刚成功写入的 Wi-Fi 名称与密码（不落盘）"
                        >
                          <History size={12} aria-hidden="true" /> 复用上次：{lastWifi.ssid}
                        </button>
                        <button
                          type="button"
                          className="xz-chip-btn xz-chip-btn--muted"
                          id="xiaozhi-wifi-clear-last"
                          disabled={Boolean(busy)}
                          onClick={clearLastWifi}
                          title="清除内存中暂存的上次 Wi-Fi 密码"
                        >
                          清除记忆
                        </button>
                      </>
                    )}
                  </div>
                </div>
                <div className="xz-form-group">
                  <label htmlFor="xiaozhi-wifi-pass">Wi-Fi 密码</label>
                  <div className="xz-input-with-btn">
                    <input
                      id="xiaozhi-wifi-pass"
                      className="text-input"
                      type={showPassword ? "text" : "password"}
                      value={draft.newPassword}
                      disabled={Boolean(busy)}
                      placeholder="开放网络可留空"
                      onChange={(e) =>
                        updateDraft((prev) => ({ ...prev, newPassword: e.target.value }))
                      }
                    />
                    <button
                      type="button"
                      className="xz-inline-eye"
                      onClick={() => setShowPassword((v) => !v)}
                      aria-label={showPassword ? "隐藏密码" : "显示密码"}
                    >
                      {showPassword ? <EyeOff size={15} /> : <Eye size={15} />}
                    </button>
                  </div>
                </div>
              </div>

              {wifiNotice && (
                <p
                  className={`xz-wifi-notice xz-wifi-notice--${wifiNotice.tone}`}
                  id="xiaozhi-wifi-notice"
                  role={wifiNotice.tone === "bad" ? "alert" : "status"}
                >
                  {wifiNotice.text}
                  {wifiNotice.settingsTarget && (
                    <button
                      type="button"
                      className="voice-link-btn xz-wifi-notice-action"
                      id="xiaozhi-wifi-open-settings"
                      onClick={() => openWifiSettings(wifiNotice.settingsTarget)}
                    >
                      去系统设置打开
                    </button>
                  )}
                  {wifiNotice.recheck && (
                    <button
                      type="button"
                      className="voice-link-btn xz-wifi-notice-action"
                      id="xiaozhi-wifi-recheck"
                      disabled={Boolean(busy) || Boolean(wifiBusy)}
                      onClick={() => queryHostWifi(lastWifiActionRef.current)}
                    >
                      重新检查
                    </button>
                  )}
                </p>
              )}

              {scanView && (
                <div className="xz-scan-box" id="xiaozhi-wifi-scan-list">
                  <div className="xz-card-head">
                    <span className="voice-muted" id="xiaozhi-wifi-scan-filter">
                      附近的 Wi-Fi（{scanView.shown.length}）·{" "}
                      {scanFilterLabel(boardChip, scanView.hiddenCount)}
                    </span>
                    <span className="xz-saved-actions">
                      <button
                        type="button"
                        className="voice-link-btn"
                        id="xiaozhi-wifi-rescan"
                        disabled={Boolean(busy) || Boolean(wifiBusy)}
                        onClick={() => queryHostWifi("wifi_scan")}
                      >
                        <RefreshCw
                          size={12}
                          aria-hidden="true"
                          className={wifiBusy === "wifi_scan" ? "voice-spin" : ""}
                        />{" "}
                        {wifiBusy === "wifi_scan" ? "扫描中…" : "重新扫描"}
                      </button>
                      <button
                        type="button"
                        className="voice-link-btn"
                        onClick={() => setWifiScan(null)}
                      >
                        收起
                      </button>
                    </span>
                  </div>
                  {scanView.shown.length === 0 ? (
                    <p className="voice-muted xz-scan-empty">
                      附近没有这块板子能连的网络，可以点“重新扫描”或直接手动输入。
                    </p>
                  ) : (
                    <ul className="xz-scan-list">
                      {scanView.shown.map((network) => {
                        const picked = network.ssid === draft.newSsid;
                        return (
                          <li key={network.ssid}>
                            <button
                              type="button"
                              className={`xz-scan-item ${picked ? "is-picked" : ""}`}
                              disabled={Boolean(busy)}
                              aria-pressed={picked}
                              onClick={() => pickScannedWifi(network)}
                            >
                              <span className="xz-scan-ssid">
                                {network.secure === false ? (
                                  <LockOpen size={12} aria-label="开放网络" />
                                ) : (
                                  <Lock size={12} aria-label="加密网络" />
                                )}
                                <span>{network.ssid}</span>
                              </span>
                              <span className="xz-saved-tags">
                                {network.current && (
                                  <span className="ob-pill ob-pill--info">电脑当前</span>
                                )}
                                {network.bands.length === 0 ? (
                                  <span className="ob-pill ob-pill--off">频段未知</span>
                                ) : (
                                  network.bands.map((band) => (
                                    <span key={band} className="ob-pill ob-pill--off">
                                      {wifiBandLabel(band)}
                                    </span>
                                  ))
                                )}
                                {network.wpa3_only === true && (
                                  <span
                                    className="ob-pill ob-pill--warn"
                                    title="部分小智 ESP32 固件只支持 WPA2，纯 WPA3 可能连不上"
                                  >
                                    仅 WPA3
                                  </span>
                                )}
                                {Number.isInteger(network.signal) && (
                                  <span className="xz-scan-signal">信号 {network.signal}%</span>
                                )}
                              </span>
                            </button>
                          </li>
                        );
                      })}
                    </ul>
                  )}
                </div>
              )}

              <div className="xz-ota-preview">
                <span>
                  本机语音地址：
                  <code>{draft.otaUrl.trim() || "（擦除自定义键，回退固件默认）"}</code>
                </span>
                <button
                  type="button"
                  className="voice-link-btn"
                  onClick={() => setTab("advanced")}
                >
                  去高级选项修改
                </button>
              </div>
              {renderStaleOta("xiaozhi-ota-stale")}

              <label className="xz-checkbox">
                <input
                  type="checkbox"
                  checked={draft.autoAllow}
                  disabled={Boolean(busy)}
                  onChange={(e) =>
                    updateDraft((prev) => ({ ...prev, autoAllow: e.target.checked }))
                  }
                />
                <span>同时在 MOSS 里把这台板子设为“已允许”（板子连上来就能用，不用再手动点允许）</span>
              </label>
              {draft.autoAllow && revoked && (
                <label className="xz-checkbox" id="xiaozhi-usb-unrevoke">
                  <input
                    type="checkbox"
                    checked={draft.unrevoke}
                    disabled={Boolean(busy)}
                    onChange={(e) =>
                      updateDraft((prev) => ({ ...prev, unrevoke: e.target.checked }))
                    }
                  />
                  <span>这台板子之前在 MOSS 里被撤销过，确认解除撤销</span>
                </label>
              )}
              {draft.autoAllow && inspectData?.device?.chip === "esp32p4" && (
                <p className="voice-muted">
                  ESP32-P4 板子上报的设备号与芯片 MAC 不同，不会自动放行：连上 MOSS 后请在设备列表里手动允许。
                </p>
              )}

              {renderWriteArea("xiaozhi-usb-write-wifi", "连接 / 写入板子")}
            </div>
          </div>
        ) : (
          <div className="xz-tab-body" role="tabpanel" aria-labelledby="xiaozhi-tab-advanced">
            <div className="xz-card">
              <div className="xz-grid-2">
                <div className="xz-form-group">
                  <label htmlFor="xiaozhi-ota-url">自定义 OTA 地址</label>
                  <div className="xz-input-with-btn">
                    <input
                      id="xiaozhi-ota-url"
                      className="text-input"
                      type="url"
                      value={draft.otaUrl}
                      disabled={Boolean(busy)}
                      placeholder="留空则擦除自定义 OTA 键（回退固件出厂默认）"
                      onChange={(e) => {
                        otaTouchedRef.current = true;
                        updateDraft((prev) => ({ ...prev, otaUrl: e.target.value }));
                      }}
                    />
                    {draft.otaUrl && (
                      <button
                        type="button"
                        className="xz-inline-clear"
                        disabled={Boolean(busy)}
                        onClick={() => {
                          otaTouchedRef.current = true;
                          updateDraft((prev) => ({ ...prev, otaUrl: "" }));
                        }}
                        aria-label="清空 OTA 地址"
                        title="清空自定义 OTA 地址（写入时恢复固件默认）"
                      >
                        ×
                      </button>
                    )}
                  </div>
                  <div className="xz-quick-fills" id="xiaozhi-ota-quick-fills">
                    {otaQuickOptions.map((opt) => (
                      <button
                        key={opt.url}
                        type="button"
                        className="xz-chip-btn"
                        disabled={Boolean(busy)}
                        title={opt.url}
                        onClick={() => {
                          otaTouchedRef.current = true;
                          updateDraft((prev) => ({ ...prev, otaUrl: opt.url }));
                        }}
                      >
                        {opt.label}
                      </button>
                    ))}
                  </div>
                  {renderStaleOta("xiaozhi-ota-stale-advanced")}
                </div>

                <div className="xz-form-group">
                  <label htmlFor="xiaozhi-max-tx-power">最大发射功率</label>
                  <select
                    id="xiaozhi-max-tx-power"
                    className="text-input"
                    value={draft.maxTxPower}
                    disabled={Boolean(busy)}
                    onChange={(e) =>
                      updateDraft((prev) => ({ ...prev, maxTxPower: Number(e.target.value) }))
                    }
                  >
                    {MAX_TX_POWER_OPTIONS.map((opt) => (
                      <option key={opt.value} value={opt.value}>
                        {opt.label}
                      </option>
                    ))}
                  </select>
                </div>
              </div>

              <label className="xz-checkbox">
                <input
                  type="checkbox"
                  checked={draft.rememberBssid}
                  disabled={Boolean(busy)}
                  onChange={(e) =>
                    updateDraft((prev) => ({ ...prev, rememberBssid: e.target.checked }))
                  }
                />
                <span>记住 BSSID（多台同名路由器下锁定同一 AP）</span>
              </label>

              <label className="xz-checkbox">
                <input
                  type="checkbox"
                  checked={draft.sleepMode}
                  disabled={Boolean(busy)}
                  onChange={(e) =>
                    updateDraft((prev) => ({ ...prev, sleepMode: e.target.checked }))
                  }
                />
                <span>睡眠模式（闲置自动调暗与息屏节能；出厂默认开启）</span>
              </label>
              <p className="voice-muted">等待时间留空 = 板型默认，填 0 = 关掉这一步；没改动的项写入时保持板子原样。</p>

              <div className="xz-sleep-grid">
                <div className="xz-form-group">
                  <label htmlFor="xiaozhi-sleep-dim">变暗等待（分钟）</label>
                  <input
                    id="xiaozhi-sleep-dim"
                    className="text-input"
                    type="number"
                    min={0}
                    max={1440}
                    step={1}
                    value={draft.sleepDimMin}
                    placeholder="板型默认"
                    disabled={Boolean(busy)}
                    onChange={(e) =>
                      updateDraft((prev) => ({ ...prev, sleepDimMin: e.target.value }))
                    }
                  />
                </div>
                <div className="xz-form-group">
                  <label htmlFor="xiaozhi-sleep-off">关屏等待（分钟）</label>
                  <input
                    id="xiaozhi-sleep-off"
                    className="text-input"
                    type="number"
                    min={0}
                    max={1440}
                    step={1}
                    value={draft.sleepOffMin}
                    placeholder="板型默认"
                    disabled={Boolean(busy)}
                    onChange={(e) =>
                      updateDraft((prev) => ({ ...prev, sleepOffMin: e.target.value }))
                    }
                  />
                </div>
              </div>
              {dimVal > 0 && offVal > 0 && offVal <= dimVal && (
                <p className="xz-field-err">关屏等待时间必须大于变暗等待时间。</p>
              )}

              {renderWriteArea("xiaozhi-usb-write-advanced", "保存设置并写入板子")}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
