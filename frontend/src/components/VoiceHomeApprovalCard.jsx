import React, { useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";

export default function VoiceHomeApprovalCard() {
  const [items, setItems] = useState([]);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const refresh = async () => {
    try {
      const data = await voiceApi.homeApprovals();
      if (!Array.isArray(data.approvals) || typeof data.can_control !== "boolean") throw new Error("家居审批安全状态不可验证");
      setItems(data.approvals);
    } catch (e) { setError(e?.message || "无法读取家居审批"); }
  };
  useEffect(() => {
    let active = true;
    const poll = async () => {
      try {
        const data = await voiceApi.homeApprovals();
        if (active && Array.isArray(data.approvals) && typeof data.can_control === "boolean") setItems(data.approvals);
      } catch (e) { if (active) setError(e?.message || "审批列表不可用"); }
    };
    void poll();
    const timer = setInterval(poll, 5000);
    return () => { active = false; clearInterval(timer); };
  }, []);
  const cancel = async (item) => {
    setBusy(item.approval_id);
    setError("");
    setMessage("");
    try {
      const result = await voiceApi.homeApprovalCancel({
        approval_id: item.approval_id, digest: item.digest, confirmed: true,
      });
      if (!["rejected", "expired"].includes(result.status)) throw new Error("撤销状态未确认");
      setMessage("家居审批已撤销；不会自动发送设备命令。");
      await refresh();
    } catch (e) { setError(e?.message || "撤销失败，请重新核对状态"); }
    finally { setBusy(""); }
  };
  const approve = async (item) => {
    if (!item.can_approve || !window.confirm(`批准 ${item.entity_id} 的 ${item.service}？\n参数：${JSON.stringify(item.parameters)}\n这将实际控制设备；结果未知时不会自动重发。`)) return;
    setBusy(item.approval_id);setError("");setMessage("");
    try {
      const result=await voiceApi.homeApprovalApprove({approval_id:item.approval_id,digest:item.digest,confirmed:true});
      setMessage(result.outcome?.message || "请检查家居审批结果。");
      await refresh();
    } catch (e) {setError(e?.message || "执行结果未确认，请刷新记录；不要重复请求。");await refresh();}
    finally {setBusy("");}
  };
  return (
    <section className="voice-card" aria-label="HomeControl 独立审批收件箱">
      <h3>家居控制审批（NV15）</h3>
      <p role="status">语音申请的家居动作在这里逐个批准；批准后会实际控制设备，结果未知时不会自动重发。不确定时请拒绝。</p>
      <button type="button" disabled={!!busy} onClick={refresh}>刷新家居审批</button>
      {error && <p role="alert">{error}</p>}
      {message && <p role="status">{message}</p>}
      {items.length === 0 ? <p>当前没有家居审批记录。</p> : (
        <ul aria-label="家居审批列表">
          {items.map((item) => <li key={item.approval_id}>
            <strong>{item.entity_id}</strong> · {item.service} · {item.status}
            {item.stale && <span> · 白名单或凭据版本已变</span>}
            <p>{item.summary}</p>
            <p>动作参数：{JSON.stringify(item.parameters)} · 来源：语音设备 …{item.device_suffix}</p>
            <p>摘要：{item.digest.slice(0, 8)}… · 到期时间：{new Date(item.expires_at * 1000).toLocaleString("zh-CN")}</p>
            {item.outcome && <p>结果：{item.outcome.phase || "未知"} · {item.outcome.message}</p>}
            {item.can_revoke && <button type="button" disabled={!!busy}
              onClick={() => cancel(item)}>拒绝 / 撤销此请求</button>}
            {item.status === "pending" && <button type="button" disabled={!!busy || !item.can_approve}
              title={item.can_approve ? "批准后实际执行一次" : "家居控制未开启、配置已变化或请求已失效"}
              onClick={() => approve(item)}>批准执行一次</button>}
          </li>)}
        </ul>
      )}
    </section>
  );
}
