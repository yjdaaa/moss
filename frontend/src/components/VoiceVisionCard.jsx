import React, { useEffect, useState } from "react";
import { FlaskConical } from "lucide-react";
import { voiceApi } from "../voiceApi.js";
import { formDirty, formatBytes, savedNoteText, visionSource, visionSourcePatch, visionRuntimeText } from "../voiceState.js";
import { SaveBar, VoiceGroup, focusVoiceGroup } from "./VoiceLayout.jsx";

const defaults = { enabled: false, url: "", model: "", use_dialog_llm: false, keep_in_history: false, local_model: "" };

export default function VoiceVisionCard() {
  const [saved, setSaved] = useState(defaults);
  const [config, setConfig] = useState(defaults);
  const [loaded, setLoaded] = useState(false);
  const [hasKey, setHasKey] = useState(false);
  const [ready, setReady] = useState(false);
  const [dialog, setDialog] = useState({ configured: false, url: "", model: "" });
  const [localModels, setLocalModels] = useState([]);
  const [runtime, setRuntime] = useState(null);
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
    setReady(!!state.ready);
    setDialog(state.dialog_llm || { configured: false, url: "", model: "" });
    setLocalModels(Array.isArray(state.local_models) ? state.local_models : []);
    setRuntime(state.local_runtime || null);
    setKeyChange("keep");
    setKeyValue("");
    setLoaded(true);
  };
  useEffect(() => {
    let mounted = true;
    voiceApi.visionStatus().then((state) => { if (mounted) apply(state); }).catch((e) => { if (mounted) setLoadError(e.message); });
    return () => { mounted = false; };
  }, []);
  // 本地视觉模型加载中时每 2 秒刷新一次运行状态（只更新状态，不覆盖正在编辑的表单）
  useEffect(() => {
    if (runtime?.state !== "starting") return undefined;
    const timer = setInterval(() => {
      voiceApi.visionStatus().then((state) => {
        setRuntime(state.local_runtime || null);
        setReady(!!state.ready);
      }).catch(() => {});
    }, 2000);
    return () => clearInterval(timer);
  }, [runtime?.state]);
  const source = visionSource(config);
  const localPicked = localModels.find((item) => item.id === config.local_model);
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
  const savedSource = visionSource(saved);
  const savedLocalLabel = localModels.find((item) => item.id === saved.local_model)?.label || saved.local_model;
  const summary = loadError
    ? "读取失败"
    : !loaded
      ? "读取中…"
      : !saved.enabled
        ? "未开启"
        : savedSource === "dialog"
          ? `已开启 · 对话模型 ${dialog.model || "未配置"}`
          : savedSource === "local"
            ? `已开启 · 本地 ${savedLocalLabel}`
            : `已开启 · ${saved.model || "未填模型"}`;
  return (
    <VoiceGroup id="vision" title="视觉理解" label="语音视觉模型" summary={summary} tone={saved.enabled ? "ok" : undefined} dirty={dirty}>
      <p className="voice-muted">默认关闭。开启后，已允许的小智设备可以上传图片请模型解释；不会自动打开摄像头，结果也不能控制机器。选云端或对话大模型时图片会发给对应服务商，可能产生费用；选本地视觉模型时图片只在这台电脑上处理。</p>
      {loadError && <div className="voice-hint voice-hint--bad" role="alert">{loadError}</div>}
      <div className="voice-form-grid">
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={config.enabled} disabled={busy || testing} onChange={(e) => update("enabled", e.target.checked)} id="voice-vision-enabled" />
          <span>启用视觉理解</span>
        </label>
        <label className="voice-wide">
          <span>看图用哪个模型</span>
          <select
            className="text-input"
            value={source}
            disabled={busy || testing}
            onChange={(e) => {
              setConfig((current) => ({ ...current, ...visionSourcePatch(e.target.value, current, localModels) }));
              setResult(null);
            }}
            id="voice-vision-source"
          >
            <option value="local">本地视觉模型（断网可用，照片不出电脑）</option>
            <option value="cloud">云端视觉模型（填地址与密钥）</option>
            <option value="dialog">当前对话大模型（需支持看图）</option>
          </select>
        </label>
        {source === "local" && (
          <>
            <label className="voice-wide">
              <span>本地视觉模型</span>
              <select className="text-input" value={config.local_model} disabled={busy || testing} onChange={(e) => update("local_model", e.target.value)} id="voice-vision-local-model">
                {localModels.map((item) => (
                  <option key={item.id} value={item.id}>
                    {item.label}（{formatBytes(item.size)}{item.installed ? "" : "，未下载"}）
                  </option>
                ))}
              </select>
            </label>
            {localPicked && !localPicked.installed ? (
              <div className="voice-hint voice-hint--bad voice-wide voice-inline" role="status" id="voice-vision-local-missing">
                <span>{localPicked.label} 还没下载：先到“本地模型”的“视觉理解”里下载，再回来保存。</span>
                <button type="button" className="voice-link-btn" onClick={() => focusVoiceGroup("local-models", "voice-local-group-vision")} id="voice-vision-local-goto">
                  去下载
                </button>
              </div>
            ) : (
              runtime && saved.local_model === config.local_model && saved.enabled && (
                <div className={`voice-hint voice-wide voice-hint--${runtime.state === "failed" ? "bad" : runtime.state === "ready" ? "ok" : ""}`} role="status" id="voice-vision-local-runtime">
                  {visionRuntimeText(runtime)}
                </div>
              )
            )}
          </>
        )}
        {source === "dialog" && (
          <div className={`voice-hint voice-wide voice-hint--${dialog.configured ? "ok" : "bad"}`} role="status">
            {dialog.configured
              ? `照片会发给当前对话模型 ${dialog.model}（${dialog.url}）。`
              : "还没有可用的对话模型：请先在“模型”里填好 OpenAI 兼容地址与模型名并保存密钥。"}
          </div>
        )}
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={config.keep_in_history} disabled={busy || testing || !config.enabled} onChange={(e) => update("keep_in_history", e.target.checked)} id="voice-vision-keep-image" />
          <span>把这张照片留在对话上下文里（多模态；后续追问会重发这张图，费用更高，且要求对话模型支持看图）</span>
        </label>
        {source === "cloud" && (
          <>
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
          </>
        )}
        {keyChange === "set" && source === "cloud" && (
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
          <button type="button" className="btn btn-ghost" disabled={busy || testing || dirty || !ready} onClick={test} title={dirty ? "先保存再测试" : "只发送程序生成的小图，不调用摄像头"}>
            <FlaskConical size={13} /> {testing ? "测试中…" : "测试模型"}
          </button>
        }
      />
    </VoiceGroup>
  );
}
