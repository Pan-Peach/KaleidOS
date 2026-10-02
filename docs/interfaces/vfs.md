# VFS 第一阶段服务契约

> 阶段契约草案，尚未实现或冻结。分层以 `docs/interfaces/filesystem.md` 为准；
> 布局、数值、方法号与 exact fingerprint 以 `abi/vfs.toml` 为唯一来源。
> SDK 目前只有声明与 ENOTSUP 占位，不能 bind / 发布可用服务。

## 范围与身份

一个 `vfs` endpoint 先组合 Namespace 和 File service；实例端口名由组合者选择。
它消费 FS providers，供 POSIX / NT / WASI 等 personality 使用。fd、HANDLE、cwd
和平台错误表示由 personality 保存。挂载配置由 VFS create 提供，尚无配置 wire。

本阶段只声明已有节点的解析、查询、枚举和只读数据流打开。write / delete 访问位
虽已预留，但当前必须拒绝为 `-ENOTSUP`；不能忽略位后按 read 成功。创建、截断、
原子 append、rename、unlink、权限 descriptor 查询和目录 open 不在此阶段 ABI。
目录可枚举，也可能支持命名流；不得伪造其默认数据流。

| 身份 | 含义 / 有效期 |
|---|---|
| `VfsPath` | mount + entry + FS incarnation + node；路径位置与底层节点分开 |
| `VfsStream` | FS incarnation + node + stream；默认流也有不透明 token |
| file token | 一次独立 open；retain 共享它，重新 open 创建另一对象 |
| directory cursor | 本次目录枚举的位置；0 为起点，不是数组下标 |

这些数值不是 authority。所有引用受发布 endpoint 的 VFS 实例有效期约束，
provider 重启必须创建新 incarnation，不能重绑旧 token 到新 provider。
验证须同时检查身份、存活与请求所需权限，不能只看数值是否存在。

root / resolve / 非 END 的 read_dir 成功均交付一个路径用户引用。
复制 `VfsPath` 内存不增加引用；独立持有 root/cwd 等需 retain_path，并配对 release_path。
操作期间 path 参数只借用；open 成功后独立保活所需对象，caller 可以 release_path。
路径保活不保证原目录项仍在 namespace 中，也不阻止 provider 失败导致逻辑失效。
目录修改、detach 和删除后的相对遍历细节仍须在写支持前定稿。

retain 成功只增加 file 用户引用，不重做 provider open / share 检查。
close 消费一个用户引用。最后用户句柄的 cleanup、I/O / mapping 排空、最终 provider
close 是不同阶段；不以计数归零承诺物理回收。有效引用被消费后，即使后续清理失败
也不得自动重试 close / release；无效 token 不消费其他引用。

## 名字与解析

路径和名字均有显式编码，没有 NUL 终止符。Bytes 不要求 UTF-8；UTF-16 的 payload
是 LE code units，长度按字节计且必须为偶数。不得把 surrogate 或不可表示字符
静默替换；provider 无法无损表示时返回 UNSUPPORTED_NAME。NUL 不可进入名字。

本阶段 namespace 路径语法使用 `/`（UTF-16 中为 U+002F）分隔；
反斜杠和冒号不在这里自动解释为 Win32 路径或 ADS。personality 先转换自己的路径语法，
命名流用 open 的独立 stream selector / input 表达。名字大小写匹配由所在目录的
provider 执行，VFS 不依据 case 提示自行折叠。

- 非空绝对路径从请求 root 开始；相对路径从 start 开始。
- 重复分隔符、`.`、`..` 在逐段遍历时处理；Root 模式的 `..` 在 root 处停留。
- BENEATH_START 拒绝绝对输入及绝对 symlink 目标，并拒绝实际逃出 start。
- 尾部分隔符要求目标为目录。中间 symlink 总须跟随；FOLLOW_FINAL 控制最后一段。
- max_symlinks 为可跟随的最大次数，0 禁止跟随；超过次数返回 `-ELOOP`。
- 未设置 CROSS_MOUNTS 时拒绝跨 mount；发生越界返回 OUTSIDE_ROOT。
- 无法取得请求要求的原生语义时返回 `-ENOTSUP`，不清洗字符串后假装符合边界。

PATH_MAX / NAME_MAX 是本阶段服务输入上限，UTF-16 同样按字节计。
后端可以有更小的原生限制，超限明确拒绝。未知 encoding、flag 或非零 reserved
为 `-EINVAL`。default stream 必须 input 为空、encoding 为 0；
named stream input 必须为非空单段名字。默认流不存在返回 NO_DATA_STREAM，
不支持命名流返回 `-ENOTSUP`。

## Direct 与 Gate

Direct 使用生成的 `VfsApi` function table + opaque ctx，标量是 C ABI 参数；
payload 的编码与 Gate 一致。指针只借用本次调用，provider 不保留、不改写 input，
也不把调用者的用户态 VA 当成可解引用的 table 参数。

Gate 使用 `kcore_endpoint_call` 的 args / input / output 平面。
所有标量逐字段 LE 编码，不能将 Rust struct / C padding 直接当成 wire。
每个 output 都先有 8 字节 VfsReplyStatus（u32 domain + u32 reserved），
再放业务 payload；Direct 则使用独立 out_status 指针。下表长度包含此回复头。

| 方法 | args | input | output |
|---|---|---|---|
| root | 空 | 空 | 40：status + VfsPath |
| resolve | 80：VfsLookup | 有界路径 payload | 40：status + VfsPath |
| node_info | 32：VfsPath | 空 | 32：status + VfsNodeInfo |
| read_dir | 40：VfsPath + u64 cursor | 空 | ≥64：status + VfsDirReply + 名字容量 |
| open | 48：VfsOpenRequest | default 空 / named 名字 | 16：status + u64 file |
| retain / close | 8：u64 file | 空 | 8：status |
| read | 8：u64 file | 空 | ≥16：status + u64 实际长度 + 数据容量 |
| read_at | 16：u64 file + u64 offset | 空 | ≥16：status + u64 实际长度 + 数据容量 |
| set_position | 16：u64 file + u64 offset | 空 | 8：status |
| stream_info | 8：u64 file | 空 | 64：status + VfsStreamInfo |
| retain_path / release_path | 32：VfsPath | 空 | 8：status |

read / read_at 的实际读取长度不得超过数据容量；零长读仍验证引用和访问状态。
顺序 read 的游标更新与并发操作须协调，read_at 不改变游标，也不能用 seek + read 模拟。

read_dir 一次返回一个完整名字与一个路径引用，不截断名字。END 时只有 END flag，
其他 header 字段为 0。非 END 时 name_len 为实际字节数，name_encoding 显式给出。
名字容量不足返回 `-ENOBUFS`，仅 header.name_len 有效，表示所需字节数；cursor 不前进，
也不交付路径引用。SDK 将这个失败保留为 `BufferTooSmall { required }`。
cursor 在目录修改后没有 snapshot 承诺，失效必须明确报错，不能静默当成 EOF。

node_info / stream_info 是 provider 查询快照。valid 位区分“属性未知”和“值为 0”；
无效字段填 0。link_count 不由 VFS 的句柄计数推导；allocated_size 和 valid_data_length
不由逻辑长度推导。权限原生模型尚未决定，不能从这些查询伪造授权。

Core 在 bind 时选择调用机制；SDK 不降级、不替换失效 endpoint。
Direct/Gate 适配器需逐方法同义，尚未实现。Gate service stack 禁止 park / 调度切换，
阻塞完成协议必须先另定契约。

## 错误通道

Core 的传输结果与 provider 的方法状态是两层结果：

- 传输失败：Core 的 `-errno`，SDK 保存为 Transport。
- 方法成功：0，回复头 domain 必须为 0，此时常规 payload 有效。
- 方法失败：负值保留原生 `-errno`，回复头 domain 可补额外分类；SDK 保存两者，
  不经过未知 errno 归一化。正的方法返回值是 InvalidReply。
- domain 为 0 或 schema 声明的值，reserved 必须为 0；其他值是 InvalidReply。

domain-status 当前保留 SHARING_VIOLATION、DELETE_PENDING、UNSUPPORTED_NAME、
STALE_REFERENCE、OUTSIDE_ROOT、NO_DATA_STREAM；权限拒绝用 `-EACCES` / domain 0，
共享 / 删除拒绝即使 errno 相同也可区分。正常业务失败的 status 头仍有效；
read_dir 的 ENOBUFS 另外保留所需名字长度。malformed frame 在调用业务后端前拒绝，
不写回复；SDK 需先校验 shape，初始化 status，并区分传输失败和正常方法回复。
内部 provider 调用的 transport 失败不能冒充本次外层 Core transport 结果。

这是 VFS 第一阶段的局部选择；既有 filesystem / block 契约继续使用 0 / -errno。
POSIX errno 与 NTSTATUS 的最终映射、原生权限模型、跨 personality 删除冲突以及
未知原生 metadata 的透传仍待定稿。该 ABI 不表示已有 Linux / Windows 兼容性。
