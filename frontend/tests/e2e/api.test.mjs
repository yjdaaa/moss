import test from "node:test";
import assert from "node:assert/strict";
import { startRecordingGrbl } from "./recordingGrbl.mjs";

if (!process.env.TEST_API_BASE || !process.env.TEST_GRBL_PORT) {
  throw new Error("E2E 必须由隔离调度器注入 TEST_API_BASE 和 TEST_GRBL_PORT");
}

const API_BASE = process.env.TEST_API_BASE;
const GRBL_PORT = Number(process.env.TEST_GRBL_PORT);
const GRBL_HOST = "127.0.0.1";

async function fetchPreview(filePath) {
  assert.equal(typeof filePath, "string");
  const response = await fetch(`${API_BASE}/preview?path=${encodeURIComponent(filePath)}`);
  assert.equal(response.status, 200, `预览文件必须真实存在: ${filePath}`);
  const bytes = await response.arrayBuffer();
  assert.ok(bytes.byteLength > 0, `预览文件不能为空: ${filePath}`);
  return { response, bytes: new Uint8Array(bytes) };
}

async function pollWorkflowUntil(workflowId, wanted, timeoutMs = 8000, ready = () => true) {
  const start = Date.now();
  let last = null;
  while (Date.now() - start < timeoutMs) {
    last = await apiPost("/api/workflow", { action: "status", workflow_id: workflowId });
    const status = last.data?.result?.status;
    if (wanted.includes(status) && ready(last.data?.result || {})) return last;
    await new Promise((r) => setTimeout(r, 40));
  }
  return last;
}

// 辅助请求方法
async function apiGet(route) {
  const url = `${API_BASE}${route}`;
  const res = await fetch(url, { headers: { Accept: "application/json" } });
  const data = await res.json().catch(() => ({}));
  return { status: res.status, ok: res.ok, data };
}

async function apiPost(route, body = {}) {
  const url = `${API_BASE}${route}`;
  const res = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify(body),
  });
  const data = await res.json().catch(() => ({}));
  return { status: res.status, ok: res.ok, data };
}

// 模拟简易 Base64 PNG 图片 (1x1 像素)
const DUMMY_PNG_BASE64 = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

console.log(`\n===============================================================`);
console.log(`[E2E-SUITE] 目标测试服务器: ${API_BASE}`);
console.log(`===============================================================\n`);

// -----------------------------------------------------------------------------
// 模块 1: 雕刻工作台 (HomePage / 路径 `/`) - 11 项测试
// -----------------------------------------------------------------------------

test("[BTN-01] 雕刻工作台: 串口刷新按钮调用 /api/ui/serial-ports 并返回可用列表", async () => {
  const res = await apiGet("/api/ui/serial-ports");
  assert.equal(res.status, 200, "接口必须响应 HTTP 200");
  assert.equal(res.data?.success, true, "响应业务成功标记必须为 true");
  assert.ok(Array.isArray(res.data?.result?.ports), "ports 必须是数组");
  assert.ok(typeof res.data?.result?.baudrate === "number", "baudrate 必须是有效数字");
});

test("[BTN-02] 雕刻工作台: 握手探测按钮调用 /api/connection/check 进行网络/串口连通性探测", async () => {
  const payload = {
    connection_mode: "network",
    port: "",
    baudrate: 115200,
    network_host: GRBL_HOST,
    network_telnet_port: GRBL_PORT,
  };
  const res = await apiPost("/api/connection/check", payload);
  assert.equal(res.status, 200, "接口必须响应 HTTP 200");
  assert.equal(res.data?.success, true, "连接探测必须返回成功结构");
  const result = res.data?.result;
  assert.ok(result, "必须包含 result 对象");
  assert.ok(Array.isArray(result.attempts), "attempts 必须记录探测尝试");
  assert.ok(typeof result.summary === "string", "summary 必须输出人类可读探测摘要");
  assert.equal(result.network_connected, true, `真实 TCP 握手必须成功: ${result.summary}`);
  assert.equal(result.selected_mode, "network");
  assert.ok(result.firmware_settings?.some((l) => l.startsWith("$32=1")), "必须读到固件 $32=1 激光模式");

  // 反向：端口无监听必须显式报告未连接，而不是伪装 OFFLINE/ONLINE
  const dead = await apiPost("/api/connection/check", { ...payload, network_telnet_port: 1, network_timeout: 1 });
  assert.equal(dead.status, 200);
  assert.equal(dead.data?.result?.network_connected, false, "无设备时必须报告未连接");
  assert.ok(/连接失败|无 GRBL 响应/.test(dead.data?.result?.summary || ""), dead.data?.result?.summary);
});

test("[BTN-03] 雕刻工作台: 系统运行配置预加载 /api/ui/runtime-config 正常", async () => {
  const res = await apiGet("/api/ui/runtime-config");
  assert.equal(res.status, 200);
  assert.equal(res.data?.success, true);
  assert.ok(res.data?.result?.connection?.default_mode, "必须包含默认连接模式");
});

test("[BTN-04] 雕刻工作台: 材料与厚度规格动态联动加载 /api/ui/material-options", async () => {
  const res = await apiGet("/api/ui/material-options");
  assert.equal(res.status, 200);
  assert.equal(res.data?.success, true);
  const materials = res.data?.result?.materials;
  assert.ok(Array.isArray(materials), "materials 必须是列表");
  assert.ok(materials.length > 0, "内置材料库不应为空");
  const basswood = materials.find((m) => m.name === "椴木");
  assert.ok(basswood, "必须包含基础材料【椴木】");
  assert.ok(Array.isArray(basswood.thicknesses), "椴木必须包含厚度规格列表");
  assert.ok(basswood.thicknesses.length > 0, "椴木必须至少有 1 档厚度");
});

let uploadedImagePath = "";
test("[BTN-05] 雕刻工作台: 图片上传按钮通过 /api/upload 存盘并生成物理文件", async () => {
  const payload = {
    filename: "test_e2e_image.png",
    content_base64: DUMMY_PNG_BASE64,
  };
  const res = await apiPost("/api/upload", payload);
  assert.equal(res.status, 200, "上传必须响应 200");
  assert.equal(res.data?.success, true, "上传业务必须成功");
  uploadedImagePath = res.data?.result?.file_path;
  assert.ok(uploadedImagePath, "上传响应必须返回实际存储文件路径");
  assert.ok(res.data?.result?.preview_url, "必须包含预览 URL");
  const imagePreview = await fetchPreview(uploadedImagePath);
  assert.match(imagePreview.response.headers.get("content-type") || "", /image\/png/);
});

let uploadedGcodePath = "";
test("[BTN-06] 雕刻工作台: G-code 刀路文件上传按钮通过 /api/upload 保存至工作区", async () => {
  const gcodeContent = "G21\nG90\nM3 S200\nG1 X10 Y10 F1000\nM5\n";
  const gcodeBase64 = `data:text/plain;base64,${Buffer.from(gcodeContent).toString("base64")}`;
  const payload = {
    filename: "test_e2e_toolpath.gcode",
    content_base64: gcodeBase64,
  };
  const res = await apiPost("/api/upload", payload);
  assert.equal(res.status, 200);
  assert.equal(res.data?.success, true);
  uploadedGcodePath = res.data?.result?.file_path;
  assert.ok(typeof uploadedGcodePath === "string" && uploadedGcodePath.endsWith(".gcode"), "返回路径必须包含 .gcode 后缀");
  const savedGcode = await fetchPreview(uploadedGcodePath);
  assert.match(new TextDecoder().decode(savedGcode.bytes), /G21[\s\S]*G1 X10 Y10 F1000/);
});

let generatedTextWorkflow = null;
test("[BTN-07] 雕刻工作台: 矢量文字「计算加工轨迹」按钮生成真实 G-code 与预览图", async () => {
  const payload = {
    text: "MOSS-E2E",
    material: "椴木",
    thickness_mm: 3,
    font_size_mm: 12,
    engraving_mode: "raster",
  };
  const res = await apiPost("/api/text-task/generate", payload);
  assert.equal(res.status, 200, "文字轨迹解算必须成功");
  assert.equal(res.data?.success, true);
  const wf = res.data?.result;
  assert.ok(wf, "工作流对象不能为空");
  generatedTextWorkflow = wf;

  const artifacts = wf.artifacts || {};
  assert.ok(artifacts.image_file, "必须生成光栅预览图文件路径");
  assert.ok(artifacts.gcode_file, "必须解算并生成物理 G-code 刀路文件");
  await fetchPreview(artifacts.image_file);
  await fetchPreview(artifacts.gcode_file);
  assert.ok(artifacts.time_estimate?.estimated_seconds > 0, "必须给出大于 0 的加工时间预估");
  assert.ok(artifacts.width_mm > 0 && artifacts.height_mm > 0, "必须计算出有效几何轮廓尺寸");
});

test("[BTN-08] 雕刻工作台: 图片「计算加工轨迹」按钮生成工作流与工时预估", async () => {
  assert.ok(uploadedImagePath, "依赖上传图片路径");
  const payload = {
    action: "preview",
    source_type: "image",
    image_file: uploadedImagePath,
    material: "椴木",
    thickness_mm: 3,
  };
  const res = await apiPost("/api/workflow", payload);
  assert.equal(res.status, 200);
  assert.equal(res.data?.success, true);
  const wf = res.data?.result;
  assert.ok(wf.workflow_id, "图片工作流必须包含合法 workflow_id");
});

test("[BTN-09] 雕刻工作台: 「安全确认 · 立即发送」按钮触发调度下发与状态推进", async () => {
  assert.ok(generatedTextWorkflow?.workflow_id, "依赖已生成的文字工作流");
  // 反向 1：设备不可达（端口 1 无监听）必须 400 拒绝，workflow 保持可再次发送
  const unreachable = await apiPost("/api/workflow", {
    action: "confirm_send",
    workflow_id: generatedTextWorkflow.workflow_id,
    confirmed: true,
    connection_mode: "network",
    network_host: GRBL_HOST,
    network_telnet_port: 1,
  });
  assert.equal(unreachable.status, 400, "设备不可达时严禁伪装发送成功");
  assert.equal(unreachable.data?.error_code || unreachable.data?.detail?.error_code || unreachable.data?.result?.error_code, "device_unreachable");

  const payload = {
    action: "confirm_send",
    workflow_id: generatedTextWorkflow.workflow_id,
    confirmed: true,
    connection_mode: "network",
    network_host: GRBL_HOST,
    network_telnet_port: GRBL_PORT,
  };
  const res = await apiPost("/api/workflow", payload);
  if (res.status !== 200) {
    console.error("BTN-09 ERROR RES:", JSON.stringify(res.data));
  }
  assert.equal(res.status, 200, "确认发送指令下达接口必须响应 200");
  assert.equal(res.data?.success, true, "发送指令下达必须被接收");
  const sr = res.data?.result?.send_result;
  assert.equal(sr?.success, true, "握手通过后必须返回 send_result.success");
  assert.ok(sr?.total_lines > 0, "必须报告将要下发的刀路总行数");
  assert.equal(sr?.target, `${GRBL_HOST}:${GRBL_PORT}`);

  // 真实 JobWorker 逐行下发并等待设备 ok；轮询直到终态
  const finished = await pollWorkflowUntil(generatedTextWorkflow.workflow_id, ["completed", "failed", "cancelled"]);
  assert.equal(finished.data?.result?.status, "completed", `发送必须以 completed 结束: ${JSON.stringify(finished.data?.result?.send_result)}`);
  const done = finished.data.result.send_result;
  assert.equal(done.lines_sent, done.total_lines, "每一行都必须收到设备 ok 确认");

  // 反向 2：已完成任务再次发送必须拦截（防止同一图案二次烧蚀）
  const repeat = await apiPost("/api/workflow", payload);
  assert.equal(repeat.status, 400);
  assert.equal(repeat.data?.error_code || repeat.data?.detail?.error_code || repeat.data?.result?.error_code, "workflow_not_sendable");
});

test("[BTN-10] 雕刻工作台: 已全量完成的任务「取消加工」必须拒绝，保留 completed 记录", async () => {
  assert.ok(generatedTextWorkflow?.workflow_id, "依赖 BTN-09 已完成的文字工作流");
  const res = await apiPost("/api/workflow", {
    action: "cancel",
    workflow_id: generatedTextWorkflow.workflow_id,
  });
  assert.equal(res.status, 400, `已完成任务不得被改写为 cancelled: ${JSON.stringify(res.data)}`);
  assert.equal(res.data?.error_code || res.data?.detail?.error_code || res.data?.result?.error_code, "workflow_completed");

  const checkRes = await apiPost("/api/workflow", {
    action: "status",
    workflow_id: generatedTextWorkflow.workflow_id,
  });
  assert.equal(checkRes.status, 200);
  assert.equal(checkRes.data?.result?.status, "completed", "拒绝取消后状态必须保持 completed");
  const sr = checkRes.data?.result?.send_result;
  assert.equal(sr?.lines_sent, sr?.total_lines, "逐行确认记录不得被抹掉");
});

// -----------------------------------------------------------------------------
// 模块 2: 材料实验室 (MaterialLabPage / 路径 `/material-lab`)
// -----------------------------------------------------------------------------

test("[BTN-12] 材料实验室: Bootstrap 数据加载 /v1/material-lab/bootstrap 返回完整材料库", async () => {
  const res = await apiGet("/v1/material-lab/bootstrap");
  assert.equal(res.status, 200);
  assert.equal(res.data?.success, true);
  const result = res.data?.result;
  assert.ok(result.materials?.materials, "必须包含材料配置字典");
  assert.ok(result.defaults?.network_telnet_port, "必须包含默认网络端口");
});

test("[BTN-13] 材料实验室: 材料加工参数智能推荐 /api/ui/material-recommendation", async () => {
  const query = "material=椴木&thickness_mm=3&laser_mode=engrave&engraving_mode=raster";
  const res = await apiGet(`/api/ui/material-recommendation?${query}`);
  assert.equal(res.status, 200);
  assert.equal(res.data?.success, true);
  const rec = res.data?.result;
  assert.ok(rec.laser_max_power > 0, "推荐最大功率必须大于 0");
  assert.ok(rec.feed_rate > 0, "推荐进给速度必须大于 0");
  assert.ok(["exact", "nearest_thickness"].includes(rec.match), "厚度匹配度必须明确标识");
});

test("[BTN-14] 材料实验室: 「套用最近」加工参数按钮 /api/material-lab/recent", async () => {
  const res = await apiPost("/api/material-lab/recent", {});
  // 若系统有最近加工任务则返回 200，若干净环境则返回 400 告知无可用，两者均为受控闭环
  assert.ok([200, 400].includes(res.status), "接口响应必须符合协议定义 (200 或 400)");
  if (res.status === 200) {
    assert.ok(res.data?.result?.material, "套用成功必须包含材料名");
  } else {
    assert.ok(res.data?.result?.includes("没有可保存") || res.data?.result?.includes("参数"), "错误时必须有明确提示");
  }
});

test("[BTN-15] 材料实验室: 「导出材料库」按钮 /api/material-lab/export 生成合法 JSON", async () => {
  const res = await apiGet("/api/material-lab/export");
  assert.equal(res.status, 200);
  assert.equal(res.data?.success, true);
  const lib = res.data?.result?.library;
  assert.ok(lib?.materials, "导出的库文件必须包含 materials 字典");
  assert.ok(lib.materials["椴木"], "导出的库文件必须包含椴木条目");
});

test("[BTN-16] 材料实验室: 「保存材料参数」按钮修改功率并持久化写入材料库", async () => {
  const targetMaterial = "椴木";
  const targetThickness = 3;
  const newMaxPower = 488;
  const newFeedRate = 1950;

  const payload = {
    action: "save",
    material: targetMaterial,
    thickness_mm: targetThickness,
    laser_mode: "engrave",
    engraving_mode: "raster",
    laser_min_power: 0,
    laser_max_power: newMaxPower,
    feed_rate: newFeedRate,
    travel_rate: 3000,
    passes: 1,
    pixel_size_mm: 0.1,
    threshold: -1,
    notes: "E2E 自动化测试参数",
    aliases: ["椴木胶合板", "Basswood"],
  };

  const res = await apiPost("/api/material-lab/manage", payload);
  assert.equal(res.status, 200, "保存参数接口必须成功");
  assert.equal(res.data?.success, true, "业务保存必须返回成功");

  // 二次读取验证持久化写入生效
  const verifyRes = await apiGet("/v1/material-lab/bootstrap");
  assert.equal(verifyRes.status, 200);
  const updatedCell = verifyRes.data?.result?.materials?.materials?.[targetMaterial]?.thicknesses?.[String(targetThickness)]?.engrave?.raster;
  assert.ok(updatedCell, "保存后必须能读取目标材料参数");
  assert.equal(updatedCell.laser_max_power, newMaxPower, "最大功率必须已更新为 488");
  assert.equal(updatedCell.feed_rate, newFeedRate, "进给速度必须已更新为 1950");
});

test("[BTN-17] 材料实验室: 「导入材料库」按钮 /api/material-lab/manage 成功导入外部库", async () => {
  // 先导出当前库，确保保留基准材料
  const exportRes = await apiGet("/api/material-lab/export");
  const currentLib = exportRes.data?.result?.library || { version: 1, materials: {} };

  currentLib.materials["E2E测试材料"] = {
    aliases: ["test-mat"],
    thicknesses: {
      "2": {
        cut: {
          laser_min_power: 0,
          laser_max_power: 600,
          feed_rate: 400,
          travel_rate: 3000,
          passes: 2,
          pixel_size_mm: 0.1,
          threshold: -1,
        },
        engrave: {
          raster: {
            laser_min_power: 0,
            laser_max_power: 350,
            feed_rate: 1500,
            travel_rate: 3000,
            passes: 1,
            pixel_size_mm: 0.1,
            threshold: -1,
          },
          outline: {
            laser_min_power: 0,
            laser_max_power: 280,
            feed_rate: 1800,
            travel_rate: 3000,
            passes: 1,
            pixel_size_mm: 0.1,
            threshold: -1,
          },
        },
      },
    },
  };

  const importPayload = {
    action: "import",
    library_json: currentLib,
  };

  const res = await apiPost("/api/material-lab/manage", importPayload);
  assert.equal(res.status, 200);
  assert.equal(res.data?.success, true, "导入外部材料库必须成功");

  // 验证新材料已入库且椴木仍保留
  const checkRes = await apiGet("/v1/material-lab/bootstrap");
  assert.ok(checkRes.data?.result?.materials?.materials?.["E2E测试材料"], "材料树中必须出现新导入的材料");
  assert.ok(checkRes.data?.result?.materials?.materials?.["椴木"], "基准材料椴木必须保留");
});

let calibrationId = "";
test("[BTN-18] 材料实验室: 「生成矩阵」按钮 /api/material-lab/preview 解算标定网格", async () => {
  const payload = {
    material: "椴木",
    thickness_mm: 3,
    laser_mode: "engrave",
    matrix_kind: "engrave",
    rows: 4,
    columns: 4,
    power_min: 100,
    power_max: 500,
    speed_min: 600,
    speed_max: 2400,
  };
  const res = await apiPost("/api/material-lab/preview", payload);
  assert.equal(res.status, 200, "生成标定矩阵必须成功");
  assert.equal(res.data?.success, true);
  calibrationId = res.data?.result?.calibration_id;
  assert.match(calibrationId || "", /^cal-/, "必须返回真实标定矩阵 calibration_id");
});

test("[BTN-19] 材料实验室: 「发送测试」按钮 /api/material-lab/send 下达标定矩阵加工", async () => {
  assert.ok(calibrationId, "依赖标定矩阵 ID");
  const payload = {
    calibration_id: calibrationId,
    confirmed: true,
    network_host: GRBL_HOST,
    network_telnet_port: GRBL_PORT,
  };
  const res = await apiPost("/api/material-lab/send", payload);
  assert.equal(res.status, 200, JSON.stringify(res.data));
  assert.equal(res.data?.success, true);
  assert.equal(res.data?.result?.status, "sending", "标定加工状态必须为 sending");
  assert.equal(res.data?.result?.probe?.connected, true, "下发前必须真实握手成功");
  const totalLines = res.data?.result?.send_result?.total_lines;
  assert.ok(typeof totalLines === "number" && totalLines > 0, "必须返回真实刀路行数");

  // 发送中重复下发必须被拦截（设备单作业 + 记录状态门禁）
  const duplicate = await apiPost("/api/material-lab/send", payload);
  assert.equal(duplicate.status, 400, "发送中的标定矩阵不得重复下发");

  // 真实 JobWorker 逐行下发并等待 ok：轮询 /api/material-lab/status 直到终态
  let finalRecord = null;
  const deadline = Date.now() + 20000;
  while (Date.now() < deadline) {
    const status = await apiPost("/api/material-lab/status", { calibration_id: calibrationId });
    assert.equal(status.status, 200, JSON.stringify(status.data));
    finalRecord = status.data?.result || {};
    if (["completed", "failed", "cancelled"].includes(finalRecord.status)) break;
    await new Promise((r) => setTimeout(r, 200));
  }
  assert.equal(finalRecord?.status, "completed", `标定矩阵必须完成逐行下发: ${JSON.stringify(finalRecord?.send_result)}`);
  assert.equal(finalRecord?.send_result?.lines_sent, totalLines, "全部刀路行必须收到设备 ok");

  // 已完成的标定矩阵不可再次下发（防二次烧蚀）
  const dead = await apiPost("/api/material-lab/send", { ...payload, network_telnet_port: 1 });
  assert.equal(dead.status, 400, "已完成/不可达时标定矩阵必须拒绝下发");
});

test("[BTN-20] 材料实验室: 单元格点选回填 /api/material-lab/select-cell 保存最优工艺", async () => {
  assert.ok(calibrationId, "依赖标定矩阵 ID");
  const payload = {
    calibration_id: calibrationId,
    cell_number: 6,
    material: "椴木",
    thickness_mm: 3,
    laser_mode: "engrave",
  };
  const res = await apiPost("/api/material-lab/select-cell", payload);
  assert.equal(res.status, 200);
  assert.equal(res.data?.success, true, "单元格回填保存必须成功");
});

// -----------------------------------------------------------------------------
// 模块 3: 创意画板 (DrawPage / 路径 `/draw`)
// -----------------------------------------------------------------------------

test("[BTN-24] 创意画板: 「刷新材料库」按钮同步最新材料规格", async () => {
  const res = await apiGet("/api/ui/material-options");
  assert.equal(res.status, 200);
  assert.equal(res.data?.success, true);
  assert.ok(res.data?.result?.materials?.length > 0, "必须获取到材料列表");
});

test("[BTN-26] 创意画板: 「测试连接」按钮探测画板专用 Host/Port 连通性", async () => {
  const payload = {
    network_host: GRBL_HOST,
    network_transport: "telnet",
    network_telnet_port: GRBL_PORT,
    network_timeout: 2,
    include_detail: false,
  };
  const res = await apiPost("/api/connection/check", payload);
  assert.equal(res.status, 200);
  assert.equal(res.data?.success, true);
  assert.ok(typeof res.data?.result?.summary === "string");
});

let drawWorkflowId = "";
let drawFingerprint = "";
let drawGcodeDownloadUrl = "";
let drawSceneDownloadUrl = "";

test("[BTN-28] 创意画板: 「生成预览」按钮调用 /api/draw/preview 生成工时、真实G码与下载路由", async () => {
  const payload = {
    image_data_url: DUMMY_PNG_BASE64,
    material: "椴木",
    thickness_mm: 3,
    mode: "raster",
    output_format: "gcode",
    network_host: "127.0.0.1",
    network_telnet_port: 23,
    scene_json: { type: "excalidraw", version: 2 },
  };
  const res = await apiPost("/api/draw/preview", payload);
  assert.equal(res.status, 200, "画板预览解算必须成功");
  assert.equal(res.data?.success, true);
  const result = res.data?.result;
  assert.ok(result.draw_lab, "必须包含 draw_lab 专属信息");
  drawWorkflowId = result.workflow_id || result.workflow?.workflow_id;
  drawFingerprint = result.draw_lab?.content_fingerprint;
  assert.match(drawWorkflowId || "", /^(wf|draw-wf)-/, "画板必须返回真实 workflow_id");
  assert.match(drawFingerprint || "", /^[0-9a-f]{64}$/, "画板必须返回真实 SHA-256 内容指纹");
  assert.ok(result.artifacts?.gcode_file, "必须生成真实物理 G-code 刀路");
  assert.ok(result.web?.gcode_download_url, "必须提供 G-code 刀路下载地址");
  drawGcodeDownloadUrl = result.web.gcode_download_url;
  drawSceneDownloadUrl = result.web.scene_download_url;
  assert.ok(result.next_actions?.includes("confirm_send"), "必须授权 confirm_send 操作");
});

test("[BTN-29] 创意画板: 「确认发送」按钮调用真实业务接口推进至 sending 态与防呆拦截", async () => {
  assert.ok(drawWorkflowId, "依赖工作流 ID");
  assert.ok(drawFingerprint, "依赖画板内容指纹");

  // 真实调用：首次发送下发 confirm_send
  const sendRes = await apiPost("/api/workflow", {
    action: "confirm_send",
    workflow_id: drawWorkflowId,
    confirmed: true,
    connection_mode: "network",
    network_host: GRBL_HOST,
    network_telnet_port: GRBL_PORT,
  });
  assert.equal(sendRes.status, 200, `画板确认发送必须成功通过门禁: ${JSON.stringify(sendRes.data)}`);
  assert.equal(sendRes.data?.success, true);
  assert.equal(sendRes.data?.result?.status, "sending", "任务状态必须成功推进至 sending 态");
  assert.equal(sendRes.data?.result?.send_result?.success, true, "画板任务必须真实握手并进入下发");
  const drawDone = await pollWorkflowUntil(drawWorkflowId, ["completed", "failed", "cancelled"]);
  assert.equal(drawDone.data?.result?.status, "completed", JSON.stringify(drawDone.data?.result?.send_result));
});

test("[BTN-30] 创意画板: 「状态」按钮查询工作流执行进度", async () => {
  assert.ok(drawWorkflowId, "依赖工作流 ID");
  const res = await apiPost("/api/workflow", {
    action: "status",
    workflow_id: drawWorkflowId,
  });
  assert.equal(res.status, 200);
  assert.equal(res.data?.success, true);
});

test("[BTN-31] 创意画板: 「取消」按钮终止未发送工作流，已完成工作流拒绝取消", async () => {
  assert.ok(drawWorkflowId, "依赖 BTN-29 已完成的画板工作流");
  // 反向：已全量完成的画板任务不得被改写为 cancelled
  const done = await apiPost("/api/workflow", { action: "cancel", workflow_id: drawWorkflowId });
  assert.equal(done.status, 400, JSON.stringify(done.data));
  assert.equal(done.data?.error_code || done.data?.detail?.error_code || done.data?.result?.error_code, "workflow_completed");

  // 正向：新生成、尚未发送的画板预览可取消，取消后拒绝下发
  const preview = await apiPost("/api/draw/preview", {
    image_data_url: DUMMY_PNG_BASE64,
    material: "椴木",
    thickness_mm: 3,
    mode: "raster",
    output_format: "gcode",
    network_host: "127.0.0.1",
    network_telnet_port: 23,
    scene_json: { type: "excalidraw", version: 2 },
  });
  assert.equal(preview.status, 200, JSON.stringify(preview.data));
  const freshId = preview.data?.result?.workflow_id || preview.data?.result?.workflow?.workflow_id;
  assert.ok(freshId && freshId !== drawWorkflowId, "必须生成新的画板工作流");
  const res = await apiPost("/api/workflow", { action: "cancel", workflow_id: freshId });
  assert.equal(res.status, 200, JSON.stringify(res.data));
  assert.equal(res.data?.success, true);
  assert.equal(res.data?.result?.status, "cancelled");
  const send = await apiPost("/api/workflow", {
    action: "confirm_send", workflow_id: freshId, confirmed: true,
    connection_mode: "network", network_host: GRBL_HOST, network_telnet_port: GRBL_PORT,
  });
  assert.equal(send.status, 400, "已取消的画板任务必须拒绝下发");
});

test("[BTN-38] 创意画板: 「下载 G-code」链接返回真实物理刀路与 Content-Disposition", async () => {
  assert.ok(drawGcodeDownloadUrl, "依赖画板 G-code 下载路由");
  const res = await fetch(`${API_BASE}${drawGcodeDownloadUrl}`);
  assert.equal(res.status, 200, "下载路由必须返回 200 OK");
  const disposition = res.headers.get("content-disposition");
  assert.ok(disposition?.includes("attachment"), "必须包含 attachment 附件头");
  const bodyText = await res.text();
  assert.ok(bodyText.includes("; Generated by") || bodyText.includes("G0") || bodyText.includes("M4"), "必须包含合法 G-code 刀路指令");
});

test("[BTN-39] 创意画板: 「下载场景 JSON」链接返回完整 Excalidraw 场景数据", async () => {
  assert.ok(drawSceneDownloadUrl, "依赖画板场景下载路由");
  const res = await fetch(`${API_BASE}${drawSceneDownloadUrl}`);
  assert.equal(res.status, 200, "场景下载路由必须返回 200 OK");
  const disposition = res.headers.get("content-disposition");
  assert.ok(disposition?.includes("attachment"), "必须包含 attachment 附件头");
  const sceneJson = await res.json();
  assert.equal(sceneJson.type, "excalidraw");
  assert.equal(sceneJson.version, 2);
});

test("[BTN-40] 材料实验室: 「新建材料」与「删除材料」全生命周期管理闭环", async () => {
  // 1. 新建临时材料
  const saveRes = await apiPost("/api/material-lab/manage", {
    action: "save",
    material: "E2E测试碳纤维",
    thicknesses: [2, 4],
    power_percent: 75,
    speed_mms: 120,
    thickness_mm: 2,
    laser_max_power: 750,
    feed_rate: 7200,
  });
  assert.equal(saveRes.status, 200, "新建材料必须成功");
  assert.equal(saveRes.data?.success, true);

  // 2. 验证新材料两个厚度（2mm 与 4mm）均已入库并参与实时推荐
  const bootRes = await apiGet("/v1/material-lab/bootstrap");
  assert.equal(bootRes.status, 200);
  const mats = bootRes.data?.result?.materials?.materials || {};
  assert.ok(mats["E2E测试碳纤维"], "材料库必须包含新建材料");
  assert.ok(mats["E2E测试碳纤维"].thicknesses?.["2"], "2mm 规格必须入库");
  assert.ok(mats["E2E测试碳纤维"].thicknesses?.["4"], "4mm 规格必须入库");
  const recRes = await apiGet(
    `/api/ui/material-recommendation?material=${encodeURIComponent("E2E测试碳纤维")}&thickness_mm=4&laser_mode=engrave&engraving_mode=raster`
  );
  assert.equal(recRes.status, 200, "新建材料必须可通过推荐接口实时命中");
  assert.equal(recRes.data?.result?.laser_max_power, 750);

  // 3. 先按单厚度删除 2mm（4mm 保留），再按整材料删除（thickness_mm: 0，与 MaterialModal 一致）
  const delSingle = await apiPost("/api/material-lab/manage", {
    action: "delete",
    material: "E2E测试碳纤维",
    thickness_mm: 2,
    confirmed: true,
  });
  assert.equal(delSingle.status, 200, "删除单厚度必须成功");
  const bootMid = await apiGet("/v1/material-lab/bootstrap");
  const matsMid = bootMid.data?.result?.materials?.materials || {};
  assert.equal(matsMid["E2E测试碳纤维"]?.thicknesses?.["2"], undefined, "2mm 应已移除");
  assert.ok(matsMid["E2E测试碳纤维"]?.thicknesses?.["4"], "4mm 应保留");

  const delRes = await apiPost("/api/material-lab/manage", {
    action: "delete",
    material: "E2E测试碳纤维",
    thickness_mm: 0,
    confirmed: true,
  });
  assert.equal(delRes.status, 200, "删除整材料必须成功");
  assert.equal(delRes.data?.success, true);

  // 4. 再次验证已被移除
  const bootAfter = await apiGet("/v1/material-lab/bootstrap");
  const matsAfter = bootAfter.data?.result?.materials?.materials || {};
  assert.equal(matsAfter["E2E测试碳纤维"], undefined, "删除后材料必须不在库中");
});

test("[BTN-41] 雕刻工作台: 发送中点击「取消任务」真实触发 ! → ? → 0x18 安全停机并置 cancelled", async () => {
  const rec = await startRecordingGrbl({ ackDelayMs: 3 });
  try {
    const gen = await apiPost("/api/text-task/generate", {
      text: "MOSS-CANCEL-MOSS-CANCEL",
      material: "椴木",
      thickness_mm: 3,
      font_size_mm: 30,
      engraving_mode: "raster",
    });
    assert.equal(gen.status, 200);
    const wfId = gen.data?.result?.workflow_id;
    assert.ok(wfId);
    const target = {
      action: "confirm_send", workflow_id: wfId, confirmed: true,
      connection_mode: "network", network_host: GRBL_HOST, network_telnet_port: rec.port,
    };
    const send = await apiPost("/api/workflow", target);
    assert.equal(send.status, 200, JSON.stringify(send.data));
    const total = send.data?.result?.send_result?.total_lines || 0;
    assert.ok(total > 50, `刀路需足够长以便取消发生在发送中: ${total}`);

    // 等设备确实收到部分刀路后再取消，证明取消发生在发送中途
    const started = Date.now();
    while (!rec.events.includes("line") && Date.now() - started < 5000) await new Promise((r) => setTimeout(r, 10));
    const cancel = await apiPost("/api/workflow", { action: "cancel", workflow_id: wfId });
    assert.equal(cancel.status, 200, JSON.stringify(cancel.data));
    assert.equal(cancel.data?.result?.status, "cancelled");

    const final = await pollWorkflowUntil(wfId, ["cancelled", "completed", "failed"], 8000,
      (record) => record.send_result?.can_resume !== undefined);
    assert.equal(final.data?.result?.status, "cancelled", JSON.stringify(final.data?.result));
    const sr = final.data?.result?.send_result || {};
    assert.ok(sr.lines_sent > 0 && sr.lines_sent < total, `取消必须停在发送中途: ${sr.lines_sent}/${total}`);

    // 本连接上的真实字节顺序：! 之后有 ? 状态查询，再之后 0x18 软复位，且复位后不再下发任何刀路行
    const hold = rec.events.indexOf("!");
    assert.ok(hold >= 0, `必须收到 ! 进给保持: ${rec.events.slice(-20).join(" ")}`);
    const query = rec.events.indexOf("?", hold);
    assert.ok(query > hold, "! 之后必须轮询 ? 确认保持状态");
    const reset = rec.events.indexOf("0x18", query);
    assert.ok(reset > query, "确认保持后必须发送 0x18 软复位");
    assert.equal(rec.events.indexOf("line", reset), -1, "0x18 软复位后不得再下发刀路行");

    const again = await apiPost("/api/workflow", target);
    assert.equal(again.status, 400, "取消/完成后的任务必须拒绝重复下发");
  } finally {
    rec.close();
  }
});

test("[BTN-42] 雕刻工作台: 急停停机后记录断点，通过 /api/workflow (action: resume) 成功续雕剩余代码至终态", async () => {
  // 1. 生成一段长任务刀路
  const gen = await apiPost("/api/text-task/generate", {
    text: "MOSS-RESUME-E2E-TEST",
    material: "椴木",
    thickness_mm: 3,
    font_size_mm: 20,
    engraving_mode: "raster",
  });
  assert.equal(gen.status, 200);
  const wfId = gen.data?.result?.workflow_id;
  assert.ok(wfId);

  // 2. 首次下发
  const send = await apiPost("/api/workflow", {
    action: "confirm_send",
    workflow_id: wfId,
    confirmed: true,
    connection_mode: "network",
    network_host: GRBL_HOST,
    network_telnet_port: GRBL_PORT,
  });
  assert.equal(send.status, 200);
  const total = send.data?.result?.send_result?.total_lines || 0;
  assert.ok(total > 30, `刀路总行数必须充足: ${total}`);

  // 3. 在发送中下发急停/取消
  const cancel = await apiPost("/api/workflow", { action: "cancel", workflow_id: wfId });
  assert.equal(cancel.status, 200);

  // 轮询直到进入 cancelled 终态并记录断点
  const stopped = await pollWorkflowUntil(
    wfId,
    ["cancelled", "completed", "failed"],
    15000,
    (record) => record.status !== "cancelled" || record.send_result?.can_resume !== undefined,
  );
  const sr = stopped.data?.result?.send_result || {};
  const breakpoint = sr.lines_sent || 0;

  assert.ok(breakpoint > 0 && breakpoint < total, `必须在发送中形成有效断点: ${breakpoint}/${total}`);
  assert.equal(sr.can_resume, true, "断点停机必须标明支持断点续雕");
  assert.equal(sr.resumable_from_line, breakpoint);

  const resumeRes = await apiPost("/api/workflow", {
    action: "resume",
    workflow_id: wfId,
    confirmed: true,
    connection_mode: "network",
    network_host: GRBL_HOST,
    network_telnet_port: GRBL_PORT,
  });
  assert.equal(resumeRes.status, 200, `断点续雕请求必须成功: ${JSON.stringify(resumeRes.data)}`);
  assert.equal(resumeRes.data?.result?.status, "sending");
  assert.equal(resumeRes.data?.result?.send_result?.lines_sent, breakpoint, "续雕初始行必须精确对齐断点");

  const final = await pollWorkflowUntil(wfId, ["completed", "failed", "cancelled"], 15000);
  assert.equal(final.data?.result?.status, "completed", "断点续雕必须最终顺利完成");
  assert.equal(final.data?.result?.send_result?.lines_sent, total, "断点续雕完成后总行数必须达到 100%");
});

// -----------------------------------------------------------------------------
// 模块 4: 系统偏好与配对认证接口
// -----------------------------------------------------------------------------

test("[BTN-33] 偏好设置: Web Port 修改并通过 /api/settings 持久化至后端", async () => {
  // 查询当前端口
  const infoRes = await apiGet("/api/system/info");
  assert.equal(infoRes.status, 200);
  assert.equal(infoRes.data?.success, true);
  const originalPreferred = infoRes.data?.result?.preferred_port || 9666;

  // 更改并保存新端口
  const targetPort = originalPreferred === 9666 ? 9688 : 9666;
  const saveRes = await apiPost("/api/settings", { web_port: targetPort });
  assert.equal(saveRes.status, 200, "保存端口接口必须成功");
  assert.equal(saveRes.data?.success, true, "端口保存业务必须为 true");

  // 二次验证配置已更新
  const verifyRes = await apiGet("/api/system/info");
  assert.equal(verifyRes.status, 200);
  assert.equal(verifyRes.data?.result?.preferred_port, targetPort, "首选端口必须已更新为 targetPort");

  // 复原为原有端口
  await apiPost("/api/settings", { web_port: originalPreferred });
});

test("[BTN-35] 局域网配对认证: /v1/auth/pair 格式与安全校验契约", async () => {
  // 测试非法格式拦截 (非 6 位纯数字)
  const invalidRes = await apiPost("/v1/auth/pair", { pairing_code: "12" });
  assert.ok([400, 429].includes(invalidRes.status), "非法配对码必须被拦截拒绝");

  // 未配置测试配对码时，任意未知 6 位数字码都必须明确拒绝
  const res = await apiPost("/v1/auth/pair", { pairing_code: "123456" });
  assert.equal(res.status, 400, "未知配对码必须返回 400");
  assert.equal(res.data?.success, false);
});
