# Laser Hardware BOM Baseline

This project uses the user's physical laser engraver as the default software target. The hardware is built from the Bilibili Dayu DIY "Yisu V1.0" procurement list, so generated jobs and default safety limits should assume this machine unless later device evidence proves otherwise.

## Source

- Source file: `B站【大鱼DIY】激光雕刻机采购清单_翼宿 V1.0.xlsx`
- Source package directory: `B站【大鱼DIY】激光雕刻机采购清单_翼宿 V1.01 全套资料`
- Video reference in the workbook: `https://www.bilibili.com/video/BV1PdFhzjEzL/`

Do not copy seller-specific runtime values, private serial ports, device IPs, Wi-Fi credentials, or tokens into this document.

## Machine Profile

| Field | Baseline |
|---|---|
| Machine | 大鱼 DIY 翼宿 V1.0 laser engraver |
| Work area | 100 mm x 100 mm |
| Controller board | G3 |
| Laser module | 24 V, 5 W, wired module |
| Power supply | 24 V, 6 A |
| Motion system | GT2 belt drive with MGN9H linear rails |
| Rails | 140 mm rail + MGN9H carriage, quantity 3 |
| Stepper motors | 42 stepper motor, 40 mm height, quantity 2 for Y axis |
| Cooling | 4028 fan, 24 V, 10000 RPM |
| Frame/parts | PETG printed parts, semi-transparent 3 mm acrylic, pulleys, idlers, 688ZZ bearings, 8 mm shaft, M3/M4 hardware |

## Software Implications

- Treat the machine envelope as `100 mm x 100 mm`. The host reads the real travel from FluidNC `$CD` (`max_travel_mm`) or classic GRBL `$130/$131` during the connection check and passes it to the frontend as `machine_limits`; since 2026-11-06 image and text previews are sized against a 100 mm × 100 mm default work area (overridable per request), and every send / framing run recomputes the toolpath extent and is refused with `job_exceeds_travel` when it is wider or taller than the travel the firmware reported. The draw-board export still uses its own 400 mm canvas and relies on that send-time check.
- Treat the laser as a `5 W` diode module. Use conservative defaults and material calibration rather than assuming higher-power laser behaviour.
- Treat GRBL `S` values as controller-scale values, not watts. Generated G-code uses a `0..1000` scale; before every send the host reads the device `$30` and rescales `S` on the fly when the device scale differs (for example 255). The host never writes `$30` or `$32`.
- The only full-job path is the workflow: `preview` freezes the file SHA-256 and processing fields, `confirm_send` re-reads the physical file, re-computes the SHA-256, performs the `$$` handshake (`$32=1`), the `$CD` read, the Idle/pin preflight and only then streams line by line. Network jobs use Telnet only.
- Single arbitrary GRBL commands are not exposed. Controlled actions (`status`, `unlock`, `home`, `set_origin`, `laser_off`, moves with `M5` first, `0x18`, feed/power override) go through `laser_safe_action_tool` / `/api/safe-action`; state-changing actions require `confirmed=true`.
- `check_laser_connection_tool` is safe for connection checks: it only performs the read-only handshake and returns the settings lines when `include_detail=true`.
- The workbook does not specify firmware, serial port, baud rate, microstepping, or GRBL settings. Verify those with read-only commands such as `$I`, `$$`, `$CD` and `$G` before changing controller assumptions.

## Runtime Configuration Keys

These are the only environment variables the Rust host reads (`laser_runtime::LaserSettings`, `laser_tools::resolve_connection_spec`, `laser_mcp::endpoint_from_env`). Put them in a local `.env`; never commit real values.

| Key | Default | Purpose |
|---|---:|---|
| `LASER_DEFAULT_CONNECTION_MODE` | `network` | Default transport when the caller does not say serial/network; `network` or `serial` |
| `LASER_ENGRAVING_MODE` | `raster` | Default engraving strategy; `raster` or `outline` |
| `LASER_WEB_HOST` | `127.0.0.1` | HTTP bind address; non-loopback binds require pairing |
| `LASER_WEB_PORT` | `9666` | HTTP port; the runtime `settings.json` `web_port` is used when the variable is absent |
| `LASER_MATERIAL_PARAMS_FILE` | empty | Optional material-library JSON mirror; when empty the library lives only in SQLite, seeded from the built-in presets |
| `GRBL_DEFAULT_PORT` | empty | Default serial port; when empty the UI selection is required |
| `GRBL_BAUDRATE` | `115200` | Serial baud rate |
| `LASER_NETWORK_HOST` | empty | Default controller host; leave empty rather than guessing, values containing `<` are ignored |
| `LASER_NETWORK_TELNET_PORT` | `23` | Telnet port for all network jobs |
| `MCP_ENDPOINT` | empty | Xiaozhi console endpoint (`ws://` or `wss://`); the bridge stays off when empty or still a template |
| `MOSS_ENABLE_DEVELOPER_COMMANDS` | empty | `true` exposes `command_execution_tool`; local debugging only |

Work-area size, safe margins, power scale, output directories and AI provider keys are no longer configurable through environment variables; they are fixed in code or derived from the device.

## BOM Summary

| Category | Item | Specification | Quantity | Workbook total |
|---|---|---|---:|---:|
| Electronics | Controller board | G3 | 1 | 96 |
| Electronics | Laser module | 24 V 5 W with wiring | 1 | 368 |
| Structure | PETG printed parts | Full Yisu printed set, 3 walls, 40% infill | 1 | 30 |
| Motion | Linear rails | 140 mm rail + MGN9H carriage | 3 | 75.6 |
| Bundle | Yisu accessory kit | See accessory sheet | 1 | 75 |
| Hardware | Screw/nut kit | See hardware sheet | 1 | 23 |
| Power | Power adapter | 24 V 6 A | 1 | 30 |
| Panel | Semi-transparent acrylic | Custom cut, 3 mm thickness | 1 | 18 |

Main sheet total: `715.6`.

## Accessory Sheet Highlights

| Item | Specification | Quantity |
|---|---|---:|
| 42 stepper motor | 40 mm height, 80 cm XH2.54 wire, Y axis | 2 |
| GT2 pulley | 20 teeth, 5 mm bore, 16 mm height, 6 mm belt | 2 |
| GT2 pulley | 20 teeth, 8 mm bore, 16 mm height, 6 mm belt | 1 |
| Double-slot pulley | 20 teeth, 8 mm bore | 1 |
| Idler | 16 teeth, 3 mm bore, toothed, 6 mm belt | 2 |
| Idler | 16 teeth, 3 mm bore, smooth, 6 mm belt | 2 |
| Idler | 20 teeth, 3 mm bore, toothed, 6 mm belt | 2 |
| GT2 belt | 6 mm width, 2 m | 1 |
| Closed-loop GT2 belt | GT2-122 mm | 1 |
| Fan | 4028, ball bearing, 24 V, 10000 RPM | 1 |
| Bearing | 688ZZ, 8 x 16 x 5 mm | 2 |
| Shaft | 8 mm diameter, 186 mm length | 1 |

Accessory sheet total: `82.4`.

## Fastener Sheet Highlights

| Item | Specification | Quantity |
|---|---|---:|
| Button-head socket screw | M3 x 8 | 31 |
| Button-head socket screw | M3 x 12 | 24 |
| Button-head socket screw | M3 x 16 | 8 |
| Button-head socket screw | M3 x 22 | 12 |
| Button-head socket screw | M3 x 40 | 2 |
| Countersunk socket screw | M3 x 8 | 12 |
| Countersunk socket screw | M3 x 16 | 2 |
| Countersunk socket screw | M3 x 40 | 7 |
| Flat-head socket screw | M3 x 6, 5.5 head diameter, 1 mm thick | 6 |
| Square nut | M3 x 6 x 2 | 74 |
| Square-head screw | M4 x 8 | 1 |
| Magnet | 6 mm diameter, 3 mm thick | 6 |
| Damping hinge | Small 1.5 kgf.cm right, non-adjustable | 1 |
| Damping hinge | Small 1.5 kgf.cm left, non-adjustable | 1 |

Fastener sheet total: `17.065`.
