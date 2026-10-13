import React, { useCallback, useEffect, useRef, useState } from "react";
import {
  Radio,
  Server,
  Cable,
  Smartphone,
  ShieldCheck,
  Play,
  Square,
  RotateCcw,
  FlaskConical,
  Save,
  RefreshCw,
  ScrollText,
  ListChecks,
  FolderCog,
} from "lucide-react";
import { getJson } from "../api/client.js";
import { showAlert, showConfirm } from "../appDialog.js";
import { kindLabel, sourceLabel } from "../approvalState.js";
import { voiceApi } from "../voiceApi.js";
import { VoiceBuiltinCard } from "../components/VoiceBuiltinCard.jsx";
import { VoiceMcpCard } from "../components/VoiceMcpCard.jsx";
import { VoiceEndpointsCard } from "../components/VoiceEndpointsCard.jsx";
import {
  SECTIONS,
  VOICE_POLL_MS,
  approvalHistoryStatus,
  canSaveSection,
  deviceView,
  emptyForm,
  finalUrl,
  isSectionBlank,
  maskLogLine,
  modeConfirmText,
  sectionFingerprint,
  sectionInputError,
  sectionPayload,
  serverStateView,
  voiceErrorText,
} from "../voiceState.js";

// 语音中心（技术书 §9.4 / §9.5.1，实施清单 P4-08）。只在 MOSS 桌面端显示（A-69）。
// 所有操作经 Tauri 命令执行；模型密钥只在本页内存中，提交后清空，不回显已保存的密钥（A-67）。

const TONE_CLASS = { ok: "ob-pill--ok", info: "ob-pill--info", warn: "ob-pill--warn", bad: "ob-pill--bad", off: "ob-pill--off" };

function Pill({ tone = "off", children, id }) {
  return (
    <span className={`ob-pill ${TONE_CLASS[tone] || TONE_CLASS.off}`} id={id}>
      {children}
    </span>
  );
}

function formatTime(seconds) {
  if (!seconds) return "—";
  const date = new Date(seconds * 1000);
  return date.toLocaleString("zh-CN", { hour12: false, month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit" });
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

export function VoicePage() {
  const [status, setStatus] = useState(null);
  const [loadError, setLoadError] = useState("");
  const [busy, setBusy] = useState("");
  const [form, setForm] = useState(null);
  const [tab, setTab] = useState("llm");
  const [models, setModels] = useState(() => ({ llm: emptyForm("llm"), asr: emptyForm("asr"), tts: emptyForm("tts") }));
  const [tests, setTests] = useState({});
  const [saveNote, setSaveNote] = useState("");
  const [logLines, setLogLines] = useState([]);
  const [approvals, setApprovals] = useState([]);
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    try {
      const next = await voiceApi.status();
      if (!mounted.current) return;
      setStatus(next);
      setLoadError("");
      setForm((current) => current || settingsForm(next));
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
  const server = serverStateView(status?.server);
  const device = deviceView(mode, status?.device);
  const fullModule = Boolean(status?.server_config?.full_module);

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

  const saveSettings = async () => {
    if (!form) return;
    const port = Number(form.server_port);
    const builtinWs = Number(form.builtin_ws_port);
    const builtinHttp = Number(form.builtin_http_port);
    const payload = {
      server_dir: form.server_dir,
      python: form.python,
      args: form.args,
      server_port: Number.isFinite(port) ? port : 0,
      builtin_ws_port: Number.isFinite(builtinWs) ? builtinWs : 0,
      builtin_http_port: Number.isFinite(builtinHttp) ? builtinHttp : 0,
    };
    for (const key of ["self_hosted_mcp_endpoint", "endpoint_health_url", "official_mcp_endpoint"]) {
      if (form[key].trim()) payload[key] = form[key].trim();
    }
    const result = await run("保存设置", () => voiceApi.saveSettings(payload));
    if (result) {
      setForm(settingsForm(result));
      setSaveNote("服务端设置已保存");
      setTimeout(() => mounted.current && setSaveNote(""), 4000);
    }
  };

  const updateModel = (section, key, value) => {
    setModels((current) => ({ ...current, [section]: { ...current[section], [key]: value } }));
  };

  const testSection = async (section) => {
    const formValue = models[section];
    const inputError = sectionInputError(section, formValue);
    if (inputError) {
      setTests((current) => ({ ...current, [section]: { ok: false, detail: inputError, fingerprint: "" } }));
      return;
    }
    const fingerprint = sectionFingerprint(section, formValue);
    setBusy(`test-${section}`);
    try {
      const result = await voiceApi.modelsTest(section, sectionPayload(section, formValue));
      if (mounted.current) setTests((current) => ({ ...current, [section]: { ...result, fingerprint } }));
    } catch (error) {
      if (mounted.current) setTests((current) => ({ ...current, [section]: { ok: false, detail: voiceErrorText(error), fingerprint: "" } }));
    } finally {
      if (mounted.current) setBusy("");
      refresh();
    }
  };

  const saveSection = async (section) => {
    const result = await run("保存模型配置", () => voiceApi.modelsSave({ [section]: sectionPayload(section, models[section]) }));
    if (!result) return;
    // 密钥只在提交时使用：保存后清空输入框，之后也不回显
    setModels((current) => ({ ...current, [section]: { ...current[section], api_key: "" } }));
    setTests((current) => ({ ...current, [section]: { ...current[section], fingerprint: "" } }));
    if (result.restart_hint) {
      const restart = await showConfirm("已写入服务端配置（原文件已备份为 .config.yaml.bak）。需要重启服务端才能生效，现在重启？", {
        title: "保存成功",
        tone: "info",
        confirmText: "重启服务端",
        cancelText: "稍后",
      });
      if (restart) await run("重启服务端", () => voiceApi.serverRestart());
    } else {
      await showAlert(
        `已写入服务端配置${result.backup ? "（原文件已备份为 .config.yaml.bak）" : "（新建 data/.config.yaml）"}。服务端下次启动时生效。`,
        { title: "保存成功", tone: "success" },
      );
    }
  };

  if (!status) {
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

  const currentTab = SECTIONS.find((item) => item.key === tab) || SECTIONS[0];
  const currentForm = models[tab];
  const currentTest = tests[tab];
  const savedTest = status.models?.[tab];
  const preview = finalUrl(tab, currentForm.url);
  const saveEnabled = canSaveSection(tab, currentForm, currentTest, { fullModule, busy: Boolean(busy) });
  const endpointHealth = status.endpoint?.health;

  return (
    <div className="ob-page voice-page">
      {/* 1. 语音链路 */}
      <section className="ob-card" id="voice-mode-card">
        <div className="ob-card-h">
          <h2>
            <span className="ob-n">1</span> 语音链路
          </h2>
          <span className="ob-k">VOICE LINK</span>
        </div>
        <div className="voice-mode-row" role="radiogroup" aria-label="语音链路">
          {[
            { key: "official", title: "官方小智", desc: "设备连官方服务器；MOSS 作为工具接入官方接入点" },
            { key: "self_hosted", title: "自建服务端", desc: "设备连自建 xiaozhi-server；严格模式必开，运动与出光需桌面审批" },
            { key: "builtin", title: "MOSS 内置服务", desc: "设备直连 MOSS，不需要外部服务端；随 MOSS 启动，加工确认规则与官方链路相同" },
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
                <b>{item.title}</b>
                <small>{item.desc}</small>
              </span>
            </button>
          ))}
        </div>
        <div className={`voice-hint voice-hint--${device.tone}`} id="voice-ota-hint">
          设备 OTA 一致性：{device.hint}
        </div>
        {mode === "official" && !status.strict?.enabled && (
          <div className="voice-hint voice-hint--warn">
            官方链路下严格模式未开启：大模型可以自行确认发送加工任务。建议在 .env 中设置 MOSS_FUSION_STRICT=1，使运动与出光都需要在本机批准。
          </div>
        )}
      </section>

      {/* 内置语音服务（NV1 起；NV5 起配齐模型后为对话模式） */}
      <VoiceBuiltinCard />
      <VoiceMcpCard />

      {/* 2. 四张状态卡 */}
      <div className="voice-status-grid">
        <section className="ob-card voice-stat" id="voice-server-card">
          <div className="ob-card-h">
            <h2>
              <Server size={16} aria-hidden="true" /> 服务端
            </h2>
            <Pill tone={server.tone} id="voice-server-state">{server.text}</Pill>
          </div>
          <div className="voice-kv">
            {status.server?.pid && <span>进程 {status.server.pid}</span>}
            {status.server?.started_at && <span>启动于 {formatTime(status.server.started_at)}</span>}
            {server.state === "exited" && <span className="voice-bad">退出码 {status.server.exit_code ?? "未知"}，不会自动重启</span>}
            {status.server?.message && server.state !== "exited" && <span>{status.server.message}</span>}
            {!status.server_config?.exists && <span className="voice-warn">缺少 data/.config.yaml，请先完成模型配置</span>}
          </div>
          <div className="voice-actions">
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
            <a className="btn btn-ghost" href="#voice-log-card" onClick={refreshLog}>
              <ScrollText size={13} /> 日志
            </a>
          </div>
        </section>

        <section className="ob-card voice-stat" id="voice-endpoint-card">
          <div className="ob-card-h">
            <h2>
              <Cable size={16} aria-hidden="true" /> MCP 接入点
            </h2>
            <Pill tone={status.bridge?.connected ? "ok" : "bad"}>{status.bridge?.connected ? "MOSS 桥已连接" : "MOSS 桥未连接"}</Pill>
          </div>
          <div className="voice-kv">
            <span className="mono voice-break">{status.mcp_endpoint || "未设置 mcp_endpoint"}</span>
            {endpointHealth ? (
              endpointHealth.error ? (
                <span className="voice-warn">健康检查：{endpointHealth.error}</span>
              ) : (
                <span>
                  工具连接 {endpointHealth.tool_connections ?? "—"} · 设备会话 {endpointHealth.robot_connections ?? "—"}
                </span>
              )
            ) : (
              <span className="voice-muted">
                {mode === "self_hosted" ? "未填写健康检查地址，设备会话未知" : mode === "builtin" ? "内置链路的设备连接见“内置语音服务”卡片" : "官方接入点不提供会话数"}
              </span>
            )}
          </div>
        </section>

        <section className="ob-card voice-stat" id="voice-device-card">
          <div className="ob-card-h">
            <h2>
              <Smartphone size={16} aria-hidden="true" /> 设备
            </h2>
            <Pill tone={device.tone} id="voice-device-state">{device.text}</Pill>
          </div>
          <div className="voice-kv">
            <span>
              {mode === "self_hosted" ? "依据接入点的设备会话（robot_connections）判断" : mode === "builtin" ? "内置链路：设备连接由内置语音服务显示" : "官方链路无法得知设备会话"}
            </span>
          </div>
        </section>

        <section className="ob-card voice-stat" id="voice-strict-card">
          <div className="ob-card-h">
            <h2>
              <ShieldCheck size={16} aria-hidden="true" /> 严格模式
            </h2>
            <Pill tone={status.strict?.enabled ? "ok" : "warn"} id="voice-strict-state">{status.strict?.enabled ? "已开启" : "未开启"}</Pill>
          </div>
          <div className="voice-kv">
            <span>{status.strict?.locked ? "自建模式下必须开启，不可关闭" : status.strict?.from_env ? "由 .env 的 MOSS_FUSION_STRICT 开启" : "官方链路下可选（在 .env 设置）"}</span>
            <span className="voice-muted">网关：{status.gateway?.enabled ? "已开启" : "未启用（可选 P2b）"}</span>
          </div>
        </section>
      </div>

      {/* 3. 服务端设置 */}
      <section className="ob-card" id="voice-settings-card">
        <div className="ob-card-h">
          <h2>
            <span className="ob-n">2</span> 服务端与接入点设置
          </h2>
          <span className="ob-k">SETTINGS</span>
        </div>
        {form && (
          <div className="voice-form-grid">
            <label>
              <span>xiaozhi-server 目录（main/xiaozhi-server）</span>
              <input className="text-input" value={form.server_dir} onChange={(e) => setForm({ ...form, server_dir: e.target.value })} spellCheck="false" id="voice-server-dir" />
            </label>
            <label>
              <span>Python 3.10 解释器路径</span>
              <input className="text-input" value={form.python} onChange={(e) => setForm({ ...form, python: e.target.value })} spellCheck="false" id="voice-python" />
            </label>
            <label>
              <span>启动参数</span>
              <input className="text-input" value={form.args} onChange={(e) => setForm({ ...form, args: e.target.value })} spellCheck="false" />
            </label>
            <label>
              <span>服务端端口（识别外部实例）</span>
              <input className="text-input" inputMode="numeric" value={form.server_port} onChange={(e) => setForm({ ...form, server_port: e.target.value })} />
            </label>
            <label>
              <span>内置服务设备连接端口（默认 8000，修改后重启内置服务生效）</span>
              <input className="text-input" inputMode="numeric" value={form.builtin_ws_port} onChange={(e) => setForm({ ...form, builtin_ws_port: e.target.value })} id="voice-builtin-ws-port" />
            </label>
            <label>
              <span>内置服务 OTA 端口（默认 8003）</span>
              <input className="text-input" inputMode="numeric" value={form.builtin_http_port} onChange={(e) => setForm({ ...form, builtin_http_port: e.target.value })} id="voice-builtin-http-port" />
            </label>
            <label className="voice-wide">
              <span>自建接入点（单模块 MCP 接入点 ws://…/mcp_endpoint/mcp/?token=…）{status.settings?.self_hosted_mcp_endpoint ? `；当前：${status.settings.self_hosted_mcp_endpoint}` : ""}</span>
              <input className="text-input mono" value={form.self_hosted_mcp_endpoint} onChange={(e) => setForm({ ...form, self_hosted_mcp_endpoint: e.target.value })} placeholder={status.settings?.self_hosted_mcp_endpoint_set ? "留空表示不改" : "ws://<接入点地址>:8004/mcp_endpoint/mcp/?token=…"} spellCheck="false" autoComplete="off" id="voice-self-endpoint" />
            </label>
            <label className="voice-wide">
              <span>接入点健康检查地址（用于判断设备在线，可不填）{status.settings?.endpoint_health_url ? `；当前：${status.settings.endpoint_health_url}` : ""}</span>
              <input className="text-input mono" value={form.endpoint_health_url} onChange={(e) => setForm({ ...form, endpoint_health_url: e.target.value })} placeholder={status.settings?.endpoint_health_url_set ? "留空表示不改" : "http://<接入点地址>:8004/mcp_endpoint/health?key=…"} spellCheck="false" autoComplete="off" />
            </label>
            <label className="voice-wide">
              <span>
                官方接入点（小智控制台生成的 wss:// 地址）{status.settings?.official_mcp_endpoint ? `；${mode === "official" ? "当前" : "切回官方时恢复"}：${status.settings.official_mcp_endpoint}` : ""}
              </span>
              <input className="text-input mono" value={form.official_mcp_endpoint} onChange={(e) => setForm({ ...form, official_mcp_endpoint: e.target.value })} placeholder={status.settings?.official_mcp_endpoint_set ? "留空表示不改" : "wss://…/mcp/?token=…"} spellCheck="false" autoComplete="off" id="voice-official-endpoint" />
            </label>
          </div>
        )}
        <div className="voice-actions">
          <button type="button" className="btn btn-hud-primary" onClick={saveSettings} disabled={Boolean(busy)} id="voice-settings-save">
            <FolderCog size={13} /> 保存设置
          </button>
          {saveNote && <span className="voice-ok">{saveNote}</span>}
          <span className="voice-muted">官方模式下修改官方接入点会立即重连小智桥。</span>
        </div>
      </section>

      {/* 接入点与工具（NV8）：附加接入点、连接状态与身份、工具开关与调用统计 */}
      <VoiceEndpointsCard overview={status.mcp} busy={busy} run={run} />

      {/* 4. 模型配置向导 */}
      <section className="ob-card" id="voice-models-card">
        <div className="ob-card-h">
          <h2>
            <span className="ob-n">3</span> 模型配置
          </h2>
          <span className="ob-k">MODELS</span>
        </div>
        {fullModule && (
          <div className="voice-hint voice-hint--warn">检测到全模块部署（manager-api.url）：模型配置由智控台管理，这里只能测试连接，不会写文件。</div>
        )}
        <div className="voice-tabs" role="tablist">
          {SECTIONS.map((item) => {
            const record = status.models?.[item.key];
            return (
              <button key={item.key} type="button" role="tab" aria-selected={tab === item.key} className={`voice-tab${tab === item.key ? " on" : ""}`} onClick={() => setTab(item.key)} id={`voice-tab-${item.key}`}>
                {item.label}
                <span className={record ? (record.ok ? "voice-ok" : "voice-bad") : "voice-muted"}>{record ? (record.ok ? " ✓" : " ✗") : ""}</span>
              </button>
            );
          })}
        </div>
        <div className="voice-form-grid">
          <label className="voice-wide">
            <span>地址（{currentTab.hint}）</span>
            <input className="text-input mono" value={currentForm.url} onChange={(e) => updateModel(tab, "url", e.target.value)} spellCheck="false" autoComplete="off" id="voice-model-url" />
          </label>
          <label>
            <span>密钥（不回显已保存的密钥；留空则保留服务端配置中的原密钥）</span>
            <input className="text-input" type="password" value={currentForm.api_key} onChange={(e) => updateModel(tab, "api_key", e.target.value)} autoComplete="new-password" spellCheck="false" id="voice-model-key" />
          </label>
          <label>
            <span>模型</span>
            <input className="text-input mono" value={currentForm.model} onChange={(e) => updateModel(tab, "model", e.target.value)} spellCheck="false" id="voice-model-name" />
          </label>
          {tab === "tts" && (
            <>
              <label>
                <span>音色</span>
                <input className="text-input" value={currentForm.voice} onChange={(e) => updateModel(tab, "voice", e.target.value)} spellCheck="false" id="voice-model-voice" />
              </label>
              <label>
                <span>语速（0.25 ~ 4.0）</span>
                <input className="text-input" inputMode="decimal" value={currentForm.speed} onChange={(e) => updateModel(tab, "speed", e.target.value)} />
              </label>
            </>
          )}
        </div>
        <div className="voice-final mono" id="voice-model-final">
          写入地址：{preview || "—"}
        </div>
        <div className="voice-actions">
          <button type="button" className="btn btn-ghost" onClick={() => testSection(tab)} disabled={Boolean(busy) || isSectionBlank(currentForm)} id="voice-model-test">
            <FlaskConical size={13} /> {busy === `test-${tab}` ? "测试中…" : "测试连接"}
          </button>
          <button type="button" className="btn btn-primary" onClick={() => saveSection(tab)} disabled={!saveEnabled} id="voice-model-save" title={saveEnabled ? "写入服务端 data/.config.yaml" : "先用当前填写的内容测试连接并成功后才能保存"}>
            <Save size={13} /> 保存到服务端配置
          </button>
        </div>
        {currentTest ? (
          <div className={`voice-hint voice-hint--${currentTest.ok ? "ok" : "bad"}`} id="voice-model-result">
            {currentTest.ok ? "测试通过" : "测试未通过"}
            {typeof currentTest.elapsed_ms === "number" ? ` · ${(currentTest.elapsed_ms / 1000).toFixed(1)} s` : ""} · {currentTest.detail}
            {currentTest.ok && currentTest.fingerprint !== sectionFingerprint(tab, currentForm) && "（测试后内容已修改，请重新测试）"}
          </div>
        ) : savedTest ? (
          <div className="voice-muted">
            上次测试：{savedTest.ok ? "通过" : "未通过"} · {(savedTest.elapsed_ms / 1000).toFixed(1)} s · {formatTime(savedTest.at)}
          </div>
        ) : (
          <div className="voice-muted">未测试。测试只用本页填写的值发一次最小请求（15 秒超时、不跟随重定向），不落盘。</div>
        )}
      </section>

      {/* 5. 审批记录 */}
      <section className="ob-card" id="voice-approvals-card">
        <div className="ob-card-h">
          <h2>
            <ListChecks size={16} aria-hidden="true" /> 审批记录
          </h2>
          <span className="ob-k">APPROVALS</span>
        </div>
        <div className="ob-tbl voice-tbl">
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
      </section>

      {/* 6. 服务端日志 */}
      <section className="ob-card" id="voice-log-card">
        <div className="ob-card-h">
          <h2>
            <ScrollText size={16} aria-hidden="true" /> 服务端日志
          </h2>
          <span className="voice-muted">已脱敏（api_key、Authorization、token 等替换为 ***）</span>
          <button type="button" className="btn btn-ghost voice-push" onClick={refreshLog} id="voice-log-refresh">
            <RefreshCw size={13} /> 刷新
          </button>
        </div>
        <pre className="voice-log mono" id="voice-log">
          {logLines.length ? logLines.map(maskLogLine).join("\n") : "暂无日志"}
        </pre>
      </section>
    </div>
  );
}
