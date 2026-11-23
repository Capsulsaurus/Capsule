---
title: 自行代管
description: 在 PostgreSQL 與檔案系統 blob 儲存區上執行 capsule-server
status: draft
---

目前尚未發布任何版本，因此您需要從原始碼建置伺服器。伺服器只儲存密文，從不解碼媒體；所有處理與加密都由用戶端完成。

<a id="what-you-need"></a>

## 需要準備的項目

- **PostgreSQL 18**：持久化索引（帳號、資源、配額）以及伺服器中會過期的狀態（登入工作階段、上傳工作階段、速率計數器）。它是伺服器使用的唯一資料庫，而且必須在其持久性設定完好的情況下執行（[](#postgresql)）。
- **一個 blob 目錄**（`BLOB_ROOT`），位於**單一 POSIX 檔案系統**上，該檔案系統須遵守 `fsync` 並共用檔案鎖定。它保存所有 blob，並在 `ledger/` 下保存每個擁有者的簽章帳本，索引可以據此重建（[](#blob-目錄)）。
- **一個終止 TLS 的反向 Proxy**，位於伺服器前方，並原封不動地傳遞壓縮（[](#反向-proxy)）。
- **同步的時鐘**：主機透過 NTP 保持時鐘同步（[](#時鐘)）。

### PostgreSQL

伺服器在一列資料認可後即回應寫入，因此如果 PostgreSQL 確認了一次認可之後又將其遺失，就可能讓已撤銷的工作階段重新生效，或讓第二因素驗證碼被接受兩次。請維持：

- `fsync = on` 與 `full_page_writes = on`，也就是預設值；
- `synchronous_commit` 為 `on`、`local`、`remote_write` 或 `remote_apply`，絕不能為 `off`；
- 資料目錄位於會遵守排清（flush）操作的儲存裝置上。沒有斷電保護的磁碟寫入快取，或主機會忽略排清操作的虛擬磁碟，會在斷電時遺失已確認的認可（[](/design/filesystem/server/)）。

PostgreSQL 可以位於與 `BLOB_ROOT` 不同的檔案系統、磁碟或主機上。

<a id="blob-directory"></a>

### Blob 目錄

`BLOB_ROOT` 是與檔案系統之間的一份契約（[](/design/filesystem/server/#blob-root-filesystem-contract)），其配置方式請見 [](/design/filesystem/server/#blob-store-layout)：

- **單一檔案系統**。完成處理是一次不可分割的重新命名，因此整個目錄樹（包括 `incoming/`）不能跨越多個磁碟區、資料集或 Kubernetes PersistentVolume。
- **`fsync` 必須確實生效**：在伺服器確認寫入之前，對檔案與目錄的 `fsync` 都必須確實生效。
- **檔案鎖定必須共用**：所有寫入該目錄樹的處理程序都必須共用檔案鎖定。`serve` 與 `purge --apply` 在附加之前，會鎖定每個擁有者的 `ledger/<owner>/.lock`（[](/design/filesystem/server/#concurrent-appenders)）。
- **每個 blob 目錄只執行一個 `serve`**。上傳分塊與完成處理都在提供服務的處理程序內部認領（[](/design/transfer/upload/)），因此不支援在同一個目錄樹上執行兩個 `serve` 處理程序，無論它們位於同一部主機或兩部主機上。`gc`、`purge` 與 `scrub` 可以與它並行執行；`rebuild --apply` 則需要先將它停止（[](#維護)）。

支援：

- 本機儲存或區塊儲存上的 ext4、XFS 與 btrfs：磁碟、RAID、LVM、iSCSI 或雲端區塊儲存磁碟區；
- `sync=standard`（預設值）或 `sync=always` 的 ZFS；
- NFSv4，或搭配鎖定管理員的 NFSv3，兩者都須以 `sync` 方式匯出；
- 由上述任一儲存支援的 Kubernetes PersistentVolume 或容器磁碟區。

不支援：

- 停用寫入屏障的掛載，或主機會忽略排清操作的虛擬磁碟；
- `sync=disabled` 的 ZFS，它會在未寫入的情況下確認 `fsync`；
- 以 `async` 方式匯出的 NFS（它會確認 NFS 伺服器尚未儲存的寫入），或以 `nolock`、`local_lock=flock` 或 `local_lock=all` 掛載的 NFS（這些選項會將鎖定限制在單一主機上）；
- SMB/CIFS 共用；
- FUSE 檔案系統（sshfs、rclone mount、s3fs、gcsfuse）以及任何物件儲存掛載；
- 將一個目錄樹分散到多顆磁碟上的集區或聯合檔案系統（mergerfs、位於 `/mnt/user` 下的 Unraid 使用者共用）：成員磁碟之間的重新命名其實是複製，而不是不可分割的重新命名；
- 容器本身的可寫入層，以及以繫結掛載方式掛進容器虛擬機器的 macOS 或 Windows 主機目錄。請改用虛擬機器內的磁碟區。

在 Linux 上，NFS 上的 `flock` 是以位元組範圍鎖定來模擬，而鎖定管理員會在主機之間共用這些鎖定。如果停用了鎖定，不同主機上的 `serve` 與 `purge --apply` 可能會交錯地附加內容到同一個帳本。

- **在信任硬體之前，先進行斷電演練**。`serve` 在開機時會檢查檔案系統類型、不可分割的重新命名與鎖定共用，但任何軟體檢查都無法察覺那些確認了實際上並未執行的排清操作的磁碟、控制器或 Hypervisor。請在實際部署的硬體上，一邊上傳一邊切斷電源（而不是正常關機），然後開機並執行 `capsule-server scrub --deep`：伺服器確認過的每一次上傳都必須完好無缺（[](/design/filesystem/server/#blob-root-filesystem-contract)）。
- **保留可用空間**。blob 根目錄會保留一部分可用空間，預設取磁碟區容量的 2% 與 4 GiB 中較大者。低於此值時，新的上傳會以 `507` 遭到拒絕，而已在伺服器上進行的上傳仍可完成，刪除操作也仍然有效（[](/design/quota/#enforcement-points)）。此保留功能已在規劃中，但尚未實作；在此之前，磁碟寫滿時所有寫入都會遭到拒絕。
- **審慎清理隔離區**。`quarantine/` 會保存重新計算雜湊失敗或無法讀取的 blob，直到您將其刪除為止（之後已修復的除外）。規劃中的 `capsule-server quarantine prune` 會先列出再移除，且不加 `--apply` 時不會移除任何內容（[](/design/filesystem/server/#quarantine-retention)）。`scrub` 會回報隔離區的大小，請透過它進行監控。

<a id="reverse-proxy"></a>

### 反向 Proxy

TLS 在 Proxy 處終止，絕不在伺服器處終止（[](/design/networking/#transport-security)）。伺服器監聽未加密的 HTTP，而且必須只能從 Proxy 存取：透過回送位址、私人網路或 Pod 網路。請將 Proxy 設定為：

- 接受至少 16 MiB 的要求本文，也就是最大的上傳分塊；
- 將要求本文串流傳送給伺服器，而不是先加以緩衝；
- 停用 TLS 早期資料（0-RTT），或附上 `Early-Data: 1` 標頭轉送，讓伺服器能夠拒絕寫入；
- 不對要求本文設定總時長逾時，並將閒置逾時設定得比伺服器的停滯上限更長，讓緩慢但持續傳輸的分塊永遠不會被中斷；
- **原封不動地傳遞壓縮**。伺服器自行決定要壓縮哪些回應，而且從不壓縮 `application/octet-stream`（[](/design/networking/#compression)）。Proxy 依伺服器送出的原樣傳遞 Capsule 的上游回應：它不得（MUST NOT）對回應進行 gzip 或其他壓縮，不得（MUST NOT）解壓縮回應，也不得新增、變更或移除 `Content-Encoding`，並且必須（MUST）原封不動地轉送用戶端的 `Accept-Encoding`。在 nginx 中，請為 Capsule 網站維持 `gzip off;`，它會覆寫從發行版 `nginx.conf` 繼承而來的 `gzip on;`。在 Caddy 中，請勿加入 `encode` 指示詞；在 Traefik 中，請勿附加 `compress` 中介軟體。

Proxy 面向用戶端一側的 HTTP/2 與 HTTP/3 為選用；用戶端會退回使用 HTTP/1.1。如果需要核心 TLS（kTLS）與網路卡 TLS 卸載，它們屬於 Proxy 的設定（在 nginx 中為 `ssl_conf_command Options KTLS;` 搭配 `sendfile on;`）。伺服器本身的 TLS 堆疊只用於它主動建立的連線：連到 OpenID Connect 提供者以及聯邦對等節點。這些連線信任內建的 Mozilla 根憑證以及任何 `OIDC_CA_BUNDLE`，而不是主機的憑證存放區。

<a id="clock"></a>

### 時鐘

伺服器以自己的時鐘判斷時間，有好幾項規則都仰賴這個時鐘的準確性。請在主機上使用 NTP（chrony 或 systemd-timesyncd）讓它保持同步。容器讀取的是主機的時鐘，因此請同步主機。時鐘偏差應該（SHOULD）保持在一秒以內，而且必須（MUST）保持在 30 秒以內。超出這些界限時：

| 偏差 | 會出現什麼問題 |
| --- | --- |
| 超過 30 秒 | 第二因素驗證碼開始失效：驗證碼只在伺服器時鐘的一個 30 秒間隔內被接受（[](/design/authentication/)） |
| 超過 5 分鐘 | 聯邦在兩個方向上都會停止：對等節點會拒絕本伺服器簽章的要求、提示與報告，本伺服器也會拒絕對方的（[](/design/federation/)） |
| 時鐘偏快 | 垃圾桶中的內容會在保留期限結束前被清除，及時提出的還原也會遭到拒絕（[](/design/organization/#retention-window)）。工作階段、分享連結與上傳工作階段會提前過期 |
| 時鐘偏慢 | 已過期的工作階段、分享連結與計數器，會在超過設定的時間後仍然有效 |
| 嚴重偏差 | 用戶端會拒絕 `received_at` 不合理的保管收據（[](/design/transfer/durability/)） |

請在時鐘同步之後才啟動 `serve`。在 systemd 下，請將它排在 `time-sync.target` 之後，並啟用 `chrony-wait.service` 或 `systemd-time-wait-sync.service`。從快照還原的虛擬機器會以快照中的時鐘繼續執行：請讓 NTP 在 `serve` 啟動之前校正時鐘。伺服器目前還無法自行偵測或回報時鐘偏差。

<a id="hardware"></a>

## 硬體

以下數據適用於在同一個節點上同時執行伺服器與 PostgreSQL 的情況。這些是根據伺服器的工作內容推算出的容量規劃建議，而非效能測試結果。

伺服器的工作負載很輕，因此大部分的硬體預算應該投入到**儲存空間**上。它在每個分塊抵達時以 SHA-256 計算雜湊，並在完成處理時以及 `scrub --deep` 期間重新計算雜湊。它在完成處理時驗證用戶端的混合簽章（Ed25519 與 ML-DSA-65），以同樣的方式簽署收據，並在每次密碼登入時執行一次 Argon2id（每次約 19 MiB）。它從不解碼、縮放或轉碼媒體，從不加密或解密，並將 TLS 交給反向 Proxy 處理。上傳的要求本文最多只會緩衝一個分塊，因此記憶體用量不會隨檔案大小增加。

| | 最低需求 | 建議配置 |
| --- | --- | --- |
| **CPU** | 2 核心，x86_64 或 arm64 | 4 核心。SHA-2 指令（x86_64 上的 SHA-NI、Armv8 加密擴充功能）可加快雜湊計算 |
| **RAM** | 2 GB | 4 到 8 GB。對於大型相片庫，更多記憶體有助於 PostgreSQL 的快取，但投資儲存空間的效益會更早顯現 |
| **系統儲存空間** | SSD，10 GB 可用空間 | 為 PostgreSQL 使用 SSD 或 NVMe |
| **Blob 儲存空間** | 相片庫的大小，再加上進行中上傳所需的空間 | 備援磁碟（鏡像、RAID 或 ZFS），容量依數年的成長量規劃。傳統硬碟即可 |
| **網路** | 10 Mbit/s 上傳頻寬 | 區域網路內 Gigabit；如果裝置從外部同步，上傳頻寬 50 Mbit/s 以上 |
| **電源** | — | 一台 UPS，讓斷電不會在 PostgreSQL 寫入途中將其中斷 |

- **`BLOB_ROOT` 必須位於同一個檔案系統**。整個目錄樹（包括 `incoming/`）必須位於一個檔案系統中，因此不能拆分到 SSD 與磁碟陣列上，也不能拆分到兩個 ZFS 資料集上。請把它全部放在磁碟陣列上。
- **把伺服器用不到的記憶體留給 PostgreSQL**。每個經過驗證的要求都會從 PostgreSQL 讀取其工作階段，每個上傳分塊都會在那裡記錄其範圍，因此它的快取既服務相片庫索引，也服務要求路徑。工作階段、上傳工作階段與計數器都很小，過期後伺服器就會將其刪除。
- **上傳頻寬是遠端裝置的瓶頸**。位於您網路之外的手機會以您連線的*下載*速度上傳，但每次擷取預覽或原始檔時，資料都會以您的*上傳*速度流出。分塊只有在停止抵達時才會被中斷，絕不會因為耗時較久而被中斷；用戶端會依據測得的速度調整分塊大小，使其在較短的目標時間內完成，因此慢速連線會以自己的速率上傳。請讓伺服器的停滯上限低於 Proxy 的閒置逾時，這樣伺服器會在 Proxy 中斷連線之前回應停滯的分塊。
- **依原始檔而非衍生檔規劃磁碟容量**。在由 RAW 檔與影片組成的相片庫中，衍生檔只占很小的比例。伺服器對每個 blob 只保存一份，也就是用戶端加密後的形式。
- **使用者增加的是頻寬，而不是運算量**。搜尋、縮圖與整理都在用戶端執行。瀏覽的人越多，需要提供的位元組就越多，但幾乎不會增加 CPU 或記憶體的需求。

`capsule-server/compose.yaml` 會以適合本機使用的設定執行 Postgres（`mise run serve-deps`）。其中的認證資訊僅供開發使用。

<a id="build"></a>

## 建置

```sh
cargo build --release -p capsule-server -p capsule-server-migration
```

這些二進位檔會連結 glibc，不需要另外安裝任何東西。請以非特權使用者身分執行它們，並讓該使用者唯一可寫入的路徑是 `BLOB_ROOT`，這與下文容器映像檔所強制的形式相同。

<a id="container-image"></a>

## 容器映像檔

:::caution
目前尚未發布任何映像檔，`/livez` 與 `/readyz` 路由以及 `healthcheck` 子指令也尚未實作。本節是建置映像檔所遵循的契約。
:::

- **基礎映像檔**：`gcr.io/distroless/cc-debian12:nonroot`，以摘要值固定。它包含 glibc、GCC 執行階段、CA 憑證與時區資料，不含 shell 或套件管理員。
- **架構**：一個適用於 `linux/amd64` 與 `linux/arm64` 的多架構映像檔，每種架構都針對其 glibc 目標原生建置。建置不針對特定 CPU 調校；雜湊計算會在執行時選用 SHA-NI 或 Armv8 加密擴充功能。
- **內容**：`capsule-server`（進入點，預設為 `serve`）與 `capsule-server-migration`，兩者來自同一次建置。
- **使用者**：非 root 使用者，UID 與 GID 皆為 65532。容器不需要任何 Linux capability：請將其全部移除，並設定 `no-new-privileges`。
- **唯讀根檔案系統**。伺服器只會在 `BLOB_ROOT` 下寫入，因此請將 blob 目錄掛載為唯一可寫入的磁碟區（擁有者為 UID 65532），並以唯讀方式掛載根檔案系統。
- **監聽**：在容器內設定 `SERVER_HOST=0.0.0.0`（或 `::`）；預設連接埠為 3000。只將它公開給反向 Proxy（[](#反向-proxy)）。
- **日誌**會輸出到 stderr；若使用日誌收集器，請設定 `LOG_FORMAT=json`。
- **停止**：`serve` 收到 `SIGTERM` 時會正常關閉。因停止而中斷的上傳，會從伺服器已認可的範圍處繼續。
- **單一複本**。每個 blob 目錄只執行一個 `serve`（[](#blob-目錄)）。在 Kubernetes 上，請使用單一複本並採用 `Recreate` 更新策略：滾動更新會在舊伺服器仍在提供服務時就啟動新伺服器。
- **遷移**在 `serve` 啟動之前，以同一個映像檔標籤執行，可以作為 Job、init 容器或 `ExecStartPre=`：`capsule-server-migration up`（[](#先遷移再啟動)）。
- **維護**指令以同一個映像檔、同一個使用者身分、在同一個磁碟區上執行，可以作為 CronJob 或計時器（[](#維護)）。
- **健康狀態**：`/livez` 在處理程序提供服務期間回應 `GET`，不檢查其他任何項目，供存活探查使用。`/readyz` 只在 PostgreSQL 有回應、結構描述與二進位檔相符且 `BLOB_ROOT` 可寫入時才回應，供就緒探查使用。兩者都位於 `/v1` 之外，因此探查不依賴協定版本，而且兩者都不需要能透過 Proxy 存取。映像檔中沒有 `curl`，因此它本身的健康檢查會執行 `capsule-server healthcheck`，此指令會呼叫本機監聽器上的 `/livez`。
- **時鐘**：容器讀取主機的時鐘；請同步主機（[](#時鐘)）。

<a id="configure"></a>

## 設定

設定只能透過環境變數進行。`capsule-server/.env.example` 是一個起始範本，本頁列出了營運者必須自行決定的變數。缺少以下任一變數時，持久化伺服器都會拒絕啟動（結束狀態 2），並一次列出所有缺少的變數：

| 變數 | 用途 |
| --- | --- |
| `DATABASE_URL` | PostgreSQL 連線 |
| `BLOB_ROOT` | Blob 目錄 |
| `JWT_ED25519_DER` | 用於簽署工作階段權杖的 Base64 Ed25519 金鑰 |
| `ATTESTATION_KEY_SEED` | 用於簽署保管收據的 Base64 種子；必須獨立於權杖金鑰 |

- **最優先保護權杖金鑰**。資料庫中不保存任何持有人（bearer）密鑰，因此外洩的資料庫或備份本身無法產生任何工作階段；正是 `JWT_ED25519_DER` 會把這樣的外洩變成所有作用中工作階段的外洩（[](/design/authentication/#session-token)）。重新整理權杖重複使用偵測（[](/design/authentication/#token-lineage-and-reuse)）對此無能為力，因為持有該金鑰的人可以直接簽發權杖，而不需要重送權杖。請將它保存在您的機密存放區中，絕不要放在資料庫或資料庫的備份中；如果懷疑發生外洩，請輪替它，這會讓所有裝置登出。
- **第二因素密封金鑰已在規劃中**。伺服器將使用一把獨立於權杖金鑰的專用金鑰，對靜態儲存的 TOTP 密鑰進行密封，這樣資料庫或備份外洩不會暴露任何第二因素，輪替權杖金鑰也不會讓任何第二因素失效（[](/design/authentication/#the-second-factor)）。其設定目前尚不存在；推出之後，請像對待權杖金鑰一樣，將它與資料庫及其備份分開保管。
- **開放註冊需要預設配額**（規劃中）。一旦配額可以設定，在啟用密碼註冊卻沒有預設上限的情況下，`serve` 會拒絕啟動；不需要上限的營運者必須明確地將其設為 `unlimited`，每次啟動時都會以 `WARN` 層級記錄這項設定（[](/design/quota/#thresholds-and-states)）。目前所有部署都沒有上限。

未設定 `SYNC_CURSOR_MAC_KEY` 時，它會從 `JWT_ED25519_DER` 衍生，因此除非您明確設定它，否則輪替權杖金鑰也會讓尚未使用的同步游標失效。只有設定了 `FEDERATION_ENABLED=true` 與 `FEDERATION_URL` 時才會啟用聯邦；請保持關閉。

請指明您的反向 Proxy，讓依來源計算的速率限制看到的是用戶端而不是 Proxy。

| 變數 | 意義 |
| --- | --- |
| `TRUSTED_PROXIES` | 以逗號分隔的位址與 CIDR 網路（`10.0.0.0/8`）。`/0` 網路會遭到拒絕，因為它會信任所有傳送者。 |
| `TRUSTED_PROXY_HOPS` | 信任距離伺服器最近的指定數量轉送躍點，無論其位址為何。 |
| `TRUSTED_PROXY_HEADER` | 伺服器讀取的唯一一個轉送欄位：`x-forwarded-for`（預設值）或 `forwarded`（RFC 7239）。任何其他值都會導致拒絕啟動。 |

未設定時，伺服器不會信任任何轉送標頭。這是安全的，但 Proxy 後方的所有用戶端會共用 Proxy 的額度，於是一個猜測密碼的陌生人就會拖慢所有人的登入（[](/design/authentication/#sign-in-limits)）；當伺服器在不信任任何躍點的情況下看到轉送標頭時，會記錄一則警告。相反的錯誤更嚴重：信任一個不會附加用戶端位址的躍點，會讓每個用戶端都能自行選擇速率限制的鍵值。`TRUSTED_PROXY_HEADER` 未指定的欄位無論由誰送出都會被忽略，因此用戶端無法藉由送出一個會被您的 Proxy 原樣轉送的欄位，來自行選擇速率限制的鍵值。請指定您的 Proxy 會附加的那個欄位。使用預設值時，請讓 Proxy 將它看到的位址附加到 `X-Forwarded-For`，nginx 的做法如下：

```nginx
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
```

只有在您的 Proxy 會向 `Forwarded` 附加自己的元素，且不寫入 `X-Forwarded-For` 時，才設定 `TRUSTED_PROXY_HEADER=forwarded`。

<a id="migrate-then-start"></a>

## 先遷移，再啟動

`serve` 從不執行遷移，並會拒絕比二進位檔舊的資料庫結構描述。每次升級時都要先執行遷移：

```sh
DATABASE_URL=… capsule-server-migration up
capsule-server serve
```

`serve` 會將日誌寫入 stderr，並向 stdout 印出 `listening on <url>`。請勿在正式環境中使用 `--memory`：它會把 blob 以外的所有內容都保存在處理程序的記憶體中。

<a id="maintenance"></a>

## 維護

請排程執行這些一次性指令（cron 或計時器）。它們需要 `DATABASE_URL` 與 `--blob-root`，而且預設以模擬執行方式進行：

- `capsule-server gc --apply`：在寬限期過後回收未被參照的 blob，並刪除已過期的工作階段、上傳工作階段與計數器。即使儲存空間並不吃緊，也請定期執行。
- `capsule-server purge --apply`：永久清除保留期限已過的軟刪除資源。它也需要伺服器的 `ATTESTATION_KEY_SEED`，因為每次清除都會先簽章寫入擁有者帳本（[](/design/filesystem/server/#the-owner-ledger)），缺少此種子時會拒絕執行。當帳本無法記錄某次清除時，它會以非零狀態結束；該資源會保留到下一次執行。
- `capsule-server scrub [--deep]`：以唯讀方式檢查索引與 blob 儲存區的完整性。它**發現任何問題都會以非零狀態結束**，因此請據此設定警示（[](/design/filesystem/maintenance/#server-side-integrity-scrub)）。

在還原備份或遺失資料庫之後，請根據 `BLOB_ROOT` 下的擁有者帳本重建索引（[](/design/filesystem/server/#rebuild)）。請在主機上設定 `ATTESTATION_KEY_SEED`，讓它能驗證帳本的簽章。

1. `capsule-server rebuild` 會將索引與帳本進行比對。
2. 在停止 `serve` 的情況下，`capsule-server rebuild --apply` 會還原索引中缺少的資料列。它從不覆寫索引中已有的資料列。對於遺失了尾端的帳本，它會重新錨定，之後伺服器會再次寫入該擁有者的資料。
3. 如果報告指出某個帳本曾被改寫，伺服器會拒絕該擁有者的寫入，直到您調查清楚並執行 `capsule-server rebuild --apply --accept-rewrites` 為止。

如果伺服器在記錄 blob 大小之前就套用了生命週期操作（刪除、還原、中繼資料更新、衍生檔、取代），這些參照的大小會記為 0。請執行一次 `capsule-server repair-sizes` 將它們列出，再執行 `capsule-server repair-sizes --apply`，根據 blob 儲存區補上每個大小。
它可以在 `serve` 執行期間安全地執行，每個修復後的資源都會重新傳送給用戶端。當某個 blob 遺失或為空時，它會以非零狀態結束，並保留該參照不變。

<a id="backups"></a>

## 備份

持久狀態由 PostgreSQL 加上 `BLOB_ROOT`（blob 與 `ledger/`）組成；請一併備份兩者。資料庫也保存著登入工作階段與進行中的上傳工作階段，因此還原之後使用者仍會保持登入。也請將上述的金鑰變數保存在您的機密存放區中：它們不在資料庫裡，而伺服器的簽署身分與證明身分都仰賴它們。

請先備份資料庫，再備份 blob：

```sh
pg_dump --format=custom --file=capsule.dump "$DATABASE_URL"
rsync -a "$BLOB_ROOT"/ backup/blob-root/
```

`serve` 可以繼續執行。一個 blob 會先寫入磁碟並附加其帳本項目，之後參照它的索引資料列才會認可，因此在傾印之後進行的 blob 複製會包含傾印所參照的每一個 blob，也可能包含傾印遺漏的寫入。備份進行期間請勿執行 `gc --apply` 或 `purge --apply`：兩者都可能移除傾印仍在參照的 blob。

下文的重建會還原在傾印與 blob 複製之間上傳的資源，因為它會加入索引中缺少的每一筆資料列。它從不變更傾印中已有的資料列，因此在這段期間內對較早資源進行的刪除、還原或中繼資料編輯不會被套用。只有當這類變更改動了資源的 blob 時，重建才會將其回報為不一致的資料列（並以非零狀態結束）；刪除或還原則不會被回報。當備份必須擷取所有變更時，請在備份這兩部分期間停止 `serve`。如果還原時回報了不一致的資料列，而舊主機仍在執行，請以這種方式重新備份。

<a id="restoring-onto-a-new-host"></a>

### 還原到新主機

1. 啟動一個空的 PostgreSQL，並設定與舊主機相同的 `JWT_ED25519_DER` 與 `ATTESTATION_KEY_SEED`。
2. 還原資料庫。傾印中包含結構描述及其遷移紀錄，因此請勿先執行 `capsule-server-migration up`：
   `pg_restore --no-owner --exit-on-error --dbname "$DATABASE_URL" capsule.dump`。
3. 將 blob 目錄複製回 `BLOB_ROOT`，並保持在同一個檔案系統上。
4. 在停止 `serve` 的情況下，執行 `capsule-server rebuild` 並閱讀其報告。報告會列出傾印之後上傳的資源；`capsule-server rebuild --apply` 會以它們原有的序號還原這些資源，讓用戶端的同步游標保持有效。
5. 執行 `capsule-server scrub --deep`。它應該不會回報任何問題；發現任何問題時都會以非零狀態結束。
6. 啟動 `serve`。工作階段會隨資料庫一併還原，因此在傾印時已登入的用戶端仍會保持登入，除非其工作階段在那之後已經過期。在開始監聽之前，`serve` 會捨棄所有 `BLOB_ROOT` 下暫存檔案未被還原的未完成上傳工作階段，並刪除所有沒有對應工作階段的暫存檔案；上傳遭到捨棄的用戶端會重新開始上傳該檔案。用戶端會從中斷處繼續同步。

如果資料庫遺失且沒有傾印，請遷移一個空的資料庫，然後對 blob 目錄執行 `capsule-server rebuild --apply`。它會根據擁有者帳本還原每一筆資源資料列。帳號、已發布的裝置目錄、相簿成員資格與分享連結只保存在資料庫中，重建無法還原它們。

在把真實資料交給這套流程之前，請先演練一次，例如使用 `capsule restore` 檢查每個原始檔是否都能逐位元組還原。`capsule-e2e/tests/backup_restore_drill.rs` 會針對容器執行相同的步驟（`mise run test-rust-containers`）。
