/**
 * 顶栏“硬件”胶囊（2026-10-14）：显示的是最近一次握手探测的结果，不是实时状态。
 * 后端不持续轮询设备（轮询会占用串口），所以文字写“握手通过 / 未连接 / 未探测”，
 * 提示里写明探测时间。
 */
export function hardwarePillView(status) {
  if (!status?.checkedAt) {
    return { online: false, text: "硬件 · 未探测", title: "还没有做过握手探测；点击前往雕刻工作台探测" };
  }
  const time = new Date(status.checkedAt).toLocaleTimeString("zh-CN", { hour12: false });
  if (status.connected) {
    return {
      online: true,
      text: "硬件 · 握手通过",
      title: `${time} 握手探测通过。这是探测时的结果，不是实时状态；点击前往雕刻工作台重新探测`,
    };
  }
  return {
    online: false,
    text: "硬件 · 未连接",
    title: `${time} 探测：${status.summary || "未连接"}。点击前往雕刻工作台重新探测`,
  };
}
