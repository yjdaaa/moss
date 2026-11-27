# -*- coding: utf-8 -*-
"""VB3/VC2 离线识别测评与外放背景噪声工具：复用 chain_clips.py 的排风扇/气泵持续与突发噪声逻辑。
须使用 `python3 -I scripts/voice-bench/asr_noise_mix.py` 运行：
- 批量混噪：`--batch <manifest.json>`
- 生成纯背景噪声 WAV（供 VC2 `--noise fan|pump|burst` 循环播放）：
  `--generate-noise <fan|pump|burst|888|999> --duration-secs 12 --gain 0.25 --output <wav>`"""
import array
import json
import os
import sys
import wave

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
if SCRIPT_DIR not in sys.path:
    sys.path.insert(0, SCRIPT_DIR)

import chain_clips  # noqa: E402

NOISE_PRESET_MAP = {
    "fan": 888,
    "machine": 888,
    "pump": 999,
    "burst": 999,
}


def write_wav_16k(path, pcm):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with wave.open(path, "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(chain_clips.RATE)
        wf.writeframes(pcm.tobytes())


def generate_noise_track(preset_raw, duration_secs, gain, seed=42):
    key = str(preset_raw).strip().lower()
    level = NOISE_PRESET_MAP.get(key)
    if level is None:
        level = int(key)
    samples = max(chain_clips.RATE, int(round(float(duration_secs) * chain_clips.RATE)))
    zeros = array.array("h", [0] * samples)
    raw = chain_clips.noise(zeros, level, int(seed))
    scale = max(0.01, min(1.0, float(gain)))
    if abs(scale - 1.0) > 1e-4:
        return array.array("h", [max(-32768, min(32767, int(s * scale))) for s in raw])
    return raw


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--batch":
        with open(sys.argv[2], "r", encoding="utf-8") as fp:
            jobs = json.load(fp)
        for job in jobs:
            pcm = chain_clips.read_wav(job["input"])
            mixed = chain_clips.noise(pcm, int(job["level"]), int(job.get("seed", 0)))
            write_wav_16k(job["output"], mixed)
        return
    if len(sys.argv) >= 3 and sys.argv[1] == "--generate-noise":
        preset = sys.argv[2]
        duration_secs = 12.0
        gain = 0.25
        output = ""
        seed = 42
        i = 3
        while i < len(sys.argv):
            arg = sys.argv[i]
            if arg == "--duration-secs" and i + 1 < len(sys.argv):
                duration_secs = float(sys.argv[i + 1])
                i += 2
            elif arg == "--gain" and i + 1 < len(sys.argv):
                gain = float(sys.argv[i + 1])
                i += 2
            elif arg == "--output" and i + 1 < len(sys.argv):
                output = sys.argv[i + 1]
                i += 2
            elif arg == "--seed" and i + 1 < len(sys.argv):
                seed = int(sys.argv[i + 1])
                i += 2
            else:
                sys.stderr.write(f"未知参数: {arg}\n")
                sys.exit(2)
        if not output:
            sys.stderr.write("缺少 --output 参数\n")
            sys.exit(2)
        pcm = generate_noise_track(preset, duration_secs, gain, seed)
        write_wav_16k(output, pcm)
        return
    if len(sys.argv) == 5:
        src, dst, level, seed = sys.argv[1:5]
        pcm = chain_clips.read_wav(src)
        mixed = chain_clips.noise(pcm, int(level), int(seed))
        write_wav_16k(dst, mixed)
        return
    sys.stderr.write(
        "用法: python3 -I asr_noise_mix.py --batch <manifest.json> | --generate-noise <fan|pump|burst> --output <wav>\n"
    )
    sys.exit(2)


if __name__ == "__main__":
    main()
