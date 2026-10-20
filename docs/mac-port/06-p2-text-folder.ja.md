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

## 未実装（次弾以降）
- `text.convert`（CSV/JSON/XML/INI/HTML/MD 変換、旧 `TextConvert.cs` 425 行）
- フォルダのリネーム・解析レポート・圧縮、重複/空フォルダの**削除実行**（ごみ箱は Swift 側の依頼イベント `needs{trash}` 経由）
- アーカイブ展開（zip-slip / zip bomb 対策付き）、PDF 結合
- キャンセル（エンジン側は `Progress::cancelled` の受け口あり。stdin 読み取りと並行に処理するスレッド分離は未実装）
