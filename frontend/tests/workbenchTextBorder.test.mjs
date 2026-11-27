// 工作台矢量文字：多行文本与外框 / 铭牌（2026-11-26）
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  TEXT_BORDER_OPTIONS,
  WORKBENCH_TEXT_MAX_CHARS,
  normalizeWorkbenchText,
  textBorderCutAllowed,
  textBorderPayload,
  workbenchTextPayload,
  workbenchTextStats,
} from "../src/workbenchParams.js";

test("工作台文字最多 3 行、72 字，统一换行符", () => {
  assert.equal(normalizeWorkbenchText("张三\r\n2026.11.26"), "张三\n2026.11.26");
  assert.equal(normalizeWorkbenchText("a\nb\nc\nd\ne"), "a\nb\nc", "第 4 行起丢掉");
  assert.equal(Array.from(normalizeWorkbenchText("字".repeat(100))).length, WORKBENCH_TEXT_MAX_CHARS);
  assert.deepEqual(workbenchTextStats("张三\n2026"), { chars: 6, lines: 2 });
  assert.deepEqual(workbenchTextStats(""), { chars: 0, lines: 0 });
  assert.equal(workbenchTextPayload("\n  张 三  \n2026 \n\n"), "  张 三\n2026", "去首尾空行与行尾空白，行内空格保留");
});

test("外框 / 铭牌：无外框不带字段，有外框要木牌尺寸并校验机器行程", () => {
  assert.deepEqual(textBorderPayload({ shape: "none" }), {});
  assert.deepEqual(TEXT_BORDER_OPTIONS.map((o) => o.value), ["none", "rect", "rounded", "circle", "ellipse"]);
  assert.deepEqual(
    textBorderPayload({ shape: "rounded", boardWidthMm: "80", boardHeightMm: "40" }),
    { border: { shape: "rounded", style: "single" }, width_mm: 80, height_mm: 40 },
  );
  assert.deepEqual(
    textBorderPayload({ shape: "circle", double: true, boardWidthMm: 50, boardHeightMm: "" }),
    { border: { shape: "circle", style: "double" }, width_mm: 50, height_mm: 50 },
    "圆牌只要直径",
  );
  assert.deepEqual(
    textBorderPayload({ shape: "rounded", double: true, cutBorder: true, boardWidthMm: "80", boardHeightMm: "40" }),
    { border: { shape: "rounded", style: "double" }, border_cut: true, width_mm: 80, height_mm: 40 },
    "勾选切下外框时携带 border_cut: true",
  );
  assert.equal(textBorderCutAllowed({ textIsCut: false, surfaceOnly: false }), true);
  assert.equal(textBorderCutAllowed({ textIsCut: true, surfaceOnly: false }), false);
  assert.equal(textBorderCutAllowed({ textIsCut: false, surfaceOnly: true }), false);
  assert.deepEqual(
    textBorderPayload({
      shape: "rounded",
      double: true,
      cutBorder: true,
      allowCutBorder: textBorderCutAllowed({ textIsCut: true, surfaceOnly: false }),
      boardWidthMm: "80",
      boardHeightMm: "40",
    }),
    { border: { shape: "rounded", style: "double" }, width_mm: 80, height_mm: 40 },
    "N3：刻法为外框切割（勾选框隐藏）时不得提交 border_cut",
  );
  assert.deepEqual(
    textBorderPayload({
      shape: "rounded",
      double: false,
      cutBorder: true,
      allowCutBorder: textBorderCutAllowed({ textIsCut: false, surfaceOnly: true }),
      boardWidthMm: "80",
      boardHeightMm: "40",
    }),
    { border: { shape: "rounded", style: "single" }, width_mm: 80, height_mm: 40 },
    "N3：表面通用材质（勾选框隐藏）时不得提交 border_cut",
  );
  const homeSrc = readFileSync(new URL("../src/pages/HomePage.jsx", import.meta.url), "utf8");
  assert.match(
    homeSrc,
    /textBorderCutAllowed\(\{\s*textIsCut,\s*surfaceOnly:\s*surfaceOnlyMaterial\s*\}\)/,
    "N3：HomePage 提交 border_cut 必须与勾选框显示条件一致",
  );
  assert.match(homeSrc, /allowCutBorder:\s*canCutBorder/, "N3：HomePage 调 textBorderPayload 必须传入 allowCutBorder");
  assert.match(textBorderPayload({ shape: "rect", boardWidthMm: "80", boardHeightMm: "" }).error, /宽和高/);
  assert.match(textBorderPayload({ shape: "circle", boardWidthMm: "" }).error, /直径/);
  assert.match(textBorderPayload({ shape: "rect", boardWidthMm: "-1", boardHeightMm: "10" }).error, /大于 0/);
  assert.match(textBorderPayload({ shape: "rect", boardWidthMm: "300", boardHeightMm: "40", maxWidthMm: 100, maxHeightMm: 100 }).error, /超出机器行程/);
  assert.match(textBorderPayload({ shape: "star", boardWidthMm: 1, boardHeightMm: 1 }).error, /不认识/);
});

test("最近任务：文字任务回填文字、刻法、外框与实际参数，图片任务只回填材料与参数", async () => {
  const { recentTaskLabel, recentTaskRecall, recentTextStyle } = await import("../src/workbenchParams.js");
  const text = {
    workflow_id: "wf-1",
    source_type: "text",
    updated_at: Date.UTC(2026, 10, 25, 6, 5) / 1000,
    input: {
      text: "张三\n2026.11.25",
      font_size_mm: 8,
      laser_mode: "engrave",
      text_fill: "fill",
      origin: "lower_left",
      material: "椴木",
      thickness_mm: 3,
      border: { shape: "rounded", style: "double" },
      width_mm: 80,
      height_mm: 40,
    },
    artifacts: { actual_params: { laser_max_power: 420, power_scale: 1000, feed_rate: 1800, passes: 2 } },
  };
  assert.deepEqual(recentTaskRecall(text), {
    material: "椴木",
    thickness: "3",
    tool: "text",
    text: "张三\n2026.11.25",
    fontSizeMm: 8,
    textStyle: "fill",
    textOrigin: "lower_left",
    textBorder: { shape: "rounded", double: true, boardWidthMm: "80", boardHeightMm: "40" },
    custom: { power: "42", feed: "1800", passes: "2" },
  });
  assert.match(recentTaskLabel(text, Date.UTC(2026, 10, 26)), /^11\/25 \d\d:05 · 文字 · “张三 \/ 2026\.11\.25” · 椴木 3mm$/);
  assert.equal(recentTextStyle({ laser_mode: "cut" }), "cut");
  assert.equal(recentTextStyle({ text_fill: "hatch" }), "hatch");
  assert.equal(recentTextStyle({}), "outline");

  const image = { workflow_id: "wf-2", source_type: "image", input: { material: "亚克力", thickness_mm: 2 } };
  assert.deepEqual(recentTaskRecall(image), { material: "亚克力", thickness: "2" }, "无图片文件路径时仅回填材料与厚度");
  const fullImage = {
    workflow_id: "wf-2b",
    source_type: "image",
    updated_at: Date.UTC(2026, 10, 25, 6, 5) / 1000,
    input: {
      material: "椴木",
      thickness_mm: 3,
      image_file: "uploads/logo.png",
      width_mm: 60,
      height_mm: 40,
      image_mode: "jarvis",
      negative: true,
      line_interval_scale: 1.5,
    },
    artifacts: { actual_params: { laser_max_power: 500, power_scale: 1000, feed_rate: 2000, passes: 1 } },
  };
  assert.deepEqual(recentTaskRecall(fullImage), {
    material: "椴木",
    thickness: "3",
    tool: "image",
    imagePath: "uploads/logo.png",
    imageName: "logo.png",
    imageWidthMm: "60",
    imageHeightMm: "40",
    imageMode: "jarvis",
    imageEngravingMode: "raster",
    imageExtras: {
      negative: true,
      passThrough: false,
      dotWidthMm: "",
      halftoneCellMm: "",
      halftoneAngleDeg: "",
      grayMinPowerPercent: "",
      intervalScale: "1.5",
    },
    custom: { power: "50", feed: "2000", passes: "1" },
  });
  assert.match(recentTaskLabel(fullImage, Date.UTC(2026, 10, 26)), /^11\/25 \d\d:05 · 图片 · “logo\.png” · 椴木 3mm$/);
  const outlineImage = recentTaskRecall({
    ...fullImage,
    input: { ...fullImage.input, engraving_mode: "outline" },
  });
  assert.equal(outlineImage.imageEngravingMode, "outline");
  const cutBorderRecall = recentTaskRecall({
    ...text,
    input: { ...text.input, border_cut: true },
  });
  assert.equal(cutBorderRecall.textBorder.cutBorder, true);
  const { actualParamsText, materialRecommendationModes } = await import("../src/workbenchParams.js");
  assert.deepEqual(
    materialRecommendationModes({ activeTool: "image", imageEngravingMode: "outline" }),
    { laserMode: "engrave", engravingMode: "outline" },
  );
  assert.match(
    actualParamsText({
      artifacts: {
        text_fill: "outline",
        actual_params: { laser_max_power: 420, feed_rate: 1500, passes: 1, power_scale: 1000 },
        border_cut_params: { laser_max_power: 850, feed_rate: 260, passes: 3, power_scale: 1000 },
      },
    }),
    /外框切割 85% \/ 260 mm\/min × 3 遍/,
  );
  const gcode = {
    workflow_id: "wf-2c",
    source_type: "gcode",
    input: { material: "椴木", thickness_mm: 3, gcode_file: "uploads/part.gcode" },
  };
  assert.deepEqual(recentTaskRecall(gcode), {
    material: "椴木",
    thickness: "3",
    tool: "gcode",
    gcodePath: "uploads/part.gcode",
    gcodeName: "part.gcode",
  });
  assert.equal(recentTaskRecall({ workflow_id: "wf-3", source_type: "text", input: {} }), null);
  const noBorder = recentTaskRecall({ ...text, input: { ...text.input, border: undefined } });
  assert.deepEqual(noBorder.textBorder, { shape: "none", double: false, boardWidthMm: "", boardHeightMm: "" });
});
