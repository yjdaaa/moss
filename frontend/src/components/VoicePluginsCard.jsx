import React, { useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";

const INITIAL = {
  weather_enabled: false, weather_host: "", weather_latitude: null, weather_longitude: null, weather_auth: "api_key",
  lunar_enabled: false, lunar_utc_offset_minutes: 480,
  news_enabled: false, news_sources: [],
  search_enabled: false, search_provider: "tavily", search_host: "https://api.tavily.com", metaso_host: "https://metaso.cn",
  rag_enabled: false, rag_host: "", rag_dataset_ids: [],
};
const KEY_KINDS = ["weather", "search", "metaso", "rag"];
const emptyKeys = () => Object.fromEntries(KEY_KINDS.map((kind) => [kind, { action: "keep", value: "" }]));
const parseLines = (text) => text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean).map((line) => {
  const index = line.indexOf("|");
  if (index < 0) return { id: "", url: "", kind: "rss", source_id: "" };
  const pieces = line.split("|").map((value) => value.trim());
  if (pieces.length === 2) return { id: pieces[0], url: pieces[1], kind: "rss", source_id: "" };
  if (pieces.length === 4 && pieces[1] === "newsnow") return { id: pieces[0], kind: "newsnow", url: pieces[2], source_id: pieces[3] };
  return { id: "", url: "", kind: "rss", source_id: "" };
});

export default function VoicePluginsCard() {
  const [config, setConfig] = useState(INITIAL);
  const [keyStatus, setKeyStatus] = useState({});
  const [keyChanges, setKeyChanges] = useState(emptyKeys);
  const [sources, setSources] = useState("");
  const [datasets, setDatasets] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const load = async () => {
    const data = await voiceApi.pluginsStatus();
    const next = { ...INITIAL, ...(data.settings || {}) };
    setConfig(next);
    setKeyStatus(data.keys || {});
    setSources((next.news_sources || []).map((s) => s.kind === "newsnow" ? [s.id, "newsnow", s.url, s.source_id].join("|") : [s.id, s.url].join("|")).join("\n"));
    setDatasets((next.rag_dataset_ids || []).join("\n"));
    setKeyChanges(emptyKeys());
  };
  useEffect(() => {
    let active = true;
    voiceApi.pluginsStatus().then((data) => {
      if (!active) return;
      const next = { ...INITIAL, ...(data.settings || {}) };
      setConfig(next);
      setKeyStatus(data.keys || {});
      setSources((next.news_sources || []).map((s) => s.kind === "newsnow" ? [s.id, "newsnow", s.url, s.source_id].join("|") : [s.id, s.url].join("|")).join("\n"));
      setDatasets((next.rag_dataset_ids || []).join("\n"));
    }).catch((e) => { if (active) setError(e.message); }).finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, []);
  const set = (name, value) => setConfig((old) => ({ ...old, [name]: value }));
  const keyUpdate = (kind, patch) => setKeyChanges((old) => ({ ...old, [kind]: { ...old[kind], ...patch } }));
  const save = async () => {
    setError(""); setMessage(""); setSaving(true);
    try {
      const nextSources = parseLines(sources);
      if (sources.trim() && nextSources.some((row) => !row.id || !row.url || (row.kind === "newsnow" && !row.source_id))) throw new Error("RSS：编号|HTTPS地址；NewsNow：编号|newsnow|服务地址|来源ID，每行一个");
      const next = {
        ...config,
        news_sources: nextSources,
        rag_dataset_ids: datasets.split(/[\n,]/).map((id) => id.trim()).filter(Boolean),
      };
      await voiceApi.pluginsSave({ settings: next, keys: keyChanges });
      await load();
      setMessage("插件设置已保存。下轮语音对话会重新读取开关与权限。");
    } catch (e) {
      setError(e.message || "保存失败");
    } finally {
      setSaving(false);
    }
  };
  const field = (name, label, placeholder = "", type = "text") => (
    <label className="voice-form-group" key={name}>
      <span>{label}</span>
      <input type={type} placeholder={placeholder} disabled={saving}
        value={config[name] ?? ""} onChange={(e) => set(name, type === "number" ? (e.target.value ? Number(e.target.value) : null) : e.target.value)} />
    </label>
  );
  const toggle = (name, label) => (
    <label className="voice-form-group">
      <input type="checkbox" disabled={saving} checked={!!config[name]} onChange={(e) => set(name, e.target.checked)} /> {label}
    </label>
  );
  const keyField = (kind, label) => (
    <div className="voice-form-group">
      <label>{label}：{keyStatus[kind] ? "已保存密钥" : "未设置密钥"}</label>
      <select aria-label={label + " 密钥操作"} value={keyChanges[kind].action} disabled={saving}
        onChange={(e) => keyUpdate(kind, { action: e.target.value, value: "" })}>
        <option value="keep">保留原密钥</option>
        <option value="set">设置新密钥</option>
        <option value="clear">清除密钥</option>
      </select>
      {keyChanges[kind].action === "set" && <input aria-label={label + " 新密钥"} type="password" autoComplete="new-password" value={keyChanges[kind].value} disabled={saving}
        onChange={(e) => keyUpdate(kind, { value: e.target.value })} />}
    </div>
  );
  return <section className="voice-card" aria-label="语音信息插件">
    <h3>信息插件（NV11）</h3>
    <p>全部默认关闭。启用后，你的天气位置、搜索词或知识库问题会发送到已配置的服务商，可能产生 API 费用。外部资料不授予任何控制设备的权限。</p>
    {loading ? <p>正在读取插件配置…</p> : <>
      <h4>天气 · 和风天气</h4>
      {toggle("weather_enabled", "启用实时天气")}
      {field("weather_host", "和风 API Host", "https://你的专属host")}
      {field("weather_latitude", "默认纬度", "39.9", "number")}
      {field("weather_longitude", "默认经度", "116.4", "number")}
      <label className="voice-form-group">鉴权方式
        <select value={config.weather_auth} disabled={saving} onChange={(e) => set("weather_auth", e.target.value)}>
          <option value="api_key">X-QW-Api-Key</option><option value="bearer">Bearer JWT</option>
        </select>
      </label>
      {keyField("weather", "天气 API 密钥")}
      <h4>农历</h4>
      {toggle("lunar_enabled", "启用本机农历（不联网）")}
      {field("lunar_utc_offset_minutes", "时区 UTC 偏移（分钟，北京 480）", "480", "number")}
      <h4>新闻 · RSS</h4>
      {toggle("news_enabled", "启用新闻 RSS")}
      <label className="voice-form-group">新闻来源白名单：RSS「编号|HTTPS地址」或 NewsNow「编号|newsnow|服务地址|来源ID」
        <textarea rows={3} disabled={saving} value={sources} onChange={(e) => setSources(e.target.value)}
          placeholder={"tech|https://example.com/feed.xml"} />
      </label>
      <h4>联网搜索</h4>
      {toggle("search_enabled", "启用联网搜索")}
      <label className="voice-form-group">搜索服务
        <select aria-label="搜索服务" value={config.search_provider} disabled={saving} onChange={(e) => set("search_provider", e.target.value)}>
          <option value="tavily">Tavily</option><option value="metaso">秘塔 Metaso</option>
        </select>
      </label>
      {field("search_host", "Tavily API Host", "https://api.tavily.com")}
      {keyField("search", "Tavily API 密钥")}
      {field("metaso_host", "秘塔 API Host", "https://metaso.cn")}
      {keyField("metaso", "秘塔 API 密钥")}
      <p>两家服务分别保存密钥，只调用当前选择的一家；超时或失败不会自动切换或重试。</p>
      <h4>知识库 · RAGFlow</h4>
      {toggle("rag_enabled", "启用知识库检索")}
      {field("rag_host", "RAGFlow Host", "https://rag.example.com")}
      <label className="voice-form-group">数据集白名单，每行一个 Dataset ID
        <textarea rows={3} disabled={saving} value={datasets} onChange={(e) => setDatasets(e.target.value)} />
      </label>
      {keyField("rag", "RAGFlow API 密钥")}
      {error && <p role="alert">{error}</p>}
      {message && <p role="status">{message}</p>}
      <button type="button" className="btn btn-primary" disabled={saving} onClick={save}>{saving ? "正在保存…" : "保存插件设置"}</button>
    </>}
  </section>;
}
