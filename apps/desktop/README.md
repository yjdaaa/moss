# MOSS Desktop

Tauri 2 desktop shell for the embedded `frontend/dist`.

The shell currently owns an idempotent shutdown request. Device emergency-stop
handling is intentionally not claimed until `laser-host` exposes synchronous raw
handles for the `0x18` safety sequence.
