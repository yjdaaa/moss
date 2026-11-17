import React, { useCallback, useEffect, useRef, useState } from "react";
import { Radio } from "lucide-react";
import { getJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { kindLabel, sourceLabel } from "../approvalState.js";
import { voiceApi } from "../voiceApi.js";
import {
  BuiltinDevicesPanel,
  BuiltinEvents,
  BuiltinJevModelGroup,
  BuiltinModelGroups,
  BuiltinScreenPreview,
  BuiltinServicePanel,
  ExperienceGroup,
  LiveDebugTraces,
  LocalModelsGroup,
  PrivateSettingsEntry,
  XiaozhiUsbProvisionCard,
  useBuiltinStatus,
} from "../components/VoiceBuiltinCard.jsx";
import VoiceMusicCard from "../components/VoiceMusicCard.jsx";
import VoiceFirmwareCard from "../components/VoiceFirmwareCard.jsx";
import VoiceAgentCard from "../components/VoiceAgentCard.jsx";
import VoiceVisionCard from "../components/VoiceVisionCard.jsx";
import VoiceKnowledgeCard from "../components/VoiceKnowledgeCard.jsx";
import { VoiceMcpCard } from "../components/VoiceMcpCard.jsx";
import { VoiceEndpointsCard, VoiceToolsCard } from "../components/VoiceEndpointsCard.jsx";
import VoiceWakeWordSection from "../components/VoiceWakeWordSection.jsx";
import { SaveBar, VOICE_GOTO_EVENT, VoiceDirtyProvider, VoiceGroup, VoiceTabPanel, focusVoiceGroup } from "../components/VoiceLayout.jsx";
import { HOME_APPROVALS, VOICE_FOCUS_EVENT, peekVoiceFocus } from "../voiceFocus.js";
import { MossVoiceTwin } from "../components/MossVoiceTwin.jsx";
import {
  SETTINGS_GROUPS,
  VOICE_MODE_LABEL,
  VOICE_POLL_MS,
  VOICE_TABS,
  normalizeVoiceTab,
  voiceTabsFor,
  approvalHistoryStatus,
  approvalListSummary,
  approvalOutcomeReason,
  deviceView,
  dirtyTabs,
  modeConfirmText,
  savedNoteText,
  settingsGroupDirty,
  settingsGroupPayload,
  settingsSaveEffect,
  voiceErrorText,
} from "../voiceState.js";

// 语音中心（技术书 §9.5.1，实施清单 P4-08；r15 起按标签与链路分组，UI-3）。只在 MOSS 桌面端显示（A-69）。
// 所有操作经 Tauri 命令执行；模型密钥只在本页内存中，提交后清空，不回显已保存的密钥（A-67）。
// 顶部选链路，下面按任务分标签；当前链路用不到的分组不渲染（2026-10-18）。
// n69（2026-11-07“完整整理”）：内置链路 6 个标签——服务（启停与连接地址）/ 设备（设备列表与快捷控制、唤醒词、副屏预览、固件）/
// 模型（混搭栏、云端、本地、对话体验）/ 扩展（含 MOSS 内置工具）/ 隐私与家居 / 记录与诊断；同一个设置只在一处可改。
// 标签和分组都只隐藏、不卸载，切换标签、收起分组或切换链路都不会丢失未保存的填写。
// 2026-11-17（用户决定）：删除自建服务端链路，链路只剩“MOSS 内置服务”与“官方小智”。

const TONE_CLASS = { ok: "ob-pill--ok", info: "ob-pill--info", warn: "ob-pill--warn", bad: "ob-pill--bad", off: "ob-pill--off" };
const TAB_KEY = "moss_voice_tab";

function Pill({ tone = "off", children, id, title }) {
  return (
    <span className={`ob-pill ${TONE_CLASS[tone] || TONE_CLASS.off}`} id={id} title={title}>
      {children}
    </span>
  );
}

function formatTime(seconds) {
  if (!seconds) return "—";
  const date = new Date(seconds * 1000);
  return date.toLocaleString("zh-CN", { hour12: false, month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function readTab() {
  try {
    // 记住的旧标签（如已并入“设备”的 "wake"）换到新位置
    const value = normalizeVoiceTab(globalThis.localStorage?.getItem(TAB_KEY));
    if (value) return value;
  } catch (_error) {
    // 存储不可用时打开第一个标签
  }
  return VOICE_TABS[0].key;
}

function writeTab(value) {
  try {
    globalThis.localStorage?.setItem(TAB_KEY, value);
  } catch (_error) {
    // 只影响下次打开时的默认标签
  }
}

function settingsForm(status) {
  const settings = status?.settings || {};
  return {
    builtin_ws_port: String(settings.builtin_ws_port || 8000),
    builtin_http_port: String(settings.builtin_http_port || 8003),
    builtin_mqtt_enabled: String(settings.builtin_mqtt_enabled === true),
    builtin_mqtt_port: String(settings.builtin_mqtt_port || 1883),
    builtin_udp_port: String(settings.builtin_udp_port || 8888),
    // 带令牌的地址不回填，只显示脱敏后的当前值；留空表示不改
    official_mcp_endpoint: "",
  };
}

/**
 * 按链路摆放的一组内容：当前链路用得到的在前；其他链路的排在后面，默认折叠。
 * 所有部分始终在同一父元素下按固定顺序渲染（只用 CSS order 调整位置），切换链路不会重建组件、不丢填写。
 */
/** 只渲染当前链路用得到的那部分：没选中的链路卡片直接不出现（2026-10-18 用户决定）。 */
function ModeSplit({ id, mode, parts }) {
  const visible = parts.filter((part) => part.modes.includes(mode));
  return (
    <div className="voice-modesplit" id={`voice-split-${id}`}>
      {visible.map((part) => (
        <div key={part.key} className="voice-part" data-part={part.key}>
          {part.node}
        </div>
      ))}
    </div>
  );
}

const FIELD = {
  official_mcp_endpoint: { label: "官方接入点（小智控制台生成的 wss:// 地址）", wide: true, mono: true, secret: true, placeholder: "wss://…/mcp/?token=…", id: "voice-official-endpoint" },
  builtin_ws_port: { label: "设备连接端口（默认 8000）", numeric: true, id: "voice-builtin-ws-port" },
  builtin_http_port: { label: "OTA 端口（默认 8003）", numeric: true, id: "voice-builtin-http-port" },
  builtin_mqtt_enabled: { label: "开启常连模式：小智开机就在线，不唤醒也能调音量、推屏；说话时语音走加密 UDP", toggle: true, wide: true, id: "voice-builtin-mqtt-enabled" },
  builtin_mqtt_port: { label: "常连 MQTT 端口（默认 1883，不能用 8883）", numeric: true, id: "voice-builtin-mqtt-port" },
  builtin_udp_port: { label: "常连语音 UDP 端口（默认 8888）", numeric: true, id: "voice-builtin-udp-port" },
};

/** 链路设置的一个分组：只提交本组字段，按钮写明保存对象。 */
function SettingsGroup({ group, status, form, baseline, onChange, onSaved, busy, setBusy, children }) {
  const spec = SETTINGS_GROUPS[group];
  const settings = status.settings || {};
  const [result, setResult] = useState(null);
  const dirty = settingsGroupDirty(group, form, baseline);
  const label = `saving-${group}`;

  const summary = {
    official: settings.official_mcp_endpoint ? `已设置 · ${settings.official_mcp_endpoint}` : "未设置",
    builtinPorts: `设备连接 ${settings.builtin_ws_port || 8000} · OTA ${settings.builtin_http_port || 8003}`,
    builtinMqtt: settings.builtin_mqtt_enabled
      ? `已开启 · MQTT ${settings.builtin_mqtt_port || 1883} · 语音 UDP ${settings.builtin_udp_port || 8888}`
      : "未开启（小智唤醒后才连接）",
  }[group];

  const save = async () => {
    setBusy(label);
    try {
      const next = await voiceApi.saveSettings(settingsGroupPayload(group, form));
      onSaved(group, next);
      setResult({ ok: true, text: savedNoteText(spec.title, settingsSaveEffect(group, Boolean(next?.reconnected))) });
    } catch (error) {
      setResult({ ok: false, text: `保存失败：${voiceErrorText(error)}` });
    } finally {
      setBusy("");
    }
  };

  const renderField = (key) => {
    const field = FIELD[key];
    const current = settings[key];
    if (field.toggle) {
      return (
        <label key={key} className="voice-wide voice-check">
          <input
            type="checkbox"
            id={field.id}
            checked={form[key] === "true"}
            onChange={(e) => {
              onChange(key, e.target.checked ? "true" : "false");
              setResult(null);
            }}
          />
          <span>{field.label}</span>
        </label>
      );
    }
    const input = (
      <input
        className={`text-input${field.mono ? " mono" : ""}`}
        inputMode={field.numeric ? "numeric" : undefined}
        value={form[key]}
        onChange={(e) => {
          onChange(key, e.target.value);
          setResult(null);
        }}
        placeholder={field.secret ? (settings[`${key}_set`] ? "留空表示不改" : field.placeholder) : undefined}
        spellCheck="false"
        autoComplete="off"
        id={field.id}
      />
    );
    return (
      <label key={key} className={field.wide ? "voice-wide" : undefined}>
        <span>
          {field.label}
          {field.secret && current ? `；当前：${current}` : ""}
        </span>
        {input}
      </label>
    );
  };

  return (
    <VoiceGroup id={`settings-${group}`} title={spec.title} summary={summary} dirty={dirty}>
      {children}
      <div className="voice-form-grid">{spec.keys.map(renderField)}</div>
      <SaveBar id={`voice-settings-save-${group}`} target={spec.title} dirty={dirty} busy={Boolean(busy)} saving={busy === label} result={result} onSave={save} />
    </VoiceGroup>
  );
}

export function VoicePage({ mcpStatus = null, approvalCount = 0 }) {
  const [status, setStatus] = useState(null);
  const [loadError, setLoadError] = useState("");
  const [busy, setBusy] = useState("");
  const [form, setForm] = useState(null);
  const [baseline, setBaseline] = useState(null);
  const [tab, setTab] = useState(() => (peekVoiceFocus() === HOME_APPROVALS ? "privacy" : readTab()));
  // 全局审批卡“去家居审批”：切到“隐私与家居”标签（展开家居审批由 PrivateSettingsEntry 处理）
  useEffect(() => {
    const onFocus = (event) => {
      if (event.detail === HOME_APPROVALS) setTab("privacy");
    };
    window.addEventListener(VOICE_FOCUS_EVENT, onFocus);
    return () => window.removeEventListener(VOICE_FOCUS_EVENT, onFocus);
  }, []);
  // n69 页面内“去设置”跳转：先切标签，等标签面板显示后再展开并滚动到目标分组
  const [gotoTarget, setGotoTarget] = useState(null);
  useEffect(() => {
    const onGoto = (event) => {
      const key = normalizeVoiceTab(event.detail?.tab);
      if (!key) return;
      setTab(key);
      writeTab(key);
      setGotoTarget({ group: event.detail?.group || "", target: event.detail?.target || "" });
    };
    window.addEventListener(VOICE_GOTO_EVENT, onGoto);
    return () => window.removeEventListener(VOICE_GOTO_EVENT, onGoto);
  }, []);
  useEffect(() => {
    if (!gotoTarget) return;
    focusVoiceGroup(gotoTarget.group, gotoTarget.target);
    setGotoTarget(null);
  }, [gotoTarget]);
  const [dirty, setDirty] = useState({});
  const [approvals, setApprovals] = useState([]);
  // n68：列表上限与读取失败原因（失败时保留上次列表并明确提示，不悄悄显示旧数据）
  const [approvalsMeta, setApprovalsMeta] = useState({ limit: 0, error: "" });
  const mounted = useRef(true);
  const builtin = useBuiltinStatus();

  const reportDirty = useCallback((id, where) => {
    setDirty((current) => ((current[id] || "") === where ? current : { ...current, [id]: where }));
  }, []);

  const refresh = useCallback(async () => {
    try {
      const next = await voiceApi.status();
      if (!mounted.current) return;
      setStatus(next);
      setLoadError("");
      setForm((current) => current || settingsForm(next));
      setBaseline((current) => current || settingsForm(next));
    } catch (error) {
      if (mounted.current) setLoadError(voiceErrorText(error));
    }
  }, []);

  /// 严格模式开关（技术书 §6.2）：写运行目录 `.env`；开启立即生效，关闭需重启 MOSS。
  const toggleStrict = async () => {
    if (!status?.strict) return;
    const enabled = !status.strict.enabled;
    const ok = await showConfirm(
      enabled
        ? "开启后，小智、局域网与网关要求运动或出光时只生成待批准请求，必须在电脑上批准才执行；急停、取消与降低倍率不受影响。确定开启？"
        : "关闭后立即生效：语音与局域网在模型带上确认参数时就能直接让机器运动、出光，没有桌面批准。确定关闭？",
      {
        title: enabled ? "开启严格模式？" : "关闭严格模式？",
        tone: enabled ? "warning" : "error",
        danger: !enabled,
        confirmText: enabled ? "开启" : "关闭",
      },
    );
    if (!ok) return;
    try {
      const result = await voiceApi.strictSet(enabled, true);
      const text = result?.needs_restart
        ? `已写入 ${result.env_file}。这次运行仍保持原状态，重启 MOSS 后生效。`
        : enabled
          ? "严格模式已开启，立即生效。"
          : "严格模式已关闭，立即生效。";
      await showAlert(text, { title: "严格模式", tone: result?.needs_restart ? "warning" : "success" });
      await refresh();
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "设置失败", tone: "error" });
    }
  };

  const refreshApprovals = useCallback(async () => {
    let res;
    try {
      res = await getJson("/v1/approvals?status=all");
    } catch (error) {
      if (mounted.current) setApprovalsMeta((meta) => ({ ...meta, error: voiceErrorText(error) }));
      return;
    }
    if (!mounted.current) return;
    if (res?.ok && res.data?.success) {
      setApprovals(Array.isArray(res.data.result?.approvals) ? res.data.result.approvals : []);
      setApprovalsMeta({ limit: Number(res.data.result?.limit) || 0, error: "" });
    } else {
      const result = res?.data?.result;
      const code = res?.data?.detail?.error_code;
      const reason = typeof result === "string" && result
        ? result
        : code
          ? `错误码 ${code}`
          : res?.status
            ? `后端返回 HTTP ${res.status}`
            : "没有收到具体原因";
      setApprovalsMeta((meta) => ({ ...meta, error: reason }));
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    refresh();
    refreshApprovals();
    const timer = setInterval(() => {
      if (!document.hidden) {
        refresh();
        refreshApprovals();
      }
    }, VOICE_POLL_MS);
    const onVisibility = () => {
      if (!document.hidden) {
        refresh();
        refreshApprovals();
      }
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      mounted.current = false;
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [refresh, refreshApprovals]);

  const run = useCallback(
    async (label, action, { quiet = false } = {}) => {
      setBusy(label);
      try {
        const result = await action();
        if (result && result.mode && result.settings) setStatus(result);
        else if (result?.status?.mode) setStatus(result.status);
        return result;
      } catch (error) {
        if (!quiet) await showAlert(voiceErrorText(error), { title: `${label}失败`, tone: "error" });
        return null;
      } finally {
        if (mounted.current) setBusy("");
      }
    },
    [],
  );

  const mode = status?.mode || "official";
  // 当前链路要显示的标签：官方链路没有“模型”标签；记住的标签若当前链路没有，回退到第一个
  const tabs = voiceTabsFor(mode);
  const activeTab = tabs.some((item) => item.key === tab) ? tab : tabs[0].key;
  const device = deviceView(mode);

  const selectTab = (key) => {
    setTab(key);
    writeTab(key);
  };

  const switchMode = async (target) => {
    if (target === mode || busy) return;
    let changes;
    try {
      changes = (await voiceApi.modeChanges(target))?.changes || [];
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "无法切换", tone: "warning" });
      return;
    }
    const { title, message } = modeConfirmText(target, changes);
    if (!(await showConfirm(message, { title, tone: "warning", confirmText: "确认切换" }))) return;
    await run("切换链路", () => voiceApi.setMode(target));
  };

  const changeSetting = (key, value) => setForm((current) => ({ ...current, [key]: value }));

  // 保存一组后只重置这组字段，其他组未保存的填写保留
  const settingsSaved = (group, next) => {
    setStatus(next);
    const fresh = settingsForm(next);
    const keys = SETTINGS_GROUPS[group].keys;
    setBaseline(fresh);
    setForm((current) => {
      const merged = { ...current };
      for (const key of keys) merged[key] = fresh[key];
      return merged;
    });
  };

  if (!status || !form) {
    return (
      <div className="ob-page voice-page">
        <section className="ob-card">
          <div className="ob-card-h">
            <h2>
              <Radio size={16} aria-hidden="true" /> 语音中心
            </h2>
          </div>
          <p className="voice-muted">{loadError || "正在读取语音中心状态…"}</p>
        </section>
      </div>
    );
  }

  const builtinStatus = builtin.status && !builtin.status.error ? builtin.status : null;
  const builtinSessions = Array.isArray(builtinStatus?.sessions) ? builtinStatus.sessions.length : 0;
  const unsaved = dirtyTabs(dirty);
  const pendingApprovals = approvalCount || approvals.filter((item) => item?.status === "pending").length;
  const serviceView = {
    builtin: builtinStatus?.running ? { text: "内置服务运行中", tone: "ok" } : { text: "内置服务未运行", tone: "off" },
    official: status.bridge?.connected ? { text: "小智桥已连接", tone: "ok" } : { text: "小智桥未连接", tone: "bad" },
  }[mode];
  const deviceLine = mode === "builtin" ? { text: `${builtinSessions} 台设备在线`, tone: builtinSessions ? "ok" : "warn" } : { text: `设备：${device.text}`, tone: device.tone };

  const groupProps = { status, form, baseline, onChange: changeSetting, onSaved: settingsSaved, busy, setBusy };

  const officialBlock = (
    <div className="voice-block" id="voice-endpoint-card">
      <div className="voice-block-h">
        <h3>小智接入点</h3>
        <Pill tone={status.bridge?.connected ? "ok" : "bad"}>{status.bridge?.connected ? "MOSS 桥已连接" : "MOSS 桥未连接"}</Pill>
      </div>
      <div className="voice-kv">
        <span className="mono voice-break">{status.mcp_endpoint || "未设置接入点（在下面“官方接入点”组填写）"}</span>
      </div>
    </div>
  );

  return (
    <VoiceDirtyProvider report={reportDirty}>
      <div className="ob-page voice-page">
        <section className="ob-card" id="voice-mode-card">
          <div className="ob-card-h">
            <h2>
              <Radio size={16} aria-hidden="true" /> 语音链路
            </h2>
            <span className="ob-k">VOICE LINK</span>
          </div>
          <div className="voice-mode-row" role="radiogroup" aria-label="语音链路">
            {[
              { key: "builtin", desc: "设备直连 MOSS，不需要外部服务端" },
              { key: "official", desc: "设备连官方服务器，MOSS 作为工具接入" },
            ].map((item) => (
              <button
                key={item.key}
                type="button"
                role="radio"
                aria-checked={mode === item.key}
                className={`voice-mode-option${mode === item.key ? " on" : ""}`}
                onClick={() => switchMode(item.key)}
                disabled={Boolean(busy)}
                id={`voice-mode-${item.key}`}
              >
                <span className="voice-radio" aria-hidden="true" />
                <span>
                  <b>{VOICE_MODE_LABEL[item.key]}</b>
                  <small>{item.desc}</small>
                </span>
              </button>
            ))}
          </div>
          <div className="voice-statusline" id="voice-statusline">
            <Pill tone={serviceView.tone}>{serviceView.text}</Pill>
            <Pill tone={deviceLine.tone}>{deviceLine.text}</Pill>
            <button
              type="button"
              className={`ob-pill voice-strict-toggle ${TONE_CLASS[status.strict?.enabled ? "ok" : "warn"] || TONE_CLASS.off}`}
              id="voice-strict-state"
              onClick={toggleStrict}
              disabled={!status.strict}
              title={status.strict?.from_env ? "当前由 .env 开启；点这里可以切换" : "点这里切换（会自动写入运行目录的 .env）"}
            >
              严格模式{status.strict?.enabled ? "已开启" : "未开启"} · 点此切换
            </button>
          </div>
          <MossVoiceTwin
            mode={mode}
            status={status}
            builtinStatus={builtinStatus}
            mcpStatus={mcpStatus}
            approvalCount={pendingApprovals}
          />
          {(device.hint || (mode === "official" && !status.strict?.enabled)) && (
            <div className={`voice-hint voice-hint--${mode === "official" && !status.strict?.enabled ? "warn" : device.tone}`} id="voice-ota-hint">
              {[
                device.hint,
                mode === "official" && !status.strict?.enabled ? "严格模式未开启：大模型可以自行确认加工任务；要每次批准就点上面的开关。" : "",
              ].filter(Boolean).join(" · ")}
            </div>
          )}
        </section>

        <div className="voice-pagetabs" role="tablist" aria-label="语音中心">
          {tabs.map((item) => (
            <button
              key={item.key}
              type="button"
              role="tab"
              aria-selected={tab === item.key}
              aria-controls={`voice-panel-${item.key}`}
              className={`voice-pagetab${tab === item.key ? " on" : ""}`}
              onClick={() => selectTab(item.key)}
              id={`voice-page-tab-${item.key}`}
            >
              {item.label}
              {unsaved.has(item.key) && (
                <span className="voice-tab-dot" title="有未保存的修改" aria-label="有未保存的修改">
                  ●
                </span>
              )}
            </button>
          ))}
        </div>

        <section className="ob-card voice-panels">
          <VoiceTabPanel tab="service" active={activeTab === "service"}>
            <ModeSplit
              id="service"
              mode={mode}
              parts={[
                {
                  key: "official",
                  modes: ["official"],
                  node: (
                    <div className="voice-stack">
                      {officialBlock}
                      <SettingsGroup group="official" {...groupProps} />
                    </div>
                  ),
                },
                {
                  key: "builtin",
                  modes: ["builtin"],
                  node: (
                    <div className="voice-stack">
                      <BuiltinServicePanel builtin={builtin} />
                      <SettingsGroup group="builtinPorts" {...groupProps} />
                      <SettingsGroup group="builtinMqtt" {...groupProps}>
                        <p className="voice-muted" id="voice-builtin-mqtt-note">
                          实验功能，默认关。打开后小智开机自动连上 MOSS，语音中心直接显示在线；麦克风声音只在唤醒后才发。
                          保存后重启内置服务，再把小智重启一次它才会换连接方式；关掉开关、小智重启一次就回到原来的方式。
                          第一次用时电脑防火墙要放行这两个端口。
                        </p>
                      </SettingsGroup>
                    </div>
                  ),
                },
              ]}
            />
          </VoiceTabPanel>

          <VoiceTabPanel tab="models" active={activeTab === "models"}>
            <ModeSplit
              id="models"
              mode={mode}
              parts={[
                {
                  key: "builtin",
                  modes: ["builtin"],
                  node: (
                    <div className="voice-stack" id="voice-builtin-models-panel">
                      {/* n69 顺序：混搭栏 → 识别 → 大模型 → 合成 → 视觉理解 → Jev（高级）→ 本地模型 → 对话体验 */}
                      <BuiltinModelGroups builtin={builtin} />
                      <VoiceVisionCard />
                      <BuiltinJevModelGroup builtin={builtin} />
                      {builtinStatus && (
                        <>
                          <h3 className="voice-section-h">本地模型（断网可用）</h3>
                          <LocalModelsGroup builtin={builtin} />
                          <h3 className="voice-section-h">对话体验</h3>
                          <ExperienceGroup builtin={builtin} />
                        </>
                      )}
                    </div>
                  ),
                },
              ]}
            />
          </VoiceTabPanel>

          <VoiceTabPanel tab="devices" active={activeTab === "devices"}>
            <div className="voice-stack" id="voice-devices-panel">
              {/* n69：设备列表与快捷控制（音量只在这里调）、唤醒词、副屏预览、固件都在“设备”标签 */}
              {mode === "builtin" && <BuiltinDevicesPanel builtin={builtin} />}
              <VoiceEndpointsCard mode={mode} overview={status.mcp} busy={busy} run={run} />
              <VoiceWakeWordSection mode={mode} active={activeTab === "devices"} />
              {mode === "builtin" && <BuiltinScreenPreview builtin={builtin} />}
              {/* n67（2026-11-07）：独立安全窗口取消，固件升级直接在这里；每台推送前仍逐台确认 */}
              <VoiceGroup id="device-firmware" title="固件升级" summary="导入自己编译的固件，逐台确认推送" lazy>
                <VoiceFirmwareCard />
              </VoiceGroup>
              {mode === "builtin" && (
                <VoiceGroup
                  id="device-usb-provision"
                  title="USB 一键配网写地址"
                  summary="插线直写小智 Wi-Fi、本机地址与设备密钥（免手机热点）"
                  lazy
                >
                  <XiaozhiUsbProvisionCard
                    serviceStatus={builtin.status}
                    onSetStatus={builtin.setStatus}
                  />
                </VoiceGroup>
              )}
            </div>
          </VoiceTabPanel>

          <VoiceTabPanel tab="features" active={activeTab === "features"}>
            <div className="voice-stack">
              {mode === "builtin" && (
                <>
                  <h3 className="voice-section-h">内置服务能力（只在“MOSS 内置服务”链路生效）</h3>
                  <VoiceMusicCard />
                  <VoiceKnowledgeCard />
                </>
              )}
              <h3 className="voice-section-h">MOSS 内置工具（所有链路共用）</h3>
              <VoiceToolsCard overview={status.mcp} busy={busy} run={run} />
              <h3 className="voice-section-h">第三方扩展与本机代理（内置服务链路调用）</h3>
              {mode !== "builtin" && (
                <div className="voice-hint voice-hint--warn">
                  {`当前为“${VOICE_MODE_LABEL[mode]}”链路：第三方 MCP 工具与本机 Agent 仅在“MOSS 内置服务”链路下对语音开放；此处可预先配置，切换至内置服务后生效。`}
                </div>
              )}
              <VoiceMcpCard defaultOpen={false} />
              <VoiceAgentCard />
            </div>
          </VoiceTabPanel>

          <VoiceTabPanel tab="privacy" active={activeTab === "privacy"}>
            {mode === "builtin" && <PrivateSettingsEntry status={builtinStatus} />}
          </VoiceTabPanel>

          <VoiceTabPanel tab="records" active={activeTab === "records"}>
            <ModeSplit
              id="records"
              mode={mode}
              parts={[
                {
                  key: "approvals",
                  modes: ["official", "builtin"],
                  node: (
                    <VoiceGroup id="approvals" title="审批记录" summary={approvalListSummary(approvals.length, approvalsMeta.limit, approvalsMeta.error)} defaultOpen>
                      {approvalsMeta.error && (
                        <div className="voice-hint voice-hint--bad" id="voice-approvals-error">
                          审批记录读取失败：{approvalsMeta.error}{approvals.length ? "（下面是上次读到的记录）" : ""}
                        </div>
                      )}
                      <div className="ob-tbl voice-tbl" id="voice-approvals-card">
                        <table>
                          <thead>
                            <tr>
                              <th>时间</th>
                              <th>内容</th>
                              <th>来源</th>
                              <th>状态</th>
                            </tr>
                          </thead>
                          <tbody>
                            {approvals.length === 0 ? (
                              <tr>
                                <td colSpan={4} className="voice-muted">暂无审批记录</td>
                              </tr>
                            ) : (
                              approvals.map((record) => {
                                const view = approvalHistoryStatus(record);
                                const reason = approvalOutcomeReason(record);
                                return (
                                  <tr key={record.approval_id}>
                                    <td className="mono">{formatTime(record.created_at)}</td>
                                    <td>
                                      {kindLabel(record.kind)} · {record.summary || "—"}
                                    </td>
                                    <td>{sourceLabel(record.source, record.session_id)}</td>
                                    <td>
                                      <Pill tone={view.tone}>{view.text}</Pill>
                                      {reason && <div className="voice-muted voice-approval-reason">原因：{reason}</div>}
                                    </td>
                                  </tr>
                                );
                              })
                            )}
                          </tbody>
                        </table>
                      </div>
                    </VoiceGroup>
                  ),
                },
                // 2026-11-09：激光雕刻机与 MOSS 软件的事件和语音链路无关，两种链路都显示
                { key: "builtin-events", modes: ["official", "builtin"], node: <BuiltinEvents builtin={builtin} /> },
                { key: "debug-traces", modes: ["builtin"], node: <LiveDebugTraces builtin={builtin} /> },
              ]}
            />
          </VoiceTabPanel>
        </section>
      </div>
    </VoiceDirtyProvider>
  );
}
