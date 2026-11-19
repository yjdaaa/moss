import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";
import {
  buildUsbProvisionWritePayload,
  createDraftFromInspect,
  deriveWsUrlForMoss,
  formatUsbWriteConfirmText,
  isRecognizedXiaozhiInspect,
  isRevokedInMoss,
  isValidOtaUrl,
  needsBoardIdentityWarning,
  pickDefaultUsbPort,
  pinSavedWifiSlot,
  removeSavedWifiSlot,
  validateUsbProvisionDraft,
} from "../src/components/voice-builtin/usbProvisionModel.js";

test("串口挑选跳过激光端口并优先选择 ESP 端口", () => {
  const port = pickDefaultUsbPort([
    { port_name: "/dev/cu.usbserial-LASER", is_laser_port: true, is_likely_esp: true },
    { port_name: "/dev/cu.usbserial-other", is_laser_port: false, is_likely_esp: false },
    { port_name: "/dev/cu.usbmodem14201", is_laser_port: false, is_likely_esp: true },
  ]);
  assert.equal(port, "/dev/cu.usbmodem14201");
});

test("读取草稿、Wi-Fi 置顶/删除与 UTF-8 / 睡眠校验", () => {
  const inspectData = {
    device: { chip: "esp32s3", mac_address: "dc:da:0c:11:22:33", flash_size: "16MB" },
    nvs: {
      wifi_slots: [
        { slot: 0, ssid: "HomeNet", has_password: true, channel: null },
        { slot: 2, ssid: "Workshop", has_password: false, channel: 6 },
      ],
      ota_url: "",
      max_tx_power: 52,
      remember_bssid: true,
      sleep_mode: true,
      sleep_dim_min: 10,
      sleep_off_min: 30,
    },
  };
  const serviceStatus = {
    ota_ip_url: "http://192.168.1.50:9666/xiaozhi/ota/",
    ota_url: "http://moss.local:9666/xiaozhi/ota/",
    ws_port: 9667,
  };

  let draft = createDraftFromInspect(inspectData, serviceStatus);
  assert.equal(draft.otaUrl, "http://192.168.1.50:9666/xiaozhi/ota/");
  assert.equal(draft.autoAllow, false, "自动允许默认不勾，由用户显式开启");
  draft = { ...draft, autoAllow: true };
  assert.equal(validateUsbProvisionDraft(draft), "");

  // 置顶第二条（原始槽位 2）并删掉原第一条后只留 slot 2
  draft = {
    ...draft,
    savedSlots: removeSavedWifiSlot(pinSavedWifiSlot(draft.savedSlots, 1), 1),
  };
  assert.deepEqual(draft.savedSlots.map((s) => s.slot), [2]);

  // 中文 UTF-8 超 32 字节拦下
  assert.match(
    validateUsbProvisionDraft({ ...draft, newSsid: "一二三四五六七八九十十一" }),
    /32 字节/,
  );
  // 睡眠关屏时间 <= 变暗时间拦下
  assert.match(
    validateUsbProvisionDraft({ ...draft, sleepDimMin: 20, sleepOffMin: 20 }),
    /大于变暗/,
  );

  // 派生 WS 地址与保留密码载荷
  const wsUrl = deriveWsUrlForMoss(draft.otaUrl, serviceStatus);
  assert.equal(wsUrl, "ws://192.168.1.50:9667/xiaozhi/v1/");
  assert.equal(
    deriveWsUrlForMoss("http://192.168.50.8:9666/xiaozhi/ota/", {
      ...serviceStatus,
      http_port: 9666,
      ota_ip_urls: [
        "http://192.168.1.50:9666/xiaozhi/ota/",
        "http://192.168.50.8:9666/xiaozhi/ota/",
      ],
    }),
    "ws://192.168.50.8:9667/xiaozhi/v1/",
    "命中本机另一张网卡 IP + 相同 HTTP 端口应派生 MOSS WebSocket 地址",
  );
  assert.equal(
    deriveWsUrlForMoss("http://other-host.example.com:9666/xiaozhi/ota/", {
      ...serviceStatus,
      http_port: 9666,
      ota_ip_urls: [
        "http://192.168.1.50:9666/xiaozhi/ota/",
        "http://192.168.50.8:9666/xiaozhi/ota/",
      ],
    }),
    "",
    "同端口的非本机公网域名不得误判为 MOSS",
  );
  assert.equal(
    deriveWsUrlForMoss("http://192.168.1.99:9666/xiaozhi/ota/", {
      ...serviceStatus,
      http_port: 9666,
      ota_ip_urls: [
        "http://192.168.1.50:9666/xiaozhi/ota/",
        "http://192.168.50.8:9666/xiaozhi/ota/",
      ],
    }),
    "",
    "同端口的局域网其它主机 IP 不得误判为本机 MOSS",
  );
  assert.equal(deriveWsUrlForMoss("https://api.tenclass.net/xiaozhi/ota/", serviceStatus), "");

  const payload = buildUsbProvisionWritePayload({
    port: "/dev/cu.usbmodem14201",
    inspectData,
    draft,
    wsUrl,
  });
  assert.equal(payload.action, "write");
  assert.equal(payload.confirmed, true);
  assert.equal(payload.auto_allow, true);
  assert.equal(payload.expected_mac, "dc:da:0c:11:22:33");
  assert.deepEqual(payload.patch.retain_slots, [2]);
  assert.equal(payload.patch.ws_url, "ws://192.168.1.50:9667/xiaozhi/v1/");

  // 改为外部 OTA 地址或关闭自动放行时一并清掉 ws_url 且强制 auto_allow=false
  const externalPayload = buildUsbProvisionWritePayload({
    port: "/dev/cu.usbmodem14201",
    inspectData,
    draft: {
      ...draft,
      otaUrl: "https://api.tenclass.net/xiaozhi/ota/",
      autoAllow: true,
    },
    wsUrl: "",
  });
  assert.equal(externalPayload.auto_allow, false);
  assert.equal(externalPayload.patch.ws_url, "");

  const noAutoAllowPayload = buildUsbProvisionWritePayload({
    port: "/dev/cu.usbmodem14201",
    inspectData,
    draft: { ...draft, autoAllow: false },
    wsUrl,
  });
  assert.equal(noAutoAllowPayload.auto_allow, false);
  assert.equal(noAutoAllowPayload.patch.ws_url, "");

  // 清空 OTA 地址时清掉 ws_url 且不自动登记，恢复出厂
  const clearPayload = buildUsbProvisionWritePayload({
    port: "/dev/cu.usbmodem14201",
    inspectData,
    draft: { ...createDraftFromInspect(inspectData, serviceStatus), otaUrl: "", autoAllow: true },
    wsUrl: "",
  });
  assert.equal(clearPayload.auto_allow, false);
  assert.equal(clearPayload.patch.ota_url, "");
  assert.equal(clearPayload.patch.ws_url, "");
  assert.equal(clearPayload.patch.retain_slots, undefined);

  // 未检出小智特征的板卡判定
  assert.equal(isRecognizedXiaozhiInspect(null), false);
  assert.equal(
    isRecognizedXiaozhiInspect({
      partition: { has_xiaozhi_markers: false },
      nvs: { namespaces: ["wifi", "nvs.net80211"] },
    }),
    false,
  );
  assert.equal(
    isRecognizedXiaozhiInspect({
      partition: { has_xiaozhi_markers: true },
      nvs: { namespaces: [] },
    }),
    true,
  );
  assert.equal(
    isRecognizedXiaozhiInspect({
      partition: { has_xiaozhi_markers: false },
      nvs: { namespaces: ["board"] },
    }),
    true,
  );
  assert.equal(
    isRecognizedXiaozhiInspect({
      partition: { has_xiaozhi_markers: false },
      nvs: { namespaces: ["websocket"] },
    }),
    true,
  );

  const confirmMsg = formatUsbWriteConfirmText({
    port: "/dev/cu.usbmodem14201",
    inspectData,
    payload,
  });
  assert.match(confirmMsg, /dc:da:0c:11:22:33/);
  assert.match(confirmMsg, /192\.168\.1\.50:9666/);
});

test("渲染无弹窗暗色双标签小智配网卡片、服务页入口且设备表不重复挂按钮", async () => {
  const prevWindow = globalThis.window;
  globalThis.window = {
    __TAURI_INTERNALS__: { invoke: async () => ({}) },
    addEventListener: () => {},
    removeEventListener: () => {},
  };
  const server = await createServer({
    configFile: false,
    appType: "custom",
    logLevel: "silent",
    server: { middlewareMode: true },
  });
  try {
    const [{ XiaozhiUsbProvisionCard }, { BuiltinServicePanel }, { BuiltinDevicesPanel }] =
      await Promise.all([
        server.ssrLoadModule("/src/components/voice-builtin/XiaozhiUsbProvisionCard.jsx"),
        server.ssrLoadModule("/src/components/voice-builtin/BuiltinServicePanel.jsx"),
        server.ssrLoadModule("/src/components/voice-builtin/BuiltinDevicesPanel.jsx"),
      ]);

    const cardHtml = renderToStaticMarkup(
      React.createElement(XiaozhiUsbProvisionCard, {
        serviceStatus: { ota_ip_url: "http://192.168.1.50:9666/xiaozhi/ota/", ws_port: 9667 },
      }),
    );
    assert.match(cardHtml, /Wi-Fi 配置/);
    assert.match(cardHtml, /高级选项/);
    assert.match(cardHtml, /已保存的 Wi-Fi/);
    assert.match(cardHtml, /新的 Wi-Fi/);
    assert.match(cardHtml, /连接 \/ 写入板子/);
    assert.match(cardHtml, /读取板子配置/);
    assert.doesNotMatch(cardHtml, /modal-backdrop|role="dialog"|xiaozhi-usb-close/);

    const status = {
      running: true,
      mode: "conversation",
      ota_url: "http://moss.local:9666/xiaozhi/ota/",
      ota_ip_url: "http://192.168.1.50:9666/xiaozhi/ota/",
      ws_port: 9667,
      http_port: 9666,
      devices: [],
      sessions: [],
    };
    const svcHtml = renderToStaticMarkup(
      React.createElement(BuiltinServicePanel, { builtin: { status, busy: "", act: () => {} } }),
    );
    assert.match(svcHtml, /id="voice-builtin-usb-provision"[^>]*>.*USB 一键配网写地址/);
    assert.doesNotMatch(svcHtml, /modal-backdrop|role="dialog"/);

    const devHtml = renderToStaticMarkup(
      React.createElement(BuiltinDevicesPanel, { builtin: { status, busy: "", act: () => {} } }),
    );
    assert.doesNotMatch(devHtml, /voice-devices-usb-provision|USB 一键配网写地址|modal-backdrop|role="dialog"/);
  } finally {
    await server.close();
    globalThis.window = prevWindow;
  }
});

test("OTA 地址规则与官方固件 IsValidOtaUrl 的全部用例一致", () => {
  // 来源：78/esp-wifi-connect tests/url_cases.json（2026-11-12 7709506）
  for (const [url, valid] of [
    ["", true],
    ["Huawei-1ct5dn", false],
    ["https://api.example.com/ota/", true],
    ["http://192.168.1.2:8000/xiaozhi/ota/", true],
    ["http://localhost/ota", true],
    ["http://a", true],
    ["HTTP://EXAMPLE.COM/ota", true],
    ["https://example.com:65535/ota?q=a%20b", true],
    ["https://[2001:db8::1]:8443/ota/", true],
    ["http://user:password@example.com/ota", true],
    ["https://example.com/%E4%B8%AD%E6%96%87", true],
    ["https://example.com/中文", false],
    ["/ota/", false],
    ["//example.com/ota", false],
    ["ftp://example.com/ota", false],
    ["javascript:alert(1)", false],
    ["https:example.com", false],
    ["https://", false],
    ["https:///ota", false],
    ["https://?q=x", false],
    ["https://#fragment", false],
    ["https://:8080/ota", false],
    ["https://example.com:0/ota", false],
    ["https://example.com:65536/ota", false],
    ["https://example.com:abc/ota", false],
    [" https://example.com/ota", false],
    ["https://example.com/ota ", false],
    ["https://example.com/a b", false],
    ["https://example.com/a\nb", false],
    ["https://example.com\\ota", false],
    ["https://example.com/%", false],
    ["https://example.com/%0x", false],
  ]) {
    assert.equal(isValidOtaUrl(url), valid, JSON.stringify(url));
  }
});

test("高级选项只提交改过的项：出厂没设过的休眠不会被写成关闭", () => {
  const factory = {
    device: { chip: "esp32s3", mac_address: "dc:da:0c:11:22:33" },
    nvs: { wifi_slots: [], ota_url: "", remember_bssid: false, sleep_mode: null, sleep_dim_min: null, sleep_off_min: null },
  };
  const status = { ota_ip_url: "http://192.168.1.50:9666/xiaozhi/ota/", ws_port: 9667 };
  const draft = createDraftFromInspect(factory, status);
  assert.equal(draft.sleepMode, true, "没有 sleep_mode 键时固件默认开启");
  assert.equal(draft.sleepDimMin, "", "空 = 板型默认");
  const untouched = buildUsbProvisionWritePayload({
    port: "/dev/cu.usbmodem1",
    inspectData: factory,
    draft: { ...draft, newSsid: "Office", newPassword: "pw" },
  });
  for (const key of ["sleep_mode", "sleep_dim_min", "sleep_off_min", "max_tx_power", "remember_bssid"]) {
    assert.equal(key in untouched.patch, false, `只改 Wi-Fi 时不提交 ${key}`);
  }
  const changed = buildUsbProvisionWritePayload({
    port: "/dev/cu.usbmodem1",
    inspectData: factory,
    draft: { ...draft, sleepMode: false, sleepDimMin: "5", sleepOffMin: "30" },
  });
  assert.equal(changed.patch.sleep_mode, false);
  assert.equal(changed.patch.sleep_dim_min, 5);
  assert.equal(changed.patch.sleep_off_min, 30);
  // 已设过的值清空 = 回到板型默认（后端擦除该键）
  const set = createDraftFromInspect(
    { ...factory, nvs: { ...factory.nvs, sleep_dim_min: 10 } },
    status,
  );
  assert.equal(set.sleepDimMin, "10");
  const cleared = buildUsbProvisionWritePayload({
    port: "/dev/cu.usbmodem1",
    inspectData: factory,
    draft: { ...set, sleepDimMin: "" },
  });
  assert.equal(cleared.patch.sleep_dim_min, -1);
  assert.match(validateUsbProvisionDraft({ ...set, sleepDimMin: "abc" }), /0～1440/);
  assert.equal(validateUsbProvisionDraft({ ...set, sleepDimMin: "", sleepOffMin: "" }), "");
  // SSID 按原样保存，全空白当作没填
  const spaced = buildUsbProvisionWritePayload({
    port: "/dev/cu.usbmodem1",
    inspectData: factory,
    draft: { ...draft, newSsid: " Lab ", newPassword: "pw" },
  });
  assert.equal(spaced.patch.new_wifi.ssid, " Lab ");
  const blank = buildUsbProvisionWritePayload({
    port: "/dev/cu.usbmodem1",
    inspectData: factory,
    draft: { ...draft, newSsid: "   " },
  });
  assert.equal(blank.patch.new_wifi, undefined);
});

test("被撤销过的设备要自动允许必须显式解除撤销", () => {
  const inspectData = {
    device: { chip: "esp32s3", mac_address: "dc:da:0c:11:22:33" },
    nvs: { wifi_slots: [], ota_url: "" },
    known_device: { device_id: "dc:da:0c:11:22:33", status: "revoked", status_revision: 4 },
  };
  const status = { ota_ip_url: "http://192.168.1.50:9666/xiaozhi/ota/", ws_port: 9667 };
  assert.equal(isRevokedInMoss(inspectData), true);
  const draft = { ...createDraftFromInspect(inspectData, status), autoAllow: true };
  const wsUrl = deriveWsUrlForMoss(draft.otaUrl, status);
  const plain = buildUsbProvisionWritePayload({ port: "/dev/cu.usbmodem1", inspectData, draft, wsUrl });
  assert.equal(plain.unrevoke, false);
  assert.equal(plain.expected_status_revision, 4);
  const confirmed = buildUsbProvisionWritePayload({
    port: "/dev/cu.usbmodem1",
    inspectData,
    draft: { ...draft, unrevoke: true },
    wsUrl,
  });
  assert.equal(confirmed.unrevoke, true);
  assert.equal(confirmed.expected_status_revision, 4);
  assert.match(
    formatUsbWriteConfirmText({ port: "/dev/cu.usbmodem1", inspectData, payload: confirmed }),
    /解除撤销/,
  );
});

test("乐鑫原生 USB 口优先，转换芯片串口读取前提示确认是小智", () => {
  const ports = [
    { port_name: "/dev/cu.usbserial-1", vid: 0x1a86, is_laser_port: false, is_likely_esp: true },
    { port_name: "/dev/cu.usbmodem1", vid: 0x303a, is_laser_port: false, is_likely_esp: true },
  ];
  assert.equal(pickDefaultUsbPort(ports), "/dev/cu.usbmodem1");
  assert.equal(needsBoardIdentityWarning(ports[0]), true);
  assert.equal(needsBoardIdentityWarning(ports[1]), false);
  assert.equal(needsBoardIdentityWarning(undefined), false);
});
