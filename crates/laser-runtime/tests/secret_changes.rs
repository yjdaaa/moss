use laser_runtime::db::RuntimeDb;

/// 设置与密钥分存两处：密钥批量变更必须整批生效，并返回原值供后续步骤失败时还原（2026-10-13 审查，NV11-4）。
#[test]
fn secret_changes_apply_together_and_can_be_restored() {
    let db = RuntimeDb::open_memory().unwrap();
    db.set_secret("voice.plugin.weather", "old-weather", 1.).unwrap();
    db.set_secret("voice.plugin.rag", "old-rag", 1.).unwrap();

    let changes = vec![
        ("voice.plugin.weather".to_owned(), Some("new-weather".to_owned())),
        ("voice.plugin.rag".to_owned(), None),
        ("voice.plugin.search".to_owned(), Some("new-search".to_owned())),
    ];
    let previous = db.apply_secret_changes(&changes, 2.).unwrap();
    assert_eq!(db.get_secret("voice.plugin.weather").unwrap().as_deref(), Some("new-weather"));
    assert_eq!(db.get_secret("voice.plugin.rag").unwrap(), None);
    assert_eq!(db.get_secret("voice.plugin.search").unwrap().as_deref(), Some("new-search"));
    assert_eq!(
        previous,
        vec![
            ("voice.plugin.weather".to_owned(), Some("old-weather".to_owned())),
            ("voice.plugin.rag".to_owned(), Some("old-rag".to_owned())),
            ("voice.plugin.search".to_owned(), None),
        ]
    );

    // 还原：原本不存在的项被删除，被删或被改的项恢复原值
    db.restore_secrets(&previous, 3.).unwrap();
    assert_eq!(db.get_secret("voice.plugin.weather").unwrap().as_deref(), Some("old-weather"));
    assert_eq!(db.get_secret("voice.plugin.rag").unwrap().as_deref(), Some("old-rag"));
    assert_eq!(db.get_secret("voice.plugin.search").unwrap(), None);
}

/// 同一名称在一批里出现多次时，还原后回到批次开始前的值。
#[test]
fn restoring_a_batch_with_repeated_names_returns_to_the_original_value() {
    let db = RuntimeDb::open_memory().unwrap();
    db.set_secret("voice.vision.main", "original", 1.).unwrap();
    let changes = vec![
        ("voice.vision.main".to_owned(), Some("first".to_owned())),
        ("voice.vision.main".to_owned(), None),
    ];
    let previous = db.apply_secret_changes(&changes, 2.).unwrap();
    assert_eq!(db.get_secret("voice.vision.main").unwrap(), None);
    db.restore_secrets(&previous, 3.).unwrap();
    assert_eq!(db.get_secret("voice.vision.main").unwrap().as_deref(), Some("original"));
}
