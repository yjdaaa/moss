fn main() {
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-changed=tauri.conf.json");

    // 前端通过 invoke 调用的命令都必须登记在此，并在 capabilities/default.json 授权（见 lib.rs 测试）。
    let app_manifest = tauri_build::AppManifest::new().commands(&[
        "get_backend_origin",
        "get_desktop_session",
        "save_export_file",
        "get_export_settings",
        "pick_export_dir",
        "reset_export_dir",
        "set_export_ask_each_time",
        "open_export_dir",
        "reveal_exported_file",
        "request_shutdown",
        "minimize_to_tray",
        "exit_application",
        // 语音中心（技术书 §9.4.6，仅桌面端可用；移动端命令直接返回错误）
        "voice_status",
        "voice_save_settings",
        "voice_mode_changes",
        "voice_set_mode",
        "voice_server_start",
        "voice_server_stop",
        "voice_server_restart",
        "voice_server_log",
        "voice_models_test",
        "voice_models_save",
        // 内置语音服务（内置语音技术书 NV1）
        "voice_builtin_status",
        "voice_builtin_start",
        "voice_builtin_stop",
        "voice_device_set_status",
        "voice_device_rename",
        "voice_music_pick_directory",
        "voice_music_status",
        "voice_music_save",
        "voice_home_status",
        "voice_home_save",
        "voice_home_backup_export",
        "voice_home_backup_audit",
        "voice_home_backup_restore",
        "voice_home_token_save",
        "voice_home_voice_consent",
        "voice_home_approvals",
        "voice_home_approval_cancel",
        "voice_home_control_consent",
        "voice_home_approval_approve",
        "voice_home_discover",
        "voice_agent_status",
        "voice_agent_save",
        "voice_vision_status",
        "voice_vision_save",
        "voice_vision_test",
        "open_voice_private",
        "voice_plugins_status",
        "voice_plugins_save",
        "voice_memory_list",
        "voice_memory_set_enabled",
        "voice_memory_save",
        "voice_memory_clear",
        "voice_speaker_manage",
        "voice_firmware_status",
        // 内置语音服务的模型配置（NV5）
        "voice_builtin_models_test",
        "voice_builtin_models_save",
        // 内置语音服务的本地模型（NV6）
        "voice_local_save",
        "voice_local_model_download",
        "voice_local_model_cancel",
        "voice_local_model_delete",
        "voice_mcp_status",
        "voice_mcp_save",
        "voice_mcp_delete",
        "voice_mcp_reconnect",
    ]);
    let attributes = tauri_build::Attributes::new().app_manifest(app_manifest);

    tauri_build::try_build(attributes).expect("failed to build MOSS desktop application")
}
