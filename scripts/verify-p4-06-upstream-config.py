"""P4-06 / A-67 离线校验：用上游 xiaozhi-server 原版 config_loader 读取并合并语音中心写出的配置。

只做本地读取，不启动服务、不访问网络。上游的 `config.manage_api_client`（智控台 HTTP 客户端）
在单模块部署下不会被调用，这里替换为空桩以免导入 httpx；`read_config`、`merge_configs`
与 provider 选择规则均用上游原代码与原文件。

用法（以 -I 隔离运行，上游源码目录只作为参数传入）：
  python -I scripts/verify-p4-06-upstream-config.py --server-dir <上游>/main/xiaozhi-server \
      --original <写入前.yaml> --custom <语音中心写出的.yaml>
退出码：0 全部通过；1 有检查失败；2 参数或环境错误。
"""

import argparse
import os
import sys
import types

FAKE_KEY = "sk-moss-p4-06-fake-key"  # 与 crates/moss-voice/examples/p4_06_config_yaml.rs 一致

# 语音中心写入的字段（技术书 §9.4.4 字段表），以及上游 provider 实际读取它们的位置
EXPECTED = {
    ("selected_module", "LLM"): "MossLLM",
    ("selected_module", "ASR"): "MossASR",
    ("selected_module", "TTS"): "MossTTS",
    ("selected_module", "Intent"): "function_call",
    ("LLM", "MossLLM", "type"): "openai",
    ("LLM", "MossLLM", "url"): "https://llm.example.invalid/v1",  # llm/openai/openai.py: config.get("url")
    ("LLM", "MossLLM", "api_key"): FAKE_KEY,
    ("LLM", "MossLLM", "model_name"): "moss-llm-model",
    ("ASR", "MossASR", "type"): "openai",
    ("ASR", "MossASR", "base_url"): "https://asr.example.invalid/v1/audio/transcriptions",  # asr/openai.py: 直接 POST
    ("ASR", "MossASR", "api_key"): FAKE_KEY,
    ("ASR", "MossASR", "model_name"): "moss-asr-model",
    ("ASR", "MossASR", "output_dir"): "tmp/",
    ("TTS", "MossTTS", "type"): "openai",
    ("TTS", "MossTTS", "api_url"): "https://tts.example.invalid/v1/audio/speech",
    ("TTS", "MossTTS", "api_key"): FAKE_KEY,
    ("TTS", "MossTTS", "model"): "moss-tts-model",
    ("TTS", "MossTTS", "voice"): "moss-voice",
    ("TTS", "MossTTS", "speed"): 1.25,
    ("TTS", "MossTTS", "output_dir"): "tmp/",
    ("mcp_endpoint",): "ws://127.0.0.1:8004/mcp_endpoint/mcp/?token=placeholder",
}


def lookup(tree, path):
    for key in path:
        if not isinstance(tree, dict) or key not in tree:
            return KeyError
        tree = tree[key]
    return tree


def flatten(tree, prefix=()):
    if isinstance(tree, dict):
        for key, value in tree.items():
            yield from flatten(value, prefix + (key,))
    else:
        yield prefix, tree


def load_upstream_loader(server_dir):
    stub = types.ModuleType("config.manage_api_client")
    for name in ("init_service", "get_server_config", "get_agent_models", "get_correct_words"):
        setattr(stub, name, lambda *args, **kwargs: None)
    for name in ("DeviceNotFoundException", "DeviceBindException"):
        setattr(stub, name, type(name, (Exception,), {}))
    sys.modules["config.manage_api_client"] = stub
    sys.path.insert(0, server_dir)
    import config.config_loader as loader  # noqa: E402  上游原文件

    return loader


def main():
    # -I 会忽略 PYTHONIOENCODING；Windows 控制台默认 GBK，统一按 UTF-8 输出报告
    sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser()
    parser.add_argument("--server-dir", required=True)
    parser.add_argument("--original", required=True)
    parser.add_argument("--custom", required=True)
    args = parser.parse_args()
    server_dir = os.path.abspath(args.server_dir)
    if not os.path.isfile(os.path.join(server_dir, "config", "config_loader.py")):
        print(f"ERROR: {server_dir} 下没有 config/config_loader.py")
        return 2

    loader = load_upstream_loader(server_dir)
    default = loader.read_config(os.path.join(server_dir, "config.yaml"))
    original = loader.read_config(args.original) or {}
    custom = loader.read_config(args.custom)

    failures = []

    def check(ok, label):
        print(("PASS " if ok else "FAIL ") + label)
        if not ok:
            failures.append(label)

    check(isinstance(custom, dict), "上游 read_config 能解析写出的文件")
    check(not (custom.get("manager-api") or {}).get("url"), "单模块部署：load_config 走 merge_configs 分支")
    merged = loader.merge_configs(default, custom)

    for path, expected in EXPECTED.items():
        actual = lookup(merged, path)
        check(actual == expected, f"{'.'.join(path)} = {actual!r}")

    # 与 core/utils/modules_initialize.py 相同的选择规则：config[模块][selected_module[模块]]["type"]
    providers = {
        "LLM": os.path.join("core", "providers", "llm", "{t}", "{t}.py"),
        "ASR": os.path.join("core", "providers", "asr", "{t}.py"),
        "TTS": os.path.join("core", "providers", "tts", "{t}.py"),
        "Intent": os.path.join("core", "providers", "intent", "{t}", "{t}.py"),
    }
    for module, pattern in providers.items():
        selected = merged["selected_module"][module]
        kind = lookup(merged, (module, selected, "type"))
        target = os.path.join(server_dir, pattern.format(t=kind))
        check(os.path.isfile(target), f"{module}.{selected}.type={kind} → {os.path.relpath(target, server_dir)}")

    # 未写入的键：写出文件中与写入前一致（按行编辑不得改动其他内容）
    written = {path for path in EXPECTED}
    changed = [
        ".".join(map(str, path))
        for path, value in flatten(original)
        if path not in written and lookup(custom, path) != value
    ]
    check(not changed, "写入前已有的其他键保持不变" + (f"：{changed[:5]}" if changed else ""))
    # 默认配置中其余模块仍可用（合并只覆盖写入的键）
    vad = merged["selected_module"].get("VAD")
    check(vad is not None and lookup(merged, ("VAD", vad, "type")) is not KeyError, f"默认 VAD 模块 {vad} 仍可解析")

    print(f"\n{len(failures)} 项失败" if failures else "\n全部通过")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
