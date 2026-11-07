import test from "node:test";
import assert from "node:assert/strict";
import { appUpdateActionLabel, appUpdateBusy, appUpdateProgressView, appUpdateAvailable, appUpdateScopeProgress } from "../src/appUpdate.js";

test("应用内更新进度文字与百分比", () => {
  assert.equal(appUpdateBusy({ phase: "downloading" }), true);
  assert.equal(appUpdateBusy({ phase: "installer_launched" }), false, "拉起 Android 系统安装器后停止轮询并允许再次点击");
  assert.equal(appUpdateBusy({ phase: "error" }), false, "失败后可重试");
  assert.equal(appUpdateBusy(null), false);
  assert.deepEqual(appUpdateProgressView({ phase: "downloading", downloaded: 5242880, total: 20971520 }), { text: "正在下载 5.0 / 20.0 MB", percent: 25 });
  assert.equal(appUpdateProgressView({ phase: "downloading", downloaded: 1048576, total: null }).percent, null, "服务器不给总大小时不显示假百分比");
  assert.match(appUpdateProgressView({ phase: "installing" }).text, /签名校验通过/);
  assert.match(appUpdateProgressView({ phase: "installer_launched" }).text, /系统安装界面/);
  assert.equal(appUpdateProgressView({ phase: "error", error: "设备正在加工" }).text, "设备正在加工");
  assert.equal(appUpdateProgressView({ phase: "idle" }).text, "");
});

test("桌面端与 Android 更新按钮文案按平台和阶段区分", () => {
  assert.equal(appUpdateActionLabel({ platform: "macos" }, null), "下载并重启更新");
  assert.equal(appUpdateActionLabel({ platform: "windows" }, { phase: "error" }), "重试下载并更新");
  assert.equal(appUpdateActionLabel({ platform: "android" }, null), "下载并安装更新");
  assert.equal(appUpdateActionLabel({ platform: "android" }, { phase: "error" }), "重试下载并安装");
  assert.equal(appUpdateActionLabel({ platform: "android" }, { phase: "installer_launched" }), "重新打开安装界面");
  assert.equal(appUpdateActionLabel({ platform: "android" }, { phase: "downloading" }), "正在更新…");
});

test("非客户端环境不提供应用内更新", () => {
  assert.equal(appUpdateAvailable(), false);
});

test("切换新版本时丢弃旧版本的非运行中进度状态，运行中保持锁定", () => {
  const launched = { phase: "installer_launched", version: "0.4.0" };
  assert.equal(appUpdateScopeProgress(launched, "0.4.0"), launched);
  assert.equal(appUpdateScopeProgress(launched, "v0.4.0"), launched);
  assert.equal(appUpdateScopeProgress(launched, "0.4.1-beta.1"), null, "切到新版本时不再继承旧版已打开安装器状态");
  const downloading = { phase: "downloading", version: "0.4.0", downloaded: 1024, total: 2048 };
  assert.equal(appUpdateScopeProgress(downloading, "0.4.1-beta.1"), downloading, "后台仍在下载时必须保留锁定状态");
});
