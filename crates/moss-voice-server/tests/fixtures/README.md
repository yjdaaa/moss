# NV14 Synthetic Music Decoder Fixtures

These four files are generated **from scratch** as 1-second, 440 Hz,
48 kHz, 2-channel sine waves for automated decoder tests only.
There are no human recordings, songs, user files or actual device audio.

| File | Purpose | SHA-256 |
| --- | --- | --- |
| `sine-48k-stereo.mp3` | MP3 decoding | `24189bd0b286a5703319f231c8acdaed23258cf9db3cbc56c02667abffcf9508` |
| `sine-48k-stereo.flac` | FLAC decoding | `420df7bc22a6fa023a20cc819bbf9684f3bc0278bd6f71e0a69fa8cdb5f22569` |
| `sine-48k-stereo.ogg` | Ogg Vorbis decoding | `7409deee04c0f57c785abdf50bee5c74e03d74ec084bccecb2d62f08a6cc4efc` |
| `sine-48k-stereo-opus.ogg` | Unsupported Ogg Opus must fail closed | `3589074759bb933f96c59833dedb97e452660bc594323b24e48edd75f8c9b2f8` |

To recreate these test-only files from a pinned cached Ubuntu 22.04
Docker image, run from the repository root:

```powershell
pwsh -NoProfile -File .\scripts\generate-nv14-synthetic-audio-fixtures.ps1
```

The generator temporarily uses Ubuntu **GPG-verified** ffmpeg package
`7:4.4.2-0ubuntu0.22.04.1`. Codec conversion occurs inside a
`--network none`, non-root, capability-dropped disposable container.
It deletes only its own temporary Docker image/container/build directory
and leaves existing audio fixtures unchanged unless their hashes match.
It does not install FFmpeg onto Windows or access any personal music.

The encoded fixture files only contain the generated sine wave; the
FFmpeg executable and its GPL/LGPL-linked libraries are **not shipped**
inside MOSS.
