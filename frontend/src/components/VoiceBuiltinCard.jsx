import VoiceMusicCard from "./VoiceMusicCard.jsx";
import VoiceAgentCard from "./VoiceAgentCard.jsx";
import VoiceVisionCard from "./VoiceVisionCard.jsx";
import React, { useCallback, useEffect, useRef, useState } from "react";
import { Cpu, Play, Square, ShieldCheck, ShieldOff, Pencil, Check, X, FlaskConical, Save, Trash2, Download } from "lucide-react";
import { showAlert, showConfirm } from "../appDialog.js";
import { voiceApi } from "../voiceApi.js";
import {
  SECTIONS,
  VOICE_POLL_MS,
  allowDeviceConfirmText,
  builtinDeviceView,
  builtinKeyPlaceholder,
  builtinModeView,
  builtinModelForm,
  canSaveSection,
  finalUrl,
  formatBytes,
  isSectionBlank,
  localModelGroups,
  localModelView,
  localSettingsError,
  localSettingsForm,
  localSettingsPayload,
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
// 识别、大模型、合成三项配齐后为对话模式（NV5），否则为回声测试。只在桌面端的语音中心页中出现。

const TONE_CLASS = { ok: "ob-pill--ok", info: "ob-pill--info", warn: "ob-pill--warn", bad: "ob-pill--bad", off: "ob-pill--off" };

function formatTime(seconds) {
  if (!seconds) return "—";
  return new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false, month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

/** 本地模型（NV6）：判停、识别、合成各有几个可选模型，每类选一个、可分别下载。
 *  模型由与 MOSS 同目录的辅助程序 moss-voice-local 运行。 */
function LocalModels({ status, busy, setBusy, onStatus }) {
  const local = status.local || {};
  const models = Array.isArray(local.models) ? local.models : [];
  const groups = localModelGroups(models);
  const [form, setForm] = useState(() => localSettingsForm(local.settings));
  const formError = localSettingsError(form, models);
  const voices = selectedVoices(form, models);
  const update = (key, value) => setForm((current) => ({ ...current, [key]: value }));
  const pick = (kind, id) => setForm((current) => selectLocalModel(current, models, kind, id));

  const act = async (label, action) => {
    setBusy(label);
    try {
      const next = await action();
      if (next) onStatus(next);
      return next;
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: `${label}失败`, tone: "error" });
      return null;
    } finally {
      setBusy("");
    }
  };

  const remove = async (model) => {
    if (!(await showConfirm(`删除本地模型“${model.label}”？需要时可以重新下载。`, { title: "删除模型？", tone: "warning", danger: true, confirmText: "删除" }))) return;
    await act("删除模型", () => voiceApi.localDelete(model.id));
  };

  const save = async () => {
    const next = await act("保存本地模型设置", () => voiceApi.localSave(localSettingsPayload(form)));
    if (next) {
      setForm(localSettingsForm(next.local?.settings));
      const restarted = next.restarted ? "内置服务已重启并按新设置运行。" : "内置服务下次启动时生效。";
      await showAlert(`本地模型设置已保存。${restarted}`, { title: "已保存", tone: "success" });
    }
  };

  return (
    <div className="voice-builtin-models" id="voice-local-models">
      <h3>本地模型（断网可用，不需要密钥）</h3>
      {!local.helper_found && (
        <div className="voice-hint voice-hint--warn">
          没有找到本地模型程序 moss-voice-local（应与 MOSS 放在同一目录）。可以先下载模型，但打开本地开关后内置服务将无法启动。
        </div>
      )}
      {groups.map((group) => {
        const selectedId = form[`${group.kind}_model`];
        const selected = group.models.find((model) => model.id === selectedId);
        return (
          <div className="voice-local-group" key={group.kind} id={`voice-local-group-${group.kind}`}>
            <label className="voice-check">
              <input type="checkbox" checked={form[group.kind]} onChange={(e) => update(group.kind, e.target.checked)} id={`voice-local-${group.kind}`} />
              <span>
                {group.toggle}
                {form[group.kind] && selected && !selected.installed && <span className="voice-bad">（选用的模型未下载，开关暂不生效）</span>}
              </span>
            </label>
            <div className="ob-tbl voice-tbl">
              <table>
                <thead>
                  <tr>
                    <th className="voice-pick">选用</th>
                    <th>{group.title}模型</th>
                    <th>大小</th>
                    <th>状态</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {group.models.map((model) => {
                    const view = localModelView(model);
                    const chosen = model.id === selectedId;
                    return (
                      <tr key={model.id} className={chosen ? "on" : ""}>
                        <td className="voice-pick">
                          <input
                            type="radio"
                            name={`voice-local-${group.kind}-model`}
                            checked={chosen}
                            onChange={() => pick(group.kind, model.id)}
                            id={`voice-local-pick-${model.id}`}
                            aria-label={`选用 ${model.label}`}
                          />
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
        return <div className="voice-local-group" key={model.id}>
          <h4>{model.label}</h4>
          <p>{model.note} 下载不会开启声纹采集或个人身份识别。</p>
          <p>{formatBytes(model.size)} · {view.text}{view.detail ? ` · ${view.detail}` : ""}</p>
          {view.canDownload && <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => act("下载声纹模型", () => voiceApi.localDownload(model.id))}>下载声纹模型</button>}
          {view.canCancel && <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => act("取消声纹下载", () => voiceApi.localCancel(model.id))}>取消下载</button>}
          {view.canDelete && <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => remove(model)}>删除模型</button>}
        </div>;
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
          <span>模型下载地址（留空为官方地址 {local.default_mirror}；可填镜像，下载后一律校验 SHA-256）</span>
          <input className="text-input mono" value={form.mirror} onChange={(e) => update("mirror", e.target.value)} spellCheck="false" autoComplete="off" />
        </label>
      </div>
      {formError && <div className="voice-hint voice-hint--bad">{formError}</div>}
      <h3>语音体验</h3>
      <p className="voice-muted">停止词整句命中时立即请求急停，不能关闭。中间结果先说“停止”再续句，也可能触发停止；设备是否停下仍需确认。</p>
      <div className="voice-form-grid">
        <label className="voice-wide"><span><input type="checkbox" checked={form.read_only_shortcuts} onChange={(e) => update("read_only_shortcuts", e.target.checked)} id="voice-read-shortcuts" /> 快捷查询状态、连接、进度、剩余时间与材料（默认开启）</span></label>
        <label className="voice-wide"><span><input type="checkbox" checked={form.asr_beam_search} onChange={(e) => update("asr_beam_search", e.target.checked)} id="voice-asr-beam" /> X-ASR 热词识别（默认开启；较慢时可关闭，切回快速识别）</span></label>
        <label className="voice-wide"><span><input type="checkbox" checked={form.barge_in} onChange={(e) => update("barge_in", e.target.checked)} id="voice-barge-in" /> 说话时可直接插话（默认关闭，仅设备实时模式）</span></label>
        <label><span>连续插话时长（300 ~ 600 毫秒，默认 400）</span><input className="text-input" type="number" min="300" max="600" step="20" value={form.barge_in_ms} onChange={(e) => update("barge_in_ms", e.target.value)} id="voice-barge-in-ms" /></label>
        <p className="voice-muted voice-wide">须先在设备固件开启设备端回声消除，并使用 realtime 模式。MOSS 不修改或刷写固件；未消除的回声可能误打断，自动与手动模式不启用此功能。</p>
        <label className="voice-wide"><span>自定义热词（每行一条，最多 64 条，每条 32 字）</span><textarea className="text-input" rows={4} value={form.custom_hotwords} onChange={(e) => update("custom_hotwords", e.target.value)} id="voice-hotwords" /></label>
      </div>
      <p className="voice-muted">自动合并材料库名称和行业词。{form.asr && form.asr_model.startsWith("sense_voice") ? "SenseVoice 不支持热词。" : "X-ASR 只加入词表支持的中文热词，暂不支持英文热词。"} 云端识别会携带热词提示，是否生效取决于提供方；豆包使用较短的词表。</p>
      <div className="voice-actions">
        <button type="button" className="btn btn-primary" onClick={save} disabled={Boolean(busy) || Boolean(formError)} id="voice-local-save">
          <Save size={13} /> 保存模型与体验设置
        </button>
        <span className="voice-muted">每类可以下载多个模型，只使用选中的那个；模型保存在运行目录 models 文件夹。介绍中的速度为一台普通电脑的实测，因机器而异。</span>
      </div>
    </div>
  );
}

function TurnTimings({ timings }) {
  const items = voiceTimingReport(timings);
  const copy = async () => {
    try { await navigator.clipboard.writeText(JSON.stringify(items, null, 2)); await showAlert("耗时数据已复制", { title: "已复制", tone: "success" }); }
    catch { await showAlert("无法访问剪贴板，请检查浏览器权限", { title: "复制失败", tone: "error" }); }
  };
  return <div className="voice-builtin-models" id="voice-turn-timings">
    <h3>最近对话耗时</h3><p className="voice-muted">最近 20 轮，仅保存在内存，重启清空；不保存对话内容。各时间从本轮开始拾音计算，首帧发出不代表设备已播放。</p>
    <button type="button" className="btn btn-ghost" onClick={copy} disabled={!items.length} id="voice-timings-copy">复制耗时数据</button>
    {!items.length && <p className="voice-muted">完成一轮对话后显示。</p>}
    {items.map((item, index) => <details key={index} open={index === 0}>
      <summary>第 {item.turn} 轮 · {item.outcome === "done" ? "结束" : item.outcome === "interrupted" ? "被打断" : "未完成"} · {item.models.join(" / ")}</summary>
      <div className="voice-table-wrap"><table className="voice-table"><thead><tr><th>阶段</th><th>毫秒</th></tr></thead><tbody>
        {Object.entries(item.stages).map(([stage, ms]) => <tr key={stage}><td>{VOICE_TIMING_STAGES[stage]}</td><td>{ms}</td></tr>)}
        {item.tools.map((tool, i) => <tr key={`tool-${i}`}><td>工具 {tool.name}</td><td>{tool.ms}（调用耗时）</td></tr>)}
      </tbody></table></div>
    </details>)}
  </div>;
}

function formsFrom(models) {
  return Object.fromEntries(SECTIONS.map(({ key }) => [key, builtinModelForm(key, models?.[key])]));
}

/** 内置服务的模型配置：与外部服务端向导相同的“先测试、后保存”，密钥只提交、不回显。 */
function BuiltinModels({ status, busy, setBusy, onStatus }) {
  const [tab, setTab] = useState("llm");
  const [forms, setForms] = useState(() => formsFrom(status.models));
  const [tests, setTests] = useState({});
  const entry = status.models?.[tab];
  const form = forms[tab];
  const test = tests[tab];
  const current = SECTIONS.find((item) => item.key === tab) || SECTIONS[0];
  const saveEnabled = canSaveSection(tab, form, test, { busy: Boolean(busy) });
  const doubao = isDoubao(form);

  const update = (key, value) => setForms((all) => ({ ...all, [tab]: { ...all[tab], [key]: value } }));
  const changeKind = (kind) => setForms((all) => ({ ...all, [tab]: switchKind(tab, all[tab], kind) }));

  const runTest = async () => {
    const inputError = sectionInputError(tab, form);
    if (inputError) {
      setTests((all) => ({ ...all, [tab]: { ok: false, detail: inputError, fingerprint: "" } }));
      return;
    }
    const fingerprint = sectionFingerprint(tab, form);
    setBusy(`builtin-test-${tab}`);
    try {
      const result = await voiceApi.builtinModelsTest(tab, sectionPayload(tab, form));
      setTests((all) => ({ ...all, [tab]: { ...result, fingerprint } }));
    } catch (error) {
      setTests((all) => ({ ...all, [tab]: { ok: false, detail: voiceErrorText(error), fingerprint: "" } }));
    } finally {
      setBusy("");
    }
  };

  const afterSave = async (next, done) => {
    onStatus(next);
    setForms(formsFrom(next.models));
    setTests((all) => ({ ...all, [tab]: undefined }));
    const mode = builtinModeView(next);
    const restarted = next.restarted ? `内置服务已重启，当前为“${mode.text}”模式。` : `内置服务下次启动时以“${mode.text}”模式运行。`;
    await showAlert(`${done}${restarted}`, { title: "已保存", tone: "success" });
  };

  const save = async () => {
    setBusy("builtin-save");
    try {
      const next = await voiceApi.builtinModelsSave({ [tab]: sectionPayload(tab, form) });
      await afterSave(next, `${current.label}设置已保存（密钥存本机数据库，不显示）。`);
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "保存失败", tone: "error" });
    } finally {
      setBusy("");
    }
  };

  const clear = async () => {
    const message = `删除内置服务的“${current.label}”设置与已保存的密钥？删除后内置服务回到回声测试模式。`;
    if (!(await showConfirm(message, { title: "删除设置？", tone: "warning", danger: true, confirmText: "删除" }))) return;
    setBusy("builtin-clear");
    try {
      const next = await voiceApi.builtinModelsSave({}, [tab]);
      await afterSave(next, `${current.label}设置与密钥已删除。`);
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "删除失败", tone: "error" });
    } finally {
      setBusy("");
    }
  };

  return (
    <div className="voice-builtin-models" id="voice-builtin-models">
      <h3>模型配置（内置服务）</h3>
      {(status.local?.settings?.asr || status.local?.settings?.tts) && (
        <div className="voice-muted">已打开本地识别或合成：对应分区以本地模型为准，这里的云端设置作为备用。大模型只能用云端（或本机 Ollama 等 OpenAI 兼容服务）。</div>
      )}
      <div className="voice-tabs" role="tablist">
        {SECTIONS.map((item) => {
          const saved = status.models?.[item.key];
          return (
            <button key={item.key} type="button" role="tab" aria-selected={tab === item.key} className={`voice-tab${tab === item.key ? " on" : ""}`} onClick={() => setTab(item.key)} id={`voice-builtin-tab-${item.key}`}>
              {item.label}
              <span className={saved?.configured ? "voice-ok" : "voice-muted"}>{saved?.configured ? " ✓" : " 未配置"}</span>
            </button>
          );
        })}
      </div>
      <div className="voice-form-grid">
        {tab !== "llm" && (
          <label className="voice-wide">
            <span>接口类型</span>
            <select className="text-input" value={doubao ? "doubao" : "openai"} onChange={(e) => changeKind(e.target.value)} id="voice-builtin-model-kind">
              <option value="openai">OpenAI 兼容（HTTP，整段{tab === "asr" ? "识别" : "合成"}）</option>
              <option value="doubao">豆包流式（WebSocket，{tab === "asr" ? "边说边识别" : "边合成边播放"}，首句更快）</option>
            </select>
          </label>
        )}
        <label className="voice-wide">
          <span>地址（{doubao ? "豆包 WebSocket 地址，一般不用改" : current.hint}）</span>
          <input className="text-input mono" value={form.url} onChange={(e) => update("url", e.target.value)} spellCheck="false" autoComplete="off" id="voice-builtin-model-url" />
        </label>
        <label>
          <span>{doubao ? "API Key" : "密钥"}</span>
          <input className="text-input" type="password" value={form.api_key} placeholder={doubao && !entry?.key_set ? "控制台 API Key；旧版控制台填“APP ID:Access Token”" : builtinKeyPlaceholder(entry)} onChange={(e) => update("api_key", e.target.value)} autoComplete="new-password" spellCheck="false" id="voice-builtin-model-key" />
        </label>
        <label>
          <span>{doubao ? "资源 ID（留空用默认）" : "模型"}</span>
          <input className="text-input mono" value={form.model} onChange={(e) => update("model", e.target.value)} spellCheck="false" id="voice-builtin-model-name" />
        </label>
        {tab === "tts" && (
          <>
            <label>
              <span>{doubao ? "音色 ID（见豆包控制台音色列表）" : "音色"}</span>
              <input className="text-input" value={form.voice} onChange={(e) => update("voice", e.target.value)} spellCheck="false" id="voice-builtin-model-voice" />
            </label>
            <label>
              <span>{doubao ? "语速（0.5 ~ 2.0）" : "语速（0.25 ~ 4.0）"}</span>
              <input className="text-input" inputMode="decimal" value={form.speed} onChange={(e) => update("speed", e.target.value)} />
            </label>
          </>
        )}
      </div>
      <div className="voice-final mono">保存的地址：{finalUrl(tab, form.url, form.kind) || "—"}</div>
      <div className="voice-actions">
        <button type="button" className="btn btn-ghost" onClick={runTest} disabled={Boolean(busy) || isSectionBlank(form)} id="voice-builtin-model-test">
          <FlaskConical size={13} /> {busy === `builtin-test-${tab}` ? "测试中…" : "测试连接"}
        </button>
        <button type="button" className="btn btn-primary" onClick={save} disabled={!saveEnabled} id="voice-builtin-model-save" title={saveEnabled ? "保存到本机" : "先用当前填写的内容测试连接并成功后才能保存"}>
          <Save size={13} /> 保存
        </button>
        {entry?.configured && (
          <button type="button" className="btn btn-ghost" onClick={clear} disabled={Boolean(busy)} id="voice-builtin-model-clear">
            <Trash2 size={13} /> 删除此项
          </button>
        )}
      </div>
      {test ? (
        <div className={`voice-hint voice-hint--${test.ok ? "ok" : "bad"}`} id="voice-builtin-model-result">
          {test.ok ? "测试通过" : "测试未通过"}
          {typeof test.elapsed_ms === "number" ? ` · ${(test.elapsed_ms / 1000).toFixed(1)} s` : ""} · {test.detail}
          {test.ok && test.fingerprint !== sectionFingerprint(tab, form) && "（测试后内容已修改，请重新测试）"}
        </div>
      ) : (
        <div className="voice-muted">
          测试只用本页填写的值（密钥留空时用已保存的密钥）发一次最小请求，15 秒超时、不跟随重定向。密钥保存在本机数据库，不写入 settings.json 与日志，页面不再显示。
        </div>
      )}
    </div>
  );
}

export function VoiceBuiltinCard() {
  const [status, setStatus] = useState(null);
  const [busy, setBusy] = useState("");
  const [editing, setEditing] = useState({ id: "", name: "" });
  const mounted = useRef(true);

  const refresh = useCallback(async () => {
    try {
      const next = await voiceApi.builtinStatus();
      if (mounted.current) setStatus(next);
    } catch (error) {
      if (mounted.current) setStatus({ error: voiceErrorText(error) });
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

  const act = async (label, action) => {
    setBusy(label);
    try {
      const next = await action();
      if (mounted.current && next) setStatus(next);
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: `${label}失败`, tone: "error" });
    } finally {
      if (mounted.current) setBusy("");
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

  if (!status) return null;
  const running = Boolean(status.running);
  const devices = Array.isArray(status.devices) ? status.devices : [];
  const sessions = Array.isArray(status.sessions) ? status.sessions : [];
  const events = Array.isArray(status.events) ? status.events.slice(0, 20) : [];
  const modeView = builtinModeView(status);

  return (
    <section className="ob-card" id="voice-builtin-card">
      <div className="ob-card-h">
        <h2>
          <Cpu size={16} aria-hidden="true" /> 内置语音服务
        </h2>
        {!status.error && (
          <span className={`ob-pill ${TONE_CLASS[modeView.tone]}`} id="voice-builtin-mode">
            {modeView.text}
          </span>
        )}
        <span className={`ob-pill ${running ? TONE_CLASS.ok : TONE_CLASS.off}`} id="voice-builtin-state">
          {running ? "运行中" : "未运行"}
        </span>
      </div>
      {status.error ? (
        <div className="voice-hint voice-hint--bad">{status.error}</div>
      ) : (
        <>
          <div className="voice-hint">
            小智设备直接连到 MOSS，不需要外部服务端。{modeView.hint}
          </div>
          <div className="voice-muted">{modeView.autostart}</div>
          {status.last_error && !running && (
            <div className="voice-hint voice-hint--bad" id="voice-builtin-error">
              自动启动失败：{status.last_error}
            </div>
          )}
          <div className="voice-kv">
            <span>
              设备连接端口 {status.ws_port} · OTA 端口 {status.http_port}（可在下方“服务端与接入点设置”中修改）
            </span>
            <span>
              设备 OTA 地址：
              <b className="mono voice-break" id="voice-builtin-ota">{status.ota_url || "未能获取本机局域网地址"}</b>
            </span>
            <span className="voice-muted">
              在设备的配网页面把 OTA 地址改成上面的地址并重启设备；第一次连接后，在下面的设备列表中点“允许”，再重启一次设备。首次启动时 Windows 防火墙可能询问是否允许 MOSS 联网，请选择允许（专用网络）。
            </span>
          </div>
          <div className="voice-actions">
            {running ? (
              <button type="button" className="btn btn-danger" disabled={Boolean(busy)} onClick={() => act("停止内置服务", () => voiceApi.builtinStop())} id="voice-builtin-stop">
                <Square size={13} /> 停止
              </button>
            ) : (
              <button type="button" className="btn btn-primary" disabled={Boolean(busy)} onClick={() => act("启动内置服务", () => voiceApi.builtinStart())} id="voice-builtin-start">
                <Play size={13} /> 启动
              </button>
            )}
            {sessions.length > 0 && <span className="voice-ok">已连接 {sessions.length} 台设备</span>}
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
                              <button type="button" className="btn btn-ghost" onClick={saveName} title="保存">
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

          <VoiceMusicCard />
        <VoiceAgentCard />
          <VoiceVisionCard />
          <section className="voice-card" aria-label="私人记忆与信息插件">
            <h3>私人记忆与信息插件</h3>
            <p>私人记忆内容及插件凭据状态仅在本机打包的安全窗口中显示；主窗口不会读取这些敏感数据。</p>
            <button type="button" className="btn btn-primary" id="voice-private-settings-open"
              onClick={async () => {
                try {
                  await voiceApi.openPrivateSettings();
                } catch (error) {
                  await showAlert(voiceErrorText(error), { title: "无法打开安全设置", tone: "error" });
                }
              }}>打开本地安全设置窗口</button>
          </section>
          <BuiltinModels status={status} busy={busy} setBusy={setBusy} onStatus={setStatus} />
          <LocalModels status={status} busy={busy} setBusy={setBusy} onStatus={setStatus} />
          <TurnTimings timings={status.timings} />

          {events.length > 0 && (
            <pre className="voice-log mono" id="voice-builtin-events">
              {events.map((event) => `${formatTime(event.at)}  ${event.text}`).join("\n")}
            </pre>
          )}
        </>
      )}
    </section>
  );
}
