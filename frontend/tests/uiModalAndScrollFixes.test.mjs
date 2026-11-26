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

// styles.css 是按层叠顺序 @import 各分片（src/styles/*.css）的入口：按同样顺序内联回完整样式表再断言。
function readStylesheet(file) {
  const css = fs.readFileSync(file, "utf8");
  return css.replace(/^@import\s+["']([^"']+)["'];[ \t]*\r?\n?/gm, (_, rel) => readStylesheet(path.resolve(path.dirname(file), rel)));
}

test("吸顶栏滚动避让与通用模态框视口限高样式契约", () => {
  const css = readStylesheet(path.join(SRC_DIR, "styles.css"));
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
  assert.doesNotMatch(
    css,
    /\.ob-main\s*\{[^}]*z-index\s*:/s,
    ".ob-main 不得设置 z-index 锁死层叠上下文，否则窄屏侧栏会压住内部顶栏与急停按钮",
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
  assert.match(css, /\.device-conn-backdrop\s*\{\s*z-index:\s*126;\s*\}/, "统一设备连接弹窗层级应为 126，高于 AI 预览窗 (125) 以便就地配连接，低于严格模式审批卡 (128)");
  assert.match(css, /\.voice-device-allow-backdrop\s*\{\s*z-index:\s*127;\s*\}/, "小智新设备接入居中弹窗层级应为 127");
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
    "components/DeviceConnectionModal.jsx",
    "components/MaterialModal.jsx",
    "components/AuthModal.jsx",
    "components/CloseConfirmModal.jsx",
    "components/LocalTtsVoicesModal.jsx",
    "components/ApprovalCenter.jsx",
    "components/AiPreviewCenter.jsx",
    "components/VoiceDeviceAllowModal.jsx",
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
  assert.match(
    selectSrc,
    /className="ob-select-group-label"/,
    "ObsidianSelectHost 须渲染 <optgroup> 分组小标题，避免本地音色语种与方言分组在浮层中丢失",
  );

  const sideNavSrc = fs.readFileSync(path.join(SRC_DIR, "components/SideNav.jsx"), "utf8");
  assert.match(
    sideNavSrc,
    /aria-label=\{collapsed \? (?:collapsedLabel|item\.label) : undefined\}/,
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
    const { DeviceConnectionModal } = await server.ssrLoadModule("/src/components/DeviceConnectionModal.jsx");
    const { LocalTtsVoicesModal } = await server.ssrLoadModule("/src/components/LocalTtsVoicesModal.jsx");

    // 验证多层模态叠加时仅最顶层遮罩命中 isTopmostModalBackdrop
    const matBackdrop = { z: "110", inObMain: false, closest: () => null };
    const settingsBackdrop = { z: "115", inObMain: false, closest: () => null };
    const approvalBackdrop = { z: "125", inObMain: false, closest: () => null };
    const deviceConnBackdrop = { z: "126", inObMain: false, closest: () => null };
    const strictApprovalBackdrop = { z: "128", inObMain: false, closest: () => null };
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
    assert.equal(isTopmostModalBackdrop(deviceConnBackdrop, mockDoc([approvalBackdrop, deviceConnBackdrop]), mockWin), true);
    assert.equal(isTopmostModalBackdrop(deviceConnBackdrop, mockDoc([deviceConnBackdrop, strictApprovalBackdrop]), mockWin), false);
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
    const { version } = JSON.parse(fs.readFileSync(path.join(TEST_DIR, "../package.json"), "utf8"));
    assert.ok(settingsHtml.includes(`MOSS v${version}</strong>`), "缺少构建注入时使用包版本");

    const ttsVoicesHtml = renderToStaticMarkup(
      React.createElement(LocalTtsVoicesModal, {
        visible: true,
        modelId: "kokoro",
        modelLabel: "Kokoro 合成 v1.0（中英，多音色）",
        onClose: () => {},
      }),
    );
    assert.match(ttsVoicesHtml, /Kokoro 合成 v1\.0/);

    const connHtml = renderToStaticMarkup(
      React.createElement(DeviceConnectionModal, { visible: true, onClose: () => {} }),
    );
    assert.match(connHtml, /id="device-connection-modal"/);
    assert.match(connHtml, /设备连接/);

    // 安卓手机浏览器访问电脑端 http://<device-ip>:<port> 时，保留串口选项且不覆盖已存的 serial 模式
    const prevWin = Object.getOwnPropertyDescriptor(globalThis, "window");
    const prevNav = Object.getOwnPropertyDescriptor(globalThis, "navigator");
    const prevStorage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
    const storeMap = new Map([
      ["laser-forge-conn-mode", "serial"],
      ["laser-forge-serial-port", "COM3"],
      ["laser-forge-baud", "115200"],
    ]);
    Object.defineProperty(globalThis, "window", {
      configurable: true,
      value: { location: { protocol: "http:", hostname: "192.0.2.10", port: "9666" } },
    });
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: { userAgent: "Mozilla/5.0 (Linux; Android 14; Pixel 8) Chrome/126.0.0.0 Mobile Safari/537.36" },
    });
    Object.defineProperty(globalThis, "localStorage", {
      configurable: true,
      value: {
        getItem: (k) => (storeMap.has(k) ? storeMap.get(k) : null),
        setItem: (k, v) => storeMap.set(k, String(v)),
        removeItem: (k) => storeMap.delete(k),
      },
    });
    try {
      const androidLanConnHtml = renderToStaticMarkup(
        React.createElement(DeviceConnectionModal, { visible: true, onClose: () => {} }),
      );
      assert.match(androidLanConnHtml, /<option value="serial"[^>]*>串口 \(USB \/ Serial\)<\/option>/);
      assert.match(androidLanConnHtml, /id="conn-serial-port"/, "已存 serial 模式在安卓手机网页上保持串口表单");
      assert.equal(storeMap.get("laser-forge-conn-mode"), "serial");
    } finally {
      if (prevWin) Object.defineProperty(globalThis, "window", prevWin);
      else delete globalThis.window;
      if (prevNav) Object.defineProperty(globalThis, "navigator", prevNav);
      else delete globalThis.navigator;
      if (prevStorage) Object.defineProperty(globalThis, "localStorage", prevStorage);
      else delete globalThis.localStorage;
    }

    const { buildSelectOptions } = await server.ssrLoadModule("/src/components/ObsidianSelectHost.jsx");
    const parsedOpts = buildSelectOptions({
      options: [
        { value: "45", textContent: "45 · 晓北（女声）", disabled: false, parentElement: { tagName: "OPTGROUP", label: "中文普通话（推荐）" } },
        { value: "8", textContent: "8 · Eric（男声 · 四川话）", disabled: false, parentElement: { tagName: "OPTGROUP", label: "中文 · 方言 / 口音" } },
        { value: "0", textContent: "默认项", disabled: false, parentElement: { tagName: "SELECT" } },
      ],
    });
    assert.equal(parsedOpts[0].group, "中文普通话（推荐）");
    assert.equal(parsedOpts[0].title, "45 · 晓北");
    assert.equal(parsedOpts[0].badge, "女声");
    assert.equal(parsedOpts[1].group, "中文 · 方言 / 口音");
    assert.equal(parsedOpts[2].group, "");

    const {
      ALLOW_SUCCESS_DETAIL,
      ALLOW_SUCCESS_FEEDBACK_MS,
      ALLOW_SUCCESS_HEADLINE,
      ANTI_SPOOF_PROMPT_TEXT,
      VoiceDeviceAllowModal,
      executeAllowVoiceDevice,
      formatRelativeUnixSeconds,
      nextPendingDeviceIdAfterAction,
      readDeviceAllowDraft,
      resolvePendingDeviceQueue,
      selectPendingVoiceDevices,
      stepPendingQueueDeviceId,
      voiceDeviceAllowHeading,
      writeDeviceAllowDraft,
    } = await server.ssrLoadModule("/src/components/VoiceDeviceAllowModal.jsx");
    const { AUTO_ALLOW_DEVICES_CONFIRM_TEXT } = await server.ssrLoadModule("/src/voiceState.js");
    const builtinDevicesSrc = fs.readFileSync(
      path.join(SRC_DIR, "components/voice-builtin/BuiltinDevicesPanel.jsx"),
      "utf8",
    );
    assert.match(
      builtinDevicesSrc,
      /AUTO_ALLOW_DEVICES_CONFIRM_TEXT/,
      "BuiltinDevicesPanel 与弹窗须共用同一份自动允许局域网信任确认文案常量",
    );
    assert.match(
      AUTO_ALLOW_DEVICES_CONFIRM_TEXT,
      /只对之后新连入.*已在排队的设备仍需逐台允许/s,
      "自动允许确认文案须写明只对之后新连入设备生效、已在排队的仍需逐台允许",
    );

    const dismissed = new Map([["aa:bb:01", 100]]);
    const statusWithPending = {
      local: { settings: { auto_allow_devices: false } },
      devices: [
        { device_id: "aa:bb:01", status: "pending", last_seen: 100, model: "lichuang-dev", firmware_version: "2.2.0" },
        { device_id: "aa:bb:02", status: "pending", last_seen: 120, model: "lichuang-dev", firmware_version: "2.2.0", last_ip: "<device-ip>" },
        { device_id: "aa:bb:03", status: "allowed", last_seen: 130 },
      ],
    };
    assert.deepEqual(
      selectPendingVoiceDevices(statusWithPending, dismissed).map((d) => d.device_id),
      ["aa:bb:02"],
      "同一次来访已点稍后处理的设备不重复弹出，只弹出新待允许设备",
    );
    assert.deepEqual(
      selectPendingVoiceDevices({ ...statusWithPending, local: { settings: { auto_allow_devices: true } } }, dismissed).map((d) => d.device_id),
      ["aa:bb:02"],
      "开启自动允许后仅对后续新设备生效，存量待允许设备仍照常弹窗逐台确认",
    );

    // 勾选“以后自动允许”时必须先弹局域网信任确认，取消则整体不执行
    const cancelCalls = [];
    const cancelledRes = await executeAllowVoiceDevice({
      device: { device_id: "aa:bb:02", name: "" },
      nameDraft: "工作台小智",
      autoAllowAll: true,
      confirm: async (msg) => {
        cancelCalls.push(msg);
        return false;
      },
      api: {
        deviceSetStatus: async () => {
          throw new Error("取消确认时不得调用 deviceSetStatus");
        },
      },
      alert: async () => {
        throw new Error("取消确认时不得报错弹窗");
      },
    });
    assert.equal(cancelledRes.cancelled, true);
    assert.equal(cancelledRes.allowed, false);
    assert.deepEqual(cancelCalls, [AUTO_ALLOW_DEVICES_CONFIRM_TEXT]);

    // deviceSetStatus 本身失败时提示“允许设备失败”
    const allowFailAlerts = [];
    const allowFailRes = await executeAllowVoiceDevice({
      device: { device_id: "aa:bb:02", name: "" },
      nameDraft: "工作台小智",
      autoAllowAll: false,
      api: {
        deviceSetStatus: async () => {
          throw new Error("数据库繁忙");
        },
      },
      alert: async (msg, opts) => {
        allowFailAlerts.push({ msg, opts });
      },
    });
    assert.equal(allowFailRes.allowed, false);
    assert.equal(allowFailAlerts.length, 1);
    assert.equal(allowFailAlerts[0].opts.title, "允许设备失败");

    // 允许成功后改名或自动允许保存失败时，分段提示“设备已允许，但备注名 / 自动允许开关保存失败”
    const partialAlerts = [];
    const partialRes = await executeAllowVoiceDevice({
      device: { device_id: "aa:bb:02", name: "" },
      nameDraft: "工作台小智",
      autoAllowAll: true,
      confirm: async () => true,
      api: {
        deviceSetStatus: async () => ({ devices: [{ device_id: "aa:bb:02", status: "allowed" }] }),
        deviceRename: async () => {
          throw new Error("名称含非法字符");
        },
        localSave: async () => {
          throw new Error("写盘失败");
        },
      },
      alert: async (msg, opts) => {
        partialAlerts.push({ msg, opts });
      },
    });
    assert.equal(partialRes.allowed, true);
    assert.equal(partialAlerts.length, 1);
    assert.equal(partialAlerts[0].opts.title, "设备已允许，但备注名和自动允许开关保存失败");
    assert.match(partialAlerts[0].msg, /设备已允许，但备注名和自动允许开关保存失败/);

    const multiPendingStatus = {
      local: { settings: { auto_allow_devices: false } },
      devices: [
        {
          device_id: "aa:bb:01",
          name: "",
          status: "pending",
          first_seen: 1700,
          last_seen: 1995,
          model: "lichuang-dev",
          firmware_version: "2.2.0",
        },
        {
          device_id: "aa:bb:02",
          name: "书桌小智",
          status: "pending",
          first_seen: 1000,
          last_seen: 1960,
          model: "esp32-s3-box",
          firmware_version: "2.3.1",
          last_ip: "<device-ip>",
        },
      ],
    };

    // C2 标题区名称回退与相对时间计算
    assert.equal(voiceDeviceAllowHeading(multiPendingStatus.devices[0]), "lichuang-dev", "无备注名时标题区显示型号");
    assert.equal(voiceDeviceAllowHeading(multiPendingStatus.devices[1]), "书桌小智", "有备注名时标题区优先显示名称");
    assert.equal(formatRelativeUnixSeconds(1995, 2000), "刚刚");
    assert.equal(formatRelativeUnixSeconds(1960, 2000), "40 秒前");
    assert.equal(formatRelativeUnixSeconds(1700, 2000), "5 分钟前");
    assert.equal(formatRelativeUnixSeconds(0, 2000), "—");

    // C3 多台排队切换、草稿按 device_id 隔离、允许/拒绝后自动衔接下一台
    const pendingTwo = selectPendingVoiceDevices(multiPendingStatus);
    assert.equal(resolvePendingDeviceQueue(pendingTwo, "").index, 0);
    const secondId = stepPendingQueueDeviceId(pendingTwo, "aa:bb:01", 1);
    assert.equal(secondId, "aa:bb:02");
    assert.equal(stepPendingQueueDeviceId(pendingTwo, secondId, -1), "aa:bb:01");
    assert.equal(nextPendingDeviceIdAfterAction(pendingTwo, "aa:bb:01"), "aa:bb:02");
    assert.equal(nextPendingDeviceIdAfterAction(pendingTwo, "aa:bb:02"), "aa:bb:01");

    let drafts = {};
    drafts = writeDeviceAllowDraft(drafts, pendingTwo[0], { nameDraft: "工作台小智草稿", autoAllowAll: true });
    assert.deepEqual(readDeviceAllowDraft(drafts, pendingTwo[0]), {
      nameDraft: "工作台小智草稿",
      autoAllowAll: true,
    });
    assert.deepEqual(
      readDeviceAllowDraft(drafts, pendingTwo[1]),
      { nameDraft: "书桌小智", autoAllowAll: false },
      "切换到第二台设备时不得串入第一台的备注草稿与自动允许勾选状态",
    );

    const allowModalHtml = renderToStaticMarkup(
      React.createElement(VoiceDeviceAllowModal, {
        voiceEnabled: true,
        initialStatus: multiPendingStatus,
        initialDrafts: drafts,
        nowSeconds: 2000,
      }),
    );
    // C1 防冒认提示出现在设备信息 <dl> 上方
    assert.ok(allowModalHtml.includes(ANTI_SPOOF_PROMPT_TEXT), "须包含防冒认核对提示");
    assert.ok(
      allowModalHtml.indexOf('id="voice-device-allow-verify-tip"') < allowModalHtml.indexOf('class="approval-kv'),
      "防冒认提示须排在设备信息列表上方",
    );
    // C2 型号标题、等宽 MAC + 复制按钮、首次出现 / 最近来访相对时间
    assert.match(allowModalHtml, /id="voice-device-allow-heading">lichuang-dev</);
    assert.match(allowModalHtml, /发现小智设备请求连接/);
    assert.match(allowModalHtml, /<code class="mono" id="voice-device-allow-mac">aa:bb:01<\/code>/);
    assert.match(allowModalHtml, /id="voice-device-allow-copy-mac"/);
    assert.match(allowModalHtml, /id="voice-device-allow-seen"[^>]*>5 分钟前 \/ 刚刚</);
    // C3 可切换队列（第 1 / 2 台，上一台禁用、下一台可点）及草稿回显
    assert.match(allowModalHtml, /第 <b class="mono">1<\/b> \/ <b class="mono">2<\/b> 台待允许设备/);
    assert.match(allowModalHtml, /id="voice-device-allow-prev"[^>]*disabled=""/);
    assert.doesNotMatch(allowModalHtml, /id="voice-device-allow-next"[^>]*disabled/);
    assert.match(allowModalHtml, /id="voice-device-allow-name"[^>]*value="工作台小智草稿"/);
    assert.match(allowModalHtml, /id="voice-device-allow-auto-all"[^>]*checked=""/);

    // 切到第 2 台渲染：草稿完全隔离，不串入第 1 台输入与勾选
    const secondModalHtml = renderToStaticMarkup(
      React.createElement(VoiceDeviceAllowModal, {
        voiceEnabled: true,
        initialStatus: multiPendingStatus,
        initialSelectedId: "aa:bb:02",
        initialDrafts: drafts,
        nowSeconds: 2000,
      }),
    );
    assert.match(secondModalHtml, /id="voice-device-allow-heading">书桌小智</);
    assert.match(secondModalHtml, /第 <b class="mono">2<\/b> \/ <b class="mono">2<\/b> 台待允许设备/);
    assert.doesNotMatch(secondModalHtml, /id="voice-device-allow-prev"[^>]*disabled/);
    assert.match(secondModalHtml, /id="voice-device-allow-next"[^>]*disabled=""/);
    assert.match(secondModalHtml, /id="voice-device-allow-name"[^>]*value="书桌小智"/);
    assert.doesNotMatch(secondModalHtml, /工作台小智草稿/);
    assert.doesNotMatch(secondModalHtml, /id="voice-device-allow-auto-all"[^>]*checked/);

    // C4 按钮层级（主按钮在最右侧、拒绝按钮不用实心 .btn-danger、初始焦点不在允许按钮上）与 C5/C6 契约
    const laterPos = allowModalHtml.indexOf('id="voice-device-allow-later"');
    const rejectPos = allowModalHtml.indexOf('id="voice-device-allow-reject"');
    const confirmPos = allowModalHtml.indexOf('id="voice-device-allow-confirm"');
    assert.ok(laterPos > 0 && rejectPos > laterPos && confirmPos > rejectPos, "主按钮允许并记住此设备须排在最右侧");
    assert.match(allowModalHtml, /class="btn voice-device-allow-reject" id="voice-device-allow-reject"/);
    assert.doesNotMatch(allowModalHtml, /btn-danger/, "拒绝接入按钮须改为描边次要危险样式，不使用实心 btn-danger");
    assert.doesNotMatch(allowModalHtml, /id="voice-device-allow-confirm"[^>]*autofocus/i, "允许按钮不得带初始焦点");
    assert.match(
      allowModalHtml,
      /aria-describedby="voice-device-allow-verify-tip voice-device-allow-desc"/,
      "对话框须具备 aria-describedby 指向防冒认提示与底部说明",
    );
    assert.equal(ALLOW_SUCCESS_FEEDBACK_MS, 1500);
    assert.match(ALLOW_SUCCESS_HEADLINE, /已允许，现在唤醒小智即可对话/);
    assert.match(ALLOW_SUCCESS_DETAIL, /常连模式.*重启设备/s);
    assert.match(ALLOW_SUCCESS_DETAIL, /拍照识图.*下次重启后才可用/s);

    const addendaCss = fs.readFileSync(path.join(SRC_DIR, "styles/15-addenda-2026-11.css"), "utf8");
    const n133Css = addendaCss.slice(addendaCss.indexOf("/* 2026-11-26（n133）"));
    assert.match(n133Css, /\.voice-device-allow-reject\s*\{[^}]*background:\s*transparent;[^}]*color:\s*var\(--ob-red\);/s);
    assert.match(n133Css, /\.voice-device-allow-foot\s*\{[^}]*flex-wrap:\s*wrap;/s);
    assert.doesNotMatch(n133Css, /#[0-9a-fA-F]{3,8}\b|rgba?\(/, "n133 弹窗样式颜色只使用已有 CSS 变量，兼容深浅主题");
  } finally {
    await server.close();
  }
});

test("n134：BuiltinDevicesPanel 仅在 allowed / pending 设备显示移除按钮，已撤销设备默认折叠在底部并保留重新允许", async () => {
  const server = await createServer({
    root: path.resolve(TEST_DIR, ".."),
    server: { middlewareMode: true },
    appType: "custom",
    logLevel: "silent",
  });
  try {
    const { BuiltinDevicesPanel } = await server.ssrLoadModule("/src/components/voice-builtin/BuiltinDevicesPanel.jsx");
    const status = {
      running: true,
      mode: "chat",
      link_mode: "builtin",
      ws_port: 8000,
      http_port: 8003,
      sessions: [],
      device_prefs: {},
      local: { settings: { auto_allow_devices: false } },
      devices: [
        { device_id: "aa:bb:cc:00:00:01", name: "书房小智", model: "lichuang-dev", firmware_version: "2.2.0", status: "allowed", last_seen: 1_790_000_100 },
        { device_id: "aa:bb:cc:00:00:02", name: "", model: "box-3", firmware_version: "2.2.0", status: "pending", last_seen: 1_790_000_090 },
        { device_id: "aa:bb:cc:00:00:03", name: "旧测试机一", model: "lichuang-dev", firmware_version: "2.1.0", status: "revoked", last_seen: 1_790_000_050 },
        { device_id: "aa:bb:cc:00:00:04", name: "旧测试机二", model: "box-3", firmware_version: "2.0.0", status: "revoked", last_seen: 1_790_000_040 },
      ],
    };
    const html = renderToStaticMarkup(
      React.createElement(BuiltinDevicesPanel, { builtin: { status, busy: "", act: () => {} } }),
    );

    const removeIds = [...html.matchAll(/data-device-remove="([^"]+)"/g)].map((m) => m[1]);
    assert.deepEqual(
      removeIds,
      ["aa:bb:cc:00:00:01", "aa:bb:cc:00:00:02"],
      "移除按钮只出现在 allowed 与 pending 设备上，不出现在 revoked 设备上",
    );

    const foldIdx = html.indexOf('id="voice-revoked-devices"');
    assert.ok(foldIdx > 0, "存在已撤销设备时须渲染底部折叠分组");
    const foldTag = html.slice(html.lastIndexOf("<details", foldIdx), html.indexOf(">", foldIdx) + 1);
    assert.doesNotMatch(foldTag, /\bopen\b/i, "已撤销设备分组须默认折叠收起");
    const foldHtml = html.slice(foldIdx);
    assert.match(foldHtml, /<summary>已撤销（2 台）<\/summary>/, "已撤销分组计数须准确");
    assert.match(foldHtml, /重新允许/, "已撤销分组内须保留重新允许按钮");
    assert.doesNotMatch(foldHtml, /data-device-remove=/, "已撤销分组内不渲染移除按钮");
  } finally {
    await server.close();
  }
});
