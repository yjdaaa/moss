import React from "react";
import { postJson } from "../../api/client.js";
import { showAlert } from "../../appDialog.js";
import { IMAGE_PRESET_HINTS, detectImagePreset } from "../../qualityPresets.js";
import { IMAGE_INTERVAL_SCALE_LIMITS, IMAGE_MODE_OPTIONS, imageAdvancedSummary } from "../../workbenchParams.js";
import QualityPresetBar from "../QualityPresetBar.jsx";
import FillOptionsPanel from "../FillOptionsPanel.jsx";

/** 雕刻工作台 ② 加工内容 · 图片处理：上传原图、质量预设、成品尺寸与图片高级设置。原样搬自 pages/HomePage.jsx（2026-11-16 按职责拆分） */
export function WorkbenchImageTool({
  addLog,
  uploadedImagePath,
  setUploadedImagePath,
  uploadedImageName,
  setUploadedImageName,
  uploadedImageThumb,
  setUploadedImageThumb,
  imageMode,
  setImageMode,
  imageExtras,
  setImageExtra,
  imageFields,
  applyImageQualityPreset,
  imageWidthMm,
  setImageWidthMm,
  imageHeightMm,
  setImageHeightMm,
  fillSettings,
  updateFillSettings,
}) {
  return (
    <div className="tool-body wb-spacer">
      <div className="wb-spacer-2">
        <label className="wb-label-2">
          上传图像文件 (PNG / JPG / BMP / GIF / WebP)
        </label>
        <input
          type="file"
          accept=".png,.jpg,.jpeg,.bmp,.gif,.webp"
          onChange={async (e) => {
            const file = e.target.files?.[0];
            e.target.value = "";
            if (!file) return;
            if (file.size > 14 * 1024 * 1024) {
              showAlert("图片文件过大（上限 14 MB，Base64 传输请求体限 20 MB），请先压缩分辨率后再上传。", {
                title: "图片超出大小限制",
                tone: "warning",
              });
              return;
            }
            addLog("FILE", `正在读取素材: ${file.name}...`, "info");
            const reader = new FileReader();
            reader.onload = async () => {
              const dataUrl = typeof reader.result === "string" ? reader.result : "";
              try {
                const res = await postJson("/api/upload", {
                  filename: file.name,
                  content_base64: dataUrl,
                });
                if (res.ok && res.data?.success) {
                  setUploadedImagePath(res.data.result?.file_path || file.name);
                  setUploadedImageName(file.name);
                  setUploadedImageThumb(dataUrl);
                  addLog("FILE", `已保存至工作区: ${file.name}`, "ok");
                } else {
                  const err = res.data?.result || "上传失败";
                  addLog("FILE", `图片上传失败: ${err}`, "error");
                  showAlert(err, { title: "图片上传失败", tone: "error" });
                }
              } catch (err) {
                addLog("FILE", `网络或系统异常: ${err.message}`, "error");
                showAlert(err.message, { title: "上传异常", tone: "error" });
              }
            };
            reader.onerror = () => {
              addLog("FILE", "本地文件读取失败", "error");
            };
            reader.readAsDataURL(file);
          }}
          className="file-input wb-field-6"
        />
        {uploadedImagePath && (
          <div className="wb-box-2 wb-image-thumb-row" id="wb-image-selected">
            {uploadedImageThumb && (
              <img src={uploadedImageThumb} alt="已选原图缩略图" className="wb-image-thumb" />
            )}
            <span className="wb-image-name">✓ 已选原图: {uploadedImageName || uploadedImagePath}</span>
          </div>
        )}
      </div>
      <QualityPresetBar
        id="wb-image-preset"
        value={detectImagePreset(imageMode, imageExtras.intervalScale)}
        hints={IMAGE_PRESET_HINTS}
        onSelect={applyImageQualityPreset}
      />
      <div className="wb-params-grid">
        <label>
          <span>成品宽 mm</span>
          <input
            id="wb-image-width"
            type="number"
            inputMode="decimal"
            min={1}
            max={1000}
            placeholder="自动"
            value={imageWidthMm}
            onChange={(e) => setImageWidthMm(e.target.value)}
            className="text-input"
          />
        </label>
        <label>
          <span>成品高 mm</span>
          <input
            id="wb-image-height"
            type="number"
            inputMode="decimal"
            min={1}
            max={1000}
            placeholder="自动"
            value={imageHeightMm}
            onChange={(e) => setImageHeightMm(e.target.value)}
            className="text-input"
          />
        </label>
      </div>
      <details className="fill-options" id="wb-image-advanced">
        <summary>
          <span className="fill-options-title">高级设置</span>
          <span className="fill-options-summary">{imageAdvancedSummary(imageMode, imageExtras)}</span>
        </summary>
        <div className="wb-params-grid">
          <label>
            <span>转换方式</span>
            <select
              id="wb-image-mode"
              value={imageMode}
              onChange={(e) => setImageMode(e.target.value)}
              className="select-input wb-compact-select"
            >
              {IMAGE_MODE_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>{opt.label}</option>
              ))}
            </select>
          </label>
          <label title="行间距 = 材料推荐线距 × 倍数；大于 1 更快更粗，小于 1 更细更深（直通模式不用）">
            <span>行间距倍数</span>
            <input
              id="wb-image-interval-scale"
              type="number"
              inputMode="decimal"
              min={IMAGE_INTERVAL_SCALE_LIMITS.min}
              max={IMAGE_INTERVAL_SCALE_LIMITS.max}
              step={0.1}
              placeholder="1"
              value={imageExtras.intervalScale}
              onChange={(e) => setImageExtra("intervalScale", e.target.value)}
              className="text-input"
            />
          </label>
          {imageFields.halftone && (
            <>
              <label>
                <span>网点网格 mm</span>
                <input
                  id="wb-image-halftone-cell"
                  type="number"
                  inputMode="decimal"
                  min={0.1}
                  max={10}
                  step={0.05}
                  placeholder="自动"
                  value={imageExtras.halftoneCellMm}
                  onChange={(e) => setImageExtra("halftoneCellMm", e.target.value)}
                  className="text-input"
                />
              </label>
              <label>
                <span>网点角度 °</span>
                <input
                  id="wb-image-halftone-angle"
                  type="number"
                  inputMode="decimal"
                  min={-90}
                  max={90}
                  placeholder="45"
                  value={imageExtras.halftoneAngleDeg}
                  onChange={(e) => setImageExtra("halftoneAngleDeg", e.target.value)}
                  className="text-input"
                />
              </label>
            </>
          )}
          {imageFields.grayscale && (
            <label title="最黑处用本次功率（推荐或自定义），最浅处用这里的最小功率；纯白不出光">
              <span>灰度最小功率 %</span>
              <input
                id="wb-image-gray-min"
                type="number"
                inputMode="decimal"
                min={0}
                max={99}
                placeholder="0"
                value={imageExtras.grayMinPowerPercent}
                onChange={(e) => setImageExtra("grayMinPowerPercent", e.target.value)}
                className="text-input"
              />
            </label>
          )}
          <label title="每段出光两端各缩短一半，抵消光斑直径；须小于行间距">
            <span>点宽补偿 mm</span>
            <input
              id="wb-image-dot-width"
              type="number"
              inputMode="decimal"
              min={0}
              max={0.5}
              step={0.01}
              placeholder="不补偿"
              value={imageExtras.dotWidthMm}
              onChange={(e) => setImageExtra("dotWidthMm", e.target.value)}
              className="text-input"
            />
          </label>
        </div>
        <div className="wb-image-switches">
          <label className="wb-params-switch" title="明暗对调，适合石板、玻璃等刻痕发白的材料">
            <input
              id="wb-image-negative"
              type="checkbox"
              checked={imageExtras.negative}
              onChange={(e) => setImageExtra("negative", e.target.checked)}
            />
            <span>反相（石板 / 玻璃）</span>
          </label>
          <label className="wb-params-switch" title="不重采样，按原图像素逐点雕刻；行间距 = 成品宽度 ÷ 原图像素宽">
            <input
              id="wb-image-pass-through"
              type="checkbox"
              checked={imageExtras.passThrough}
              onChange={(e) => setImageExtra("passThrough", e.target.checked)}
            />
            <span>直通（不重采样）</span>
          </label>
        </div>
        {imageFields.grayscale && (
          <div className="wb-params-hint">灰度变功率适合二极管激光；最黑处用本次功率，可在下方“这次自定义功率和速度”里调最大功率。</div>
        )}
      </details>
      {/* 逐行扫描用得上的扫描设置（r46）：双向、扫描偏移与过扫描，与文字 / 画板共用一份本机设置 */}
      {fillSettings && updateFillSettings ? (
        <FillOptionsPanel idPrefix="wb-img" settings={fillSettings} onChange={updateFillSettings} scanOnly />
      ) : null}
    </div>
  );
}
