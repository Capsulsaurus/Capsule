---
title: 用户指南
description: 使用 capsule CLI 日常使用 Capsule 照片库：导入、备份、验证、整理以及安全删除。
audit: none
---

本指南介绍参考客户端 `capsule` CLI。标有（_计划中_）的命令已列入[路线图](https://github.com/Capsulsaurus/Capsule/blob/master/ROADMAP.md)，但目前尚不可用。参数以 `capsule <command> --help` 为准；CLI 参考（[](/reference/cli/)）列出了全部参数。

:::caution
Capsule 目前处于 Alpha 阶段。在首个内部试用（dogfood）版本发布之前，请为你导入的每个原始文件另外保留一份独立备份。
:::

<a id="set-up"></a>

## 设置

1. 创建照片库：`capsule library init --name "Photos"`。照片库是一个本地的加密目录；它的密码短语保护本机上的密钥。
2. 登录你的服务器：`capsule auth register` 或 `capsule auth login`（单点登录请使用 `--oidc` 或 `--device`）。
3. 设置恢复：`capsule auth escrow`。请将打印出的恢复凭据离线保存。如果没有它和你的密码短语，丢失一台机器就意味着丢失整个照片库，因为服务器无法解密任何内容。

<a id="import-and-back-up"></a>

## 导入与备份

- `capsule import <path>` 将文件复制到照片库中，为每个资源的来源记录和元数据签名，并生成缩略图。`--provider takeout` 会导入 Google Takeout 导出包及其附属元数据（[](/guides/google-photos-migration/)）。
- `capsule push` 上传照片库。上传会从中断处继续，因此重新运行 `push` 始终是安全的。每上传完一个资源，`push` 都会请求服务器确认它已持久保存每个 blob，并将服务器签发的托管回执保存在照片库中。某个资源失败时会被报告，运行仍会继续。只要有任何资源失败，命令就会以非零状态退出，`--json` 会列出每个资源的结果。
- `capsule import --push` 一步完成以上两项操作。
- **在托管得到验证之前，请保留原始文件**。只有当服务器报告某个资源已持久保存，并签发了经过签名的托管回执（`push` 会将其报告为已验证）后，该资源才算安全存储。在此之前不要删除源文件。`import --move --push` 仅在每个资源通过验证后才删除对应的源文件。不带 `--push` 时，`--move` 会保留所有源文件并给出说明；之后对同一批文件运行 `import --move --push` 即可将其释放。
- `capsule restore`（_计划中_）会下载原始文件，并逐字节核对它们与导入时签名的内容是否一致。

<a id="organize"></a>

## 整理

- **标签**可以自由填写，可附加到资源、堆叠（RAW+JPEG 配对、连拍、包围曝光）、相册或图像中的某个区域。你添加的标签与 AI 建议的标签分开保存，你可以采纳或忽略某条建议。资源标签目前已可用；堆叠、相册和区域标签则是（_计划中_）功能。
- `capsule show <asset>` 会打印资源的签名元数据所记录的内容：说明文字、评分、标签、位置、拍摄时间。
- `capsule repair capture-time` 会查找拍摄时间与原始文件 EXIF 不一致的资源，加上 `--apply` 时会予以更正。位置编辑功能（_计划中_）。

每次编辑都是一次经过签名、只可追加的更改：先前的值会保留在资源的来源历史中，来自两台设备的并发编辑会以确定性的方式合并。

<a id="delete-safely"></a>

## 安全删除

请在删除之前先修正地理位置等元数据，趁周围的资源仍能提供上下文。一张本身没有位置信息的照片，在同一次散步中拍摄的相邻照片还在照片库里时，很容易确定拍摄地点；一旦这些照片被删除，这些上下文也随之消失。

- `capsule cull` 将资源标记为留用、未决定或排除，并可按标记筛选。
- `capsule cull --sweep` 将已排除的资源移到废纸篓。移入废纸篓的资源在保留期（`--retain-days`）内仍可恢复，之后才会被彻底清除。
- `capsule trash delete --asset <id>` 会将资源同时移到服务器上和照片库中的废纸篓。已在废纸篓中的资源（例如运行 `capsule cull --sweep` 之后）会保留其移入时的保留期：`--retain-days` 仅适用于新的删除操作，`--json` 会以 `retention_until` 报告所发送的保留期。
- `capsule trash restore --asset <id>` 会恢复一个资源。保留期结束后的恢复请求会被拒绝（在 `--json` 下为 `retention_elapsed`）且不会重试；该资源会留在废纸篓中，直到被清除。

<a id="share"></a>

## 共享

- `capsule share create --album <id>` 或 `--asset <id>` 会生成一个仅供查看的链接；`--protect` 为链接添加密码短语，`--expires-at` 设置过期时间。链接的密钥位于 URL 片段中，永远不会到达服务器。
- `capsule share revoke <id>` 使链接无法再打开。

<a id="sync-another-machine"></a>

## 同步另一台机器

- `capsule sync` 从服务器拉取照片库的变更流；`capsule list` 显示它同步下来的内容。
- `capsule auth recover` 使用恢复凭据在新机器上恢复账户。
