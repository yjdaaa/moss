import React, { useEffect, useState } from "react";
import { fetchPreviewBlobUrl } from "../../api/client.js";
import { isRemoteMode, resolveApiUrl } from "../../authSession.js";

// 创意画板预览图。原样搬自 pages/DrawPage.jsx（2026-11-16 按职责拆分）

// r49：`tone="dark"` 时按画板的深色底显示。矢量 / 轮廓刀路图本身是透明底青色发光线，原色显示；
// 只有光栅扫描的黑白位图（白底黑点）要 `invert` 反成黑底白点（反相会把发光线的白芯变黑、整体变暗）
export function PreviewImage({ title, url, stale = false, tone = "light", invert = false, empty = "暂无" }) {
  // 连接电脑时 <img> 直链不带配对 token，改为经鉴权取回 blob 再显示；
  // 本机模式直接使用唯一文件名 URL（不再在 render 期拼 Date.now() 导致敲字时反复重拉闪烁）
  const remote = isRemoteMode();
  const [blobUrl, setBlobUrl] = useState("");
  useEffect(() => {
    if (!remote || !url) {
      setBlobUrl("");
      return undefined;
    }
    let cancelled = false;
    let created = "";
    fetchPreviewBlobUrl(url).then((value) => {
      created = value;
      if (cancelled) {
        if (value) URL.revokeObjectURL(value);
      } else {
        setBlobUrl(value);
      }
    });
    return () => {
      cancelled = true;
      if (created) URL.revokeObjectURL(created);
    };
  }, [remote, url]);
  const src = remote ? blobUrl : (url ? resolveApiUrl(url) : "");
  return (
    <div
      className={`preview-box${stale ? " is-stale" : ""}${tone === "dark" ? " preview-box--dark" : ""}${
        tone === "dark" && invert ? " is-invert" : ""
      }`}
    >
      {title ? <div className="preview-title">{title}{stale && src ? "（旧图 · 需重新生成）" : ""}</div> : null}
      {src ? <img src={src} alt={title || "刀路预览"} /> : <div className="preview-empty">{empty}</div>}
    </div>
  );
}
