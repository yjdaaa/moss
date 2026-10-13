import React, { useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";

const defaults = { enabled: false, url: "", model: "" };

export default function VoiceVisionCard() {
  const [config, setConfig] = useState(defaults);
  const [hasKey, setHasKey] = useState(false);
  const [keyChange, setKeyChange] = useState("keep");
  const [keyValue, setKeyValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [testing, setTesting] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState("");
  const load = async () => {
    const state = await voiceApi.visionStatus();
    setConfig({ ...defaults, ...(state.settings || {}) });
    setHasKey(!!state.key_configured);
    setKeyChange("keep");
    setKeyValue("");
  };
  useEffect(() => {
    let mounted = true;
    voiceApi.visionStatus().then((state) => {
      if (!mounted) return;
      setConfig({ ...defaults, ...(state.settings || {}) });
      setHasKey(!!state.key_configured);
    }).catch((e) => { if (mounted) setError(e.message); });
    return () => { mounted = false; };
  }, []);
  const update = (field, value) => setConfig((current) => ({ ...current, [field]: value }));
  const save = async () => {
    setBusy(true); setError(""); setResult("");
    try {
      await voiceApi.visionSave({ settings: config, key: { action: keyChange, value: keyChange === "set" ? keyValue : "" } });
      await load();
      setResult("视觉配置已保存。仅当设备已授权且视觉模型配置完整时，下次 MCP 初始化才会提供上传能力。");
    } catch (e) { setError(e.message || "保存失败"); }
    finally { setBusy(false); }
  };
  const test = async () => {
    setTesting(true); setError(""); setResult("");
    try {
      const result = await voiceApi.visionTest();
      setResult(`模型测试成功（${result.elapsed_ms} ms）：${result.preview || "已返回结果"}`);
    } catch (e) { setError(e.message || "视觉模型连接失败"); }
    finally { setTesting(false); }
  };
  return <section className="voice-card" aria-label="语音视觉模型">
    <h3>视觉理解（NV12）</h3>
    <p>默认关闭。启用后，已授权的小智设备可主动上传图片让配置的大模型解释。<strong>不会自动开启电脑或设备摄像头，也不会让图片结果控制机器。</strong> 图片会发送给你配置的模型供应商，可能产生费用。</p>
    <label className="voice-form-group">
      <input type="checkbox" checked={config.enabled} disabled={busy || testing}
        onChange={(e) => update("enabled", e.target.checked)} /> 启用视觉理解
    </label>
    <label className="voice-form-group">OpenAI 兼容模型地址
      <input value={config.url} disabled={busy || testing} placeholder="https://example.com/v1"
        onChange={(e) => update("url", e.target.value)} />
    </label>
    <label className="voice-form-group">视觉模型名称
      <input value={config.model} disabled={busy || testing} placeholder="视觉模型 ID"
        onChange={(e) => update("model", e.target.value)} />
    </label>
    <div className="voice-form-group">
      <label>API 密钥：{hasKey ? "已保存" : "未设置"}</label>
      <select aria-label="视觉模型密钥操作" value={keyChange} disabled={busy || testing}
        onChange={(e) => { setKeyChange(e.target.value); setKeyValue(""); }}>
        <option value="keep">保留原密钥</option>
        <option value="set">设置新密钥</option>
        <option value="clear">清除密钥</option>
      </select>
      {keyChange === "set" &&
        <input aria-label="新的视觉模型 API 密钥" type="password" autoComplete="new-password"
          value={keyValue} disabled={busy || testing} onChange={(e) => setKeyValue(e.target.value)} />}
    </div>
    {error && <p role="alert">{error}</p>}
    {result && <p role="status">{result}</p>}
    <button type="button" disabled={busy || testing} onClick={save}>{busy ? "保存中…" : "保存视觉配置"}</button>
    <button type="button" disabled={busy || testing || !config.enabled || !hasKey} onClick={test}>
      {testing ? "测试中…" : "用程序生成的图片测试模型"}
    </button>
    <p>测试按钮只发送程序在内存中生成的小图，不调用摄像头；请先保存设置再测试。</p>
  </section>;
}
