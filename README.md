<p align="center">
  <img src="./assets/moss-banner.png" width="960" alt="MOSS AI Laser CNC and Voice Hub Banner">
</p>

<h1 align="center">MOSS</h1>

<p align="center">Pure-Rust FluidNC/GRBL laser CNC host and built-in Xiaozhi AI voice hub across Windows, macOS, and Android.</p>

<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-1.98%2B-3776AB?style=flat-square&logo=rust&logoColor=white" alt="Rust 1.98+">
  <img src="https://img.shields.io/badge/Tauri-2.0-3776AB?style=flat-square&logo=tauri&logoColor=white" alt="Tauri 2.0">
  <img src="https://img.shields.io/badge/Hardware-FluidNC%20%7C%20GRBL-0F766E?style=flat-square" alt="Hardware: FluidNC | GRBL">
  <img src="https://img.shields.io/badge/Platform-Windows%20%7C%20macOS%20%7C%20Android-4B5563?style=flat-square" alt="Platform: Windows | macOS | Android">
  <a href="./LICENSE"><img src="https://img.shields.io/badge/License-MIT-22C55E?style=flat-square" alt="License: MIT"></a>
  <a href="./docs/README.md"><img src="https://img.shields.io/badge/Docs-Reference-4F46E5?style=flat-square" alt="Documentation"></a>
</p>

MOSS unifies G-code toolpath generation, a 3D machine digital twin, an Excalidraw creative drawpad, serial/Telnet/SD-card job control for FluidNC and GRBL controllers, and a built-in Xiaozhi AI voice and MCP server in a single Rust workspace. Toolpath generation never communicates with hardware directly, and every physical motion, laser firing, smart-home action, or local CLI agent execution passes explicit preflight checks and user approval gates.

## Highlights

| Capability | Why it matters |
| --- | --- |
| Pure-Rust Laser Engine | Generates vector outline, hatch, and raster G-code in `laser-core` without Python runtime dependencies, isolated from physical device I/O. |
| FluidNC & GRBL Streaming | Extracts controller YAML configuration (`$CD`), streams over dynamic `1024B` Rx buffers via serial or Telnet, and verifies SD-card uploads for offline execution. |
| Multi-Layer Safety Gates | Recalculates on-disk G-code SHA-256 before firing, routes `0x18` software reset over a dedicated real-time channel, blocks PVC, and requires explicit confirmation for unknown plastics. |
| Built-in Xiaozhi Voice Hub | Runs an embedded Xiaozhi WebSocket/OTA voice pipeline (`moss-voice-server`) supporting cloud providers and offline local models (`moss-voice-local` for VAD, ASR, TTS, and speaker hints) alongside MCP JSON-RPC 2.0 tool dispatch. |
| 3D Digital Twin & Drawpad | Integrates the main CNC workbench (`/`), an Excalidraw vector canvas (`/draw`), and a Three.js 3D work-envelope and toolpath preview (`/twin`). |
| Guarded Local Extensions | Keeps Home Assistant, local `Claude Code` / `Codex` runs, speaker hints, and firmware push disabled by default; when enabled in the private voice window, sensitive actions require per-action desktop approval. |

## Architecture

```text
+-----------------------------------------------------------------------------------+
| React + Vite Frontend (Workbench / | Drawpad /draw | 3D Twin /twin | Voice /voice)|
+------------------------------------+-------------------------+--------------------+
                                     | HTTP / SSE              | Tauri IPC (private ACL)
                                     v                         v
+-----------------------------------------------------------------------------------+
| Tauri 2 Shell (apps/desktop)  OR  Headless CLI Host (apps/moss-host)              |
|   +-- laser-host (startup orchestration, approvals, guarded local CLI runner)     |
+----------------+-----------------------------+--------------------------+---------+
                 |                             |                          |
                 v                             v                          v
      +--------------------+        +--------------------+     +--------------------+
      | laser-api          |        | laser-mcp          |     | moss-voice-server  |
      | REST + SSE routes  |<-------+ MCP JSON-RPC 2.0   |<----+ Built-in Xiaozhi   |
      +----+----------+----+        +---------+----------+     | WS / OTA / Context |
           |          |                       |                +---------+----------+
           v          v                       v                          | stdio IPC
+---------------+  +------------------+  +--------------+                v
| laser-core    |  | laser-runtime    |  | laser-tools  |     +---------------------+
| G-code / font |  | SQLite WAL store |  | ToolCatalog  |     | moss-voice-local    |
| (no hardware) |  | + OS secret store|  +------+-------+     | Offline VAD/ASR/TTS |
+---------------+  +--------+---------+         |             | (standalone GPL-3)  |
                            |                   |             +---------------------+
                            v                   v
                 +--------------------------------------+
                 | laser-device (preflight + 0x18 lane) |
                 +------------------+-------------------+
                                    | Serial / Telnet / HTTP SD upload
                                    v
                 +--------------------------------------+
                 | FluidNC / GRBL / ESP32 Controller    |
                 +--------------------------------------+
```

`laser-core` computes toolpaths and estimates without touching serial or network ports. MCP tools that mutate state or start jobs route back through `HostBackend` in `laser-host` so HTTP and voice-driven actions share identical SHA-256 verification, material gates, and desktop approval records. Note that `0x18` is a real-time software reset byte and does not replace a physical emergency-stop switch on the machine.

## Usage Example

Generate an outline toolpath preview from vector text without firing the laser, then pass the confirmation send gate:

```bash
# 1. Preview a vector text job (writes G-code and computes SHA-256; never fires hardware)
curl -X POST http://127.0.0.1:9666/api/workflow \
  -H "Content-Type: application/json" \
  -d '{"action":"preview","source_type":"text","text":"MOSS","engraving_mode":"outline","material":"Basswood","thickness_mm":3}'

# Example response fields:
# {"status":"prepared","gcode_sha256":"<computed-sha256>","workflow_id":"wf-..."}

# 2. Confirm and dispatch through the hardware gate (rejects if unconfirmed or file hash changed)
curl -X POST http://127.0.0.1:9666/api/workflow \
  -H "Content-Type: application/json" \
  -d '{"action":"confirm_send","workflow_id":"wf-...","confirmed":true}'
```

`JobWorker` streams G-code only after re-reading the physical file from disk to verify `gcode_sha256` and confirming that the controller reports `Idle`.

## Quick Install

Prerequisites:

- **Rust** `1.98+` (`rustup` stable toolchain)
- **Node.js** `20+` and `npm`
- **Platform SDKs**: WebView2 on Windows, Xcode Command Line Tools on macOS (`11.0+`), and Android SDK/NDK if building the Android APK

```bash
# Clone the repository
git clone https://github.com/yjdaaa/moss.git
cd moss

# Install frontend dependencies
cd frontend
npm install
cd ..
```

## Quick Start

### 1. Run Desktop Application (Development)

```bash
cd apps/desktop/src-tauri
cargo tauri dev
```

### 2. Run Standalone Headless Host

```bash
cargo run -p moss-host -- serve
```

### 3. Build Release Installers

```powershell
# Windows: NSIS installer + standalone moss-voice-local sidecar
powershell -ExecutionPolicy Bypass -File scripts/build-windows-installer.ps1
```

```bash
# macOS: .app and .dmg bundle (bundles moss-voice-local sidecar by default)
bash scripts/build-macos-installer.sh

# Android: aarch64 release APK
cd apps/desktop/src-tauri
cargo tauri android build --target aarch64 --apk
```

## Verification

Run the automated test suites from the repository root before submitting changes:

```bash
# Rust workspace tests (laser crates, voice crates, runtime schema, and safety gates)
cargo test --workspace

# Standalone local voice helper workspace tests
cargo test --manifest-path apps/moss-voice-local/Cargo.toml

# Frontend unit tests and production bundle check
cd frontend && npm test && npm run build && cd ..

# End-to-end closed-loop workflow tests against the loopback GRBL simulator
cd frontend && npm run test:e2e && cd ..
```

## Documentation

Full documentation is indexed in [`docs/README.md`](./docs/README.md):

| Topic | What it covers | Link |
| --- | --- | --- |
| Quick Start | First-time desktop/host setup, `.env` options, LAN phone pairing, and safe first engrave | [docs/user/quick-start.md](./docs/user/quick-start.md) |
| Hardware Acceptance | Six-stage verification checklist from single PC to ESP32 + FluidNC hardware | [docs/integration/moss-hardware-acceptance-plan.md](./docs/integration/moss-hardware-acceptance-plan.md) |
| Architecture Overview | Process topology, crate boundaries, HTTP/SSE/MCP bridges, and shutdown/reset paths | [docs/developer/architecture.md](./docs/developer/architecture.md) |
| Safety Gates & Preflight | On-disk SHA-256 check, material rules, spindle scaling, and `0x18` real-time reset | [docs/developer/safety.md](./docs/developer/safety.md) |
| Packaging & Installers | Windows NSIS, macOS `.app`/`.dmg`, Android APK, and voice sidecar bundling rules | [docs/developer/packaging-install.md](./docs/developer/packaging-install.md) |
| Built-in Voice Spec | Native Xiaozhi protocol server, local/cloud providers, long-term memory, and privacy ACLs | [docs/integration/moss-native-voice-spec.md](./docs/integration/moss-native-voice-spec.md) |
| Web API Contracts | `/api/*` and `/v1/*` HTTP routes, SSE payloads, error codes, and Bearer auth | [docs/developer/api-contracts.md](./docs/developer/api-contracts.md) |
| Database Schema | `<RuntimeRoot>/moss.sqlite` WAL schema, migrations, approvals, and material library format | [docs/developer/db-schema.md](./docs/developer/db-schema.md) |
| Laser & FluidNC Setup | Controller YAML settings, SD offline upload workflow, and ESP32 network commands | [docs/laser/README.md](./docs/laser/README.md) |
| Development Standards | Coding conventions, `ToolCatalog` registration, verification matrix, and PR rules | [docs/developer/standards.md](./docs/developer/standards.md) |

## License

The main MOSS workspace is licensed under the [MIT License](./LICENSE).

The optional offline voice helper in [`apps/moss-voice-local`](./apps/moss-voice-local/README.md) is an independent Cargo workspace licensed under [GPL-3.0-or-later](./apps/moss-voice-local/COPYING). MOSS communicates with it strictly over standard I/O frames (`crates/moss-voice-local-proto`, MIT) without linking its code into the main workspace.
