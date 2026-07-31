# BuildStorm Stage 2 Provenance

- Source branch: `wbq_final_buildstorm_compile`
- Source commit before uncommitted Stage 2 work:
  `2fb4254e24cdba11d2d408be4c2657449c6923e9`
- Official suite ref: `final-2026`
  `2f6ea561af35c36f4d1bf0dad5ab2eb312839ccc`
- Official script SHA-256:
  `446A3321F8D45F37637AC43B1B6BD66E09BEB304969F8ABAC69CCD701252AA0B`
- Official judge SHA-256:
  `586A30E260F39CF425C0960A2965D8684BC3A93B77842A68DFD80FA6FF3EE87B`
- RISC-V64 image SHA-256:
  `C03C6091EB1C400D4C1F400130D11810B00E18811D4E5C201E7F75802DFAFDE0`
- LoongArch64 image SHA-256:
  `15B2CEF4AF8FCB0BA9EC44FDBE41973EE180B699454B57C630FDCFB5E6DFA550`
- QEMU: `11.0.0 (v11.0.0-12122-ga4bb4b10c9)`.
- Official and SMP invocations use `-snapshot -smp 8 -m 8G`. Exact commands,
  kernel SHA-256 values, build timing, host timing, and image byte counts are
  recorded in the adjacent runner JSON files.

Evidence classification:

- `official-pass`: dual-architecture toolchain and minibuild only.
- `capability-pass`: dual-architecture independent SMP and ASID regression.
- `unverified`: both fixed 600-second RISC-V64 complete-stage diagnostic
  windows; neither reached the official complete marker.
