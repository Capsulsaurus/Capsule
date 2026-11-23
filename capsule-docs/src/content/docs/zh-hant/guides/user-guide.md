---
title: 使用者指南
description: 使用 capsule CLI 日常操作 Capsule 相片庫：匯入、備份、驗證、整理以及安全地刪除。
status: draft
---

本指南介紹參考用戶端 `capsule` CLI。標有（*規劃中*）的指令已列入[路線圖](https://github.com/Capsulsaurus/Capsule/blob/master/ROADMAP.md)，但目前尚無法使用。旗標以 `capsule <command> --help` 為準；CLI 參考（[](/reference/cli/)）列出了所有旗標。

:::caution
Capsule 目前處於 Alpha 階段。在第一個內部試用（dogfood）版本發布之前，請為您匯入的每個原始檔另外保留一份獨立備份。
:::

<a id="set-up"></a>

## 設定

1. 建立相片庫：`capsule library init --name "Photos"`。相片庫是一個本機的加密目錄；它的密碼短語保護這部電腦上的金鑰。
2. 登入您的伺服器：`capsule auth register` 或 `capsule auth login`（單一登入請使用 `--oidc` 或 `--device`）。
3. 設定復原：`capsule auth escrow`。請將印出的復原憑證離線保存。如果沒有它與您的密碼短語，遺失一部電腦就代表遺失整個相片庫，因為伺服器無法解密任何內容。

<a id="import-and-back-up"></a>

## 匯入與備份

- `capsule import <path>` 會將檔案複製到相片庫中，為每個資源的來源紀錄與中繼資料簽章，並產生縮圖。`--provider takeout` 會匯入 Google Takeout 匯出檔及其附屬中繼資料（[](/guides/google-photos-migration/)）。
- `capsule push` 會上傳相片庫。上傳會從中斷處繼續，因此重新執行 `push` 一律是安全的。每上傳完一個資源，`push` 都會要求伺服器確認它已持久保存每個 blob，並將伺服器簽發的保管收據保存在相片庫中。某個資源失敗時會被回報，執行仍會繼續。只要有任何資源失敗，指令就會以非零狀態結束，`--json` 會列出每個資源的結果。
- `capsule import --push` 可一次完成以上兩項操作。
- **在保管獲得驗證之前，請保留原始檔**。只有當伺服器回報某個資源已持久保存，並簽發了經過簽章的保管收據（`push` 會將其回報為已驗證）之後，該資源才算安全儲存。在此之前請勿刪除來源檔案。`import --move --push` 只會在每個資源通過驗證後才刪除對應的來源檔案。不加 `--push` 時，`--move` 會保留所有來源檔案並加以說明；之後對同一批檔案執行 `import --move --push`，即可將其釋放。
- `capsule restore`（*規劃中*）會下載原始檔，並逐位元組核對它們與匯入時簽章的內容是否一致。

<a id="organize"></a>

## 整理

- **標籤**可以自由填寫，可附加到資源、堆疊（RAW+JPEG 配對、連拍、包圍曝光）、相簿或影像中的某個區域。您加入的標籤與 AI 建議的標籤分開保存，您可以採用或略過某項建議。資源標籤目前已可使用；堆疊、相簿與區域標籤則是（*規劃中*）功能。
- `capsule show <asset>` 會印出資源的簽章中繼資料所記錄的內容：說明文字、評分、標籤、位置、拍攝時間。
- `capsule repair capture-time` 會找出拍攝時間與原始檔 EXIF 不一致的資源，加上 `--apply` 時會加以更正。位置編輯功能（*規劃中*）。

每次編輯都是一次經過簽章、只能附加的變更：先前的值會保留在資源的來源歷史中，來自兩部裝置的並行編輯會以確定性的方式合併。

<a id="delete-safely"></a>

## 安全地刪除

請在刪除之前先修正地理位置等中繼資料，趁周圍的資源仍能提供脈絡。一張本身沒有位置資訊的相片，在同一趟散步中拍攝的相鄰相片還在相片庫裡時，很容易判斷拍攝地點；一旦這些相片被刪除，這些脈絡也會隨之消失。

- `capsule cull` 會將資源標記為留用、未決定或排除，並可依標記篩選。
- `capsule cull --sweep` 會將已排除的資源移到垃圾桶。移到垃圾桶的資源在保留期限（`--retain-days`）內仍可復原，之後才會被永久清除。
- `capsule trash delete --asset <id>` 會將資源同時移到伺服器上與相片庫中的垃圾桶。已在垃圾桶中的資源（例如執行 `capsule cull --sweep` 之後）會保留其移入時的保留期限：`--retain-days` 只適用於新的刪除操作，`--json` 會以 `retention_until` 回報所送出的保留期限。
- `capsule trash restore --asset <id>` 會還原一個資源。保留期限結束後的還原要求會被拒絕（在 `--json` 下為 `retention_elapsed`），且不會重試；該資源會留在垃圾桶中，直到被清除為止。

<a id="share"></a>

## 分享

- `capsule share create --album <id>` 或 `--asset <id>` 會產生一個僅供檢視的連結；`--protect` 會為連結加上密碼短語，`--expires-at` 則設定到期時間。連結的金鑰位於 URL 片段中，永遠不會傳送到伺服器。
- `capsule share revoke <id>` 會讓連結無法再開啟。

<a id="sync-another-machine"></a>

## 同步另一部電腦

- `capsule sync` 會從伺服器拉取相片庫的變更動態；`capsule list` 會顯示它同步下來的內容。
- `capsule auth recover` 會使用復原憑證，在新電腦上復原帳號。
