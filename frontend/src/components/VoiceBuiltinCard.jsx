import React, { useCallback, useEffect, useRef, useState } from "react";
import { Play, Square, ShieldCheck, ShieldOff, Pencil, Check, X, FlaskConical, Trash2, Download, Lock, ChevronDown, Copy } from "lucide-react";
import { showAlert, showConfirm } from "../appDialog.js";
import { voiceApi } from "../voiceApi.js";
import { SaveBar, VoiceGroup } from "./VoiceLayout.jsx";
import VoiceMemoryCard from "./VoiceMemoryCard.jsx";
import VoicePluginsCard from "./VoicePluginsCard.jsx";
import VoiceSpeakerCard from "./VoiceSpeakerCard.jsx";
import VoiceHomeCard from "./VoiceHomeCard.jsx";
import VoiceHomeApprovalCard from "./VoiceHomeApprovalCard.jsx";
import VoiceContextCard from "./VoiceContextCard.jsx";
import {
  EXPERIENCE_KEYS,
  LOCAL_MODEL_KEYS,
  SECTIONS,
  VOICE_POLL_MS,
  allowDeviceConfirmText,
  builtinDeviceView,
  builtinKeyPlaceholder,
  builtinModeView,
  builtinModelForm,
  builtinModelSummary,
  canSaveSection,
  experienceSettingsError,
  finalUrl,
  formDirty,
  formatBytes,
  isSectionBlank,
  localModelGroups,
  localModelSettingsError,
  localModelView,
  localSettingsForm,
  localSettingsPayload,
  mergeGroup,
  savedNoteText,
  selectLocalModel,
  selectedVoices,
  isDoubao,
  sectionFingerprint,
  sectionInputError,
  sectionPayload,
  switchKind,
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
  const [status, setStatus] = useState(null);
  const [busy, setBusy] = useState("");
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    try {
      const next = await voiceApi.builtinStatus();
      if (mounted.current) setStatus(next);
    } catch (error) {
      // 读过一次后保留上次状态，只标出刷新失败；不卸载各分组，避免丢失未保存的填写
      if (mounted.current) setStatus((prev) => (prev && !prev.error ? { ...prev, poll_error: voiceErrorText(error) } : { error: voiceErrorText(error) }));
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    refresh();
    const timer = setInterval(refresh, VOICE_POLL_MS);
    return () => {
      mounted.current = false;
      clearInterval(timer);
    };
  }, [refresh]);

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
  }, []);

  return { status, setStatus, busy, setBusy, act };
}

/** 服务与设备：运行状态、启停、设备要填的 OTA 地址、设备允许列表。 */
export function BuiltinServicePanel({ builtin }) {
  const { status, busy, act } = builtin;
  const [editing, setEditing] = useState({ id: "", name: "" });
  if (!status) return <p className="voice-muted">正在读取内置服务状态…</p>;
  if (status.error) return <div className="voice-hint voice-hint--bad">{status.error}</div>;

  const running = Boolean(status.running);
  const devices = Array.isArray(status.devices) ? status.devices : [];
  const sessions = Array.isArray(status.sessions) ? status.sessions : [];
  const modeView = builtinModeView(status);

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

  const copyOta = async () => {
    if (!status.ota_url) return;
    try {
      await navigator.clipboard.writeText(status.ota_url);
      await showAlert("设备 OTA 地址已复制，粘贴到设备配网页面即可", { title: "已复制", tone: "success" });
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
          设备 OTA 地址：<b className="mono voice-break" id="voice-builtin-ota">{status.ota_url || "未能获取本机局域网地址"}</b>
          {status.ota_url && (
            <button type="button" className="btn btn-ghost voice-inline-btn" onClick={copyOta} id="voice-builtin-ota-copy" title="复制设备 OTA 地址">
              <Copy size={13} /> 复制
            </button>
          )}
        </span>
        <span className="voice-muted">
          在设备配网页面填入这个地址并重启设备，再在下面的列表中点“允许”。端口 {status.ws_port} / {status.http_port}，在“连接”标签修改。
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
                const online = sessions.some((session) => session.device_id === device.device_id);
                return (
                  <tr key={device.device_id}>
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
                      </span>
                    </td>
                  </tr>
                );
              })
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

/** 内置服务的一项云端模型（大模型 / 语音识别 / 语音合成）：先测试、后保存；密钥只提交、不回显。 */
function BuiltinModelGroup({ section, builtin }) {
  const { status, setStatus, busy, setBusy } = builtin;
  const current = SECTIONS.find((item) => item.key === section);
  const entry = status.models?.[section];
  const baseline = builtinModelForm(section, entry);
  const [form, setForm] = useState(baseline);
  const [test, setTest] = useState(null);
  const [result, setResult] = useState(null);
  const dirty = formDirty(baseline, form);
  const saveEnabled = canSaveSection(section, form, test);
  const doubao = isDoubao(form);
  const localOn = section !== "llm" && Boolean(status.local?.settings?.[section]);
  const n = (suffix) => `voice-builtin-${suffix}-${section}`;

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
    setBusy(`builtin-test-${section}`);
    try {
      const outcome = await voiceApi.builtinModelsTest(section, sectionPayload(section, form));
      setTest({ ...outcome, fingerprint });
    } catch (error) {
      setTest({ ok: false, detail: voiceErrorText(error), fingerprint: "" });
    } finally {
      setBusy("");
    }
  };

  const save = async () => {
    setBusy(`builtin-save-${section}`);
    try {
      const next = await voiceApi.builtinModelsSave({ [section]: sectionPayload(section, form) });
      setStatus(next);
      setForm(builtinModelForm(section, next.models?.[section]));
      setTest(null);
      setResult({ ok: true, text: savedNoteText(`${current.label}设置`, `密钥存本机、不显示 · ${restartEffect(next)}`) });
    } catch (error) {
      setResult({ ok: false, text: `保存失败：${voiceErrorText(error)}` });
    } finally {
      setBusy("");
    }
  };

  const clear = async () => {
    const message = `删除内置服务的“${current.label}”设置与已保存的密钥？删除后内置服务回到回声测试模式。`;
    if (!(await showConfirm(message, { title: "删除设置？", tone: "warning", danger: true, confirmText: "删除" }))) return;
    setBusy(`builtin-clear-${section}`);
    try {
      const next = await voiceApi.builtinModelsSave({}, [section]);
      setStatus(next);
      setForm(builtinModelForm(section, next.models?.[section]));
      setTest(null);
      setResult({ ok: true, text: `已删除${current.label}设置与密钥 · ${restartEffect(next)}` });
    } catch (error) {
      setResult({ ok: false, text: `删除失败：${voiceErrorText(error)}` });
    } finally {
      setBusy("");
    }
  };

  return (
    <VoiceGroup
      id={`builtin-model-${section}`}
      title={current.label}
      summary={builtinModelSummary(section, entry, localOn)}
      tone={entry?.configured ? "ok" : undefined}
      dirty={dirty}
    >
      {localOn && <p className="voice-muted">已打开本地{current.label}：以本地模型为准，这里的云端设置作为备用。</p>}
      <div className="voice-form-grid">
        {section !== "llm" && (
          <label className="voice-wide">
            <span>接口类型</span>
            <select className="text-input" value={doubao ? "doubao" : "openai"} onChange={(e) => { setForm((all) => switchKind(section, all, e.target.value)); setResult(null); }} id={n("model-kind")}>
              <option value="openai">OpenAI 兼容（整段{section === "asr" ? "识别" : "合成"}）</option>
              <option value="doubao">豆包流式（{section === "asr" ? "边说边识别" : "边合成边播放"}，首句更快）</option>
            </select>
          </label>
        )}
        <label className="voice-wide">
          <span>地址（{doubao ? "豆包 WebSocket 地址，一般不用改" : current.hint}）</span>
          <input className="text-input mono" value={form.url} onChange={(e) => update("url", e.target.value)} spellCheck="false" autoComplete="off" id={n("model-url")} />
        </label>
        <label>
          <span>{doubao ? "API Key" : "密钥"}</span>
          <input className="text-input" type="password" value={form.api_key} placeholder={doubao && !entry?.key_set ? "控制台 API Key；旧版控制台填“APP ID:Access Token”" : builtinKeyPlaceholder(entry)} onChange={(e) => update("api_key", e.target.value)} autoComplete="new-password" spellCheck="false" id={n("model-key")} />
        </label>
        <label>
          <span>{doubao ? "资源 ID（留空用默认）" : "模型"}</span>
          <input className="text-input mono" value={form.model} onChange={(e) => update("model", e.target.value)} spellCheck="false" id={n("model-name")} />
        </label>
        {section === "tts" && (
          <>
            <label>
              <span>{doubao ? "音色 ID（见豆包控制台）" : "音色"}</span>
              <input className="text-input" value={form.voice} onChange={(e) => update("voice", e.target.value)} spellCheck="false" id={n("model-voice")} />
            </label>
            <label>
              <span>{doubao ? "语速（0.5 ~ 2.0）" : "语速（0.25 ~ 4.0）"}</span>
              <input className="text-input" inputMode="decimal" value={form.speed} onChange={(e) => update("speed", e.target.value)} />
            </label>
          </>
        )}
      </div>
      <div className="voice-final mono">保存的地址：{finalUrl(section, form.url, form.kind) || "—"}</div>
      {test ? (
        <div className={`voice-hint voice-hint--${test.ok ? "ok" : "bad"}`} id={n("model-result")}>
          {test.ok ? "测试通过" : "测试未通过"}
          {typeof test.elapsed_ms === "number" ? ` · ${(test.elapsed_ms / 1000).toFixed(1)} s` : ""} · {test.detail}
          {test.ok && test.fingerprint !== sectionFingerprint(section, form) && "（测试后内容已修改，请重新测试）"}
        </div>
      ) : (
        <p className="voice-muted">先测试连接，通过后才能保存。密钥留空时测试用已保存的密钥。</p>
      )}
      <SaveBar
        id={n("model-save")}
        target={`${current.label}设置`}
        dirty={dirty}
        busy={Boolean(busy)}
        saving={busy === `builtin-save-${section}`}
        blocked={saveEnabled ? "" : "先测试连接，通过后才能保存"}
        result={result}
        onSave={save}
        extra={
          <>
            <button type="button" className="btn btn-ghost" onClick={runTest} disabled={Boolean(busy) || isSectionBlank(form)} id={n("model-test")}>
              <FlaskConical size={13} /> {busy === `builtin-test-${section}` ? "测试中…" : "测试连接"}
            </button>
            {entry?.configured && (
              <button type="button" className="btn btn-ghost" onClick={clear} disabled={Boolean(busy)} id={n("model-clear")}>
                <Trash2 size={13} /> 删除此项
              </button>
            )}
          </>
        }
      />
    </VoiceGroup>
  );
}

/** 内置服务的三项云端模型，各自一个分组、各自保存。 */
export function BuiltinModelGroups({ builtin }) {
  if (!builtin.status || builtin.status.error) return <p className="voice-muted">{builtin.status?.error || "正在读取内置服务状态…"}</p>;
  return SECTIONS.map((item) => <BuiltinModelGroup key={item.key} section={item.key} builtin={builtin} />);
}

/**
 * 本地模型与语音体验共用同一份已保存设置，各自只提交自己的字段，也只检查自己的字段：
 * 另一组已保存的值即使不合前端规则，也不挡本组保存（由后端最终校验）。
 */
function useLocalGroup(builtin, keys, validate) {
  const local = builtin.status?.local || {};
  const models = Array.isArray(local.models) ? local.models : [];
  const baseline = localSettingsForm(local.settings);
  const [draft, setDraft] = useState(baseline);
  const [result, setResult] = useState(null);
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
      const next = await voiceApi.localSave(localSettingsPayload(merged));
      builtin.setStatus(next);
      setDraft(localSettingsForm(next.local?.settings));
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
  const group = useLocalGroup(builtin, LOCAL_MODEL_KEYS, localModelSettingsError);
  const { local, models, baseline, draft: form, update, dirty, error, result } = group;
  const groups = localModelGroups(models);
  const voices = selectedVoices(form, models);
  const pick = (kind, id) => {
    group.setDraft((current) => selectLocalModel(current, models, kind, id));
    group.setResult(null);
  };
  // 折叠摘要只反映已保存的设置
  const on = groups.filter((item) => baseline[item.kind]).map((item) => item.title);

  const remove = async (model) => {
    if (!(await showConfirm(`删除本地模型“${model.label}”？需要时可以重新下载。`, { title: "删除模型？", tone: "warning", danger: true, confirmText: "删除" }))) return;
    await act("删除模型", () => voiceApi.localDelete(model.id));
  };

  return (
    <VoiceGroup id="local-models" title="本地模型（断网可用）" summary={on.length ? `已开启：${on.join("、")}` : "未开启"} dirty={dirty}>
      <div id="voice-local-models" className="voice-stack">
        {!local.helper_found && (
          <div className="voice-hint voice-hint--warn">没有找到本地模型程序 moss-voice-local（应与 MOSS 放在同一目录）。可以先下载模型，但打开本地开关后内置服务将无法启动。</div>
        )}
        <p className="voice-muted">每类可下载多个模型，只用选中的那个。下载、删除立即执行；开关、选用和音色要点下方保存。</p>
        {groups.map((item) => {
          const selectedId = form[`${item.kind}_model`];
          const selected = item.models.find((model) => model.id === selectedId);
          return (
            <div className="voice-local-group" key={item.kind} id={`voice-local-group-${item.kind}`}>
              <label className="voice-check">
                <input type="checkbox" checked={form[item.kind]} onChange={(e) => update(item.kind, e.target.checked)} id={`voice-local-${item.kind}`} />
                <span>
                  {item.toggle}
                  {form[item.kind] && selected && !selected.installed && <span className="voice-bad">（选用的模型未下载，开关暂不生效）</span>}
                </span>
              </label>
              <div className="ob-tbl voice-tbl">
                <table>
                  <thead>
                    <tr>
                      <th className="voice-pick">选用</th>
                      <th>{item.title}模型</th>
                      <th>大小</th>
                      <th>状态</th>
                      <th>操作</th>
                    </tr>
                  </thead>
                  <tbody>
                    {item.models.map((model) => {
                      const view = localModelView(model);
                      const chosen = model.id === selectedId;
                      return (
                        <tr key={model.id} className={chosen ? "on" : ""}>
                          <td className="voice-pick">
                            <input type="radio" name={`voice-local-${item.kind}-model`} checked={chosen} onChange={() => pick(item.kind, model.id)} id={`voice-local-pick-${model.id}`} aria-label={`选用 ${model.label}`} />
                          </td>
                          <td className="voice-model-cell">
                            <label htmlFor={`voice-local-pick-${model.id}`} className="voice-model-name">
                              <b>{model.label}</b>
                              {model.streaming && <span className="ob-pill ob-pill--info voice-tag">边说边识别</span>}
                            </label>
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
  const summary = [baseline.read_only_shortcuts ? "快捷查询开" : "快捷查询关", baseline.barge_in ? "插话开" : "插话关", `${String(baseline.custom_hotwords || "").split(/\r?\n/).filter((w) => w.trim()).length} 个自定义热词`].join(" · ");
  return (
    <VoiceGroup id="experience" title="语音体验" summary={summary} dirty={dirty}>
      <p className="voice-muted">停止词整句命中时立即请求急停，不能关闭；设备是否停下仍需确认。</p>
      <div className="voice-form-grid">
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={form.read_only_shortcuts} onChange={(e) => update("read_only_shortcuts", e.target.checked)} id="voice-read-shortcuts" />
          <span>快捷查询状态、连接、进度、剩余时间与材料（不经大模型，默认开）</span>
        </label>
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={form.asr_beam_search} onChange={(e) => update("asr_beam_search", e.target.checked)} id="voice-asr-beam" />
          <span>X-ASR 热词识别（默认开；识别较慢时可关闭）</span>
        </label>
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
      </div>
      {dirty && error && <div className="voice-hint voice-hint--bad">{error}</div>}
      <SaveBar id="voice-experience-save" target="语音体验设置" dirty={dirty} busy={Boolean(busy)} saving={busy === "local-save-语音体验设置"} blocked={error ? "请先改正上面的错误" : ""} result={result} onSave={() => group.save("语音体验设置")} />
    </VoiceGroup>
  );
}

/** 最近 20 轮对话各环节耗时（只在内存）。 */
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
              <span><strong>{title}</strong><small>{description}</small></span>
              <ChevronDown className="voice-private-item-arrow" size={18} aria-hidden="true" />
            </summary>
            <div className="voice-private-item-body">
              {key === "memory" && opened.has("memory") && (
                <>
                  <VoiceMemoryCard status={status} />
                  <details className="voice-private-more" id="voice-private-memory-advanced"
                    onToggle={(event) => { if (event.currentTarget.open) expandAdvanced("memory"); }}>
                    <summary>长期记忆、资料与恢复摘要</summary>
                    {advanced.has("memory") && <VoiceContextCard />}
                  </details>
                </>
              )}
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
