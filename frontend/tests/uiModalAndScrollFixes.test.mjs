import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";

const TEST_DIR = path.dirname(fileURLToPath(import.meta.url));
const SRC_DIR = path.resolve(TEST_DIR, "../src");

test("吸顶栏滚动避让与通用模态框视口限高样式契约", () => {
  const css = fs.readFileSync(path.join(SRC_DIR, "styles.css"), "utf8");
  assert.match(
    css,
    /html\s*\{[^}]*scroll-padding-top:\s*calc\(var\(--ob-top-h,\s*64px\)\s*\+\s*12px\)/,
    "根滚动口必须按动态 --ob-top-h 预留 scroll-padding-top，避免吸顶顶栏遮挡滚入视口的按钮",
  );
  assert.match(
    css,
    /html:has\(\.ob-top--compact\)\s*\{[^}]*scroll-padding-top:\s*56px/,
    "顶栏收起为右上角悬浮胶囊（--ob-top-h 为 0px）时仍须保留顶部安全滚动间距，防止右侧按钮滚到胶囊底下",
  );
  assert.doesNotMatch(
    css,
    /\.laser-app-root\s*\{[^}]*z-index\s*:/s,
    ".laser-app-root 不得单独封死 z-index: 1 层叠上下文，否则会把 ApprovalCenter/AiPreviewCenter 压在 Portal 弹窗底下",
  );
  assert.match(
    css,
    /\.mat-modal,\s*\.send-confirm-dialog\s*\{[^}]*max-height:\s*calc\(100vh\s*-\s*40px\)[^}]*display:\s*flex[^}]*flex-direction:\s*column/s,
    "所有 .mat-modal 弹窗须统一限制视口最大高度并启用纵向弹性布局，防止矮窗口裁切右上角关闭与底部按钮",
  );
  assert.match(
    css,
    /\.send-confirm-dialog\s*\{[^}]*display:\s*block[^}]*overflow-y:\s*auto/s,
    ".send-confirm-dialog 无 .mat-modal-body 包裹，须回退为块级纵向滚动以免矮窗口裁切底部发送按钮",
  );
  assert.match(
    css,
    /\.mat-modal-body\s*\{[^}]*min-height:\s*0[^}]*overflow-y:\s*auto/s,
    ".mat-modal-body 须支持内部滚动",
  );
  assert.match(css, /#mat-modal\s*\{\s*z-index:\s*110;\s*\}/, "材料模态框层级应为 110");
  assert.match(css, /\.settings-box\s*\{\s*z-index:\s*115;\s*\}/, "偏好设置层级应为 115，低于安全审批中心 (125)");
  assert.match(css, /\.tts-voices-backdrop\s*\{\s*z-index:\s*115;\s*\}/, "本地合成音色弹窗层级应与设置弹窗对齐为 115");
  assert.match(css, /\.auth-box\s*\{\s*z-index:\s*132;\s*\}/, "局域网配对框层级应高于设置/材料/审批框");
  assert.match(css, /\.close-confirm-box\s*\{\s*z-index:\s*140;\s*\}/, "关闭窗口确认框层级应高于普通业务弹窗");
  assert.match(
    css,
    /\.app-dialog-backdrop\s*\{\s*z-index:\s*150;/,
    "全局确认/提示对话框层级须高于所有模态框",
  );
  assert.match(
    css,
    /\.app-dialog-ext-box\s*\{\s*z-index:\s*150;\s*\}/,
    ".app-dialog-ext-box 层级须与 .app-dialog-backdrop 对齐为 150，防止后置规则回退覆盖",
  );
});

test("模态框统一 Portal 逃逸层叠上下文、顶层栈 Esc 关闭及拖拽安全背景点击", () => {
  for (const rel of [
    "components/SettingsModal.jsx",
    "components/MaterialModal.jsx",
    "components/AuthModal.jsx",
    "components/CloseConfirmModal.jsx",
    "components/LocalTtsVoicesModal.jsx",
  ]) {
    const src = fs.readFileSync(path.join(SRC_DIR, rel), "utf8");
    assert.match(src, /createPortal\(content,\s*document\.body\)/, `${rel} 须通过 createPortal 挂载到 document.body，逃逸 .main-content-viewport 层叠上下文`);
    assert.match(src, /e\.key === "Escape"/, `${rel} 须支持 Escape 键关闭`);
    assert.match(src, /e\.isComposing/, `${rel} 须忽略中文输入法组合态按键`);
    assert.match(src, /isTopmostModalBackdrop\(backdropRef\.current\)/, `${rel} 须仅在自身处于最顶层可见模态框时响应 Escape`);
    assert.match(src, /e\.button === 0 && e\.target === e\.currentTarget/, `${rel} 背景遮罩须仅响应左键按下`);
    assert.match(src, /onMouseUp=\{\(e\)\s*=>\s*\{\s*if\s*\(e\.target !== e\.currentTarget\)\s*backdropPressRef\.current = false;/s, `${rel} 从遮罩拖入弹窗内部抬起时不得误关弹窗`);
    assert.match(src, /aria-modal="true"/, `${rel} 对话框须标注 aria-modal="true"`);
  }

  const settingsSrc = fs.readFileSync(path.join(SRC_DIR, "components/SettingsModal.jsx"), "utf8");
  assert.match(
    settingsSrc,
    /className="mat-modal-close"[^>]*disabled=\{modalBusy\}/,
    "SettingsModal 忙状态须禁用右上角关闭按钮",
  );
  assert.match(
    settingsSrc,
    /settings-btn-6"[^>]*disabled=\{modalBusy\}/,
    "SettingsModal 忙状态须禁用底部完成设置按钮",
  );

  const selectSrc = fs.readFileSync(path.join(SRC_DIR, "components/ObsidianSelectHost.jsx"), "utf8");
  assert.match(
    selectSrc,
    /createPortal\(popover,\s*document\.body\)/,
    "ObsidianSelectHost 须通过 createPortal 挂载到 document.body，避免被 Portal 模态框遮罩挡在底下",
  );
  assert.match(
    selectSrc,
    /if\s*\(event\.key === "Escape"\)\s*\{\s*event\.preventDefault\(\);\s*event\.stopPropagation\(\);\s*closeMenu\(\);/,
    "ObsidianSelectHost 按 Escape 收起下拉浮层时须阻止事件继续冒泡关闭外层模态框",
  );

  const sideNavSrc = fs.readFileSync(path.join(SRC_DIR, "components/SideNav.jsx"), "utf8");
  assert.match(
    sideNavSrc,
    /aria-label=\{collapsed \? item\.label : undefined\}/,
    "侧栏收起隐藏文字后须保留按钮无障碍名称",
  );
});

test("模态框在无 document 的 SSR 静态渲染环境下安全回退且 isTopmostModalBackdrop 按真实层级栈判定", async () => {
  const server = await createServer({
    configFile: false,
    appType: "custom",
    logLevel: "silent",
    optimizeDeps: { noDiscovery: true },
    server: { middlewareMode: true, hmr: false, watch: null },
  });
  try {
    const { isTopmostModalBackdrop } = await server.ssrLoadModule("/src/components/AppDialog.jsx");
    const { MaterialModal } = await server.ssrLoadModule("/src/components/MaterialModal.jsx");
    const { AuthModal } = await server.ssrLoadModule("/src/components/AuthModal.jsx");
    const { CloseConfirmModal } = await server.ssrLoadModule("/src/components/CloseConfirmModal.jsx");
    const { SettingsModal } = await server.ssrLoadModule("/src/components/SettingsModal.jsx");
    const { LocalTtsVoicesModal } = await server.ssrLoadModule("/src/components/LocalTtsVoicesModal.jsx");

    // 验证多层模态叠加时仅最顶层遮罩命中 isTopmostModalBackdrop
    const matBackdrop = { z: "110", inObMain: false, closest: () => null };
    const settingsBackdrop = { z: "115", inObMain: false, closest: () => null };
    const approvalBackdrop = { z: "125", inObMain: false, closest: () => null };
    const authBackdrop = { z: "132", inObMain: false, closest: () => null };
    const inPageDrawBackdrop = { z: "130", inObMain: true, closest: (sel) => (sel === ".ob-main" ? {} : null) };
    const mockWin = {
      getComputedStyle: (el) => ({ zIndex: el.z, display: el.hidden ? "none" : "flex", visibility: "visible", opacity: "1" }),
    };
    const mockDoc = (list, hasAppDialog = false) => ({
      querySelector: (sel) => (sel === '[data-testid="app-dialog"]' && hasAppDialog ? {} : null),
      querySelectorAll: () => list,
    });

    assert.equal(isTopmostModalBackdrop(matBackdrop, mockDoc([matBackdrop, authBackdrop]), mockWin), false);
    assert.equal(isTopmostModalBackdrop(authBackdrop, mockDoc([matBackdrop, authBackdrop]), mockWin), true);
    assert.equal(isTopmostModalBackdrop(settingsBackdrop, mockDoc([settingsBackdrop, approvalBackdrop]), mockWin), false);
    assert.equal(isTopmostModalBackdrop(settingsBackdrop, mockDoc([inPageDrawBackdrop, settingsBackdrop]), mockWin), true);
    assert.equal(isTopmostModalBackdrop(authBackdrop, mockDoc([authBackdrop], true), mockWin), false);

    const matHtml = renderToStaticMarkup(
      React.createElement(MaterialModal, { visible: true, initialData: null, onClose: () => {} }),
    );
    assert.match(matHtml, /id="mat-modal"/);

    const authHtml = renderToStaticMarkup(
      React.createElement(AuthModal, { visible: true, onClose: () => {} }),
    );
    assert.match(authHtml, /局域网访问配对认证/);

    const closeHtml = renderToStaticMarkup(
      React.createElement(CloseConfirmModal, { visible: true, onClose: () => {}, onConfirm: () => {} }),
    );
    assert.match(closeHtml, /关闭主窗口/);

    const settingsHtml = renderToStaticMarkup(
      React.createElement(SettingsModal, { visible: true, onClose: () => {} }),
    );
    assert.match(settingsHtml, /系统偏好设置/);

    const ttsVoicesHtml = renderToStaticMarkup(
      React.createElement(LocalTtsVoicesModal, {
        visible: true,
        modelId: "kokoro",
        modelLabel: "Kokoro 合成 v1.0（中英，多音色）",
        onClose: () => {},
      }),
    );
    assert.match(ttsVoicesHtml, /Kokoro 合成 v1\.0/);
  } finally {
    await server.close();
  }
});
