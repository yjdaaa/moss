import test from "node:test";
import assert from "node:assert/strict";
import {
  ExportDirUnavailableError,
  exportFile,
  parseExportDirUnavailable,
  saveFile,
  savedMessage,
} from "../src/saveFile.js";

function tauri(handler) {
  const calls = [];
  return {
    calls,
    win: { __TAURI__: { core: { invoke: async (cmd, args) => { calls.push([cmd, args]); return handler(cmd, args, calls); } } } },
  };
}

test("Tauri 中导出交给宿主命令，原样传递文件名、字节与回退标记", async () => {
  const { calls, win } = tauri(() => "C:\\Users\\me\\Downloads\\a.png");
  const result = await saveFile(new Blob([new Uint8Array([1, 2, 3])]), "a.png", win);
  assert.deepEqual(calls[0], ["save_export_file", { filename: "a.png", bytes: [1, 2, 3], fallbackDefault: false }]);
  assert.deepEqual(result, { path: "C:\\Users\\me\\Downloads\\a.png", cancelled: false });
  assert.equal(savedMessage("PNG ", result), "PNG 已保存到：C:\\Users\\me\\Downloads\\a.png");
});

test("“另存为”被取消时返回 cancelled，不报错", async () => {
  const { win } = tauri(() => null);
  const result = await saveFile(new Blob(["x"]), "a.json", win);
  assert.deepEqual(result, { path: null, cancelled: true });
  assert.equal(savedMessage("场景 ", result), "已取消导出");
});

test("目录不可用错误被识别为 ExportDirUnavailableError", async () => {
  assert.deepEqual(parseExportDirUnavailable("export_dir_unavailable:E:\\U盘|导出目录不存在"), { dir: "E:\\U盘", reason: "导出目录不存在" });
  assert.equal(parseExportDirUnavailable(new Error("写入文件失败")), null);
  const { win } = tauri(() => { throw "export_dir_unavailable:E:\\U盘|导出目录不存在"; });
  await assert.rejects(saveFile(new Blob(["x"]), "a.json", win), (error) => {
    assert.ok(error instanceof ExportDirUnavailableError);
    assert.equal(error.dir, "E:\\U盘");
    return true;
  });
});

test("宿主其他错误原样抛出，不假装成功", async () => {
  const { win } = tauri(() => { throw new Error("写入文件失败"); });
  await assert.rejects(saveFile(new Blob(["x"]), "a.json", win), /写入文件失败/);
});

test("没有 Tauri 也没有 document 时明确报错", async () => {
  await assert.rejects(saveFile(new Blob(["x"]), "a.json", {}), /无法保存文件/);
});

test("目录不可用 → 选“本次保存到下载文件夹”只带 fallbackDefault 重试一次", async () => {
  const { calls, win } = tauri((cmd, args) => {
    if (cmd === "save_export_file" && !args.fallbackDefault) throw "export_dir_unavailable:E:\\U盘|导出目录不存在";
    if (cmd === "save_export_file") return "C:\\Users\\me\\Downloads\\a.png";
    return null;
  });
  const choices = ["confirm", "cancel"]; // 第二次是“导出完成”框，选关闭
  const result = await exportFile(new Blob(["x"]), "a.png", "PNG ", { win, showChoice: async () => choices.shift(), showAlert: async () => {} });
  assert.equal(result.path, "C:\\Users\\me\\Downloads\\a.png");
  assert.deepEqual(calls.map(([cmd, args]) => [cmd, args?.fallbackDefault]), [["save_export_file", false], ["save_export_file", true]]);
});

test("目录不可用 → 重新选择后按新目录重试；选择框取消则放弃导出", async () => {
  let picked = { dir: "D:\\导出" };
  let fail = true;
  const { calls, win } = tauri((cmd) => {
    if (cmd === "save_export_file") {
      if (fail) { fail = false; throw "export_dir_unavailable:E:\\U盘|导出目录不存在"; }
      return "D:\\导出\\a.png";
    }
    if (cmd === "pick_export_dir") return picked;
    return null;
  });
  const result = await exportFile(new Blob(["x"]), "a.png", "PNG ", { win, showChoice: async () => (calls.length === 1 ? "cancel" : "dismiss"), showAlert: async () => {} });
  assert.equal(result.path, "D:\\导出\\a.png");
  assert.deepEqual(calls.map(([cmd]) => cmd), ["save_export_file", "pick_export_dir", "save_export_file"]);

  picked = null;
  fail = true;
  calls.length = 0;
  const alerts = [];
  const cancelled = await exportFile(new Blob(["x"]), "a.png", "PNG ", { win, showChoice: async () => "cancel", showAlert: async (m) => alerts.push(m) });
  assert.deepEqual(cancelled, { path: null, cancelled: true });
  assert.deepEqual(calls.map(([cmd]) => cmd), ["save_export_file", "pick_export_dir"]);
});

test("导出成功后选择“打开所在文件夹”调用 reveal_exported_file", async () => {
  const { calls, win } = tauri((cmd) => (cmd === "save_export_file" ? "C:\\x\\a.png" : null));
  await exportFile(new Blob(["x"]), "a.png", "PNG ", { win, showChoice: async () => "confirm", showAlert: async () => {} });
  assert.deepEqual(calls.at(-1), ["reveal_exported_file", { path: "C:\\x\\a.png" }]);
});
