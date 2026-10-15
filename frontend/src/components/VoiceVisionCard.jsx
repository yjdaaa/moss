import React, { useEffect, useState } from "react";
import { FlaskConical } from "lucide-react";
import { voiceApi } from "../voiceApi.js";
import { formDirty, savedNoteText } from "../voiceState.js";
import { SaveBar, VoiceGroup } from "./VoiceLayout.jsx";

const defaults = { enabled: false, url: "", model: "" };

export default function VoiceVisionCard() {
  const [saved, setSaved] = useState(defaults);
  const [config, setConfig] = useState(defaults);
  const [loaded, setLoaded] = useState(false);
  const [hasKey, setHasKey] = useState(false);
  const [keyChange, setKeyChange] = useState("keep");
  const [keyValue, setKeyValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [testing, setTesting] = useState(false);
  const [loadError, setLoadError] = useState("");
  const [result, setResult] = useState(null);
  const [testResult, setTestResult] = useState(null);
  const dirty = loaded && (formDirty(saved, config) || keyChange !== "keep");

  const apply = (state) => {
    const next = { ...defaults, ...(state.settings || {}) };
    setSaved(next);
    setConfig(next);
    setHasKey(!!state.key_configured);
    setKeyChange("keep");
    setKeyValue("");
    setLoaded(true);
  };
  useEffect(() => {
    let mounted = true;
    voiceApi.visionStatus().then((state) => { if (mounted) apply(state); }).catch((e) => { if (mounted) setLoadError(e.message); });
    return () => { mounted = false; };
  }, []);
  const update = (field, value) => {
    setConfig((current) => ({ ...current, [field]: value }));
    setResult(null);
  };
  const save = async () => {
    setBusy(true);
    try {
      await voiceApi.visionSave({ settings: config, key: { action: keyChange, value: keyChange === "set" ? keyValue : "" } });
    } catch (e) {
      setResult({ ok: false, text: `保存失败：${e.message || "保存失败"}` });
      setBusy(false);
      return;
    }
    let effect = "设备下次连接时生效";
    try {
      apply(await voiceApi.visionStatus());
    } catch (e) {
      // 已经保存成功，只是读回状态失败：按提交的内容记为已保存，不能报“保存失败”
      setSaved(config);
      setHasKey(keyChange === "set" || (keyChange === "keep" && hasKey));
      setKeyChange("keep");
      setKeyValue("");
      effect += ` · 读取最新状态失败：${e.message || "未知错误"}`;
    }
    setResult({ ok: true, text: savedNoteText("视觉设置", effect) });
    setBusy(false);
  };
  const test = async () => {
    setTesting(true);
    setTestResult(null);
    try {
      const outcome = await voiceApi.visionTest();
      setTestResult({ ok: true, text: `测试通过（${outcome.elapsed_ms} ms）：${outcome.preview || "已返回结果"}` });
    } catch (e) {
      setTestResult({ ok: false, text: e.message || "视觉模型连接失败" });
    } finally {
      setTesting(false);
    }
  };
  const summary = loadError ? "读取失败" : !loaded ? "读取中…" : saved.enabled ? `已开启 · ${saved.model || "未填模型"}` : "未开启";
  return (
    <VoiceGroup id="vision" title="视觉理解" label="语音视觉模型" summary={summary} tone={saved.enabled ? "ok" : undefined} dirty={dirty}>
      <p className="voice-muted">默认关闭。开启后，已允许的小智设备可以上传图片请模型解释；不会自动打开摄像头，结果也不能控制机器。图片会发给你配置的模型服务商，可能产生费用。</p>
      {loadError && <div className="voice-hint voice-hint--bad" role="alert">{loadError}</div>}
      <div className="voice-form-grid">
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={config.enabled} disabled={busy || testing} onChange={(e) => update("enabled", e.target.checked)} id="voice-vision-enabled" />
          <span>启用视觉理解</span>
        </label>
        <label>
          <span>模型地址（OpenAI 兼容）</span>
          <input className="text-input mono" value={config.url} disabled={busy || testing} placeholder="https://example.com/v1" onChange={(e) => update("url", e.target.value)} />
        </label>
        <label>
          <span>视觉模型名称</span>
          <input className="text-input mono" value={config.model} disabled={busy || testing} placeholder="视觉模型 ID" onChange={(e) => update("model", e.target.value)} />
        </label>
        <label>
          <span>密钥（{hasKey ? "已保存" : "未设置"}）</span>
          <select className="text-input" aria-label="视觉模型密钥操作" value={keyChange} disabled={busy || testing} onChange={(e) => { setKeyChange(e.target.value); setKeyValue(""); setResult(null); }}>
            <option value="keep">保留原密钥</option>
            <option value="set">设置新密钥</option>
            <option value="clear">清除密钥</option>
          </select>
        </label>
        {keyChange === "set" && (
          <label>
            <span>新密钥</span>
            <input className="text-input" aria-label="新的视觉模型 API 密钥" type="password" autoComplete="new-password" value={keyValue} disabled={busy || testing} onChange={(e) => setKeyValue(e.target.value)} />
          </label>
        )}
      </div>
      {testResult && <div className={`voice-hint voice-hint--${testResult.ok ? "ok" : "bad"}`} role={testResult.ok ? "status" : "alert"}>{testResult.text}</div>}
      <SaveBar
        id="voice-vision-save"
        target="视觉设置"
        dirty={dirty}
        busy={busy || testing}
        saving={busy}
        result={result}
        onSave={save}
        extra={
          <button type="button" className="btn btn-ghost" disabled={busy || testing || dirty || !saved.enabled || !hasKey} onClick={test} title={dirty ? "先保存再测试" : "只发送程序生成的小图，不调用摄像头"}>
            <FlaskConical size={13} /> {testing ? "测试中…" : "测试模型"}
          </button>
        }
      />
    </VoiceGroup>
  );
}
