# 2026-08-17 评测输出 42/26 与 LA vars fixture 边界

## 身份

- 评测对应已推送分支 `wbq_final_buildstorm_compile` 的 `d676f7508bc08fb563ab451f184028158b942ed2`。
- GitHub 与 GitLab 的该分支都指向同一提交。
- RISC-V64 日志 SHA-256：`b7e07cd66075a069f947976e05f41234ed6b0e940a11bd06eec886373c24bfd4`。
- LoongArch64 日志 SHA-256：`1b769404917943ac286af760234ebb6082d3a3812b4b50912b20e49008b7065d`。
- 当前 Stage23 生产候选仍是未提交 dirty patch，没有混入上述评测身份。

## 最早失败边界

RV 包含完整成功标记：

```text
BUILDSTORM_RESULT mode=multi status=OK rc=0 cores=8 elapsed_s=2831.14 ... run=OK
```

LA 的 release artifact 也完成，axbuild 用时 `3008.46s`。唯一首错发生在不计时的
nested-QEMU 准备阶段：

```text
cp: cannot stat '/work/buildstorm.vars.fd/vars.fd': Not a directory
```

上一轮的 `/work/buildstorm.esp` mkdir/fchdir 错误已经消失，说明 legacy mkdir 与
directory-FD 修复推进了失败边界；`vars.fd` 不是该错误的派生结果。

## 可证伪审计

`ENOTDIR` 表示被解析路径的某个非末尾分量不是目录。`/work/tgoskits` 已完成整轮
clean build，因此 `/` 与 `/work` 不是失败分量，剩余分量是
`/work/buildstorm.vars.fd` 本身或它跟随后的 symlink 目标。

独立 LA SMP8 用户态探针在当前内核上通过以下序列：

- 带 `.fd` 后缀的 ext4 目录及其中普通文件；
- GNU `stat`、`cp`、`cmp`；
- 短目录 symlink 作为中间路径分量；
- 超过 60 字节、使用块存储的长目录 symlink；
- 普通文件经 `stat` 缓存后删除，再以同名目录重建并访问子文件。

最终仍包含 `pass phase=directory-metadata-abi` 和 `pass cpus=8`，串口 SHA-256 为
`a5e236277058c94871b6413d88e3204ec7976086feeb1c0e0085efc13917fca0`。这属于
`capability-pass`，不能替代隐藏评测。

## 结论与合规边界

现有证据不支持把 Linux VFS 改成允许普通文件拥有子路径。按特定名字把
`regular/child` 重定向到 `regular` 会违反 POSIX/Linux 语义，也会构成按评测路径
选择生产行为。正确的修复点必须是 hidden fixture 的构造：要么
`/work/buildstorm.vars.fd` 应是包含 `vars.fd` 的目录，要么脚本应直接使用这个普通
文件。由于 hidden 镜像和脚本均不可取得，本地状态保持 `unverified`。

成功队伍公开脚本使用 `/opt/qemu-la64/.../vars.fd`，复制到 `/tmp` 私有目录；它绕开
了上述 fixture，不能证明评测机的 `/work/buildstorm.vars.fd/vars.fd` 在 Linux 上
有效，也没有被用作代码来源。

本轮没有修改官方镜像、suite、guest script、judge、marker、guest 时间或 timeout，
也没有加入测试名、crate、路径、命令或输出特判。

## Stage25 后续修订

上述“不能放宽 VFS 类型语义”的结论继续成立，但“只能修 hidden fixture”的候选
范围已被后续证据缩小。对 harness 的独立负向回归证明：旧脚本启动逻辑在全局 root
与 suite root 同时提供 shebang 解释器时，会错误优先 suite root，使绝对 `/work`
解析到 suite 前缀下。Cosmos mirror 的 `pivot_root` 架构也确认成功实现把评测盘作为
唯一全局 root。当前候选因此在 harness root authority 层修复，而不是修改 VFS；
完整证据与仍为 `unverified` 的隐藏门见
`20260817-stage25-script-root-namespace/README.md`。
