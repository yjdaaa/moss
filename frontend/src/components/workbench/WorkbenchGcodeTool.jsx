import React from "react";
import { postJson } from "../../api/client.js";
import { showAlert } from "../../appDialog.js";

/** 雕刻工作台 ② 加工内容 · 已有 G 码：导入现有 .gcode / .nc 刀路文件。原样搬自 pages/HomePage.jsx（2026-11-16 按职责拆分） */
export function WorkbenchGcodeTool({
  addLog,
  uploadedGcodePath,
  setUploadedGcodePath,
  uploadedGcodeName,
  setUploadedGcodeName,
}) {
  return (
    <div className="tool-body wb-spacer">
      <div className="wb-spacer-2">
        <label className="wb-label-2">
          导入现有 G-code / NC 刀路文件 (.gcode / .nc)
        </label>
        <input
          type="file"
          accept=".gcode,.nc"
          onChange={async (e) => {
            const file = e.target.files?.[0];
            e.target.value = "";
            if (!file) return;
            if (file.size > 14 * 1024 * 1024) {
              showAlert("G-code 刀路文件过大（在线上传上限 14 MB，请求体限 20 MB）。", {
                title: "刀路超出大小限制",
                tone: "warning",
              });
              return;
            }
            addLog("FILE", `正在读取刀路文件: ${file.name}...`, "info");
            const reader = new FileReader();
            reader.onload = async () => {
              try {
                const res = await postJson("/api/upload", {
                  filename: file.name,
                  content_base64: reader.result,
                });
                if (res.ok && res.data?.success) {
                  setUploadedGcodePath(res.data.result?.file_path || file.name);
                  setUploadedGcodeName(file.name);
                  addLog("FILE", `已载入 G-code 刀路: ${file.name}`, "ok");
                } else {
                  const err = res.data?.result || "载入失败";
                  addLog("FILE", `G-code 载入失败: ${err}`, "error");
                  showAlert(err, { title: "G-code 载入失败", tone: "error" });
                }
              } catch (err) {
                addLog("FILE", `网络或系统异常: ${err.message}`, "error");
                showAlert(err.message, { title: "载入异常", tone: "error" });
              }
            };
            reader.onerror = () => {
              addLog("FILE", "本地刀路读取失败", "error");
            };
            reader.readAsDataURL(file);
          }}
          className="file-input wb-field-6"
        />
        {uploadedGcodePath && (
          <div className="wb-box-2" id="wb-gcode-selected">
            ✓ 载入刀路: {uploadedGcodeName || uploadedGcodePath}
          </div>
        )}
      </div>
    </div>
  );
}
