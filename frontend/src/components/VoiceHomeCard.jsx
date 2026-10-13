import React, { useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";

// NV15 stores inert settings and a separate local token, never connecting to
// HA or issuing control requests before the dedicated production gate.
const EMPTY = { enabled: false, base_url: "", entities: [] };
const asForm = (settings) => ({
  ...EMPTY,
  ...(settings || {}),
  enabled: false,
  entities: Array.isArray(settings?.entities) ? settings.entities.map((entry) => ({
    entity_id: String(entry.entity_id || ""),
    alias: String(entry.alias || ""),
    enabled: !!entry.enabled,
    policy: "ask",
    media: Array.isArray(entry.media) ? entry.media : [],
  })) : [],
});

export default function VoiceHomeCard() {
  const [form, setForm] = useState(EMPTY);
  const [loaded, setLoaded] = useState(false);
  const [revision, setRevision] = useState(null);
  const [credentialRevision, setCredentialRevision] = useState(null);
  const [recoveryOnly, setRecoveryOnly] = useState(false);
  const [credentialConfigured, setCredentialConfigured] = useState(false);
  const [voiceReadEnabled, setVoiceReadEnabled] = useState(false);
  const [voiceControlEnabled, setVoiceControlEnabled] = useState(false);
  const [portableBackup, setPortableBackup] = useState("");
  const [backupAudit, setBackupAudit] = useState(null);
  const [token, setToken] = useState("");
  const [discovery, setDiscovery] = useState(null);
  const [syncEnabled, setSyncEnabled] = useState(false);
  const [syncedAt, setSyncedAt] = useState(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");

  useEffect(() => {
    let alive = true;
    voiceApi.homeStatus().then((data) => {
      if (!alive) return;
      if (!Number.isSafeInteger(data.revision) || !data.settings) {
        setError(data.reason || "Home Assistant 配置无法安全读取");
        return;
      }
      setForm(asForm(data.settings));
      setRevision(data.revision);
      setCredentialRevision(data.credential_revision);
      setCredentialConfigured(!!data.key_configured);
      setRecoveryOnly(!!data.recovery_only);
      setVoiceReadEnabled(!!data.voice_read_enabled);
      setVoiceControlEnabled(!!data.voice_control_enabled);
      setDiscovery(null);
      setSyncEnabled(false);
      setLoaded(true);
    }).catch((e) => {
      if (alive) setError(e?.message || "Home Assistant 离线配置状态读取失败");
    });
    return () => { alive = false; };
  }, []);

  const reload = async () => {
    setError("");
    setMessage("");
    try {
      const data = await voiceApi.homeStatus();
      if (!Number.isSafeInteger(data.revision) || !data.settings) throw new Error(data.reason || "配置不可读取");
      setForm(asForm(data.settings));
      setRevision(data.revision);
      setCredentialRevision(data.credential_revision);
      setCredentialConfigured(!!data.key_configured);
      setRecoveryOnly(!!data.recovery_only);
      setVoiceReadEnabled(!!data.voice_read_enabled);
      setVoiceControlEnabled(!!data.voice_control_enabled);
      setDiscovery(null);
      setSyncEnabled(false);
      setLoaded(true);
      setMessage("已重新加载最新白名单；请检查内容后再保存。");
    } catch (e) {
      setError(e?.message || "重新加载白名单失败");
    }
  };


  const exportPortable = async () => {
    setBusy(true); setError(""); setMessage("");
    try {
      const result = await voiceApi.homeBackupExport();
      if (result.credentials_included !== false || result.reenter_token_required !== true
        || typeof result.profile !== "string") throw new Error("安全备份响应不符合预期");
      setPortableBackup(result.profile);
      setMessage("已生成只包含已保存白名单的配置备份。令牌、设备授权和审批不会导出；请手动复制并妥善保管。");
    } catch (e) { setError(e?.message || "生成安全备份失败"); }
    finally { setBusy(false); }
  };
  const auditHistoricalBackup = async () => {
    setBusy(true); setError(""); setMessage(""); setBackupAudit(null);
    try {
      const result = await voiceApi.homeBackupAudit();
      if (result === null) return;
      if (!Number.isSafeInteger(result?.inspected_bytes) || result.inspected_bytes <= 0
        || !["suspected_legacy_credential", "encrypted_credential_material", "unknown_treat_as_sensitive"].includes(result.risk)
        || result.safe_to_share !== false) throw new Error("审计响应不符合安全要求");
      setBackupAudit(result);
    } catch (_e) { setError("审计失败：无法安全读取该文件，未修改历史备份"); }
    finally { setBusy(false); }
  };
  const restorePortable = async () => {
    if (!window.confirm("恢复会清除本机 Home Assistant 令牌、全部语音家居授权和待审批动作；白名单实体需重新勾选，恢复后必须重新输入令牌。确定继续？")) return;
    setBusy(true);setError("");setMessage("");setSyncEnabled(false);setDiscovery(null);
    try {
      if (!Number.isSafeInteger(revision) || !Number.isSafeInteger(credentialRevision))
        throw new Error("请重新加载白名单与令牌版本");
      const status=await voiceApi.homeBackupRestore({
        profile:portableBackup,expected_revision:revision,
        expected_credential_revision:credentialRevision,confirmed:true,
      });
      if (!Number.isSafeInteger(status.revision) || !Number.isSafeInteger(status.credential_revision))
        throw new Error("恢复已提交但状态读取失败，请重新加载");
      setRevision(status.revision);
      setCredentialRevision(status.credential_revision);
      setRecoveryOnly(false);
      setCredentialConfigured(false);
      setVoiceReadEnabled(false);
      setVoiceControlEnabled(false);
      setToken("");
      setForm(asForm(status.settings));
      setMessage("白名单配置已恢复为全部停用；旧令牌和全部家居授权已撤销。请重新输入令牌并逐项核对设备。");
    } catch (e) {
      setCredentialConfigured(false);
      setToken("");
      setError(e?.message || "恢复失败；凭据可能已经撤销，请重新加载");
    } finally { setBusy(false); }
  };
  const saveToken = async (action) => {
    setBusy(true);
    setSyncEnabled(false);
    setError("");
    setMessage("");
    try {
      if (!Number.isSafeInteger(credentialRevision)) throw new Error("请重新加载凭据状态");
      const input = { action, expected_revision: credentialRevision };
      if (action === "set") input.value = token;
      const result = await voiceApi.homeTokenSave(input);
      if (!Number.isSafeInteger(result.credential_revision)) throw new Error("凭据保存回执无效");
      setRecoveryOnly(false);
      setCredentialRevision(result.credential_revision);
      setCredentialConfigured(!!result.key_configured);
      setVoiceReadEnabled(false);
      setVoiceControlEnabled(false);
      setToken("");
      setMessage(action === "clear" ? "Home Assistant 令牌已撤销，旧审批失效；真实控制保持禁用。" : "令牌已保存到独立凭据表；不会回显或写入普通配置。真实控制保持禁用。");
    } catch (e) {
      setToken("");
      setError(e?.code === "home_conflict" ? "凭据版本发生变化，请重新加载。" : e?.message || "令牌操作失败");
    } finally { setBusy(false); }
  };

  const consent = async (enabled) => {
    setBusy(true);
    setError("");
    try {
      const state = await voiceApi.homeVoiceConsent({
        enabled, policy_revision: revision, credential_revision: credentialRevision,
      });
      setVoiceReadEnabled(!!state.voice_read_enabled);
      setMessage(enabled ? "已授权语音模型只读查询白名单实体，真实家居控制仍禁用。" : "已撤销语音模型的只读家居查询权限。");
    } catch (e) {
      setVoiceReadEnabled(false);
      setError(e?.message || "Home Assistant 语音只读授权操作失败，请重新加载。");
    } finally { setBusy(false); }
  };
  const controlConsent = async (enabled) => {
    if (enabled && !window.confirm("允许语音创建白名单家居控制审批？每次仍须在电脑批准。批准后会实际操作对应设备，包括门锁和报警器。")) return;
    setBusy(true);setError("");setMessage("");
    try {
      const result=await voiceApi.homeControlConsent({enabled,confirmed:true,policy_revision:revision,credential_revision:credentialRevision});
      setVoiceControlEnabled(!!result.voice_control_enabled);
      setMessage(enabled ? "已允许语音申请家居控制；每次都需独立桌面审批。" : "家居控制授权已撤销，旧审批无法执行。");
    } catch (e) {setError(e?.message || "控制授权操作失败，请重新加载。");}
    finally {setBusy(false);}
  };

  const discover = async () => {
    setBusy(true);
    setError("");
    setMessage("");
    setDiscovery(null);
    try {
      const result = await voiceApi.homeDiscover();
      if (!Array.isArray(result.entities)) throw new Error("发现响应格式无效");
      if (result.policy_revision !== revision || result.credential_revision !== credentialRevision) {
        throw new Error("查询期间配置已发生改变，请先重新加载");
      }
      setDiscovery(result.entities);
      setMessage("已完成只读同步。未修改白名单，也没有执行任何设备动作。");
    } catch (e) {
      setError(e?.message || "Home Assistant 只读查询失败");
    } finally {setBusy(false);}
  };

  // Opt-in, bounded read-only polling. No home control or auto-authorization.
  useEffect(() => {
    if (!syncEnabled || !loaded || !credentialConfigured) return;
    let active = true;
    let inFlight = false;
    const refresh = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const result = await voiceApi.homeDiscover();
        if (!active) return;
        if (!Array.isArray(result.entities) || result.policy_revision !== revision
          || result.credential_revision !== credentialRevision) {
          setSyncEnabled(false);
          setDiscovery(null);
          setError("配置或令牌已经改变，自动同步已停止；请重新加载白名单。");
          return;
        }
        setDiscovery(result.entities);
        setSyncedAt(new Date().toLocaleTimeString("zh-CN"));
        setError("");
      } catch (e) {
        if (!active) return;
        setDiscovery((old) => old?.map((entry) => ({
          ...entry, state: "unavailable", actions_available: [], attributes: {}, last_updated: "",
        })) ?? null);
        setError(e?.message || "Home Assistant 只读同步断线，当前状态已标记不可用。");
        if (e?.code === "home_unauthorized" || e?.code === "home_conflict") {
          setSyncEnabled(false);
        }
      } finally { inFlight = false; }
    };
    void refresh();
    const timer = setInterval(refresh, 15_000);
    return () => { active = false; clearInterval(timer); };
  }, [syncEnabled, loaded, credentialConfigured, revision, credentialRevision]);

  const update = (index, patch) => setForm((current) => ({
    ...current,
    entities: current.entities.map((entity, i) => i === index ? { ...entity, ...patch } : entity),
  }));

  const add = () => setForm((current) => current.entities.length >= 128
    ? current
    : {
      ...current,
      entities: [...current.entities, {
        entity_id: "", alias: "", enabled: false, policy: "ask", media: [],
      }],
    });

  const remove = (index) => setForm((current) => ({
    ...current, entities: current.entities.filter((_, i) => i !== index),
  }));

  const save = async () => {
    setBusy(true);
    setError("");
    setMessage("");
    try {
      const settings = {
        enabled: false,
        base_url: form.base_url.trim(),
        entities: form.entities.map((entity) => ({
          entity_id: entity.entity_id.trim(),
          alias: entity.alias.trim(),
          enabled: entity.enabled,
          policy: "ask",
          media: entity.media || [],
        })),
      };
      if (!Number.isSafeInteger(revision)) throw new Error("缺少有效配置版本，请重新加载");
      const result = await voiceApi.homeSave({ settings, expected_revision: revision });
      if (!Number.isSafeInteger(result.revision)) throw new Error("保存回执缺少有效版本");
      setRevision(result.revision);
      setForm(asForm(result.settings));
      setDiscovery(null);
      setSyncEnabled(false);
      setMessage("离线白名单已保存。Home Assistant 控制仍被禁用；没有连接任何真实设备。");
    } catch (e) {
      setError(e?.code === "home_conflict" ? "白名单已在其他窗口更新。请点击重新加载，核对后再次保存。" : e?.message || "Home Assistant 白名单保存失败");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="voice-card" aria-label="Home Assistant 离线白名单">
      <h3>Home Assistant（NV15）</h3>
      <p>
        <strong>生产控制禁用。</strong>
        可设置 HTTPS 地址、白名单和独立加密的长期令牌。只有你明确点击同步时才会通过 HTTPS 读取设备状态；不发送控制命令，任何实际设备动作仍须独立桌面审批。
      </p>
      {recoveryOnly && <p role="alert">检测到旧 HA 令牌密文无法由当前 Windows 账户解密。设备查询和控制均已禁用。请先使用下方安全配置备份恢复（会撤销旧密文与授权），或重新设置长期访问令牌；不能继续使用旧审批。</p>}
      <div className="voice-form-group">
        <p>Home Assistant 访问令牌：{credentialConfigured ? "已设置" : "未设置"}（凭据版本 {credentialRevision ?? "未知"}）。令牌不写入普通设置，也不会回显。</p>
        <label><span>长期访问令牌（不会自动连接家居）</span>
          <input type="password" value={token} disabled={!loaded || busy} autoComplete="new-password"
            onChange={(event) => setToken(event.target.value)} placeholder="输入新令牌以替换" />
        </label>
        <div className="voice-form-actions">
          <button type="button" disabled={!loaded || busy || token.length < 20} onClick={() => saveToken("set")}>保存 / 更换令牌</button>
          <button type="button" disabled={!loaded || busy || !credentialConfigured} onClick={() => saveToken("clear")}>撤销令牌</button>
        </div>
      </div>

      <div className="voice-form-group">
        <h4>安全备份与跨电脑迁移</h4>
        <p>这里只导出已保存的 HTTPS 地址和白名单，不导出令牌、DPAPI 密文、审批或设备授权。不要直接复制原始 SQLite/WAL/SHM 作为可移植凭据备份。</p>
        <div className="voice-form-actions">
          <button type="button" disabled={!loaded || busy} onClick={exportPortable}>生成脱敏配置备份</button>
          <button type="button" disabled={busy} onClick={auditHistoricalBackup}>选择历史备份文件并审计（只读）</button>
          <button type="button" disabled={!loaded || busy || !portableBackup.trim() || portableBackup.length > 65536}
            onClick={restorePortable}>从粘贴的备份恢复（撤销旧令牌）</button>
        </div>
        {backupAudit && <p role="status">历史文件只读审计：{backupAudit.risk === "suspected_legacy_credential" ? "疑似包含旧访问令牌" : backupAudit.risk === "encrypted_credential_material" ? "包含凭据加密材料标记" : "无法确认安全，按敏感文件处理"}；读取 {backupAudit.inspected_bytes} 字节。请勿公开分享；若疑似泄漏应在 HA 服务端撤销并更换令牌。未读取或显示令牌内容。</p>}
        <label><span>配置备份 JSON（手动复制或粘贴，最多 64 KiB）</span>
          <textarea value={portableBackup} disabled={busy} rows={5} spellCheck={false}
            onChange={(event) => setPortableBackup(event.target.value)} />
        </label>
        <p>恢复后所有实体处于未选中状态，控制权限不自动启用，必须重新输入令牌并重新审批。</p>
      </div>
      <div className="voice-form-group">
        <p>可手动读取已保存 HTTPS 地址上的设备与服务状态；只发送 GET，不自动添加设备或执行动作。</p>
        <p>语音模型只读查询：{voiceReadEnabled ? "已单独授权" : "关闭"}。该授权绑定当前白名单与令牌版本，任何变更都会使旧授权失效。</p>
        <button type="button" disabled={!loaded || busy || (!credentialConfigured && !voiceReadEnabled)}
          onClick={() => consent(!voiceReadEnabled)}>{voiceReadEnabled ? "撤销语音只读权限" : "授权语音只读查询"}</button>
        <button type="button" disabled={!loaded || busy || !credentialConfigured || !form.base_url.trim()}
          onClick={discover}>只读同步设备状态（不执行控制）</button>
        <label><input type="checkbox" checked={syncEnabled} disabled={!loaded || busy || !credentialConfigured || !revision}
          onChange={(event) => setSyncEnabled(event.target.checked)} />
          开启每 15 秒只读刷新（关闭窗口或改变授权后停止）
        </label>
        {syncedAt && <p role="status">上次只读同步时间：{syncedAt}</p>}
        {Array.isArray(discovery) && <ul aria-label="Home Assistant 已选设备的同步状态">
          {discovery.map((entity) => <li key={entity.entity_id}>
            <strong>{entity.entity_id}</strong>：{entity.state}
            {entity.actions_available?.length ? " · 可用服务：" + entity.actions_available.join("、") : " · 无可用控制服务"}
          </li>)}
        </ul>}
      </div>
      <label className="voice-form-group">
        <span>Home Assistant HTTPS 地址（保存后可手动只读同步）</span>
        <input value={form.base_url} disabled={!loaded || busy} autoComplete="off"
          placeholder="https://ha.example.test:8123/"
          onChange={(event) => setForm((current) => ({
            ...current, base_url: event.target.value,
          }))} />
      </label>
      <p>设备白名单（最多 128 项；每一项都必须经独立审批，绝不自动解锁门锁或解除报警）</p>
      {form.entities.map((entity, index) => (
        <div className="voice-form-group" key={index}>
          <label>
            <span>实体 ID</span>
            <input value={entity.entity_id} disabled={!loaded || busy}
              placeholder="例如 light.desk"
              onChange={(event) => update(index, { entity_id: event.target.value })} />
          </label>
          <label>
            <span>语音别名</span>
            <input value={entity.alias} disabled={!loaded || busy}
              placeholder="例如 书桌灯"
              onChange={(event) => update(index, { alias: event.target.value })} />
          </label>
          <label>
            <input type="checkbox" checked={entity.enabled} disabled={!loaded || busy}
              onChange={(event) => update(index, { enabled: event.target.checked })} />
            加入语音实体白名单（动作仍需逐次审批）
          </label>
          {entity.entity_id.startsWith("media_player.") && <label>
            <span>本地音乐白名单，每行 library_ID|music 或 library_ID|playlist</span>
            <textarea value={(entity.media || []).map((m) => `${m.id}|${m.media_type}`).join("\n")} disabled={!loaded || busy}
              onChange={(event) => update(index, {media:event.target.value.split(/\r?\n/).filter((s) => s.trim()).map((s) => {
                const [id,media_type]=s.split("|");return {id:id?.trim() || "",media_type:media_type?.trim() || ""};
              })})} />
          </label>}
          <button type="button" disabled={!loaded || busy}
            onClick={() => remove(index)}>移除该项</button>
        </div>
      ))}
      <button type="button" disabled={!loaded || busy || form.entities.length >= 128}
        onClick={add}>添加白名单实体</button>
      <button type="button" disabled={!loaded || busy}
        onClick={save}>{busy ? "正在校验与保存…" : "保存离线白名单（不连接设备）"}</button>
      <button type="button" disabled={busy} onClick={reload}>重新加载最新白名单</button>
      {Number.isSafeInteger(revision) && <p>离线配置版本：{revision}</p>}
      {message && <p role="status">{message}</p>}
      {error && <p role="alert">{error}</p>}
      <p role="status">NV15 正式控制安全门禁未通过：实际设备控制和新控制授权均不可用；可撤销已有授权和旧审批。更改白名单或令牌也会使旧审批失效。</p>
      <button type="button" disabled={!loaded || busy || !voiceControlEnabled} onClick={() => controlConsent(false)}>
        {voiceControlEnabled ? "撤销已有家居控制授权" : "家居控制授权未开放"}
      </button>
    </section>
  );
}
