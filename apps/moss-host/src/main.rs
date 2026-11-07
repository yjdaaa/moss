fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("migrate") {
        if let Err(error) = run_migrate(&args[2..]) {
            eprintln!("迁移失败：{error:?}");
            std::process::exit(2);
        }
        return;
    }
    if args.get(1).map(String::as_str) == Some("serve") {
        let host = laser_host::HostApplication::init().expect("failed to init host");
        let (server, actual_port) = host
            .spawn_http_server()
            .expect("failed to spawn http server");
        println!(
            "moss-host serving on {}:{}",
            host.settings.web_host, actual_port
        );
        match host.spawn_mcp_bridge() {
            0 => println!("moss-host mcp bridge disabled (MCP_ENDPOINT 未配置)"),
            count => println!("moss-host mcp bridge connecting to {count} endpoint(s)"),
        }
        let _ = server.join();
        return;
    }
    assert!(laser_api::embedded_fixture_is_valid());
    assert_eq!(laser_runtime::packaging_sqlite_probe().unwrap(), 42);
    assert_eq!(
        laser_mcp::tools_list_json(true)["tools"]
            .as_array()
            .map(Vec::len),
        Some(20)
    );
    println!("moss-host stage-0 probes passed");
}

fn run_migrate(args: &[String]) -> Result<(), String> {
    let mut source_root = None;
    let mut source_roots = Vec::new();
    // 默认目标与程序实际使用的数据目录一致（Windows 为 %APPDATA%\moss），避免迁到一个程序不读的目录。
    let mut target_root = laser_runtime::RuntimeRoot::default_base().map_err(|e| e.to_string())?;
    let mut historical_roots = Vec::new();
    let mut dry_run = false;
    let mut on_conflict = laser_runtime::ConflictPolicy::Keep;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--from" => {
                index += 1;
                let value = args.get(index).ok_or("--from 缺少目录")?;
                let path = std::path::PathBuf::from(value);
                source_root = Some(path.clone());
                source_roots.push(path.clone());
                historical_roots.push(path);
            }
            "--target" => {
                index += 1;
                target_root = std::path::PathBuf::from(args.get(index).ok_or("--target 缺少目录")?);
            }
            "--dry-run" => dry_run = true,
            "--on-conflict" => {
                index += 1;
                on_conflict = match args.get(index).map(String::as_str) {
                    Some("keep") | None => laser_runtime::ConflictPolicy::Keep,
                    Some("overwrite") => laser_runtime::ConflictPolicy::Overwrite,
                    Some(other) => return Err(format!("未知冲突策略：{other}")),
                };
            }
            other => return Err(format!("未知迁移参数：{other}")),
        }
        index += 1;
    }
    let source_root = source_root.ok_or("必须提供 --from <旧根>")?;
    let options = laser_runtime::MigrationOptions {
        source_root,
        source_roots,
        target_root,
        historical_roots,
        dry_run,
        on_conflict,
    };
    let (_, report_path) = laser_runtime::run_migration(&options).map_err(|e| format!("{e:?}"))?;
    println!("迁移报告：{}", report_path.display());
    Ok(())
}
