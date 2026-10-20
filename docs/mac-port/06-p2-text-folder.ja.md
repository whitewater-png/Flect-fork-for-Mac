# Flect-for-Mac — 06 P2 第1弾: テキスト・フォルダ（Rust）

関連: [03 設計計画](03-design-plan.ja.md) §6、[05 P1 基盤](05-p1-foundation.ja.md)

## 実装（`core/flect-core`）
| モジュール | 内容 | 移植元 |
| --- | --- | --- |
| `text` | 文字コード判定（BOM → 厳密 UTF-8 → `chardetng`）、UTF-8/BOM/UTF-16/Shift_JIS 出力、SJIS 不可文字の方針（Fail / Substitute / ReplaceAndWarn）、改行変換、JSON 整形・キーソート（コメント・末尾カンマ許容、キー順・巨大数値・日本語を保持） | `TextFile`, `Tasks/Text/*`, `SortJson.cs` |
| `folder` | 除外ルール（ワイルドカード、大小無視）、走査（**symlink は辿らない**）、空フォルダ（macOS の `.DS_Store` / `._*` / `.localized` を無視対象に）、重複検出（サイズ→先頭 64KiB→全体ハッシュ→**バイト比較で確定**）、残す方針（古い/新しい/全削除）、ツリー出力 | `Scanner.cs`, `Duplicate.cs`, `Tree.cs`, `Exclusion.cs` |
| `tasks` | `text.encode` / `text.lineEndings` / `json.pretty` / `json.sort` を実ファイルに適用。元ファイルは不変、`(2)` 採番、**上書きしない commit**、失敗時は残骸なし | `BatchTask` 群 |
| `flect-engine` | 上記 4 タスクを `run` で実行（1 ファイルずつ、失敗しても継続、status / step / warn / error / done を送出） | — |

## 安全面の判断
- **重複判定はハッシュだけで確定しない**: xxh3 は暗号学的ハッシュではなく、細工されたファイルで衝突させればユーザーのファイルを消させられるため、同一グループ内をバイト比較で確認する（旧 Windows 版にはない追加）。テストで「サイズも先頭 64KiB も同じだが末尾 1 byte だけ違う」ファイルが重複扱いにならないことを確認。
- 走査は symlink を辿らない（ループ・選択範囲外への脱出を防ぐ）。テストで自己ループ symlink を使用。
- SJIS への変換は `encoding_rs` 既定の「`&#…;` への置換」を使わず、不可文字を自前で処理（`&#` が出力に混ざらないことをテスト）。
- テキスト系の入力は 256 MiB まで（全体をメモリに載せるため）。

## 検証結果（Linux）
- `cargo fmt --check` / `clippy -D warnings` / `cargo test`: **49 テスト全て成功**（P1 の 24 + 本弾 25）
- エンジンの実走: `json.sort`（BOM・末尾カンマ付き JSON）、`text.encode?to=SJIS`（絵文字を含み警告、存在しないファイルは `msg.ioError` で継続）、`text.lineEndings?to=CRLF` を実ファイルで確認。
- 簡易ベンチ（`cargo run --release --example bench_dup`、4 コアの Linux、合成データ）: **1 万ファイル / 約 1.4 GiB で 走査 19 ms + 重複検出 179 ms**（2,000 グループ、ページキャッシュ上）。ディスクが冷えた状態や Mac での値ではないため、**絶対値目標の根拠にはまだ使えない**（Mac 実機で再測定する）。

## 第2弾の追加（同日）
| モジュール | 内容 | 安全面 |
| --- | --- | --- |
| `convert` | CSV/JSON/XML/INI → JSON/XML/CSV/HTML/MD（区切り文字の自動判定、引用符、表出力。旧 `TextConvert.cs`） | XML は DTD・外部実体・実体展開を**行わない**（未知の実体は構文エラー）、ネスト 128 超は拒否。XML 出力は使えない制御文字を除去 |
| `archive` | ZIP/TAR/tar.gz の展開・作成、7z/RAR は `7zz` 経由（旧 Archive.cs/Folder.cs） | 名前の検査（`..`、絶対パス、`\`、ドライブ文字、NUL を拒否）、**リンク/デバイスは作らない**（警告）、サイズは**実際に書いた量**で計上（ヘッダを信用しない）、総量・件数・圧縮率の上限、権限の正規化（setuid 除去）、重複名は上書きせず警告、隠しスクラッチで展開→成功時のみ可視化。`7zz` は固定ディレクトリのみ探索・他者書込可なら拒否・**展開前に一覧でサイズ/件数を検査**・展開後に木を監査（リンク削除） |
| `tasks` | `text.convert`、`archive.extract`、`folder.compress` を追加。エンジンから実行可能 | — |
| `flect-tools` | `make-workflows`: Quick Action（35 個）生成器。種別ごとの UTI、`inputMethod=1`、シェル注入を防ぐトークン検査 | 生成物の plist をテストで構文検証 |

テスト合計: **81 件成功**（core 74 + engine 2 + tools 5）。`7zz` はこの環境に無いため、偽の `7zz` スクリプトで流れ（一覧→展開→監査）を検証しており、**実物の 7zz での確認は Mac で必要**。

## 未実装（次弾以降）
- フォルダのリネーム・解析レポート・圧縮、重複/空フォルダの**削除実行**（ごみ箱は Swift 側の依頼イベント `needs{trash}` 経由）
- PDF 結合・変換、画像、動画・音声
- キャンセル（エンジン側は `Progress::cancelled` の受け口あり。stdin 読み取りと並行に処理するスレッド分離は未実装）


## 第3〜5弾（同日）: 画像・フォルダ残り・動画音声・PDF・JAR・キャンセル
詳細な契約は [08 エンジンプロトコル](08-engine-protocol.ja.md)、項目別の状況は [09 パリティ表](09-parity.ja.md)。

| モジュール | 内容 | 安全面の要点 |
| --- | --- | --- |
| `imaging` | リサイズ（アルファ事前乗算の Lanczos、SIMD）、任意角回転、変換（PNG/JPG/WebP/ICO/GIF/SVG）、目標サイズ圧縮（品質二分探索→パレット PNG→WebP→縮小）、色キー、アニメ GIF | 画素数 1.5 億・入力 512 MiB の上限、ヘッダ偽装の巨大画像を拒否、SVG は外部ファイル参照を解決しない（テストで確認） |
| `metadata` | JPEG/PNG/WebP のメタデータをコンテナ単位で**無劣化**削除。GPS はポインタだけでなく値とサムネイルのバイトを**ゼロ埋め** | 壊れた入力で panic しない（全長の切り詰めテスト） |
| `foldertasks` + `batch` | リネーム（2 相適用・上書きしない・失敗時ロールバック）、解析レポート（CSV/JSON/HTML/TXT）、重複/空フォルダの削除計画 | 削除はごみ箱依頼（`needs:trash`）。Permanent 設定でも選択フォルダの外は拒否。HTML レポートはファイル名をエスケープ |
| `media` + `mediatasks` | ffmpeg/ffprobe を固定ディレクトリから探索、`-protocol_whitelist file,pipe` `-nostdin` `-n`、絶対 `file:` 入力、コンテナ許可リスト（HLS 等を拒否）。リサイズ/回転/変換/トリム/サムネ/音声抽出/GIF/目標サイズ圧縮 | HLS プレイリスト経由のローカルファイル読み取りを実 ffmpeg で拒否確認。VideoToolbox は試験エンコード成功後のみ使用 |
| `pdf` | 結合（lopdf）。継承属性を各ページへ展開してから付け替え | 暗号化・壊れた PDF・ページツリーのループで停止/panic しない |
| `jar` | CFR 実行。**SHA-256 固定の jar のみ**使用、`sandbox-exec`（ネットワーク禁止・書込は出力先のみ）に包む | sandbox は Mac 未検証。プロファイル自体が拒否された場合のみ 1 回だけ無しで再試行し警告を出す |
| `flect-engine` | 読み取りスレッド分離で処理中の `cancel` が効く（150 ファイルのバッチが数百 ms で停止）。単一のディスパッチ | stdio の e2e テスト |

テスト合計: **155 件成功**（core 145 + engine 2+3 + tools 5。JAR の実 CFR テストは `FLECT_TEST_CFR_JAR` を設定した場合のみ実行、ffmpeg の E2E は `/usr/bin/ffmpeg` がある場合のみ実行）。

### 旧版との意図的な差分
- 画像の出力に元の EXIF/ICC は引き継がない（再エンコードのため）。向きは画素へ反映。
- 重複判定は最後にバイト比較で確定（ハッシュ衝突対策）。
- PDF の画像化/テキスト抽出/プレビューは PDFKit（Swift）に任せる設計に変更（PDFium の同梱を避けるため）。
