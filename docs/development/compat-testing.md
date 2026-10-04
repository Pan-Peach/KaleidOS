# libc / compatibility 测试

> 操作指南。当前验证记录见 `STATUS.md` §3.21；用户执行的依赖顺序见
> `docs/development/userspace.md`。这套工具构建普通应用，不注册 `.kcomp`。

## 用例来源与共同范围

测试正文直接编译 `third_party/libc-test/src/` 中的上游文件，不复制、不修改断言。
上游为 [musl 的 libc-test](https://repo.or.cz/libc-test.git)，submodule 从
[kraj/libc-test HTTPS mirror](https://github.com/kraj/libc-test) 获取，当前固定在
`b7ec467969a53756258778fa7d9b045f912d1c93`（2021-10-23 的 corpus 快照，非最新上游）。
该版本的 `COPYRIGHT` 为 MIT；`AUTHORS` 与版权全文随双平台包保留。上游 math 向量
另有 BSD/GPL 来源；当前不选择 `src/math/`，不笼统宣称整棵树都是 MIT。

`tests/compat/cases.txt` 是唯一用例清单，记录稳定名称、适用 target、契约类别、上游
source 和所需 support。平台选择由这份宿主测试清单驱动，不改变内核 `.config` 或 profile。

| 范围 | 用例 | 依赖与边界 |
|---|---|---|
| ISO C，Linux / Windows 共用 | memcpy、memset、strchr、strcspn、strstr、wcsstr、qsort、strtod-simple、iswspace-null、snprintf-g-zeros、sscanf-eof、scanf-bytes-consumed、scanf-literal-eof | 相同 source、相同断言；strtod-simple 使用 libm / snprintf；qsort 使用上游 `src/common/rand.c` |
| 纯计算，Linux / Windows 共用 | compiler/udiv | 64 位除法 / 取模向量；不计作 libc 函数覆盖 |
| POSIX，Linux 专属 | posix/fdopen、posix/stat | mkstemp/write/fdopen/fseeko/ftello/unlink；stat/fstat、uid/gid、time、`/dev/null` 与可写临时文件 |

POSIX 两项在 Windows 记录为 UNSUPPORTED，不用 `_fdopen` 或 Win32 shim 隐藏接口差异。
此阶段没有自写 `test_file_ops`、Win32 API 测试或共享文件语义 adapter。

覆盖不是按 API 名字凑数量：`functional/string.c` 混有 BSD strlcpy/strlcat，
strtol/wcstol 包含非法 base 的 errno / endptr 预期，malloc-0 强制要求非空且互异的
零尺寸分配。这些都未选入共同 ISO C 子集。当前没有完整 memmove / memcmp / strlen /
strcmp / strncmp / strtol / bsearch / malloc / calloc / realloc 专项覆盖；后续应继续筛选
现成用例并说明额外契约，避免把某个 libc 的实现选择当成全部平台的标准要求。

## 文件与职责

```text
third_party/libc-test/       pinned upstream source + COPYRIGHT / AUTHORS
tests/compat/
  cases.txt                 explicit shared / platform-specific corpus
  include/test.h            minimal upstream-compatible diagnostic declarations
  support.c                 t_status / t_printf, ISO C stderr backend
  runner.py                 host-only build / native run / package
  test_runner.py            host orchestration failure-path checks
build/compat/
  linux/                    bin/, logs/, build.json, results.json, results.tsv
  windows/                  bin/, logs/, build.json, results.json, results.tsv
  package/                  combined binaries, manifest.txt, licenses, build metadata
  compat-package.tar.gz     transferable package preserving executable mode
```

`include/test.h` 的位置宏与错误宏来自上游 MIT 文件，保留名称以直接编译测试正文。
它只声明当前 corpus 使用的 support，去掉上游 header 的无条件 unistd 依赖。
`support.c` 只处理诊断，不提供被测 libc 函数；失败先设置 `t_status=1` 再格式化。
成功不要求 printf 输出。guest 不需要 Python、JSON parser、shell 或测试框架。

## 构建与参考运行

先初始化 submodule。宿主需要 Python 3.8+、Git、GCC-compatible C 编译器及目标 libc 开发文件。

```sh
git submodule update --init --recursive
python3 tests/compat/runner.py list
make compat-linux
make test-compat-linux
make compat-windows
```

首条 guest 路线为 RV64，可从同一份上游 source 交叉构建到独立输出目录：

```sh
python3 tests/compat/runner.py build --target linux --cc riscv64-linux-gnu-gcc \
  --out build/compat-rv64
```

交叉构建不产生原生参考或 guest PASS。静态 glibc 仍带启动栈、TLS、内存与 Linux
syscall 依赖；先用 `make exec-fixtures` 验证执行机制，细节见
`docs/development/userspace.md` 的 exec 夹具说明。

Linux 默认 `cc`，默认 `-static`，没有静态库时明确 BUILD_FAIL，不自动切换链接方式。
Nix 的默认 cc 可能不带 libc.a/libm.a；有系统 GCC 时可用：

```sh
make test-compat-linux COMPAT_LINUX_CC=/usr/bin/gcc
```

显式选择动态参考构建：`make test-compat-linux COMPAT_LINKAGE=dynamic`。
工具记录编译器版本、`-dumpmachine`、完整命令、链接选项、upstream revision 和 binary SHA256。
`-O0 -fno-builtin` 避免基础函数测试被编译器展开或常量折叠成不调用被测函数的程序。

Windows 支持原生 MinGW-w64 GCC，也支持 Linux 上的 `x86_64-w64-mingw32-gcc` 交叉构建。
**不使用 MSYS / Cygwin 的 POSIX gcc 替代 Windows CRT**，runner 检查 compiler target。
推荐原生参考环境为 MSYS2 UCRT64，安装 `git`、`mingw-w64-ucrt-x86_64-gcc` 与
`mingw-w64-ucrt-x86_64-python` 后，在该 shell 中运行：

```sh
python3 tests/compat/runner.py test --target windows --cc gcc
```

Windows 构建启用 MinGW 的 C99 stdio 支持 `__USE_MINGW_ANSI_STDIO=1`；因此报告验证的是
选定的 MinGW-w64 + Windows CRT 组合，不能当作 MSVC/UCRT 独立实现的认证。
`-static` 在这里静态链接可静态链接的 MinGW/compiler runtime，**PE 仍会导入系统 DLL**。
MSVCRT toolchain 的实测包依赖 KERNEL32.dll 和 msvcrt.dll；UCRT toolchain 的导入由其构建
决定。它不是无 DLL 的独立 PE，未来 Windows personality 仍需 import resolution 与对应 API。

只有原生 Linux / Windows 才能产生相应 `reference=native` 的结果。Linux 交叉编译出 PE
只证明构建，不产生 Windows PASS；runner 在错误宿主明确拒绝参考执行。Wine 运行也不作为
原生 Windows 参考。可针对具体用例运行或构建：

```sh
python3 tests/compat/runner.py test --target linux --cc /usr/bin/gcc --filter libc/memcpy
python3 tests/compat/runner.py test --target linux --cc /usr/bin/gcc --filter libc
```

每次 build 都重新编译选中用例并清除旧结果；过滤构建会替换该 target 的 build manifest。
打包之前需重新完整构建两边。`--cflag=-option` / `--ldflag=-option` 可重复传入交叉工具链参数。

## 结果与 CI

每个程序独立运行，私有工作目录、stdin 关闭、默认 30 秒 timeout。0 = PASS，非零 = FAIL；
断言文件 / 行号在 `logs/*.run.log`，不把行号装入可能截断的 POSIX exit status。
运行器另区分 BUILD_FAIL、LOAD_FAIL（不存在、内容变化或不能启动）、CRASH、TIMEOUT、
UNSUPPORTED。UNSUPPORTED 不算 PASS，也不参与可执行测试的 PASS 分母。任何实际失败使
runner 返回非零，保留 compiler / application exit status。重跑不复用陈旧 PASS。

`results.tsv` 给出 test / target / status / exit_status / reason；host JSON 还保留来源信息。
`python3 tests/compat/test_runner.py` 用真实宿主子进程验证失败、crash、timeout、缺失 / 篡改
binary、未构建用例与错误参考平台的报告契约；这些 fixture 不是 libc 测试。

`.github/workflows/compat.yml` 有独立 Linux / Windows 原生参考 job；Windows 使用 UCRT64
native Python + GCC。两个参考 job 均成功后再合并 artifacts、校验 binary hash、生成组合包。
上传 binary / build metadata / results / logs。现有 CoreTest / ArchTest 的职责不变。

## 组合包与 KaleidOS 接缝

两边完整构建后运行 `make compat-package`。包保留普通 ELF / PE 与许可证，不加入 init.kpkg。
`manifest.txt` 是 whitespace 分隔文本，每一行：

```text
# test contract target machine executable (relative to package root)
libc/memcpy iso-c linux x86_64-linux-gnu linux/bin/libc_memcpy
libc/memcpy iso-c windows x86_64-w64-mingw32 windows/bin/libc_memcpy.exe
posix/fdopen posix windows x86_64-w64-mingw32 -
```

`-` 表示该 target 不在用例契约内。machine 来自编译器，不从 ELF / PE 格式推断 ISA。
包允许同时保存两种格式，**不代表当前 RV64 能执行 x86_64 程序**。未来 RV64 Linux
构建也可通过 `--cc` 指定真实 Linux 交叉工具链；正常 Windows x86_64 PE 留给同 ISA 的
KaleidOS 执行路径，当前不加 emulation / binary translation。

KaleidOS 已能在 RV64 S/MMU 运行普通静态 ELF，包含独立用户 AS、真实 U-mode /
trap / copy、启动栈、fork / execve / wait4 与退出 / fault 结果。ksh `exec` 从真实 FS
读镜像，交给 POSIX 的不可变镜像 profile；机制验收在 CoreTest，见
[userspace.md](userspace.md)。这不代替通用 VFS 或 SandboxedNative 组件装载。

本套上游程序尚未在 KaleidOS PASS。已实际从 FAT 尝试 RV64 静态 glibc 的
`compiler/udiv`：ELF 装载成功，启动因缺失 uname、mmap、signal 等 syscall 终止。
涉及文件的 fdopen/stat 还需 VFS 与 POSIX fd；Windows 还需相同 guest ISA 的 PE loader、
CRT startup 和 DLL imports。当前组合包为 x86_64，不能在 RV64 直接执行。

后续 guest runner 应读清单、选择该应用的 loader / personality、等待并收集结果，
每项区分装载失败、应用失败、故障、timeout 与 unsupported。现阶段未发布本 suite 的
自动 guest runner；参考结果与执行探针结果分别保留，不互相替代。

静态 libc 函数测试验证随程序链接的 libc，以及启动 / 退出链；涉及文件 / syscall 的测试才
进一步覆盖 personality / 服务语义。参考平台 PASS 不是 KaleidOS PASS，两个参考也不能用来
否定 ISO C 所允许的实现差异；新增共用用例仍须审查其断言的实际契约。
