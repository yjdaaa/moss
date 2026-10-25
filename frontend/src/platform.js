/** 识别客户端系统类型，用于平台自适应路径示例、串口默认名与系统指引文案。 */
export function detectClientOs(nav = globalThis.navigator) {
  const ua = String(nav?.userAgent || "");
  const platform = String(nav?.userAgentData?.platform || nav?.platform || "");
  if (/Android/i.test(ua)) return "android";
  if (/iPhone|iPad|iPod/i.test(ua)) return "ios";
  if (/Mac/i.test(platform) || /Macintosh|Mac OS X/i.test(ua)) return "macos";
  if (/Win/i.test(platform) || /Windows/i.test(ua)) return "windows";
  if (/Linux/i.test(platform) || /Linux/i.test(ua)) return "linux";
  return "other";
}

export function defaultSerialPortHint(nav = globalThis.navigator) {
  const os = detectClientOs(nav);
  if (os === "macos") {
    return {
      prompt: "请输入串口名称 (例如 /dev/cu.usbserial-0001 或 /dev/cu.wchusbserial1410):",
      fallback: "/dev/cu.usbserial-0001",
    };
  }
  if (os === "linux") {
    return {
      prompt: "请输入串口名称 (例如 /dev/ttyUSB0 或 /dev/ttyACM0):",
      fallback: "/dev/ttyUSB0",
    };
  }
  return {
    prompt: "请输入串口名称 (例如 COM3 或 /dev/cu.usbserial-0001):",
    fallback: "COM3",
  };
}

export function trayActionCopy(nav = globalThis.navigator) {
  if (detectClientOs(nav) === "macos") {
    return {
      title: "隐藏到菜单栏图标",
      modalDetail: "窗口隐藏到顶部菜单栏，后台继续保持激光作业与网络服务（点 Dock 或菜单栏图标可重开）",
      settingsDetail: "直接隐藏窗口到顶部菜单栏，后台作业不中断，可点 Dock 重开或在菜单栏图标退出",
    };
  }
  return {
    title: "最小化到系统托盘",
    modalDetail: "窗口隐藏到任务栏托盘区，后台继续保持激光作业与网络服务",
    settingsDetail: "直接隐藏窗口到右下角托盘区，后台作业不中断，可在托盘右键退出",
  };
}

export function musicDirPlaceholder(nav = globalThis.navigator) {
  const os = detectClientOs(nav);
  if (os === "macos") return "例如 /Users/你的用户名/Music";
  if (os === "linux") return "例如 /home/你的用户名/Music";
  return "例如 D:\\Music";
}

export function phoneLinkHostCopy(nav = globalThis.navigator) {
  const os = detectClientOs(nav);
  if (os === "macos") {
    return {
      enableAlert: "已保存。请完全退出并重新打开 MOSS 后生效；首次启动时若 macOS 防火墙弹窗，请点击“允许传入连接”。",
      missingIpHint: "（未检测到局域网地址，请在“系统设置 → 网络”中查看本机 IPv4）",
    };
  }
  if (os === "linux") {
    return {
      enableAlert: "已保存。请完全退出并重新打开 MOSS 后生效；若系统开启了防火墙，请放行对应局域网端口。",
      missingIpHint: "（未检测到局域网地址，请在系统网络设置中查看本机 IPv4）",
    };
  }
  return {
    enableAlert: "已保存。请完全退出并重新打开 MOSS 后生效；首次启动时 Windows 防火墙若弹窗，请勾选“专用网络”并允许。",
    missingIpHint: "（未检测到局域网地址，请在 Windows 网络设置中查看本机 IPv4）",
  };
}

export function agentPlatformCopy(nav = globalThis.navigator) {
  const os = detectClientOs(nav);
  if (os === "macos") {
    return {
      sandboxSupported: true,
      workspacePlaceholder: "/Users/你的用户名/agent-workspace",
      codexLabel: "Codex 可执行文件（codex，留空自动读取本机）",
      codexPlaceholder: "留空自动读取本机安装的 Codex（换设备无需重填）",
      claudeLabel: "Claude Code 可执行文件（claude，留空自动读取本机）",
      claudePlaceholder: "留空自动读取本机安装的 Claude Code（换设备无需重填）",
    };
  }
  if (os === "linux") {
    return {
      sandboxSupported: true,
      workspacePlaceholder: "/home/你的用户名/agent-workspace",
      codexLabel: "Codex 可执行文件（codex，留空自动读取本机）",
      codexPlaceholder: "留空自动读取本机安装的 Codex（换设备无需重填）",
      claudeLabel: "Claude Code 可执行文件（claude，留空自动读取本机）",
      claudePlaceholder: "留空自动读取本机安装的 Claude Code（换设备无需重填）",
    };
  }
  return {
    sandboxSupported: true,
    workspacePlaceholder: "C:\\agent-workspace",
    codexLabel: "Codex 可执行文件（codex.exe，留空自动读取本机）",
    codexPlaceholder: "留空自动读取本机安装的 Codex（换设备无需重填）",
    claudeLabel: "Claude Code 可执行文件（claude.exe，留空自动读取本机）",
    claudePlaceholder: "留空自动读取本机安装的 Claude Code（换设备无需重填）",
  };
}
