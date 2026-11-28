mod connection;
mod diagnose;
mod discovery;
mod dry_run;
mod preflight;
mod probe;
mod protocol;
mod transport;
mod upload;

pub use connection::{
    CancellationToken, DeviceConnection, DeviceState, JobProgress, JobWorker, rescale_gcode_line_s,
};
pub use diagnose::diagnose_device_message;
pub use discovery::{
    ControllerIdentity, DeviceIdentity, DiscoveredController, discover_controllers,
    discover_controllers_with, identify_controller, identify_grbl, local_lan_ipv4,
    parse_device_identity_line, scan_hosts_port, scan_hosts_port_with, scan_subnet_port,
    scan_subnet_port_with, subnet_hosts,
};
pub use dry_run::{
    DRY_RUN_DEFAULT_MAX_FEED_MM_MIN, DRY_RUN_FEED_OVERRIDE_DIVISOR, DRY_RUN_MAX_FEED_MM_MIN,
    DRY_RUN_MIN_FEED_MM_MIN, DRY_RUN_TTL_SECS, DryRunBlockHandler, DryRunBlockedCommand,
    DryRunBlockedKind, DryRunPolicy, DryRunSnapshot, DryRunState, DryRunTransport,
    check_gcode_lines_for_dry_run, detect_dry_run_blocked_line,
    detect_dry_run_blocked_line_for_transport, rewrite_gcode_line_for_dry_run,
    rewrite_gcode_lines_for_dry_run, validate_dry_run_max_feed,
};
pub use preflight::{SendPreflight, authorize_send};
pub use probe::{ProbeReport, available_serial_ports, probe_device, probe_opened_device};
pub use protocol::{
    AxisHoming, MAX_GCODE_LINE_BYTES, MachineActivity, MachineFeatures, MachineLimits,
    PreparedGcode, SdProgress, StatusReport, extract_device_explanation, is_reset_banner,
    laser_off_on_alarm_warning, parse_fluidnc_yaml, parse_grbl_settings_limits,
    parse_status_report, prepare_gcode_lines, strip_gcode_comments, validate_gcode_line,
};
pub use transport::{
    ConnectionSpec, DeviceTransport, FLUIDNC_CHANNEL_RX_WINDOW, FLUIDNC_LINE_QUEUE_LIMIT, FakeGrbl,
    GRBL_CLASSIC_RX_WINDOW, LineReader, MAX_RX_WINDOW, RealtimeCommand, SerialGrbl, TcpGrbl,
    open_transport,
};
pub use upload::upload_gcode_to_fluidnc_sd;
