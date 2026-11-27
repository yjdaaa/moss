# -*- coding: utf-8 -*-
"""VB3 离线识别测评混噪工具：复用 chain_clips.py 的排风扇/气泵持续与突发噪声混合逻辑。
须使用 `python3 -I scripts/voice-bench/asr_noise_mix.py --batch <manifest.json>` 运行。"""
import json
import os
import sys
import wave

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
if SCRIPT_DIR not in sys.path:
    sys.path.insert(0, SCRIPT_DIR)

import chain_clips  # noqa: E402


def write_wav_16k(path, pcm):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with wave.open(path, "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(chain_clips.RATE)
        wf.writeframes(pcm.tobytes())


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--batch":
        with open(sys.argv[2], "r", encoding="utf-8") as fp:
            jobs = json.load(fp)
        for job in jobs:
            pcm = chain_clips.read_wav(job["input"])
            mixed = chain_clips.noise(pcm, int(job["level"]), int(job.get("seed", 0)))
            write_wav_16k(job["output"], mixed)
        return
    if len(sys.argv) == 5:
        src, dst, level, seed = sys.argv[1:5]
        pcm = chain_clips.read_wav(src)
        mixed = chain_clips.noise(pcm, int(level), int(seed))
        write_wav_16k(dst, mixed)
        return
    sys.stderr.write("用法: python3 -I asr_noise_mix.py --batch <manifest.json>\n")
    sys.exit(2)


if __name__ == "__main__":
    main()
