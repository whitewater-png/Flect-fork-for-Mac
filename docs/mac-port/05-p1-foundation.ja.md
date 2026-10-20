# Flect-for-Mac — 05 P1 基盤 完了記録

関連: [03 設計計画](03-design-plan.ja.md) §6

## 実施内容
- Windows 資産を `legacy/windows/` へ `git mv`（履歴維持）。旧 workflow も移したため GitHub では無効。
- `core/` に Rust workspace を新設（`flect-core` は `#![forbid(unsafe_code)]`、`flect-engine` は stdin/stdout の JSON Lines サーバ）。
- 旧 `.github/workflows/ci.yml` の代わりに、Rust 用 CI（fmt / clippy / test / cargo-deny）を新設。

## `flect-core` の構成
| モジュール | 内容 | 移植元 |
| --- | --- | --- |
| `menu` | メニュー定義（リーフ **87**）、選択の分類、テキスト変換先、`menu.json` 出力 | `MenuCatalog.cs`, `Selection.cpp`, `MenuBuilder.cpp` |
| `paths` | `(2)` 採番、ダングリング symlink も「使用中」扱い、隠しスクラッチ名 | `Path.cs` |
| `workspace` | 失敗時に消える作業ファイル、**上書きしない commit（hard link）**、スクラッチ dir | `Workspace.cs` |
| `settings` | `settings.json` 互換（PascalCase、BOM 付き読込、旧 Discord 上限の移行）、`0600`、原子的書込、壊れた/巨大ファイルの退避 | `Settings.cs`, `Store.cs` |
| `protocol` | 1 行 1 JSON、1 MiB 上限、不明フィールド拒否、パスは絶対・NUL 不可・4096 byte・2 万件以内 | 新規 |
| `locations` | `~/Library/Application Support/Flect-for-Mac`（`FLECT_HOME` で上書き） | `Locations.cs` |

## 検証結果（Linux）
- `cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` / `cargo test`: **24 テスト全て成功**
- `menu.json` のリーフ ID 87 件は、旧 `MenuCatalog.cs` から grep で抽出した ID と **diff で完全一致**。
- エンジンの実走: `export-menu` は JSON を返し、未実装タスクは `task.unimplemented`、未知は `task.unknown`、相対パス・壊れた JSON は拒否。
- 訂正: 01 文書の「リーフ 67」は誤りで、正しくは **87**（修正済み）。

## 判断と注意
- `HardwareEncoder` に `VideoToolbox` を追加（`Nvenc/Qsv/Amf` は Windows 設定ファイル読込用に残すだけで Mac では提示しない）。
- 画像・動画などの実処理は未実装（P2 以降）。`cargo-deny` は CI で初回実行されるため、ライセンス許可リストは結果を見て調整する可能性あり（ローカルには未導入）。
- Swift 側は未着手。`menu.json` とプロトコルが Swift 側の契約になる。
