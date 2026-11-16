import React, { useEffect, useState } from "react";
import { fetchPreviewBlobUrl } from "../../api/client.js";
import { isRemoteMode, resolveApiUrl } from "../../authSession.js";

// 创意画板预览图。原样搬自 pages/DrawPage.jsx（2026-11-16 按职责拆分）

export function PreviewImage({ title, url, stale = false }) {
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
    <div className={`preview-box${stale ? " is-stale" : ""}`}>
      <div className="preview-title">{title}{stale && src ? "（旧图 · 需重新生成）" : ""}</div>
      {src ? <img src={src} alt={title} /> : <div className="preview-empty">暂无</div>}
    </div>
  );
}
