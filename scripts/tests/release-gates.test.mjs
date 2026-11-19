// 发布门禁、Android 固定签名、macOS 手动标签校验与 CI 覆盖回归测试。
import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdtempSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { checkSidecarLicenseGate, EXPECTED_ORT_ROOT_SHA256, REQUIRED_NATIVE_LIBS } from "../voice-helper/check-sidecar-license-gate.mjs";
import { resolveSuiteFiles } from "../test_e2e.mjs";

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const readRepo = (rel) => readFileSync(join(REPO_ROOT, rel), "utf8");

function sha256Hex(buf) {
  return createHash("sha256").update(buf).digest("hex").toLowerCase();
}

function extractBetweenMarkers(text, beginMarker, endMarker) {
  const start = text.indexOf(beginMarker);
  const end = text.indexOf(endMarker);
  assert.ok(start >= 0 && end > start, `Missing markers ${beginMarker}..${endMarker}`);
  return text.slice(start + beginMarker.length, end);
}

function stripYamlIndent(block) {
  const lines = block.replace(/\r\n/g, "\n").split("\n");
  const indents = lines.filter((l) => l.trim().length > 0).map((l) => l.match(/^ */)[0].length);
  const minIndent = indents.length > 0 ? Math.min(...indents) : 0;
  return lines.map((l) => l.slice(minIndent)).join("\n");
}

test("Android release verification accepts only the configured certificate", {
  skip: process.platform === "win32",
}, () => {
  const yaml = readRepo(".github/workflows/release.yml");
  assert.match(yaml, /keytool -exportcert -keystore/);
  assert.match(yaml, /MOSS_ANDROID_EXPECTED_CERT_SHA256=%s/);
  assert.match(yaml, /ANDROID_RELEASE_BUILD_TOOLS: '36\.0\.0'/);
  assert.match(yaml, /"\$SDKMGR" --install .*"build-tools;\$ANDROID_RELEASE_BUILD_TOOLS"/);
  assert.doesNotMatch(yaml, /keytool -printcert -jarfile/);
  const shell = stripYamlIndent(extractBetweenMarkers(
    yaml,
    '          test -s "$APK_SRC"\n',
    '          cp "$APK_SRC"',
  ));
  const root = mkdtempSync(join(tmpdir(), "moss-android-cert-"));
  try {
    const tools = join(root, "build-tools", "36.0.0");
    mkdirSync(tools, { recursive: true });
    writeFileSync(join(tools, "apksigner"), [
      "#!/usr/bin/env bash",
      '[[ "$1" == verify && "$2" == --print-certs && "$3" == synthetic.apk ]] || exit 2',
      'printf "%s\n" "$MOSS_FIXTURE_APK_CERTS"',
      'exit "$MOSS_FIXTURE_APKSIGNER_EXIT"',
      "",
    ].join("\n"), { mode: 0o755 });
    const expected = "a".repeat(64);
    const cert = (id, hash) => "Signer #" + id + " certificate SHA-256 digest: " + hash;
    const newerTools = join(root, "build-tools", "37.0.0");
    mkdirSync(newerTools, { recursive: true });
    writeFileSync(join(newerTools, "apksigner"), [
      "#!/usr/bin/env bash",
      'printf "V2 Signer: certificate SHA-256 digest: %s\\n" "$MOSS_ANDROID_EXPECTED_CERT_SHA256"',
      "",
    ].join("\n"), { mode: 0o755 });
    const verify = (certs, exitCode = "0", sdk = root) => spawnSync(
      "bash",
      ["-euo", "pipefail", "-c", shell],
      {
        cwd: root,
        encoding: "utf8",
        env: {
          PATH: process.env.PATH,
          ANDROID_HOME: sdk,
          ANDROID_RELEASE_BUILD_TOOLS: "36.0.0",
          APK_SRC: "synthetic.apk",
          MOSS_ANDROID_EXPECTED_CERT_SHA256: expected,
          MOSS_FIXTURE_APK_CERTS: certs,
          MOSS_FIXTURE_APKSIGNER_EXIT: exitCode,
        },
      },
    );
    const good = verify(cert(1, expected.toUpperCase()));
    assert.equal(good.status, 0, good.stderr);
    const wrongDigest = "b".repeat(64);
    const wrongSigner = verify(cert(1, wrongDigest));
    assert.ok(wrongSigner.stderr.includes(`Expected certificate SHA-256: ${expected}`));
    assert.ok(wrongSigner.stderr.includes(`APK certificate SHA-256: ${wrongDigest}`));
    const invalid = [
      wrongSigner,
      verify(""),
      verify(cert(1, expected) + "\n" + cert(2, expected)),
      verify(cert(1, expected), "1"),
      verify(cert(1, expected), "0", join(root, "missing-sdk")),
    ];
    for (const rejected of invalid) {
      assert.ok(rejected.status > 0, rejected.stderr);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

function createApprovedSidecarFixture(rootDir) {
  const helperDir = join(rootDir, "apps", "moss-voice-local");
  const nativeDir = join(helperDir, "native-license-evidence");
  const rustDir = join(helperDir, "rust-license-evidence");
  mkdirSync(nativeDir, { recursive: true });
  mkdirSync(rustDir, { recursive: true });

  const lockText = `[[package]]
name = "demo-crate"
version = "1.2.3"
checksum = "${sha256Hex(Buffer.from("crate-bytes"))}"
`;
  writeFileSync(join(helperDir, "Cargo.lock"), lockText, "utf8");
  writeFileSync(
    join(helperDir, "THIRD_PARTY_NOTICES.md"),
    REQUIRED_NATIVE_LIBS.map((name) => `- ${name}`).join("\n") + "\n",
    "utf8",
  );

  const srcBytes = Buffer.from("upstream-source-tar");
  const licBytes = Buffer.from("MIT License");
  writeFileSync(join(nativeDir, "src.tar.gz"), srcBytes);
  writeFileSync(join(nativeDir, "LICENSE.txt"), licBytes);

  writeFileSync(
    join(helperDir, "native-source-evidence.json"),
    JSON.stringify({
      archive_sha256: "56ffcf3c454c1f14f7bc9887286cc8143e7e542dc632804e1c447d5f8d534eaf",
      macos_archive_sha256: "1".repeat(64),
      platforms: ["windows", "macos"],
      review_status: "approved",
      reviewed_by: "license-reviewer",
      limitations: [],
      libraries: REQUIRED_NATIVE_LIBS.map((name) => ({
        name,
        source_file: "src.tar.gz",
        source_sha256: sha256Hex(srcBytes),
        license_file: "LICENSE.txt",
        license_sha256: sha256Hex(licBytes),
      })),
    }),
    "utf8",
  );

  const ortDir = join(nativeDir, "ort-dependencies");
  mkdirSync(ortDir);
  writeFileSync(join(ortDir, "LICENSE.txt"), licBytes);
  const sourceNames = ("abseil_cpp coremltools cudnn_frontend cutlass cxxopts date dawn directx_headers dlpack eigen " +
    "extensions flatbuffers fp16 fxdiv gitlink-emsdk gitlink-libprotobuf-mutator gitlink-onnx google_benchmark " +
    "googletest googlexnnpack json kleidiai kleidiai-qmx microsoft_gsl microsoft_wil mimalloc mp11 onnx " +
    "onnx_tensorrt protobuf psimd pthreadpool pybind11 pytorch_cpuinfo re2 safeint tensorboard vulkan_headers").split(" ");
  const binaries = [
    "protoc_linux_aarch64", "protoc_linux_x64", "protoc_linux_x86", "protoc_mac_universal", "protoc_win32", "protoc_win64",
  ];
  const dependencies = sourceNames.map((name) => {
    const archive = Buffer.from(`fixture source: ${name}`);
    writeFileSync(join(ortDir, `${name}.tar.gz`), archive);
    return {
      name, collection: "collected_candidate", source_file: `ort-dependencies/${name}.tar.gz`,
      source_sha256: sha256Hex(archive), upstream_sha1: createHash("sha1").update(archive).digest("hex"),
      license_files: [{ file: "ort-dependencies/LICENSE.txt", sha256: sha256Hex(licBytes) }],
    };
  }).concat(binaries.map((name) => ({ name, collection: "not_collected_binary" })));
  writeFileSync(join(nativeDir, "ort-dependency-evidence.json"), JSON.stringify({
    release_ready: true,
    review_status: "approved",
    reviewed_by: "license-reviewer",
    root_archive_sha256: EXPECTED_ORT_ROOT_SHA256,
    gitlink_resolution_error: null,
    uncollected_sources: [],
    limitations: [],
    dependencies,
  }), "utf8");

  const crateBytes = Buffer.from("crate-bytes");
  const crateLicBytes = Buffer.from("Apache-2.0");
  writeFileSync(join(rustDir, "demo-crate-1.2.3.crate"), crateBytes);
  writeFileSync(join(rustDir, "demo-crate-1.2.3-LICENSE.txt"), crateLicBytes);
  writeFileSync(
    join(helperDir, "rust-source-evidence.json"),
    JSON.stringify({
      review_status: "approved",
      cargo_lock_sha256: sha256Hex(Buffer.from(lockText)),
      registry_packages: 1,
      missing_license_texts: [],
      packages: [
        {
          name: "demo-crate",
          version: "1.2.3",
          review_status: "approved",
          source_file: "demo-crate-1.2.3.crate",
          source_sha256: sha256Hex(crateBytes),
          license_files: [
            {
              file: "demo-crate-1.2.3-LICENSE.txt",
              sha256: sha256Hex(crateLicBytes),
            },
          ],
        },
      ],
    }),
    "utf8",
  );
}

test("[#21] 真实仓库的 sidecar 原生许可门禁在未闭环前 fail-closed 拒绝分发", () => {
  for (const platform of ["all", "windows", "macos"]) {
    const res = checkSidecarLicenseGate({ root: REPO_ROOT, platform });
    assert.equal(res.ready, false, `platform=${platform} 必须拒绝未审核 sidecar`);
    assert.ok(res.blockers.includes("native_source_correspondence_review_pending"));
    assert.ok(res.blockers.includes("transitive_source_license_closure_incomplete"));
    assert.ok(res.blockers.includes("rust_source_review_pending"));
  }

  const cli = spawnSync(
    process.execPath,
    ["scripts/voice-helper/check-sidecar-license-gate.mjs", "--platform", "all", "--require-complete"],
    { cwd: REPO_ROOT, encoding: "utf8" },
  );
  assert.notEqual(cli.status, 0, "--require-complete 必须返回非零退出码");
  assert.match(cli.stdout, /NV18_SIDECAR_RELEASE_READY=false/);
});

test("[#21] check-sidecar-license-gate.mjs 对完整审批与各类失败路径判定准确", () => {
  const tmp = mkdtempSync(join(tmpdir(), "moss-license-gate-"));
  try {
    createApprovedSidecarFixture(tmp);
    const ok = checkSidecarLicenseGate({ root: tmp, platform: "macos" });
    assert.deepEqual(ok, { ready: true, platform: "macos", blockers: [] });

    // Windows 平台必须同时核对固定预编译 archive
    const winMissing = checkSidecarLicenseGate({ root: tmp, platform: "windows" });
    assert.equal(winMissing.ready, false);
    assert.ok(winMissing.blockers.includes("pinned_archive_missing"));

    // 1. 存在未闭环 limitations 必须拒绝
    const evPath = join(tmp, "apps", "moss-voice-local", "native-source-evidence.json");
    const ev = JSON.parse(readFileSync(evPath, "utf8"));
    writeFileSync(evPath, JSON.stringify({ ...ev, limitations: ["open"] }), "utf8");
    assert.ok(checkSidecarLicenseGate({ root: tmp, platform: "macos" }).blockers.includes("transitive_source_license_closure_incomplete"));

    // 2. 缺失 macOS provenance 必须拒绝
    writeFileSync(evPath, JSON.stringify({ ...ev, platforms: ["windows"] }), "utf8");
    assert.ok(checkSidecarLicenseGate({ root: tmp, platform: "macos" }).blockers.includes("macos_native_license_provenance_unreviewed"));

    // 3. ORT 候选未审批清单不得自批
    writeFileSync(evPath, JSON.stringify(ev), "utf8");
    const ortPath = join(tmp, "apps", "moss-voice-local", "native-license-evidence", "ort-dependency-evidence.json");
    const approved = JSON.parse(readFileSync(ortPath, "utf8"));
    writeFileSync(ortPath, JSON.stringify({ ...approved, release_ready: false, review_status: "pending" }), "utf8");
    const pending = checkSidecarLicenseGate({ root: tmp, platform: "macos" });
    assert.equal(pending.ready, false);
    assert.deepEqual(pending.blockers, ["ort_candidate_receipt_unapproved"]);
    writeFileSync(ortPath, JSON.stringify(approved), "utf8");
    assert.equal(checkSidecarLicenseGate({ root: tmp, platform: "macos" }).ready, true);
    writeFileSync(ortPath, JSON.stringify({ ...approved, dependencies: approved.dependencies.slice(1) }), "utf8");
    assert.ok(checkSidecarLicenseGate({ root: tmp, platform: "macos" }).blockers.includes("ort_candidate_collection_incomplete"));
    const [first, ...rest] = approved.dependencies;
    writeFileSync(ortPath, JSON.stringify({ ...approved, dependencies: [{ ...first, name: "fake" }, ...rest] }), "utf8");
    assert.ok(checkSidecarLicenseGate({ root: tmp, platform: "macos" }).blockers.includes("ort_dependency_set_mismatch"));
    writeFileSync(ortPath, JSON.stringify(approved), "utf8");
    const ortLicense = join(tmp, "apps", "moss-voice-local", "native-license-evidence", "ort-dependencies", "LICENSE.txt");
    writeFileSync(ortLicense, "证据字节被改动");
    assert.ok(checkSidecarLicenseGate({ root: tmp, platform: "macos" }).blockers.includes("ort_evidence_bytes_changed"));

    // 4. 证据文件改为软链必须拒绝
    const licPath = join(tmp, "apps", "moss-voice-local", "native-license-evidence", "LICENSE.txt");
    rmSync(licPath);
    symlinkSync(join(tmp, "apps", "moss-voice-local", "native-license-evidence", "src.tar.gz"), licPath);
    assert.ok(
      checkSidecarLicenseGate({ root: tmp, platform: "macos" }).blockers.some((b) =>
        b.startsWith("native_source_evidence_file_missing:"),
      ),
    );
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
});

test("ORT 许可门禁拒绝同一份源码归档改名复制", (t) => {
  const tmp = mkdtempSync(join(tmpdir(), "moss-ort-duplicate-"));
  t.after(() => rmSync(tmp, { recursive: true, force: true }));
  createApprovedSidecarFixture(tmp);
  assert.equal(checkSidecarLicenseGate({ root: tmp, platform: "macos" }).ready, true);

  const nativeDir = join(tmp, "apps", "moss-voice-local", "native-license-evidence");
  const receiptPath = join(nativeDir, "ort-dependency-evidence.json");
  const receipt = JSON.parse(readFileSync(receiptPath, "utf8"));
  const [original, duplicate] = receipt.dependencies;
  const renamed = "ort-dependencies/renamed.tar.gz";
  copyFileSync(join(nativeDir, original.source_file), join(nativeDir, renamed));
  Object.assign(duplicate, {
    source_file: renamed,
    source_sha256: original.source_sha256,
    upstream_sha1: original.upstream_sha1,
  });
  writeFileSync(receiptPath, JSON.stringify(receipt));

  assert.deepEqual(checkSidecarLicenseGate({ root: tmp, platform: "macos" }), {
    ready: false,
    platform: "macos",
    blockers: ["ort_source_archive_duplicate"],
  });
});

test("[#21] release.yml 的 latest.json 生成逻辑按真实构建产物发布并拦截未授权 sidecar 文件", () => {
  const releaseYml = readRepo(".github/workflows/release.yml");
  const rawBlock = extractBetweenMarkers(releaseYml, "# BEGIN LATEST_JSON_GENERATOR", "# END LATEST_JSON_GENERATOR");
  const dedented = stripYamlIndent(rawBlock);
  const pyMatch = dedented.match(/python3 - "\$VERSION" "\$TAG" "\$GITHUB_REPOSITORY" "\$SIDECAR_READY" <<'PY'\n([\s\S]*?)\nPY/);
  assert.ok(pyMatch, "应能提取 release.yml 的 latest.json Python 脚本");
  const pyCode = pyMatch[1];

  const runGenerator = (distFiles, { ver = "0.4.0", tag = "v0.4.0", repo = "yjdaaa/moss", sidecarReady = "false" } = {}) => {
    const work = mkdtempSync(join(tmpdir(), "moss-manifest-"));
    try {
      const dist = join(work, "dist");
      mkdirSync(dist, { recursive: true });
      for (const [name, content] of Object.entries(distFiles)) {
        writeFileSync(join(dist, name), content);
      }
      const res = spawnSync("python3", ["-", ver, tag, repo, sidecarReady], {
        cwd: work,
        input: pyCode,
        encoding: "utf8",
      });
      const latestPath = join(dist, "latest.json");
      const latest = res.status === 0 && readFileSyncSafe(latestPath);
      return { status: res.status, stdout: res.stdout, stderr: res.stderr, latest, remainingFiles: readdirNames(dist) };
    } finally {
      rmSync(work, { recursive: true, force: true });
    }
  };

  function readFileSyncSafe(p) {
    try {
      return JSON.parse(readFileSync(p, "utf8"));
    } catch {
      return null;
    }
  }
  function readdirNames(dir) {
    return spawnSync("ls", ["-A", dir], { encoding: "utf8" }).stdout.trim().split("\n").filter(Boolean);
  }

  // Case A: 普通包发布（sidecar_ready=false）：只生成 windows-x86_64-lite 与 android-aarch64，不生成 darwin-* 与 offline-voice
  const ordinary = runGenerator(
    {
      "MOSS_0.4.0_x64-lite-setup.exe": "lite",
      "MOSS_0.4.0_x64-lite-setup.exe.sig": "sig-lite",
      "MOSS_0.4.0_windows_x64_portable.exe": "portable",
      "MOSS_0.4.0_macos_universal.dmg": "dmg",
      "MOSS_0.4.0_android_aarch64.apk": "apk",
      "MOSS_0.4.0_android_aarch64.apk.sig": "sig-apk",
    },
    { sidecarReady: "false" },
  );
  assert.equal(ordinary.status, 0, ordinary.stderr);
  assert.deepEqual(Object.keys(ordinary.latest.platforms).sort(), [
    "android-aarch64",
    "windows-x86_64-lite",
  ]);
  assert.equal("darwin-aarch64" in ordinary.latest.platforms, false);
  assert.equal("darwin-x86_64" in ordinary.latest.platforms, false);
  assert.equal("windows-x86_64-offline-voice" in ordinary.latest.platforms, false);
  assert.equal(ordinary.remainingFiles.some((f) => f.endsWith(".sig")), false, ".sig 不得保留进发布目录");

  // Case B1: sidecar_ready=false 时混入 offline-voice 或 gpl-source 必须 fail-closed
  const leaked = runGenerator(
    {
      "MOSS_0.4.0_x64-lite-setup.exe": "lite",
      "MOSS_0.4.0_x64-offline-voice-setup.exe": "leaked-sidecar",
    },
    { sidecarReady: "false" },
  );
  assert.notEqual(leaked.status, 0, "未过门禁混入 sidecar 文件必须失败");

  // Case B2: sidecar_ready=false 时混入 macOS .app.tar.gz 更新包必须 fail-closed（防覆盖旧版带 helper 的 .app）
  const leakedDarwinUpdater = runGenerator(
    {
      "MOSS_0.4.0_x64-lite-setup.exe": "lite",
      "MOSS_0.4.0_x64-lite-setup.exe.sig": "sig-lite",
      "MOSS_0.4.0_macos_universal.dmg": "dmg",
      "MOSS_0.4.0_macos_universal.app.tar.gz": "tar",
      "MOSS_0.4.0_macos_universal.app.tar.gz.sig": "sig-mac",
      "MOSS_0.4.0_android_aarch64.apk": "apk",
      "MOSS_0.4.0_android_aarch64.apk.sig": "sig-apk",
    },
    { sidecarReady: "false" },
  );
  assert.notEqual(leakedDarwinUpdater.status, 0, "未过门禁不得产出或发布 darwin 应用内更新包");

  // Case C: sidecar_ready=true 时恢复 windows-x86_64-offline-voice 与 darwin-aarch64 / darwin-x86_64
  const full = runGenerator(
    {
      "MOSS_0.4.0_x64-lite-setup.exe": "lite",
      "MOSS_0.4.0_x64-lite-setup.exe.sig": "sig-lite",
      "MOSS_0.4.0_x64-offline-voice-setup.exe": "voice",
      "MOSS_0.4.0_x64-offline-voice-setup.exe.sig": "sig-voice",
      "moss-voice-local-0.4.0-gpl-source.zip": "src",
      "MOSS_0.4.0_macos_universal.app.tar.gz": "tar",
      "MOSS_0.4.0_macos_universal.app.tar.gz.sig": "sig-mac",
      "MOSS_0.4.0_android_aarch64.apk": "apk",
      "MOSS_0.4.0_android_aarch64.apk.sig": "sig-apk",
    },
    { sidecarReady: "true" },
  );
  assert.equal(full.status, 0, full.stderr);
  assert.deepEqual(Object.keys(full.latest.platforms).sort(), [
    "android-aarch64",
    "darwin-aarch64",
    "darwin-x86_64",
    "windows-x86_64-lite",
    "windows-x86_64-offline-voice",
  ]);

  // Case D: 无签名私钥时跳过 latest.json（未过门禁与过门禁均清理或跳过）
  const unsigned = runGenerator({
    "MOSS_0.4.0_x64-lite-setup.exe": "lite",
    "MOSS_0.4.0_macos_universal.dmg": "dmg",
    "MOSS_0.4.0_android_aarch64.apk": "apk",
  });
  assert.equal(unsigned.status, 0, unsigned.stderr);
  assert.equal(unsigned.latest, null);

  // Case E: 只有部分签名必须 fail-closed
  const partialSig = runGenerator({
    "MOSS_0.4.0_x64-lite-setup.exe": "lite",
    "MOSS_0.4.0_x64-lite-setup.exe.sig": "sig-lite",
    "MOSS_0.4.0_macos_universal.dmg": "dmg",
    "MOSS_0.4.0_android_aarch64.apk": "apk",
  });
  assert.notEqual(partialSig.status, 0, "部分缺失更新签名必须失败");
});

test("[#21/#22] 打包脚本、许可审计与 Android 正式签名门禁全部 fail-closed", () => {
  const auditPs1 = readRepo("scripts/voice-helper/audit-native-licenses.ps1");
  assert.match(auditPs1, /rust\.review_status -ne 'approved'/);
  assert.match(auditPs1, /ort\.release_ready -ne \$true -or \$ort\.review_status -ne 'approved'/);
  // native-source-evidence.json 核查必须在 $extracted 缺失分支外无条件执行
  const extractedCloseIdx = auditPs1.indexOf("NV18_ARCHIVE_HAS_NO_EMBEDDED_LICENSES_SIDECAR_EVIDENCE_REQUIRED=True");
  const evidenceCheckIdx = auditPs1.indexOf("NV18_REVIEWED_NATIVE_SOURCE_EVIDENCE_PRESENT=");
  const extractedElseIdx = auditPs1.indexOf("if (-not (Test-Path -LiteralPath $extracted");
  assert.ok(extractedElseIdx >= 0 && extractedCloseIdx > extractedElseIdx && evidenceCheckIdx > extractedCloseIdx);

  const sidecarPs1 = readRepo("scripts/voice-helper/build-sidecar-installer.ps1");
  assert.match(sidecarPs1, /\[switch\]\$AllowUnauditedInternal/);
  assert.match(sidecarPs1, /audit-native-licenses\.ps1/);
  assert.match(sidecarPs1, /-RequireComplete/);
  assert.ok(sidecarPs1.indexOf("& cargo build --release") < sidecarPs1.indexOf("& $auditScript -RequireComplete"));
  assert.ok(sidecarPs1.indexOf("& $auditScript -RequireComplete") < sidecarPs1.indexOf("Compress-Archive"));
  assert.match(sidecarPs1, /Refusing -EmitGplSourceZip with -AllowUnauditedInternal/);

  const macosSh = readRepo("scripts/build-macos-installer.sh");
  assert.match(macosSh, /^WITH_SIDECAR=0$/m, "macOS 打包脚本默认构建不含 sidecar 的普通包");
  assert.doesNotMatch(macosSh, /audit-native-licenses\.ps1/);
  assert.match(macosSh, /check-sidecar-license-gate\.mjs"? --platform macos --require-complete/);
  assert.match(macosSh, /Unexpected unaudited voice sidecar found/);
  const bashSyntax = spawnSync("bash", ["-n", "scripts/build-macos-installer.sh"], { cwd: REPO_ROOT, encoding: "utf8" });
  assert.equal(bashSyntax.status, 0, bashSyntax.stderr);

  const releaseYml = readRepo(".github/workflows/release.yml");
  assert.doesNotMatch(releaseYml, /keytool -genkeypair/, "正式发布绝不允许临时生成 Android 签名证书");
  assert.doesNotMatch(releaseYml, /ANDROID_FIXED_SIGNING=false/);
  assert.match(releaseYml, /ANDROID_KEYSTORE_BASE64.*ANDROID_KEY_ALIAS.*ANDROID_KEY_PASSWORD/);
  assert.match(releaseYml, /keytool -list -keystore/);
  assert.match(releaseYml, /MOSS_REQUIRE_ANDROID_RELEASE_SIGNING=true/);
  assert.match(releaseYml, /\$APKSIGNER"?\s+verify/);
  assert.match(releaseYml, /if:\s*needs\.prepare\.outputs\.sidecar_release_ready == 'true'/);
  assert.match(releaseYml, /不含离线语音/);
  assert.doesNotMatch(releaseYml, /audit-native-licenses\.ps1|--platform all/);
  assert.match(releaseYml, /id: license_gate[\s\S]*?--platform macos --require-complete/);
  assert.match(releaseYml, /TAURI_SIGNING_PRIVATE_KEY="" TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""\s*\\\s*\n\s*bash scripts\/build-macos-installer\.sh --without-voice-sidecar/);
  assert.match(releaseYml, /if \[\[ "\$SIDECAR_READY" == "true" && -n "\$TAURI_SIGNING_PRIVATE_KEY" \]\]/);
  assert.match(releaseYml, /if \[\[ "\$SIDECAR_READY" == "true" && -f "dist\/MOSS_\$\{VERSION\}_macos_universal\.app\.tar\.gz" \]\]/);

  const androidAppGradle = readRepo("apps/desktop/src-tauri/gen/android/app/build.gradle.kts");
  assert.match(androidAppGradle, /MOSS_REQUIRE_ANDROID_RELEASE_SIGNING/);
  assert.match(androidAppGradle, /"keyAlias", "password", "storeFile"/);
});

test("[#25] CI 覆盖 Windows UI E2E、helper 编译测试与 Android 无上传 release 烟测", () => {
  assert.deepEqual(resolveSuiteFiles([]), [
    "frontend/tests/e2e/api.test.mjs",
    "frontend/tests/e2e/workbench.ui.test.mjs",
  ]);
  assert.deepEqual(resolveSuiteFiles(["--api-only"]), ["frontend/tests/e2e/api.test.mjs"]);
  assert.deepEqual(resolveSuiteFiles([], { MOSS_E2E_API_ONLY: "1" }), ["frontend/tests/e2e/api.test.mjs"]);

  const ciYml = readRepo(".github/workflows/ci.yml");
  assert.doesNotMatch(ciYml, /--api-only/, "CI Windows E2E 不得跳过 UI E2E");
  assert.match(ciYml, /node scripts\/test_e2e\.mjs/);
  assert.match(ciYml, /working-directory:\s*frontend[\s\S]*npm ci/);
  assert.match(ciYml, /working-directory:\s*apps\/moss-voice-local[\s\S]*cargo clippy --all-targets -- -D warnings/);
  assert.match(ciYml, /working-directory:\s*apps\/moss-voice-local[\s\S]*cargo test/);
  assert.match(ciYml, /working-directory:\s*apps\/moss-voice-local[\s\S]*cargo build --release/);
  assert.doesNotMatch(ciYml, /moss-voice-local\.exe/, "Windows CI 工件不得上传未经许可闭环的 helper");

  assert.match(ciYml, /path:\s*apps\/desktop\/src-tauri\/gen\/android\/app\/build\/outputs\/apk\/universal\/debug\/\*\.apk/);
  assert.match(ciYml, /cargo tauri android build --target aarch64 --apk --ci/);
  assert.match(ciYml, /app-universal-release-unsigned\.apk/);
});

test("[#26/#21] release-macos.yml 先解析绑定目标标签与版本再 checkout，且默认禁用未闭环 sidecar", () => {
  const macYml = readRepo(".github/workflows/release-macos.yml");
  const metaPos = macYml.indexOf("id: meta");
  const checkoutPos = macYml.indexOf("uses: actions/checkout@v4");
  const buildPos = macYml.indexOf("Build Universal macOS .app & .dmg");
  const attachPos = macYml.indexOf("Attach macOS DMG to GitHub Release");
  assert.ok(metaPos > 0 && checkoutPos > metaPos && buildPos > checkoutPos && attachPos > buildPos);
  assert.match(macYml, /ref:\s*\$\{\{\s*steps\.meta\.outputs\.checkout_ref\s*\}\}/);
  assert.match(macYml, /include_voice_sidecar:[\s\S]*default:\s*false/);
  assert.match(macYml, /CFBundleShortVersionString/);
  assert.doesNotMatch(macYml, /audit-native-licenses\.ps1|--platform all/);
  assert.match(macYml, /check-sidecar-license-gate\.mjs --platform macos --require-complete/);

  const rawResolver = extractBetweenMarkers(macYml, "# BEGIN MACOS_META_RESOLVER", "# END MACOS_META_RESOLVER");
  const resolver = stripYamlIndent(rawResolver);

  const runResolver = (env) => {
    const tmp = mkdtempSync(join(tmpdir(), "moss-mac-meta-"));
    const outFile = join(tmp, "gh_output.txt");
    writeFileSync(outFile, "", "utf8");
    try {
      const res = spawnSync("bash", ["-euo", "pipefail", "-c", resolver], {
        env: { ...process.env, GITHUB_OUTPUT: outFile, MOSS_TEST_SKIP_GH_LOOKUP: "1", REPO: "yjdaaa/moss", ...env },
        encoding: "utf8",
      });
      const outputs = Object.fromEntries(
        readFileSync(outFile, "utf8")
          .trim()
          .split("\n")
          .filter(Boolean)
          .map((line) => {
            const idx = line.indexOf("=");
            return [line.slice(0, idx), line.slice(idx + 1)];
          }),
      );
      return { status: res.status, stderr: res.stderr, outputs };
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  };

  // 1. 手动指定历史 release_tag：先绑定 refs/tags/v0.3.2
  const hist = runResolver({ INPUT_TAG: " v0.3.2 ", TRIGGER_REF: "refs/heads/main", INCLUDE_SIDECAR: "false" });
  assert.equal(hist.status, 0, hist.stderr);
  assert.deepEqual(hist.outputs, {
    tag: "v0.3.2",
    version: "0.3.2",
    checkout_ref: "refs/tags/v0.3.2",
    attach_release: "true",
    include_sidecar: "false",
  });

  // 2. 非法标签必须直接拒绝
  for (const badTag of ["latest", "v1.2", "v0.3.2;echo pwn", "0.3.2"]) {
    const bad = runResolver({ INPUT_TAG: badTag, TRIGGER_REF: "refs/heads/main", INCLUDE_SIDECAR: "false" });
    assert.notEqual(bad.status, 0, `非法标签 ${badTag} 必须失败`);
  }

  // 3. 未指定 release_tag 且分支触发：只保留构建产物，不附加 Release
  const branchOnly = runResolver({ INPUT_TAG: "", TRIGGER_REF: "refs/heads/main", INCLUDE_SIDECAR: "false" });
  assert.equal(branchOnly.status, 0, branchOnly.stderr);
  assert.deepEqual(branchOnly.outputs, {
    tag: "",
    version: "",
    checkout_ref: "refs/heads/main",
    attach_release: "false",
    include_sidecar: "false",
  });

  // 4. 标签触发：自动绑定该标签
  const tagTrig = runResolver({ INPUT_TAG: "", TRIGGER_REF: "refs/tags/v0.3.9", INCLUDE_SIDECAR: "true" });
  assert.equal(tagTrig.status, 0, tagTrig.stderr);
  assert.deepEqual(tagTrig.outputs, {
    tag: "v0.3.9",
    version: "0.3.9",
    checkout_ref: "refs/tags/v0.3.9",
    attach_release: "true",
    include_sidecar: "true",
  });
});

test("通过符号链接启动 sidecar 门禁时仍检查并拒绝未闭环许可", (t) => {
  const dir = mkdtempSync(join(tmpdir(), "moss-sidecar-cli-"));
  try {
    const cli = join(dir, "gate.mjs");
    try {
      symlinkSync(join(REPO_ROOT, "scripts/voice-helper/check-sidecar-license-gate.mjs"), cli);
    } catch (error) {
      if (process.platform !== "win32" || error.code !== "EPERM") throw error;
      t.skip("Windows 未允许创建文件符号链接");
      return;
    }
    const run = spawnSync(process.execPath, [cli, "--root", dir, "--platform", "macos", "--require-complete"], { encoding: "utf8" });
    assert.equal(run.status, 1, run.stderr);
    assert.match(run.stdout, /NV18_SIDECAR_RELEASE_READY=false/);
    assert.match(run.stdout, /ort_candidate_receipt_missing/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("release.yml 的发布说明生成器对超长 CHANGELOG 自动按行截断以避开 GitHub 125000 字符上限", () => {
  const releaseYml = readRepo(".github/workflows/release.yml");
  const rawBlock = extractBetweenMarkers(releaseYml, "# BEGIN RELEASE_NOTES_GENERATOR", "# END RELEASE_NOTES_GENERATOR");
  const dedented = stripYamlIndent(rawBlock);
  const pyMatch = dedented.match(
    /python3 - "\$VERSION" "\$TAG" "\$SIDECAR_READY" "\$GITHUB_REPOSITORY" > release-notes\.md <<'PY'\n([\s\S]*?)\nPY/,
  );
  assert.ok(pyMatch, "应能提取 release.yml 的发布说明生成 Python 脚本");
  const pyCode = pyMatch[1];

  const runNotes = (changelogText, { ver = "0.3.12", tag = "v0.3.12", sidecarReady = "false", repo = "yjdaaa/moss" } = {}) => {
    const work = mkdtempSync(join(tmpdir(), "moss-notes-"));
    try {
      mkdirSync(join(work, "dist"), { recursive: true });
      writeFileSync(join(work, "CHANGELOG.md"), changelogText);
      writeFileSync(join(work, "dist", "SHA256SUMS.txt"), "deadbeef  MOSS_0.3.12_x64-lite-setup.exe\n");
      return spawnSync("python3", ["-", ver, tag, sidecarReady, repo], {
        cwd: work,
        input: pyCode,
        encoding: "utf8",
      });
    } finally {
      rmSync(work, { recursive: true, force: true });
    }
  };

  // 超长段落（约 19 万字符）必须截断到 125,000 字符以内，同时保留门禁提示、完整 SHA-256 与 CHANGELOG 链接
  const hugeSection = Array.from({ length: 1200 }, (_, i) => `- 第 ${i + 1} 条改动：${"详情说明".repeat(38)}`).join("\n");
  const hugeRes = runNotes(`# Changelog\n\n## [Unreleased]\n\n## [0.3.12] - 2026-11-19\n\n${hugeSection}\n\n## [0.3.11] - 2026-11-07\n- 旧版\n`);
  assert.equal(hugeRes.status, 0, hugeRes.stderr);
  assert.ok(hugeRes.stdout.length < 125_000, `超长发布说明必须小于 125000 字符，实际 ${hugeRes.stdout.length}`);
  assert.match(hugeRes.stdout, /^## MOSS v0\.3\.12/m);
  assert.match(hugeRes.stdout, /完整更新记录见 \[CHANGELOG\.md\]\(https:\/\/github\.com\/yjdaaa\/moss\/blob\/v0\.3\.12\/CHANGELOG\.md\)/);
  assert.match(hugeRes.stdout, /不含离线语音 sidecar/);
  assert.match(hugeRes.stdout, /deadbeef  MOSS_0\.3\.12_x64-lite-setup\.exe/);

  // 正常短段落不应被截断或追加省略提示
  const shortRes = runNotes(`# Changelog\n\n## [0.3.12] - 2026-11-19\n\n- 精简改动\n`, { sidecarReady: "true" });
  assert.equal(shortRes.status, 0, shortRes.stderr);
  assert.match(shortRes.stdout, /- 精简改动/);
  assert.doesNotMatch(shortRes.stdout, /完整更新记录见/);
});
