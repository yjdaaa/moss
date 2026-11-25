import React, { useEffect, useState } from "react";
import { X, Trash2, Download, Volume2 } from "lucide-react";
import { showAlert, showConfirm } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { SaveBar, VoiceGroup } from "../VoiceLayout.jsx";
import { LocalTtsVoicesModal } from "../LocalTtsVoicesModal.jsx";
import {
  LOCAL_MODEL_KEYS,
  canonicalLocalJevModel,
  comboMissing,
  comboPatch,
  formatBytes,
  isLocalJevModel,
  isRecommendedLocalModel,
  localModelGroups,
  localModelLayout,
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
import { restartEffect, TONE_CLASS, LAYA_HINT_CLASS, focusJevSwitch, LOCAL_MORE_REVEAL_EVENT, consumeLocalMoreReveal } from "./shared.js";
import { useLocalGroup } from "./useLocalGroup.js";
import { ColabBridgeSection } from "./ColabBridgeSection.jsx";
import { ModelPairingGuide } from "./ModelPairingGuide.jsx";
import { LocalTtsSettings } from "./LocalTtsSettings.jsx";

/** 本地模型（NV6）：判停、识别、合成各有几个可选模型，每类选一个、可分别下载。 */
export function LocalModelsGroup({ builtin }) {
  const { busy } = builtin;
  // 2026-11-07：下载 / 取消 / 删除按模型各自加锁，点一个模型不再让其他模型的按钮变灰，可同时下载多个
  const [pending, setPending] = useState({});
  // “更多 / 实验”折叠区：用户点开 / 收起后以用户为准；顶部下拉选了收起区里还没下载的模型时自动展开
  const [moreOverride, setMoreOverride] = useState(() => (consumeLocalMoreReveal() ? true : null));
  useEffect(() => {
    const reveal = () => {
      consumeLocalMoreReveal();
      setMoreOverride(true);
    };
    globalThis.addEventListener?.(LOCAL_MORE_REVEAL_EVENT, reveal);
    return () => globalThis.removeEventListener?.(LOCAL_MORE_REVEAL_EVENT, reveal);
  }, []);
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
  const applyCombo = async (combo) => {
    const missing = comboMissing(combo, models);
    if (missing.length) {
      return { ok: false, text: `“${combo.title}”还需要先下载：${missing.join("、")}（在下方对应的模型包里点“下载”）` };
    }
    builtin.setBusy(`local-save-combo-${combo.id}`);
    try {
      const patch = comboPatch(combo);
      const next = await voiceApi.localSave(patch);
      builtin.setStatus(next);
      // 只换这套组合涉及的字段（换合成模型时后端会把音色换成新模型的默认音色），
      // 同组里还没保存的其他修改（语速、下载地址等）原样保留
      const saved = localSettingsForm(next.local?.settings);
      setDraft((prev) => {
        if (!prev) return saved;
        const merged = { ...prev };
        for (const key of [...Object.keys(patch), "tts_speaker"]) merged[key] = saved[key];
        return merged;
      });
      return { ok: true, text: `已换成“${combo.title}”这套模型 · ${restartEffect(next)}` };
    } catch (e) {
      return { ok: false, text: `切换失败：${voiceErrorText(e)}` };
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

  const layout = localModelLayout(groups, baseline);
  const moreOpen = moreOverride ?? layout.moreOpen;
  const isAddon = (kind) => kind === "denoise" || kind === "turn" || kind === "kws";
  const groupStatus = (item) => {
    const activeOn = Boolean(form[item.kind]);
    const selected = item.models.find((model) => model.id === form[`${item.kind}_model`]);
    if (item.kind === "vision") return "下载后在上方「视觉理解」卡片选用";
    if (isAddon(item.kind)) {
      if (activeOn) return selected?.installed ? `已开启：${selected.label}` : `已开启 ${selected?.label || item.title}（未下载，暂不生效）`;
      if (item.kind === "denoise") return "未开启（实测对识别无提升）";
      if (item.kind === "kws") return "未开启（需在顶部启用本地判停）";
      return "未开启（实测差别不大）";
    }
    if (activeOn) return selected?.installed ? `当前选用：${selected.label}` : `顶部已选 ${selected?.label || "本地模型"}（未下载）`;
    if (item.kind === "vad") return "顶部使用：内置能量判停";
    if (item.kind === "tts" && fallbackTtsModel) return `顶部使用：云端（异常兜底：${fallbackTtsModel.label}）`;
    return "顶部使用：云端模型";
  };
  // 2026-11-24 n111：区分“替换”（判停模型几选一）与“叠加”（降噪 / 语义判停 / 命令词抗噪，可同时开）
  const ROLE_TAG = { replace: "几选一", addon: "叠加，可同时开" };
  const ROLE_NOTE = {
    kws: "跑在本地判停的音频上，专门听“停 / 退下”这类短命令；要先在顶部选本地判停，可与降噪、语义判停同时开。",
    denoise: "先把音频降噪，再交给本地判停和本地识别；可与任何本地判停、本地识别同时开。GTCRN 与 DPDFNet 二选一（表里点“选用”）。",
    turn: "判停说“停了”之后，再按语气判断是不是真说完，没说完就多等一会儿；可与任何判停同时开（包括能量判停）。",
  };
  const renderHead = (item, title, showStatus = true, role = null) => (
    <div className="voice-field-head">
      <b>
        {title}
        {role && (
          <span className={`ob-pill ${role === "replace" ? "ob-pill--info" : "ob-pill--off"} voice-tag voice-role-tag`} data-role={role}>
            {ROLE_TAG[role]}
          </span>
        )}
      </b>
      {showStatus && item.models.length > 0 && <span className="voice-muted">{groupStatus(item)}</span>}
    </div>
  );
  const renderNote = (kind) => ROLE_NOTE[kind] && <p className="voice-muted voice-role-note" id={`voice-local-${kind}-note`}>{ROLE_NOTE[kind]}</p>;
  const renderToggle = (item) => (
    <label className="voice-check">
      <input type="checkbox" id={`voice-local-${item.kind}-toggle`} checked={Boolean(form[item.kind])} onChange={(e) => update(item.kind, e.target.checked)} />
      <span>
        {item.kind === "denoise"
          ? "启用前端降噪（保存后重启生效）"
          : item.kind === "kws"
            ? "启用命令词抗噪（随本地判停运行，保存后重启生效）"
            : "启用语义判停（保存后重启生效）"}
      </span>
    </label>
  );
  const renderTable = (item) => {
    const selectedId = form[`${item.kind}_model`];
    const activeOn = Boolean(form[item.kind]);
    return (
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
              // 降噪有两个模型但顶部没有降噪下拉：在表里选用哪一个（2026-11-23 n109，之前 DPDFNet 下载后选不到）
              const pickedDenoise = item.kind === "denoise" && !activeOn && model.id === selectedId;
              const canPickDenoise = item.kind === "denoise" && Boolean(model.installed) && model.id !== selectedId;
              return (
                <tr key={model.id} id={`voice-local-row-${model.id}`} className={chosen ? "on" : ""}>
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
                      {isRecommendedLocalModel(item.kind, model.id) && (
                        <span className="ob-pill ob-pill--ok voice-tag" id={`voice-local-recommended-${model.id}`}>
                          推荐
                        </span>
                      )}
                      {chosen && <span className="ob-pill ob-pill--ok voice-tag">当前选用</span>}
                      {pickedDenoise && <span className="ob-pill ob-pill--info voice-tag">已选，开启后使用</span>}
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
                        <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => pickFallbackTts(model)} id={`voice-local-fallback-${model.id}`}>
                          设为云端兜底
                        </button>
                      )}
                      {canPickDenoise && (
                        <button type="button" className="btn btn-ghost" disabled={Boolean(busy)} onClick={() => update("denoise_model", model.id)} id={`voice-local-pick-${model.id}`}>
                          选用
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
    );
  };

  // 2026-11-25：本地模型按顶部“当前语音链路”的执行顺序排列（1 判停 → 2 识别 → 3 Jev → 4 大模型 → 5 合成），
  // 不在链路里的视觉理解、声纹放最后；序号与顶部各栏一致，方便对照
  const renderOther = (kind, order = "") => {
    const item = layout.others.find((group) => group.kind === kind);
    if (!item) return null;
    return (
      <div className="voice-local-group" id={`voice-local-group-${item.kind}`}>
        {renderHead(item, `${order}${item.title}模型包`)}
        {item.kind === "tts" &&
          (form.tts_model === "qwen3_tts_0_6b" || item.models.some((m) => m.id === "qwen3_tts_0_6b" && m.installed)) &&
          local.speech_env?.summary && (
            <div className="voice-hint" id="voice-local-speech-env-hint">
              Qwen3-TTS 本机运行时（speech.cpp）：{local.speech_env.summary}
            </div>
          )}
        {renderTable(item)}
        {/* 2026-11-25：音色与语速从卡片底部挪到合成模型包里，选完模型就近调整，仍随“保存本地模型设置”提交 */}
        {item.kind === "tts" && (
          <LocalTtsSettings
            form={form}
            update={update}
            models={models}
            voices={voices}
            onBrowse={() => {
              const target = models.find((m) => m.kind === "tts" && m.id === form.tts_model) || models.find((m) => m.kind === "tts");
              if (target) setTtsVoicesModalModel(target);
            }}
          />
        )}
      </div>
    );
  };

  return (
    <VoiceGroup id="local-models" title="本地模型与音色" summary={on.length ? `已开启：${on.join("、")}` : "未开启"} dirty={dirty}>
      <div id="voice-local-models" className="voice-stack">
        {!local.helper_found && (
          <div className="voice-hint voice-hint--warn">没有找到本地模型程序 moss-voice-local（应与 MOSS 放在同一目录）。可以先下载模型，但打开本地开关后内置服务将无法启动。</div>
        )}
        <p className="voice-muted">
          断网可用的本地模型在这里下载和管理，按上方「当前语音链路」的顺序排列；启用哪个模型在上方下拉框切换，选了还没下载的模型会直接跳到这里它那一行。
        </p>
        <ModelPairingGuide
          settings={baseline}
          models={models}
          busy={builtin.busy}
          onApply={applyCombo}
          onDownload={(ids) => Promise.all(ids.map((id) => modelAct(id, "下载模型", () => voiceApi.localDownload(id))))}
        />
        {/* 2026-11-23 n109：命令词抗噪 / 降噪 / 语义判停并入“拾音判停”，实测没提升的收进“更多 / 实验” */}
        <div className="voice-local-group" id="voice-local-group-vad">
          {renderHead(layout.vadMain, "1. 拾音判停模型包", true, "replace")}
          <p className="voice-muted voice-role-note" id="voice-local-vad-legend">
            判停模型几选一：同时只用一个，在顶部“拾音判停”下拉里换，都不选就是能量判停。命令词抗噪、前端降噪、语义判停是叠加在判停上的功能，可以同时开。
          </p>
          {renderTable(layout.vadMain)}
          {layout.addons.map((item) => (
            <div className="voice-local-addon" key={item.kind} id={`voice-local-group-${item.kind}`}>
              {renderHead(item, item.title, true, "addon")}
              {renderNote(item.kind)}
              {renderToggle(item)}
              {renderTable(item)}
            </div>
          ))}
          <details className="voice-local-more" id="voice-local-vad-more" open={moreOpen} onToggle={(e) => setMoreOverride(e.currentTarget.open)}>
            <summary>
              更多：其他判停模型（替换）与叠加功能（实测无明显提升）
              <small>旧版判停、TEN-VAD 与上面的判停模型几选一；前端降噪、语义判停叠加在判停上，一般不用开</small>
            </summary>
            {layout.vadMore.models.length > 0 && (
              <div className="voice-local-addon" id="voice-local-group-vad-more">
                {renderHead(layout.vadMore, "其他判停模型", false, "replace")}
                <p className="voice-muted voice-role-note">与上面的 Silero v6.2 几选一，同时只用一个；在顶部“拾音判停”下拉里换。</p>
                {renderTable(layout.vadMore)}
              </div>
            )}
            {layout.experiments.map((item) => (
              <div className="voice-local-addon" key={item.kind} id={`voice-local-group-${item.kind}`}>
                {renderHead(item, item.title, true, "addon")}
                {renderNote(item.kind)}
                {renderToggle(item)}
                {renderTable(item)}
              </div>
            ))}
          </details>
        </div>
        {renderOther("asr", "2. ")}
        {models.filter((model) => model.kind === "jev").length > 0 && (
          <div className="voice-local-group" id="voice-local-group-jev">
            <div className="voice-field-head">
              <b>3. Jev 前置粗筛模型包（OneJev / Laya）</b>
              <span className="voice-muted">
                {form.jev_filter_enabled && isLocalJevModel(form.jev_model)
                  ? `当前选用：${models.find((m) => m.id === canonicalLocalJevModel(form.jev_model))?.label || form.jev_model}`
                  : form.jev_filter_enabled
                    ? `顶部使用：云端 Jev（${form.jev_model}）`
                    : "顶部未启用（下载后在顶部第 3 栏选用）"}
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
                        <tr key={model.id} id={`voice-local-row-${model.id}`} className={chosen ? "on" : ""}>
                          <td className="voice-model-cell">
                            <div className="voice-model-name">
                              <b>{model.label}</b>
                              {chosen && <span className="ob-pill ob-pill--ok voice-tag">当前选用</span>}
                              <span className="ob-pill ob-pill--info voice-tag">
                                {isOneJevRow ? "免 Python" : "需 Python 环境"}
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
        {/* 第 4 栏大模型没有断网可用的本地模型，这里只有 Colab T4 联动（它同时接管大模型、看图与合成） */}
        <ColabBridgeSection builtin={builtin} />
        {renderOther("tts", "5. ")}
        {renderOther("vision")}
        {models.filter((model) => model.kind === "speaker").map((model) => {
          const view = localModelView(model);
          return (
            <div className="voice-local-group" key={model.id}>
              <b>{model.label}</b>
              <p className="voice-muted">
                {model.note} {formatBytes(model.size)} · {view.text}
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
        {/* 2026-11-24 n111：小智全量补字库不是可选模型，内置服务启动后自动下载、下好即生效，界面不列出（用户决定） */}
        <div className="voice-form-grid">
          <label className="voice-wide">
            <span>模型下载地址（留空用官方地址，支持镜像，下载均校验 SHA-256）</span>
            <input className="text-input mono" value={form.mirror} placeholder={local.default_mirror || ""} onChange={(e) => update("mirror", e.target.value)} spellCheck="false" autoComplete="off" />
            {local.models_dir && <small className="voice-muted mono">本地模型存放目录：{local.models_dir}</small>}
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
