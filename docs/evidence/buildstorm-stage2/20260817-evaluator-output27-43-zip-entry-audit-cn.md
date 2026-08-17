# 2026-08-17 评测输出 27/43 与 zip 根入口审计

## 输入身份

- 评测提交：`2e6980773d34b5eea961549e6317dabe207c6431`。
- LoongArch64 原始附件：`C:\Users\22478\Downloads\LoongArch输出 (27).txt`。
- RISC-V64 原始附件：`C:\Users\22478\Downloads\Riscv输出 (43).txt`。
- LoongArch64 附件 SHA-256：
  `1c25d7b053a328dd5cc688dc4fbfadad26610a969b4dd26cc54859f594c10252`。
- RISC-V64 附件 SHA-256：
  `ca163317de386dc84e60de61d384d352caadAA43BAE000D4B633EAC32295277A`。

## 评测首错

RISC-V64 串口包含：

```text
BUILDSTORM_RESULT mode=multi status=OK rc=0 cores=8 elapsed_s=1641.37 ... run=OK
```

本轮官方汇总为 RV BuildStorm `178.1`、LA BuildStorm `20.0`，CAgent 双架构
均 `199.1`。LoongArch64 在 `1500.31s` 完成 arceos artifact，之后隐藏
nested-QEMU 准备阶段首次出现：

```text
cp: cannot stat '/work/buildstorm.vars.fd/vars.fd': Not a directory
```

公开 `final-2026` 的 `scripts/buildstorm_testcode.sh` 不包含该路径，因此这是
评测机隐藏 fixture/后处理边界，证据等级为 `unverified`。不能把普通文件前缀按
路径改成可遍历目录；须由隐藏通道确认 `/work/buildstorm.vars.fd` 的实际对象类型。

## zip 根目录错误

`2e698077` 的 Git tree 顶层确实有 `Makefile`、`Cargo.toml`、`os/Cargo.toml` 和
`rust-toolchain.toml`。zip 通道却在 QEMU/内核启动前报告：

```text
make: *** No rule to make target 'all'. Stop.
```

因此失败发生在提交包解压目录，不是内核编译错误。最可能的上传物是 GitHub
下载格式的 `<repo>-<commit>/...` 外层目录，评测器没有自动进入该目录。

## 修复与复现

`create_kernel_zip.py` 使用当前提交的 `git archive --format=zip HEAD`，确保条目
直接从 `Makefile`、`Cargo.toml` 等根路径开始，并校验根入口和 ZIP CRC。顶层
`Makefile` 的 `all` 首先执行通用 `submission-preflight`，输出
`WLL_SUBMISSION_PREFLIGHT status=OK`，用于区分 zip/workdir 错误和内核/QEMU 错误。

```bash
python3 create_kernel_zip.py --output ../wll_os-submission.zip
unzip -l ../wll_os-submission.zip | head
```

该改动不修改官方 image、suite、judge、guest marker、时间源或 VFS 的
`ENOTDIR` 语义；zip 入口属于 `capability-pass`，LA hidden `vars.fd` 仍为
`unverified`。
