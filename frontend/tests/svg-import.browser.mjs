// 创作画板导入 SVG：在真实浏览器 SVG 引擎里测量与清洗（2026-11-26）
// 验证：单位与 viewBox 换算、变换与 CSS 类样式、奇偶规则孔洞、开放线、白名单清洗（脚本不执行、忽略项计数）、错误提示。
// Run: PLAYWRIGHT_CHANNEL=chrome node tests/svg-import.browser.mjs (from frontend/)
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { createServer } from "vite";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");
const root = path.dirname(fileURLToPath(new URL("../package.json", import.meta.url)));

const vite = await createServer({ root, server: { host: "127.0.0.1", port: 0, strictPort: false }, logLevel: "error" });
let browser;
try {
  await vite.listen();
  const local = vite.resolvedUrls?.local?.[0];
  if (!local) throw new Error("Vite loopback server URL unavailable");
  browser = await chromium.launch({
    headless: true,
    channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined),
  });
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (err) => errors.push(err.message));
  await page.goto(new URL("src/svgImport.js", local).href);

  const run = (svg) =>
    page.evaluate(async (text) => {
      const mod = await import("/src/svgImport.js");
      try {
        const out = mod.extractSvgVectors(text);
        const area = (ring) => mod.signedArea(ring.slice(0, -1));
        return {
          ok: true,
          size: out.data.size,
          paths: out.data.paths.map((p) => ({
            rings: p.rings.length,
            open: p.open.length,
            fill: p.fill,
            stroke: p.stroke,
            areas: p.rings.map(area),
            openFirst: p.open[0]?.[0],
            openLast: p.open[0]?.[p.open[0].length - 1],
          })),
          ignored: out.ignored,
          svg: out.svg,
          hostsLeft: document.querySelectorAll("div[aria-hidden='true']").length,
        };
      } catch (error) {
        return { ok: false, message: error.message };
      }
    }, svg);

  // 1. viewBox + 毫米宽高，奇偶规则孔洞
  const holed = await run(
    '<svg xmlns="http://www.w3.org/2000/svg" width="20mm" height="20mm" viewBox="0 0 200 200"><path fill-rule="evenodd" d="M0 0H200V200H0Z M50 50H150V150H50Z"/></svg>',
  );
  assert.ok(holed.ok, holed.message);
  assert.ok(Math.abs(holed.size[0] - 20) < 0.01 && Math.abs(holed.size[1] - 20) < 0.01, `按毫米换算：${holed.size}`);
  assert.equal(holed.paths.length, 1);
  assert.equal(holed.paths[0].rings, 2, "外框与孔是同一图形的两个环");
  assert.ok(holed.paths[0].fill && !holed.paths[0].stroke, "默认填充黑色、无描边");
  assert.ok(Math.sign(holed.paths[0].areas[0]) !== Math.sign(holed.paths[0].areas[1]), "奇偶规则的孔换成反方向");
  assert.ok(Math.abs(Math.abs(holed.paths[0].areas[0]) - 400) < 1, `外框面积约 400 mm²：${holed.paths[0].areas[0]}`);
  assert.equal(holed.hostsLeft, 0, "测量用的离屏容器用完即删");

  // 2. 组变换 + <style> 类样式（只描边的圆），无 viewBox 按 96 dpi
  const circle = await run(
    '<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200"><style>.ring{fill:none;stroke:#e00;stroke-width:1}</style><g transform="translate(10,0) scale(2)"><circle class="ring" cx="5" cy="5" r="5"/></g></svg>',
  );
  assert.ok(circle.ok, circle.message);
  const diameter = (20 * 25.4) / 96;
  assert.ok(Math.abs(circle.size[0] - diameter) < 0.02, `scale(2) 后直径 20px ≈ ${diameter.toFixed(3)} mm：${circle.size[0]}`);
  assert.ok(!circle.paths[0].fill && circle.paths[0].stroke, "CSS 类样式生效：不填只描");
  assert.equal(circle.paths[0].rings, 1);
  const expectedArea = Math.PI * (diameter / 2) ** 2;
  assert.ok(Math.abs(Math.abs(circle.paths[0].areas[0]) - expectedArea) / expectedArea < 0.01, "圆的折线面积误差 < 1%");

  // 3. 开放直线：1 英寸长
  const line = await run('<svg xmlns="http://www.w3.org/2000/svg"><line x1="0" y1="0" x2="96" y2="0" stroke="black"/></svg>');
  assert.ok(line.ok, line.message);
  assert.equal(line.paths[0].open, 1);
  assert.equal(line.paths[0].rings, 0);
  assert.ok(Math.abs(line.paths[0].openLast[0] - line.paths[0].openFirst[0] - 25.4) < 0.01);

  // 4. 白名单清洗：脚本、事件属性、外链、foreignObject、image、text、use 都去掉，脚本不执行
  const hostile = await run(
    '<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" onload="window.__pwned=1" viewBox="0 0 10 10" width="10mm">' +
      '<script>window.__pwned=2</script><foreignObject><div xmlns="http://www.w3.org/1999/xhtml">x</div></foreignObject>' +
      '<image href="http://example.invalid/a.png"/><text>hi</text><use href="#r"/>' +
      '<style>@import url(http://example.invalid/x.css); .f{fill:url(http://example.invalid/g)}</style>' +
      '<rect id="r" class="f" x="0" y="0" width="10" height="10" onclick="window.__pwned=3" style="stroke:url(#g)"/></svg>',
  );
  assert.ok(hostile.ok, hostile.message);
  assert.equal(await page.evaluate(() => window.__pwned), undefined, "脚本与事件属性都没执行");
  assert.doesNotMatch(hostile.svg, /script|onload|onclick|foreignObject|<image|<text|<use|example\.invalid|@import/i);
  assert.deepEqual(hostile.ignored, { script: 1, foreignObject: 1, image: 1, text: 1, use: 1 });
  assert.ok(hostile.paths[0].fill && hostile.paths[0].stroke, "url() 引用的渐变改成纯色，仍算填充 / 描边");

  // 4b. 画幅四周有留白：显示用的 SVG 裁到图形外框，画布图片与刀路轮廓同框对齐
  const margin = await run(
    '<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="100mm" viewBox="0 0 100 100"><rect x="20" y="30" width="20" height="10"/></svg>',
  );
  assert.ok(margin.ok, margin.message);
  assert.ok(Math.abs(margin.size[0] - 20) < 0.01 && Math.abs(margin.size[1] - 10) < 0.01, `${margin.size}`);
  assert.match(margin.svg, /viewBox="20 30 20 10"/, "裁到图形外框");
  assert.match(margin.svg, /width="20mm"/);
  assert.match(margin.svg, /height="10mm"/);

  // 5. 错误提示
  const notSvg = await run("<html><body>hi</body></html>");
  assert.equal(notSvg.ok, false);
  assert.match(notSvg.message, /不是有效的 SVG/);
  const empty = await run('<svg xmlns="http://www.w3.org/2000/svg"><rect width="10" height="10" fill="none"/><text>只有文字</text></svg>');
  assert.equal(empty.ok, false);
  assert.match(empty.message, /没有可刻的图形/);

  assert.deepEqual(errors, []);
  console.log("svg-import browser: OK");
} finally {
  await browser?.close();
  await vite.close();
}
