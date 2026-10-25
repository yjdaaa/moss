import test from "node:test";
import assert from "node:assert/strict";
import {
  agentPlatformCopy,
  defaultSerialPortHint,
  detectClientOs,
  musicDirPlaceholder,
  phoneLinkHostCopy,
  trayActionCopy,
} from "../src/platform.js";

test("detectClientOs distinguishes macOS, Windows, Linux and Android", () => {
  assert.equal(detectClientOs({ platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)" }), "macos");
  assert.equal(detectClientOs({ platform: "Win32", userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" }), "windows");
  assert.equal(detectClientOs({ platform: "Linux x86_64", userAgent: "Mozilla/5.0 (X11; Linux x86_64)" }), "linux");
  assert.equal(detectClientOs({ platform: "Linux armv8l", userAgent: "Mozilla/5.0 (Linux; Android 14)" }), "android");
});

test("platform helpers adapt serial, tray, music, phone link and agent copy for macOS vs Windows", () => {
  const mac = { platform: "MacIntel", userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)" };
  const win = { platform: "Win32", userAgent: "Mozilla/5.0 (Windows NT 10.0)" };

  assert.equal(defaultSerialPortHint(mac).fallback, "/dev/cu.usbserial-0001");
  assert.equal(defaultSerialPortHint(win).fallback, "COM3");
  assert.equal(trayActionCopy(mac).title, "隐藏到菜单栏图标");
  assert.equal(trayActionCopy(win).title, "最小化到系统托盘");
  assert.match(musicDirPlaceholder(mac), /\/Users\//);
  assert.equal(musicDirPlaceholder(win), "例如 D:\\Music");
  assert.match(phoneLinkHostCopy(mac).enableAlert, /macOS 防火墙/);
  assert.match(phoneLinkHostCopy(win).enableAlert, /Windows 防火墙/);
  assert.equal(agentPlatformCopy(mac).sandboxSupported, true);
  assert.equal(agentPlatformCopy(win).sandboxSupported, true);
  assert.match(agentPlatformCopy(mac).codexLabel, /codex，留空自动读取本机/);
  assert.match(agentPlatformCopy(win).codexLabel, /codex\.exe，留空自动读取本机/);
  assert.match(agentPlatformCopy(mac).codexPlaceholder, /留空自动读取本机/);
  assert.match(agentPlatformCopy(win).codexPlaceholder, /留空自动读取本机/);
});
