---
title: 自托管
description: 在 PostgreSQL 和文件系统 blob 存储上运行 capsule-server
status: draft
---

目前尚未发布任何版本，因此你需要从源代码构建服务器。服务器只存储密文，从不解码媒体；所有处理和加密都由客户端完成。

<a id="what-you-need"></a>

## 所需条件

- **PostgreSQL 18**：持久化索引（账户、资源、配额）以及服务器中会过期的状态（登录会话、上传会话、速率计数器）。它是服务器使用的唯一数据库，并且必须在其持久性设置完好的情况下运行（[](#postgresql)）。
- **一个 blob 目录**（`BLOB_ROOT`），位于**单个 POSIX 文件系统**上，该文件系统须遵守 `fsync` 并共享文件锁。它保存所有 blob，并在 `ledger/` 下保存每个所有者的签名账本，索引可以据此重建（[](#blob-目录)）。
- **一个终止 TLS 的反向代理**，位于服务器前方，并原样透传压缩（[](#反向代理)）。
- **同步的时钟**：主机通过 NTP 保持时钟同步（[](#时钟)）。

### PostgreSQL

服务器在一行数据提交后即应答写入，因此如果 PostgreSQL 确认了一次提交随后又将其丢失，就可能让已撤销的会话重新生效，或使第二因素验证码被接受两次。请保持：

- `fsync = on` 和 `full_page_writes = on`，即默认值；
- `synchronous_commit` 为 `on`、`local`、`remote_write` 或 `remote_apply`，绝不能为 `off`；
- 数据目录位于会遵守刷新（flush）操作的存储上。没有掉电保护的磁盘写缓存，或宿主机会忽略刷新操作的虚拟磁盘，会在断电时丢失已确认的提交（[](/design/filesystem/server/)）。

PostgreSQL 可以位于与 `BLOB_ROOT` 不同的文件系统、磁盘或主机上。

<a id="blob-directory"></a>

### Blob 目录

`BLOB_ROOT` 是与文件系统之间的一份契约（[](/design/filesystem/server/#blob-root-filesystem-contract)），其布局见 [](/design/filesystem/server/#blob-store-layout)：

- **单个文件系统**。最终化是一次原子重命名，因此整个目录树（包括 `incoming/`）不能跨越多个卷、数据集或 Kubernetes PersistentVolume。
- **`fsync` 必须生效**：在服务器确认写入之前，对文件和目录的 `fsync` 都必须真正生效。
- **文件锁必须共享**：所有写入该目录树的进程都必须共享文件锁。`serve` 和 `purge --apply` 在追加之前会锁定每个所有者的 `ledger/<owner>/.lock`（[](/design/filesystem/server/#concurrent-appenders)）。
- **每个 blob 目录只运行一个 `serve`**。上传分块和最终化都在提供服务的进程内部认领（[](/design/transfer/upload/)），因此不支持在同一目录树上运行两个 `serve` 进程，无论它们位于同一台主机还是两台主机上。`gc`、`purge` 和 `scrub` 可以与它并行运行；`rebuild --apply` 则需要先停止它（[](#维护)）。

支持：

- 本地存储或块存储上的 ext4、XFS 和 btrfs：磁盘、RAID、LVM、iSCSI 或云块存储卷；
- `sync=standard`（默认）或 `sync=always` 的 ZFS；
- NFSv4，或带锁管理器的 NFSv3，均须以 `sync` 方式导出；
- 由上述任一存储支持的 Kubernetes PersistentVolume 或容器卷。

不支持：

- 禁用了写屏障的挂载，或宿主机会忽略刷新操作的虚拟磁盘；
- `sync=disabled` 的 ZFS，它会在未写入的情况下确认 `fsync`；
- 以 `async` 方式导出的 NFS（它会确认 NFS 服务器尚未存储的写入），或以 `nolock`、`local_lock=flock` 或 `local_lock=all` 挂载的 NFS（这些选项会把锁限制在单台主机上）；
- SMB/CIFS 共享；
- FUSE 文件系统（sshfs、rclone mount、s3fs、gcsfuse）以及任何对象存储挂载；
- 将一个目录树分散到多块磁盘上的池化或联合文件系统（mergerfs、位于 `/mnt/user` 下的 Unraid 用户共享）：成员磁盘之间的重命名是一次复制，而不是原子重命名；
- 容器自身的可写层，以及绑定挂载到容器虚拟机中的 macOS 或 Windows 主机目录。请改用虚拟机内的卷。

在 Linux 上，NFS 上的 `flock` 通过字节范围锁来模拟，锁管理器会在主机之间共享这些锁。如果禁用了锁，不同主机上的 `serve` 和 `purge --apply` 可能会交错地向同一个账本追加内容。

- **在信任硬件之前，先进行断电演练**。`serve` 在启动时会检查文件系统类型、原子重命名和锁共享，但任何软件检查都无法发现那些确认了实际并未执行的刷新操作的磁盘、控制器或虚拟机监控程序。请在实际部署的硬件上，一边上传一边切断电源（而不是正常关机），然后开机并运行 `capsule-server scrub --deep`：服务器确认过的每一次上传都必须完好无损（[](/design/filesystem/server/#blob-root-filesystem-contract)）。
- **预留可用空间**。blob 根目录会保留一部分可用空间，默认取卷容量的 2% 与 4 GiB 中的较大者。低于该值时，新的上传会以 `507` 被拒绝，而已在服务器上进行的上传仍可完成，删除操作也仍然有效（[](/design/quota/#enforcement-points)）。该预留功能已在计划中，尚未实现；在此之前，磁盘写满时所有写入都会被拒绝。
- **有意识地清理隔离区**。`quarantine/` 会保存重新计算哈希失败或无法读取的 blob，直到你将其删除为止（此后已修复的除外）。计划中的 `capsule-server quarantine prune` 会先列出再删除，且不带 `--apply` 时不会删除任何内容（[](/design/filesystem/server/#quarantine-retention)）。`scrub` 会报告隔离区的大小，请通过它进行监控。

<a id="reverse-proxy"></a>

### 反向代理

TLS 在代理处终止，绝不在服务器处终止（[](/design/networking/#transport-security)）。服务器监听明文 HTTP，并且必须只能从代理访问：通过回环地址、私有网络或 Pod 网络。请将代理配置为：

- 接受至少 16 MiB 的请求体，即最大的上传分块；
- 将请求体流式传输给服务器，而不是先缓冲；
- 禁用 TLS 早期数据（0-RTT），或者附带 `Early-Data: 1` 请求头转发，以便服务器拒绝写入；
- 不对请求体设置总时长超时，并将空闲超时设置得比服务器的停滞上限更长，这样缓慢但持续传输的分块永远不会被切断；
- **原样透传压缩**。服务器自行决定压缩哪些响应，并且从不压缩 `application/octet-stream`（[](/design/networking/#compression)）。代理按服务器发送的原样透传 Capsule 的上游响应：它不得（MUST NOT）对响应进行 gzip 或其他压缩，不得（MUST NOT）解压响应，也不得添加、修改或删除 `Content-Encoding`，并且必须（MUST）原样转发客户端的 `Accept-Encoding`。在 nginx 中，请为 Capsule 站点保持 `gzip off;`，它会覆盖从发行版的 `nginx.conf` 继承来的 `gzip on;`。在 Caddy 中，不要添加 `encode` 指令；在 Traefik 中，不要挂载 `compress` 中间件。

代理面向客户端一侧的 HTTP/2 和 HTTP/3 是可选的；客户端会回退到 HTTP/1.1。如果需要内核 TLS（kTLS）和网卡 TLS 卸载，它们属于代理的设置（在 nginx 中为 `ssl_conf_command Options KTLS;` 配合 `sendfile on;`）。服务器自身的 TLS 栈仅用于它主动发起的连接：连接到 OpenID Connect 提供方以及联邦对等节点。这些连接信任内置的 Mozilla 根证书以及任何 `OIDC_CA_BUNDLE`，而不是主机的证书存储。

<a id="clock"></a>

### 时钟

服务器以自己的时钟判断时间，多条规则都依赖于这个时钟的准确性。请在主机上使用 NTP（chrony 或 systemd-timesyncd）保持其同步。容器读取的是主机的时钟，因此请同步主机。时钟偏差应当（SHOULD）保持在一秒以内，并且必须（MUST）保持在 30 秒以内。超出这些界限时：

| 偏差 | 会出现什么问题 |
| --- | --- |
| 超过 30 秒 | 第二因素验证码开始失效：验证码仅在服务器时钟的一个 30 秒步长内被接受（[](/design/authentication/)） |
| 超过 5 分钟 | 联邦在两个方向上都会停止：对等节点拒绝本服务器签名的请求、提示和报告，本服务器也拒绝对方的（[](/design/federation/)） |
| 时钟偏快 | 废纸篓中的内容会在保留期结束前被清除，及时发起的恢复也会被拒绝（[](/design/organization/#retention-window)）。会话、共享链接和上传会话会提前过期 |
| 时钟偏慢 | 已过期的会话、共享链接和计数器会在超出配置时长后仍然有效 |
| 严重偏差 | 客户端会拒绝 `received_at` 不合理的托管回执（[](/design/transfer/durability/)） |

只有在时钟同步之后才能启动 `serve`。在 systemd 下，请将其排在 `time-sync.target` 之后，并启用 `chrony-wait.service` 或 `systemd-time-wait-sync.service`。从快照恢复的虚拟机会以快照中的时钟继续运行：请让 NTP 在 `serve` 启动之前校正时钟。服务器目前还不能自行检测或报告时钟偏差。

<a id="hardware"></a>

## 硬件

以下数据适用于在同一节点上同时运行服务器和 PostgreSQL 的情况。它们是根据服务器的工作内容推算出的容量规划建议，而非基准测试结果。

服务器的工作负载很轻，因此大部分硬件预算应当投入到**存储**上。它在每个分块到达时用 SHA-256 计算哈希，并在最终化时以及 `scrub --deep` 期间重新计算哈希。它在最终化时验证客户端的混合签名（Ed25519 和 ML-DSA-65），以同样的方式签署回执，并在每次密码登录时运行一次 Argon2id（每次约 19 MiB）。它从不解码、缩放或转码媒体，从不加密或解密，并将 TLS 交给反向代理处理。上传的请求体最多只缓冲一个分块，因此内存占用不会随文件大小增长。

| | 最低配置 | 推荐配置 |
| --- | --- | --- |
| **CPU** | 2 核，x86_64 或 arm64 | 4 核。SHA-2 指令（x86_64 上的 SHA-NI、Armv8 加密扩展）可以加快哈希计算 |
| **内存** | 2 GB | 4 到 8 GB。对于大型照片库，更多内存有助于 PostgreSQL 的缓存，但投资存储的回报更早显现 |
| **系统存储** | SSD，10 GB 可用空间 | 为 PostgreSQL 使用 SSD 或 NVMe |
| **Blob 存储** | 照片库的大小，外加进行中的上传所需的空间 | 冗余磁盘（镜像、RAID 或 ZFS），容量按数年的增长规划。机械硬盘即可 |
| **网络** | 10 Mbit/s 上行 | 局域网内千兆；如果设备从外部同步，则上行 50 Mbit/s 或更高 |
| **电源** | — | 一台 UPS，这样断电不会在 PostgreSQL 写入途中将其中断 |

- **`BLOB_ROOT` 保持在同一个文件系统上**。整个目录树（包括 `incoming/`）必须位于一个文件系统中，因此不能拆分到 SSD 和磁盘阵列上，也不能拆分到两个 ZFS 数据集上。请把它全部放在阵列上。
- **把服务器用不到的内存留给 PostgreSQL**。每个经过身份验证的请求都会从 PostgreSQL 读取其会话，每个上传分块都会在那里记录其范围，因此它的缓存既服务于照片库索引，也服务于请求路径。会话、上传会话和计数器都很小，过期后服务器会将其删除。
- **上行带宽是远程设备的瓶颈**。位于你网络之外的手机以你连接的*下行*速度上传，但每次获取预览或原始文件时，数据都以你的*上行*速度流出。分块只有在停止到达时才会被切断，绝不会因为耗时长而被切断；客户端会根据测得的速度调整分块大小，使其在较短的目标时间内完成，因此慢速链路会以自己的速率上传。请让服务器的停滞上限低于代理的空闲超时，这样服务器会在代理切断连接之前对停滞的分块作出应答。
- **按原始文件而非衍生文件来规划磁盘容量**。在一个由 RAW 文件和视频组成的照片库中，衍生文件只占很小一部分。服务器对每个 blob 只保存一份，即客户端加密后的形式。
- **用户增加的是带宽，而不是计算量**。搜索、缩略图和整理都在客户端运行。浏览的人越多，需要提供的字节就越多，但几乎不会增加 CPU 或内存的需求。

`capsule-server/compose.yaml` 以适合本地使用的设置运行 Postgres（`mise run serve-deps`）。其中的凭据仅供开发使用。

<a id="build"></a>

## 构建

```sh
cargo build --release -p capsule-server -p capsule-server-migration
```

这些二进制文件链接 glibc，无需安装其他任何依赖。请以非特权用户身份运行它们，并让该用户唯一可写的路径是 `BLOB_ROOT`，这与下文容器镜像所强制的形态相同。

<a id="container-image"></a>

## 容器镜像

:::caution
目前尚未发布任何镜像，`/livez` 和 `/readyz` 路由以及 `healthcheck` 子命令也尚未实现。本节是构建镜像所遵循的契约。
:::

- **基础镜像**：`gcr.io/distroless/cc-debian12:nonroot`，按摘要固定。它包含 glibc、GCC 运行时、CA 证书和时区数据，不含 shell 或包管理器。
- **架构**：一个面向 `linux/amd64` 和 `linux/arm64` 的多架构镜像，每种架构都针对其 glibc 目标原生构建。构建不针对某一款 CPU 调优；哈希计算会在运行时选择 SHA-NI 或 Armv8 加密扩展。
- **内容**：`capsule-server`（入口点，默认为 `serve`）和 `capsule-server-migration`，二者来自同一次构建。
- **用户**：非 root 用户，UID 和 GID 均为 65532。容器不需要任何 Linux capability：请将其全部移除并设置 `no-new-privileges`。
- **只读根文件系统**。服务器只在 `BLOB_ROOT` 下写入，因此请将 blob 目录作为唯一可写的卷挂载（属主为 UID 65532），并以只读方式挂载根文件系统。
- **监听**：在容器内设置 `SERVER_HOST=0.0.0.0`（或 `::`）；默认端口为 3000。只向反向代理公开该端口（[](#反向代理)）。
- **日志**输出到 stderr；如使用日志收集器，请设置 `LOG_FORMAT=json`。
- **停止**：`serve` 收到 `SIGTERM` 时会正常关闭。因停止而中断的上传会从服务器已提交的范围处继续。
- **单个副本**。每个 blob 目录只运行一个 `serve`（[](#blob-目录)）。在 Kubernetes 上，请使用单个副本并采用 `Recreate` 更新策略：滚动更新会在旧服务器仍在提供服务时就启动新服务器。
- **迁移**在 `serve` 启动之前使用同一镜像标签运行，可以作为 Job、init 容器或 `ExecStartPre=`：`capsule-server-migration up`（[](#先迁移再启动)）。
- **维护**命令使用同一镜像、以同一用户身份、在同一个卷上运行，可以作为 CronJob 或定时器（[](#维护)）。
- **健康检查**：`/livez` 在进程提供服务期间响应 `GET`，不检查其他任何内容，用于存活探针。`/readyz` 仅在 PostgreSQL 有响应、架构与二进制文件匹配且 `BLOB_ROOT` 可写时才响应，用于就绪探针。两者都位于 `/v1` 之外，因此探针不依赖于协议版本，并且两者都无需能通过代理访问。镜像中没有 `curl`，因此它自身的健康检查运行 `capsule-server healthcheck`，该命令会调用本地监听器上的 `/livez`。
- **时钟**：容器读取主机的时钟；请同步主机（[](#时钟)）。

<a id="configure"></a>

## 配置

配置只通过环境变量进行。`capsule-server/.env.example` 是一个起始模板，本页列出了运维者必须自行决定的变量。缺少以下任一变量时，持久化服务器都会拒绝启动（退出状态 2），并一次性列出所有缺失的变量：

| 变量 | 用途 |
| --- | --- |
| `DATABASE_URL` | PostgreSQL 连接 |
| `BLOB_ROOT` | Blob 目录 |
| `JWT_ED25519_DER` | 用于签署会话令牌的 Base64 Ed25519 密钥 |
| `ATTESTATION_KEY_SEED` | 用于签署托管回执的 Base64 种子；必须独立于令牌密钥 |

- **首先要保护好令牌密钥**。数据库中不保存任何持有者（bearer）凭据，因此泄露的数据库或备份本身无法产生任何会话；正是 `JWT_ED25519_DER` 会把这样的泄露变成所有活跃会话的泄露（[](/design/authentication/#session-token)）。刷新令牌重用检测（[](/design/authentication/#token-lineage-and-reuse)）对此无能为力，因为持有该密钥的人可以直接签发令牌，而无需重放令牌。请将它保存在你的密钥存储中，绝不要放在数据库或数据库的备份中；如果怀疑发生了泄露，请轮换它，这会让所有设备退出登录。
- **第二因素密封密钥已在计划中**。服务器将使用一个独立于令牌密钥的专用密钥，对静态存储的 TOTP 密钥进行密封，这样数据库或备份泄露不会暴露任何第二因素，轮换令牌密钥也不会让任何第二因素失效（[](/design/authentication/#the-second-factor)）。其设置项目前尚不存在；推出后，请像对待令牌密钥一样，将它与数据库及其备份分开保管。
- **开放注册需要默认配额**（计划中）。一旦配额可以配置，在启用了密码注册却没有默认限额的情况下，`serve` 会拒绝启动；不需要限额的运维者须显式地将其设为 `unlimited`，每次启动时都会以 `WARN` 级别记录这一设置（[](/design/quota/#thresholds-and-states)）。目前所有部署都不受限额约束。

未设置 `SYNC_CURSOR_MAC_KEY` 时，它由 `JWT_ED25519_DER` 派生，因此除非你显式设置它，否则轮换令牌密钥也会使尚未使用的同步游标失效。只有设置了 `FEDERATION_ENABLED=true` 和 `FEDERATION_URL` 时才会启用联邦；请保持关闭。

请指明你的反向代理，使按来源计算的速率限制看到的是客户端而不是代理。

| 变量 | 含义 |
| --- | --- |
| `TRUSTED_PROXIES` | 以逗号分隔的地址和 CIDR 网络（`10.0.0.0/8`）。`/0` 网络会被拒绝，因为它会信任所有发送方。 |
| `TRUSTED_PROXY_HOPS` | 信任距离服务器最近的这么多个转发跳点，无论其地址如何。 |
| `TRUSTED_PROXY_HEADER` | 服务器读取的唯一一个转发字段：`x-forwarded-for`（默认）或 `forwarded`（RFC 7239）。任何其他值都会导致拒绝启动。 |

未设置时，服务器不信任任何转发请求头。这是安全的，但代理后面的所有客户端会共享代理的额度，于是一个猜测密码的陌生人就会拖慢所有人的登录（[](/design/authentication/#sign-in-limits)）；当服务器在不信任任何跳点的情况下看到转发请求头时，会记录一条警告。相反的错误更严重：信任一个不追加客户端地址的跳点，会让每个客户端都能自选速率限制键。`TRUSTED_PROXY_HEADER` 未指定的字段无论由谁发送都会被忽略，因此客户端无法通过发送一个会被你的代理原样转发的字段来自选速率限制键。请指定你的代理所追加的那个字段。使用默认值时，请让代理将它看到的地址追加到 `X-Forwarded-For`，nginx 的做法如下：

```nginx
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
```

仅当你的代理会向 `Forwarded` 追加自己的元素且不写入 `X-Forwarded-For` 时，才设置 `TRUSTED_PROXY_HEADER=forwarded`。

<a id="migrate-then-start"></a>

## 先迁移，再启动

`serve` 从不执行迁移，并会拒绝比二进制文件旧的数据库架构。每次升级时都要先运行迁移：

```sh
DATABASE_URL=… capsule-server-migration up
capsule-server serve
```

`serve` 将日志写入 stderr，并向 stdout 打印 `listening on <url>`。不要在生产环境中使用 `--memory`：它会把除 blob 以外的所有内容都保存在进程内存中。

<a id="maintenance"></a>

## 维护

请定期调度这些一次性命令（cron 或定时器）。它们需要 `DATABASE_URL` 和 `--blob-root`，并且默认以试运行方式执行：

- `capsule-server gc --apply`：在宽限期之后回收未被引用的 blob，并删除已过期的会话、上传会话和计数器。即使存储空间并不紧张，也要定期运行。
- `capsule-server purge --apply`：彻底清除保留期已过的软删除资源。它还需要服务器的 `ATTESTATION_KEY_SEED`，因为每次清除都会先签名写入所有者账本（[](/design/filesystem/server/#the-owner-ledger)），缺少该种子时会拒绝运行。当账本无法记录某次清除时，它会以非零状态退出；该资源会保留到下一次运行。
- `capsule-server scrub [--deep]`：只读地检查索引与 blob 存储的完整性。它**发现任何问题都会以非零状态退出**，因此请据此设置告警（[](/design/filesystem/maintenance/#server-side-integrity-scrub)）。

在恢复备份或丢失数据库之后，请根据 `BLOB_ROOT` 下的所有者账本重建索引（[](/design/filesystem/server/#rebuild)）。请在主机上设置 `ATTESTATION_KEY_SEED`，以便它验证账本的签名。

1. `capsule-server rebuild` 将索引与账本进行比较。
2. 在停止 `serve` 的情况下，`capsule-server rebuild --apply` 会恢复索引中缺少的行。它从不覆盖索引中已有的行。对于丢失了尾部的账本，它会重新锚定，之后服务器会再次向该所有者写入。
3. 如果报告显示某个账本被重写过，服务器会拒绝该所有者的写入，直到你调查清楚并运行 `capsule-server rebuild --apply --accept-rewrites`。

如果服务器在记录 blob 大小之前就应用了生命周期操作（删除、恢复、元数据更新、衍生文件、替换），那么这些引用的大小会记为 0。运行一次 `capsule-server repair-sizes` 将它们列出，然后运行 `capsule-server repair-sizes --apply`，根据 blob 存储补全每个大小。
它可以在 `serve` 运行时安全执行，每个修复后的资源都会重新发送给客户端。当某个 blob 缺失或为空时，它会以非零状态退出，并保持该引用不变。

<a id="backups"></a>

## 备份

持久状态由 PostgreSQL 加上 `BLOB_ROOT`（blob 和 `ledger/`）组成；请同时备份两者。数据库还保存着登录会话和进行中的上传会话，因此恢复后用户仍会保持登录。也请将上述密钥变量保存在你的密钥存储中：它们不在数据库里，而服务器的签名身份和证明身份都依赖于它们。

先备份数据库，再备份 blob：

```sh
pg_dump --format=custom --file=capsule.dump "$DATABASE_URL"
rsync -a "$BLOB_ROOT"/ backup/blob-root/
```

`serve` 可以继续运行。一个 blob 会先写入磁盘并追加其账本条目，然后引用它的索引行才会提交，因此在转储之后进行的 blob 复制包含转储所引用的每一个 blob，还可能包含转储遗漏的写入。备份进行期间不要运行 `gc --apply` 或 `purge --apply`：两者都可能删除转储仍在引用的 blob。

下文的重建会恢复在转储与 blob 复制之间上传的资源，因为它会添加索引中缺少的每一行。它从不更改转储中已有的行，因此在这段时间内对较早资源进行的删除、恢复或元数据编辑不会被应用。只有当这类更改改变了资源的 blob 时，重建才会将其报告为不一致的行（并以非零状态退出）；删除或恢复则不会被报告。当备份必须捕获所有更改时，请在备份这两部分期间停止 `serve`。如果恢复时报告了不一致的行，而旧主机仍在运行，请以这种方式重新进行备份。

<a id="restoring-onto-a-new-host"></a>

### 恢复到新主机

1. 启动一个空的 PostgreSQL，并设置与旧主机相同的 `JWT_ED25519_DER` 和 `ATTESTATION_KEY_SEED`。
2. 恢复数据库。转储中包含架构及其迁移记录，因此不要先运行 `capsule-server-migration up`：
   `pg_restore --no-owner --exit-on-error --dbname "$DATABASE_URL" capsule.dump`。
3. 将 blob 目录复制回 `BLOB_ROOT`，并保持在同一个文件系统上。
4. 在停止 `serve` 的情况下，运行 `capsule-server rebuild` 并阅读其报告。报告会列出转储之后上传的资源；`capsule-server rebuild --apply` 会以它们原有的序列号恢复这些资源，从而使客户端的同步游标保持有效。
5. 运行 `capsule-server scrub --deep`。它应当不报告任何问题；发现任何问题都会以非零状态退出。
6. 启动 `serve`。会话会随数据库一起恢复，因此在转储时已登录的客户端仍会保持登录，除非其会话此后已经过期。在开始监听之前，`serve` 会丢弃所有其 `BLOB_ROOT` 下暂存文件未被恢复的未完成上传会话，并删除所有没有对应会话的暂存文件；上传被丢弃的客户端会重新开始上传该文件。客户端会从中断处继续同步。

如果数据库丢失且没有转储，请迁移一个空数据库，然后对 blob 目录运行 `capsule-server rebuild --apply`。它会根据所有者账本恢复每一条资源行。账户、已发布的设备目录、相册成员关系和共享链接只保存在数据库中，重建无法恢复它们。

在把真实数据托付给这套流程之前，请先演练一遍，例如使用 `capsule restore` 检查每个原始文件是否都能逐字节恢复。`capsule-e2e/tests/backup_restore_drill.rs` 会针对容器运行同样的步骤（`mise run test-rust-containers`）。
