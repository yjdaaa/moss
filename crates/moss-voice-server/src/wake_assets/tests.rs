use super::*;

fn sha(bytes: &[u8]) -> String {
    crate::firmware::sha256_hex(bytes)
}

fn entry(name: &str, data: &[u8]) -> AssetEntry {
    AssetEntry {
        name: name.into(),
        data: data.to_vec(),
        width: 0,
        height: 0,
    }
}

fn index_of(package: &[u8]) -> Value {
    let entries = parse_assets(package).unwrap();
    serde_json::from_slice(
        &entries
            .iter()
            .find(|e| e.name == "index.json")
            .unwrap()
            .data,
    )
    .unwrap()
}

fn model_names(srmodels: &[u8]) -> Vec<String> {
    let count = u32_at(srmodels, 0).unwrap() as usize;
    let mut at = 4;
    let mut names = Vec::new();
    for _ in 0..count {
        names.push(fixed_name(&srmodels[at..at + 32]).unwrap());
        let files = u32_at(srmodels, at + 32).unwrap() as usize;
        at += 36 + files * 40;
    }
    names
}

fn moss() -> WakeTarget {
    WakeTarget::Custom(WakeWord::new("你好 MOSS", "ni hao mo si", 0.2).unwrap())
}

#[test]
fn n61_vendored_esp_sr_models_are_byte_exact() {
    // 与 78/xiaozhi-assets-generator@55517b4 web/public/static 下的文件逐字节相同（见 assets/esp-sr/README.md）
    for (data, expected) in [
        (
            MN6_DATA,
            "a2fbc1935df3032431f963c01a88bde725d0048e045eaf6b02d8f4495f428671",
        ),
        (
            MN6_INDEX,
            "cd57e5b2bcd364ca9bdd5b886590715d3bd81e179ac072c0959935abb4b98981",
        ),
        (
            MN6_INFO,
            "a292558227a95f415a0c0f3af1fdca639f5e87c4f51740161ec7997aed559651",
        ),
        (
            MN6_VOCAB,
            "7505578a7543455554a3d0a45cad7e2fd2f258097ba3b5f381d18d20d3f7ed51",
        ),
        (
            FST_CN,
            "a516f97ec1a826c7cd8396156857169aa3b73df122596f7854d90b23a15f2252",
        ),
        (
            FST_EN,
            "517b7df8f3471105ae638cd910f4d1c85f9ba7eab9e960aad2c6ca82713ad47e",
        ),
        (
            WN9_INFO,
            "058c04f027862ab745c056b8c8d896396b1cf871e0a8098537610f1a0e16a108",
        ),
        (
            WN9_DATA,
            "3bd2998013971a5999ca024745bf282381ce93c1ac79468a674abfe3d91399d8",
        ),
        (
            WN9_INDEX,
            "b463a5051d01bddc1f5723cddc8f3d5f11a6c750106694ca4aa0c8fdc235032c",
        ),
    ] {
        assert_eq!(sha(data), expected);
    }
}

#[test]
fn n61_package_matches_the_official_generator_byte_for_byte() {
    // 参照值由官方生成器的 SpiffsGenerator + WakenetModelPacker（Node 运行，输入同一份 index.json 字节）算出，
    // 证明 MOSS 打出的资源包与网页生成器格式完全一致。
    let package = build_package(None, &moss()).unwrap();
    assert_eq!(
        String::from_utf8(
            parse_assets(&package)
                .unwrap()
                .into_iter()
                .find(|e| e.name == "index.json")
                .unwrap()
                .data
        )
        .unwrap(),
        r#"{"multinet_model":{"commands":[{"action":"wake","command":"ni hao mo si","text":"你好 MOSS"}],"duration":3000,"language":"cn","threshold":0.2},"srmodels":"srmodels.bin","version":1}"#
    );
    assert_eq!(sha(&package), GENERATOR_REFERENCE_SHA256);
    let restore = build_package(None, &WakeTarget::Default).unwrap();
    assert_eq!(sha(&restore), GENERATOR_REFERENCE_DEFAULT_SHA256);
}

const GENERATOR_REFERENCE_SHA256: &str =
    "de987e7f7a7b6a8f175082af44426c583bc9f13a4f02ab4ca3dc5a65347765f1";
const GENERATOR_REFERENCE_DEFAULT_SHA256: &str =
    "9796ebe0d306b40d134b1354c01b4c7a9461726e2e0ffce4cc9b220e4abf339a";

#[test]
fn n61_parse_rejects_damaged_packages() {
    let good = pack_assets(&[entry("index.json", b"{}"), entry("a.bin", &[1, 2, 3])]).unwrap();
    assert_eq!(parse_assets(&good).unwrap().len(), 2);
    let mut bad_sum = good.clone();
    *bad_sum.last_mut().unwrap() ^= 0xFF;
    assert!(parse_assets(&bad_sum).is_err(), "校验和不对");
    let mut bad_len = good.clone();
    bad_len.push(0);
    assert!(parse_assets(&bad_len).is_err(), "长度与头部不符");
    assert!(parse_assets(&good[..10]).is_err());
    assert!(parse_assets(&[0u8; 12]).is_err(), "0 个文件");
    let dup = pack_assets(&[entry("a.bin", b"1"), entry("a.bin", b"2")]).unwrap();
    assert!(parse_assets(&dup).is_err(), "重名");
    assert!(
        pack_assets(&[entry(&"x".repeat(33), b"1")]).is_err(),
        "文件名超过 32 字节"
    );
}

#[test]
fn n61_custom_wake_word_package_without_base_holds_only_models_and_index() {
    let package = build_package(None, &moss()).unwrap();
    let entries = parse_assets(&package).unwrap();
    assert_eq!(
        entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ["srmodels.bin", "index.json"]
    );
    let srmodels = &entries[0].data;
    assert_eq!(
        model_names(srmodels),
        ["fst", "mn6_cn"],
        "MultiNet 存在时固件改走自定义唤醒词"
    );
    let index = index_of(&package);
    assert_eq!(
        index["multinet_model"]["commands"][0]["command"],
        "ni hao mo si"
    );
    assert_eq!(index["multinet_model"]["commands"][0]["action"], "wake");
    assert!(
        package.len() > SMALL_ASSETS_PARTITION_BYTES && package.len() < ASSETS_MAX_BYTES,
        "8 MB 闪存的 2 MB 资源分区装不下"
    );
}

#[test]
fn n61_base_package_keeps_fonts_emoji_and_skin_and_only_swaps_the_wake_word() {
    let base_index = json!({
        "version": 1, "chip_model": "esp32s3", "srmodels": "old_models.bin", "text_font": "font_puhui_16_4.bin",
        "emoji_collection": [{"name": "happy", "file": "happy.png"}], "skin": {"light": {"text_color": "#000000"}},
        "multinet_model": {"language": "cn", "commands": [{"command": "xiao ming", "text": "小明", "action": "wake"}]},
    });
    let base = pack_assets(&[
        entry("index.json", &serde_json::to_vec(&base_index).unwrap()),
        entry("old_models.bin", &[7; 100]),
        entry("font_puhui_16_4.bin", &[1; 50]),
        AssetEntry {
            name: "happy.png".into(),
            data: vec![2; 40],
            width: 64,
            height: 64,
        },
    ])
    .unwrap();
    assert_eq!(describe_base(&base).unwrap()["wake_word"], "小明");
    let package = build_package(Some(&base), &moss()).unwrap();
    let entries = parse_assets(&package).unwrap();
    let names: BTreeSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "index.json",
            "srmodels.bin",
            "font_puhui_16_4.bin",
            "happy.png"
        ]),
        "旧模型被替换，其余文件保留"
    );
    let png = entries.iter().find(|e| e.name == "happy.png").unwrap();
    assert_eq!(
        (png.width, png.height, png.data.len()),
        (64, 64, 40),
        "图片尺寸与内容原样保留"
    );
    let index = index_of(&package);
    assert_eq!(index["srmodels"], "srmodels.bin");
    assert_eq!(index["text_font"], base_index["text_font"]);
    assert_eq!(index["emoji_collection"], base_index["emoji_collection"]);
    assert_eq!(index["skin"], base_index["skin"]);
    assert_eq!(index["multinet_model"]["commands"][0]["text"], "你好 MOSS");

    let restore = build_package(Some(&package), &WakeTarget::Default).unwrap();
    let index = index_of(&restore);
    assert!(
        index.get("multinet_model").is_none(),
        "恢复默认时去掉自定义唤醒词配置"
    );
    assert_eq!(index["text_font"], base_index["text_font"]);
    let srmodels = parse_assets(&restore)
        .unwrap()
        .into_iter()
        .find(|e| e.name == "srmodels.bin")
        .unwrap()
        .data;
    assert_eq!(model_names(&srmodels), ["wn9_nihaoxiaozhi_tts"]);
}

#[test]
fn n61_base_package_must_be_a_valid_and_supported_assets_file() {
    assert!(build_package(Some(b"not a package"), &moss()).is_err());
    let no_index = pack_assets(&[entry("a.bin", b"1")]).unwrap();
    assert!(
        build_package(Some(&no_index), &moss())
            .unwrap_err()
            .contains("index.json")
    );
    let newer = pack_assets(&[entry("index.json", br#"{"version":2}"#)]).unwrap();
    assert!(
        build_package(Some(&newer), &moss())
            .unwrap_err()
            .contains("版本")
    );
    let huge = pack_assets(&[
        entry("index.json", b"{}"),
        entry("big.bin", &vec![0; ASSETS_MAX_BYTES - 3 * 1024 * 1024]),
    ])
    .unwrap();
    assert!(
        build_package(Some(&huge), &moss())
            .unwrap_err()
            .contains("8 MB"),
        "加上模型超过 8 MB 时拒绝"
    );
}

#[test]
fn n61_wake_word_input_is_checked_against_the_model_pinyin() {
    let syllables = pinyin_syllables();
    assert!(syllables.len() > 350 && syllables.contains("mo") && syllables.contains("si"));
    assert_eq!(
        normalize_command("  Ni HAO   mo si ").unwrap(),
        "ni hao mo si"
    );
    assert_eq!(normalize_command("mo si").unwrap(), "mo si");
    assert!(normalize_command("mo").is_err(), "一个音节太短");
    assert!(normalize_command("moss").is_err(), "不是拼音音节");
    assert!(normalize_command("mo si 1").is_err());
    assert!(normalize_command("mó sī").is_err(), "不带声调");
    assert!(
        normalize_command("ni hao mo si mo si mo").is_err(),
        "超过 6 个音节"
    );
    assert_eq!(normalize_text("  你好   MOSS ").unwrap(), "你好 MOSS");
    assert!(
        normalize_text("").is_err()
            && normalize_text(&"字".repeat(17)).is_err()
            && normalize_text("a\u{7}").is_err()
    );
    assert_eq!(normalize_threshold(0.234).unwrap(), 0.23);
    assert!(
        normalize_threshold(0.0).is_err()
            && normalize_threshold(1.0).is_err()
            && normalize_threshold(f64::NAN).is_err()
    );
}

#[test]
fn n61_staged_package_is_one_device_one_token_and_expires() {
    let store = AssetsStore::default();
    let first = store.stage("dev-a", vec![1, 2, 3]).unwrap();
    assert!(crate::firmware::token_ok(&first));
    assert_eq!(
        store.fetch(&first).map(|(d, b)| (d, b.as_ref().clone())),
        Some(("dev-a".into(), vec![1, 2, 3]))
    );
    let other = store.stage("dev-b", vec![9]).unwrap();
    let second = store.stage("dev-a", vec![4]).unwrap();
    assert!(
        store.fetch(&first).is_none(),
        "同一台设备的新推送顶掉旧令牌"
    );
    assert!(store.fetch(&second).is_some() && store.fetch(&other).is_some());
    assert!(store.fetch("../etc/passwd").is_none() && store.fetch(&"0".repeat(32)).is_none());
    store
        .staged
        .lock()
        .unwrap()
        .get_mut(&second)
        .unwrap()
        .expires = Instant::now() - Duration::from_secs(1);
    assert!(store.fetch(&second).is_none(), "过期后不再给");
}

#[test]
fn n84_base_pack_fingerprint_detects_font_and_asset_changes_while_ignoring_wake_word() {
    let none_fp = fingerprint_base_pack(None);
    assert!(!none_fp.has_text_font);

    let base_v1 = pack_assets(&[
        entry("font.bin", &[1, 2, 3]),
        entry("srmodels.bin", &[9, 9]),
        entry(
            "index.json",
            r#"{"version":1,"text_font":"font.bin","srmodels":"srmodels.bin","multinet_model":{"commands":[{"text":"旧词"}]}}"#
                .as_bytes(),
        ),
    ])
    .unwrap();
    let fp_v1 = fingerprint_base_pack(Some(&base_v1));
    assert!(fp_v1.has_text_font);
    assert_ne!(fp_v1.digest, none_fp.digest);

    // 仅唤醒模型或 multinet_model 不同、字体与其余资源一致时，视觉/字库指纹不变
    let base_same_visual = pack_assets(&[
        entry("font.bin", &[1, 2, 3]),
        entry("srmodels.bin", &[7, 7, 7, 7]),
        entry(
            "index.json",
            br#"{"srmodels":"srmodels.bin","text_font":"font.bin","version":1}"#,
        ),
    ])
    .unwrap();
    assert_eq!(fingerprint_base_pack(Some(&base_same_visual)), fp_v1);

    // 字体字节或额外表情变化时，指纹立即改变
    let base_font_modified = pack_assets(&[
        entry("font.bin", &[1, 2, 4]),
        entry(
            "index.json",
            br#"{"version":1,"text_font":"font.bin","srmodels":"srmodels.bin"}"#,
        ),
    ])
    .unwrap();
    assert_ne!(
        fingerprint_base_pack(Some(&base_font_modified)).digest,
        fp_v1.digest
    );
}

#[test]
fn parse_assets_rejects_overlapping_entries_exceeding_cumulative_limit() {
    let payload = vec![0xABu8; 2 * 1024 * 1024];
    let count = 5usize; // 5 * 2 MiB = 10 MiB > ASSETS_MAX_BYTES (8 MiB)，但整包仅约 2 MiB
    let row_len = 32 + 12;
    let table_len = count * row_len;
    let mut body = vec![0u8; table_len + 2 + payload.len()];
    for i in 0..count {
        let row = i * row_len;
        let name = format!("dup_{i}.bin");
        body[row..row + name.len()].copy_from_slice(name.as_bytes());
        body[row + 32..row + 36].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        body[row + 36..row + 40].copy_from_slice(&0u32.to_le_bytes());
    }
    body[table_len] = 0x5A;
    body[table_len + 1] = 0x5A;
    body[table_len + 2..].copy_from_slice(&payload);

    let sum = super::checksum(&body);
    let mut raw = Vec::with_capacity(12 + body.len());
    raw.extend_from_slice(&(count as u32).to_le_bytes());
    raw.extend_from_slice(&sum.to_le_bytes());
    raw.extend_from_slice(&(body.len() as u32).to_le_bytes());
    raw.extend_from_slice(&body);

    assert!(raw.len() < ASSETS_MAX_BYTES);
    let err = parse_assets(&raw).expect_err("重叠目录项累计超过 8 MB 时应拒绝放大分配");
    assert_eq!(err, "资源包里的文件数据损坏");
}
