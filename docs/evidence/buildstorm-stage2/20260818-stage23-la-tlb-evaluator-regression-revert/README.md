# Stage23 LoongArch TLB generation evaluator regression and conservative revert

日期：2026-08-18

## 结论

当前官方评测在 LoongArch nested BuildStorm 的 glibc clean build 阶段失败：

```text
corrupted double-linked list
Error: failed with status: signal: 6 (SIGABRT)
BUILDSTORM_RESULT mode=multi status=FAIL rc=1 cores=12 elapsed_s=72.96 run=FAIL
```

失败发生在 `ax-hal` 编译期间。此前的 LoongArch UAL 启动阻塞已经消失，内核
已完成启动、Hello World 和 nested QEMU 准备；Cargo 的 `failed to save last-use
data` 与 `out of range integral type conversion attempted` 只是非致命缓存警告。

## 归因

成功通过的 v16 树 `1e0a3a98` 在用户根激活与切回 kernel root 时保持保守的
LoongArch `TLB::flush_all()`。合并 `2e698077` 后引入了 `PageTableWrapper`
translation generation、CPU-local `{root, ASID, generation}` 验证，以及跨
kernel-root 区间保留非零用户 ASID 的逻辑。当前失败树为 `61527620`，其余 UAL、
脚本根命名空间和提交工具链均来自该合并树。

官方失败形状与 stale LoongArch translation 写入已复用的 glibc heap metadata
一致：架构限定、SMP12/36G 压力下出现 allocator integrity abort，且精确的
保守 flush v16 ZIP 曾得到 BuildStorm RV/LA 180/180。Stage23 的 public 8G/8
与四 cfg/SMP 门禁只能算 capability/历史 official-pass，不能覆盖此 evaluator
条件下的稳定性。

## 修复范围

只撤销 Stage23 的高风险 TLB reuse：

- 恢复 LoongArch 用户根激活时的本地全量失效；
- 恢复从用户 root 切回 kernel root 时的本地全量失效；
- 删除 generation wrapper 和 CPU-local verified-root 状态；
- 恢复 `mark_current_address_space(root)` 的原始接口及 SMP regression 调用。

保留 UAL auxv、timer/exec 修复、Stage25 全局脚本 root、目录 ABI、VFS/MM 优化和
根相对 ZIP 入口。没有修改官方 image、suite、guest script、judge、marker 或
guest 时间，也没有按 crate、测试名、路径、命令或输出分支生产行为。

## 当前验证

- `git diff --check`: 通过
- RISC-V64 production release `cargo check`: `EXIT=0`
- RISC-V64 diagnostics release `cargo check`: `EXIT=0`
- LoongArch64 production release `cargo check`: `EXIT=0`
- LoongArch64 diagnostics release `cargo check`: `EXIT=0`
- 官方复测：未完成，必须取得精确 `BUILDSTORM_COMPILE mode=multi ok=true` 后才能
  重新宣称 `official-pass`。

原始官方日志：用户附件
`C:\Users\22478\.codex\attachments\236dd4fa-6595-45b5-ac13-3364d80d5881\pasted-text.txt`。

AI 用于日志/源码/diff 交叉核对、回归归因和修复编排；开发者可从 commit、diff、
四 cfg 输出和新的官方串口日志独立复核。该记录中的失败分级为 `unverified`，
直到新的官方结果产生。
