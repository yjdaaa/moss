import React, { useCallback, useEffect, useRef, useState } from "react";
import { Play, Square, ShieldCheck, ShieldOff, Pencil, Check, X, FlaskConical, Trash2, Download, Lock, ChevronDown, Copy, RefreshCw, Volume2 } from "lucide-react";
import { showAlert, showConfirm } from "../appDialog.js";
import { voiceApi } from "../voiceApi.js";
import { SaveBar, VoiceGroup } from "./VoiceLayout.jsx";
import VoiceMemoryCard from "./VoiceMemoryCard.jsx";
import VoicePluginsCard from "./VoicePluginsCard.jsx";
import VoiceSpeakerCard from "./VoiceSpeakerCard.jsx";
import VoiceHomeCard from "./VoiceHomeCard.jsx";
import VoiceHomeApprovalCard from "./VoiceHomeApprovalCard.jsx";
import {
  DEFAULT_JEV_DECISIONS_URL,
  DEFAULT_JEV_MODEL,
  DOUBAO_MODEL_OPTIONS,
  EXPERIENCE_KEYS,
  JEV_PRESET_MODELS,
  LOCAL_MODEL_KEYS,
  MIX_SECTIONS,
  SECTIONS,
  VOICE_POLL_MS,
  allowDeviceConfirmText,
  applyDefaultTtsVoice,
  BUILTIN_ASR_CORRECTION_PREVIEW,
  PERSONA_STYLE_OPTIONS,
  GOGGLE_BOT_POSES,
  SCREEN_CARD_STYLE_OPTIONS,
  builtinDeviceControlsView,
  builtinDeviceInfoView,
  builtinDeviceView,
  builtinJevForm,
  builtinJevKeyPlaceholder,
  builtinJevSavePayload,
  builtinJevSummary,
  builtinKeyPlaceholder,
  builtinModeView,
  builtinModelForm,
  builtinModelSummary,
  builtinSectionSourceView,
  builtinSourceSwitchPatch,
  builtinSourceSwitchPayload,
  canAutoFetchModels,
  canSaveSection,
  chatOnlyAudioGatewayError,
  chatOnlyGatewayName,
  experienceSettingsError,
  filterModelOptions,
  finalUrl,
  findPeerCloudSource,
  formDirty,
  formatBytes,
  isLocalLayaJevModel,
  isSectionBlank,
  jevFingerprint,
  jevModelError,
  localModelGroups,
  localModelSettingsError,
  localModelView,
  localGroupPatch,
  localSettingsForm,
  localSettingsPayload,
  mergeGroup,
  syncJevDraft,
  syncLocalDraft,
  modelsListError,
  modelsListKey,
  savedNoteText,
  resolveFallbackTtsModel,
  selectLocalModel,
  selectedVoices,
  isDoubao,
  sectionFingerprint,
  sectionInputError,
  sectionPayload,
  switchKind,
  ttsVoiceOptionsFor,
  voiceErrorText,
  VOICE_TIMING_STAGES,
  voiceTimingReport,
} from "../voiceState.js";

// 内置语音服务（内置语音技术书 NV1 起）。设备连到 MOSS 本身，不需要外部服务端。
// 识别、大模型、合成三项配齐后为对话模式（NV5），否则为回声测试。
// 语音中心页按标签分开摆放本文件导出的各部分（UI-3）：服务与设备、模型、记录与诊断。

const TONE_CLASS = { ok: "ob-pill--ok", info: "ob-pill--info", warn: "ob-pill--warn", bad: "ob-pill--bad", off: "ob-pill--off" };

function formatTime(seconds) {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false, month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

function restartEffect(next) {
  const mode = builtinModeView(next);
  return next?.restarted ? `内置服务已重启（${mode.text}模式）` : `下次启动内置服务时生效（${mode.text}模式）`;
}

/** 内置服务状态：每 4 秒刷新；`busy` 在本服务的各部分之间共享，避免并发操作。 */
export function useBuiltinStatus() {
  const [status, rawSetStatus] = useState(null);
  const [busy, setBusy] = useState("");
  const mounted = useRef(true);
  const versionRef = useRef(0);

  const setStatus = useCallback((next) => {
    versionRef.current += 1;
    rawSetStatus(next);
  }, []);

  const refresh = useCallback(async () => {
    const ver = versionRef.current;
    try {
      const next = await voiceApi.builtinStatus();
      if (mounted.current && versionRef.current === ver) {
        rawSetStatus(next);
      }
    } catch (error) {
      // 读过一次后保留上次状态，只标出刷新失败；不卸载各分组，避免丢失未保存的填写
      if (mounted.current && versionRef.current === ver) {
        rawSetStatus((prev) => (prev && !prev.error ? { ...prev, poll_error: voiceErrorText(error) } : { error: voiceErrorText(error) }));
      }
    }
  }, []);

  const activeTurn = Boolean(
    status?.running
    && Array.isArray(status?.sessions)
    && status.sessions.some((item) => item?.state === "listening" || item?.state === "thinking" || item?.state === "speaking"),
  );

  useEffect(() => {
    mounted.current = true;
    refresh();
    return () => {
      mounted.current = false;
    };
  }, [refresh]);

  useEffect(() => {
    const intervalMs = activeTurn ? 140 : VOICE_POLL_MS;
    const timer = setInterval(refresh, intervalMs);
    return () => clearInterval(timer);
  }, [activeTurn, refresh]);
  const act = useCallback(async (label, action) => {
    setBusy(label);
    try {
      const next = await action();
      if (mounted.current && next) setStatus(next);
      return next;
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: `${label}失败`, tone: "error" });
      return null;
    } finally {
      if (mounted.current) setBusy("");
    }
  }, [setStatus]);

  return { status, setStatus, busy, setBusy, act };
}

/** 服务与设备：运行状态、启停、设备要填的 OTA 地址、设备允许列表。 */
export function BuiltinServicePanel({ builtin }) {
  const { status, busy, act } = builtin;
  const [editing, setEditing] = useState({ id: "", name: "" });
  const [expanded, setExpanded] = useState("");
  const [asking, setAsking] = useState("");
  const [controlDrafts, setControlDrafts] = useState({});
  const [controlNotice, setControlNotice] = useState({});
  const [botNotice, setBotNotice] = useState("");
  if (!status) return <p className="voice-muted">正在读取内置服务状态…</p>;
  if (status.error) return <div className="voice-hint voice-hint--bad">{status.error}</div>;

  const running = Boolean(status.running);
  const devices = Array.isArray(status.devices) ? status.devices : [];
  const sessions = Array.isArray(status.sessions) ? status.sessions : [];
  const modeView = builtinModeView(status);
  const currentCardStyle = status.local?.settings?.screen_card_style === "goggle_bot" ? "goggle_bot" : "moss_hud";
  const botGifs = status.goggle_bot_gifs && typeof status.goggle_bot_gifs === "object" ? status.goggle_bot_gifs : {};
  const onlineColorDevice = devices.find((d) => {
    if (d.status !== "allowed") return false;
    const sess = sessions.find((s) => s.device_id === d.device_id);
    return Boolean(sess && sess.screen_monochrome !== true);
  }) || null;

  const switchCardStyle = async (nextStyle) => {
    if (nextStyle === currentCardStyle) return;
    setBotNotice("");
    const opt = SCREEN_CARD_STYLE_OPTIONS.find((item) => item.id === nextStyle);
    const updated = await act("切换卡片画风", () => voiceApi.localSave({ screen_card_style: nextStyle }));
    if (updated) {
      setBotNotice(`已切换为「${opt?.shortLabel || nextStyle}」画风（在线小智无需重连立即生效）`);
    }
  };

  const pushBotPose = async (pose) => {
    if (!onlineColorDevice) return;
    setAsking(onlineColorDevice.device_id);
    setBotNotice("");
    try {
      await voiceApi.deviceRequest(onlineColorDevice.device_id, "push_bot_pose", { pose: pose.id });
      setBotNotice(`已将「小 MOSS · ${pose.label}」投送到 ${builtinDeviceView(onlineColorDevice).title}`);
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "投送小机器人预览失败", tone: "error" });
    } finally {
      setAsking("");
    }
  };

  const allow = async (device) => {
    if (!(await showConfirm(allowDeviceConfirmText(device), { title: "允许此设备？", tone: "warning", confirmText: "允许" }))) return;
    await act("允许设备", () => voiceApi.deviceSetStatus(device.device_id, "allowed"));
  };
  const revoke = async (device) => {
    if (!(await showConfirm(`撤销后，设备“${builtinDeviceView(device).title}”将无法再连接内置语音服务，直到重新允许。`, { title: "撤销此设备？", tone: "warning", danger: true, confirmText: "撤销" }))) return;
    await act("撤销设备", () => voiceApi.deviceSetStatus(device.device_id, "revoked"));
  };
  const saveName = async () => {
    const { id, name } = editing;
    setEditing({ id: "", name: "" });
    await act("修改名称", () => voiceApi.deviceRename(id, name));
  };
  /** 2026-11-02：设备信息面板的「刷新信息」——让设备回一次 `self.get_device_status`。 */
  const refreshDeviceInfo = async (device) => {
    setAsking(device.device_id);
    try {
      await voiceApi.deviceRequest(device.device_id, "refresh_info");
      await showAlert("已请求设备上报信息，回包后这里的内容会更新（一般 1~2 秒）。", { title: "已发送", tone: "success" });
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "刷新信息失败", tone: "error" });
    } finally {
      setAsking("");
    }
  };
  /** 会打断当前对话，所以单独确认；重启后设备重新走 OTA，固件版本随之刷新。 */
  const rebootDevice = async (device) => {
    const view = builtinDeviceView(device);
    if (!(await showConfirm(
      `重启设备“${view.title}”？正在进行的对话会中断，屏幕会黑一下；重启后它会重新上报固件版本。`,
      { title: "重启设备？", tone: "warning", danger: true, confirmText: "重启" },
    ))) return;
    setAsking(device.device_id);
    try {
      await voiceApi.deviceRequest(device.device_id, "reboot");
      await showAlert("已发送重启指令，设备会在几秒后重新连接。", { title: "已发送", tone: "success" });
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "重启设备失败", tone: "error" });
    } finally {
      setAsking("");
    }
  };
  const sendDeviceControl = async (device, request, value, doneText) => {
    setAsking(device.device_id);
    setControlNotice((prev) => ({ ...prev, [device.device_id]: "" }));
    try {
      await voiceApi.deviceRequest(device.device_id, request, value);
      setControlNotice((prev) => ({ ...prev, [device.device_id]: doneText }));
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "设备操作失败", tone: "error" });
    } finally {
      setAsking("");
    }
  };

  const copyOta = async (url = status.ota_url, label = "设备 OTA 地址") => {
    if (!url) return;
    try {
      await navigator.clipboard.writeText(url);
      await showAlert(`${label}已复制，粘贴到设备配网页面即可`, { title: "已复制", tone: "success" });
    } catch {
      await showAlert("无法访问剪贴板，请手动选中地址复制", { title: "复制失败", tone: "error" });
    }
  };
  return (
    <div className="voice-block" id="voice-builtin-card">
      <div className="voice-block-h">
        <h3>内置语音服务</h3>
        <span className={`ob-pill ${TONE_CLASS[modeView.tone]}`} id="voice-builtin-mode" title={modeView.hint}>
          {modeView.text}
        </span>
        <span className={`ob-pill ${running ? TONE_CLASS.ok : TONE_CLASS.off}`} id="voice-builtin-state">
          {running ? "运行中" : "未运行"}
        </span>
        <span className="voice-actions voice-push">
          {running ? (
            <button type="button" className="btn btn-danger" disabled={Boolean(busy)} onClick={() => act("停止内置服务", () => voiceApi.builtinStop())} id="voice-builtin-stop">
              <Square size={13} /> 停止
            </button>
          ) : (
            <button type="button" className="btn btn-primary" disabled={Boolean(busy)} onClick={() => act("启动内置服务", () => voiceApi.builtinStart())} id="voice-builtin-start">
              <Play size={13} /> 启动
            </button>
          )}
        </span>
      </div>
      <p className="voice-muted">
        {modeView.text === "对话" ? "识别、大模型、合成已配齐。" : "模型没配齐，先以回声测试运行；在“模型”标签配齐后自动切换为对话。"}
        {modeView.autostart}
      </p>
      {status.poll_error && <div className="voice-hint voice-hint--warn">状态刷新失败（显示的是上次读取的结果）：{status.poll_error}</div>}
      {status.last_error && !running && (
        <div className="voice-hint voice-hint--bad" id="voice-builtin-error">
          自动启动失败：{status.last_error}
        </div>
      )}
      <div className="voice-kv">
        <span className="voice-inline">
          设备 OTA 地址（固定不变）：<b className="mono voice-break" id="voice-builtin-ota">{status.ota_url || "未能获取本机局域网地址"}</b>
          {status.ota_url && (
            <button type="button" className="btn btn-ghost voice-inline-btn" onClick={() => copyOta(status.ota_url, "设备 OTA 地址")} id="voice-builtin-ota-copy" title="复制设备 OTA 地址">
              <Copy size={13} /> 复制
            </button>
          )}
          {status.ota_ip_url && status.ota_ip_url !== status.ota_url && (
            <>
              <span className="voice-muted">· 当前 Wi-Fi IP：</span>
              <span className="mono voice-break" id="voice-builtin-ota-ip">{status.ota_ip_url}</span>
              <button type="button" className="btn btn-ghost voice-inline-btn" onClick={() => copyOta(status.ota_ip_url, "当前 Wi-Fi OTA 地址")} id="voice-builtin-ota-ip-copy" title="复制当前 Wi-Fi IP 地址">
                <Copy size={13} /> 复制 IP
              </button>
            </>
          )}
        </span>
        <span className="voice-muted">
          在设备配网页面填入上方 <code>moss.local</code> 地址后，换 Wi-Fi 无需重改；若公共网络屏蔽局域网广播，可改用右侧当前 Wi-Fi IP 地址。端口 {status.ws_port} / {status.http_port}，在“服务”标签下方修改。
        </span>
      </div>

      <div className="ob-tbl voice-tbl">
        <table>
          <thead>
            <tr>
              <th>设备</th>
              <th>型号 / 固件</th>
              <th>最近来访</th>
              <th>状态</th>
              <th>操作</th>
            </tr>
          </thead>
          <tbody>
            {devices.length === 0 ? (
              <tr>
                <td colSpan={5} className="voice-muted">还没有设备连接过。启动服务并把设备 OTA 地址指向本机后，设备会出现在这里。</td>
              </tr>
            ) : (
              devices.map((device) => {
                const view = builtinDeviceView(device);
                const session = sessions.find((item) => item.device_id === device.device_id) || null;
                const online = Boolean(session);
                const details = builtinDeviceInfoView(device);
                const controls = builtinDeviceControlsView(device, session, status.device_prefs);
                const draftCtrl = controlDrafts[device.device_id] || {};
                const volVal = draftCtrl.volume ?? controls.volume;
                const briVal = draftCtrl.brightness ?? controls.brightness;
                return (
                  <React.Fragment key={device.device_id}>
                  <tr>
                    <td>
                      {editing.id === device.device_id ? (
                        <span className="voice-actions">
                          <input className="text-input" value={editing.name} maxLength={32} onChange={(e) => setEditing({ id: device.device_id, name: e.target.value })} aria-label="设备名称" />
                          <button type="button" className="btn btn-ghost" onClick={saveName} title="保存设备名称">
                            <Check size={13} />
                          </button>
                          <button type="button" className="btn btn-ghost" onClick={() => setEditing({ id: "", name: "" })} title="取消">
                            <X size={13} />
                          </button>
                        </span>
                      ) : (
                        <>
                          <b>{view.title}</b>
                          {device.name && <div className="voice-muted mono">{device.device_id}</div>}
                          {online && <div className="voice-ok">在线</div>}
                        </>
                      )}
                    </td>
                    <td>
                      {device.model || "—"}
                      <div className="voice-muted">{device.firmware_version || "—"}</div>
                    </td>
                    <td className="mono">{formatTime(device.last_seen)}</td>
                    <td>
                      <span className={`ob-pill ${TONE_CLASS[view.tone]}`}>{view.text}</span>
                    </td>
                    <td>
                      <span className="voice-actions">
                        {view.canAllow && (
                          <button type="button" className="btn btn-primary" disabled={Boolean(busy)} onClick={() => allow(device)}>
                            <ShieldCheck size={13} /> 允许
                          </button>
                        )}
                        {view.canRevoke && (
                          <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => revoke(device)}>
                            <ShieldOff size={13} /> 撤销
                          </button>
                        )}
                        <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => setEditing({ id: device.device_id, name: device.name || "" })} title="修改名称">
                          <Pencil size={13} />
                        </button>
                        <button
                          type="button"
                          className="btn btn-ghost"
                          disabled={Boolean(busy)}
                          onClick={() => setExpanded(expanded === device.device_id ? "" : device.device_id)}
                          title="设备信息"
                        >
                          {expanded === device.device_id ? "收起" : "信息"}
                        </button>
                      </span>
                    </td>
                  </tr>
                  {expanded === device.device_id && (
                    <tr>
                      <td colSpan={5}>
                        {device.status === "allowed" && (
                          <div className="voice-device-controls" id={`voice-device-controls-${device.device_id}`}>
                            <div className="voice-device-controls-head">
                              <span className="voice-actions">
                                <b>小智硬件在线快捷控制</b>
                                {controls.screenResolution && (
                                  <span className="ob-pill ob-pill--info voice-tag mono">
                                    {controls.screenResolution}
                                    {controls.screenMonochrome === true ? " OLED" : " 彩屏"}
                                  </span>
                                )}
                              </span>
                              <span className="voice-muted">
                                {online
                                  ? "调节后自动记忆偏好（静音 0% / 息屏 0% / 按键模式在设备断电重连后自动恢复）"
                                  : "设备离线（上线后可即时调节音量、亮度、深浅色主题、按键抗噪模式与投送雕刻预览）"}
                              </span>
                            </div>
                            <div className="voice-device-controls-grid">
                              <div className="voice-device-control-item">
                                <span className="voice-device-control-label">
                                  <span>扬声器音量</span>
                                  <b className="mono">{volVal}%</b>
                                </span>
                                <div className="voice-actions">
                                  <input
                                    type="range"
                                    min="0"
                                    max="100"
                                    step="5"
                                    value={volVal}
                                    disabled={Boolean(asking) || !online || !controls.canVolume}
                                    onChange={(e) =>
                                      setControlDrafts((prev) => ({
                                        ...prev,
                                        [device.device_id]: { ...(prev[device.device_id] || {}), volume: Number(e.target.value) },
                                      }))
                                    }
                                    aria-label="设备音量"
                                  />
                                  <button
                                    type="button"
                                    className="btn btn-ghost"
                                    disabled={Boolean(asking) || !online || !controls.canVolume}
                                    onClick={() => sendDeviceControl(device, "set_volume", { volume: volVal }, `音量已设为 ${volVal}%（重连自动保持）`)}
                                  >
                                    应用
                                  </button>
                                  <button
                                    type="button"
                                    className="btn btn-ghost"
                                    disabled={Boolean(asking) || !online || !controls.canVolume}
                                    onClick={() => {
                                      setControlDrafts((prev) => ({
                                        ...prev,
                                        [device.device_id]: { ...(prev[device.device_id] || {}), volume: 0 },
                                      }));
                                      sendDeviceControl(device, "set_volume", { volume: 0 }, "已静音（0%，设备重启后 MOSS 会自动恢复静音）");
                                    }}
                                  >
                                    静音
                                  </button>
                                </div>
                              </div>
                              <div className="voice-device-control-item">
                                <span className="voice-device-control-label">
                                  <span>屏幕亮度</span>
                                  <b className="mono">{briVal}%</b>
                                </span>
                                <div className="voice-actions">
                                  <input
                                    type="range"
                                    min="0"
                                    max="100"
                                    step="5"
                                    value={briVal}
                                    disabled={Boolean(asking) || !online || !controls.canBrightness}
                                    onChange={(e) =>
                                      setControlDrafts((prev) => ({
                                        ...prev,
                                        [device.device_id]: { ...(prev[device.device_id] || {}), brightness: Number(e.target.value) },
                                      }))
                                    }
                                    aria-label="设备屏幕亮度"
                                  />
                                  <button
                                    type="button"
                                    className="btn btn-ghost"
                                    disabled={Boolean(asking) || !online || !controls.canBrightness}
                                    onClick={() => sendDeviceControl(device, "set_brightness", { brightness: briVal }, `屏幕亮度已设为 ${briVal}%`)}
                                  >
                                    应用
                                  </button>
                                </div>
                              </div>
                              <div className="voice-device-control-item">
                                <span className="voice-device-control-label">
                                  <span>屏幕主题与激光预览投屏</span>
                                </span>
                                <div className="voice-actions">
                                  <button
                                    type="button"
                                    className={`btn btn-ghost${controls.theme === "dark" ? " is-active" : ""}`}
                                    disabled={Boolean(asking) || !online || !controls.canTheme}
                                    onClick={() => sendDeviceControl(device, "set_theme", { theme: "dark" }, "已切换为深色主题")}
                                  >
                                    深色屏
                                  </button>
                                  <button
                                    type="button"
                                    className={`btn btn-ghost${controls.theme === "light" ? " is-active" : ""}`}
                                    disabled={Boolean(asking) || !online || !controls.canTheme}
                                    onClick={() => sendDeviceControl(device, "set_theme", { theme: "light" }, "已切换为浅色主题")}
                                  >
                                    浅色屏
                                  </button>
                                  <button
                                    type="button"
                                    className="btn btn-primary"
                                    disabled={Boolean(asking) || !online}
                                    title={
                                      controls.screenMonochrome === true
                                        ? "已识别为单色 OLED 官方固件：将把最新雕刻预览的材料、尺寸、功率与速度作为滚动文字预览卡投送到 OLED 屏"
                                        : controls.canPreviewImage
                                          ? "已识别为彩屏：将把最近生成的激光雕刻预览缩略图推送到小智屏幕上显示"
                                          : "彩屏推送 PNG 缩略图，普通单色 OLED 自动适配推送滚动参数预览卡（无需改官方固件）"
                                    }
                                    onClick={() =>
                                      sendDeviceControl(
                                        device,
                                        "push_preview",
                                        undefined,
                                        controls.screenMonochrome === true
                                          ? "已将雕刻预览参数卡推送到小智 OLED 屏幕滚动显示"
                                          : "已将最新雕刻预览推送到小智屏幕",
                                      )
                                    }
                                  >
                                    {controls.screenMonochrome === true ? "投送预览参数卡" : "投送雕刻预览"}
                                  </button>
                                </div>
                              </div>
                              <div className="voice-device-control-item">
                                <span className="voice-device-control-label">
                                  <span>按键拾音模式（排风抗噪）</span>
                                  <span className="voice-muted mono">
                                    {controls.talkMode === "press_to_talk" ? "长按说话" : "单击唤醒"}
                                  </span>
                                </span>
                                <div className="voice-actions">
                                  <button
                                    type="button"
                                    className={`btn btn-ghost${controls.talkMode === "click_to_talk" ? " is-active" : ""}`}
                                    disabled={Boolean(asking) || !online || !controls.canPressToTalk}
                                    title="安静环境推荐：短按 BOOT 键或说唤醒词即可连续对话"
                                    onClick={() =>
                                      sendDeviceControl(
                                        device,
                                        "set_talk_mode",
                                        { mode: "click_to_talk" },
                                        "已切换为「单击唤醒」模式（已同步写入设备 NVS 与上位机记忆）",
                                      )
                                    }
                                  >
                                    单击唤醒
                                  </button>
                                  <button
                                    type="button"
                                    className={`btn btn-ghost${controls.talkMode === "press_to_talk" ? " is-active" : ""}`}
                                    disabled={Boolean(asking) || !online || !controls.canPressToTalk}
                                    title="激光雕刻排风/气泵轰鸣时推荐：按住 BOOT 键说话、松手立即判停，彻底隔绝环境噪音误触发"
                                    onClick={() =>
                                      sendDeviceControl(
                                        device,
                                        "set_talk_mode",
                                        { mode: "press_to_talk" },
                                        "已切换为「长按说话（抗噪）」模式：按住设备按键说话，松手即停",
                                      )
                                    }
                                  >
                                    长按说话（抗噪）
                                  </button>
                                </div>
                              </div>
                            </div>
                            {controlNotice[device.device_id] && (
                              <div className="voice-ok" style={{ marginTop: 6 }}>{controlNotice[device.device_id]}</div>
                            )}
                          </div>
                        )}
                        <p className="voice-muted">
                          设备信息：版本与芯片来自设备开机上报，协议与音频参数来自连接握手；「刷新信息」只更新实时状态。回声消除不在本面板断言——固件只在启用服务端 AEC 时才上报 `aec` 字段，设备端 AEC 不上报。
                        </p>
                        <table className="voice-table">
                          <tbody>
                            {details.rows.map(([label, value]) => (
                              <tr key={label}>
                                <th scope="row">{label}</th>
                                <td className="mono">{value}</td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                        <span className="voice-actions">
                          <button
                            type="button"
                            className="btn btn-ghost"
                            disabled={Boolean(asking) || !online}
                            onClick={() => refreshDeviceInfo(device)}
                            title="让设备重新上报实时状态（音量、屏幕、电池、网络）"
                          >
                            刷新信息
                          </button>
                          <button
                            type="button"
                            className="btn btn-ghost"
                            disabled={Boolean(asking) || !online}
                            onClick={() => rebootDevice(device)}
                            title="重启设备，让它重新上报固件版本"
                          >
                            重启设备
                          </button>
                        </span>
                      </td>
                    </tr>
                  )}
                  </React.Fragment>
                );
              })
            )}
          </tbody>
        </table>
      </div>

      <div className="voice-bot-showcase" id="voice-goggle-bot-card">
        <div className="voice-bot-showcase-head">
          <span className="voice-actions">
            <b>小智彩屏动态卡片 & 护目镜小机器人 GIF</b>
            <span className="ob-pill ob-pill--ok voice-tag">零 Flash 风险 · 纯内存渲染</span>
          </span>
          <div className="voice-bot-style-switch" role="group" aria-label="小智彩屏卡片画风">
            <span className="voice-muted">卡片画风：</span>
            {SCREEN_CARD_STYLE_OPTIONS.map((opt) => {
              const active = currentCardStyle === opt.id;
              return (
                <button
                  key={opt.id}
                  type="button"
                  className={`btn btn-ghost${active ? " is-active" : ""}`}
                  disabled={Boolean(busy)}
                  title={opt.hint}
                  id={`voice-card-style-${opt.id}`}
                  onClick={() => switchCardStyle(opt.id)}
                >
                  {opt.shortLabel}
                </button>
              );
            })}
          </div>
        </div>
        <div className="voice-bot-scene-tags">
          <span className="ob-pill ob-pill--info voice-tag">① 待电脑审批警戒卡（含倒计时条）</span>
          <span className="ob-pill ob-pill--info voice-tag">② 完工庆祝 / 急停断点 5 秒徽章卡（固件自动淡出）</span>
          <span className="ob-pill ob-pill--info voice-tag">③ 本地音乐像素唱机 / 听歌小机器人卡</span>
          <span className="ob-pill ob-pill--off voice-tag">1:1 激光刀路预览 + 实时加工进度 HUD 同步切换画风</span>
        </div>
        <div className="voice-bot-gif-grid" id="voice-goggle-bot-gallery">
          {GOGGLE_BOT_POSES.map((pose) => {
            const gifSrc = botGifs[pose.id];
            return (
              <div className="voice-bot-gif-tile" key={pose.id} data-pose={pose.id}>
                <div className="voice-bot-gif-frame">
                  {gifSrc ? (
                    <img src={gifSrc} alt={`小 MOSS 护目镜小机器人：${pose.label}`} className="voice-bot-gif-img" />
                  ) : (
                    <span className="voice-muted mono">GIF</span>
                  )}
                </div>
                <div className="voice-bot-gif-title">{pose.label}</div>
                <div className="voice-bot-gif-desc">{pose.desc}</div>
                <button
                  type="button"
                  className="btn btn-ghost"
                  disabled={Boolean(asking) || Boolean(busy) || !onlineColorDevice}
                  title={
                    onlineColorDevice
                      ? `将「${pose.label}」1:1 投送到在线彩屏小智试看`
                      : "连接并允许彩屏小智（如立创·实战派 320×240）后可一键推屏试看"
                  }
                  id={`voice-push-bot-${pose.id}`}
                  onClick={() => pushBotPose(pose)}
                >
                  推到小智屏
                </button>
              </div>
            );
          })}
        </div>
        {botNotice && <div className="voice-ok" id="voice-bot-showcase-notice">{botNotice}</div>}
      </div>
    </div>
  );
}

/** 已读取的服务商模型列表内存缓存（切换标签页或同服务商跨分区时直接复用，点“刷新列表”时强制重读）。 */
const REMOTE_MODELS_CACHE = new Map();

function remoteCacheKey(section, form, entry, peerKeySet = false) {
  const baseKey = modelsListKey(form, entry, peerKeySet);
  if (isDoubao(form)) return baseKey;
  const hasKey = Boolean(String(form?.api_key || "").trim() || entry?.key_set || peerKeySet);
  const isClineLlm = section === "llm" && chatOnlyGatewayName(form?.url) === "Cline (api.cline.bot)";
  return hasKey && !isClineLlm ? `shared:${baseKey}` : `${section}:${baseKey}`;
}

/** 内置服务的一项云端模型（大模型 / 语音识别 / 语音合成）：支持拉取全量模型下拉筛选或手填；先测试、后保存；密钥只提交、不回显。 */
function BuiltinModelGroup({ section, builtin }) {
  const { status, setStatus, busy } = builtin;
  const current = SECTIONS.find((item) => item.key === section);
  const entry = status.models?.[section];
  const baseline = builtinModelForm(section, entry);
  const [form, setForm] = useState(baseline);
  const [test, setTest] = useState(null);
  const [result, setResult] = useState(null);
  const [localBusy, setLocalBusy] = useState("");
  const [remoteModels, setRemoteModels] = useState({ key: "", items: [], modelSections: null, detail: "", ok: null, loading: false });
  const [menuOpen, setMenuOpen] = useState(false);
  const [voiceMenuOpen, setVoiceMenuOpen] = useState(false);
  const [playingAudio, setPlayingAudio] = useState(false);
  const [filtering, setFiltering] = useState(false);
  const [showAllModels, setShowAllModels] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const comboRef = useRef(null);
  const voiceComboRef = useRef(null);
  const audioRef = useRef(null);
  const fetchSeq = useRef(0);
  const dirty = formDirty(baseline, form);
  const saveEnabled = canSaveSection(section, form, test);
  const doubao = isDoubao(form);
  const sourceView = builtinSectionSourceView(section, status);
  const localOn = sourceView.localOn;
  const sectionBusy = Boolean(busy || localBusy);
  const n = (suffix) => `voice-builtin-${suffix}-${section}`;
  const peerCloud = findPeerCloudSource(section, status, form.url);
  const peerKeySet = Boolean(peerCloud.sameOrigin?.keySet);
  const currentListKey = modelsListKey(form, entry, peerKeySet);
  const currentCacheKey = remoteCacheKey(section, form, entry, peerKeySet);
  const cachedModels = !doubao ? REMOTE_MODELS_CACHE.get(currentCacheKey) : null;
  const rawOptions = doubao
    ? DOUBAO_MODEL_OPTIONS[section] || []
    : remoteModels.key === currentListKey && remoteModels.ok !== null
      ? remoteModels.items
      : cachedModels
        ? cachedModels.items
        : [];
  const modelSections = !doubao
    ? remoteModels.key === currentListKey && remoteModels.ok !== null
      ? remoteModels.modelSections
      : cachedModels?.modelSections || null
    : null;
  const allOptions = filterModelOptions(rawOptions, "", false, { section, modelSections });
  const matchedOptions = allOptions.filter((item) => item.sections?.includes(section));
  const anyClassified = allOptions.some((item) => item.sections?.length > 0);
  const onlyMatched = !doubao && !showAllModels && anyClassified;
  const scopedOptions = onlyMatched ? matchedOptions : allOptions;
  const filteredOptions = filterModelOptions(rawOptions, form.model, filtering, { section, onlyMatched, modelSections });
  const voiceOptions = section === "tts" ? ttsVoiceOptionsFor(form) : [];
  const gatewayAudioErr = !doubao ? chatOnlyAudioGatewayError(section, form.url) : "";

  useEffect(() => {
    if (!menuOpen && !voiceMenuOpen) return undefined;
    const handleOutsideClick = (event) => {
      if (comboRef.current && !comboRef.current.contains(event.target)) {
        setMenuOpen(false);
        setFiltering(false);
      }
      if (voiceComboRef.current && !voiceComboRef.current.contains(event.target)) {
        setVoiceMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", handleOutsideClick);
    return () => document.removeEventListener("mousedown", handleOutsideClick);
  }, [menuOpen, voiceMenuOpen]);

  useEffect(() => {
    return () => {
      if (audioRef.current) {
        audioRef.current.pause();
        audioRef.current = null;
      }
    };
  }, []);

  const update = (key, value) => {
    setForm((all) => ({ ...all, [key]: value }));
    setResult(null);
  };

  const stopAudioPreview = () => {
    if (audioRef.current) {
      audioRef.current.pause();
      audioRef.current = null;
    }
    setPlayingAudio(false);
  };

  const toggleAudioPreview = async () => {
    if (!test?.audio_data_url) return;
    if (playingAudio && audioRef.current) {
      stopAudioPreview();
      return;
    }
    stopAudioPreview();
    try {
      const audio = new Audio(test.audio_data_url);
      audioRef.current = audio;
      setPlayingAudio(true);
      audio.onended = () => setPlayingAudio(false);
      audio.onerror = () => setPlayingAudio(false);
      await audio.play();
    } catch {
      setPlayingAudio(false);
    }
  };

  const fetchModels = async ({ auto = false, nextForm = form } = {}) => {
    if (isDoubao(nextForm)) return;
    const err = modelsListError(nextForm, section);
    if (err) {
      if (!auto) setRemoteModels({ key: "", items: [], modelSections: null, detail: err, ok: false, loading: false });
      return;
    }
    const nextPeer = findPeerCloudSource(section, status, nextForm.url);
    const nextPeerKeySet = Boolean(nextPeer.sameOrigin?.keySet);
    const reqKey = modelsListKey(nextForm, entry, nextPeerKeySet);
    const cacheKey = remoteCacheKey(section, nextForm, entry, nextPeerKeySet);
    if (auto) {
      if (!canAutoFetchModels(nextForm, entry, status, section)) return;
      if (remoteModels.loading && remoteModels.key === reqKey) return;
      if (remoteModels.key === reqKey && remoteModels.ok !== null) return;
      const hit = REMOTE_MODELS_CACHE.get(cacheKey);
      if (hit) {
        setRemoteModels({
          key: reqKey,
          items: hit.items,
          modelSections: hit.modelSections || null,
          detail: hit.detail,
          ok: true,
          loading: false,
        });
        return;
      }
    }
    const seq = ++fetchSeq.current;
    setRemoteModels((prev) => ({
      key: reqKey,
      items: prev.key === reqKey ? prev.items : [],
      modelSections: prev.key === reqKey ? prev.modelSections : null,
      detail: "",
      ok: prev.key === reqKey ? prev.ok : null,
      loading: true,
    }));
    try {
      const outcome = await voiceApi.builtinModelsList(section, sectionPayload(section, nextForm));
      if (fetchSeq.current !== seq) return;
      const items = Array.isArray(outcome?.models) ? outcome.models : [];
      const metaSections = outcome?.model_sections && typeof outcome.model_sections === "object" ? outcome.model_sections : null;
      const detail = outcome?.detail || (outcome?.ok ? `已读取 ${items.length} 个模型` : "读取模型列表失败");
      const ok = Boolean(outcome?.ok);
      if (ok && items.length > 0) {
        REMOTE_MODELS_CACHE.set(cacheKey, { items, modelSections: metaSections, detail });
      }
      setRemoteModels({
        key: reqKey,
        items,
        modelSections: metaSections,
        detail,
        ok,
        loading: false,
      });
    } catch (error) {
      if (fetchSeq.current !== seq) return;
      setRemoteModels({
        key: reqKey,
        items: [],
        modelSections: null,
        detail: voiceErrorText(error),
        ok: false,
        loading: false,
      });
    }
  };

  const reusePeerSource = (event) => {
    event?.stopPropagation?.();
    if (!peerCloud.reusable) return;
    const nextForm = { ...form, url: peerCloud.reusable.baseUrl };
    setForm(nextForm);
    setResult(null);
    setMenuOpen(true);
    setFiltering(false);
    fetchModels({ auto: true, nextForm });
  };

  const pickModel = (modelId) => {
    if (section === "tts") {
      setForm((all) => applyDefaultTtsVoice(all, modelId));
      setResult(null);
    } else {
      update("model", modelId);
    }
    setMenuOpen(false);
    setFiltering(false);
    setActiveIndex(-1);
  };

  const handleModelFocus = () => {
    setFiltering(false);
    setActiveIndex(-1);
    if (doubao || allOptions.length > 0 || canAutoFetchModels(form, entry, status, section)) {
      setMenuOpen(true);
      fetchModels({ auto: true });
    }
  };

  const handleModelChange = (event) => {
    const value = event.target.value;
    update("model", value);
    setFiltering(true);
    setActiveIndex(-1);
    if (allOptions.length > 0) setMenuOpen(true);
  };

  const handleModelKeyDown = (event) => {
    if (event.key === "Escape" && menuOpen) {
      event.preventDefault();
      setMenuOpen(false);
      setFiltering(false);
      return;
    }
    if (event.key === "ArrowDown") {
      if (!menuOpen) {
        setMenuOpen(true);
        setFiltering(false);
        fetchModels({ auto: true });
        return;
      }
      if (filteredOptions.length > 0) {
        event.preventDefault();
        setActiveIndex((idx) => (idx + 1) % filteredOptions.length);
      }
      return;
    }
    if (event.key === "ArrowUp" && menuOpen && filteredOptions.length > 0) {
      event.preventDefault();
      setActiveIndex((idx) => (idx <= 0 ? filteredOptions.length - 1 : idx - 1));
      return;
    }
    if (event.key === "Enter" && menuOpen && activeIndex >= 0 && filteredOptions[activeIndex]) {
      event.preventDefault();
      pickModel(filteredOptions[activeIndex].id);
    }
  };

  const toggleMenu = () => {
    const nextOpen = !menuOpen;
    setMenuOpen(nextOpen);
    setFiltering(false);
    setActiveIndex(-1);
    if (nextOpen) fetchModels({ auto: true });
  };


  const runTest = async () => {
    setMenuOpen(false);
    setVoiceMenuOpen(false);
    stopAudioPreview();
    const inputError = sectionInputError(section, form);
    if (inputError) {
      setTest({ ok: false, detail: inputError, fingerprint: "" });
      return;
    }
    const fingerprint = sectionFingerprint(section, form);
    setLocalBusy(`builtin-test-${section}`);
    try {
      const outcome = await voiceApi.builtinModelsTest(section, sectionPayload(section, form));
      setTest({ ...outcome, fingerprint });
    } catch (error) {
      setTest({ ok: false, detail: voiceErrorText(error), fingerprint: "" });
    } finally {
      setLocalBusy("");
    }
  };

  const save = async () => {
    setMenuOpen(false);
    setVoiceMenuOpen(false);
    stopAudioPreview();
    setLocalBusy(`builtin-save-${section}`);
    try {
      const next = await voiceApi.builtinModelsSave({ [section]: sectionPayload(section, form) });
      const nextEntry = next.models?.[section];
      const nextForm = builtinModelForm(section, nextEntry);
      setStatus(next);
      setForm(nextForm);
      const nextPeer = findPeerCloudSource(section, next, nextForm.url);
      const nextListKey = modelsListKey(nextForm, nextEntry, Boolean(nextPeer.sameOrigin?.keySet));
      const nextCacheKey = remoteCacheKey(section, nextForm, nextEntry, Boolean(nextPeer.sameOrigin?.keySet));
      setRemoteModels((prev) => {
        if (prev.items.length > 0) {
          REMOTE_MODELS_CACHE.set(nextCacheKey, { items: prev.items, modelSections: prev.modelSections || null, detail: prev.detail });
          return { ...prev, key: nextListKey };
        }
        return prev;
      });
      setTest(null);
      setResult({ ok: true, text: savedNoteText(`${current.label}设置`, `密钥存本机、不显示 · ${restartEffect(next)}`) });
    } catch (error) {
      setResult({ ok: false, text: `保存失败：${voiceErrorText(error)}` });
    } finally {
      setLocalBusy("");
    }
  };

  const clear = async () => {
    const message = `删除内置服务的“${current.label}”设置与已保存的密钥？删除后内置服务回到回声测试模式。`;
    if (!(await showConfirm(message, { title: "删除设置？", tone: "warning", danger: true, confirmText: "删除" }))) return;
    setMenuOpen(false);
    setLocalBusy(`builtin-clear-${section}`);
    try {
      const next = await voiceApi.builtinModelsSave({}, [section]);
      REMOTE_MODELS_CACHE.delete(currentCacheKey);
      setStatus(next);
      setForm(builtinModelForm(section, next.models?.[section]));
      setRemoteModels({ key: "", items: [], detail: "", ok: null, loading: false });
      setTest(null);
      setResult({ ok: true, text: `已删除${current.label}设置与密钥 · ${restartEffect(next)}` });
    } catch (error) {
      setResult({ ok: false, text: `删除失败：${voiceErrorText(error)}` });
    } finally {
      setLocalBusy("");
    }
  };

  return (
    <VoiceGroup
      id={`builtin-model-${section}`}
      title={current.label}
      summary={builtinModelSummary(section, entry, localOn)}
      tone={localOn ? (sourceView.localReady ? "ok" : "warn") : entry?.configured ? "ok" : undefined}
      dirty={dirty}
    >
      {localOn ? (
        <p className="voice-muted">
          当前已在上方下拉框选用本地{current.label}（{sourceView.selectedLocalModel?.label || "本地模型"}）；这里的云端配置与密钥作为备用保留，随时可在上方下拉切回。
        </p>
      ) : section === "llm" ? (
        <p className="voice-muted">支持云端大模型 API，也支持本机大模型服务（如 Ollama 填 http://127.0.0.1:11434/v1，密钥留空）。</p>
      ) : null}
      <div className="voice-form-grid">
        {section !== "llm" && (
          <label className="voice-wide">
            <span>接口类型</span>
            <select
              className="text-input"
              value={doubao ? "doubao" : "openai"}
              onChange={(e) => {
                setForm((all) => {
                  const switched = switchKind(section, all, e.target.value);
                  return section === "tts" ? applyDefaultTtsVoice(switched, switched.model) : switched;
                });
                setMenuOpen(false);
                setVoiceMenuOpen(false);
                setFiltering(false);
                setResult(null);
              }}
              id={n("model-kind")}
            >
              <option value="openai">OpenAI 兼容（整段{section === "asr" ? "识别" : "合成"}）</option>
              <option value="doubao">豆包流式（{section === "asr" ? "边说边识别" : "边合成边播放"}，首句更快）</option>
            </select>
          </label>
        )}
        <div className="voice-field voice-wide">
          <div className="voice-field-head">
            <label htmlFor={n("model-url")}>地址（{doubao ? "豆包 WebSocket 地址，一般不用改" : current.hint}）</label>
            {!doubao && peerCloud.reusable && (
              <button
                type="button"
                className="voice-link-btn"
                disabled={sectionBusy}
                onClick={reusePeerSource}
                id={n("reuse-peer")}
              >
                复用{peerCloud.reusable.label}地址{peerCloud.reusable.keySet ? "与密钥" : ""}
              </button>
            )}
          </div>
          <input
            className="text-input mono"
            value={form.url}
            onChange={(e) => update("url", e.target.value)}
            onBlur={() => fetchModels({ auto: true })}
            spellCheck="false"
            autoComplete="off"
            id={n("model-url")}
          />
        </div>
        <label>
          <span>{doubao ? "API Key" : "密钥"}</span>
          <input
            className="text-input"
            type="password"
            value={form.api_key}
            placeholder={doubao && !entry?.key_set ? "控制台 API Key；旧版控制台填“APP ID:Access Token”" : builtinKeyPlaceholder(entry, peerCloud.sameOrigin)}
            onChange={(e) => update("api_key", e.target.value)}
            onBlur={() => fetchModels({ auto: true })}
            autoComplete="new-password"
            spellCheck="false"
            id={n("model-key")}
          />
        </label>
        <div className="voice-field" ref={comboRef}>
          <div className="voice-field-head">
            <label htmlFor={n("model-name")}>{doubao ? "资源 ID（留空用默认）" : "模型"}</label>
            {!doubao && (
              <button
                type="button"
                className="voice-link-btn"
                disabled={remoteModels.loading || sectionBusy}
                onClick={() => {
                  setMenuOpen(true);
                  setFiltering(false);
                  fetchModels({ auto: false });
                }}
                id={n("model-fetch")}
              >
                <RefreshCw size={11} className={remoteModels.loading ? "voice-spin" : ""} />
                {remoteModels.loading
                  ? "读取中…"
                  : allOptions.length > 0
                    ? `刷新列表（${onlyMatched && matchedOptions.length < allOptions.length ? `${matchedOptions.length}/${allOptions.length}` : allOptions.length}）`
                    : "读取模型"}
              </button>
            )}
          </div>
          <div className="voice-combo-wrap">
            <input
              className="text-input mono voice-combo-input"
              value={form.model}
              placeholder={
                doubao
                  ? "下拉选择或手动填写资源 ID"
                  : allOptions.length > 0
                    ? onlyMatched && matchedOptions.length < allOptions.length
                      ? `已筛出 ${matchedOptions.length} 个${current.label}模型（共 ${allOptions.length} 个），点击选择或输入查询`
                      : `已读 ${allOptions.length} 个，输入关键词查询或下拉选择`
                    : "输入关键词筛选，或填好地址密钥读取"
              }
              onFocus={handleModelFocus}
              onChange={handleModelChange}
              onKeyDown={handleModelKeyDown}
              spellCheck="false"
              autoComplete="off"
              id={n("model-name")}
            />
            <button
              type="button"
              className="voice-combo-toggle"
              aria-label={doubao ? "展开常用资源 ID" : "展开模型列表"}
              aria-expanded={menuOpen}
              onClick={toggleMenu}
              id={n("model-toggle")}
            >
              <ChevronDown size={14} />
            </button>
          </div>
          {menuOpen && (
            <div className="voice-combo-menu" role="listbox" aria-label={`${current.label}可选模型`} id={n("model-options")}>
              <div className="voice-combo-meta">
                {remoteModels.loading ? (
                  <span>正在读取服务商支持的全部模型…</span>
                ) : !doubao && remoteModels.ok === false && remoteModels.detail ? (
                  <span className="voice-combo-err">{remoteModels.detail}（仍可直接手动输入模型名）</span>
                ) : allOptions.length > 0 ? (
                  <>
                    <span>
                      {doubao
                        ? `常用资源 ID（${allOptions.length} 个）`
                        : matchedOptions.length === 0
                          ? onlyMatched
                            ? `该服务商返回的 ${allOptions.length} 个模型均为大模型，无${current.label}模型`
                            : `未发现专用${current.label}模型，已显示全部 ${allOptions.length} 个`
                          : matchedOptions.length < allOptions.length
                            ? onlyMatched
                              ? `已自动筛出 ${matchedOptions.length} 个${current.label}模型（共 ${allOptions.length} 个）`
                              : `全部 ${allOptions.length} 个模型（含 ${matchedOptions.length} 个${current.label}模型）`
                            : `共 ${allOptions.length} 个${current.label}模型`}
                      {filtering && filteredOptions.length !== scopedOptions.length ? ` · 筛出 ${filteredOptions.length} 个` : matchedOptions.length > 0 || !onlyMatched ? " · 输入关键词可查询" : ""}
                    </span>
                    <span className="voice-actions">
                      {!doubao && matchedOptions.length < allOptions.length && (
                        <button
                          type="button"
                          className="voice-link-btn"
                          onClick={() => setShowAllModels((prev) => !prev)}
                          id={n("model-scope-toggle")}
                        >
                          {showAllModels ? `只看${current.label}（${matchedOptions.length}）` : `显示全部（${allOptions.length}）`}
                        </button>
                      )}
                      {filtering && filteredOptions.length !== scopedOptions.length && (
                        <button type="button" className="voice-link-btn" onClick={() => setFiltering(false)}>
                          清除筛选
                        </button>
                      )}
                    </span>
                  </>
                ) : !doubao && remoteModels.ok === true ? (
                  <span>{remoteModels.detail || "未返回可选模型，请直接手动输入"}</span>
                ) : (
                  <span>先填好上方地址与密钥，点“读取模型”即可拉取全部模型</span>
                )}
              </div>
              {filteredOptions.length > 0 ? (
                <div className="voice-combo-list">
                  {filteredOptions.map((option, idx) => (
                    <button
                      type="button"
                      role="option"
                      aria-selected={option.id === form.model}
                      key={option.id}
                      className={`voice-combo-option mono${option.id === form.model ? " is-selected" : ""}${idx === activeIndex ? " is-active" : ""}`}
                      onClick={() => pickModel(option.id)}
                    >
                      <span className="voice-combo-id">{option.id}</span>
                      {(option.displayHint || option.hint) && <span className="voice-combo-hint">{option.displayHint || option.hint}</span>}
                    </button>
                  ))}
                </div>
              ) : (
                allOptions.length > 0 && !remoteModels.loading && (
                  <div className="voice-combo-empty">
                    {onlyMatched && matchedOptions.length === 0
                      ? `该服务商没有${current.label}模型，建议在上方顶部下拉框切换为「本地${current.label}」，或点右上角“显示全部”手动选择`
                      : `没有包含“${form.model}”的模型，可直接使用当前手动输入的名称`}
                  </div>
                )
              )}
            </div>
          )}
        </div>
        {section === "tts" && (
          <>
            <div className="voice-field" ref={voiceComboRef}>
              <div className="voice-field-head">
                <label htmlFor={n("model-voice")}>{doubao ? "音色 ID（可下拉选常用或手填）" : "音色（可下拉选常用或手填）"}</label>
              </div>
              <div className="voice-combo-wrap">
                <input
                  className="text-input mono voice-combo-input"
                  value={form.voice}
                  placeholder={voiceOptions[0] ? `如 ${voiceOptions[0].id}` : "填写或下拉选择音色"}
                  onFocus={() => setVoiceMenuOpen(true)}
                  onChange={(e) => update("voice", e.target.value)}
                  spellCheck="false"
                  autoComplete="off"
                  id={n("model-voice")}
                />
                <button
                  type="button"
                  className="voice-combo-toggle"
                  aria-label="展开常用音色列表"
                  aria-expanded={voiceMenuOpen}
                  onClick={() => setVoiceMenuOpen((prev) => !prev)}
                  id={n("voice-toggle")}
                >
                  <ChevronDown size={14} />
                </button>
              </div>
              {voiceMenuOpen && voiceOptions.length > 0 && (
                <div className="voice-combo-menu" role="listbox" aria-label="语音合成常用音色" id={n("voice-options")}>
                  <div className="voice-combo-meta">
                    <span>推荐常用音色（共 {voiceOptions.length} 个，也可直接手动输入）</span>
                  </div>
                  <div className="voice-combo-list">
                    {voiceOptions.map((opt) => (
                      <button
                        type="button"
                        role="option"
                        aria-selected={opt.id === form.voice}
                        key={opt.id}
                        className={`voice-combo-option mono${opt.id === form.voice ? " is-selected" : ""}`}
                        onClick={() => {
                          update("voice", opt.id);
                          setVoiceMenuOpen(false);
                        }}
                      >
                        <span className="voice-combo-id">{opt.id}</span>
                        {opt.hint && <span className="voice-combo-hint">{opt.hint}</span>}
                      </button>
                    ))}
                  </div>
                </div>
              )}
            </div>
            <label>
              <span>{doubao ? "语速（0.5 ~ 2.0）" : "语速（0.25 ~ 4.0）"}</span>
              <input className="text-input" inputMode="decimal" value={form.speed} onChange={(e) => update("speed", e.target.value)} />
            </label>
          </>
        )}
      </div>
      <div className="voice-final mono">
        {entry?.configured && !dirty ? "已保存接口地址：" : "将请求的接口地址："}
        {gatewayAudioErr ? `—（当前服务商不支持${current.label}接口）` : finalUrl(section, form.url, form.kind) || "—"}
      </div>
      {gatewayAudioErr && (
        <div className="voice-hint voice-hint--bad" id={n("gateway-audio-error")}>
          {gatewayAudioErr}
        </div>
      )}
      {test ? (
        <div className={`voice-hint voice-hint--${test.ok ? "ok" : "bad"}`} id={n("model-result")}>
          <span>
            {test.ok ? "测试通过" : "测试未通过"}
            {typeof test.elapsed_ms === "number" ? ` · ${(test.elapsed_ms / 1000).toFixed(1)} s` : ""} · {test.detail}
            {test.ok && test.fingerprint !== sectionFingerprint(section, form) && "（测试后内容已修改，请重新测试）"}
          </span>
          {section === "tts" && test.ok && test.audio_data_url && (
            <button
              type="button"
              className="btn btn-ghost voice-inline-btn"
              onClick={toggleAudioPreview}
              id={n("model-play")}
              style={{ marginLeft: "10px" }}
            >
              {playingAudio ? <Square size={12} /> : <Volume2 size={13} />}
              {playingAudio ? "停止试听" : "试听语音"}
            </button>
          )}
        </div>
      ) : (
        <p className="voice-muted">先测试连接，通过后才能保存。密钥留空时测试用已保存的密钥。</p>
      )}
      <SaveBar
        id={n("model-save")}
        target={`${current.label}设置`}
        dirty={dirty}
        busy={sectionBusy}
        saving={localBusy === `builtin-save-${section}`}
        blocked={saveEnabled ? "" : "先测试连接，通过后才能保存"}
        result={result}
        onSave={save}
        extra={
          <>
            <button type="button" className="btn btn-ghost" onClick={runTest} disabled={sectionBusy || isSectionBlank(form)} id={n("model-test")}>
              <FlaskConical size={13} /> {localBusy === `builtin-test-${section}` ? "测试中…" : "测试连接"}
            </button>
            {entry?.configured && (
              <button type="button" className="btn btn-ghost" onClick={clear} disabled={sectionBusy} id={n("model-clear")}>
                <Trash2 size={13} /> 删除此项
              </button>
            )}
          </>
        }
      />
    </VoiceGroup>
  );
}

/** OpenRouter · Jev 决策模型配置卡片（与右上角系统设置共享本机 jev_api_key 与加工反馈开关）。 */
function BuiltinJevGroup({ builtin }) {
  const { status, setStatus, busy } = builtin;
  const baseline = builtinJevForm(status);
  const baselineSig = JSON.stringify(baseline);
  const [form, setForm] = useState(baseline);
  const prevBaselineRef = useRef(baseline);
  const opSeqRef = useRef(0);
  const [test, setTest] = useState(null);
  const [result, setResult] = useState(null);
  const [localBusy, setLocalBusy] = useState("");
  const [menuOpen, setMenuOpen] = useState(false);
  const [remoteModels, setRemoteModels] = useState({ loading: false, ok: false, models: [], detail: "" });
  const comboRef = useRef(null);

  useEffect(() => {
    const prev = prevBaselineRef.current;
    setForm((current) => syncJevDraft(prev, baseline, current));
    if (prev.model !== baseline.model) {
      setTest(null);
    }
    prevBaselineRef.current = baseline;
  }, [baselineSig]);

  useEffect(() => {
    if (!menuOpen) return undefined;
    const onPointerDown = (event) => {
      if (comboRef.current && !comboRef.current.contains(event.target)) {
        setMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => document.removeEventListener("mousedown", onPointerDown);
  }, [menuOpen]);

  const sourceView = builtinSectionSourceView("jev", status);
  const localOn = sourceView.localOn;
  const rawJevModel = status?.local?.settings?.jev_model || status?.jev?.model || DEFAULT_JEV_MODEL;
  const isCurrentLocal = isLocalLayaJevModel(rawJevModel);
  const localInstalled = Boolean(
    status?.jev?.local_installed ||
      (Array.isArray(status?.local?.models) && status.local.models.some((m) => m && m.id === "laya_multilingual" && m.installed)),
  );
  const keyReady = Boolean(
    status?.jev?.key_set || status?.jev?.openrouter_llm_key_set || (!isCurrentLocal && status?.local?.jev_key_configured)
  );
  const typedKey = String(form.api_key || "").trim();
  const modelChanged = String(form.model || "").trim() !== baseline.model;
  const keyOrModelChanged = Boolean(typedKey) || modelChanged;
  const dirty = keyOrModelChanged || form.voice_enabled !== baseline.voice_enabled || form.feedback_enabled !== baseline.feedback_enabled;
  const disablingBothSwitches = !form.voice_enabled && !form.feedback_enabled;
  const preserveLocalWithoutCloudKey =
    isCurrentLocal && !modelChanged && !typedKey && (localInstalled || disablingBothSwitches);
  const cloudInputErr = jevModelError(form.model) || (!keyReady && !typedKey ? "请填写 OpenRouter API Key" : "");
  const inputErr = preserveLocalWithoutCloudKey ? "" : cloudInputErr;
  const currentFp = jevFingerprint(form);
  const needsTest = preserveLocalWithoutCloudKey ? false : (!keyReady || keyOrModelChanged);
  const saveEnabled = !inputErr && (!needsTest || Boolean(test?.ok && test.fingerprint === currentFp));
  const sectionBusy = Boolean(busy || localBusy);
  const mergedOptions = (() => {
    const map = new Map();
    for (const item of JEV_PRESET_MODELS) map.set(item.id, item);
    for (const id of remoteModels.models) {
      if (!map.has(id)) map.set(id, { id, hint: "OpenRouter 在线模型" });
    }
    return [...map.values()];
  })();

  const update = (key, value) => {
    setForm((prev) => ({ ...prev, [key]: value }));
    setResult(null);
  };

  const fetchModels = async () => {
    setRemoteModels((prev) => ({ ...prev, loading: true }));
    try {
      const outcome = await voiceApi.builtinModelsList("jev", { api_key: form.api_key });
      setRemoteModels({
        loading: false,
        ok: Boolean(outcome.ok),
        models: Array.isArray(outcome.models) ? outcome.models : [],
        detail: outcome.detail || "",
      });
      setMenuOpen(true);
    } catch (error) {
      setRemoteModels({ loading: false, ok: false, models: [], detail: voiceErrorText(error) });
      setMenuOpen(true);
    }
  };

  const runTest = async () => {
    const err = cloudInputErr;
    if (err) {
      setTest({ ok: false, detail: err, fingerprint: "" });
      return;
    }
    const seq = ++opSeqRef.current;
    setLocalBusy("builtin-test-jev");
    setResult(null);
    const fp = jevFingerprint(form);
    try {
      const outcome = await voiceApi.builtinModelsTest("jev", { api_key: form.api_key, model: form.model });
      if (opSeqRef.current === seq) {
        setTest({ ...outcome, fingerprint: outcome.ok ? fp : "" });
      }
    } catch (error) {
      if (opSeqRef.current === seq) {
        setTest({ ok: false, detail: voiceErrorText(error), fingerprint: "" });
      }
    } finally {
      if (opSeqRef.current === seq) {
        setLocalBusy("");
      }
    }
  };

  const save = async () => {
    if (!saveEnabled) return;
    const seq = ++opSeqRef.current;
    setLocalBusy("builtin-save-jev");
    setResult(null);
    try {
      const next = await voiceApi.builtinModelsSave({
        jev: builtinJevSavePayload(status, form, baseline),
      });
      const nextBaseline = builtinJevForm(next);
      prevBaselineRef.current = nextBaseline;
      setStatus(next);
      setForm(nextBaseline);
      setTest(null);
      setResult({ ok: true, text: savedNoteText("Jev 决策模型", restartEffect(next)) });
    } catch (error) {
      setResult({ ok: false, text: `保存失败：${voiceErrorText(error)}` });
    } finally {
      if (opSeqRef.current === seq) {
        setLocalBusy("");
      }
    }
  };

  const clear = async () => {
    const ok = await showConfirm("将删除本机保存的 Jev OpenRouter API Key，并同时关闭语音 Jev 筛选与加工反馈判断。", {
      title: "删除 Jev 密钥与配置",
      danger: true,
      confirmText: "删除",
    });
    if (!ok) return;
    const seq = ++opSeqRef.current;
    setLocalBusy("builtin-clear-jev");
    setResult(null);
    try {
      const next = await voiceApi.builtinModelsSave({}, ["jev"]);
      const nextBaseline = builtinJevForm(next);
      prevBaselineRef.current = nextBaseline;
      setStatus(next);
      setForm(nextBaseline);
      setTest(null);
      setResult({ ok: true, text: `已删除 Jev 密钥与配置 · ${restartEffect(next)}` });
    } catch (error) {
      setResult({ ok: false, text: `删除失败：${voiceErrorText(error)}` });
    } finally {
      if (opSeqRef.current === seq) {
        setLocalBusy("");
      }
    }
  };

  return (
    <VoiceGroup id="builtin-model-jev" title="OpenRouter · Jev 前置粗筛模型（共享反馈密钥）" summary={builtinJevSummary(status)} dirty={dirty} tab="models">
      {localOn && (
        <p className="voice-muted">
          当前已在上方下拉框选用本地 Jev 决策（{sourceView.selectedLocalModel?.label || "Laya 多语言决策"}）；这里的 OpenRouter 云端配置与密钥作为备用保留，随时可在上方下拉切回。
        </p>
      )}
      <p className="voice-muted">
        在语音链路中位于「语音识别」之后、「大模型」之前：模型下拉选择作用于内置语音前置粗筛（半截话续听防抢话、口语快答分流、6 大工具组/外部 MCP 粗筛与记忆精筛）；OpenRouter 密钥同时可供雕刻加工反馈共用（加工反馈接口按契约固定使用 typesafe/jev-1.13），绝不授权任何设备动作。
      </p>
      <div className="voice-form-grid">
        <div className="voice-field">
          <div className="voice-field-head">
            <label htmlFor="voice-builtin-apikey-jev">OpenRouter API Key（不回显；留空则沿用已保存）</label>
          </div>
          <input
            className="text-input mono"
            type="password"
            autoComplete="off"
            value={form.api_key}
            placeholder={builtinJevKeyPlaceholder(status)}
            onChange={(e) => update("api_key", e.target.value)}
            id="voice-builtin-apikey-jev"
          />
        </div>
        <div className="voice-field" ref={comboRef}>
          <div className="voice-field-head">
            <label htmlFor="voice-builtin-model-jev">Jev 模型名称</label>
            <button
              type="button"
              className="voice-link-btn"
              onClick={fetchModels}
              disabled={sectionBusy || remoteModels.loading}
              id="voice-builtin-fetch-models-jev"
            >
              <RefreshCw size={11} className={remoteModels.loading ? "voice-spin" : ""} />
              {remoteModels.loading ? "读取中…" : "读取模型"}
            </button>
          </div>
          <div className="voice-combo-wrap">
            <input
              className="text-input mono voice-combo-input"
              value={form.model}
              placeholder="如 typesafe/jev-1.14"
              onChange={(e) => update("model", e.target.value)}
              onFocus={() => setMenuOpen(true)}
              id="voice-builtin-model-jev"
            />
            <button
              type="button"
              className="voice-combo-toggle"
              aria-label="展开可选 Jev 模型列表"
              aria-expanded={menuOpen}
              onClick={() => setMenuOpen((open) => !open)}
              id="voice-builtin-toggle-jev"
            >
              <ChevronDown size={14} />
            </button>
            {menuOpen && (
              <div className="voice-combo-menu" role="listbox" aria-label="Jev 决策可选模型" id="voice-builtin-model-options-jev">
                <div className="voice-combo-meta">
                  <span>{remoteModels.detail || `可选 Jev 模型（共 ${mergedOptions.length} 个，也可手填新版本）`}</span>
                </div>
                <div className="voice-combo-list">
                  {mergedOptions.map((opt) => (
                    <div
                      key={opt.id}
                      role="option"
                      aria-selected={opt.id === form.model}
                      className={`voice-combo-option mono${opt.id === form.model ? " is-selected" : ""}`}
                      onMouseDown={(e) => {
                        e.preventDefault();
                        update("model", opt.id);
                        setMenuOpen(false);
                      }}
                    >
                      <span className="voice-combo-id">{opt.id}</span>
                      {opt.hint && <span className="voice-combo-hint">{opt.hint}</span>}
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>
        </div>
        <label className="voice-check voice-wide">
          <input
            type="checkbox"
            checked={form.voice_enabled}
            onChange={(e) => update("voice_enabled", e.target.checked)}
            id="voice-builtin-jev-voice-enabled"
          />
          <span>启用内置语音 Jev 智能筛选与提速（也可在上方顶部下拉框直接一键切换；细项开关见“服务 → 语音体验”）</span>
        </label>
        <label className="voice-check voice-wide">
          <input
            type="checkbox"
            checked={form.feedback_enabled}
            onChange={(e) => update("feedback_enabled", e.target.checked)}
            id="voice-builtin-jev-feedback-enabled"
          />
          <span>同时启用雕刻任务加工反馈判断（共用此 OpenRouter 密钥，按契约使用 typesafe/jev-1.13 判断“太焦、太浅、切不透”等调参建议）</span>
        </label>
      </div>
      <div className="voice-final mono">最终请求地址：{DEFAULT_JEV_DECISIONS_URL}</div>
      {inputErr && dirty && <div className="voice-hint voice-hint--bad">{inputErr}</div>}
      {test ? (
        <div className={`voice-hint voice-hint--${test.ok ? "ok" : "bad"}`} id="voice-builtin-model-result-jev">
          <span>
            {test.ok ? "测试通过" : "测试未通过"}
            {typeof test.elapsed_ms === "number" ? ` · ${(test.elapsed_ms / 1000).toFixed(1)} s` : ""} · {test.detail}
            {test.ok && test.fingerprint !== currentFp && "（测试后密钥或模型已修改，请重新测试）"}
          </span>
        </div>
      ) : (
        <p className="voice-muted">
          {needsTest ? "首次配置或修改密钥/模型后请先测试连接，通过后才能保存。" : "本机已配置密钥；只改勾选开关可直接保存，也可随时测试连接。"}
        </p>
      )}
      <SaveBar
        id="voice-builtin-model-save-jev"
        target="Jev 决策模型设置"
        dirty={dirty}
        busy={sectionBusy}
        saving={localBusy === "builtin-save-jev"}
        blocked={saveEnabled ? "" : inputErr || "先测试连接，通过后才能保存"}
        result={result}
        onSave={save}
        extra={
          <>
            <button
              type="button"
              className="btn btn-ghost"
              onClick={runTest}
              disabled={sectionBusy || Boolean(inputErr)}
              id="voice-builtin-model-test-jev"
            >
              <FlaskConical size={13} /> {localBusy === "builtin-test-jev" ? "测试中…" : "测试连接"}
            </button>
            {Boolean(status?.jev?.key_set) && (
              <button type="button" className="btn btn-ghost" onClick={clear} disabled={sectionBusy} id="voice-builtin-model-clear-jev">
                <Trash2 size={13} /> 删除此项
              </button>
            )}
          </>
        }
      />
    </VoiceGroup>
  );
}

/** 内置服务顶部五栏自由搭配下拉选择栏 + 下方云端模型与 Jev 配置卡片。 */
export function BuiltinModelGroups({ builtin }) {
  const { status, setStatus, busy } = builtin;
  const [mixBusy, setMixBusy] = useState("");
  const [mixNote, setMixNote] = useState(null);
  if (!status || status.error) return <p className="voice-muted">{status?.error || "正在读取内置服务状态…"}</p>;
  const views = MIX_SECTIONS.map((item) => ({ ...item, view: builtinSectionSourceView(item.key, status) }));
  const focusConfigGroup = (groupId, scrollTargetId) => {
    const header = document.querySelector(`#voice-group-${groupId} > .voice-group-h`);
    if (header && header.getAttribute("aria-expanded") !== "true") {
      header.click();
    }
    const target = document.getElementById(scrollTargetId || `voice-group-${groupId}`);
    if (target && typeof target.scrollIntoView === "function") {
      target.scrollIntoView({ behavior: "smooth", block: "center" });
    }
  };

  const handleSelectSource = async (section, label, nextValue) => {
    if (section === "llm") {
      focusConfigGroup("builtin-model-llm", "voice-group-builtin-model-llm");
      return;
    }
    const currentView = builtinSectionSourceView(section, status);
    const pickedOption = currentView.options.find((opt) => opt.value === nextValue);
    if (section === "jev") {
      if (nextValue !== "off" && pickedOption && !pickedOption.installed) {
        if (pickedOption.isLocal) {
          focusConfigGroup("local-models", "voice-local-group-jev");
          setMixNote({
            ok: false,
            text: "本地 Laya 决策模型尚未下载：已为你展开下方“本地模型（断网可用）”，点击“下载”时将自动检测运行环境并开始下载。",
          });
        } else {
          focusConfigGroup("builtin-model-jev", "voice-group-builtin-model-jev");
          setMixNote({
            ok: false,
            text: "尚未配置 OpenRouter API Key：已为你展开下方“OpenRouter · Jev 前置粗筛与反馈模型”，填入密钥测试并保存后即可在此直接下拉切换。",
          });
        }
        return;
      }
      const payload = builtinSourceSwitchPatch(status, "jev", nextValue);
      if (!payload) return;
      setMixBusy("jev");
      builtin.setBusy("mix-switch-jev");
      setMixNote(null);
      try {
        const next = await voiceApi.localSave(payload);
        setStatus(next);
        setMixNote({
          ok: true,
          text:
            nextValue === "off"
              ? `已关闭 Jev 语音筛选（密钥与模型配置已保留） · ${restartEffect(next)}`
              : isLocalLayaJevModel(payload.jev_model)
                ? `已切换为本地 Laya 多语言决策（${payload.jev_model} · RTX 加速免密钥） · ${restartEffect(next)}`
                : `已切换为 Jev 决策（${payload.jev_model}） · ${restartEffect(next)}`,
        });
      } catch (error) {
        setMixNote({ ok: false, text: `切换失败：${voiceErrorText(error)}` });
      } finally {
        setMixBusy("");
        builtin.setBusy("");
      }
      return;
    }
    const isDefault = nextValue === "cloud" || nextValue === "energy";
    const cleanLabel = label.replace(/^\d+\.\s*/, "");
    if (!isDefault && pickedOption && !pickedOption.installed) {
      focusConfigGroup("local-models", `voice-local-group-${section}`);
      setMixNote({
        ok: false,
        text: `本地${cleanLabel}模型“${pickedOption.modelLabel || pickedOption.label}”尚未下载：已为你展开下方“本地模型（断网可用）”，点“下载”完成后即可在此下拉选用。`,
      });
      return;
    }
    const payload = builtinSourceSwitchPatch(status, section, nextValue);
    if (!payload) return;
    setMixBusy(section);
    builtin.setBusy(`mix-switch-${section}`);
    setMixNote(null);
    try {
      const next = await voiceApi.localSave(payload);
      setStatus(next);
      const nextView = builtinSectionSourceView(section, next);
      setMixNote({
        ok: true,
        text: isDefault
          ? section === "vad"
            ? `已切换为内置能量判停 · ${restartEffect(next)}`
            : `已切换为云端${cleanLabel} · ${restartEffect(next)}`
          : `已切换为本地${cleanLabel}（${nextView.selectedLocalModel?.label || "本地模型"}）${section === "vad" ? "" : "，云端配置与密钥已保留"} · ${restartEffect(next)}`,
      });
    } catch (error) {
      setMixNote({ ok: false, text: `切换失败：${voiceErrorText(error)}` });
    } finally {
      setMixBusy("");
      builtin.setBusy("");
    }
  };

  return (
    <>
      <div className="voice-mix-bar" id="voice-builtin-mix-bar">
        <div className="voice-mix-grid">
          {views.map(({ key, label, hint, view }) => (
            <label className="voice-mix-field" key={key} id={`voice-builtin-mix-${key}`} title={hint || ""}>
              <span className="voice-mix-field-head">
                <b>{label}</b>
                {(key === "llm" || key === "jev") && (
                  <button
                    type="button"
                    className="voice-link-btn"
                    onClick={() => focusConfigGroup(`builtin-model-${key}`, `voice-group-builtin-model-${key}`)}
                  >
                    去配置
                  </button>
                )}
              </span>
              <select
                className="text-input"
                value={view.selectValue}
                disabled={Boolean(busy || mixBusy)}
                onChange={(e) => handleSelectSource(key, label, e.target.value)}
                id={`voice-builtin-mix-select-${key}`}
                aria-label={`${label}当前使用模型`}
              >
                {view.options.map((opt) => (
                  <option key={opt.value} value={opt.value}>
                    {opt.label}
                  </option>
                ))}
              </select>
            </label>
          ))}
        </div>
        {mixNote && (
          <div className={`voice-hint voice-hint--${mixNote.ok ? "ok" : "warn"}`} role="status" id="voice-builtin-mix-status">
            {mixNote.text}
          </div>
        )}
        <span className="voice-muted">
          按真实语音链路从左到右排列：拾音判停 → 语音识别 → Jev 前置粗筛（识别后·进大模型前） → 大模型 → 语音合成；直接下拉选完立即生效，下方卡片用于配置对应项密钥、模型版本与下载本地模型。
        </span>
      </div>
      <BuiltinModelGroup section="asr" builtin={builtin} />
      <BuiltinJevGroup builtin={builtin} />
      <BuiltinModelGroup section="llm" builtin={builtin} />
      <BuiltinModelGroup section="tts" builtin={builtin} />
    </>
  );
}

/**
 * 本地模型与语音体验共用同一份已保存设置，各自只提交自己的字段，也只检查自己的字段：
 * 另一组已保存的值即使不合前端规则，也不挡本组保存（由后端最终校验）。
 */
function useLocalGroup(builtin, keys, validate) {
  const local = builtin.status?.local || {};
  const models = Array.isArray(local.models) ? local.models : [];
  const baseline = localSettingsForm(local.settings);
  const baselineSig = JSON.stringify(baseline);
  const [draft, setDraft] = useState(baseline);
  const prevBaselineRef = useRef(baseline);
  const [result, setResult] = useState(null);
  useEffect(() => {
    const prev = prevBaselineRef.current;
    setDraft((current) => syncLocalDraft(prev, baseline, current, keys));
    prevBaselineRef.current = baseline;
  }, [baselineSig]);
  const merged = mergeGroup(baseline, draft, keys);
  const dirty = formDirty(baseline, draft, keys);
  const error = validate(merged, models);
  const update = (key, value) => {
    setDraft((current) => ({ ...current, [key]: value }));
    setResult(null);
  };
  const save = async (target) => {
    builtin.setBusy(`local-save-${target}`);
    try {
      const next = await voiceApi.localSave(localGroupPatch(baseline, draft, keys));
      const nextForm = localSettingsForm(next.local?.settings);
      prevBaselineRef.current = nextForm;
      builtin.setStatus(next);
      setDraft(nextForm);
      setResult({ ok: true, text: savedNoteText(target, restartEffect(next)) });
    } catch (e) {
      setResult({ ok: false, text: `保存失败：${voiceErrorText(e)}` });
    } finally {
      builtin.setBusy("");
    }
  };
  return { local, models, baseline, draft, setDraft, update, dirty, error, result, setResult, save };
}

/** 本地模型（NV6）：判停、识别、合成各有几个可选模型，每类选一个、可分别下载。 */
export function LocalModelsGroup({ builtin }) {
  const { busy, act } = builtin;
  const [layaEnvHint, setLayaEnvHint] = useState(null);
  const group = useLocalGroup(builtin, LOCAL_MODEL_KEYS, localModelSettingsError);
  const { local, models, baseline, draft: form, setDraft, update, dirty, error, result, setResult } = group;
  const groups = localModelGroups(models);
  const voices = selectedVoices(form, models);
  const fallbackTtsModel = resolveFallbackTtsModel(form, models);
  const on = groups.filter((item) => baseline[item.kind]).map((item) => item.title);
  const remove = async (model) => {
    if (!(await showConfirm(`删除本地模型“${model.label}”？需要时可以重新下载。`, { title: "删除模型？", tone: "warning", danger: true, confirmText: "删除" }))) return;
    await act("删除模型", () => voiceApi.localDelete(model.id));
  };
  const pickFallbackTts = async (model) => {
    const nextDraft = selectLocalModel(form, models, "tts", model.id);
    setDraft(nextDraft);
    builtin.setBusy("local-save-云端兜底合成模型");
    try {
      const next = await voiceApi.localSave(localGroupPatch(baseline, nextDraft, LOCAL_MODEL_KEYS));
      const nextForm = localSettingsForm(next.local?.settings);
      builtin.setStatus(next);
      setDraft(nextForm);
      setResult({ ok: true, text: `已将“${model.label}”设为云端 TTS 异常时的优先兜底模型 · ${restartEffect(next)}` });
    } catch (e) {
      setResult({ ok: false, text: `设置兜底模型失败：${voiceErrorText(e)}` });
    } finally {
      builtin.setBusy("");
    }
  };

  return (
    <VoiceGroup id="local-models" title="本地模型（断网可用）" summary={on.length ? `已开启：${on.join("、")}` : "未开启"} dirty={dirty}>
      <div id="voice-local-models" className="voice-stack">
        {!local.helper_found && (
          <div className="voice-hint voice-hint--warn">没有找到本地模型程序 moss-voice-local（应与 MOSS 放在同一目录）。可以先下载模型，但打开本地开关后内置服务将无法启动。</div>
        )}
        <p className="voice-muted">
          在这里下载或管理断网可用的本地模型包（随 MOSS 自定义安装目录存放{local.models_dir ? `：${local.models_dir}` : ""}），以及调整本地合成音色与语速；当前启用哪个模型请直接在上方顶部下拉框切换。
        </p>
        {groups.map((item) => {
          const selectedId = form[`${item.kind}_model`];
          const activeOn = Boolean(form[item.kind]);
          const selected = item.models.find((model) => model.id === selectedId);
          return (
            <div className="voice-local-group" key={item.kind} id={`voice-local-group-${item.kind}`}>
              <div className="voice-field-head">
                <b>{item.title}模型包</b>
                <span className="voice-muted">
                  {activeOn
                    ? selected?.installed
                      ? `当前在顶部已选用：${selected.label}`
                      : `顶部已选 ${selected?.label || "本地模型"}（未下载，暂不生效）`
                    : item.kind === "vad"
                      ? "当前在顶部使用：内置能量判停"
                      : item.kind === "tts" && fallbackTtsModel
                        ? `当前在顶部使用：云端模型（云端异常优先兜底：${fallbackTtsModel.label}）`
                        : "当前在顶部使用：云端模型"}
                </span>
              </div>
              <div className="ob-tbl voice-tbl">
                <table>
                  <thead>
                    <tr>
                      <th>{item.title}模型</th>
                      <th>大小</th>
                      <th>状态</th>
                      <th>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {item.models.map((model) => {
                      const view = localModelView(model);
                      const chosen = activeOn && model.id === selectedId;
                      const isFallback = item.kind === "tts" && !activeOn && fallbackTtsModel?.id === model.id;
                      const canSetFallback = item.kind === "tts" && !activeOn && Boolean(model.installed) && fallbackTtsModel?.id !== model.id;
                      return (
                        <tr key={model.id} className={chosen ? "on" : ""}>
                          <td className="voice-model-cell">
                            <div className="voice-model-name">
                              <b>{model.label}</b>
                              {chosen && <span className="ob-pill ob-pill--ok voice-tag">当前选用</span>}
                              {isFallback && <span className="ob-pill ob-pill--info voice-tag">当前兜底</span>}
                              {model.streaming && <span className="ob-pill ob-pill--info voice-tag">边说边识别</span>}
                            </div>
                            <div className="ob-sub">{model.note}</div>
                          </td>
                          <td className="mono voice-size">{formatBytes(model.size)}</td>
                          <td className="voice-status">
                            <span className={`ob-pill ${TONE_CLASS[view.tone]}`}>{view.text}</span>
                            {view.detail && <div className="ob-sub voice-bad">{view.detail}</div>}
                            {view.percent !== null && (
                              <div className="voice-progress" aria-hidden="true">
                                <span style={{ width: `${view.percent}%` }} />
                              </div>
                            )}
                          </td>
                          <td>
                            <span className="voice-actions">
                              {canSetFallback && (
                                <button
                                  type="button"
                                  className="btn btn-ghost"
                                  disabled={Boolean(busy)}
                                  onClick={() => pickFallbackTts(model)}
                                  id={`voice-local-fallback-${model.id}`}
                                >
                                  设为云端兜底
                                </button>
                              )}
                              {view.canDownload && (
                                <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => act("下载模型", () => voiceApi.localDownload(model.id))} id={`voice-local-download-${model.id}`}>
                                  <Download size={13} /> 下载
                                </button>
                              )}
                              {view.canCancel && (
                                <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => act("取消下载", () => voiceApi.localCancel(model.id))}>
                                  <X size={13} /> 取消
                                </button>
                              )}
                              {view.canDelete && (
                                <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => remove(model)}>
                                  <Trash2 size={13} /> 删除
                                </button>
                              )}
                            </span>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            </div>
          );
        })}
        {models.filter((model) => model.kind === "jev").length > 0 && (
          <div className="voice-local-group" id="voice-local-group-jev">
            <div className="voice-field-head">
              <b>Jev 前置粗筛模型包（本地 System 1 决策）</b>
              <span className="voice-muted">
                {form.jev_filter_enabled && isLocalLayaJevModel(form.jev_model)
                  ? "当前在顶部已选用：本地 Laya 多语言决策"
                  : form.jev_filter_enabled
                    ? `当前在顶部使用：云端 Jev（${form.jev_model}）`
                    : "当前在顶部未启用（可在顶部第 3 栏下拉直接切换为本地 Laya）"}
              </span>
            </div>
            {layaEnvHint && (
              <div
                className={`voice-hint ${layaEnvHint.ready ? "voice-hint--ok" : "voice-hint--warn"}`}
                id="voice-local-laya-env-hint"
              >
                {layaEnvHint.ready ? `✓ ${layaEnvHint.summary}` : `⚠ ${layaEnvHint.summary}`}
              </div>
            )}
            <div className="ob-tbl voice-tbl">
              <table>
                <thead>
                  <tr>
                    <th>Jev 决策模型</th>
                    <th>大小</th>
                    <th>状态</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {models
                    .filter((model) => model.kind === "jev")
                    .map((model) => {
                      const view = localModelView(model);
                      const chosen = Boolean(form.jev_filter_enabled && isLocalLayaJevModel(form.jev_model) && model.id === "laya_multilingual");
                      const handleDownloadLaya = async () => {
                        builtin.setBusy("下载模型");
                        try {
                          const next = await voiceApi.localDownload(model.id);
                          if (next) builtin.setStatus(next);
                          if (next?.local?.laya_env) {
                            setLayaEnvHint(next.local.laya_env);
                          }
                        } catch (err) {
                          if (err?.code === "laya_env_missing") {
                            builtin.setBusy("");
                            setLayaEnvHint({ ready: false, summary: err.message });
                            const go = await showConfirm(
                              `${err.message}\n\n是否仍要先仅下载模型权重文件？`,
                              {
                                title: "未检测到本地 Laya 运行环境",
                                tone: "warning",
                                confirmText: "仍然下载",
                                cancelText: "取消",
                              }
                            );
                            if (!go) return;
                            await act("下载模型", () => voiceApi.localDownload(`${model.id}:force`));
                            return;
                          }
                          await showAlert(voiceErrorText(err), { title: "下载模型失败", tone: "error" });
                        } finally {
                          builtin.setBusy("");
                        }
                      };
                      return (
                        <tr key={model.id} className={chosen ? "on" : ""}>
                          <td className="voice-model-cell">
                            <div className="voice-model-name">
                              <b>{model.label}</b>
                              {chosen && <span className="ob-pill ob-pill--ok voice-tag">当前选用</span>}
                              <span className="ob-pill ob-pill--info voice-tag">免密钥 · 随安装目录存储</span>
                            </div>
                            <div className="ob-sub">{model.note}</div>
                          </td>
                          <td className="mono voice-size">{formatBytes(model.size)}</td>
                          <td className="voice-status">
                            <span className={`ob-pill ${TONE_CLASS[view.tone]}`}>{view.text}</span>
                            {view.detail && <div className="ob-sub voice-bad">{view.detail}</div>}
                            {view.percent !== null && (
                              <div className="voice-progress" aria-hidden="true">
                                <span style={{ width: `${view.percent}%` }} />
                              </div>
                            )}
                          </td>
                          <td>
                            <span className="voice-actions">
                              {view.canDownload && (
                                <button
                                  type="button"
                                  className="btn btn-ghost"
                                  disabled={Boolean(busy)}
                                  onClick={handleDownloadLaya}
                                  id={`voice-local-download-${model.id}`}
                                >
                                  <Download size={13} /> 下载
                                </button>
                              )}
                              {view.canCancel && (
                                <button
                                  type="button"
                                  className="btn btn-ghost"
                                  disabled={Boolean(busy)}
                                  onClick={() => act("取消下载", () => voiceApi.localCancel(model.id))}
                                >
                                  <X size={13} /> 取消
                                </button>
                              )}
                              {view.canDelete && (
                                <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => remove(model)}>
                                  <Trash2 size={13} /> 删除
                                </button>
                              )}
                            </span>
                          </td>
                        </tr>
                      );
                    })}
                </tbody>
              </table>
            </div>
          </div>
        )}
        {models.filter((model) => model.kind === "speaker").map((model) => {
          const view = localModelView(model);
          return (
            <div className="voice-local-group" key={model.id}>
              <b>{model.label}</b>
              <p className="voice-muted">
                {model.note} 下载不会开启声纹采集或身份识别。{formatBytes(model.size)} · {view.text}
                {view.detail ? ` · ${view.detail}` : ""}
              </p>
              <span className="voice-actions">
                {view.canDownload && <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => act("下载声纹模型", () => voiceApi.localDownload(model.id))}>下载声纹模型</button>}
                {view.canCancel && <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => act("取消声纹下载", () => voiceApi.localCancel(model.id))}>取消下载</button>}
                {view.canDelete && <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => remove(model)}>删除模型</button>}
              </span>
            </div>
          );
        })}
        <div className="voice-form-grid">
          {voices ? (
            <label>
              <span>本地合成音色编号（{voices.hint}；默认 {voices.default}）</span>
              <input className="text-input mono" inputMode="numeric" value={form.tts_speaker} onChange={(e) => update("tts_speaker", e.target.value)} id="voice-local-speaker" />
            </label>
          ) : (
            <label>
              <span>本地合成音色</span>
              <span className="voice-muted">所选合成模型只有一个音色</span>
            </label>
          )}
          <label>
            <span>本地合成语速（0.5 ~ 2.0）</span>
            <input className="text-input" inputMode="decimal" value={form.tts_speed} onChange={(e) => update("tts_speed", e.target.value)} />
          </label>
          <label className="voice-wide">
            <span>模型下载地址（留空为官方地址；可填镜像，下载后一律校验 SHA-256）</span>
            <input className="text-input mono" value={form.mirror} placeholder={local.default_mirror || ""} onChange={(e) => update("mirror", e.target.value)} spellCheck="false" autoComplete="off" />
          </label>
        </div>
        {dirty && error && <div className="voice-hint voice-hint--bad">{error}</div>}
        <SaveBar id="voice-local-save" target="本地模型设置" dirty={dirty} busy={Boolean(busy)} saving={busy === "local-save-本地模型设置"} blocked={error ? "请先改正上面的错误" : ""} result={result} onSave={() => group.save("本地模型设置")} />
      </div>
    </VoiceGroup>
  );
}

/** 语音体验：快捷查询、热词、自然插话。 */
export function ExperienceGroup({ builtin }) {
  const group = useLocalGroup(builtin, EXPERIENCE_KEYS, experienceSettingsError);
  const { baseline, draft: form, update, dirty, error, result } = group;
  const busy = builtin.busy;
  const selectedJevModel = form.jev_model || baseline.jev_model || DEFAULT_JEV_MODEL;
  const isLocalJev = isLocalLayaJevModel(selectedJevModel);
  const localLayaInstalled = Boolean(
    builtin.status?.jev?.local_installed ||
      (Array.isArray(builtin.status?.local?.models) &&
        builtin.status.local.models.find((m) => m && m.id === "laya_multilingual")?.installed)
  );
  const jevKeyConfigured = isLocalJev
    ? localLayaInstalled
    : Boolean(builtin.status?.jev?.key_set || builtin.status?.jev?.openrouter_llm_key_set || builtin.status?.local?.jev_key_configured);
  const jevActive = Boolean(baseline.jev_filter_enabled && (baseline.jev_unfinished_hold || (baseline.read_only_shortcuts && baseline.jev_fast_route) || baseline.jev_tool_prune || baseline.jev_memory_filter));
  const styleLabel = PERSONA_STYLE_OPTIONS.find((opt) => opt.id === (baseline.persona_style || "workshop"))?.label?.replace(/（.*）$/, "") || "简明工坊助手";
  const corrCount = String(baseline.custom_asr_corrections || "").split(/\r?\n/).filter((w) => w.trim()).length;
  const summary = [
    styleLabel,
    baseline.read_only_shortcuts ? "快捷查询开" : "快捷查询关",
    baseline.asr_homophone_fix !== false ? `同音纠偏开${corrCount ? `(+${corrCount})` : ""}` : "同音纠偏关",
    baseline.screen_preview_sync !== false ? "小智屏预览同步开" : "小智屏预览同步关",
    baseline.screen_card_style === "goggle_bot" ? "可爱小机器人画风" : "硬核太空舱画风",
    jevActive ? "Jev 筛选开" : "Jev 筛选关",
    baseline.barge_in ? "插话开" : "插话关",
  ].join(" · ");
  return (
    <VoiceGroup id="experience" title="语音体验" summary={summary} dirty={dirty}>
      <p className="voice-muted">停止词整句命中时立即请求急停，不能关闭；设备是否停下仍需确认。</p>
      <div className="voice-form-grid">
        <label className="voice-wide">
          <span>助手语气风格（底层动机器复述确认与急停铁律始终锁定不可覆盖；每轮自动注入本机日期星期时间，并抑制同一会话重复机械喊称呼）</span>
          <select
            className="select-input"
            value={form.persona_style || "workshop"}
            onChange={(e) => update("persona_style", e.target.value)}
            id="voice-persona-style"
          >
            {PERSONA_STYLE_OPTIONS.map((opt) => (
              <option key={opt.id} value={opt.id}>
                {opt.label} — {opt.hint}
              </option>
            ))}
          </select>
        </label>
        <label className="voice-wide">
          <span>工坊补充背景说明（可选，最多 300 字；如常用机型、排风习惯或偏好的说明方式）</span>
          <input
            className="text-input"
            type="text"
            maxLength={300}
            placeholder="例如：工坊主力机是翼宿 5W 蓝光雕刻机，平时主要做木质书签与亚克力铭牌"
            value={form.persona_prompt || ""}
            onChange={(e) => update("persona_prompt", e.target.value)}
            id="voice-persona-prompt"
          />
        </label>
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={form.read_only_shortcuts} onChange={(e) => update("read_only_shortcuts", e.target.checked)} id="voice-read-shortcuts" />
          <span>快捷查询状态、连接、进度、剩余时间与材料（不经大模型，默认开）</span>
        </label>
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={form.job_done_announce} onChange={(e) => update("job_done_announce", e.target.checked)} id="voice-job-announce" />
          <span title="只在设备在线且当前不在说话时播报；播报中被打断按插话机制处理。">
            加工结束后主动播报结果（默认开；设备在线且空闲时才播）
          </span>
        </label>
        <label className="voice-check voice-wide">
          <input
            type="checkbox"
            checked={form.screen_preview_sync !== false}
            onChange={(e) => update("screen_preview_sync", e.target.checked)}
            id="voice-screen-preview-sync"
          />
          <span>生成激光预览及加工发送期间，自动同步到在线小智屏幕（彩屏 1:1 点对点显示加粗刀路预览与太空舱实时进度副屏 HUD，单色 OLED 显示滚动参数卡；日常聊天不触发，默认开）</span>
        </label>
        <label className="voice-wide">
          <span>小智彩屏卡片视觉画风（不刷机、不擦写设备 Flash；预览、实时进度、待电脑审批、5 秒完工庆祝与本地音乐卡同步切换）</span>
          <select
            className="select-input"
            value={form.screen_card_style || "moss_hud"}
            onChange={(e) => update("screen_card_style", e.target.value)}
            id="voice-screen-card-style"
          >
            {SCREEN_CARD_STYLE_OPTIONS.map((opt) => (
              <option key={opt.id} value={opt.id}>
                {opt.label} — {opt.hint}
              </option>
            ))}
          </select>
        </label>
        <label className="voice-check voice-wide">
          <input
            type="checkbox"
            checked={form.asr_homophone_fix !== false}
            onChange={(e) => update("asr_homophone_fix", e.target.checked)}
            id="voice-asr-homophone-fix"
          />
          <span>
            激光行业同音错词 0ms 自动纠偏（默认开；云端与本地识别均 100% 生效：{BUILTIN_ASR_CORRECTION_PREVIEW}）
          </span>
        </label>
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={form.asr_beam_search} onChange={(e) => update("asr_beam_search", e.target.checked)} id="voice-asr-beam" />
          <span>X-ASR 热词识别（默认开；识别较慢时可关闭）</span>
        </label>
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={form.jev_filter_enabled} onChange={(e) => update("jev_filter_enabled", e.target.checked)} id="voice-jev-filter-enabled" />
          <span title={`使用 ${isLocalJev ? "本地 Laya (laya_multilingual)" : `OpenRouter Decisions（${selectedJevModel}）`} 在单次请求中并发执行断句、快答分流与上下文/工具剪枝；只做信息筛选，绝不授权任何机器动作。`}>
            Jev 智能信息筛选与提速（默认关；{isLocalJev ? `当前选用本地 ${selectedJevModel}` : `当前选用 ${selectedJevModel}`}，只做信息筛选不授权动作）
          </span>
        </label>
        {form.jev_filter_enabled && (
          <div className="voice-wide voice-stack" id="voice-jev-suboptions">
            <div className={`voice-hint ${jevKeyConfigured ? "" : "voice-hint--bad"}`}>
              {isLocalJev
                ? jevKeyConfigured
                  ? "已就绪本地 Laya 多语言决策模型（免云端密钥，识别文字不出本机）；超时或异常时自动无感回退常规链路。"
                  : "本地 Laya 决策模型尚未下载：请前往“本地模型（断网可用）”下载 laya_multilingual，未就绪时自动回退常规对话链路。"
                : jevKeyConfigured
                  ? `已就绪 OpenRouter API Key（当前语音粗筛模型 ${selectedJevModel}）；单次请求并发筛选，超时或限流时自动无感回退常规链路。`
                  : `尚未配置 OpenRouter API Key：请前往“模型与服务 → OpenRouter · Jev 前置粗筛模型”保存密钥；未配置时自动回退常规对话链路。`}
            </div>
            <label className="voice-check voice-wide">
              <input type="checkbox" checked={form.jev_unfinished_hold} onChange={(e) => update("jev_unfinished_hold", e.target.checked)} id="voice-jev-unfinished" />
              <span>半截话防抢话续听与流式预热（停顿想词时单次延后最多 1.2 秒，并提前缓存本句路由）</span>
            </label>
            <label className="voice-check voice-wide">
              <input type="checkbox" checked={form.read_only_shortcuts && form.jev_fast_route} disabled={!form.read_only_shortcuts} onChange={(e) => update("jev_fast_route", e.target.checked)} id="voice-jev-fast-route" />
              <span>口语状态快答分流（口语询问状态、连接、进度、剩余时间与材料时跳过大模型直答{form.read_only_shortcuts ? "" : "；需先开启上方快捷查询"}）</span>
            </label>
            <label className="voice-check voice-wide">
              <input type="checkbox" checked={form.jev_tool_prune} onChange={(e) => update("jev_tool_prune", e.target.checked)} id="voice-jev-tool-prune" />
              <span title="内置与扩展工具收拢为 6 个粗筛开关（支持增删工具自动适配），第三方外部 MCP 工具较多时自动按服务分组粗筛，避免逐个工具平铺累积误报；state 单条不超过 600 字符、只带最近 2 轮极简上下文。">6 大工具组按需剪枝（日常闲聊 0 工具进主模型，业务指令仅挂载相关工具组以缩短首字延迟）</span>
            </label>
            <label className="voice-check voice-wide">
              <input type="checkbox" checked={form.jev_memory_filter} onChange={(e) => update("jev_memory_filter", e.target.checked)} id="voice-jev-memory-filter" />
              <span>长期记忆相关性精筛（顺手剔除字面撞词但语义无关的候选记忆）</span>
            </label>
          </div>
        )}
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={form.barge_in} onChange={(e) => update("barge_in", e.target.checked)} id="voice-barge-in" />
          <span title="须先在设备固件开启设备端回声消除并使用 realtime 模式；未消除的回声可能误打断。MOSS 不修改固件。">
            播报时可直接插话（默认关；需设备开启回声消除与实时模式）
          </span>
        </label>
        <label>
          <span>连续插话时长（300 ~ 600 毫秒）</span>
          <input className="text-input" type="number" min="300" max="600" step="20" value={form.barge_in_ms} onChange={(e) => update("barge_in_ms", e.target.value)} id="voice-barge-in-ms" />
        </label>
        <label className="voice-wide">
          <span>
            自定义热词（每行一条，最多 64 条；自动合并材料名称和行业词。
            {baseline.asr && baseline.asr_model.startsWith("sense_voice") ? "SenseVoice 不支持热词" : "X-ASR 暂不支持英文热词"}）
          </span>
          <textarea className="text-input" rows={4} value={form.custom_hotwords} onChange={(e) => update("custom_hotwords", e.target.value)} id="voice-hotwords" />
        </label>
        <label className="voice-wide">
          <span>自定义同音纠偏词表（每行一条“错词=正词”，最多 64 条；不管识别模型是否支持热词均 0ms 生效，急停等安全词受保护不可改写）</span>
          <textarea
            className="text-input"
            rows={3}
            placeholder={"例如：\n红松木=樟子松\n大鱼一宿=大鱼翼宿"}
            value={form.custom_asr_corrections || ""}
            onChange={(e) => update("custom_asr_corrections", e.target.value)}
            id="voice-asr-corrections"
          />
        </label>
      </div>
      {dirty && error && <div className="voice-hint voice-hint--bad">{error}</div>}
      <SaveBar id="voice-experience-save" target="语音体验设置" dirty={dirty} busy={Boolean(busy)} saving={busy === "local-save-语音体验设置"} blocked={error ? "请先改正上面的错误" : ""} result={result} onSave={() => group.save("语音体验设置")} />
    </VoiceGroup>
  );
}

/** 最近 20 轮对话各环节耗时（只在内存）。 */
/** 默认关闭、仅存本机内存（关闭或重启即清）的实时对话识别与工具调用排障面板，支持划词一键加入纠错或热词。 */
export function LiveDebugTraces({ builtin }) {
  const status = builtin?.status;
  const enabled = Boolean(status?.debug_enabled);
  const traces = Array.isArray(status?.debug_traces) ? status.debug_traces : [];
  const timingItems = voiceTimingReport(status?.timings);
  const events = Array.isArray(status?.events) ? status.events.slice(0, 20) : [];
  const [selectedWord, setSelectedWord] = useState("");
  const [rightWord, setRightWord] = useState("");
  const [feedback, setFeedback] = useState("");
  const [saving, setSaving] = useState(false);

  const copyTimings = async () => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(timingItems, null, 2));
      await showAlert("耗时数据已复制", { title: "已复制", tone: "success" });
    } catch {
      await showAlert("无法访问剪贴板，请检查浏览器权限", { title: "复制失败", tone: "error" });
    }
  };

  const summaryParts = [
    enabled ? `排障已开 (${traces.length} 轮)` : "排障默认关",
    timingItems.length ? `耗时 ${timingItems.length} 轮` : "",
    events.length ? `事件 ${events.length} 条` : "",
  ].filter(Boolean);

  const toggleDebug = async (checked) => {
    setSaving(true);
    setFeedback("");
    try {
      const next = await voiceApi.localSave({ debug_trace_enabled: checked });
      if (next && builtin?.setStatus) builtin.setStatus(next);
      if (!checked) {
        setSelectedWord("");
        setRightWord("");
      }
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "切换调试流失败", tone: "error" });
    } finally {
      setSaving(false);
    }
  };

  const clearDebug = async () => {
    setSaving(true);
    try {
      const next = await voiceApi.localSave({ clear_debug_traces: true });
      if (next && builtin?.setStatus) builtin.setStatus(next);
      setSelectedWord("");
      setRightWord("");
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "清空失败", tone: "error" });
    } finally {
      setSaving(false);
    }
  };

  const captureSelection = () => {
    const raw = String(globalThis.getSelection?.()?.toString?.() || "").trim();
    if (raw && [...raw].length <= 24 && !raw.includes("\n")) {
      setSelectedWord(raw);
      setFeedback("");
    }
  };

  const addCorrection = async () => {
    const wrong = selectedWord.trim();
    const right = rightWord.trim();
    if (!wrong || !right) return;
    setSaving(true);
    setFeedback("");
    try {
      const current = Array.isArray(status?.local?.settings?.custom_asr_corrections)
        ? status.local.settings.custom_asr_corrections
        : [];
      const nextRules = [...current.filter((item) => !String(item).startsWith(`${wrong}=`)), `${wrong}=${right}`];
      const next = await voiceApi.localSave({ custom_asr_corrections: nextRules });
      if (next && builtin?.setStatus) builtin.setStatus(next);
      setFeedback(`已添加同音纠偏：${wrong} → ${right}`);
      setSelectedWord("");
      setRightWord("");
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "添加同音纠偏失败", tone: "error" });
    } finally {
      setSaving(false);
    }
  };

  const addHotword = async (wordRaw) => {
    const word = String(wordRaw || selectedWord).trim();
    if (!word) return;
    setSaving(true);
    setFeedback("");
    try {
      const current = Array.isArray(status?.local?.settings?.custom_hotwords)
        ? status.local.settings.custom_hotwords
        : [];
      const nextWords = current.includes(word) ? current : [...current, word];
      const next = await voiceApi.localSave({ custom_hotwords: nextWords });
      if (next && builtin?.setStatus) builtin.setStatus(next);
      setFeedback(`已加入自定义热词：${word}`);
      setSelectedWord("");
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "添加热词失败", tone: "error" });
    } finally {
      setSaving(false);
    }
  };

  return (
    <VoiceGroup
      id="live-debug-traces"
      title="对话诊断与服务日志（仅内存）"
      summary={summaryParts.join(" · ")}
    >
      <div className="voice-stack" id="voice-live-debug">
        <div className="voice-actions">
          <label className="voice-check">
            <input
              type="checkbox"
              checked={enabled}
              disabled={saving}
              onChange={(e) => toggleDebug(e.target.checked)}
              id="voice-debug-trace-switch"
            />
            <span>开启实时识别与工具调用调试（仅存本机内存最多 10 轮，关闭开关或退出 MOSS 立即清空，不断连设备）</span>
          </label>
          {enabled && traces.length > 0 && (
            <button
              type="button"
              className="btn btn-ghost"
              disabled={saving}
              onClick={clearDebug}
              id="voice-debug-trace-clear"
            >
              清空调试记录
            </button>
          )}
          {timingItems.length > 0 && (
            <button
              type="button"
              className="btn btn-ghost"
              onClick={copyTimings}
              id="voice-timings-copy"
            >
              复制耗时数据（{timingItems.length} 轮）
            </button>
          )}
        </div>
        {feedback && <div className="voice-ok">{feedback}</div>}
        {enabled && selectedWord && (
          <div className="voice-debug-selection-bar" id="voice-debug-selection-bar">
            <span>已选中识别词：<b className="mono">{selectedWord}</b></span>
            <input
              className="text-input"
              type="text"
              maxLength={24}
              placeholder="输入正确词（如 椴木）"
              value={rightWord}
              onChange={(e) => setRightWord(e.target.value)}
              id="voice-debug-right-word"
            />
            <button
              type="button"
              className="btn btn-primary"
              disabled={saving || !rightWord.trim()}
              onClick={addCorrection}
              id="voice-debug-add-correction"
            >
              加入同音纠偏
            </button>
            <button
              type="button"
              className="btn btn-ghost"
              disabled={saving}
              onClick={() => addHotword(selectedWord)}
              id="voice-debug-add-hotword"
            >
              直接加为热词
            </button>
            <button type="button" className="btn btn-ghost" onClick={() => setSelectedWord("")}>
              取消
            </button>
          </div>
        )}
        {!enabled ? (
          <p className="voice-muted">当前处于隐私静默状态，不留存任何语音识别文字。遇到“小智听错耗材名”或“不确定调了什么工具参数”时打开上方开关即可实时查看。</p>
        ) : traces.length === 0 ? (
          <p className="voice-muted">已开启内存调试——请唤醒小智说一句话，识别原话、0ms 纠偏结果与工具参数会实时显示在这里（鼠标划选听错的词可一键加入纠偏表）。</p>
        ) : (
          traces.map((trace, idx) => {
            const correctedChanged = trace.corrected_asr && trace.corrected_asr !== trace.raw_asr;
            return (
              <div className="voice-debug-card" key={`${trace.session_id}-${trace.turn}-${idx}`}>
                <div className="voice-debug-head">
                  <b>第 {trace.turn} 轮</b>
                  <span className="voice-muted mono">{formatTime(trace.at)}</span>
                  <span className="ob-pill ob-pill--info">{trace.outcome === "done" ? "完成" : trace.outcome}</span>
                </div>
                <div className="voice-debug-line" onMouseUp={captureSelection}>
                  <span className="voice-muted">听到原话（划选错词可纠偏）：</span>
                  <span className="voice-debug-asr">{trace.raw_asr || "—"}</span>
                  {trace.raw_asr && (
                    <button
                      type="button"
                      className="btn btn-ghost voice-inline-btn"
                      onClick={() => {
                        setSelectedWord(trace.raw_asr.slice(0, 24));
                        setFeedback("");
                      }}
                    >
                      纠偏此句
                    </button>
                  )}
                </div>
                {correctedChanged && (
                  <div className="voice-debug-line">
                    <span className="voice-ok">0ms 纠偏后：</span>
                    <b>{trace.corrected_asr}</b>
                  </div>
                )}
                {Array.isArray(trace.tools) && trace.tools.length > 0 && (
                  <div className="voice-debug-tools">
                    {trace.tools.map((t, ti) => (
                      <details key={ti}>
                        <summary className="mono">工具调用：{t.name}</summary>
                        <div className="voice-muted mono">参数：{t.arguments || "{}"}</div>
                        <div className="voice-muted mono">结果：{t.result || "—"}</div>
                      </details>
                    ))}
                  </div>
                )}
                {trace.reply && (
                  <div className="voice-debug-line">
                    <span className="voice-muted">播报回复：</span>
                    <span>{trace.reply}</span>
                  </div>
                )}
              </div>
            );
          })
        )}
        {timingItems.length > 0 && (
          <div id="voice-turn-timings" className="voice-stack">
            <span className="voice-muted">最近对话各阶段耗时（最近 {timingItems.length} 轮，从本轮开始拾音算起）：</span>
            {timingItems.map((item, index) => (
              <details key={index} open={index === 0}>
                <summary>
                  第 {item.turn} 轮 · {item.outcome === "done" ? "结束" : item.outcome === "interrupted" ? "被打断" : "未完成"} · {item.models.join(" / ")}
                </summary>
                <div className="voice-table-wrap">
                  <table className="voice-table">
                    <thead>
                      <tr>
                        <th>阶段</th>
                        <th>毫秒</th>
                      </tr>
                    </thead>
                    <tbody>
                      {Object.entries(item.stages).map(([stage, ms]) => (
                        <tr key={stage}>
                          <td>{VOICE_TIMING_STAGES[stage]}</td>
                          <td>{ms}</td>
                        </tr>
                      ))}
                      {item.tools.map((tool, i) => (
                        <tr key={`tool-${i}`}>
                          <td>工具 {tool.name}</td>
                          <td>{tool.ms}（调用耗时）</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </details>
            ))}
          </div>
        )}
        {events.length > 0 && (
          <div className="voice-stack">
            <span className="voice-muted">最近服务事件（{events.length} 条）：</span>
            <pre className="voice-log mono" id="voice-builtin-events">
              {events.map((event) => `${formatTime(event.at)}  ${event.text}`).join("\n")}
            </pre>
          </div>
        )}
      </div>
    </VoiceGroup>
  );
}

export function TurnTimings({ timings }) {
  const items = voiceTimingReport(timings);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(items, null, 2));
      await showAlert("耗时数据已复制", { title: "已复制", tone: "success" });
    } catch {
      await showAlert("无法访问剪贴板，请检查浏览器权限", { title: "复制失败", tone: "error" });
    }
  };
  return (
    <VoiceGroup id="turn-timings" title="最近对话耗时" summary={items.length ? `${items.length} 轮` : "暂无"}>
      <div id="voice-turn-timings" className="voice-stack">
        <p className="voice-muted">最近 20 轮，只在内存、重启清空，不含对话内容。时间从本轮开始拾音算起。</p>
        <span className="voice-actions">
          <button type="button" className="btn btn-ghost" onClick={copy} disabled={!items.length} id="voice-timings-copy">复制耗时数据</button>
        </span>
        {items.map((item, index) => (
          <details key={index} open={index === 0}>
            <summary>
              第 {item.turn} 轮 · {item.outcome === "done" ? "结束" : item.outcome === "interrupted" ? "被打断" : "未完成"} · {item.models.join(" / ")}
            </summary>
            <div className="voice-table-wrap">
              <table className="voice-table">
                <thead>
                  <tr>
                    <th>阶段</th>
                    <th>毫秒</th>
                  </tr>
                </thead>
                <tbody>
                  {Object.entries(item.stages).map(([stage, ms]) => (
                    <tr key={stage}>
                      <td>{VOICE_TIMING_STAGES[stage]}</td>
                      <td>{ms}</td>
                    </tr>
                  ))}
                  {item.tools.map((tool, i) => (
                    <tr key={`tool-${i}`}>
                      <td>工具 {tool.name}</td>
                      <td>{tool.ms}（调用耗时）</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </details>
        ))}
      </div>
    </VoiceGroup>
  );
}

/** 内置服务最近 20 条事件。 */
export function BuiltinEvents({ status }) {
  const events = Array.isArray(status?.events) ? status.events.slice(0, 20) : [];
  return (
    <VoiceGroup id="builtin-events" title="内置服务事件" summary={events.length ? `最近 ${events.length} 条` : "暂无"}>
      <pre className="voice-log mono" id="voice-builtin-events">
        {events.length ? events.map((event) => `${formatTime(event.at)}  ${event.text}`).join("\n") : "暂无事件"}
      </pre>
    </VoiceGroup>
  );
}

/** 主窗口只提供任务入口；私人数据与授权仍由打包的本机窗口处理。 */
async function openPrivateTask(section) {
  try {
    await voiceApi.openPrivateSettings(section);
  } catch (error) {
    await showAlert(voiceErrorText(error), { title: "无法打开本机设置", tone: "error" });
  }
}

const PRIVATE_TASKS = [
  { key: "memory", title: "记忆", description: "查看和管理小智记住的内容" },
  { key: "plugins", title: "信息服务", description: "选择天气、新闻和搜索等服务" },
  { key: "speaker", title: "声纹", description: "登记家人，完成标定后再启用" },
  { key: "home", title: "家居", description: "连接设备；每次控制仍需批准" },
];

export function PrivateSettingsEntry({ status }) {
  const [opened, setOpened] = useState(() => new Set());
  const [advanced, setAdvanced] = useState(() => new Set());
  const expand = (key) => setOpened((old) => (old.has(key) ? old : new Set([...old, key])));
  const expandAdvanced = (key) => setAdvanced((old) => (old.has(key) ? old : new Set([...old, key])));
  return (
    <section className="voice-private-entry" aria-label="隐私与家居">
      <div className="voice-private-entry-heading">
        <Lock size={17} aria-hidden="true" />
        <div><h3>你想让小智做什么？</h3><p>点开哪一项就设置哪一项，不用一次看完所有选项。</p></div>
      </div>
      <div className="voice-private-tasks">
        {PRIVATE_TASKS.map(({ key, title, description }) => (
          <details key={key} className="voice-private-item" id={`voice-private-${key}-item`}
            onToggle={(event) => { if (event.currentTarget.open) expand(key); }}>
            <summary>
              <ChevronDown className="voice-private-item-arrow" size={15} aria-hidden="true" />
              <strong>{title}</strong>
              <small>{description}</small>
            </summary>
            <div className="voice-private-item-body">
              {key === "memory" && opened.has("memory") && <VoiceMemoryCard status={status} />}
              {key === "plugins" && opened.has("plugins") && <VoicePluginsCard />}
              {key === "speaker" && opened.has("speaker") && <VoiceSpeakerCard />}
              {key === "home" && opened.has("home") && (
                <>
                  <VoiceHomeCard />
                  <details className="voice-private-more" id="voice-private-home-approvals"
                    onToggle={(event) => { if (event.currentTarget.open) expandAdvanced("home"); }}>
                    <summary>家居审批</summary>
                    {advanced.has("home") && <VoiceHomeApprovalCard />}
                  </details>
                </>
              )}
            </div>
          </details>
        ))}
      </div>
      <p className="voice-muted">内容只保存在本机，不上传；家居每次动作仍需你在电脑上批准，语音报告只作参考。</p>
    </section>
  );
}

export function PrivateFirmwareEntry() {
  return (
    <div className="voice-private-maintenance">
      <span>固件升级仅在本机逐台操作。</span>
      <button type="button" className="btn btn-ghost" id="voice-private-settings-open"
        onClick={() => openPrivateTask("firmware")}>管理固件…</button>
    </div>
  );
}
