import React, { useState } from "react";
import { X, Trash2, Download, Volume2 } from "lucide-react";
import { showAlert, showConfirm } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { SaveBar, VoiceGroup } from "../VoiceLayout.jsx";
import { LocalTtsVoicesModal } from "../LocalTtsVoicesModal.jsx";
import {
  LOCAL_MODEL_KEYS,
  canonicalLocalJevModel,
  formatBytes,
  isLocalJevModel,
  localModelGroups,
  localModelSettingsError,
  localModelView,
  layaSetupView,
  localJevRuntimeView,
  localModelDeleteText,
  localGroupPatch,
  localSettingsForm,
  resolveFallbackTtsModel,
  selectLocalModel,
  selectedVoices,
  voiceErrorText,
} from "../../voiceState.js";
import { restartEffect, TONE_CLASS, LAYA_HINT_CLASS, focusJevSwitch } from "./shared.js";
import { useLocalGroup } from "./useLocalGroup.js";
import { ColabBridgeSection } from "./ColabBridgeSection.jsx";
import { ModelPairingGuide } from "./ModelPairingGuide.jsx";

/** 本地模型（NV6）：判停、识别、合成各有几个可选模型，每类选一个、可分别下载。 */
export function LocalModelsGroup({ builtin }) {
  const { busy } = builtin;
  // 2026-11-07：下载 / 取消 / 删除按模型各自加锁，点一个模型不再让其他模型的按钮变灰，可同时下载多个
  const [pending, setPending] = useState({});
  const modelLocked = (id) => Boolean(busy) || Boolean(pending[id]);
  const modelAct = async (id, label, action) => {
    setPending((prev) => ({ ...prev, [id]: label }));
    try {
      const next = await action();
      if (next) builtin.setStatus(next);
      return next;
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: `${label}失败`, tone: "error" });
      return null;
    } finally {
      setPending((prev) => {
        const rest = { ...prev };
        delete rest[id];
        return rest;
      });
    }
  };
  const group = useLocalGroup(builtin, LOCAL_MODEL_KEYS, localModelSettingsError);
  const { local, models, baseline, draft: form, setDraft, update, dirty, error, result, setResult } = group;
  const groups = localModelGroups(models);
  const voices = selectedVoices(form, models);
  const fallbackTtsModel = resolveFallbackTtsModel(form, models);
  const colabActive = Boolean(local.colab?.active);
  const on = [
    ...groups.filter((item) => baseline[item.kind]).map((item) => item.title),
    ...(colabActive ? ["Colab T4 联动"] : []),
  ];
  const remove = async (model) => {
    if (!(await showConfirm(localModelDeleteText(model, models, local), { title: "删除模型？", tone: "warning", danger: true, confirmText: "删除" }))) return;
    await modelAct(model.id, "删除模型", () => voiceApi.localDelete(model.id));
  };
  const layaSetup = layaSetupView(local.laya_setup, local.laya_env);
  const jevRuntime = localJevRuntimeView(local.jev_runtime);
  const layaSetupLocked = modelLocked("laya-setup");
  const [ttsVoicesModalModel, setTtsVoicesModalModel] = useState(null);
  const applyVoiceFromModal = async (targetModelId, speakerId, speakerItem) => {
    builtin.setBusy("local-save-本地合成音色");
    try {
      const next = await voiceApi.localSave({
        tts: true,
        tts_model: targetModelId,
        tts_speaker: Number(speakerId),
      });
      builtin.setStatus(next);
      setDraft((prev) => ({
        ...prev,
        tts: true,
        tts_model: targetModelId,
        tts_speaker: String(speakerId),
      }));
      const text = `已选用 #${speakerId} ${speakerItem?.name || ""}（${speakerItem?.gender_label || ""}）· ${restartEffect(next)}`;
      setResult({ ok: true, text });
      return text;
    } finally {
      builtin.setBusy("");
    }
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
    <VoiceGroup id="local-models" title="本地模型包、本地合成音色与下载地址" summary={on.length ? `已开启：${on.join("、")}` : "未开启"} dirty={dirty}>
      <div id="voice-local-models" className="voice-stack">
        {!local.helper_found && (
          <div className="voice-hint voice-hint--warn">没有找到本地模型程序 moss-voice-local（应与 MOSS 放在同一目录）。可以先下载模型，但打开本地开关后内置服务将无法启动。</div>
        )}
        <p className="voice-muted">
          在这里下载或管理断网可用的本地模型包（随 MOSS 自定义安装目录存放{local.models_dir ? `：${local.models_dir}` : ""}），以及调整本地合成音色与语速；当前启用哪个模型请直接在上方顶部下拉框切换。
        </p>
        <ModelPairingGuide />
        <ColabBridgeSection builtin={builtin} />
        {groups.map((item) => {
          const selectedId = form[`${item.kind}_model`];
          const activeOn = Boolean(form[item.kind]);
          const selected = item.models.find((model) => model.id === selectedId);
          return (
            <div className="voice-local-group" key={item.kind} id={`voice-local-group-${item.kind}`}>
              <div className="voice-field-head">
                <b>{item.title}模型包</b>
                <span className="voice-muted">
                  {item.kind === "vision"
                    ? "下载后到“视觉理解”卡片的“看图用哪个模型”里选“本地视觉模型”"
                    : item.kind === "denoise" || item.kind === "turn"
                      ? activeOn
                        ? selected?.installed
                          ? `已开启：${selected.label}`
                          : `已开启 ${selected?.label || item.title}（未下载，暂不生效）`
                        : item.kind === "denoise"
                          ? "未开启（风扇/气泵环境噪声大时可下载并开启，仅作用于本地判停与本地识别）"
                          : "未开启（说话中间常停顿思考时可下载并开启，辅助判断话没说完且不延迟急停）"
                      : activeOn
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
              {(item.kind === "denoise" || item.kind === "turn") && (
                <label className="voice-check">
                  <input
                    type="checkbox"
                    id={`voice-local-${item.kind}-toggle`}
                    checked={Boolean(form[item.kind])}
                    onChange={(e) => update(item.kind, e.target.checked)}
                  />
                  <span>
                    {item.kind === "denoise"
                      ? "启用本地前端降噪（GTCRN，保存后重启内置服务生效）"
                      : "启用本地语义判停（Smart Turn v3，保存后重启内置服务生效）"}
                  </span>
                </label>
              )}
              {item.kind === "tts" &&
                (form.tts_model === "qwen3_tts_0_6b" ||
                  item.models.some((m) => m.id === "qwen3_tts_0_6b" && m.installed)) &&
                local.speech_env?.summary && (
                  <div className="voice-hint" id="voice-local-speech-env-hint">
                    Qwen3-TTS 本机运行时（speech.cpp）：{local.speech_env.summary}
                  </div>
                )}
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
                              {item.kind === "tts" ? (
                                <button
                                  type="button"
                                  className="voice-tts-model-trigger"
                                  onClick={() => setTtsVoicesModalModel(model)}
                                  title="点击查看支持的语言包、全部音色并试听本机缓存语音"
                                  id={`voice-local-tts-open-${model.id}`}
                                >
                                  <b>{model.label}</b>
                                  <span className="voice-tts-trigger-chip">
                                    <Volume2 size={12} /> 语言包与试听
                                  </span>
                                </button>
                              ) : (
                                <b>{model.label}</b>
                              )}
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
                                <button type="button" className="btn btn-ghost" disabled={modelLocked(model.id)} onClick={() => modelAct(model.id, "下载模型", () => voiceApi.localDownload(model.id))} id={`voice-local-download-${model.id}`}>
                                  <Download size={13} /> 下载
                                </button>
                              )}
                              {view.canCancel && (
                                <button type="button" className="btn btn-ghost" disabled={modelLocked(model.id)} onClick={() => modelAct(model.id, "取消下载", () => voiceApi.localCancel(model.id))} id={`voice-local-cancel-${model.id}`}>
                                  <X size={13} /> 取消
                                </button>
                              )}
                              {view.canDelete && (
                                <button type="button" className="btn btn-ghost" disabled={modelLocked(model.id)} onClick={() => remove(model)} id={`voice-local-delete-${model.id}`}>
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
              <b>Jev 前置粗筛模型包（本地 System 1 决策：OneJev 1.6 / Laya）</b>
              <span className="voice-muted">
                {form.jev_filter_enabled && isLocalJevModel(form.jev_model)
                  ? `当前在顶部已选用：${models.find((m) => m.id === canonicalLocalJevModel(form.jev_model))?.label || form.jev_model}`
                  : form.jev_filter_enabled
                    ? `当前在顶部使用：云端 Jev（${form.jev_model}）`
                    : "当前在顶部未启用（已下载的模型在顶部第 3 栏下拉里选用）"}
              </span>
            </div>
            {local.onejev_env?.summary && (
              <div className="voice-hint" id="voice-local-onejev-env-hint">
                OneJev 1.6 本机硬件加速：{local.onejev_env.summary}
              </div>
            )}
            {jevRuntime && (
              <div className={`voice-hint ${LAYA_HINT_CLASS[jevRuntime.tone] || ""}`} id="voice-local-jev-runtime" role="status">
                <div>{jevRuntime.text}</div>
                {jevRuntime.detail && <div className="ob-sub">{jevRuntime.detail}</div>}
              </div>
            )}
            {layaSetup && (
              <div className={`voice-hint ${LAYA_HINT_CLASS[layaSetup.tone] || ""}`} id="voice-local-laya-setup">
                <div>{layaSetup.text}</div>
                {layaSetup.detail && <div className="ob-sub mono voice-laya-setup-detail">{layaSetup.detail}</div>}
                {(layaSetup.canStart || layaSetup.canCancel) && (
                  <span className="voice-actions">
                    {layaSetup.canStart && (
                      <button
                        type="button"
                        className="btn btn-ghost"
                        disabled={layaSetupLocked}
                        onClick={() => modelAct("laya-setup", "准备 Laya 运行环境", () => voiceApi.localLayaSetup("start"))}
                        id="voice-local-laya-setup-start"
                      >
                        {layaSetup.startLabel}
                      </button>
                    )}
                    {layaSetup.canCancel && (
                      <button
                        type="button"
                        className="btn btn-ghost"
                        disabled={layaSetupLocked}
                        onClick={() => modelAct("laya-setup", "取消准备", () => voiceApi.localLayaSetup("cancel"))}
                        id="voice-local-laya-setup-cancel"
                      >
                        <X size={13} /> 取消准备
                      </button>
                    )}
                  </span>
                )}
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
                      const chosen = Boolean(
                        form.jev_filter_enabled && canonicalLocalJevModel(form.jev_model) === model.id,
                      );
                      const isOneJevRow = model.id !== "laya_multilingual";
                      const handleDownloadJev = async () => {
                        setPending((prev) => ({ ...prev, [model.id]: "下载模型" }));
                        try {
                          // Laya：运行环境没准备好时后端会同时在后台自动准备（2026-11-07）
                          const next = await voiceApi.localDownload(model.id);
                          if (next) builtin.setStatus(next);
                        } catch (err) {
                          if (err?.code === "laya_env_missing") {
                            setPending((prev) => {
                              const rest = { ...prev };
                              delete rest[model.id];
                              return rest;
                            });
                            const go = await showConfirm(
                              `${err.message}\n\n是否仍要先只下载模型权重文件？装好 Python 后再点“准备运行环境”。`,
                              {
                                title: "无法自动准备 Laya 运行环境",
                                tone: "warning",
                                confirmText: "仍然下载",
                                cancelText: "取消",
                              }
                            );
                            if (!go) return;
                            await modelAct(model.id, "下载模型", () => voiceApi.localDownload(`${model.id}:force`));
                            return;
                          }
                          await showAlert(voiceErrorText(err), { title: "下载模型失败", tone: "error" });
                        } finally {
                          setPending((prev) => {
                            const rest = { ...prev };
                            delete rest[model.id];
                            return rest;
                          });
                        }
                      };
                      return (
                        <tr key={model.id} className={chosen ? "on" : ""}>
                          <td className="voice-model-cell">
                            <div className="voice-model-name">
                              <b>{model.label}</b>
                              {chosen && <span className="ob-pill ob-pill--ok voice-tag">当前选用</span>}
                              <span className="ob-pill ob-pill--info voice-tag">
                                {isOneJevRow
                                  ? `免密钥 · 免 Python · ${local.onejev_env?.accelerator_label || "GPU/CPU 自动加速"}`
                                  : `免密钥 · Python laya[serve]${local.laya_env?.accelerator === "mps" ? " · Apple Metal MPS" : local.laya_env?.cuda_found ? " · CUDA 显卡加速" : ""}`}
                              </span>
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
                              {/* n69：和其他本地模型一样只在顶部下拉里选用，这里不再另设“选用”按钮写同一个开关 */}
                              {Boolean(model.installed) && !chosen && (
                                <button
                                  type="button"
                                  className="voice-link-btn"
                                  onClick={focusJevSwitch}
                                  title="Jev 用哪个模型只在本页顶部第 3 栏“Jev 前置粗筛”下拉里切换"
                                  id={`voice-local-pick-${model.id}`}
                                >
                                  去顶部选用
                                </button>
                              )}
                              {view.canDownload && (
                                <button
                                  type="button"
                                  className="btn btn-ghost"
                                  disabled={modelLocked(model.id)}
                                  onClick={handleDownloadJev}
                                  id={`voice-local-download-${model.id}`}
                                >
                                  <Download size={13} /> 下载
                                </button>
                              )}
                              {view.canCancel && (
                                <button
                                  type="button"
                                  className="btn btn-ghost"
                                  disabled={modelLocked(model.id)}
                                  onClick={() => modelAct(model.id, "取消下载", () => voiceApi.localCancel(model.id))}
                                  id={`voice-local-cancel-${model.id}`}
                                >
                                  <X size={13} /> 取消
                                </button>
                              )}
                              {view.canDelete && (
                                <button type="button" className="btn btn-ghost" disabled={modelLocked(model.id)} onClick={() => remove(model)} id={`voice-local-delete-${model.id}`}>
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
                {view.canDownload && <button type="button" className="btn btn-ghost" disabled={modelLocked(model.id)} onClick={() => modelAct(model.id, "下载声纹模型", () => voiceApi.localDownload(model.id))}>下载声纹模型</button>}
                {view.canCancel && <button type="button" className="btn btn-ghost" disabled={modelLocked(model.id)} onClick={() => modelAct(model.id, "取消声纹下载", () => voiceApi.localCancel(model.id))}>取消下载</button>}
                {view.canDelete && <button type="button" className="btn btn-ghost" disabled={modelLocked(model.id)} onClick={() => remove(model)}>删除模型</button>}
              </span>
            </div>
          );
        })}
        {models.filter((model) => model.kind === "font").map((model) => {
          const view = localModelView(model);
          return (
            <div className="voice-local-group" key={model.id} id="voice-local-group-font">
              <b>{model.label}</b>
              <p className="voice-muted">
                {model.note} {formatBytes(model.size)} · {view.text}
                {view.detail ? ` · ${view.detail}` : ""}
              </p>
              {view.percent !== null && (
                <div className="voice-progress" aria-hidden="true">
                  <span style={{ width: `${view.percent}%` }} />
                </div>
              )}
              <span className="voice-actions">
                {view.canDownload && (
                  <button
                    type="button"
                    className="btn btn-ghost"
                    disabled={modelLocked(model.id)}
                    id={`voice-local-download-${model.id}`}
                    onClick={() => modelAct(model.id, "下载全量补字库", () => voiceApi.localDownload(model.id))}
                  >
                    <Download size={13} /> 下载补字库
                  </button>
                )}
                {view.canCancel && (
                  <button
                    type="button"
                    className="btn btn-ghost"
                    disabled={modelLocked(model.id)}
                    id={`voice-local-cancel-${model.id}`}
                    onClick={() => modelAct(model.id, "取消补字库下载", () => voiceApi.localCancel(model.id))}
                  >
                    <X size={13} /> 取消下载
                  </button>
                )}
                {view.canDelete && (
                  <button
                    type="button"
                    className="btn btn-ghost"
                    disabled={modelLocked(model.id)}
                    id={`voice-local-delete-${model.id}`}
                    onClick={() => remove(model)}
                  >
                    <Trash2 size={13} /> 删除补字库
                  </button>
                )}
              </span>
            </div>
          );
        })}
        <div className="voice-form-grid">
          {voices ? (
            <label>
              <span className="voice-label-with-link">
                <span>本地合成音色编号（{voices.hint}；默认 {voices.default}）</span>
                <button
                  type="button"
                  className="voice-link-btn"
                  onClick={() => {
                    const target = models.find((m) => m.kind === "tts" && m.id === form.tts_model) || models.find((m) => m.kind === "tts");
                    if (target) setTtsVoicesModalModel(target);
                  }}
                  id="voice-local-speaker-browse"
                >
                  试听选音色
                </button>
              </span>
              <input className="text-input mono" inputMode="numeric" value={form.tts_speaker} onChange={(e) => update("tts_speaker", e.target.value)} id="voice-local-speaker" />
            </label>
          ) : (
            <label>
              <span className="voice-label-with-link">
                <span>本地合成音色</span>
                <button
                  type="button"
                  className="voice-link-btn"
                  onClick={() => {
                    const target = models.find((m) => m.kind === "tts" && m.id === form.tts_model) || models.find((m) => m.kind === "tts");
                    if (target) setTtsVoicesModalModel(target);
                  }}
                  id="voice-local-speaker-browse"
                >
                  查看语言包与试听
                </button>
              </span>
              <span className="voice-muted">所选合成模型只有一个音色</span>
            </label>
          )}
          <label>
            <span>本地合成语速（0.5 ~ 2.0）</span>
            <input className="text-input" inputMode="decimal" value={form.tts_speed} onChange={(e) => update("tts_speed", e.target.value)} />
            {form.tts_model === "qwen3_tts_0_6b" && (
              <small className="voice-muted">Qwen3-TTS 当前固定自然语速（1.0x），语速数值仅对 Kokoro / Matcha / ZipVoice 生效</small>
            )}
          </label>
          <label className="voice-wide">
            <span>模型下载地址（留空为官方地址；可填镜像，下载后一律校验 SHA-256）</span>
            <input className="text-input mono" value={form.mirror} placeholder={local.default_mirror || ""} onChange={(e) => update("mirror", e.target.value)} spellCheck="false" autoComplete="off" />
          </label>
        </div>
        {dirty && error && <div className="voice-hint voice-hint--bad">{error}</div>}
        <SaveBar id="voice-local-save" target="本地模型设置" dirty={dirty} busy={Boolean(busy)} saving={busy === "local-save-本地模型设置"} blocked={error ? "请先改正上面的错误" : ""} result={result} onSave={() => group.save("本地模型设置")} />
        <LocalTtsVoicesModal
          visible={Boolean(ttsVoicesModalModel)}
          modelId={ttsVoicesModalModel?.id || ""}
          modelLabel={ttsVoicesModalModel?.label || ""}
          currentTtsOn={Boolean(baseline.tts)}
          currentModelId={baseline.tts_model || ""}
          currentSpeaker={baseline.tts_speaker ?? null}
          onApplyVoice={applyVoiceFromModal}
          onDownloadModel={(targetId) => modelAct(targetId, "下载模型", () => voiceApi.localDownload(targetId))}
          onClose={() => setTtsVoicesModalModel(null)}
        />
      </div>
    </VoiceGroup>
  );
}
