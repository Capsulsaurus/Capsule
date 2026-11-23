---
title: トラブルシューティング
description: 報告に必要なバージョンとログを集め、Capsuleのサーバー、取り込み、ライブラリでよくある問題を解決します。
status: draft
---

問題が起きたら、まずここから始めてください。以下のどれでも解決しない場合は、[](/support/)に質問先が書かれています。また、最初のセクションの診断情報はバグ報告に必要なものです。

<a id="collect-diagnostics"></a>

## 診断情報を集める

<a id="version"></a>

### バージョン

まだリリースが公開されていないため、バージョン番号ではビルドを区別できません。すべてソースからビルドされるので、ビルドに使ったコミットを報告してください。チェックアウトしたディレクトリで`git rev-parse --short HEAD`を実行します。CLIにはバージョン表示のフラグがなく、`capsule-server --version`はクレートのバージョンしか表示しませんが、これはどのビルドでも同じです。

<a id="logs"></a>

### ログ

CLIとサーバーは、ログを**stderr**に書き出します。stdoutにはコマンドが出力する内容だけが流れ、`--json`の場合は単一のJSONドキュメントになるため、それぞれを別々に保存できます。

```sh
RUST_LOG=debug capsule push --library ~/Photos.capsule 2> capsule.log
```

- `RUST_LOG`はログの量を設定します。リリースビルドはデフォルトで`info`、デバッグビルドは`debug`でログを出力します。問題を再現するときは`RUST_LOG=debug`を使い、`RUST_LOG=trace`は求められた場合にのみ使ってください。出力が非常に大きくなります。
- サーバーのログ形式は`LOG_FORMAT`で設定します。デバッグビルドでは`pretty`、リリースビルドでは`json`がデフォルトです（[](/development/local-development/#logs)）。
- フルスクリーンのTUI（ターミナルで引数なしの`capsule`を実行したもの）は、自身が描画している画面にログを出せないため、CLIのキャッシュディレクトリに`tui.log`を書き出します。このディレクトリは`capsule status`がCache Directoryとして表示します。このファイルはセッションごとに上書きされるので、TUIを再び起動する前にコピーしてください。
- `--json`の出力は、アセットごとの結果を報告するため、コマンドが何をしたかを最も正確に記録したものです。コマンドが失敗を報告したときは添付してください。

**共有する前に機密情報を伏せてください**。パスフレーズ、リカバリーシークレット、トークン、鍵、サーバーのアドレス、個人的なファイルパスを削除してください。ライブラリのパスフレーズやリカバリーシークレットは、非公開の報告も含め、どこにも貼り付けないでください。

<a id="the-server"></a>

## サーバー

<a id="it-refuses-to-start"></a>

### 起動しない

- **終了コード2で、変数名が表示される**。永続化されたサーバーには`DATABASE_URL`、`BLOB_ROOT`、`JWT_ED25519_DER`、`ATTESTATION_KEY_SEED`が必要で、足りないものをすべて一度に表示します。[](/guides/self-hosting/#configure)を参照してください。
- **データベースのスキーマがバイナリより古い**。`serve`がマイグレーションを行うことはありません。アップグレードのたびに、`serve`の前に`capsule-server-migration up`を実行してください（[](/guides/self-hosting/#migrate-then-start)）。
- **信頼済みプロキシの設定が拒否される**。`TRUSTED_PROXIES`は`/0`のネットワークを拒否し、`TRUSTED_PROXY_HEADER`は`x-forwarded-for`または`forwarded`しか受け付けません。

稼働中のサーバーは、stdoutに`listening on <url>`を出力します。

<a id="clients-cant-sign-in-or-connect"></a>

### クライアントがサインインまたは接続できない

- `capsule server-info`は、サーバーのサインイン方法（IDプロバイダーがあるかどうか、パスワードを拒否するかどうか）を表示します。パスワードが無効になっているサーバーへのパスワードでのサインインは、設計上失敗します。
- リバースプロキシの背後では、そのプロキシが`TRUSTED_PROXIES`または`TRUSTED_PROXY_HOPS`で指定されるまで、すべてのクライアントがプロキシのレート制限を共有します。複数の人が同時にレート制限を受けているときは、まずここを確認してください（[](/guides/self-hosting/#configure)）。

<a id="the-index-and-the-blob-store-disagree"></a>

### インデックスとblobストアが一致しない

`capsule-server scrub`は読み取り専用の整合性チェックで、何か見つかると0以外の終了コードで終了します。バックアップを復元した後やデータベースを失った後は、`capsule-server rebuild`がインデックスを所有者台帳と比較します。`serve`を停止して`capsule-server rebuild --apply`を実行するまで、何も変更しません（[](/guides/self-hosting/#maintenance)）。

<a id="imports-and-uploads"></a>

## 取り込みとアップロード

- **`push`が途中で止まった**。もう一度実行してください。アップロードは中断したところから再開されるため、再実行は常に安全です。いずれかのアセットが失敗すると`push`は0以外の終了コードで終了し、`--json`は各アセットの結果を一覧表示します（[](/guides/user-guide/#import-and-back-up)）。
- **保管が検証されるまで、オリジナルを手元に残してください**。`push`がアセットを検証済みと報告するまでは、ソースファイルを削除しないでください。または、各アセットが検証された後にのみソースを削除する`import --move --push`を使ってください。
- **Googleフォトのエクスポートが不完全に見える**。Takeoutのエクスポートのすべてのパートがダウンロードされ、展開されていることを確認し、インポーターが保証できないことについて読んでください（[](/guides/google-photos-migration/#what-the-importer-cannot-guarantee)）。
- **撮影日時が間違っている**。`capsule repair capture-time`は、署名された撮影日時がオリジナルのEXIFと一致しないアセットをすべて報告し、`--apply`を付けると修正します。修正はそれぞれ恒久的な署名付きの記録になるので、まずは`--apply`なしで実行してください。

<a id="check-a-librarys-integrity"></a>

## ライブラリの整合性を確認する

写真が欠けている、または壊れているように見える場合は、原因を調べている間は**手を止め、何も削除しないでください**。削除すると、復旧に必要な手がかりが失われることがあります。

1. `capsule verify --library <path>`は、各オリジナルを永続的に保持しているかどうかをサーバーに問い合わせます。`--deep`は、保存されたバイトが存在するかを確認するだけでなく、サーバーにハッシュを計算し直させます。`--signed`は、サーバーが署名した判定結果をライブラリに保存します。
2. ゴミ箱に入れたアセットは、保持期間中は復元できます。`capsule trash restore --library <path> --asset <id>`で1つずつ元に戻せます（[](/guides/user-guide/#delete-safely)）。
3. デバイスや鍵を失った場合には、専用の手順があります：[](/design/backup-recovery/#recovery-paths)。

データが失われたり破損したりした場合は、各手順の出力を添えてバグとして報告してください。原因が何であれ、データの喪失や破損は不具合です。

<a id="still-stuck"></a>

## それでも解決しない場合

[](/support/)に、質問できる場所と、対応可能なバグ報告の書き方がまとめてあります。
