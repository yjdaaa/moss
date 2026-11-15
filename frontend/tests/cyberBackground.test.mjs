import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const TEST_DIR = path.dirname(fileURLToPath(import.meta.url));
const SRC_DIR = path.resolve(TEST_DIR, "../src");

test("CyberBackground restores flowing plasma + 2D stars fixed in space without mouse offset", () => {
  const bgSrc = fs.readFileSync(path.join(SRC_DIR, "components/CyberBackground.jsx"), "utf8");
  assert.match(bgSrc, /plasma-bg__fluid/, "必须保留 WebGL 等离子流体画布 .plasma-bg__fluid");
  assert.match(bgSrc, /plasma-bg__stars/, "必须保留 2D 星点画布 .plasma-bg__stars");
  assert.match(bgSrc, /plasma-bg__vignette/, "必须保留静态暗角 .plasma-bg__vignette");
  assert.match(bgSrc, /cyber-noise-overlay/, "必须保留细噪点层 .cyber-noise-overlay");
  assert.match(bgSrc, /float fbm\(vec2 p\)/, "必须使用原版域扭曲 fbm 等离子着色器");
  assert.doesNotMatch(bgSrc, /gl\.clear\s*\(/, "不得调用 gl.clear 以免干扰 twin-render 重绘检测");
  assert.doesNotMatch(
    bgSrc,
    /pointermove|mousemove|mouseleave|u_mouse|u_spot|uniform vec2 m\b|--bg-mx|--bg-spot|cyber-bg__grid/,
    "防眩晕规范：还原版流动等离子与星点必须固定空间锚点，不得响应鼠标偏移或手电筒",
  );
  for (const [, a, b] of bgSrc.matchAll(/smoothstep\(\s*(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)\s*,/g)) {
    assert.ok(Number(a) < Number(b), `GLSL ES 1.00 要求 smoothstep 下界(${a})严格小于上界(${b})`);
  }

  const mainSrc = fs.readFileSync(path.join(SRC_DIR, "main.jsx"), "utf8");
  assert.match(mainSrc, /<CyberBackground\s*\/>/, "main.jsx 应直接挂载无状态传参的 <CyberBackground />");

  const baseCss = fs.readFileSync(path.join(SRC_DIR, "styles/01-base.css"), "utf8");
  assert.match(baseCss, /\.cyber-background-container\b/, "CSS 必须定义 .cyber-background-container");
  assert.match(baseCss, /\.plasma-bg__fluid\b/, "CSS 必须定义 .plasma-bg__fluid");
  assert.match(baseCss, /\.plasma-bg__stars\b/, "CSS 必须定义 .plasma-bg__stars");
  assert.match(baseCss, /\.plasma-bg__vignette\b/, "CSS 必须定义 .plasma-bg__vignette");
  assert.match(baseCss, /\.cyber-noise-overlay\b/, "CSS 必须定义 .cyber-noise-overlay");
  assert.doesNotMatch(
    baseCss,
    /\.cyber-bg__grid\b|\.cyber-bg__grid-torch\b/,
    "还原等离子方案后应清理临时微点阵样式规则",
  );
});
