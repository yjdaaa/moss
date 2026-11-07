import React, { useCallback, useEffect, useRef, useState } from "react";
import { Radio, Play, Square, RotateCcw, FlaskConical, RefreshCw } from "lucide-react";
import { getJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { kindLabel, sourceLabel } from "../approvalState.js";
import { voiceApi } from "../voiceApi.js";
import {
  BuiltinModelGroups,
  BuiltinServicePanel,
  ExperienceGroup,
  LiveDebugTraces,
  LocalModelsGroup,
  PrivateSettingsEntry,
  PrivateFirmwareEntry,
  useBuiltinStatus,
} from "../components/VoiceBuiltinCard.jsx";
import VoiceMusicCard from "../components/VoiceMusicCard.jsx";
import VoiceAgentCard from "../components/VoiceAgentCard.jsx";
import VoiceVisionCard from "../components/VoiceVisionCard.jsx";
import VoiceKnowledgeCard from "../components/VoiceKnowledgeCard.jsx";
import { VoiceMcpCard } from "../components/VoiceMcpCard.jsx";
import { VoiceEndpointsCard } from "../components/VoiceEndpointsCard.jsx";
import VoiceWakeVolumePanel from "../components/VoiceWakeVolumePanel.jsx";
import { SaveBar, VoiceDirtyProvider, VoiceGroup, VoiceTabPanel } from "../components/VoiceLayout.jsx";
import { MossVoiceTwin } from "../components/MossVoiceTwin.jsx";
import {
  SECTIONS,
  SETTINGS_GROUPS,
  VOICE_MODE_LABEL,
  VOICE_POLL_MS,
  VOICE_TABS,
  voiceTabsFor,
  approvalHistoryStatus,
  canSaveSection,
  deviceView,
  dirtyTabs,
  emptyForm,
  externalModelSummary,
  finalUrl,
  formDirty,
  isSectionBlank,
  maskLogLine,
  modeConfirmText,
  savedNoteText,
  sectionFingerprint,
  sectionInputError,
  sectionPayload,
  serverStateView,
  settingsGroupDirty,
  settingsGroupPayload,
  settingsSaveEffect,
  voiceErrorText,
} from "../voiceState.js";

// 语音中心（技术书 §9.5.1，实施清单 P4-08；r15 起按标签与链路分组，UI-3）。只在 MOSS 桌面端显示（A-69）。
// 所有操作经 Tauri 命令执行；模型密钥只在本页内存中，提交后清空，不回显已保存的密钥（A-67）。
// 顶部选链路，下面按任务分标签；每个标签里当前链路用得到的分组在前，其他链路的分组折叠在后。
// 标签和分组都只隐藏、不卸载，切换标签、收起分组或切换链路都不会丢失未保存的填写。

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
    const value = globalThis.localStorage?.getItem(TAB_KEY);
    if (VOICE_TABS.some((item) => item.key === value)) return value;
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
    server_dir: settings.server_dir || "",
    python: settings.python || "",
    args: Array.isArray(settings.args) ? settings.args.join(" ") : "app.py",
    server_port: String(settings.server_port || 8000),
    builtin_ws_port: String(settings.builtin_ws_port || 8000),
    builtin_http_port: String(settings.builtin_http_port || 8003),
    // 带令牌的地址不回填，只显示脱敏后的当前值；留空表示不改
    self_hosted_mcp_endpoint: "",
    endpoint_health_url: "",
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
  server_dir: { label: "xiaozhi-server 目录（main/xiaozhi-server）", wide: true, mono: true, id: "voice-server-dir" },
  python: { label: "Python 3.10 解释器路径", mono: true, id: "voice-python" },
  args: { label: "启动参数", mono: true },
  server_port: { label: "服务端端口（识别外部实例）", numeric: true },
  self_hosted_mcp_endpoint: { label: "自建接入点（ws://…/mcp_endpoint/mcp/?token=…）", wide: true, mono: true, secret: true, placeholder: "ws://<接入点地址>:8004/mcp_endpoint/mcp/?token=…", id: "voice-self-endpoint" },
  endpoint_health_url: { label: "接入点健康检查地址（用来判断设备在线，可不填）", wide: true, mono: true, secret: true, placeholder: "http://<接入点地址>:8004/mcp_endpoint/health?key=…" },
  builtin_ws_port: { label: "设备连接端口（默认 8000）", numeric: true, id: "voice-builtin-ws-port" },
  builtin_http_port: { label: "OTA 端口（默认 8003）", numeric: true, id: "voice-builtin-http-port" },
};

/** 链路与服务端设置的一个分组：只提交本组字段，按钮写明保存对象。 */
function SettingsGroup({ group, mode, status, form, baseline, onChange, onSaved, busy, setBusy }) {
  const spec = SETTINGS_GROUPS[group];
  const settings = status.settings || {};
  const [result, setResult] = useState(null);
  const dirty = settingsGroupDirty(group, form, baseline);
  const label = `saving-${group}`;

  const summary = {
    official: settings.official_mcp_endpoint ? `已设置 · ${settings.official_mcp_endpoint}` : "未设置",
    selfHostedServer: settings.server_dir ? settings.server_dir : "未设置目录",
    selfHostedEndpoint: settings.self_hosted_mcp_endpoint ? `已设置 · ${settings.self_hosted_mcp_endpoint}` : "未设置",
    builtinPorts: `设备连接 ${settings.builtin_ws_port || 8000} · OTA ${settings.builtin_http_port || 8003}`,
  }[group];

  const save = async () => {
    setBusy(label);
    try {
      const next = await voiceApi.saveSettings(settingsGroupPayload(group, form));
      onSaved(group, next);
      setResult({ ok: true, text: savedNoteText(spec.title, settingsSaveEffect(group, mode, Boolean(next?.reconnected))) });
    } catch (error) {
      setResult({ ok: false, text: `保存失败：${voiceErrorText(error)}` });
    } finally {
      setBusy("");
    }
  };

  const advancedKeys = new Set(spec.advanced || []);
  /// 自建服务端要的 Python 路径不该让用户自己去安装目录里翻，直接探测后填入。
  const detectPython = async () => {
    setResult(null);
    try {
      const found = await voiceApi.detectPython();
      const suggested = found?.suggested;
      if (!suggested) {
        setResult({ ok: false, text: "没找到 Python 3：装好后再试，或者手动填写解释器路径" });
        return;
      }
      onChange("python", suggested);
      setResult({ ok: true, text: `已填入探测到的解释器：${suggested}` });
    } catch (error) {
      setResult({ ok: false, text: `探测失败：${voiceErrorText(error)}` });
    }
  };
  const renderField = (key) => {
    const field = FIELD[key];
    const current = settings[key];
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
        {key === "python" ? (
          <span className="voice-inline">
            {input}
            <button type="button" className="btn btn-ghost" onClick={detectPython} id="voice-settings-detect-python">
              自动探测
            </button>
          </span>
        ) : input}
      </label>
    );
  };

  return (
    <VoiceGroup id={`settings-${group}`} title={spec.title} summary={summary} dirty={dirty}>
      <div className="voice-form-grid">
        {spec.keys.filter((key) => !advancedKeys.has(key)).map(renderField)}
      </div>
      {advancedKeys.size > 0 && (
        <details className="voice-private-more" id={`voice-settings-${group}-advanced`}>
          <summary>高级选项（启动参数等，一般不用改）</summary>
          <div className="voice-form-grid">
            {spec.keys.filter((key) => advancedKeys.has(key)).map(renderField)}
          </div>
        </details>
      )}
      <SaveBar id={`voice-settings-save-${group}`} target={spec.title} dirty={dirty} busy={Boolean(busy)} saving={busy === label} result={result} onSave={save} />
    </VoiceGroup>
  );
}

/** 外部 xiaozhi-server 的一项模型：先测试、后保存，写入服务端 data/.config.yaml。 */
function ExternalModelGroup({ section, status, fullModule, busy, setBusy, refresh, run }) {
  const current = SECTIONS.find((item) => item.key === section);
  const [baseline, setBaseline] = useState(() => emptyForm(section));
  const [form, setForm] = useState(() => emptyForm(section));
  const [test, setTest] = useState(null);
  const [result, setResult] = useState(null);
  const dirty = formDirty(baseline, form);
  const saveEnabled = canSaveSection(section, form, test);
  const preview = finalUrl(section, form.url);
  const saved = status.models?.[section];
  const n = (suffix) => `voice-model-${suffix}-${section}`;

  const update = (key, value) => {
    setForm((all) => ({ ...all, [key]: value }));
    setResult(null);
  };

  const runTest = async () => {
    const inputError = sectionInputError(section, form);
    if (inputError) {
      setTest({ ok: false, detail: inputError, fingerprint: "" });
      return;
    }
    const fingerprint = sectionFingerprint(section, form);
    setBusy(`test-${section}`);
    try {
      const outcome = await voiceApi.modelsTest(section, sectionPayload(section, form));
      setTest({ ...outcome, fingerprint });
    } catch (error) {
      setTest({ ok: false, detail: voiceErrorText(error), fingerprint: "" });
    } finally {
      setBusy("");
      refresh();
    }
  };

  const save = async () => {
    setBusy(`save-${section}`);
    try {
      const outcome = await voiceApi.modelsSave({ [section]: sectionPayload(section, form) });
      // 密钥只在提交时使用：保存后清空输入框，之后也不回显
      const next = { ...form, api_key: "" };
      setForm(next);
      setBaseline(next);
      setTest(null);
      const backup = outcome.backup ? "原文件已备份为 .config.yaml.bak" : "新建了 data/.config.yaml";
      setResult({
        ok: true,
        text: savedNoteText(`${current.label}到服务端配置`, `${backup} · ${outcome.restart_hint ? "重启服务端后生效" : "服务端下次启动时生效"}`),
        action: outcome.restart_hint ? { label: "立即重启服务端", onClick: () => run("重启服务端", () => voiceApi.serverRestart()) } : undefined,
      });
    } catch (error) {
      setResult({ ok: false, text: `保存失败：${voiceErrorText(error)}` });
    } finally {
      setBusy("");
    }
  };

  const blocked = fullModule ? "全模块部署由智控台管理，这里只能测试" : saveEnabled ? "" : "先测试连接，通过后才能保存";
  return (
    <VoiceGroup id={`external-model-${section}`} title={current.label} summary={externalModelSummary(saved)} tone={saved?.ok ? "ok" : undefined} dirty={dirty}>
      <div className="voice-form-grid">
        <label className="voice-wide">
          <span>地址（{current.hint}）</span>
          <input className="text-input mono" value={form.url} onChange={(e) => update("url", e.target.value)} spellCheck="false" autoComplete="off" id={n("url")} />
        </label>
        <label>
          <span>密钥（不显示已保存的密钥；留空保留服务端原密钥）</span>
          <input className="text-input" type="password" value={form.api_key} onChange={(e) => update("api_key", e.target.value)} autoComplete="new-password" spellCheck="false" id={n("key")} />
        </label>
        <label>
          <span>模型</span>
          <input className="text-input mono" value={form.model} onChange={(e) => update("model", e.target.value)} spellCheck="false" id={n("name")} />
        </label>
        {section === "tts" && (
          <>
            <label>
              <span>音色</span>
              <input className="text-input" value={form.voice} onChange={(e) => update("voice", e.target.value)} spellCheck="false" id={n("voice")} />
            </label>
            <label>
              <span>语速（0.25 ~ 4.0）</span>
              <input className="text-input" inputMode="decimal" value={form.speed} onChange={(e) => update("speed", e.target.value)} />
            </label>
          </>
        )}
      </div>
      <div className="voice-final mono" id={n("final")}>
        写入地址：{preview || "—"}
      </div>
      {test ? (
        <div className={`voice-hint voice-hint--${test.ok ? "ok" : "bad"}`} id={n("result")}>
          {test.ok ? "测试通过" : "测试未通过"}
          {typeof test.elapsed_ms === "number" ? ` · ${(test.elapsed_ms / 1000).toFixed(1)} s` : ""} · {test.detail}
          {test.ok && test.fingerprint !== sectionFingerprint(section, form) && "（测试后内容已修改，请重新测试）"}
        </div>
      ) : saved ? (
        <p className="voice-muted">
          上次测试：{saved.ok ? "通过" : "未通过"} · {(saved.elapsed_ms / 1000).toFixed(1)} s · {formatTime(saved.at)}
        </p>
      ) : (
        <p className="voice-muted">测试只用这里填写的值发一次最小请求（15 秒超时），不写文件。</p>
      )}
      <SaveBar
        id={n("save")}
        target={`${current.label}到服务端配置`}
        dirty={dirty}
        busy={Boolean(busy)}
        saving={busy === `save-${section}`}
        blocked={blocked}
        result={result}
        onSave={save}
        extra={
          <button type="button" className="btn btn-ghost" onClick={runTest} disabled={Boolean(busy) || isSectionBlank(form)} id={n("test")}>
            <FlaskConical size={13} /> {busy === `test-${section}` ? "测试中…" : "测试连接"}
          </button>
        }
      />
    </VoiceGroup>
  );
}

export function VoicePage({ mcpStatus = null, approvalCount = 0 }) {
  const [status, setStatus] = useState(null);
  const [loadError, setLoadError] = useState("");
  const [busy, setBusy] = useState("");
  const [form, setForm] = useState(null);
  const [baseline, setBaseline] = useState(null);
  const [tab, setTab] = useState(readTab);
  const [dirty, setDirty] = useState({});
  const [logLines, setLogLines] = useState([]);
  const [approvals, setApprovals] = useState([]);
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

  const refreshLog = useCallback(async () => {
    try {
      const result = await voiceApi.serverLog(200);
      if (mounted.current) setLogLines(Array.isArray(result?.lines) ? result.lines : []);
    } catch (error) {
      if (mounted.current) setLogLines([`[读取日志失败] ${voiceErrorText(error)}`]);
    }
  }, []);

  /// 严格模式开关（技术书 §6.2）：写运行目录 `.env`；开启立即生效，关闭需重启 MOSS。
  const toggleStrict = async () => {
    if (!status?.strict) return;
    if (status.strict.locked) {
      await showAlert("自建链路必须开启严格模式，不能在这里关闭。", { title: "不可关闭", tone: "warning" });
      return;
    }
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
    const res = await getJson("/v1/approvals?status=all");
    if (mounted.current && res.ok && res.data?.success) {
      setApprovals(Array.isArray(res.data.result?.approvals) ? res.data.result.approvals : []);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    refresh();
    refreshLog();
    refreshApprovals();
    const timer = setInterval(() => {
      refresh();
      refreshApprovals();
    }, VOICE_POLL_MS);
    return () => {
      mounted.current = false;
      clearInterval(timer);
    };
  }, [refresh, refreshLog, refreshApprovals]);

  const run = useCallback(
    async (label, action, { quiet = false } = {}) => {
      setBusy(label);
      try {
        const result = await action();
        if (result && result.mode && result.server) setStatus(result);
        else if (result?.status?.mode) setStatus(result.status);
        return result;
      } catch (error) {
        if (!quiet) await showAlert(voiceErrorText(error), { title: `${label}失败`, tone: "error" });
        return null;
      } finally {
        if (mounted.current) setBusy("");
        refreshLog();
      }
    },
    [refreshLog],
  );

  const mode = status?.mode || "official";
  // 当前链路要显示的标签：官方链路没有“模型”标签；记住的标签若当前链路没有，回退到第一个
  const tabs = voiceTabsFor(mode);
  const activeTab = tabs.some((item) => item.key === tab) ? tab : tabs[0].key;
  const server = serverStateView(status?.server);
  const device = deviceView(mode, status?.device);
  const fullModule = Boolean(status?.server_config?.full_module);

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
    const result = await run("切换链路", () => voiceApi.setMode(target));
    if (result?.server_still_running) {
      const stop = await showConfirm("MOSS 启动的自建服务端仍在运行。官方链路不需要它，是否停止？", {
        title: "停止自建服务端？",
        tone: "warning",
        confirmText: "停止服务端",
        cancelText: "保持运行",
      });
      if (stop) await run("停止服务端", () => voiceApi.serverStop());
    }
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

  const endpointHealth = status.endpoint?.health;
  const builtinStatus = builtin.status && !builtin.status.error ? builtin.status : null;
  const builtinSessions = Array.isArray(builtinStatus?.sessions) ? builtinStatus.sessions.length : 0;
  const unsaved = dirtyTabs(dirty);
  const pendingApprovals = approvalCount || approvals.filter((item) => item?.status === "pending").length;
  const serviceView = {
    builtin: builtinStatus?.running ? { text: "内置服务运行中", tone: "ok" } : { text: "内置服务未运行", tone: "off" },
    self_hosted: { text: `服务端${server.text}`, tone: server.tone },
    official: status.bridge?.connected ? { text: "小智桥已连接", tone: "ok" } : { text: "小智桥未连接", tone: "bad" },
  }[mode];
  const deviceLine = mode === "builtin" ? { text: `${builtinSessions} 台设备在线`, tone: builtinSessions ? "ok" : "warn" } : { text: `设备：${device.text}`, tone: device.tone };

  const groupProps = { mode, status, form, baseline, onChange: changeSetting, onSaved: settingsSaved, busy, setBusy };
  const modelProps = { status, fullModule, busy, setBusy, refresh, run };

  const serverBlock = (
    <div className="voice-block" id="voice-server-card">
      <div className="voice-block-h">
        <h3>自建服务端</h3>
        <Pill tone={server.tone} id="voice-server-state">
          {server.text}
        </Pill>
        <span className="voice-actions voice-push">
          {server.canStart && (
            <button type="button" className="btn btn-primary" onClick={() => run("启动服务端", () => voiceApi.serverStart())} disabled={Boolean(busy)} id="voice-server-start">
              <Play size={13} /> 启动
            </button>
          )}
          {server.canStop && (
            <button type="button" className="btn btn-danger" onClick={() => run("停止服务端", () => voiceApi.serverStop())} disabled={Boolean(busy)} id="voice-server-stop">
              <Square size={13} /> 停止
            </button>
          )}
          {server.canRestart && (
            <button type="button" className="btn btn-ghost" onClick={() => run("重启服务端", () => voiceApi.serverRestart())} disabled={Boolean(busy)} id="voice-server-restart">
              <RotateCcw size={13} /> 重启
            </button>
          )}
        </span>
      </div>
      <div className="voice-kv">
        {status.server?.pid && <span>进程 {status.server.pid}</span>}
        {status.server?.started_at && <span>启动于 {formatTime(status.server.started_at)}</span>}
        {server.state === "exited" && <span className="voice-bad">退出码 {status.server.exit_code ?? "未知"}，不会自动重启（日志见“记录与诊断”）</span>}
        {status.server?.message && server.state !== "exited" && <span>{status.server.message}</span>}
        {!status.server_config?.exists && <span className="voice-warn">缺少 data/.config.yaml，请先在“模型”标签完成配置</span>}
        {!status.mcp_endpoint && (<span className="voice-warn">尚未填写自建接入点：在下面“自建接入点”组填好后保存，MOSS 才会连接</span>)}
        <span>
          设备：<Pill tone={device.tone} id="voice-device-state">{device.text}</Pill>
        </span>
        {endpointHealth?.error ? (
          <span className="voice-warn">接入点健康检查：{endpointHealth.error}</span>
        ) : endpointHealth ? (
          <span>
            工具连接 {endpointHealth.tool_connections ?? "—"} · 设备会话 {endpointHealth.robot_connections ?? "—"}
          </span>
        ) : (
        <span className="voice-muted">未填写接入点健康检查地址，无法判断设备是否在线（在“服务”标签填写）</span>
        )}
      </div>
    </div>
  );

  const officialBlock = (
    <div className="voice-block" id="voice-endpoint-card">
      <div className="voice-block-h">
        <h3>小智接入点</h3>
        <Pill tone={status.bridge?.connected ? "ok" : "bad"}>{status.bridge?.connected ? "MOSS 桥已连接" : "MOSS 桥未连接"}</Pill>
      </div>
      <div className="voice-kv">
        <span className="mono voice-break">{status.mcp_endpoint || "未设置接入点（在“服务”标签填写）"}</span>
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
              { key: "self_hosted", desc: "设备连自建 xiaozhi-server，运动与出光需桌面审批" },
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
              title={status.strict?.locked ? "自建模式下必须开启，不可关闭" : status.strict?.from_env ? "当前由 .env 开启；点这里可以切换" : "点这里切换（会自动写入运行目录的 .env）"}
            >
              严格模式{status.strict?.enabled ? "已开启" : "未开启"} · 点此切换
            </button>
          </div>
          <MossVoiceTwin
            mode={mode}
            status={status}
            builtinStatus={builtinStatus}
            mcpStatus={mcpStatus}
            logLines={logLines}
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
                  key: "selfHosted",
                  modes: ["self_hosted"],
                  node: (
                    <div className="voice-stack">
                      {serverBlock}
                      <SettingsGroup group="selfHostedServer" {...groupProps} />
                      <SettingsGroup group="selfHostedEndpoint" {...groupProps} />
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
                    <div className="voice-stack">
                      <h3 className="voice-section-h">内置服务 · 云端模型（密钥存本机）</h3>
                      <BuiltinModelGroups builtin={builtin} />
                      {builtinStatus && (
                        <>
                          <h3 className="voice-section-h">内置服务 · 本地模型与体验</h3>
                          <LocalModelsGroup builtin={builtin} />
                          <ExperienceGroup builtin={builtin} />
                        </>
                      )}
                    </div>
                  ),
                },
                {
                  key: "external",
                  modes: ["self_hosted"],
                  node: (
                    <div className="voice-stack" id="voice-models-card">
                      <h3 className="voice-section-h">自建服务端 · 模型（写入 data/.config.yaml）</h3>
                      {fullModule && <div className="voice-hint voice-hint--warn">检测到全模块部署（manager-api.url）：模型由智控台管理，这里只能测试连接，不会写文件。</div>}
                      {SECTIONS.map((item) => (
                        <ExternalModelGroup key={item.key} section={item.key} {...modelProps} />
                      ))}
                    </div>
                  ),
                },
              ]}
            />
          </VoiceTabPanel>

          <VoiceTabPanel tab="devices" active={activeTab === "devices"}>
            <div className="voice-stack" id="voice-devices-panel">
              <VoiceEndpointsCard mode={mode} overview={status.mcp} busy={busy} run={run} />
            </div>
          </VoiceTabPanel>

          <VoiceTabPanel tab="wake" active={activeTab === "wake"}>
            <VoiceWakeVolumePanel builtin={builtin} mode={mode} />
          </VoiceTabPanel>

          <VoiceTabPanel tab="features" active={activeTab === "features"}>
            <div className="voice-stack">
              {mode === "builtin" && (
                <>
                  <h3 className="voice-section-h">内置服务能力（只在“MOSS 内置服务”链路生效）</h3>
                  <VoiceMusicCard />
                  <VoiceVisionCard />
                  <VoiceKnowledgeCard />
                </>
              )}
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
                  modes: ["official", "self_hosted", "builtin"],
                  node: (
                    <VoiceGroup id="approvals" title="审批记录" summary={approvals.length ? `${approvals.length} 条` : "暂无"} defaultOpen>
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
                                return (
                                  <tr key={record.approval_id}>
                                    <td className="mono">{formatTime(record.created_at)}</td>
                                    <td>
                                      {kindLabel(record.kind)} · {record.summary || "—"}
                                    </td>
                                    <td>{sourceLabel(record.source, record.session_id)}</td>
                                    <td>
                                      <Pill tone={view.tone}>{view.text}</Pill>
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
                { key: "debug-traces", modes: ["builtin"], node: <LiveDebugTraces builtin={builtin} /> },
                {
                  key: "log",
                  modes: ["self_hosted"],
                  node: (
                    <VoiceGroup id="server-log" title="自建服务端日志" summary={logLines.length ? `${logLines.length} 行（已脱敏）` : "暂无"}>
                      <div className="voice-actions">
                        <button type="button" className="btn btn-ghost" onClick={refreshLog} id="voice-log-refresh">
                          <RefreshCw size={13} /> 刷新
                        </button>
                        <span className="voice-muted">api_key、Authorization、token 等已替换为 ***</span>
                      </div>
                      <pre className="voice-log mono" id="voice-log">
                        {logLines.length ? logLines.map(maskLogLine).join("\n") : "暂无日志"}
                      </pre>
                    </VoiceGroup>
                  ),
                },
                {
                  key: "maintenance",
                  modes: ["official", "self_hosted", "builtin"],
                  node: (
                    <VoiceGroup id="device-maintenance" title="设备维护（固件升级）" summary="只在本机安全窗口逐台操作">
                      <PrivateFirmwareEntry />
                    </VoiceGroup>
                  ),
                },
              ]}
            />
          </VoiceTabPanel>
        </section>
      </div>
    </VoiceDirtyProvider>
  );
}
