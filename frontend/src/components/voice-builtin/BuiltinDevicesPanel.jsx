import React, { useState } from "react";
import { ShieldCheck, ShieldOff, Trash2, Pencil, Check, X } from "lucide-react";
import { showAlert, showConfirm } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { VoiceGroup } from "../VoiceLayout.jsx";
import {
  AUTO_ALLOW_DEVICES_CONFIRM_OPTIONS,
  AUTO_ALLOW_DEVICES_CONFIRM_TEXT,
  allowDeviceConfirmText,
  builtinDeviceControlsView,
  builtinDeviceInfoView,
  builtinDeviceView,
  debugInjectStateView,
  partitionBuiltinDevices,
  removeDeviceConfirmText,
  voiceErrorText,
} from "../../voiceState.js";
import { formatTime, TONE_CLASS } from "./shared.js";

/** 设备：允许 / 撤销 / 移除 / 改名；展开后是在线快捷控制（音量、亮度、深浅屏、投送预览、按键拾音）与设备信息。n69 起放在“设备”标签。 */
export function BuiltinDevicesPanel({ builtin }) {
  const { status, busy, act } = builtin;
  const [editing, setEditing] = useState({ id: "", name: "" });
  const [expanded, setExpanded] = useState("");
  const [asking, setAsking] = useState("");
  const [controlDrafts, setControlDrafts] = useState({});
  const [controlNotice, setControlNotice] = useState({});
  const [injectDrafts, setInjectDrafts] = useState({});
  const [injectNotice, setInjectNotice] = useState({});
  if (!status) return <p className="voice-muted">正在读取内置服务状态…</p>;
  if (status.error) return <div className="voice-hint voice-hint--bad">{status.error}</div>;

  const devices = Array.isArray(status.devices) ? status.devices : [];
  const { activeDevices, revokedDevices } = partitionBuiltinDevices(devices);
  const sessions = Array.isArray(status.sessions) ? status.sessions : [];
  const autoAllow = status.local?.settings?.auto_allow_devices === true;
  const debugInject = debugInjectStateView(status);
  const pending = activeDevices.filter((device) => builtinDeviceView(device).status === "pending").length;
  const summary = devices.length
    ? [
        `${activeDevices.length} 台`,
        `${sessions.length} 台在线`,
        pending ? `${pending} 台待允许` : "",
        revokedDevices.length ? `${revokedDevices.length} 台已撤销` : "",
      ]
        .filter(Boolean)
        .join(" · ")
    : "还没有设备连接过";

  const toggleAutoAllow = async (on) => {
    if (on) {
      const ok = await showConfirm(
        AUTO_ALLOW_DEVICES_CONFIRM_TEXT,
        AUTO_ALLOW_DEVICES_CONFIRM_OPTIONS,
      );
      if (!ok) return;
    }
    await act(on ? "开启自动允许新设备" : "关闭自动允许新设备", () =>
      voiceApi.localSave({ auto_allow_devices: on }),
    );
  };

  const allow = async (device) => {
    if (!(await showConfirm(allowDeviceConfirmText(device), { title: "允许此设备？", tone: "warning", confirmText: "允许" }))) return;
    await act("允许设备", () => voiceApi.deviceSetStatus(device.device_id, "allowed"));
  };
  const revoke = async (device) => {
    if (!(await showConfirm(`撤销后，设备“${builtinDeviceView(device).title}”将无法再连接内置语音服务，直到重新允许。`, { title: "撤销此设备？", tone: "warning", danger: true, confirmText: "撤销" }))) return;
    await act("撤销设备", () => voiceApi.deviceSetStatus(device.device_id, "revoked"));
  };
  const remove = async (device) => {
    if (
      !(await showConfirm(removeDeviceConfirmText(device, autoAllow), {
        title: "移除此设备？",
        tone: "warning",
        danger: true,
        confirmText: "移除",
      }))
    ) {
      return;
    }
    const id = device.device_id;
    if (expanded === id) setExpanded("");
    if (editing.id === id) setEditing({ id: "", name: "" });
    setControlDrafts((prev) => {
      if (!(id in prev)) return prev;
      const next = { ...prev };
      delete next[id];
      return next;
    });
    setControlNotice((prev) => {
      if (!(id in prev)) return prev;
      const next = { ...prev };
      delete next[id];
      return next;
    });
    await act("移除设备", () => voiceApi.deviceRemove(id));
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
  const sendDebugInjectText = async (device) => {
    const text = String(injectDrafts[device.device_id] || "").trim();
    if (!text) return;
    setAsking(device.device_id);
    setInjectNotice((prev) => ({ ...prev, [device.device_id]: "" }));
    try {
      const ack = await voiceApi.deviceRequest(device.device_id, "inject_text", { text });
      setInjectDrafts((prev) => ({ ...prev, [device.device_id]: "" }));
      const turnText = ack?.turn ? `（模拟第 ${ack.turn} 轮）` : "（模拟轮次）";
      setInjectNotice((prev) => ({ ...prev, [device.device_id]: `已注入测试文字${turnText}，已触发后半段对话链路` }));
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "发送测试文字失败", tone: "error" });
    } finally {
      setAsking("");
    }
  };

  const renderDeviceRow = (device, allowLabel = "允许") => {
    const view = builtinDeviceView(device);
    const session = sessions.find((item) => item.device_id === device.device_id) || null;
    const online = Boolean(session);
    const details = builtinDeviceInfoView(device);
    const controls = builtinDeviceControlsView(device, session, status.device_prefs);
    const hasAnyControl =
      controls.showVolume ||
      controls.showBrightness ||
      controls.showTheme ||
      controls.showPreview ||
      controls.showPressToTalk;
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
                {online && (
                  <div className="voice-ok">
                    {session?.transport === "mqtt" ? "常连在线" : "在线"}
                  </div>
                )}
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
                  <ShieldCheck size={13} /> {allowLabel}
                </button>
              )}
              {view.canRevoke && (
                <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => revoke(device)}>
                  <ShieldOff size={13} /> 撤销
                </button>
              )}
              {view.canRemove && (
                <button
                  type="button"
                  className="btn btn-ghost voice-device-remove-btn"
                  data-device-remove={device.device_id}
                  disabled={Boolean(busy)}
                  onClick={() => remove(device)}
                >
                  <Trash2 size={13} /> 移除
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
                title="展开这台设备的在线快捷控制（音量、亮度、深浅屏、按键拾音、投送预览）与设备信息"
              >
                {expanded === device.device_id ? "收起" : "控制与信息"}
              </button>
            </span>
          </td>
        </tr>
        {expanded === device.device_id && (
          <tr>
            <td colSpan={5}>
              {device.status === "allowed" && hasAnyControl && (
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
                    {controls.showVolume && (
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
                    )}
                    {controls.showBrightness && (
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
                    )}
                    {(controls.showTheme || controls.showPreview) && (
                      <div className="voice-device-control-item">
                        <span className="voice-device-control-label">
                          <span>
                            {controls.showTheme && controls.showPreview
                              ? "屏幕主题与激光预览投屏"
                              : controls.showTheme
                                ? "屏幕主题"
                                : "激光预览投屏"}
                          </span>
                        </span>
                        <div className="voice-actions">
                          {controls.showTheme && (
                            <>
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
                            </>
                          )}
                          {controls.showPreview && (
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
                          )}
                        </div>
                      </div>
                    )}
                    {controls.showPressToTalk && (
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
                    )}
                  </div>
                  {controlNotice[device.device_id] && (
                    <div className="voice-ok" style={{ marginTop: 6 }}>{controlNotice[device.device_id]}</div>
                  )}
                </div>
              )}
              {debugInject.showDeviceInput(device, online) && (
                <div className="voice-device-controls" id={`voice-device-inject-${device.device_id}`}>
                  <div className="voice-device-controls-head">
                    <b>发送测试文字（模拟联调）</b>
                    <span className="voice-muted">跳过麦克风与识别，走完整后半段链路；自动带模拟标记并拒绝物理动作</span>
                  </div>
                  <div className="voice-actions" style={{ marginTop: 6 }}>
                    <input
                      type="text"
                      className="text-input"
                      id={`voice-device-inject-input-${device.device_id}`}
                      maxLength={500}
                      placeholder="输入要注入小智的话（例如：激光机现在什么状态）"
                      value={injectDrafts[device.device_id] || ""}
                      disabled={Boolean(asking)}
                      onChange={(e) =>
                        setInjectDrafts((prev) => ({ ...prev, [device.device_id]: e.target.value }))
                      }
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && String(injectDrafts[device.device_id] || "").trim()) {
                          void sendDebugInjectText(device);
                        }
                      }}
                      style={{ flex: 1, minWidth: 220 }}
                    />
                    <button
                      type="button"
                      className="btn btn-primary"
                      id={`voice-device-inject-send-${device.device_id}`}
                      disabled={Boolean(asking) || !String(injectDrafts[device.device_id] || "").trim()}
                      onClick={() => void sendDebugInjectText(device)}
                    >
                      发送测试文字
                    </button>
                  </div>
                  {injectNotice[device.device_id] && (
                    <div className="voice-ok" style={{ marginTop: 6 }}>{injectNotice[device.device_id]}</div>
                  )}
                </div>
              )}
              <p className="voice-muted">
                版本与芯片来自开机上报，音频参数来自握手，「刷新信息」仅更新实时状态；设备端自带的回声消除不会在这里显示。
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
  };

  return (
    <VoiceGroup id="builtin-devices" title="已连接设备" summary={summary} tone={pending ? "warn" : undefined} defaultOpen>
      <div className="voice-device-policy-bar">
        <p className="voice-muted" style={{ margin: 0 }}>
          {autoAllow
            ? "已开启自动允许：之后新连入局域网的小智自动设为“已允许”并永久记住；已在排队的仍需逐台允许，已撤销设备仍拒绝连接。"
            : "新设备首次连入时会在屏幕中央弹窗确认，允许一次即永久记住，无需重启小智即可直接唤醒对话。"}
        </p>
        <label className="voice-check voice-device-auto-allow">
          <input
            type="checkbox"
            id="voice-device-auto-allow-switch"
            checked={autoAllow}
            disabled={Boolean(busy)}
            onChange={(e) => void toggleAutoAllow(e.target.checked)}
          />
          <span>自动允许局域网新设备连接</span>
        </label>
      </div>
      <div id="voice-builtin-devices">
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
              {activeDevices.length === 0 ? (
                <tr>
                  <td colSpan={5} className="voice-muted">
                    {revokedDevices.length > 0
                      ? "当前没有已允许或待允许的设备；已撤销设备收在下方分组。"
                      : "还没有设备连接过。启动服务并把设备 OTA 地址指向本机后，设备会出现在这里。"}
                  </td>
                </tr>
              ) : (
                activeDevices.map((device) => renderDeviceRow(device, "允许"))
              )}
            </tbody>
          </table>
        </div>
        {revokedDevices.length > 0 && (
          <details className="fill-options voice-revoked-fold" id="voice-revoked-devices">
            <summary>已撤销（{revokedDevices.length} 台）</summary>
            <div className="ob-tbl voice-tbl voice-revoked-tbl">
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
                  {revokedDevices.map((device) => renderDeviceRow(device, "重新允许"))}
                </tbody>
              </table>
            </div>
          </details>
        )}
      </div>
    </VoiceGroup>
  );
}
