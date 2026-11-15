//! ESP-IDF 分区表与 NVS v2 分区只读解析、修改与重构工具（纯字节操作，无 I/O）。

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use serde::{Deserialize, Serialize};

pub const PARTITION_TABLE_OFFSET: u32 = 0x8000;
pub const PARTITION_TABLE_READ_SIZE: u32 = 0x1000;
pub const PARTITION_TABLE_SIZE: usize = 0xC00;
const PARTITION_ENTRY_SIZE: usize = 32;
const PARTITION_MAGIC: [u8; 2] = [0xAA, 0x50];
const PARTITION_TYPE_DATA: u8 = 0x01;
const PARTITION_SUBTYPE_NVS: u8 = 0x02;
const PARTITION_SUBTYPE_NVS_KEYS: u8 = 0x04;
const PARTITION_FLAG_ENCRYPTED: u32 = 1;

pub const NVS_PAGE_SIZE: usize = 4096;
const NVS_HEADER_SIZE: usize = 32;
const NVS_BITMAP_SIZE: usize = 32;
const NVS_ENTRY_SIZE: usize = 32;
const NVS_ENTRIES_PER_PAGE: usize = 126;
const NVS_DATA_OFFSET: usize = NVS_HEADER_SIZE + NVS_BITMAP_SIZE;
const NVS_VERSION_V2: u8 = 0xFE;
const NVS_CHUNK_MAX_SIZE: usize = 4000;
const NVS_STR_MAX_SIZE: usize = 4000;
const NVS_KEY_MAX_BYTES: usize = 15;
const NVS_WIFI_MAX_SLOTS: usize = 10;
const NVS_SSID_MAX_BYTES: usize = 32;
const NVS_PASS_MAX_BYTES: usize = 64;
const NVS_OTA_URL_MAX_BYTES: usize = 255;
const NVS_SLEEP_MAX_MIN: i32 = 1440;
/// 分区表固定在 0x8000、长 0x1000：数据分区最早从 0x9000 开始
const MIN_DATA_OFFSET: u32 = 0x9000;
pub const ALLOWED_TX_POWERS: [i8; 11] = [8, 20, 28, 34, 44, 52, 56, 60, 66, 72, 80];

const PAGE_STATE_EMPTY: u32 = 0xFFFF_FFFF;
const PAGE_STATE_ACTIVE: u32 = 0xFFFF_FFFE;
const PAGE_STATE_FULL: u32 = 0xFFFF_FFFC;
const PAGE_STATE_FREEING: u32 = 0xFFFF_FFF8;

const BITMAP_WRITTEN: u8 = 0b10;
const BITMAP_EMPTY: u8 = 0b11;
const BITMAP_ERASED: u8 = 0b00;

const TYPE_U8: u8 = 0x01;
const TYPE_I8: u8 = 0x11;
const TYPE_U16: u8 = 0x02;
const TYPE_I16: u8 = 0x12;
const TYPE_U32: u8 = 0x04;
const TYPE_I32: u8 = 0x14;
const TYPE_U64: u8 = 0x08;
const TYPE_I64: u8 = 0x18;
const TYPE_SZ: u8 = 0x21;
const TYPE_BLOB_V1: u8 = 0x41;
const TYPE_BLOB_DATA: u8 = 0x42;
const TYPE_BLOB_INDEX: u8 = 0x48;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NvsError {
    InvalidPartitionTable(String),
    EncryptedNvs(String),
    WrongDevicePartition(String),
    InvalidNvs(String),
    CapacityExceeded(String),
    Validation(String),
}

impl fmt::Display for NvsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPartitionTable(m)
            | Self::EncryptedNvs(m)
            | Self::WrongDevicePartition(m)
            | Self::InvalidNvs(m)
            | Self::CapacityExceeded(m)
            | Self::Validation(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for NvsError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionEntry {
    pub ptype: u8,
    pub subtype: u8,
    pub offset: u32,
    pub size: u32,
    pub label: String,
    pub encrypted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionSummary {
    pub nvs: Option<PartitionEntry>,
    /// 选中的 NVS 分区与分区表里别的分区重叠（异常或被篡改的分区表）
    #[serde(default)]
    pub nvs_overlaps: bool,
    pub has_nvs_keys: bool,
    pub has_fluidnc_markers: bool,
    pub has_xiaozhi_markers: bool,
    pub all_labels: Vec<String>,
}

impl PartitionSummary {
    pub fn require_writable_nvs(&self) -> Result<&PartitionEntry, NvsError> {
        if self.has_nvs_keys {
            return Err(NvsError::EncryptedNvs(
                "检测到 nvs_keys 加密密钥分区，设备启用了 NVS Flash 加密，不能通过 USB 明文读写"
                    .into(),
            ));
        }
        let nvs = self
            .nvs
            .as_ref()
            .ok_or_else(|| NvsError::InvalidPartitionTable("分区表中未找到 NVS 分区".into()))?;
        if nvs.encrypted {
            return Err(NvsError::EncryptedNvs(
                "该 NVS 分区已开启加密标志，不能通过 USB 明文读写".into(),
            ));
        }
        if self.has_fluidnc_markers {
            return Err(NvsError::WrongDevicePartition(
                "检测到激光控制器/非小智 SPIFFS 分区特征（localfs/fluidnc/spiffs/littlefs/ffat），已阻止写 NVS"
                    .into(),
            ));
        }
        if !(nvs.size as usize).is_multiple_of(NVS_PAGE_SIZE)
            || (nvs.size as usize) < NVS_PAGE_SIZE * 2
        {
            return Err(NvsError::InvalidNvs(format!(
                "NVS 分区大小异常（0x{:x} 字节，需为至少 8 KiB 且按 4 KiB 对齐）",
                nvs.size
            )));
        }
        // 擦写前的边界兜底：起点按 4 KiB 扇区对齐、不早于分区表之后（0x9000）、不与其他分区重叠。
        // 正规分区表都满足；不满足说明分区表异常，擦写可能落到别的区域，一律拒绝。
        if !(nvs.offset as usize).is_multiple_of(NVS_PAGE_SIZE)
            || nvs.offset < MIN_DATA_OFFSET
            || self.nvs_overlaps
        {
            return Err(NvsError::InvalidPartitionTable(format!(
                "NVS 分区位置异常（起点 0x{:x}，需按 4 KiB 对齐、不早于 0x{MIN_DATA_OFFSET:x} 且不与其他分区重叠），已拒绝写入",
                nvs.offset
            )));
        }
        Ok(nvs)
    }

    /// 同 [`require_writable_nvs`](Self::require_writable_nvs)，并要求 NVS 分区整个落在芯片 Flash 容量之内。
    pub fn require_writable_nvs_within(
        &self,
        flash_bytes: Option<u64>,
    ) -> Result<&PartitionEntry, NvsError> {
        let nvs = self.require_writable_nvs()?;
        if let Some(limit) = flash_bytes
            && u64::from(nvs.offset) + u64::from(nvs.size) > limit
        {
            return Err(NvsError::InvalidPartitionTable(format!(
                "NVS 分区（0x{:x} 起 {} 字节）超出芯片 Flash 容量 {limit} 字节，已拒绝写入",
                nvs.offset, nvs.size
            )));
        }
        Ok(nvs)
    }
}

pub fn parse_partition_table(raw: &[u8]) -> Result<PartitionSummary, NvsError> {
    if raw.len() < PARTITION_ENTRY_SIZE {
        return Err(NvsError::InvalidPartitionTable("分区表数据长度不足".into()));
    }
    let limit = raw.len().min(PARTITION_TABLE_SIZE);
    let mut entries = Vec::new();
    for chunk in raw[..limit].as_chunks::<PARTITION_ENTRY_SIZE>().0 {
        if chunk[0..2] != PARTITION_MAGIC {
            break;
        }
        let ptype = chunk[2];
        let subtype = chunk[3];
        let offset = u32::from_le_bytes(chunk[4..8].try_into().unwrap_or([0; 4]));
        let size = u32::from_le_bytes(chunk[8..12].try_into().unwrap_or([0; 4]));
        let label_end = chunk[12..28].iter().position(|&b| b == 0).unwrap_or(16);
        let label = String::from_utf8_lossy(&chunk[12..12 + label_end])
            .trim()
            .to_string();
        let flags = u32::from_le_bytes(chunk[28..32].try_into().unwrap_or([0; 4]));
        entries.push(PartitionEntry {
            ptype,
            subtype,
            offset,
            size,
            label,
            encrypted: (flags & PARTITION_FLAG_ENCRYPTED) != 0,
        });
    }
    if entries.is_empty() {
        return Err(NvsError::InvalidPartitionTable(
            "0x8000 处未找到有效 ESP-IDF 分区表".into(),
        ));
    }

    let has_nvs_keys = entries
        .iter()
        .any(|e| e.ptype == PARTITION_TYPE_DATA && e.subtype == PARTITION_SUBTYPE_NVS_KEYS);
    let mut nvs = None;
    let mut first_nvs = None;
    let mut all_labels = Vec::with_capacity(entries.len());
    let mut has_fluidnc_markers = false;
    let mut has_xiaozhi_markers = false;

    let bounds: Vec<(u32, u32, String)> = entries
        .iter()
        .map(|e| (e.offset, e.size, e.label.clone()))
        .collect();
    for e in entries {
        let lower = e.label.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "localfs" | "fluidnc" | "spiffs" | "littlefs" | "ffat"
        ) {
            has_fluidnc_markers = true;
        }
        if matches!(lower.as_str(), "model" | "assets") || lower.starts_with("assets_") {
            has_xiaozhi_markers = true;
        }
        all_labels.push(e.label.clone());
        if e.ptype == PARTITION_TYPE_DATA && e.subtype == PARTITION_SUBTYPE_NVS {
            if lower == "nvs" && nvs.is_none() {
                nvs = Some(e);
            } else if first_nvs.is_none() {
                first_nvs = Some(e);
            }
        }
    }

    let nvs = nvs.or(first_nvs);
    let nvs_overlaps = nvs.as_ref().is_some_and(|target| {
        let (start, end) = (
            u64::from(target.offset),
            u64::from(target.offset) + u64::from(target.size),
        );
        bounds.iter().any(|&(offset, size, ref label)| {
            let (other_start, other_end) = (u64::from(offset), u64::from(offset) + u64::from(size));
            !(offset == target.offset && size == target.size && *label == target.label)
                && other_start < end
                && start < other_end
        })
    });
    Ok(PartitionSummary {
        nvs,
        nvs_overlaps,
        has_nvs_keys,
        has_fluidnc_markers,
        has_xiaozhi_markers,
        all_labels,
    })
}

/// 条目里的键名 / 命名空间名（以 NUL 结尾，最长 15 字节）。不是合法 UTF-8 的一律拒绝：
/// 有损解码会把它改成别的名字写回去，而写前自检比较的是两边都改过的结果，查不出来。
fn entry_key(entry: &[u8]) -> Result<String, NvsError> {
    let key_len = entry[8..24].iter().position(|&b| b == 0).unwrap_or(16);
    std::str::from_utf8(&entry[8..8 + key_len])
        .map(str::to_owned)
        .map_err(|_| {
            NvsError::InvalidNvs(
                "NVS 里有无法识别的键名（不是合法 UTF-8），为免改坏配置已拒绝读写".into(),
            )
        })
}

/// 与小智固件所用 `esp-wifi-connect` 组件的 `wifi_config::IsValidOtaUrl` 同一套规则（`ota_url.cc`）：
/// 固件遇到不合规的地址会直接忽略、退回官方默认 OTA，写进去等于没写。
/// 规则：≤ 255 字节；`http://` 或 `https://` 开头（不区分大小写）；只允许 0x21–0x7E 的可见字符且不含 `\`；
/// `%` 后必须跟两位十六进制；主机名不能为空；写了端口时须为 1–65535。
pub fn is_valid_ota_url(url: &str) -> bool {
    let bytes = url.as_bytes();
    if url.is_empty() {
        return true;
    }
    let lower = url.to_ascii_lowercase();
    let rest = if let Some(rest) = lower.strip_prefix("http://") {
        rest
    } else if let Some(rest) = lower.strip_prefix("https://") {
        rest
    } else {
        return false;
    };
    if bytes.len() > NVS_OTA_URL_MAX_BYTES {
        return false;
    }
    for (i, &c) in bytes.iter().enumerate() {
        if c <= b' ' || c >= 0x7f || c == b'\\' {
            return false;
        }
        if c == b'%'
            && !(i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit())
        {
            return false;
        }
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let (host, port) = if let Some(v6) = host_port.strip_prefix('[') {
        match v6.split_once(']') {
            Some((host, tail)) => (host, tail.strip_prefix(':')),
            None => return false,
        }
    } else {
        match host_port.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        }
    };
    if host.is_empty() {
        return false;
    }
    match port {
        None => true,
        Some(port) => port.parse::<u32>().is_ok_and(|p| (1..=65_535).contains(&p)),
    }
}

/// ESP-IDF `nvs_item_hash_list` / `nvs_partition_gen.py` 所用的带初值 CRC32（`init = 0xFFFFFFFF`）。
pub fn nvs_crc32(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new_with_initial(0xFFFF_FFFF);
    hasher.update(data);
    hasher.finalize()
}

pub fn format_uuid_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NvsValue {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    Str(String),
    Blob(Vec<u8>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NvsStore {
    pub entries: BTreeMap<(String, String), NvsValue>,
}

#[derive(Debug, Clone, Copy)]
struct BlobIndexMeta {
    total_size: u32,
    chunk_count: u8,
    chunk_start: u8,
}

impl NvsStore {
    pub fn get_str(&self, ns: &str, key: &str) -> Option<&str> {
        match self.entries.get(&(ns.to_string(), key.to_string())) {
            Some(NvsValue::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn get_u8(&self, ns: &str, key: &str) -> Option<u8> {
        match self.entries.get(&(ns.to_string(), key.to_string())) {
            Some(NvsValue::U8(v)) => Some(*v),
            _ => None,
        }
    }

    pub fn get_i8(&self, ns: &str, key: &str) -> Option<i8> {
        match self.entries.get(&(ns.to_string(), key.to_string())) {
            Some(NvsValue::I8(v)) => Some(*v),
            _ => None,
        }
    }

    pub fn get_i32(&self, ns: &str, key: &str) -> Option<i32> {
        match self.entries.get(&(ns.to_string(), key.to_string())) {
            Some(NvsValue::I32(v)) => Some(*v),
            _ => None,
        }
    }

    pub fn set(&mut self, ns: &str, key: &str, val: NvsValue) -> Result<(), NvsError> {
        validate_ident(ns, "命名空间")?;
        validate_ident(key, "键名")?;
        if let NvsValue::Str(s) = &val
            && s.len() + 1 > NVS_STR_MAX_SIZE
        {
            return Err(NvsError::Validation(format!(
                "字符串 {ns}:{key} 超出 NVS 上限（{} > {} 字节）",
                s.len() + 1,
                NVS_STR_MAX_SIZE
            )));
        }
        self.entries.insert((ns.to_string(), key.to_string()), val);
        Ok(())
    }

    pub fn remove(&mut self, ns: &str, key: &str) {
        self.entries.remove(&(ns.to_string(), key.to_string()));
    }

    pub fn has_namespace(&self, ns: &str) -> bool {
        self.entries.keys().any(|(namespace, _)| namespace == ns)
    }

    pub fn parse(raw: &[u8]) -> Result<Self, NvsError> {
        if raw.len() < NVS_PAGE_SIZE * 2 || !raw.len().is_multiple_of(NVS_PAGE_SIZE) {
            return Err(NvsError::InvalidNvs(format!(
                "NVS 分区长度无效（{} 字节，需为 >= 8192 且为 4096 整数倍）",
                raw.len()
            )));
        }

        let mut pages: Vec<(u32, &[u8])> = Vec::new();
        for (page_idx, page) in raw.as_chunks::<NVS_PAGE_SIZE>().0.iter().enumerate() {
            let state = u32::from_le_bytes(page[0..4].try_into().unwrap_or([0; 4]));
            if state == PAGE_STATE_EMPTY {
                continue;
            }
            if state != PAGE_STATE_ACTIVE && state != PAGE_STATE_FULL && state != PAGE_STATE_FREEING
            {
                return Err(NvsError::InvalidNvs(format!(
                    "第 {page_idx} 页页状态异常（0x{state:08x}）"
                )));
            }
            if page[8] != NVS_VERSION_V2 {
                return Err(NvsError::InvalidNvs(format!(
                    "仅支持 NVS v2（第 {page_idx} 页版本字节为 0x{:02x}）",
                    page[8]
                )));
            }
            let expected_crc = u32::from_le_bytes(page[28..32].try_into().unwrap_or([0; 4]));
            let actual_crc = nvs_crc32(&page[4..28]);
            if expected_crc != actual_crc {
                return Err(NvsError::InvalidNvs(format!(
                    "第 {page_idx} 页页头 CRC32 校验失败"
                )));
            }
            let seq_no = u32::from_le_bytes(page[4..8].try_into().unwrap_or([0; 4]));
            pages.push((seq_no, page));
        }

        pages.sort_by_key(|(seq, _)| *seq);

        let mut namespaces: HashMap<u8, String> = HashMap::new();
        // 第一遍收集全分区命名空间定义：磨损均衡回收旧页后，ns_idx=0 可能被搬到高 seq_no 页，
        // 若单遍扫页会在更早的 FULL 页误报“引用了未定义命名空间”。
        for &(seq, page) in &pages {
            collect_page_namespaces(seq, page, &mut namespaces)?;
        }

        let mut entries: BTreeMap<(String, String), NvsValue> = BTreeMap::new();
        let mut blob_chunks: HashMap<(u8, String, u8), Vec<u8>> = HashMap::new();
        let mut blob_indices: HashMap<(u8, String), BlobIndexMeta> = HashMap::new();

        for (seq, page) in pages {
            parse_page(
                seq,
                page,
                &namespaces,
                &mut entries,
                &mut blob_chunks,
                &mut blob_indices,
            )?;
        }

        for ((ns_idx, key), meta) in blob_indices {
            let Some(ns_name) = namespaces.get(&ns_idx) else {
                continue;
            };
            let mut combined = Vec::with_capacity(meta.total_size as usize);
            for offset in 0..meta.chunk_count {
                let chunk_idx = meta.chunk_start.wrapping_add(offset);
                let chunk = blob_chunks
                    .get(&(ns_idx, key.clone(), chunk_idx))
                    .ok_or_else(|| {
                        NvsError::InvalidNvs(format!(
                            "Blob 分片缺失：{ns_name}:{key} chunk={chunk_idx}"
                        ))
                    })?;
                combined.extend_from_slice(chunk);
            }
            if combined.len() != meta.total_size as usize {
                return Err(NvsError::InvalidNvs(format!(
                    "Blob 长度不匹配：{ns_name}:{key} 期望 {} 实际 {}",
                    meta.total_size,
                    combined.len()
                )));
            }
            entries.insert((ns_name.clone(), key), NvsValue::Blob(combined));
        }

        Ok(Self { entries })
    }

    pub fn build(&self, partition_size: usize) -> Result<Vec<u8>, NvsError> {
        if partition_size < NVS_PAGE_SIZE * 2 || !partition_size.is_multiple_of(NVS_PAGE_SIZE) {
            return Err(NvsError::InvalidNvs(format!(
                "目标 NVS 分区大小无效（{partition_size} 字节）"
            )));
        }
        let total_pages = partition_size / NVS_PAGE_SIZE;
        let max_data_pages = total_pages - 1;

        let mut items: Vec<EncodedItem> = Vec::new();
        let mut ns_ids: HashMap<&str, u8> = HashMap::new();
        let mut next_ns_id: u8 = 1;

        for ((ns, key), value) in &self.entries {
            validate_ident(ns, "命名空间")?;
            validate_ident(key, "键名")?;
            let ns_id = if let Some(&id) = ns_ids.get(ns.as_str()) {
                id
            } else {
                if next_ns_id == u8::MAX {
                    return Err(NvsError::CapacityExceeded(
                        "NVS 命名空间数量超出 254 上限".into(),
                    ));
                }
                let id = next_ns_id;
                next_ns_id += 1;
                ns_ids.insert(ns.as_str(), id);
                items.push(EncodedItem::namespace(ns, id));
                id
            };
            encode_value_items(ns_id, key, value, &mut items)?;
        }

        let mut pages: Vec<PageBuilder> = vec![PageBuilder::new(0)];
        for item in items {
            if item.span > NVS_ENTRIES_PER_PAGE {
                return Err(NvsError::CapacityExceeded(
                    "单条 NVS 数据超出单页最大槽位数".into(),
                ));
            }
            if pages.last().unwrap().free_slots() < item.span {
                if pages.len() >= max_data_pages {
                    return Err(NvsError::CapacityExceeded(
                        "NVS 分区空间不足，需至少保留 1 个空白磨损均衡页".into(),
                    ));
                }
                let last = pages.last_mut().unwrap();
                last.state = PAGE_STATE_FULL;
                let next_seq = pages.len() as u32;
                pages.push(PageBuilder::new(next_seq));
            }
            pages.last_mut().unwrap().push(&item);
        }

        let mut out = vec![0xFFu8; partition_size];
        for (idx, page) in pages.into_iter().enumerate() {
            let start = idx * NVS_PAGE_SIZE;
            page.write_into(&mut out[start..start + NVS_PAGE_SIZE]);
        }

        let verified = Self::parse(&out)?;
        if verified.entries != self.entries {
            return Err(NvsError::InvalidNvs(
                "NVS 镜像写入前自检回环不一致，已中止生成".into(),
            ));
        }
        Ok(out)
    }
}

fn validate_ident(name: &str, label: &str) -> Result<(), NvsError> {
    if name.is_empty() || name.len() > NVS_KEY_MAX_BYTES {
        return Err(NvsError::Validation(format!(
            "{label}“{name}”长度必须为 1..={NVS_KEY_MAX_BYTES} 字节"
        )));
    }
    if name.as_bytes().contains(&0) {
        return Err(NvsError::Validation(format!("{label}不能包含 NUL 字节")));
    }
    Ok(())
}

fn collect_page_namespaces(
    seq: u32,
    page: &[u8],
    namespaces: &mut HashMap<u8, String>,
) -> Result<(), NvsError> {
    let mut slot = 0usize;
    while slot < NVS_ENTRIES_PER_PAGE {
        let state = bitmap_state(page, slot);
        if state == BITMAP_EMPTY || state == BITMAP_ERASED {
            slot += 1;
            continue;
        }
        if state != BITMAP_WRITTEN {
            return Err(NvsError::InvalidNvs(format!(
                "页 seq={seq} 槽位 {slot} 位图状态异常（0b{state:02b}）"
            )));
        }
        let off = NVS_DATA_OFFSET + slot * NVS_ENTRY_SIZE;
        let entry = &page[off..off + NVS_ENTRY_SIZE];
        let span = (entry[2] as usize).max(1);
        if slot + span > NVS_ENTRIES_PER_PAGE {
            return Err(NvsError::InvalidNvs(format!(
                "页 seq={seq} 槽位 {slot} span={span} 越界"
            )));
        }
        if !verify_entry_crc(entry) {
            return Err(NvsError::InvalidNvs(format!(
                "页 seq={seq} 槽位 {slot} 条目 CRC32 校验失败"
            )));
        }
        let ns_idx = entry[0];
        let dtype = entry[1];
        if ns_idx == 0 && dtype == TYPE_U8 {
            let key = entry_key(entry)?;
            namespaces.insert(entry[24], key);
        }
        slot += span;
    }
    Ok(())
}

fn clear_chunked_blob(
    ns_idx: u8,
    key: &str,
    blob_indices: &mut HashMap<(u8, String), BlobIndexMeta>,
    blob_chunks: &mut HashMap<(u8, String, u8), Vec<u8>>,
) {
    let owned_key = key.to_string();
    blob_indices.remove(&(ns_idx, owned_key));
    blob_chunks.retain(|(existing_ns, existing_key, _), _| {
        !(*existing_ns == ns_idx && existing_key == key)
    });
}

fn parse_page(
    seq: u32,
    page: &[u8],
    namespaces: &HashMap<u8, String>,
    entries: &mut BTreeMap<(String, String), NvsValue>,
    blob_chunks: &mut HashMap<(u8, String, u8), Vec<u8>>,
    blob_indices: &mut HashMap<(u8, String), BlobIndexMeta>,
) -> Result<(), NvsError> {
    let mut slot = 0usize;
    while slot < NVS_ENTRIES_PER_PAGE {
        let state = bitmap_state(page, slot);
        if state == BITMAP_EMPTY || state == BITMAP_ERASED {
            slot += 1;
            continue;
        }
        if state != BITMAP_WRITTEN {
            return Err(NvsError::InvalidNvs(format!(
                "页 seq={seq} 槽位 {slot} 位图状态异常（0b{state:02b}）"
            )));
        }
        let off = NVS_DATA_OFFSET + slot * NVS_ENTRY_SIZE;
        let entry = &page[off..off + NVS_ENTRY_SIZE];
        let span = (entry[2] as usize).max(1);
        if slot + span > NVS_ENTRIES_PER_PAGE {
            return Err(NvsError::InvalidNvs(format!(
                "页 seq={seq} 槽位 {slot} span={span} 越界"
            )));
        }
        if !verify_entry_crc(entry) {
            return Err(NvsError::InvalidNvs(format!(
                "页 seq={seq} 槽位 {slot} 条目 CRC32 校验失败"
            )));
        }

        let ns_idx = entry[0];
        let dtype = entry[1];
        let chunk_idx = entry[3];
        let key = entry_key(entry)?;
        let data = &entry[24..32];

        if ns_idx == 0 {
            slot += span;
            continue;
        }

        let Some(ns_name) = namespaces.get(&ns_idx).cloned() else {
            return Err(NvsError::InvalidNvs(format!(
                "页 seq={seq} 槽位 {slot} 引用了未定义命名空间 {ns_idx}"
            )));
        };

        match dtype {
            TYPE_U8 => {
                clear_chunked_blob(ns_idx, &key, blob_indices, blob_chunks);
                entries.insert((ns_name, key), NvsValue::U8(data[0]));
            }
            TYPE_I8 => {
                clear_chunked_blob(ns_idx, &key, blob_indices, blob_chunks);
                entries.insert((ns_name, key), NvsValue::I8(data[0] as i8));
            }
            TYPE_U16 => {
                clear_chunked_blob(ns_idx, &key, blob_indices, blob_chunks);
                entries.insert(
                    (ns_name, key),
                    NvsValue::U16(u16::from_le_bytes(data[0..2].try_into().unwrap())),
                );
            }
            TYPE_I16 => {
                clear_chunked_blob(ns_idx, &key, blob_indices, blob_chunks);
                entries.insert(
                    (ns_name, key),
                    NvsValue::I16(i16::from_le_bytes(data[0..2].try_into().unwrap())),
                );
            }
            TYPE_U32 => {
                clear_chunked_blob(ns_idx, &key, blob_indices, blob_chunks);
                entries.insert(
                    (ns_name, key),
                    NvsValue::U32(u32::from_le_bytes(data[0..4].try_into().unwrap())),
                );
            }
            TYPE_I32 => {
                clear_chunked_blob(ns_idx, &key, blob_indices, blob_chunks);
                entries.insert(
                    (ns_name, key),
                    NvsValue::I32(i32::from_le_bytes(data[0..4].try_into().unwrap())),
                );
            }
            TYPE_U64 => {
                clear_chunked_blob(ns_idx, &key, blob_indices, blob_chunks);
                entries.insert(
                    (ns_name, key),
                    NvsValue::U64(u64::from_le_bytes(data[0..8].try_into().unwrap())),
                );
            }
            TYPE_I64 => {
                clear_chunked_blob(ns_idx, &key, blob_indices, blob_chunks);
                entries.insert(
                    (ns_name, key),
                    NvsValue::I64(i64::from_le_bytes(data[0..8].try_into().unwrap())),
                );
            }
            TYPE_SZ => {
                let raw = read_varlen_payload(page, slot, span, data, &ns_name, &key)?;
                let trimmed = match raw.last() {
                    Some(&0) => &raw[..raw.len() - 1],
                    _ => raw.as_slice(),
                };
                let text = String::from_utf8(trimmed.to_vec()).map_err(|_| {
                    NvsError::InvalidNvs(format!("字符串 {ns_name}:{key} 不是有效 UTF-8"))
                })?;
                clear_chunked_blob(ns_idx, &key, blob_indices, blob_chunks);
                entries.insert((ns_name, key), NvsValue::Str(text));
            }
            TYPE_BLOB_V1 => {
                let raw = read_varlen_payload(page, slot, span, data, &ns_name, &key)?;
                clear_chunked_blob(ns_idx, &key, blob_indices, blob_chunks);
                entries.insert((ns_name, key), NvsValue::Blob(raw));
            }
            TYPE_BLOB_DATA => {
                let raw = read_varlen_payload(page, slot, span, data, &ns_name, &key)?;
                blob_chunks.insert((ns_idx, key, chunk_idx), raw);
            }
            TYPE_BLOB_INDEX => {
                let total_size = u32::from_le_bytes(data[0..4].try_into().unwrap());
                entries.remove(&(ns_name, key.clone()));
                blob_indices.insert(
                    (ns_idx, key),
                    BlobIndexMeta {
                        total_size,
                        chunk_count: data[4],
                        chunk_start: data[5],
                    },
                );
            }
            other => {
                return Err(NvsError::InvalidNvs(format!(
                    "不支持的 NVS 数据类型 0x{other:02x}（{ns_name}:{key}）"
                )));
            }
        }
        slot += span;
    }
    Ok(())
}

fn read_varlen_payload(
    page: &[u8],
    slot: usize,
    span: usize,
    data: &[u8],
    ns_name: &str,
    key: &str,
) -> Result<Vec<u8>, NvsError> {
    let size = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;
    let expected_crc = u32::from_le_bytes(data[4..8].try_into().unwrap());
    let needed_children = size.div_ceil(NVS_ENTRY_SIZE);
    if span < 1 + needed_children {
        return Err(NvsError::InvalidNvs(format!(
            "{ns_name}:{key} span={span} 小于所需槽位数 {}",
            1 + needed_children
        )));
    }
    let payload_start = NVS_DATA_OFFSET + (slot + 1) * NVS_ENTRY_SIZE;
    let payload_end = payload_start + size;
    if payload_end > NVS_PAGE_SIZE {
        return Err(NvsError::InvalidNvs(format!(
            "{ns_name}:{key} 负载越出页尾"
        )));
    }
    let payload = &page[payload_start..payload_end];
    if nvs_crc32(payload) != expected_crc {
        return Err(NvsError::InvalidNvs(format!(
            "{ns_name}:{key} 变长负载 CRC32 校验失败"
        )));
    }
    Ok(payload.to_vec())
}

fn bitmap_state(page: &[u8], slot: usize) -> u8 {
    let byte = page[NVS_HEADER_SIZE + slot / 4];
    (byte >> ((slot % 4) * 2)) & 0b11
}

fn verify_entry_crc(entry: &[u8]) -> bool {
    let expected = u32::from_le_bytes(entry[4..8].try_into().unwrap_or([0; 4]));
    let mut crc_input = [0u8; 28];
    crc_input[0..4].copy_from_slice(&entry[0..4]);
    crc_input[4..28].copy_from_slice(&entry[8..32]);
    nvs_crc32(&crc_input) == expected
}

struct EncodedItem {
    header: [u8; 32],
    children: Vec<[u8; 32]>,
    span: usize,
}

impl EncodedItem {
    fn from_raw(
        ns_id: u8,
        dtype: u8,
        span: usize,
        chunk_idx: u8,
        key: &str,
        data: [u8; 8],
    ) -> Self {
        let mut header = [0xFFu8; 32];
        header[0] = ns_id;
        header[1] = dtype;
        header[2] = span as u8;
        header[3] = chunk_idx;
        let mut key_buf = [0u8; 16];
        let kb = key.as_bytes();
        key_buf[..kb.len()].copy_from_slice(kb);
        header[8..24].copy_from_slice(&key_buf);
        header[24..32].copy_from_slice(&data);

        let mut crc_input = [0u8; 28];
        crc_input[0..4].copy_from_slice(&header[0..4]);
        crc_input[4..28].copy_from_slice(&header[8..32]);
        header[4..8].copy_from_slice(&nvs_crc32(&crc_input).to_le_bytes());

        Self {
            header,
            children: Vec::new(),
            span,
        }
    }

    fn namespace(name: &str, ns_id: u8) -> Self {
        let mut data = [0xFFu8; 8];
        data[0] = ns_id;
        Self::from_raw(0, TYPE_U8, 1, 0xFF, name, data)
    }

    fn varlen(ns_id: u8, dtype: u8, chunk_idx: u8, key: &str, payload: &[u8]) -> Self {
        let child_count = payload.len().div_ceil(NVS_ENTRY_SIZE);
        let span = 1 + child_count;
        let mut data = [0xFFu8; 8];
        data[0..2].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        data[4..8].copy_from_slice(&nvs_crc32(payload).to_le_bytes());

        let mut item = Self::from_raw(ns_id, dtype, span, chunk_idx, key, data);
        item.children = vec![[0xFFu8; 32]; child_count];
        for (idx, chunk) in payload.chunks(NVS_ENTRY_SIZE).enumerate() {
            item.children[idx][..chunk.len()].copy_from_slice(chunk);
        }
        item
    }
}

fn encode_value_items(
    ns_id: u8,
    key: &str,
    value: &NvsValue,
    out: &mut Vec<EncodedItem>,
) -> Result<(), NvsError> {
    let mut data = [0xFFu8; 8];
    match value {
        NvsValue::U8(v) => {
            data[0] = *v;
            out.push(EncodedItem::from_raw(ns_id, TYPE_U8, 1, 0xFF, key, data));
        }
        NvsValue::I8(v) => {
            data[0] = *v as u8;
            out.push(EncodedItem::from_raw(ns_id, TYPE_I8, 1, 0xFF, key, data));
        }
        NvsValue::U16(v) => {
            data[0..2].copy_from_slice(&v.to_le_bytes());
            out.push(EncodedItem::from_raw(ns_id, TYPE_U16, 1, 0xFF, key, data));
        }
        NvsValue::I16(v) => {
            data[0..2].copy_from_slice(&v.to_le_bytes());
            out.push(EncodedItem::from_raw(ns_id, TYPE_I16, 1, 0xFF, key, data));
        }
        NvsValue::U32(v) => {
            data[0..4].copy_from_slice(&v.to_le_bytes());
            out.push(EncodedItem::from_raw(ns_id, TYPE_U32, 1, 0xFF, key, data));
        }
        NvsValue::I32(v) => {
            data[0..4].copy_from_slice(&v.to_le_bytes());
            out.push(EncodedItem::from_raw(ns_id, TYPE_I32, 1, 0xFF, key, data));
        }
        NvsValue::U64(v) => {
            data[0..8].copy_from_slice(&v.to_le_bytes());
            out.push(EncodedItem::from_raw(ns_id, TYPE_U64, 1, 0xFF, key, data));
        }
        NvsValue::I64(v) => {
            data[0..8].copy_from_slice(&v.to_le_bytes());
            out.push(EncodedItem::from_raw(ns_id, TYPE_I64, 1, 0xFF, key, data));
        }
        NvsValue::Str(s) => {
            let mut payload = Vec::with_capacity(s.len() + 1);
            payload.extend_from_slice(s.as_bytes());
            payload.push(0);
            if payload.len() > NVS_STR_MAX_SIZE {
                return Err(NvsError::Validation(format!(
                    "字符串 {key} 长度超出 {NVS_STR_MAX_SIZE} 字节"
                )));
            }
            out.push(EncodedItem::varlen(ns_id, TYPE_SZ, 0xFF, key, &payload));
        }
        NvsValue::Blob(bytes) => {
            let chunks: Vec<&[u8]> = if bytes.is_empty() {
                vec![&[]]
            } else {
                bytes.chunks(NVS_CHUNK_MAX_SIZE).collect()
            };
            if chunks.len() > u8::MAX as usize {
                return Err(NvsError::CapacityExceeded(format!(
                    "Blob {key} 分片数超出 255 上限"
                )));
            }
            for (idx, chunk) in chunks.iter().enumerate() {
                out.push(EncodedItem::varlen(
                    ns_id,
                    TYPE_BLOB_DATA,
                    idx as u8,
                    key,
                    chunk,
                ));
            }
            data[0..4].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
            data[4] = chunks.len() as u8;
            data[5] = 0;
            out.push(EncodedItem::from_raw(
                ns_id,
                TYPE_BLOB_INDEX,
                1,
                0xFF,
                key,
                data,
            ));
        }
    }
    Ok(())
}

struct PageBuilder {
    seq_no: u32,
    state: u32,
    bitmap: [u8; 32],
    slots: Vec<[u8; 32]>,
}

impl PageBuilder {
    fn new(seq_no: u32) -> Self {
        Self {
            seq_no,
            state: PAGE_STATE_ACTIVE,
            bitmap: [0xFFu8; 32],
            slots: Vec::with_capacity(NVS_ENTRIES_PER_PAGE),
        }
    }

    fn free_slots(&self) -> usize {
        NVS_ENTRIES_PER_PAGE.saturating_sub(self.slots.len())
    }

    fn mark_written(&mut self, slot: usize) {
        let byte_idx = slot / 4;
        let bit_shift = (slot % 4) * 2;
        self.bitmap[byte_idx] &= !(0b11 << bit_shift);
        self.bitmap[byte_idx] |= BITMAP_WRITTEN << bit_shift;
    }

    fn push(&mut self, item: &EncodedItem) {
        let start = self.slots.len();
        self.slots.push(item.header);
        for child in &item.children {
            self.slots.push(*child);
        }
        for s in start..start + item.span {
            self.mark_written(s);
        }
    }

    fn write_into(&self, page: &mut [u8]) {
        page.fill(0xFF);
        page[0..4].copy_from_slice(&self.state.to_le_bytes());
        page[4..8].copy_from_slice(&self.seq_no.to_le_bytes());
        page[8] = NVS_VERSION_V2;
        let header_crc = nvs_crc32(&page[4..28]);
        page[28..32].copy_from_slice(&header_crc.to_le_bytes());
        page[32..64].copy_from_slice(&self.bitmap);
        for (idx, slot) in self.slots.iter().enumerate() {
            let off = NVS_DATA_OFFSET + idx * NVS_ENTRY_SIZE;
            page[off..off + NVS_ENTRY_SIZE].copy_from_slice(slot);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedWifiSlot {
    pub slot: usize,
    pub ssid: String,
    pub has_password: bool,
    pub channel: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XiaozhiNvsView {
    pub wifi_slots: Vec<SavedWifiSlot>,
    pub ota_url: Option<String>,
    pub max_tx_power: Option<i8>,
    pub remember_bssid: bool,
    /// `None`：设备上没有这个键，固件按默认“开启”处理
    pub sleep_mode: Option<bool>,
    /// `None`：没有这个键，固件按板型默认时间（固件内部记作 -1）；`Some(0)` 表示关掉这一步
    pub sleep_dim_min: Option<i32>,
    pub sleep_off_min: Option<i32>,
    pub board_uuid: Option<String>,
    pub has_ws_token: bool,
    pub ws_url: Option<String>,
    pub namespaces: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewWifiPatch {
    pub ssid: String,
    pub password: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XiaozhiNvsPatch {
    /// 保留的原始 Wi-Fi 槽位编号及排列顺序（用于置顶、删除），在后端保留原密码，绝不回传明文密码到前端。
    pub retain_slots: Option<Vec<usize>>,
    /// 若填写新 Wi-Fi，则插入到槽位 0 并顺延保留其余不重复槽位（最多 10 条）。
    pub new_wifi: Option<NewWifiPatch>,
    /// 自定义 OTA 地址；传空字符串表示擦除该键回退固件默认。
    pub ota_url: Option<String>,
    /// 最大发射功率（i8，dBm*4）；传 `Some(0)` 表示擦除恢复默认。
    pub max_tx_power: Option<i8>,
    pub remember_bssid: Option<bool>,
    /// 不传就不动设备上的值（与官方配网页一致：只保存显式提交的项）
    pub sleep_mode: Option<bool>,
    /// 不传：不动；`-1`：擦除该键，回到板型默认时间；`0..=1440`：设为该分钟数（`0` 关掉这一步）
    pub sleep_dim_min: Option<i32>,
    pub sleep_off_min: Option<i32>,
    pub board_uuid: Option<String>,
    pub ws_token: Option<String>,
    pub ws_url: Option<String>,
}

fn wifi_slot_keys(slot: usize) -> (String, String, String) {
    if slot == 0 {
        (
            "ssid".to_string(),
            "password".to_string(),
            "channel".to_string(),
        )
    } else {
        (
            format!("ssid{slot}"),
            format!("password{slot}"),
            format!("channel{slot}"),
        )
    }
}

#[derive(Clone)]
struct InternalWifiSlot {
    slot: usize,
    ssid: String,
    password: String,
    has_password_key: bool,
    channel: Option<u8>,
}

fn read_internal_wifi_slots(store: &NvsStore) -> Vec<InternalWifiSlot> {
    let mut list = Vec::new();
    for slot in 0..NVS_WIFI_MAX_SLOTS {
        let (ssid_k, pass_k, chan_k) = wifi_slot_keys(slot);
        // 与固件 `SsidManager::LoadFromNvs` 一致：ssid 与 password 两个键都在才算一个可用槽位。
        // 只有 ssid 的残缺槽位固件会跳过；这里也不列出、写回时不保留，免得补上空密码后变成开放网络生效。
        if let Some(ssid) = store.get_str("wifi", &ssid_k)
            && !ssid.is_empty()
            && let Some(password) = store.get_str("wifi", &pass_k)
        {
            list.push(InternalWifiSlot {
                slot,
                ssid: ssid.to_string(),
                password: password.to_string(),
                has_password_key: !password.is_empty(),
                channel: store.get_u8("wifi", &chan_k),
            });
        }
    }
    list
}

pub fn inspect_xiaozhi_nvs(store: &NvsStore) -> XiaozhiNvsView {
    let wifi_slots = read_internal_wifi_slots(store)
        .into_iter()
        .map(|s| SavedWifiSlot {
            slot: s.slot,
            ssid: s.ssid,
            has_password: s.has_password_key,
            channel: s.channel,
        })
        .collect();
    let mut namespaces: Vec<String> = store
        .entries
        .keys()
        .map(|(ns, _)| ns.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    namespaces.sort();

    XiaozhiNvsView {
        wifi_slots,
        ota_url: store.get_str("wifi", "ota_url").map(str::to_string),
        max_tx_power: store.get_i8("wifi", "max_tx_power"),
        remember_bssid: store
            .get_u8("wifi", "remember_bssid")
            .is_some_and(|v| v != 0),
        sleep_mode: store.get_u8("wifi", "sleep_mode").map(|v| v != 0),
        sleep_dim_min: store.get_i32("wifi", "sleep_dim_min"),
        sleep_off_min: store.get_i32("wifi", "sleep_off_min"),
        board_uuid: store.get_str("board", "uuid").map(str::to_string),
        has_ws_token: store
            .get_str("websocket", "token")
            .is_some_and(|t| !t.is_empty()),
        ws_url: store.get_str("websocket", "url").map(str::to_string),
        namespaces,
    }
}

pub fn apply_xiaozhi_nvs_patch(
    store: &mut NvsStore,
    patch: &XiaozhiNvsPatch,
) -> Result<(), NvsError> {
    apply_wifi_slots_patch(store, patch)?;
    apply_wifi_advanced_patch(store, patch)?;

    if let Some(uuid) = &patch.board_uuid {
        let trimmed = uuid.trim();
        if trimmed.is_empty() {
            return Err(NvsError::Validation("board:uuid 不能为空".into()));
        }
        store.set("board", "uuid", NvsValue::Str(trimmed.to_string()))?;
    }
    if let Some(token) = &patch.ws_token {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            store.remove("websocket", "token");
        } else {
            store.set("websocket", "token", NvsValue::Str(trimmed.to_string()))?;
        }
    }
    if let Some(ws_url) = &patch.ws_url {
        let trimmed = ws_url.trim();
        if trimmed.is_empty() {
            store.remove("websocket", "url");
        } else {
            if !trimmed.starts_with("ws://") && !trimmed.starts_with("wss://") {
                return Err(NvsError::Validation(
                    "WebSocket 地址必须以 ws:// 或 wss:// 开头".into(),
                ));
            }
            store.set("websocket", "url", NvsValue::Str(trimmed.to_string()))?;
        }
    }
    Ok(())
}

fn apply_wifi_slots_patch(store: &mut NvsStore, patch: &XiaozhiNvsPatch) -> Result<(), NvsError> {
    if patch.retain_slots.is_none() && patch.new_wifi.is_none() {
        return Ok(());
    }
    let existing = read_internal_wifi_slots(store);
    let by_slot: HashMap<usize, InternalWifiSlot> =
        existing.iter().cloned().map(|s| (s.slot, s)).collect();

    let mut target: Vec<InternalWifiSlot> = match &patch.retain_slots {
        Some(order) => {
            let mut ordered = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for &idx in order {
                if seen.insert(idx)
                    && let Some(item) = by_slot.get(&idx)
                {
                    ordered.push(item.clone());
                }
            }
            ordered
        }
        None => existing,
    };

    if let Some(new_wifi) = &patch.new_wifi {
        // 与官方配网页一致按原样保存（SSID 首尾的空格也是名字的一部分），只拒绝全空白
        let ssid = new_wifi.ssid.as_str();
        if ssid.trim().is_empty() || ssid.len() > NVS_SSID_MAX_BYTES {
            return Err(NvsError::Validation(format!(
                "Wi-Fi 名称（SSID）长度必须为 1..={NVS_SSID_MAX_BYTES} 字节"
            )));
        }
        if new_wifi.password.len() > NVS_PASS_MAX_BYTES {
            return Err(NvsError::Validation(format!(
                "Wi-Fi 密码长度不能超过 {NVS_PASS_MAX_BYTES} 字节"
            )));
        }
        target.retain(|item| item.ssid != ssid);
        target.insert(
            0,
            InternalWifiSlot {
                slot: 0,
                ssid: ssid.to_string(),
                password: new_wifi.password.clone(),
                has_password_key: !new_wifi.password.is_empty(),
                channel: None,
            },
        );
    }

    target.truncate(NVS_WIFI_MAX_SLOTS);
    for slot in 0..NVS_WIFI_MAX_SLOTS {
        let (ssid_k, pass_k, chan_k) = wifi_slot_keys(slot);
        store.remove("wifi", &ssid_k);
        store.remove("wifi", &pass_k);
        store.remove("wifi", &chan_k);
    }
    for (idx, entry) in target.into_iter().enumerate() {
        let (ssid_k, pass_k, chan_k) = wifi_slot_keys(idx);
        store.set("wifi", &ssid_k, NvsValue::Str(entry.ssid))?;
        store.set("wifi", &pass_k, NvsValue::Str(entry.password))?;
        if let Some(ch) = entry.channel {
            store.set("wifi", &chan_k, NvsValue::U8(ch))?;
        }
    }
    Ok(())
}

fn apply_wifi_advanced_patch(
    store: &mut NvsStore,
    patch: &XiaozhiNvsPatch,
) -> Result<(), NvsError> {
    if let Some(ota_url) = &patch.ota_url {
        let trimmed = ota_url.trim();
        if trimmed.is_empty() {
            store.remove("wifi", "ota_url");
        } else {
            if trimmed.len() > NVS_OTA_URL_MAX_BYTES {
                return Err(NvsError::Validation(format!(
                    "OTA 地址长度不能超过 {NVS_OTA_URL_MAX_BYTES} 字节"
                )));
            }
            if !is_valid_ota_url(trimmed) {
                return Err(NvsError::Validation(
                    "OTA 地址不合规：须以 http:// 或 https:// 开头、带主机名，不能含空格、中文、反斜杠或不完整的 % 编码（小智固件会忽略这样的地址并退回官方默认）".into(),
                ));
            }
            store.set("wifi", "ota_url", NvsValue::Str(trimmed.to_string()))?;
        }
    }
    if let Some(power) = patch.max_tx_power {
        if power == 0 {
            store.remove("wifi", "max_tx_power");
        } else if ALLOWED_TX_POWERS.contains(&power) {
            store.set("wifi", "max_tx_power", NvsValue::I8(power))?;
        } else {
            return Err(NvsError::Validation(format!(
                "最大发射功率档位 {power} 不在允许列表 {ALLOWED_TX_POWERS:?} 内"
            )));
        }
    }
    if let Some(remember) = patch.remember_bssid {
        store.set("wifi", "remember_bssid", NvsValue::U8(u8::from(remember)))?;
    }
    if let Some(sleep) = patch.sleep_mode {
        store.set("wifi", "sleep_mode", NvsValue::U8(u8::from(sleep)))?;
    }

    // 与官方配网页 `wifi_configuration_ap.cc` 同一语义：没提交的项保留原值；-1 擦除键回到板型默认；
    // 0..=1440 为分钟数。只在本次提交了等待时间时才校验“关屏须晚于变暗”（不因设备上原有的值拒绝别的改动）。
    if patch.sleep_dim_min.is_some() || patch.sleep_off_min.is_some() {
        let effective = |submitted: Option<i32>, key: &str| -> Result<Option<i32>, NvsError> {
            match submitted {
                Some(-1) => Ok(None),
                Some(minutes) if (0..=NVS_SLEEP_MAX_MIN).contains(&minutes) => Ok(Some(minutes)),
                Some(_) => Err(NvsError::Validation(format!(
                    "睡眠等待时间必须在 0..={NVS_SLEEP_MAX_MIN} 分钟之间（-1 表示板型默认）"
                ))),
                None => Ok(store
                    .get_i32("wifi", key)
                    .filter(|m| (0..=NVS_SLEEP_MAX_MIN).contains(m))),
            }
        };
        let dim = effective(patch.sleep_dim_min, "sleep_dim_min")?;
        let off = effective(patch.sleep_off_min, "sleep_off_min")?;
        if let (Some(dim), Some(off)) = (dim, off)
            && dim > 0
            && off > 0
            && off <= dim
        {
            return Err(NvsError::Validation(
                "关屏等待时间（分钟）必须大于变暗等待时间".into(),
            ));
        }
        for (key, submitted) in [
            ("sleep_dim_min", patch.sleep_dim_min),
            ("sleep_off_min", patch.sleep_off_min),
        ] {
            match submitted {
                Some(-1) => {
                    store.remove("wifi", key);
                }
                Some(minutes) => store.set("wifi", key, NvsValue::I32(minutes))?,
                None => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 官方 `78/esp-wifi-connect` `tests/url_cases.json` 的全部用例（2026-11-12 `7709506`），
    /// MOSS 的判断必须与固件 `wifi_config::IsValidOtaUrl` 逐条一致。
    #[test]
    fn ota_url_rules_match_official_firmware_cases() {
        for (url, valid) in [
            ("", true),
            ("Huawei-1ct5dn", false),
            ("https://api.example.com/ota/", true),
            ("http://192.168.1.2:8000/xiaozhi/ota/", true),
            ("http://localhost/ota", true),
            ("http://a", true),
            ("HTTP://EXAMPLE.COM/ota", true),
            ("https://example.com:65535/ota?q=a%20b", true),
            ("https://[2001:db8::1]:8443/ota/", true),
            ("http://user:password@example.com/ota", true),
            ("https://example.com/%E4%B8%AD%E6%96%87", true),
            ("https://example.com/中文", false),
            ("/ota/", false),
            ("//example.com/ota", false),
            ("ftp://example.com/ota", false),
            ("javascript:alert(1)", false),
            ("https:example.com", false),
            ("https://", false),
            ("https:///ota", false),
            ("https://?q=x", false),
            ("https://#fragment", false),
            ("https://:8080/ota", false),
            ("https://example.com:0/ota", false),
            ("https://example.com:65536/ota", false),
            ("https://example.com:abc/ota", false),
            (" https://example.com/ota", false),
            ("https://example.com/ota ", false),
            ("https://example.com/a b", false),
            ("https://example.com/a\nb", false),
            ("https://example.com\\ota", false),
            ("https://example.com/%", false),
            ("https://example.com/%0x", false),
        ] {
            assert_eq!(is_valid_ota_url(url), valid, "{url:?}");
        }
        assert!(!is_valid_ota_url(&format!("http://a/{}", "x".repeat(250))));
    }

    /// 休眠设置与官方配网页同一语义：没设过就显示“没设过”；不提交就不动；-1 回到板型默认。
    #[test]
    fn sleep_settings_are_untouched_unless_submitted() {
        let mut store = NvsStore::default();
        store
            .set("wifi", "ssid", NvsValue::Str("Home".into()))
            .unwrap();
        store
            .set("wifi", "password", NvsValue::Str("pw".into()))
            .unwrap();
        let view = inspect_xiaozhi_nvs(&store);
        assert_eq!(
            (view.sleep_mode, view.sleep_dim_min, view.sleep_off_min),
            (None, None, None),
            "出厂板子没有休眠键：固件按“开启 + 板型默认时间”"
        );
        // 只改 Wi-Fi：不能顺手写入任何休眠键
        apply_xiaozhi_nvs_patch(
            &mut store,
            &XiaozhiNvsPatch {
                new_wifi: Some(NewWifiPatch {
                    ssid: "Office".into(),
                    password: "secret".into(),
                }),
                ..Default::default()
            },
        )
        .unwrap();
        for key in ["sleep_mode", "sleep_dim_min", "sleep_off_min"] {
            assert!(
                store.get_u8("wifi", key).is_none() && store.get_i32("wifi", key).is_none(),
                "{key}"
            );
        }
        // 显式设置，再用 -1 擦回板型默认
        let set = |dim, off| XiaozhiNvsPatch {
            sleep_dim_min: dim,
            sleep_off_min: off,
            ..Default::default()
        };
        apply_xiaozhi_nvs_patch(&mut store, &set(Some(5), Some(30))).unwrap();
        assert_eq!(store.get_i32("wifi", "sleep_dim_min"), Some(5));
        apply_xiaozhi_nvs_patch(&mut store, &set(Some(-1), None)).unwrap();
        assert_eq!(store.get_i32("wifi", "sleep_dim_min"), None, "-1 擦除键");
        assert_eq!(
            store.get_i32("wifi", "sleep_off_min"),
            Some(30),
            "没提交的保留"
        );
        // 关屏须晚于变暗（两者都 > 0 时），0 表示关掉这一步不参与比较
        assert!(apply_xiaozhi_nvs_patch(&mut store, &set(Some(40), None)).is_err());
        assert!(apply_xiaozhi_nvs_patch(&mut store, &set(Some(0), None)).is_ok());
        assert!(apply_xiaozhi_nvs_patch(&mut store, &set(Some(1441), None)).is_err());
        assert!(apply_xiaozhi_nvs_patch(&mut store, &set(Some(-2), None)).is_err());
    }

    /// 只有 ssid、没有 password 键的残缺槽位固件会跳过；改写 Wi-Fi 时不能把它补成开放网络。
    /// 新 SSID 按原样保存（首尾空格是名字的一部分），全空白才拒绝。
    #[test]
    fn wifi_slots_follow_firmware_ssid_manager() {
        let mut store = NvsStore::default();
        store
            .set("wifi", "ssid", NvsValue::Str("Home".into()))
            .unwrap();
        store
            .set("wifi", "password", NvsValue::Str("pw".into()))
            .unwrap();
        store
            .set("wifi", "ssid1", NvsValue::Str("Broken".into()))
            .unwrap();
        store
            .set("wifi", "ssid2", NvsValue::Str("Cafe".into()))
            .unwrap();
        store
            .set("wifi", "password2", NvsValue::Str("".into()))
            .unwrap();
        let view = inspect_xiaozhi_nvs(&store);
        let names: Vec<_> = view.wifi_slots.iter().map(|s| s.ssid.as_str()).collect();
        assert_eq!(names, vec!["Home", "Cafe"], "残缺槽位不列出");
        assert!(
            !view.wifi_slots[1].has_password,
            "空密码 = 开放网络，如实显示"
        );
        apply_xiaozhi_nvs_patch(
            &mut store,
            &XiaozhiNvsPatch {
                new_wifi: Some(NewWifiPatch {
                    ssid: " Lab 5G ".into(),
                    password: "p".into(),
                }),
                ..Default::default()
            },
        )
        .unwrap();
        let after: Vec<_> = inspect_xiaozhi_nvs(&store)
            .wifi_slots
            .iter()
            .map(|s| s.ssid.clone())
            .collect();
        assert_eq!(after, vec![" Lab 5G ", "Home", "Cafe"]);
        assert!(
            (0..NVS_WIFI_MAX_SLOTS)
                .all(|i| store.get_str("wifi", &wifi_slot_keys(i).0) != Some("Broken")),
            "残缺槽位没有被补上空密码写回"
        );
        assert!(
            apply_xiaozhi_nvs_patch(
                &mut store,
                &XiaozhiNvsPatch {
                    new_wifi: Some(NewWifiPatch {
                        ssid: "   ".into(),
                        password: String::new()
                    }),
                    ..Default::default()
                },
            )
            .is_err()
        );
    }

    /// 擦写前的分区边界兜底：起点不对齐、落在分区表上、与别的分区重叠、超出 Flash 容量都拒绝。
    #[test]
    fn nvs_partition_bounds_are_checked_before_erasing() {
        let table = |entries: &[[u8; 32]]| {
            let mut raw = vec![0xFFu8; 0xC00];
            for (i, e) in entries.iter().enumerate() {
                raw[i * 32..(i + 1) * 32].copy_from_slice(e);
            }
            raw
        };
        let app = make_part_entry(0x00, 0x10, 0x20000, 0x100000, "ota_0", 0);
        let ok = parse_partition_table(&table(&[
            make_part_entry(0x01, 0x02, 0x9000, 0x6000, "nvs", 0),
            app,
        ]))
        .unwrap();
        assert!(ok.require_writable_nvs_within(Some(4 << 20)).is_ok());
        assert!(
            ok.require_writable_nvs_within(Some(0xC000)).is_err(),
            "超出 Flash 容量"
        );
        for bad in [
            make_part_entry(0x01, 0x02, 0x9800, 0x6000, "nvs", 0),
            make_part_entry(0x01, 0x02, 0x7000, 0x6000, "nvs", 0),
            make_part_entry(0x01, 0x02, 0x1F000, 0x6000, "nvs", 0),
        ] {
            let summary = parse_partition_table(&table(&[bad, app])).unwrap();
            assert!(summary.require_writable_nvs().is_err(), "{:?}", summary.nvs);
        }
    }

    /// 键名不是合法 UTF-8 时拒绝解析，不做有损改写。
    #[test]
    fn non_utf8_keys_are_rejected_instead_of_rewritten() {
        let mut store = NvsStore::default();
        store.set("wifi", "badkey", NvsValue::U8(1)).unwrap();
        let mut image = store.build(NVS_PAGE_SIZE * 3).unwrap();
        let at = image.windows(6).position(|w| w == b"badkey").unwrap();
        image[at] = 0xFF;
        let entry_start = at - 8;
        let entry: [u8; 32] = image[entry_start..entry_start + 32].try_into().unwrap();
        let mut crc_input = [0u8; 28];
        crc_input[0..4].copy_from_slice(&entry[0..4]);
        crc_input[4..28].copy_from_slice(&entry[8..32]);
        image[entry_start + 4..entry_start + 8]
            .copy_from_slice(&nvs_crc32(&crc_input).to_le_bytes());
        assert!(matches!(
            NvsStore::parse(&image),
            Err(NvsError::InvalidNvs(_))
        ));
    }

    fn make_part_entry(
        ptype: u8,
        subtype: u8,
        offset: u32,
        size: u32,
        label: &str,
        flags: u32,
    ) -> [u8; 32] {
        let mut e = [0u8; 32];
        e[0..2].copy_from_slice(&PARTITION_MAGIC);
        e[2] = ptype;
        e[3] = subtype;
        e[4..8].copy_from_slice(&offset.to_le_bytes());
        e[8..12].copy_from_slice(&size.to_le_bytes());
        let lb = label.as_bytes();
        let n = lb.len().min(16);
        e[12..12 + n].copy_from_slice(&lb[..n]);
        e[28..32].copy_from_slice(&flags.to_le_bytes());
        e
    }

    #[test]
    fn nvs_crc32_matches_esp_idf_initial_all_ones_vector() {
        assert_eq!(nvs_crc32(b"123456789"), 0xd202_d277);
    }

    #[test]
    fn partition_table_rejects_encrypted_nvs_and_fluidnc_boards() {
        let mut xiaozhi_tbl = vec![0xFFu8; PARTITION_TABLE_READ_SIZE as usize];
        xiaozhi_tbl[0..32].copy_from_slice(&make_part_entry(0x01, 0x02, 0x9000, 0x6000, "nvs", 0));
        xiaozhi_tbl[32..64]
            .copy_from_slice(&make_part_entry(0x00, 0x10, 0x20000, 0x200000, "ota_0", 0));
        xiaozhi_tbl[64..96].copy_from_slice(&make_part_entry(
            0x01, 0x82, 0x600000, 0x400000, "assets", 0,
        ));
        let ok = parse_partition_table(&xiaozhi_tbl).unwrap();
        let nvs = ok.require_writable_nvs().unwrap();
        assert_eq!(nvs.offset, 0x9000);
        assert_eq!(nvs.size, 0x6000);
        assert!(ok.has_xiaozhi_markers);

        // encrypted flag on nvs
        let mut enc_tbl = xiaozhi_tbl.clone();
        enc_tbl[0..32].copy_from_slice(&make_part_entry(0x01, 0x02, 0x9000, 0x6000, "nvs", 1));
        assert!(matches!(
            parse_partition_table(&enc_tbl)
                .unwrap()
                .require_writable_nvs(),
            Err(NvsError::EncryptedNvs(_))
        ));

        // nvs_keys partition present
        let mut key_tbl = xiaozhi_tbl.clone();
        key_tbl[96..128]
            .copy_from_slice(&make_part_entry(0x01, 0x04, 0xF000, 0x1000, "nvs_keys", 0));
        assert!(matches!(
            parse_partition_table(&key_tbl)
                .unwrap()
                .require_writable_nvs(),
            Err(NvsError::EncryptedNvs(_))
        ));

        // FluidNC board protection (even with ota_0/ota_1/assets present)
        let mut fluidnc_tbl = vec![0xFFu8; PARTITION_TABLE_READ_SIZE as usize];
        fluidnc_tbl[0..32].copy_from_slice(&make_part_entry(0x01, 0x02, 0x9000, 0x5000, "nvs", 0));
        fluidnc_tbl[32..64].copy_from_slice(&make_part_entry(
            0x01, 0x82, 0x310000, 0xF0000, "localfs", 0,
        ));
        assert!(matches!(
            parse_partition_table(&fluidnc_tbl)
                .unwrap()
                .require_writable_nvs(),
            Err(NvsError::WrongDevicePartition(_))
        ));

        let mut ota_fluidnc_tbl = vec![0xFFu8; PARTITION_TABLE_READ_SIZE as usize];
        ota_fluidnc_tbl[0..32]
            .copy_from_slice(&make_part_entry(0x01, 0x02, 0x9000, 0x5000, "nvs", 0));
        ota_fluidnc_tbl[32..64]
            .copy_from_slice(&make_part_entry(0x00, 0x10, 0x20000, 0x1E0000, "ota_0", 0));
        ota_fluidnc_tbl[64..96]
            .copy_from_slice(&make_part_entry(0x00, 0x11, 0x200000, 0x1E0000, "ota_1", 0));
        ota_fluidnc_tbl[96..128]
            .copy_from_slice(&make_part_entry(0x01, 0x82, 0x3E0000, 0x20000, "assets", 0));
        ota_fluidnc_tbl[128..160].copy_from_slice(&make_part_entry(
            0x01, 0x82, 0x400000, 0x100000, "localfs", 0,
        ));
        let ota_fluidnc_summary = parse_partition_table(&ota_fluidnc_tbl).unwrap();
        assert!(matches!(
            ota_fluidnc_summary.require_writable_nvs(),
            Err(NvsError::WrongDevicePartition(_))
        ));

        // Pure OTA table without model/assets should not claim Xiaozhi markers
        let mut generic_ota_tbl = vec![0xFFu8; PARTITION_TABLE_READ_SIZE as usize];
        generic_ota_tbl[0..32]
            .copy_from_slice(&make_part_entry(0x01, 0x02, 0x9000, 0x6000, "nvs", 0));
        generic_ota_tbl[32..64]
            .copy_from_slice(&make_part_entry(0x00, 0x10, 0x20000, 0x200000, "ota_0", 0));
        assert!(
            !parse_partition_table(&generic_ota_tbl)
                .unwrap()
                .has_xiaozhi_markers
        );
    }

    #[test]
    fn nvs_roundtrip_handles_multipage_blobs_unrelated_keys_and_trailing_empty_page() {
        let mut store = NvsStore::default();
        store
            .set("wifi", "ssid", NvsValue::Str("Workshop".into()))
            .unwrap();
        store
            .set("wifi", "password", NvsValue::Str("secret-pass".into()))
            .unwrap();
        store.set("wifi", "max_tx_power", NvsValue::I8(60)).unwrap();
        store
            .set("wifi", "sleep_dim_min", NvsValue::I32(15))
            .unwrap();
        store
            .set(
                "board",
                "uuid",
                NvsValue::Str("11111111-2222-4333-8444-555555555555".into()),
            )
            .unwrap();
        store.set("audio", "volume", NvsValue::U8(75)).unwrap();
        store
            .set("cal", "gyro_blob", NvsValue::Blob(vec![0xA5; 5100]))
            .unwrap();

        // 0x6000 = 6 pages; 5100-byte blob takes 2 pages across chunks, leaving >= 1 trailing 0xFF page.
        let image = store.build(0x6000).unwrap();
        assert_eq!(image.len(), 0x6000);
        // Trailing page must stay completely erased (0xFF) for ESP-IDF wear-leveling.
        assert!(image[0x5000..0x6000].iter().all(|&b| b == 0xFF));

        let parsed = NvsStore::parse(&image).unwrap();
        assert_eq!(parsed, store);

        // Fresh completely-erased NVS partition parses cleanly as empty store.
        let blank = vec![0xFFu8; 0x4000];
        assert!(NvsStore::parse(&blank).unwrap().entries.is_empty());
    }

    #[test]
    fn xiaozhi_wifi_patch_preserves_passwords_and_erases_empty_ota() {
        let mut store = NvsStore::default();
        store
            .set("wifi", "ssid", NvsValue::Str("HomeNet".into()))
            .unwrap();
        store
            .set("wifi", "password", NvsValue::Str("home-pw-123".into()))
            .unwrap();
        store
            .set("wifi", "ssid1", NvsValue::Str("LabNet".into()))
            .unwrap();
        store
            .set("wifi", "password1", NvsValue::Str("lab-pw-456".into()))
            .unwrap();
        store
            .set(
                "wifi",
                "ota_url",
                NvsValue::Str("http://192.168.1.9:9666/xiaozhi/ota/".into()),
            )
            .unwrap();
        store.set("custom", "keep_me", NvsValue::U32(42)).unwrap();

        let view = inspect_xiaozhi_nvs(&store);
        assert_eq!(view.wifi_slots.len(), 2);
        assert!(view.wifi_slots[0].has_password);

        // Pin slot 1 (LabNet) to top, erase ota_url, add third network Workshop without touching passwords.
        let patch = XiaozhiNvsPatch {
            retain_slots: Some(vec![1, 0]),
            new_wifi: Some(NewWifiPatch {
                ssid: "Workshop".into(),
                password: "ws-pw".into(),
            }),
            ota_url: Some(String::new()),
            max_tx_power: Some(52),
            remember_bssid: Some(true),
            sleep_mode: Some(true),
            sleep_dim_min: Some(10),
            sleep_off_min: Some(30),
            board_uuid: None,
            ws_token: Some("tok.abc".into()),
            ws_url: Some("ws://192.168.1.10:9667/xiaozhi/v1/".into()),
        };
        apply_xiaozhi_nvs_patch(&mut store, &patch).unwrap();

        let built = store.build(0x6000).unwrap();
        let reloaded = NvsStore::parse(&built).unwrap();
        assert_eq!(reloaded.get_str("wifi", "ssid"), Some("Workshop"));
        assert_eq!(reloaded.get_str("wifi", "password"), Some("ws-pw"));
        assert_eq!(reloaded.get_str("wifi", "ssid1"), Some("LabNet"));
        assert_eq!(reloaded.get_str("wifi", "password1"), Some("lab-pw-456"));
        assert_eq!(reloaded.get_str("wifi", "ssid2"), Some("HomeNet"));
        assert_eq!(reloaded.get_str("wifi", "password2"), Some("home-pw-123"));
        assert_eq!(reloaded.get_str("wifi", "ota_url"), None);
        assert_eq!(reloaded.get_i8("wifi", "max_tx_power"), Some(52));
        assert_eq!(reloaded.get_str("websocket", "token"), Some("tok.abc"));
        assert_eq!(
            reloaded.entries.get(&("custom".into(), "keep_me".into())),
            Some(&NvsValue::U32(42))
        );

        // Sleep validation rejects off <= dim when both > 0.
        let bad_sleep = XiaozhiNvsPatch {
            sleep_dim_min: Some(30),
            sleep_off_min: Some(20),
            ..Default::default()
        };
        assert!(matches!(
            apply_xiaozhi_nvs_patch(&mut store, &bad_sleep),
            Err(NvsError::Validation(_))
        ));
    }

    #[test]
    fn uuid_v4_formatting_sets_version_and_variant_bits() {
        let u = format_uuid_v4([0; 16]);
        assert_eq!(u, "00000000-0000-4000-8000-000000000000");
    }

    #[test]
    fn partition_table_blocks_spiffs_littlefs_and_recognizes_echoear_assets_a() {
        for label in ["spiffs", "littlefs", "ffat"] {
            let mut tbl = vec![0xFFu8; PARTITION_TABLE_READ_SIZE as usize];
            tbl[0..32].copy_from_slice(&make_part_entry(0x01, 0x02, 0x9000, 0x6000, "nvs", 0));
            tbl[32..64].copy_from_slice(&make_part_entry(0x01, 0x82, 0x200000, 0x100000, label, 0));
            let summary = parse_partition_table(&tbl).unwrap();
            assert!(summary.has_fluidnc_markers, "label={label}");
            assert!(matches!(
                summary.require_writable_nvs(),
                Err(NvsError::WrongDevicePartition(_))
            ));
        }

        let mut echoear = vec![0xFFu8; PARTITION_TABLE_READ_SIZE as usize];
        echoear[0..32].copy_from_slice(&make_part_entry(0x01, 0x02, 0x9000, 0x4000, "nvs", 0));
        echoear[32..64].copy_from_slice(&make_part_entry(
            0x01, 0x82, 0xb00000, 0x3e8000, "assets_A", 0,
        ));
        let summary = parse_partition_table(&echoear).unwrap();
        assert!(summary.has_xiaozhi_markers);
        assert!(summary.require_writable_nvs().is_ok());
    }

    #[test]
    fn wear_leveling_relocated_namespace_and_cross_type_overwrites_parse_cleanly() {
        // 模拟磨损均衡：旧页 seq=1 已含 wifi(ns=1):ssid 与一个缺失分片的旧 blob "item"，
        // 更高页 seq=5 才持有从 seq=0 搬来的 ns=1 命名空间定义，并把 "item" 覆写成字符串；
        // 更早页 seq=0 曾写字符串 "blobkey"，seq=5 将其覆写成 v2 分片 Blob。
        let mut p0_items = Vec::new();
        p0_items.push(EncodedItem::namespace("cfg", 2));
        encode_value_items(
            2,
            "blobkey",
            &NvsValue::Str("old-str".into()),
            &mut p0_items,
        )
        .unwrap();
        let mut p0 = PageBuilder::new(0);
        p0.state = PAGE_STATE_FULL;
        for item in &p0_items {
            p0.push(item);
        }

        let mut p1_items = Vec::new();
        encode_value_items(1, "ssid", &NvsValue::Str("WearNet".into()), &mut p1_items).unwrap();
        // 写一个缺分片的废弃 BlobIndex（旧 chunk 所在页已被擦除）
        let mut dangling_idx = [0xFFu8; 8];
        dangling_idx[0..4].copy_from_slice(&64u32.to_le_bytes());
        dangling_idx[4] = 1;
        dangling_idx[5] = 0;
        p1_items.push(EncodedItem::from_raw(
            1,
            TYPE_BLOB_INDEX,
            1,
            0xFF,
            "item",
            dangling_idx,
        ));
        let mut p1 = PageBuilder::new(1);
        p1.state = PAGE_STATE_FULL;
        for item in &p1_items {
            p1.push(item);
        }

        let mut p5_items = Vec::new();
        p5_items.push(EncodedItem::namespace("wifi", 1));
        encode_value_items(1, "item", &NvsValue::Str("now-text".into()), &mut p5_items).unwrap();
        encode_value_items(
            2,
            "blobkey",
            &NvsValue::Blob(vec![0x5Au8; 16]),
            &mut p5_items,
        )
        .unwrap();
        let mut p5 = PageBuilder::new(5);
        p5.state = PAGE_STATE_ACTIVE;
        for item in &p5_items {
            p5.push(item);
        }

        let mut raw = vec![0xFFu8; NVS_PAGE_SIZE * 4];
        p0.write_into(&mut raw[0..NVS_PAGE_SIZE]);
        p1.write_into(&mut raw[NVS_PAGE_SIZE..NVS_PAGE_SIZE * 2]);
        p5.write_into(&mut raw[NVS_PAGE_SIZE * 2..NVS_PAGE_SIZE * 3]);

        let parsed = NvsStore::parse(&raw).unwrap();
        assert_eq!(parsed.get_str("wifi", "ssid"), Some("WearNet"));
        assert_eq!(parsed.get_str("wifi", "item"), Some("now-text"));
        assert_eq!(
            parsed.entries.get(&("cfg".into(), "blobkey".into())),
            Some(&NvsValue::Blob(vec![0x5Au8; 16]))
        );
    }
}
