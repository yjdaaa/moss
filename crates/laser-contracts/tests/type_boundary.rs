//! 类型边界守卫（规格 §4 / 阶段 0）：`laser-tools`、`laser-device`、`laser-core`
//! 只能通过 `laser-contracts` 的强类型结构交换数据，禁止直接依赖 `serde_json`
//! 或使用 `Value` 动态访问器。原先由 `migration/check_type_boundaries.py` 承担，
//! 该脚本用正则 `\b(as_str|…)\s*\(` 匹配，会把 `String::as_str()` 误判为违规；
//! 这里改为“禁止 serde_json 依赖 + 禁止 Value 专有访问器”两条无歧义规则。

use std::path::{Path, PathBuf};

const GUARDED_CRATES: [&str; 3] = ["laser-tools", "laser-device", "laser-core"];

/// `String`/`str` 没有这些方法，只有 `serde_json::Value` 有，因此不会误判。
const VALUE_ONLY_ACCESSORS: [&str; 5] = [".as_f64(", ".as_i64(", ".as_u64(", ".as_bool(", ".as_object("];

fn workspace_crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("laser-contracts 位于 crates/ 之下")
        .to_path_buf()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("读取源码目录") {
        let path = entry.expect("目录项").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// 只检查 `[dependencies]` 段：`dev-dependencies` 允许在测试里用 serde_json 断言。
fn runtime_dependencies_section(manifest: &str) -> String {
    let mut inside = false;
    let mut section = String::new();
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            inside = trimmed == "[dependencies]";
            continue;
        }
        if inside {
            section.push_str(trimmed);
            section.push('\n');
        }
    }
    section
}

#[test]
fn contract_crates_do_not_depend_on_serde_json() {
    let root = workspace_crates_dir();
    let mut violations = Vec::new();
    for krate in GUARDED_CRATES {
        let manifest_path = root.join(krate).join("Cargo.toml");
        let manifest = std::fs::read_to_string(&manifest_path)
            .unwrap_or_else(|e| panic!("读取 {} 失败: {e}", manifest_path.display()));
        if runtime_dependencies_section(&manifest).contains("serde_json") {
            violations.push(format!("{}: [dependencies] 含 serde_json", manifest_path.display()));
        }
    }
    assert!(violations.is_empty(), "TYPE_BOUNDARY_VIOLATION\n{}", violations.join("\n"));
}

#[test]
fn contract_crate_sources_do_not_touch_serde_json_values() {
    let root = workspace_crates_dir();
    let mut violations = Vec::new();
    for krate in GUARDED_CRATES {
        let src = root.join(krate).join("src");
        assert!(src.is_dir(), "{} 缺少 src 目录", src.display());
        let mut files = Vec::new();
        rust_sources(&src, &mut files);
        files.sort();
        for file in files {
            let text = std::fs::read_to_string(&file).expect("读取源码");
            for (index, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                let hit = code.contains("serde_json")
                    || VALUE_ONLY_ACCESSORS.iter().any(|needle| code.contains(needle));
                if hit {
                    violations.push(format!("{}:{}: {}", file.display(), index + 1, line.trim()));
                }
            }
        }
    }
    assert!(violations.is_empty(), "TYPE_BOUNDARY_VIOLATION\n{}", violations.join("\n"));
}

#[test]
fn string_as_str_is_not_a_boundary_violation() {
    // 回归：`(host.as_str(), *port).to_socket_addrs()` 这类标准库用法不得被判违规。
    let sample = "let addrs = (host.as_str(), *port).to_socket_addrs();";
    assert!(!VALUE_ONLY_ACCESSORS.iter().any(|needle| sample.contains(needle)));
    assert!(!sample.contains("serde_json"));
}
