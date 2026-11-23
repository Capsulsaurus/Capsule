---
title: 疑難排解
description: 收集回報所需的版本與日誌，並解決 Capsule 伺服器、匯入或相片庫的常見問題。
audit: none
---

發生問題時請從這裡開始。如果下列內容都無法解決問題，[](/support/) 說明了該到哪裡提問，而第一節中的診斷資訊正是錯誤回報所需要的。

<a id="collect-diagnostics"></a>

## 收集診斷資訊

<a id="version"></a>

### 版本

目前尚未發布任何版本，因此版本號碼無法區分不同的建置。所有內容都從原始碼建置，所以請回報您建置時所用的提交：在您的工作副本中執行 `git rev-parse --short HEAD`。CLI 沒有版本旗標，而 `capsule-server --version` 只會印出 crate 版本，所有建置的這個版本都相同。

<a id="logs"></a>

### 日誌

CLI 與伺服器會將日誌寫入 **stderr**。stdout 只承載指令印出的內容，在 `--json` 下就是單一 JSON 文件，因此您可以分別擷取兩者：

```sh
RUST_LOG=debug capsule push --library ~/Photos.capsule 2> capsule.log
```

- `RUST_LOG` 設定日誌的詳細程度。發行版建置預設以 `info` 層級記錄，除錯建置則預設以 `debug` 層級記錄。重現問題時請使用 `RUST_LOG=debug`；只有在被要求時才使用 `RUST_LOG=trace`，因為它的輸出非常龐大。
- 伺服器的日誌格式由 `LOG_FORMAT` 設定：除錯建置預設為 `pretty`，發行版建置預設為 `json`（[](/development/local-development/#logs)）。
- 全螢幕 TUI（在終端機中直接執行不帶參數的 `capsule`）無法把日誌輸出到它正在繪製的畫面上，因此會將 `tui.log` 寫入 CLI 的快取目錄，也就是 `capsule status` 所印出的 Cache Directory。該檔案在每次工作階段都會被覆寫，因此請在再次啟動 TUI 之前先複製它。
- `--json` 輸出是指令執行情形最精確的紀錄，因為它會回報每個資源的結果。當指令回報失敗時，請附上它。

**分享之前請先遮蔽敏感資訊**。請刪去密碼短語、復原憑證、權杖、金鑰、伺服器位址與個人檔案路徑。絕對不要在任何地方貼上相片庫密碼短語或復原憑證，私下的回報也不例外。

<a id="the-server"></a>

## 伺服器

<a id="it-refuses-to-start"></a>

### 伺服器拒絕啟動

- **結束狀態為 2，並列出變數名稱**。持久化伺服器需要 `DATABASE_URL`、`BLOB_ROOT`、`JWT_ED25519_DER` 與 `ATTESTATION_KEY_SEED`，並會一次列出所有缺少的變數。請參閱 [](/guides/self-hosting/#configure)。
- **資料庫結構描述比二進位檔舊**。`serve` 從不執行遷移。每次升級時，請在 `serve` 之前執行 `capsule-server-migration up`（[](/guides/self-hosting/#migrate-then-start)）。
- **受信任 Proxy 的設定遭到拒絕**。`TRUSTED_PROXIES` 會拒絕 `/0` 網路，`TRUSTED_PROXY_HEADER` 只接受 `x-forwarded-for` 或 `forwarded`。

正在執行的伺服器會向 stdout 印出 `listening on <url>`。

<a id="clients-cant-sign-in-or-connect"></a>

### 用戶端無法登入或連線

- `capsule server-info` 會顯示伺服器的登入方式：是否設定了身分識別提供者，以及是否拒絕密碼登入。對已停用密碼的伺服器進行密碼登入，依設計就會失敗。
- 在反向 Proxy 之後，所有用戶端都會共用 Proxy 的速率限制，直到在 `TRUSTED_PROXIES` 或 `TRUSTED_PROXY_HOPS` 中指明該 Proxy 為止。當多人同時受到速率限制時，請先檢查這一點（[](/guides/self-hosting/#configure)）。

<a id="the-index-and-the-blob-store-disagree"></a>

### 索引與 blob 儲存區不一致

`capsule-server scrub` 是唯讀的完整性檢查，發現任何問題都會以非零狀態結束。在還原備份或遺失資料庫之後，`capsule-server rebuild` 會將索引與擁有者帳本進行比對。在您停止 `serve` 並執行 `capsule-server rebuild --apply` 之前，它不會做任何變更（[](/guides/self-hosting/#maintenance)）。

<a id="imports-and-uploads"></a>

## 匯入與上傳

- **`push` 中途停止**。再執行一次即可。上傳會從中斷處繼續，因此重新執行一律是安全的。只要有任何資源失敗，`push` 就會以非零狀態結束，`--json` 會列出每個資源的結果（[](/guides/user-guide/#import-and-back-up)）。
- **在保管獲得驗證之前，請保留原始檔**。在 `push` 回報某個資源已驗證之前，請勿刪除其來源檔案；或者使用 `import --move --push`，它只會在每個資源通過驗證後才刪除對應的來源檔案。
- **Google 相簿匯出檔看起來不完整**。請確認 Takeout 匯出的每個部分都已下載並解壓縮，並閱讀匯入工具無法保證的內容（[](/guides/google-photos-migration/#what-the-importer-cannot-guarantee)）。
- **拍攝時間不正確**。`capsule repair capture-time` 會回報所有簽章拍攝時間與原始檔 EXIF 不一致的資源，加上 `--apply` 則會加以更正。每次更正都是一筆永久的簽章紀錄，因此請先在不加 `--apply` 的情況下執行。

<a id="check-a-librarys-integrity"></a>

## 檢查相片庫的完整性

如果相片看起來遺失或損毀，在查明原因期間請**停下來，不要刪除任何東西**：刪除可能會移除您復原資料所需的脈絡。

1. `capsule verify --library <path>` 會向伺服器詢問它是否已持久保存每個原始檔。`--deep` 會讓伺服器重新計算已儲存位元組的雜湊值，而不只是檢查它們是否存在；`--signed` 會將伺服器簽章的判定結果保存在相片庫中。
2. 移到垃圾桶的資源在其保留期限內仍可復原：`capsule trash restore --library <path> --asset <id>` 可以還原一個資源（[](/guides/user-guide/#delete-safely)）。
3. 裝置或金鑰遺失有專屬的處理程序：[](/design/backup-recovery/#recovery-paths)。

如果資料遺失或損毀，請以錯誤回報提出，並附上每個步驟的輸出。無論原因為何，資料遺失或損毀都屬於缺陷。

<a id="still-stuck"></a>

## 仍然無法解決

[](/support/) 列出了可以提問的地方，以及如何提交能夠獲得處理的錯誤回報。
