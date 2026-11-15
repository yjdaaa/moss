import React, { useCallback, useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  ArrowUp,
  CheckCircle2,
  Eye,
  EyeOff,
  RefreshCw,
  Sliders,
  Trash2,
  Usb,
  Wifi,
} from "lucide-react";
import { voiceApi } from "../../voiceApi.js";
import { voiceErrorText } from "../../voiceState.js";
import {
  MAX_TX_POWER_OPTIONS,
  buildUsbProvisionWritePayload,
  createDraftFromInspect,
  deriveWsUrlForMoss,
  formatUsbWriteConfirmText,
  isRecognizedXiaozhiInspect,
  isRevokedInMoss,
  needsBoardIdentityWarning,
  pickDefaultUsbPort,
  pinSavedWifiSlot,
  removeSavedWifiSlot,
  utf8ByteLength,
  validateUsbProvisionDraft,
} from "./usbProvisionModel.js";

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
  const [errText, setErrText] = useState("");
  const [noticeText, setNoticeText] = useState("");
  const [inspectData, setInspectData] = useState(null);
  const [draft, setDraft] = useState(() => createDraftFromInspect(null, serviceStatus));
  const [showPassword, setShowPassword] = useState(false);
  const [pendingWrite, setPendingWrite] = useState(null);

  const mountedRef = useRef(true);
  const inFlightRef = useRef(false);
  const requestSeqRef = useRef(0);
  const cardRef = useRef(null);
  const portSelectRef = useRef(null);
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
      requestSeqRef.current += 1;
    };
  }, []);

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
    setNoticeText("");
    setDraft(createDraftFromInspect(null, serviceStatusRef.current));
  }, []);

  const loadPorts = useCallback(async () => {
    if (inFlightRef.current) return;
    inFlightRef.current = true;
    const seq = ++requestSeqRef.current;
    setBusy("ports");
    setErrText("");
    setPendingWrite(null);
    try {
      const res = await voiceApi.usbProvision({ action: "ports" });
      if (!mountedRef.current || requestSeqRef.current !== seq) return;
      const list = Array.isArray(res?.ports) ? res.ports : [];
      const prevPort = selectedPortRef.current;
      const nextPort =
        prevPort && list.some((p) => p.port_name === prevPort && !p.is_laser_port)
          ? prevPort
          : pickDefaultUsbPort(list);
      setPorts(list);
      setLaserBusy(Boolean(res?.laser_busy));
      setSelectedPort(nextPort);
      if (nextPort !== prevPort) {
        resetBoardDraft();
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
  }, [resetBoardDraft]);

  useEffect(() => {
    void loadPorts();
  }, [loadPorts]);

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
    setPendingWrite(null);
    setBusy("write");
    setErrText("");
    setNoticeText("");
    try {
      const res = await voiceApi.usbProvision(payload);
      if (!mountedRef.current || requestSeqRef.current !== seq) return;
      setInspectData(res);
      setDraft(createDraftFromInspect(res, serviceStatusRef.current));
      const notes = ["已写入 NVS 并回读校验无误，小智板子正在重启，几秒后会连上 Wi-Fi 并向 MOSS 报到。"];
      if (res?.auto_allow_skipped) notes.push(res.auto_allow_skipped);
      if (res?.reset_warning) {
        notes.push(`配置已写好，但板子没能自动重启（${res.reset_warning}），请手动按一下 RST 键。`);
      }
      setNoticeText(notes.join(" "));
      if (typeof onSetStatus === "function") {
        try {
          const nextStatus = await voiceApi.builtinStatus();
          if (mountedRef.current && requestSeqRef.current === seq && nextStatus) {
            onSetStatus(nextStatus);
          }
        } catch {
          // 忽略后台状态刷新失败
        }
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

  const resetSavedSlots = () => {
    if (!inspectData) return;
    const fresh = createDraftFromInspect(inspectData, serviceStatus);
    updateDraft((prev) => ({ ...prev, savedSlots: fresh.savedSlots }));
  };

  const renderWriteArea = (buttonId, buttonLabel) => {
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
    );
  };

  const slotsModified =
    draft.savedSlots.length !== draft.originalSlotIds.length ||
    draft.savedSlots.some((item, idx) => item.slot !== draft.originalSlotIds[idx]);
  const ssidBytes = utf8ByteLength(draft.newSsid.trim());
  const otaIp = serviceStatus?.ota_ip_url || "";
  const otaMdns = serviceStatus?.ota_url || "";
  const dimVal = String(draft.sleepDimMin ?? "").trim() === "" ? -1 : Number(draft.sleepDimMin);
  const offVal = String(draft.sleepOffMin ?? "").trim() === "" ? -1 : Number(draft.sleepOffMin);
  const revoked = isRevokedInMoss(inspectData);
  const ssidEdgeSpaces = draft.newSsid.trim() !== "" && draft.newSsid !== draft.newSsid.trim();

  return (
    <div className="xiaozhi-usb-card" id="xiaozhi-usb-card" ref={cardRef} tabIndex={-1}>
      <p className="xiaozhi-usb-intro">
        用 USB 数据线连接小智（若无法自动进下载模式，先按住板子 BOOT 键再点“读取板子配置”），免开手机热点直写闪存 NVS 分区的 Wi-Fi、本机语音地址与设备密钥；原已保存的 Wi-Fi 密码不回显。
      </p>

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
              setSelectedPort(e.target.value);
              resetBoardDraft();
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
            onClick={loadPorts}
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
                  <div className="xz-quick-fills">
                    {otaIp && (
                      <button
                        type="button"
                        className="xz-chip-btn"
                        disabled={Boolean(busy)}
                        onClick={() => {
                          otaTouchedRef.current = true;
                          updateDraft((prev) => ({ ...prev, otaUrl: otaIp }));
                        }}
                      >
                        填入本机 Wi-Fi IP 地址
                      </button>
                    )}
                    {otaMdns && otaMdns !== otaIp && (
                      <button
                        type="button"
                        className="xz-chip-btn"
                        disabled={Boolean(busy)}
                        onClick={() => {
                          otaTouchedRef.current = true;
                          updateDraft((prev) => ({ ...prev, otaUrl: otaMdns }));
                        }}
                      >
                        填入 moss.local 地址
                      </button>
                    )}
                  </div>
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
