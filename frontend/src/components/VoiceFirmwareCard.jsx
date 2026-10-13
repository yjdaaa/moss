import React, { useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";
export default function VoiceFirmwareCard(){
  const [data,setData]=useState(null),[error,setError]=useState("");
  const refresh=async()=>{try{setData(await voiceApi.firmwareStatus());setError("");}catch(e){setError(e.message);}};
  useEffect(()=>{void refresh();},[]);
  const labels={pending:"待下载",downloading:"下载中",downloaded:"文件已下载（未证明刷写）",unknown:"下载结果未知，需要核对"};
  return <section className="voice-card voice-speaker-card">
    <h2>固件交付记录</h2><p>固件下载和升级保持禁用。当前 OTA 只返回语音连接信息；设备自报型号、版本和编号不会获得固件下载权限。</p>
    <p>{data?.reason} 启动时未完成的下载会转为结果未知，不自动重试。下载完成、设备上报和实际刷写分别记录。</p>
    {error&&<p role="alert">{error}</p>}<button className="btn btn-ghost" type="button" onClick={refresh}>刷新交付记录</button>
    {!data?.records?.length&&<p>暂无固件交付记录。</p>}
    <ul>{data?.records?.map(row=><li key={row.delivery_id}>{row.device_id} · {labels[row.download_status]||"未知状态"} · {row.device_report_status==='reported'?"收到设备上报，仍需可信验证":"未收到设备上报"} · SHA-256 <code>{row.artifact_sha256}</code></li>)}</ul>
  </section>;
}
