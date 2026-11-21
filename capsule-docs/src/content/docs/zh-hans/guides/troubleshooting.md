---
title: 故障排除
description: 收集报告所需的版本和日志，并解决 Capsule 服务器、导入或照片库的常见问题。
status: draft
---

出现问题时请从这里开始。如果下面的内容都无法解决问题，[](/support/) 说明了该去哪里提问，而第一节中的诊断信息正是缺陷报告所需要的。

<a id="collect-diagnostics"></a>

## 收集诊断信息

<a id="version"></a>

### 版本

目前尚未发布任何版本，因此版本号无法区分不同的构建。所有内容都从源代码构建，所以请报告你构建时所用的提交：在你的检出目录中运行 `git rev-parse --short HEAD`。CLI 没有版本参数，而 `capsule-server --version` 只打印 crate 版本，所有构建的这个版本都相同。

<a id="logs"></a>

### 日志

CLI 和服务器将日志写入 **stderr**。stdout 只承载命令打印的内容，在 `--json` 下就是单个 JSON 文档，因此你可以分别捕获两者：

```sh
RUST_LOG=debug capsule push --library ~/Photos.capsule 2> capsule.log
```

- `RUST_LOG` 设置日志的详细程度。发布构建默认以 `info` 级别记录，调试构建默认以 `debug` 级别记录。复现问题时请使用 `RUST_LOG=debug`；只有在被要求时才使用 `RUST_LOG=trace`，因为它的输出非常庞大。
- 服务器的日志格式由 `LOG_FORMAT` 设置：调试构建默认为 `pretty`，发布构建默认为 `json`（[](/development/local-development/#logs)）。
- 全屏 TUI（在终端中直接运行不带参数的 `capsule`）无法把日志输出到它正在绘制的屏幕上，因此会将 `tui.log` 写入 CLI 的缓存目录，即 `capsule status` 所打印的 Cache Directory。该文件在每次会话时都会被覆盖，因此请在再次启动 TUI 之前复制它。
- `--json` 输出是命令执行情况最精确的记录，因为它会报告每个资源的结果。当命令报告失败时，请附上它。

**分享之前先脱敏**。请删去密码短语、恢复凭据、令牌、密钥、服务器地址和个人文件路径。切勿在任何地方粘贴照片库密码短语或恢复凭据，私下的报告也不例外。

<a id="the-server"></a>

## 服务器

<a id="it-refuses-to-start"></a>

### 服务器拒绝启动

- **退出状态为 2，并列出变量名**。持久化服务器需要 `DATABASE_URL`、`BLOB_ROOT`、`JWT_ED25519_DER` 和 `ATTESTATION_KEY_SEED`，并会一次性列出所有缺失的变量。参见 [](/guides/self-hosting/#configure)。
- **数据库架构比二进制文件旧**。`serve` 从不执行迁移。每次升级时，请在 `serve` 之前运行 `capsule-server-migration up`（[](/guides/self-hosting/#migrate-then-start)）。
- **可信代理设置被拒绝**。`TRUSTED_PROXIES` 会拒绝 `/0` 网络，`TRUSTED_PROXY_HEADER` 只接受 `x-forwarded-for` 或 `forwarded`。

正在运行的服务器会向 stdout 打印 `listening on <url>`。

<a id="clients-cant-sign-in-or-connect"></a>

### 客户端无法登录或连接

- `capsule server-info` 会显示服务器的登录方式：是否配置了身份提供方，以及是否拒绝密码登录。向已禁用密码的服务器进行密码登录，按设计就会失败。
- 在反向代理之后，所有客户端都共享代理的速率限制，直到在 `TRUSTED_PROXIES` 或 `TRUSTED_PROXY_HOPS` 中指明该代理为止。当多人同时被限速时，请首先检查这一点（[](/guides/self-hosting/#configure)）。

<a id="the-index-and-the-blob-store-disagree"></a>

### 索引与 blob 存储不一致

`capsule-server scrub` 是只读的完整性检查，发现任何问题都会以非零状态退出。在恢复备份或丢失数据库之后，`capsule-server rebuild` 会将索引与所有者账本进行比较。在你停止 `serve` 并运行 `capsule-server rebuild --apply` 之前，它不会做任何更改（[](/guides/self-hosting/#maintenance)）。

<a id="imports-and-uploads"></a>

## 导入与上传

- **`push` 中途停止**。再运行一次即可。上传会从中断处继续，因此重新运行始终是安全的。只要有任何资源失败，`push` 就会以非零状态退出，`--json` 会列出每个资源的结果（[](/guides/user-guide/#import-and-back-up)）。
- **在托管得到验证之前，请保留原始文件**。在 `push` 报告某个资源已验证之前，不要删除其源文件；或者使用 `import --move --push`，它仅在每个资源通过验证后才删除对应的源文件。
- **Google 相册导出看起来不完整**。请检查 Takeout 导出的每个部分是否都已下载并解压，并阅读导入工具无法保证的内容（[](/guides/google-photos-migration/#what-the-importer-cannot-guarantee)）。
- **拍摄时间不正确**。`capsule repair capture-time` 会报告所有签名拍摄时间与原始文件 EXIF 不一致的资源，加上 `--apply` 则会予以更正。每次更正都是一条永久的签名记录，因此请先在不加 `--apply` 的情况下运行。

<a id="check-a-librarys-integrity"></a>

## 检查照片库的完整性

如果照片看起来丢失或损坏，在查明原因期间请**停下来，不要删除任何东西**：删除可能会移除你恢复数据所需的上下文。

1. `capsule verify --library <path>` 会向服务器询问它是否已持久保存每个原始文件。`--deep` 让服务器重新计算已存储字节的哈希值，而不仅仅检查它们是否存在；`--signed` 会将服务器签名的判定结果保存在照片库中。
2. 移入废纸篓的资源在其保留期内仍可恢复：`capsule trash restore --library <path> --asset <id>` 可以恢复一个资源（[](/guides/user-guide/#delete-safely)）。
3. 设备或密钥丢失有专门的处理流程：[](/design/backup-recovery/#recovery-paths)。

如果数据丢失或损坏，请将其作为缺陷报告，并附上每一步的输出。无论原因如何，数据丢失或损坏都属于缺陷。

<a id="still-stuck"></a>

## 仍然无法解决

[](/support/) 列出了可以提问的地方，以及如何提交能够被处理的缺陷报告。
