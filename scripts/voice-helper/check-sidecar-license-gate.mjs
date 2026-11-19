#!/usr/bin/env node
// Fail-closed cross-platform native-license release gate for the optional voice helper sidecar
// (apps/moss-voice-local). Mirrors scripts/voice-helper/audit-native-licenses.ps1 so Linux,
// macOS, and Windows release jobs enforce docs/developer/safety.md §4.5 uniformly.
import { createHash } from "node:crypto";
import { existsSync, lstatSync, readdirSync, readFileSync, realpathSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

export const REPO_ROOT = resolve(dirname(realpathSync(fileURLToPath(import.meta.url))), "../..");
export const EXPECTED_WIN_ARCHIVE_NAME = "sherpa-onnx-v1.13.8-win-x64-static-MT-Release-lib";
export const EXPECTED_WIN_ARCHIVE_SHA256 =
  "56ffcf3c454c1f14f7bc9887286cc8143e7e542dc632804e1c447d5f8d534eaf";
export const EXPECTED_WIN_ARCHIVE_BYTES = 123206268;
export const REQUIRED_NATIVE_LIBS = [
  "sherpa-onnx-c-api",
  "sherpa-onnx-core",
  "kaldi-decoder-core",
  "sherpa-onnx-kaldifst-core",
  "sherpa-onnx-fstfar",
  "sherpa-onnx-fst",
  "kaldi-native-fbank-core",
  "kissfft-float",
  "piper_phonemize",
  "espeak-ng",
  "ucd",
  "onnxruntime",
  "ssentencepiece_core",
];

const SAFE_REL_PATH_RE = /^[a-zA-Z0-9][a-zA-Z0-9._/-]{0,180}$/;
const SAFE_FLAT_NAME_RE = /^[a-zA-Z0-9][a-zA-Z0-9._+-]{0,180}$/;
const SHA256_RE = /^[a-f0-9]{64}$/;
export const EXPECTED_ORT_ROOT_SHA256 = "a5003dfd85d66a4d45bd2757c7f813f9e835142e9a936ac04877c788d6afe938";

function sha256File(filePath) {
  return createHash("sha256").update(readFileSync(filePath)).digest("hex").toLowerCase();
}

function isRegularNonSymlinkFile(filePath) {
  try {
    const st = lstatSync(filePath);
    return st.isFile() && !st.isSymbolicLink();
  } catch {
    return false;
  }
}

function isRegularNonSymlinkDir(dirPath) {
  try {
    const st = lstatSync(dirPath);
    return st.isDirectory() && !st.isSymbolicLink();
  } catch {
    return false;
  }
}

function collectFilesRecursive(dirPath, out = []) {
  for (const entry of readdirSync(dirPath, { withFileTypes: true })) {
    if (entry.isSymbolicLink()) continue;
    const full = join(dirPath, entry.name);
    if (entry.isDirectory()) {
      collectFilesRecursive(full, out);
    } else if (entry.isFile()) {
      out.push(full);
    }
  }
  return out;
}

function parseCargoLockChecksums(lockText) {
  const locked = new Map();
  for (const block of lockText.split("[[package]]")) {
    const nameMatch = block.match(/^name = "([^"]+)"/m);
    const versionMatch = block.match(/^version = "([^"]+)"/m);
    const checksumMatch = block.match(/^checksum = "([a-f0-9]{64})"/m);
    if (nameMatch && versionMatch && checksumMatch) {
      locked.set(`${nameMatch[1]}@${versionMatch[1]}`, checksumMatch[1]);
    }
  }
  return locked;
}

function checkWindowsPrebuilt(root, blockers) {
  const cacheDir = join(root, "target", "sherpa-onnx-prebuilt");
  const archive = join(cacheDir, `${EXPECTED_WIN_ARCHIVE_NAME}.tar.bz2`);
  const extracted = join(cacheDir, EXPECTED_WIN_ARCHIVE_NAME);

  if (!isRegularNonSymlinkFile(archive)) {
    blockers.push("pinned_archive_missing");
  } else {
    const st = lstatSync(archive);
    if (st.size !== EXPECTED_WIN_ARCHIVE_BYTES || sha256File(archive) !== EXPECTED_WIN_ARCHIVE_SHA256) {
      blockers.push("pinned_archive_sha256_or_size_mismatch");
    }
  }

  if (!isRegularNonSymlinkDir(extracted)) {
    blockers.push("pinned_archive_extraction_missing");
  } else {
    const libNames = new Set(
      collectFilesRecursive(extracted)
        .filter((f) => f.endsWith(".lib"))
        .map((f) => f.split(/[/\\]/).pop().slice(0, -4)),
    );
    for (const required of REQUIRED_NATIVE_LIBS) {
      if (!libNames.has(required)) {
        blockers.push(`required_native_library_missing:${required}`);
      }
    }
  }
}

function checkNativeSourceEvidence(root, platform, blockers) {
  const helperDir = join(root, "apps", "moss-voice-local");
  const evidencePath = join(helperDir, "native-source-evidence.json");
  const sourcesRoot = join(helperDir, "native-license-evidence");

  if (!isRegularNonSymlinkFile(evidencePath)) {
    blockers.push("corresponding_native_source_provenance_unreviewed");
    return;
  }

  let record;
  try {
    record = JSON.parse(readFileSync(evidencePath, "utf8"));
  } catch {
    blockers.push("native_source_evidence_invalid_json");
    return;
  }

  if (record?.review_status !== "approved" || typeof record?.reviewed_by !== "string" || !record.reviewed_by.trim()) {
    blockers.push("native_source_correspondence_review_pending");
  }
  if (Array.isArray(record?.limitations) && record.limitations.length > 0) {
    blockers.push("transitive_source_license_closure_incomplete");
  }
  if (platform === "macos" || platform === "all") {
    const platforms = Array.isArray(record?.platforms) ? record.platforms : [];
    if (!platforms.includes("macos") || typeof record?.macos_archive_sha256 !== "string" || !SHA256_RE.test(record.macos_archive_sha256)) {
      blockers.push("macos_native_license_provenance_unreviewed");
    }
  }

  const libraries = Array.isArray(record?.libraries) ? record.libraries : [];
  if (
    record?.archive_sha256 !== EXPECTED_WIN_ARCHIVE_SHA256 ||
    libraries.length !== REQUIRED_NATIVE_LIBS.length ||
    !isRegularNonSymlinkDir(sourcesRoot)
  ) {
    blockers.push("native_source_evidence_incomplete");
  } else {
    const safeRoot = resolve(sourcesRoot) + sep;
    for (const required of REQUIRED_NATIVE_LIBS) {
      const matches = libraries.filter((lib) => lib?.name === required);
      if (matches.length !== 1) {
        blockers.push(`native_source_evidence_missing_or_duplicate:${required}`);
        continue;
      }
      const entry = matches[0];
      const checks = [
        { file: entry.source_file, hash: entry.source_sha256, role: "source" },
        { file: entry.license_file, hash: entry.license_sha256, role: "license" },
      ];
      if (Array.isArray(entry.additional_licenses)) {
        for (const extra of entry.additional_licenses) {
          checks.push({ file: extra?.file, hash: extra?.sha256, role: "additional_license" });
        }
      }
      for (const check of checks) {
        const rel = String(check.file ?? "");
        const expectedHash = String(check.hash ?? "");
        if (!SAFE_REL_PATH_RE.test(rel) || /(^|\/)\.\.?(\/|$)/.test(rel) || !SHA256_RE.test(expectedHash)) {
          blockers.push(`native_source_evidence_invalid_path_or_hash:${required}:${check.role}`);
          continue;
        }
        const full = resolve(sourcesRoot, rel);
        if (!full.startsWith(safeRoot) || !isRegularNonSymlinkFile(full)) {
          blockers.push(`native_source_evidence_file_missing:${required}:${check.role}`);
          continue;
        }
        if (sha256File(full) !== expectedHash) {
          blockers.push(`native_source_evidence_hash_or_reparse_mismatch:${required}:${check.role}`);
        }
      }
    }
  }

}

function checkOrtEvidence(sourcesRoot, blockers) {
  const ortEvidencePath = join(sourcesRoot, "ort-dependency-evidence.json");
  if (!isRegularNonSymlinkFile(ortEvidencePath)) {
    blockers.push("ort_candidate_receipt_missing");
    return;
  }
  let ort;
  try {
    ort = JSON.parse(readFileSync(ortEvidencePath, "utf8"));
  } catch {
    blockers.push("ort_candidate_receipt_invalid_json");
    return;
  }
  if (ort?.release_ready !== true || ort?.review_status !== "approved" || typeof ort?.reviewed_by !== "string" || !ort.reviewed_by.trim()) {
    blockers.push("ort_candidate_receipt_unapproved");
  }
  if (ort?.root_archive_sha256 !== EXPECTED_ORT_ROOT_SHA256) {
    blockers.push("ort_root_archive_mismatch");
  }
  const dependencies = Array.isArray(ort?.dependencies) ? ort.dependencies : [];
  const collected = dependencies.filter((dependency) => dependency?.collection === "collected_candidate");
  if (dependencies.length !== 44 || collected.length !== 38
      || dependencies.some((dep) => typeof dep?.name !== "string" || !/^[a-z0-9_-]+$/.test(dep.name)
        || !["collected_candidate", "not_collected_binary"].includes(dep.collection))
      || !Array.isArray(ort?.uncollected_sources) || ort.uncollected_sources.length > 0
      || !Array.isArray(ort?.limitations) || ort.limitations.length > 0 || ort.gitlink_resolution_error) {
    blockers.push("ort_candidate_collection_incomplete");
    return;
  }
  // 固定 ORT 根包的 35 项源码、3 个 gitlink 和 6 个仅构建宿主使用的 protoc。
  const collectNames = (entries) => createHash("sha256")
    .update(entries.map((dep) => dep.name).sort().join("\n")).digest("hex");
  if (collectNames(collected) !== "c3953ee18312beb2e11c52b25e518597a20fbbafe96a437fe8914f0d243b4e59"
      || collectNames(dependencies.filter((dep) => dep.collection === "not_collected_binary"))
        !== "38b27be84af5f57042e4fd0bc7503903830ffc2fa00977a21aca25d3fc3840b8") {
    blockers.push("ort_dependency_set_mismatch");
  }
  const sourceHashes = new Set();
  for (const dep of collected) {
    if (!dep.name.startsWith("gitlink-") && !/^[a-f0-9]{40}$/.test(dep.upstream_sha1)) {
      blockers.push("ort_candidate_collection_incomplete");
      continue;
    }
    if (!Array.isArray(dep.license_files) || dep.license_files.length === 0) {
      blockers.push("ort_candidate_collection_incomplete");
      continue;
    }
    const files = [{ file: dep.source_file, sha256: dep.source_sha256, sha1: dep.upstream_sha1 }, ...dep.license_files];
    for (const [index, entry] of files.entries()) {
      if (!/^ort-dependencies\/[a-zA-Z0-9][a-zA-Z0-9._-]{0,180}$/.test(entry?.file)
          || !SHA256_RE.test(entry?.sha256)) {
        blockers.push("ort_evidence_invalid_path_or_hash");
        continue;
      }
      const file = join(sourcesRoot, entry.file);
      if (!isRegularNonSymlinkDir(join(sourcesRoot, "ort-dependencies")) || !isRegularNonSymlinkFile(file)) {
        blockers.push("ort_evidence_file_missing");
        continue;
      }
      const actualHash = sha256File(file);
      if (actualHash !== entry.sha256) {
        blockers.push("ort_evidence_bytes_changed");
        continue;
      }
      if (entry.sha1 && createHash("sha1").update(readFileSync(file)).digest("hex") !== entry.sha1) {
        blockers.push("ort_upstream_sha1_changed");
        continue;
      }
      // 只有首项是源码归档。不同依赖可以共用同一份许可文本。
      if (index === 0) {
        if (sourceHashes.has(actualHash)) blockers.push("ort_source_archive_duplicate");
        sourceHashes.add(actualHash);
      }
    }
  }
}

function checkThirdPartyNotices(root, blockers) {
  const noticesPath = join(root, "apps", "moss-voice-local", "THIRD_PARTY_NOTICES.md");
  if (!isRegularNonSymlinkFile(noticesPath)) {
    blockers.push("third_party_notices_missing");
    return;
  }
  const text = readFileSync(noticesPath, "utf8").toLowerCase();
  for (const required of REQUIRED_NATIVE_LIBS) {
    if (!text.includes(required.toLowerCase())) {
      blockers.push(`native_component_missing_from_notices:${required}`);
    }
  }
}

function checkRustSourceEvidence(root, blockers) {
  const helperDir = join(root, "apps", "moss-voice-local");
  const rustEvidencePath = join(helperDir, "rust-source-evidence.json");
  const rustRoot = join(helperDir, "rust-license-evidence");
  const rustLockPath = join(helperDir, "Cargo.lock");

  if (!isRegularNonSymlinkFile(rustEvidencePath)) {
    blockers.push("rust_source_evidence_missing");
    return;
  }
  if (!isRegularNonSymlinkFile(rustLockPath)) {
    blockers.push("rust_cargo_lock_missing");
    return;
  }

  let rust;
  try {
    rust = JSON.parse(readFileSync(rustEvidencePath, "utf8"));
  } catch {
    blockers.push("rust_source_evidence_invalid_json");
    return;
  }

  if (rust?.review_status !== "approved") {
    blockers.push("rust_source_review_pending");
  }

  const locked = parseCargoLockChecksums(readFileSync(rustLockPath, "utf8"));
  const packages = Array.isArray(rust?.packages) ? rust.packages : [];
  if (
    rust?.cargo_lock_sha256 !== sha256File(rustLockPath) ||
    packages.length !== locked.size ||
    rust?.registry_packages !== locked.size
  ) {
    blockers.push("rust_lock_manifest_mismatch");
  }
  if (Array.isArray(rust?.missing_license_texts) && rust.missing_license_texts.length > 0) {
    blockers.push("rust_license_text_missing");
  }

  const seen = new Set();
  for (const pkg of packages) {
    const key = `${pkg?.name ?? ""}@${pkg?.version ?? ""}`;
    if (seen.has(key) || !locked.has(key) || locked.get(key) !== pkg?.source_sha256) {
      blockers.push(`rust_package_lock_mismatch:${key}`);
      continue;
    }
    seen.add(key);
    if (Object.hasOwn(pkg, "review_status") && pkg.review_status !== "approved") {
      blockers.push(`rust_package_review_pending:${key}`);
    }
    const licenseFiles = Array.isArray(pkg.license_files) ? pkg.license_files : [];
    if (licenseFiles.length === 0) {
      blockers.push(`rust_package_license_missing:${key}`);
    }
    const checks = [
      { file: pkg.source_file, hash: pkg.source_sha256 },
      ...licenseFiles.map((l) => ({ file: l?.file, hash: l?.sha256 })),
    ];
    for (const check of checks) {
      const name = String(check.file ?? "");
      const hash = String(check.hash ?? "");
      if (!SAFE_FLAT_NAME_RE.test(name) || !SHA256_RE.test(hash)) {
        blockers.push(`rust_evidence_invalid:${key}`);
        continue;
      }
      const file = join(rustRoot, name);
      if (!isRegularNonSymlinkFile(file)) {
        blockers.push(`rust_evidence_missing:${key}`);
        continue;
      }
      if (sha256File(file) !== hash) {
        blockers.push(`rust_evidence_hash_or_reparse_mismatch:${key}`);
      }
    }
  }
}

/**
 * Evaluate whether external distribution of the voice helper sidecar is permitted.
 * Fail-closed: returns `{ ready: false, blockers: [...] }` until every required check passes.
 */
export function checkSidecarLicenseGate({ root = REPO_ROOT, platform = "all" } = {}) {
  if (!["all", "windows", "macos"].includes(platform)) {
    throw new Error(`Unsupported platform filter: ${platform}`);
  }
  const blockers = [];
  if (platform === "windows" || platform === "all") {
    checkWindowsPrebuilt(root, blockers);
  }
  checkNativeSourceEvidence(root, platform, blockers);
  checkOrtEvidence(join(root, "apps", "moss-voice-local", "native-license-evidence"), blockers);
  checkThirdPartyNotices(root, blockers);
  checkRustSourceEvidence(root, blockers);
  return {
    ready: blockers.length === 0,
    platform,
    blockers,
  };
}

function parseCliArgs(argv) {
  let root = REPO_ROOT;
  let platform = "all";
  let requireComplete = false;
  let json = false;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--require-complete") requireComplete = true;
    else if (arg === "--json") json = true;
    else if (arg === "--platform") platform = argv[++i] ?? "";
    else if (arg === "--root") root = resolve(argv[++i] ?? "");
    else throw new Error(`Unknown argument: ${arg}`);
  }
  return { root, platform, requireComplete, json };
}

if (process.argv[1] && existsSync(process.argv[1])
    && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))) {
  const opts = parseCliArgs(process.argv.slice(2));
  const result = checkSidecarLicenseGate({ root: opts.root, platform: opts.platform });
  if (opts.json) {
    console.log(JSON.stringify(result, null, 2));
  } else {
    for (const blocker of result.blockers) {
      console.log(`NV18_SIDECAR_BLOCKER=${blocker}`);
    }
    console.log(`NV18_SIDECAR_RELEASE_READY=${result.ready}`);
    console.log(`NV18_SIDECAR_BLOCKER_COUNT=${result.blockers.length}`);
  }
  if (opts.requireComplete && !result.ready) {
    console.error("Native-license sidecar release gate FAIL-CLOSED: cannot distribute sidecar package.");
    process.exitCode = 1;
  }
}
