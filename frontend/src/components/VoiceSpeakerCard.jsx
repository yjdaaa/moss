import React, { useEffect, useState, useRef } from "react";
import { voiceApi } from "../voiceApi.js";
import { showConfirm } from "../appDialog.js";
import { speakerCalibrationView } from "../voiceState.js";

export default function VoiceSpeakerCard() {
  const [data,setData]=useState(null),[error,setError]=useState(""),[pollError,setPollError]=useState(""),[busy,setBusy]=useState(false);
  const [name,setName]=useState(""),[session,setSession]=useState("");
  const [drafts,setDrafts]=useState({});
  const epoch=useRef(0),mutating=useRef(false);
  useEffect(()=>{
    let live=true,pending=false;
    const poll=async()=>{if(pending||mutating.current)return;pending=true;const started=epoch.current;try{const next=await voiceApi.speakerManage({action:"status"});if(live&&started===epoch.current){setData(next);setPollError("");}}catch(e){if(live&&started===epoch.current)setPollError(e.message);}finally{pending=false;}};
    void poll();const timer=setInterval(poll,2000);return()=>{live=false;clearInterval(timer);};
  },[]);
  const act=async(input)=>{if(mutating.current)return false;mutating.current=true;epoch.current++;setBusy(true);try{setData(await voiceApi.speakerManage(input));setError("");return true;}catch(e){setError(e.message);return false;}finally{epoch.current++;mutating.current=false;setBusy(false);}};
  const calibration=speakerCalibrationView(data);
  const selectedSession=()=>data?.sessions?.find(row=>JSON.stringify([row.device_id,row.session_id])===session);
  const beginTest=async(row)=>{
    const target=selectedSession();if(!target)return;
    if(!await showConfirm(`接下来90秒内，请“${row.name}”对这台设备再说三段2～10秒的测试句（换不同的话）。只生成测试向量用于计算阈值，不保存原始音频，算完即丢弃。`,{title:"录测试音",confirmText:"开始录"}))return;
    await act({action:"begin_test",id:row.id,device_id:target.device_id,session_id:target.session_id,confirmed:true});
  };
  const calibrate=async()=>{
    if(!await showConfirm("用刚录的测试音计算识别阈值并自检：有人被认成别人、或本人认对不到三分之二都不会通过。通过后才可以开启按人记忆。声纹始终不能代替任何设备审批。",{title:"计算阈值",confirmText:"计算"}))return;
    await act({action:"calibrate",confirmed:true});
  };
  const begin=async()=>{
    const target=data?.sessions?.find(row=>JSON.stringify([row.device_id,row.session_id])===session);if(!target)return;
    if(!await showConfirm("接下来90秒内，这台设备说出的三段2～10秒语音将生成声纹向量并存入本机数据库。原始音频不保存。请仅让登记人说话；这不会开启身份识别或获得设备控制权限。",{title:"同意采集声纹？",confirmText:"开始登记"}))return;
    await act({action:"begin",name:name.trim(),device_id:target.device_id,session_id:target.session_id,confirmed:true});
  };
  return <section className="voice-card voice-speaker-card">
    <h2>声纹登记</h2>
    <label className="voice-check"><input type="checkbox" checked={!!data?.personal_memory_enabled} disabled={busy||!data?.recognition_enabled} onChange={async e=>{
      const enabled=e.target.checked;
      if(enabled&&!await showConfirm("开启后只按当前句识别到的人读取或总结个人记忆；未知身份不回退到共享设备摘要。总结会使用当前大模型，云模型会收到筛选后的对话文字。声纹不能代替任何设备审批。",{title:"开启按人记忆？",confirmText:"开启"}))return;
      await act({action:"personal_mode",enabled,confirmed:true});
    }}/><span>按人使用对话记忆（完成下方简易标定后可开启）</span></label>
    <p>声纹仅是个人记忆的候选身份提示，不能授权加工或家居动作。个人摘要可在本机维护；完成简易标定并明确开启前不会用于对话，当前沿用既有设备记忆开关。</p>
    <p>请先在语音中心下载 CampPlus 模型，准备协议4辅助程序，并启动对话模式。每人说三段不同句子，避开多人同时说话、噪声和削波。删除声纹会同时删除其个人摘要。</p>
    {error&&<p role="alert">{error}</p>}
    {pollError&&<p role="alert">状态刷新失败：{pollError}</p>}
    <div className="voice-form-grid">
    <label>姓名 <input className="text-input" value={name} maxLength={64} onChange={e=>setName(e.target.value)} disabled={busy||data?.enrollment?.active}/></label>
    <label>采集设备 <select className="text-input" value={session} onChange={e=>setSession(e.target.value)} disabled={busy||data?.enrollment?.active}>
      <option value="">选择当前会话</option>{data?.sessions?.map(row=><option key={JSON.stringify([row.device_id,row.session_id])} value={JSON.stringify([row.device_id,row.session_id])}>{row.device_id}</option>)}
    </select></label>
    </div>
    <button type="button" className="btn btn-ghost" disabled={busy||!session||!name.trim()||data?.enrollment?.active} onClick={begin}>开始三段登记</button>
    <p role="status">{data?.enrollment?.message||"正在加载"} · {data?.enrollment?.collected||0}/3{data?.enrollment?.processing?" · 计算中":""}</p>
    {data?.enrollment?.active&&<button type="button" className="btn btn-ghost" disabled={busy} onClick={()=>act({action:"cancel"})}>取消并丢弃片段</button>}
    <div className="voice-form-group" id="voice-speaker-calibration">
      <h3>简易标定（开启身份识别前必须完成）</h3>
      <p role="status">{calibration.status}</p>
      <p>步骤：至少登记两个人 → 选上面的采集设备，给每个人点“录测试音”各说三段新句子 → 点“计算阈值并自检”。只拿已登记的人互相比，测不到陌生人，所以阈值设了下限；增删或重新登记任何人后都要重新标定。</p>
      <button type="button" className="btn btn-ghost" disabled={busy||!calibration.ready||data?.enrollment?.active} onClick={calibrate} title={calibration.blocked||"计算阈值并自检"}>计算阈值并自检</button>
      {calibration.blocked&&<span className="voice-muted"> {calibration.blocked}</span>}
      {calibration.state!=="none"&&<button type="button" className="btn btn-ghost" disabled={busy} onClick={async()=>{
        if(await showConfirm("清除标定后身份识别与按人记忆立即停用，需要重新标定。",{title:"清除标定",danger:true,confirmText:"清除"}))await act({action:"reset_calibration",confirmed:true});
      }}>清除标定</button>}
    </div>
    {data?.speakers?.map(row=><div className="voice-form-group" key={row.id}>
      <h3>{row.name} · {row.dimension}维</h3>
      <p className="voice-muted">标定测试音 {calibration.counts[row.id]||0}/3</p>
      <button type="button" className="btn btn-ghost" disabled={busy||!session||data?.enrollment?.active} onClick={()=>beginTest(row)}>录测试音</button>
      <label className="voice-person-summary">个人摘要 <textarea className="text-input" rows={3} maxLength={1500} value={drafts[row.id]?.text??row.memory_summary??""} disabled={busy} onChange={e=>setDrafts(old=>({...old,[row.id]:{text:e.target.value,revision:old[row.id]?.revision??row.memory_revision}}))}/></label>
      {drafts[row.id]&&drafts[row.id].revision!==row.memory_revision&&<p role="status">个人摘要已被其他操作更新。你的草稿仍保留；请核对后重新编辑。</p>}
      {drafts[row.id]&&<button className="btn btn-ghost" type="button" disabled={busy} onClick={()=>setDrafts(old=>{const next={...old};delete next[row.id];return next;})}>丢弃草稿并加载最新摘要</button>}
      <button className="btn btn-ghost" type="button" disabled={busy||!drafts[row.id]} onClick={async()=>{
        const draft=drafts[row.id];if(await act({action:"save_memory",id:row.id,revision:draft.revision,summary:draft.text}))setDrafts(old=>{const next={...old};delete next[row.id];return next;});
      }}>保存个人摘要</button>
      <button type="button" className="btn btn-ghost" disabled={busy} onClick={async()=>{
        if(await showConfirm(`删除“${row.name}”的声纹及个人记忆？`,{title:"删除声纹",danger:true,confirmText:"删除"})){
          if(await act({action:"delete",id:row.id,revision:row.revision,confirmed:true}))setDrafts(old=>{const next={...old};delete next[row.id];return next;});
        }
      }}>删除声纹和记忆</button>
    </div>)}
  </section>;
}
