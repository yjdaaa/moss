import { useEffect, useRef, useState } from "react";
import { voiceApi } from "../../voiceApi.js";
import {
  formDirty,
  localGroupPatch,
  localSettingsForm,
  mergeGroup,
  syncLocalDraft,
  savedNoteText,
  voiceErrorText,
} from "../../voiceState.js";
import { restartEffect } from "./shared.js";

/**
 * 本地模型与语音体验共用同一份已保存设置，各自只提交自己的字段，也只检查自己的字段：
 * 另一组已保存的值即使不合前端规则，也不挡本组保存（由后端最终校验）。
 */
/** 后端热更新、不重启内置服务的字段（与 `local_save` 的 hot_only 一致）。 */
const HOT_LOCAL_KEYS = ["screen_card_style", "persona_style", "persona_prompt", "custom_personas"];

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
      const patch = localGroupPatch(baseline, draft, keys);
      const next = await voiceApi.localSave(patch);
      const nextForm = localSettingsForm(next.local?.settings);
      prevBaselineRef.current = nextForm;
      builtin.setStatus(next);
      setDraft(nextForm);
      const hotOnly = Object.keys(patch).every((key) => HOT_LOCAL_KEYS.includes(key));
      setResult({ ok: true, text: savedNoteText(target, hotOnly ? "已立即生效，无需重启" : restartEffect(next)) });
    } catch (e) {
      setResult({ ok: false, text: `保存失败：${voiceErrorText(e)}` });
    } finally {
      builtin.setBusy("");
    }
  };
  return { local, models, baseline, draft, setDraft, update, dirty, error, result, setResult, save };
}

export { useLocalGroup };
