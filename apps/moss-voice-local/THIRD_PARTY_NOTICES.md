# Third-party notices for moss-voice-local

- moss-voice-local: GPL-3.0-or-later; corresponding program source and COPYING are packaged next to the installed application.
- sherpa-onnx Rust crate and native libraries: Apache-2.0 upstream, including third-party components; native static builds may contain espeak-ng governed by GPL-3.0. Review the exact linked release's notices and provide the corresponding native-source offer before distributing.
- moss-voice-local-proto: MIT; source is included with the helper source bundle.
- Remaining Rust dependencies: inspect the pinned Cargo.lock and upstream license texts before external distribution. This notice is a preliminary inventory, not evidence of full license compliance.

## Native static library inventory (sherpa-onnx 1.13.8, Windows x64 MT Release)

| Linked targets | Upstream source candidate | License texts collected |
| --- | --- | --- |
| sherpa-onnx-c-api, sherpa-onnx-core | k2-fsa/sherpa-onnx v1.13.8 | LICENSE (Apache-2.0) |
| kaldi-decoder-core | k2-fsa/kaldi-decoder v0.3.0 | LICENSE (Apache-2.0) |
| sherpa-onnx-kaldifst-core | k2-fsa/kaldifst v1.8.0 | LICENSE (Apache-2.0) |
| sherpa-onnx-fst, sherpa-onnx-fstfar | csukuangfj/openfst v1.8.5-2026-04-10 | COPYING (Apache-2.0) |
| kaldi-native-fbank-core | csukuangfj/kaldi-native-fbank v1.22.3 | LICENSE (Apache-2.0) |
| kissfft-float | mborgerding/kissfft febd4caeed32e33ad8b2e0bb5ea77542c40f18ec | COPYING (BSD-3-Clause) |
| piper_phonemize | csukuangfj/piper-phonemize f3ff95afc03640bc1399e113e83361192a2fafb4 | LICENSE.md (MIT), licenses/uni-algo/LICENSE.md |
| espeak-ng, ucd | csukuangfj/espeak-ng ed530aa113046142eb5115cf2fc9157854d0ffe1 | COPYING and src/ucd-tools/COPYING (GPL-3.0); both COPYING.UCD files |
| onnxruntime | microsoft/onnxruntime v1.28.2 source candidate; prebuilt csukuangfj/onnxruntime-libs | LICENSE (MIT), ThirdPartyNotices.txt; recursive submodule source closure pending |
| ssentencepiece_core | pkufool/simple-sentencepiece v0.7 | LICENSE (Apache-2.0) |

`native-source-evidence.json` records the actual collected archives and license
file SHA-256 values. It deliberately states review_status=pending. Public source
selection and byte verification do not establish correspondence to every linked
binary or completion of transitive license obligations. The source collector
does not authorize publishing the installer. Registry collection contains 146
Cargo.lock-verified archives and 279 actual license/notice texts, including the
complete MIT grant/copyright in r-efi 5.3.0 and 6.0.0 AUTHORS. Collection alone
does not establish transitive correspondence. ORT recursive submodules, its
pinned cmake/deps.txt dependencies and independent review remain release blockers.

## Additional ORT source candidates

The optional ORT candidate collection now includes 35 deps.txt source archives
verified with their pinned upstream SHA1 plus measured SHA256, and three
gitlink source candidates pinned to the release tree. Six protoc host binaries
are recorded as tooling, not corresponding source. See
`native-license-evidence/ort-dependency-evidence.json`. These 38 archives are
not evidence that all candidates were linked. Actual prebuilt build options,
recursive closure and independent source/binary review are still pending.

## Optional speaker model

CampPlus `iic/speech_campplus_sv_zh-cn_16k-common` v1.0.0 has an
Apache License 2.0 declaration in the official ModelScope model card. The public
sherpa speaker model asset is downloaded separately, not included in this helper
or installer. Its fixed byte count/hash is in the protocol and model catalog;
speaker recognition and real-person accuracy remain unverified.
