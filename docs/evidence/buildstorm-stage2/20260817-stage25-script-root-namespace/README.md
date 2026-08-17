# Stage25：脚本全局根命名空间审计与验证

## 1. 身份与证据边界

- 候选 worktree：`stage23-la-tlb-generation-d676`。
- 分支：`codex/buildstorm-stage23-la-tlb-generation`。
- 基线 HEAD：`d676f7508bc08fb563ab451f184028158b942ed2`；本阶段仍是未提交 dirty candidate。
- 评测 LA 输出 26 SHA-256：
  `1b769404917943ac286af760234ebb6082d3a3812b4b50912b20e49008b7065d`。
- 官方 `final-2026`：
  `b5ec6ef8497e1818cbdec3b54bb722f036e57972`。
- Cosmos `mirror-upload-b2488999da07` 独立历史：根提交
  `d398fe4e705c6ba6f37e5d82c6708b163a391f9f`，审计 tip
  `52267f92afced3ffb7b484856e2f691c9151a58f`。
- 公共 LA 镜像：`/srv/buildstorm/images/sdcard-la-pub.img`，
  SHA-256 `d1410544e677e11efb1c240be6ffb201c89d6de58c9675e73314a696e4cefdc5`。
- QEMU：11.0.3；所有运行均使用 `-snapshot`，且没有并发 QEMU。
- 本阶段 harness diff SHA-256：
  `f6f13e9fd1fcbb393be941d31cc392106b3f3d4c3957942b543837ff8eaeb24e`。

本目录中的独立 SMP 回归属于 `capability-pass`。公共原始 suite 的完整成功属于
public-suite `official-pass`；评测机额外的 nested-QEMU/`vars.fd` 隐藏门仍为
`unverified`，必须由新提交的评测结果确认。

## 2. 评测首错与候选排序

评测机在 LA release artifact 已成功生成后出现：

```text
cp: cannot stat '/work/buildstorm.vars.fd/vars.fd': Not a directory
```

上一轮 `/work/buildstorm.esp` 的 `mkdir ENOSYS` 已消失。Stage25 对三个模型逐项审计：

1. **脚本进程选择了错误的逻辑 root，最强候选。**旧 harness 从脚本目录向上找
   最近的 shebang 解释器。如果全局 `/bin/sh` 和 suite-local `/glibc/bin/sh` 同时
   存在，绝对路径脚本 `/glibc/...` 会被隐式改成 root `/glibc`，其子进程中的
   `/work/...` 随后解析为 host `/glibc/work/...`。这违反已有全局 mount namespace
   对绝对路径的所有权。
2. **GNU `cp`、目录、symlink 或同名 file-to-directory 重建损坏，证据较弱。**独立
   双架构回归覆盖 `.fd` 目录、中间目录 symlink、长绝对 symlink、`cp -a`、`cp -R`
   和删除普通文件后以同名目录重建，均通过。
3. **隐藏 fixture 本身不是目录，仍可能。**隐藏镜像和脚本不可取得；若 Linux 上该
   对象本来就是普通文件，则内核必须继续返回 `ENOTDIR`，不能增加路径特判或容错。

## 3. Cosmos 线索及其适用范围

Cosmos mirror 没有针对 `vars.fd` 修改 `cp` 或 VFS。其 bootstrap 把评测盘挂到
`/mnt`，验证 `/mnt/bin/sh` 与正式脚本存在后执行 `pivot_root`，将评测盘变成唯一的
`/`，再卸载旧 bootstrap root。它自己的 nested-QEMU 脚本从
`/opt/qemu-la64/.../vars.fd` 复制到私有 `/tmp`，没有访问评测日志里的
`/work/buildstorm.vars.fd`。

因此，可迁移的架构结论只有“全局 root 必须显式拥有绝对路径解析”。该证据与独立
负向回归同向，但不能证明隐藏镜像一定同时提供两个解释器，也没有被当作源码来源。

## 4. 生产设计与不变量

`logical_path_for_script()` 的 root 选择顺序改为：

1. 解析 shebang 的绝对解释器；无 shebang 时使用 `/bin/sh`。
2. 若全局 root 中该解释器可执行，则 `/` 是权威 root，脚本保持其原绝对路径。
3. 只有全局解释器不可用时，才从脚本目录向上寻找真正自包含的兼容 userspace。
4. 两者都不存在时保持原有全局 fallback，由后续 ELF/BusyBox 检查返回真实错误。

该实现不检查架构、测试名、crate、命令、输出或 `vars.fd` 路径。VFS 仍严格拒绝
`regular/child`；syscall、MM、调度、TLB 和 guest 时间没有改变。独立回归同时构造：

- 全局和 suite 都有解释器、资源只在全局 root，旧逻辑必须失败；
- 全局没有目标解释器、资源只在 suite root，兼容回退必须继续成功。

## 5. 可证伪结果

### 5.1 负向基线

未改 root 优先级的 LA SMP8 稳定输出：

```text
[smp-regression] fail phase=script-root-namespace exit=1 root=/.__wll_script_root_namespace/suite
```

原始串口 SHA-256：
`b2f2252a3deb28bf7ee3396e9f5eaadc376d749d486e7a14b5a1c8cdf4652fb4`。

### 5.2 最终独立回归

RV 与 LA 的 8G/8 串口均包含：

```text
[smp-regression] pass phase=directory-metadata-abi
[smp-regression] pass phase=script-root-namespace root=/
[smp-regression] pass phase=script-compat-root
[smp-regression] pass cpus=8 ...
```

- LA serial SHA-256：
  `a6010338bf1c6900cc56d3976cf045994c338b9934024521dc6f043068191825`。
- RV serial SHA-256：
  `63d59c64978315d4c46c66709c0697666fb23b5e42a6d1415d9be35355f0f826`。
- LA/RV SMP kernel SHA-256：
  `52ba484ea7cb271fc29a459ecf641b24aa7d9af79f01f34fd1f7fcb7ca4fe569` /
  `ab96f9ea8bbb9a5ace777fe72a992971153983643ade7f63d2206b4d30a6c670`。

### 5.3 构建与公共完整流程

| Gate | 结果 | 等级 |
| --- | --- | --- |
| RV production / diagnostics release | 通过 / 通过 | `capability-pass` |
| LA production / diagnostics release | 通过 / 通过 | `capability-pass` |
| RV SMP8 / LA SMP8 | 完整通过 / 完整通过 | `capability-pass` |
| LA public 8G/8 | `ok=true elapsed_s=543.97 cores=8` | public-suite `official-pass` |
| LA public 36G/12 | `ok=true elapsed_s=547.47 cores=12`，最新 public judge 180/180 | `capability-pass` |
| 隐藏 `vars.fd` 门 | 无法在公共镜像构造 | `unverified` |

production kernel SHA-256 为
`9b7f0b179126645183af6dceb92f89474ff727ddefedda72b4a5a9e75b3284bf`。
8G/8 与 36G/12 public serial SHA-256 分别为
`75e3f2a29fedf9ff835fd0f33dbc27ceca5c9418086b29af6ba56474b9b347e2` 和
`8fccf151ff0bce28eb18b35e1d9aec242dce5240807639c397e11e7a350376d3`。

543.97/547.47 秒不是本修复的性能收益：root 选择只发生在启动正式脚本时，且两次
资源配置不同。Stage23 同机 8G/8 的 542.02 秒与本轮仅差 0.36%，属于运行噪声。

## 6. 复现

```bash
python3 scripts/run_smp_regression.py \
  --arch loongarch64 --image /srv/buildstorm/images/sdcard-la-pub.img
python3 scripts/run_smp_regression.py \
  --arch riscv64 --image /srv/buildstorm/images/sdcard-rv-pub.img
python3 scripts/run_buildstorm.py \
  --arch loongarch64 --image /srv/buildstorm/images/sdcard-la-pub.img \
  --stage complete --timeout 15000 --memory 36G --smp 12
```

AI 用于交叉核对评测日志、dirty diff、官方仓库、Cosmos mirror 和原始串口，设计
可证伪回归并执行单实例 QEMU。开发者可从本目录的 baseline/candidate/public/build
原始文件独立复核所有结论。未修改官方镜像、suite、judge、marker、timeout、guest
时间或 `/proc/uptime`。
