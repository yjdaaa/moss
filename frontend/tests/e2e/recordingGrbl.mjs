import net from "node:net";

/**
 * 记录型 GRBL 模拟器（BTN-41 与界面 E2E 共用）：每行 ok 延迟 ackDelayMs，保证取消一定发生在发送中途；
 * 按到达顺序记录 G-code 行与实时字节，`!` 后状态报告 Hold:0，用于断言 ! → ? → 0x18 及软复位后不再下发刀路。
 */
export function startRecordingGrbl({ ackDelayMs = 3 } = {}) {
  const events = [];
  const sockets = new Set();
  const server = net.createServer((socket) => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    let buffer = "";
    let held = false;
    socket.on("data", (chunk) => {
      for (const b of chunk) {
        if (b === 0x18) { events.push("0x18"); held = false; socket.write("\r\nGrbl 1.1h ['$' for help]\r\n"); continue; }
        if (b === 0x21) { events.push("!"); held = true; continue; }
        if (b === 0x3f) {
          events.push("?");
          socket.write(held ? "<Hold:0|MPos:0.000,0.000,0.000|FS:0,0>\r\n" : "<Idle|MPos:0.000,0.000,0.000|FS:0,0>\r\n");
          continue;
        }
        if (b === 0x7e || b === 0x85) continue;
        buffer += String.fromCharCode(b);
      }
      let idx;
      while ((idx = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, idx).replace(/\r$/, "").trim();
        buffer = buffer.slice(idx + 1);
        if (line === "$$") socket.write("$0=10\r\n$30=1000\r\n$32=1\r\nok\r\n");
        else if (line === "$CD") socket.write("name: \"E2E Rec\"\r\nok\r\n");
        else if (/^(?:[GMTS]\d+)(?:\s|$)/i.test(line) || line.startsWith("$")) {
          if (/^[GMTS]\d+/i.test(line)) events.push("line");
          setTimeout(() => { if (!socket.destroyed) socket.write("ok\r\n"); }, ackDelayMs);
        } else if (line.length > 0) socket.write("error:3\r\n");
      }
    });
    socket.on("error", () => {});
  });
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    // 后端取消后可能保留设备连接供急停重试；收尾时主动断开，否则测试进程无法退出
    const close = () => {
      for (const socket of sockets) socket.destroy();
      server.close();
    };
    server.listen(0, "127.0.0.1", () => resolve({ close, events, port: server.address().port }));
  });
}
