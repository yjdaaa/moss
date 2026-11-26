// 工作台矢量文字：多行文本与外框 / 铭牌（2026-11-26）
import assert from "node:assert/strict";
import test from "node:test";
import {
  TEXT_BORDER_OPTIONS,
  WORKBENCH_TEXT_MAX_CHARS,
  normalizeWorkbenchText,
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
  assert.deepEqual(recentTaskRecall(image), { material: "亚克力", thickness: "2" }, "图片任务不回填文字，没有实际参数时不改自定义参数");
  assert.equal(recentTaskRecall({ workflow_id: "wf-3", source_type: "text", input: {} }), null);
  const noBorder = recentTaskRecall({ ...text, input: { ...text.input, border: undefined } });
  assert.deepEqual(noBorder.textBorder, { shape: "none", double: false, boardWidthMm: "", boardHeightMm: "" });
});
