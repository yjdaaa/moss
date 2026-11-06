# ESP-SR 模型文件（设备唤醒词资源包用）

这些文件原样取自乐鑫 ESP-SR 的模型目录，经 `78/xiaozhi-assets-generator`（MIT）提交
`55517b40d724014faff00f941ca700cbf9d14b51` 的 `web/public/static/` 转载：

- `mn6_cn/`：MultiNet6 中文命令词模型，用作自定义唤醒词（`multinet_model`）。
- `fst/`：MultiNet6/7 需要的默认命令词表。
- `wn9_nihaoxiaozhi_tts/`：WakeNet9“你好小智”，用于恢复默认唤醒词。

上游：<https://github.com/espressif/esp-sr/tree/master/model>。许可见同目录 `LICENSE`（ESPRESSIF MIT
License，仅限在乐鑫芯片产品上使用）；MOSS 只把它们打进下发给 ESP32 小智设备的资源包，桌面端不运行这些模型。

文件按字节原样保存（见仓库 `.gitattributes`），SHA-256 由 `wake_assets` 测试核对。
