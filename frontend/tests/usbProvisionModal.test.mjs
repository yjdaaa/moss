import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createServer } from "vite";
import {
  applyPickedWifi,
  boardSupports5g,
  buildUsbProvisionWritePayload,
  canOpenWifiSettings,
  createDraftFromInspect,
  currentWifiFillNotice,
  boardWifiBands,
  filterScannedWifiForBoard,
  hostWifiProblem,
  scanFilterLabel,
  staleBoardOta,
  mossOtaUrls,
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
  wifiBandLabel,
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
    assert.match(cardHtml, /id="xiaozhi-wifi-use-current"[^>]*>.*用本机当前 Wi-Fi/);
    assert.match(cardHtml, /id="xiaozhi-wifi-scan"[^>]*>.*扫描附近/);
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

test("本机 Wi-Fi：按板子芯片筛选频段、填入当前网络与选中扫描结果", () => {
  assert.equal(boardSupports5g("esp32s3"), false);
  assert.equal(boardSupports5g("esp32c5"), true);
  assert.equal(boardSupports5g("ESP32-C5"), true);
  assert.equal(boardSupports5g(undefined), false);
  assert.equal(wifiBandLabel("2.4g"), "2.4 GHz");

  assert.deepEqual(boardWifiBands("esp32s3"), ["2.4g"]);
  assert.deepEqual(boardWifiBands("esp32c5"), ["2.4g", "5g"]);
  assert.deepEqual(boardWifiBands(""), ["2.4g"], "还没读板子按 2.4 GHz");

  // 双频同名网络：2.4G 信号 40，5G 信号 90
  const dual = {
    ssid: "Home",
    signal: 90,
    bands: ["2.4g", "5g"],
    band_signals: { "2.4g": 40, "5g": 90 },
    secure: true,
  };
  const only5 = { ssid: "Fast", signal: 80, bands: ["5g"], band_signals: { "5g": 80 }, secure: true, current: true };
  const only6 = { ssid: "Six", signal: 95, bands: ["6g"], band_signals: { "6g": 95 }, secure: true };
  const plain24 = { ssid: "Cafe", signal: 60, bands: ["2.4g"], band_signals: { "2.4g": 60 }, secure: false };
  const unknown = { ssid: "Mystery", signal: 30, bands: [], band_signals: {}, secure: null };
  const all = [only5, dual, only6, plain24, unknown];

  // 非 C5：只显示 2.4 GHz，双频网络只标 2.4 GHz、信号按 2.4G 算
  const s3 = filterScannedWifiForBoard(all, "esp32s3");
  assert.deepEqual(s3.shown.map((n) => n.ssid), ["Cafe", "Home", "Mystery"]);
  assert.equal(s3.hiddenCount, 2);
  const home = s3.shown.find((n) => n.ssid === "Home");
  assert.deepEqual(home.bands, ["2.4g"]);
  assert.equal(home.signal, 40);
  assert.deepEqual(s3.shown.find((n) => n.ssid === "Mystery").bands, [], "系统没给频率的网络保留");
  assert.deepEqual(filterScannedWifiForBoard(all, "").shown, s3.shown, "没读板子时与 2.4G 芯片一致");

  // C5：显示 2.4 / 5 GHz，电脑当前在前，6 GHz 隐藏
  const c5 = filterScannedWifiForBoard(all, "esp32c5");
  assert.deepEqual(c5.shown.map((n) => n.ssid), ["Fast", "Home", "Cafe", "Mystery"]);
  assert.equal(c5.hiddenCount, 1);
  assert.deepEqual(c5.shown.find((n) => n.ssid === "Home").bands, ["2.4g", "5g"]);
  assert.equal(c5.shown.find((n) => n.ssid === "Home").signal, 90);
  assert.deepEqual(filterScannedWifiForBoard(null, "esp32s3"), { shown: [], hiddenCount: 0 });

  assert.equal(scanFilterLabel("esp32s3", 2), "按板子 esp32s3 只显示 2.4 GHz，已隐藏 2 个板子连不上的网络");
  assert.equal(scanFilterLabel("esp32c5", 0), "按板子 esp32c5 只显示 2.4 GHz / 5 GHz");
  assert.match(scanFilterLabel("", 0), /还没读取板子，先只显示 2\.4 GHz/);

  assert.match(currentWifiFillNotice(only5, "esp32s3"), /电脑连的是 5 GHz，这块板子（esp32s3）只支持 2\.4 GHz/);
  assert.match(currentWifiFillNotice(only5, ""), /小智常见芯片只支持 2\.4 GHz/);
  assert.doesNotMatch(currentWifiFillNotice(only5, "esp32c5"), /只支持/);
  assert.match(currentWifiFillNotice(only6, "esp32c5"), /只支持 2\.4 GHz \/ 5 GHz/);
  assert.match(currentWifiFillNotice(plain24, "esp32s3"), /开放网络，可留空/);
  assert.equal(currentWifiFillNotice(null, "esp32s3"), "");

  const draft = { ...createDraftFromInspect(null, null), newSsid: "Old", newPassword: "old-pass" };
  const picked = applyPickedWifi(draft, dual);
  assert.equal(picked.newSsid, "Home");
  assert.equal(picked.newPassword, "", "换网络名清掉旧密码");
  const same = applyPickedWifi({ ...draft, newSsid: "Home" }, dual);
  assert.equal(same.newPassword, "old-pass", "同名网络保留已填密码");
  assert.equal(applyPickedWifi(draft, { ssid: "" }), draft);
});

test("本机 Wi-Fi 读不到时：缺定位权限跳定位页、Wi-Fi 关着跳 Wi-Fi 设置页", () => {
  assert.equal(hostWifiProblem({ permission: "granted", current: { ssid: "Home" } }, null), null);
  assert.equal(hostWifiProblem(null, { code: "wifi_unavailable", message: "没有无线网卡" }), null);
  const denied = hostWifiProblem({ permission: "denied", hint: "去定位服务里打开 MOSS" }, null);
  assert.equal(denied.target, "location");
  assert.equal(denied.permission, "denied");
  assert.equal(denied.message, "去定位服务里打开 MOSS");
  assert.match(denied.title, /定位权限/);
  assert.equal(hostWifiProblem({ permission: "not_determined" }, null).permission, "not_determined");
  assert.match(hostWifiProblem({ permission: "services_off" }, null).message, /MOSS 不读取、不保存位置/);
  const radio = hostWifiProblem(null, { code: "wifi_radio_off", message: "电脑的 Wi-Fi 是关着的" });
  assert.equal(radio.target, "wifi");
  assert.match(radio.title, /Wi-Fi 没打开/);
  assert.equal(canOpenWifiSettings("macos"), true);
  assert.equal(canOpenWifiSettings("windows"), true);
  assert.equal(canOpenWifiSettings("linux"), false);
});

test("读出的板子 OTA 地址不是 MOSS 当前地址时提醒，默认不替换", () => {
  const status = {
    ota_ip_url: "http://192.168.1.50:9666/xiaozhi/ota/",
    ota_url: "http://moss.local:9666/xiaozhi/ota/",
    ota_ip_urls: ["http://192.168.1.50:9666/xiaozhi/ota/", "http://10.0.0.8:9666/xiaozhi/ota/"],
  };
  const inspect = (ota) => ({ nvs: { ota_url: ota } });
  assert.deepEqual(mossOtaUrls(status), [
    "http://192.168.1.50:9666/xiaozhi/ota/",
    "http://moss.local:9666/xiaozhi/ota/",
    "http://192.168.1.50:9666/xiaozhi/ota/",
    "http://10.0.0.8:9666/xiaozhi/ota/",
  ]);
  // 电脑换了 IP：板子里还是旧地址
  const old = "http://192.168.1.23:9666/xiaozhi/ota/";
  assert.deepEqual(staleBoardOta(inspect(old), old, status), {
    boardUrl: old,
    mossUrl: "http://192.168.1.50:9666/xiaozhi/ota/",
  });
  // 改过端口也算旧地址
  const oldPort = "http://192.168.1.50:8003/xiaozhi/ota/";
  assert.ok(staleBoardOta(inspect(oldPort), oldPort, status));
  // 与 MOSS 任一当前地址一致（含另一块网卡、moss.local，大小写与末尾斜杠不计）不提醒
  for (const same of [
    "http://192.168.1.50:9666/xiaozhi/ota/",
    "http://10.0.0.8:9666/xiaozhi/ota/",
    "http://MOSS.local:9666/xiaozhi/ota",
  ]) {
    assert.equal(staleBoardOta(inspect(same), same, status), null, same);
  }
  // 用户已在输入框改掉、板子没存过、或服务没给出地址时不提醒
  assert.equal(staleBoardOta(inspect(old), "http://192.168.1.50:9666/xiaozhi/ota/", status), null);
  assert.equal(staleBoardOta(inspect(""), "", status), null);
  assert.equal(staleBoardOta(null, old, status), null);
  assert.equal(staleBoardOta(inspect(old), old, {}), null);
});
