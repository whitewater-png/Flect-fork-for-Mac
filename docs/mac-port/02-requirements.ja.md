# Flect macOS 版 — 02 要件定義
関連: [01 可否チェック](01-feasibility-check.ja.md) / [02 要件定義](02-requirements.ja.md) / [03 設計計画](03-design-plan.ja.md)

## 4. 要件定義

### 4.1 機能要件
| ID | 要件 | 優先 |
| --- | --- | --- |
| FR-1 | Finder で単一/複数のファイル・フォルダを右クリックして Flect を実行できる | Must |
| FR-2 | 選択内容（画像/動画/音声/PDF/テキスト/JAR/アーカイブ/フォルダ）に応じて項目を出し分ける | Must |
| FR-3 | 選択項目を処理し、進捗ウィンドウ（キャンセル可）を表示。複数選択は一括処理 | Must |
| FR-4 | 元ファイルは変更せず隣に新規出力。衝突は `(2)`。失敗/キャンセル時は生成物を掃除。出力は完成まで隠し一時ファイル | Must |
| FR-5 | 画像: リサイズ / 圧縮(目標サイズ) / 回転(任意角) / 変換(PNG,JPG,WEBP,HEIC,ICO,GIF,SVG) / OCR / メタデータ削除 / 背景除去 | Must |
| FR-6 | 動画・音声: リサイズ / トリム / サムネイル / 字幕生成 / 圧縮(目標サイズ・Discord) / 回転 / 変換 / 音声抽出 | Must |
| FR-7 | フォルダ: 重複削除 / 空フォルダ削除 / ツリー / リネーム / 解析 / 圧縮(ZIP,7z,TAR,tar.gz) | Must |
| FR-8 | アーカイブ展開(ZIP,7z,TAR,tar.gz,RAR) | Must |
| FR-9 | PDF: プレビュー / 結合 / PNG,JPG,TXT 変換 | Must |
| FR-10 | テキスト: 形式変換(CSV/JSON/XML/INI 等)・文字コード・改行、JSON 整形/キーソート | Must |
| FR-11 | JAR デコンパイル(CFR + 利用者導入の Java) | Must |
| FR-12 | 設定: テーマ/言語(EN・JA・ZH)/メニュー表示切替/プリセット/各タスク設定 | Must |
| FR-13 | 削除は既定でごみ箱。完全削除は設定で選択 | Must |
| FR-14 | 更新確認（通知のみ。自動置換はしない） | Could |

### 4.2 非機能要件
| ID | 要件 |
| --- | --- |
| NFR-1 | macOS 14 以降 / Apple Silicon (arm64) のみ（Intel は対象外案。要確認） |
| NFR-2 | 完全ローカル処理。通信は「依存/モデル取得」「更新確認」のみで、**明示操作または初回のみ**。テレメトリ無し |
| NFR-3 | VideoToolbox を既定で活用。**速度目標は P0 で C# 版と実測比較して設定**（推定値は置かない） |
| NFR-4 | 日本語・NFD ファイル名、スペース、先頭 `-`、改行を含むパス、シンボリックリンクで壊れない |
| NFR-5 | 署名: ad-hoc（Developer Program 不要）。**ローカルビルドして使う運用**を正とする |
| NFR-6 | Apache-2.0 遵守（変更の明記、`NOTICES.md` 更新、ffmpeg ビルドの GPL/LGPL を確認） |
| NFR-7 | 設定スキーマは現行 `settings.json` と可能な範囲で互換（インポート可能にする） |

### 4.3 セキュリティ要件（安全側の前提条件）
| ID | 要件 |
| --- | --- |
| SEC-1 | 処理コアは **Rust（`#![forbid(unsafe_code)]`、unsafe が必要な箇所は専用 crate に隔離しレビュー対象化）** |
| SEC-2 | **プロセス分離**: UI(Swift) ⇄ エンジン(Rust) ⇄ 外部ツール(ffmpeg/7zz/java/python) を別プロセスにし、通信は stdio の JSON Lines（スキーマ検証、サイズ上限あり）。シェルは介さず **argv 配列のみ**で起動 |
| SEC-3 | **アーカイブ展開**: zip-slip/絶対パス/`..`/シンボリックリンク脱出/ハードリンクを拒否、展開サイズ・ファイル数・圧縮率の上限（zip bomb 対策）。7zz 使用時は一時ディレクトリに展開→検証→移動 |
| SEC-4 | **ffmpeg 強化**: `-protocol_whitelist file,pipe`（HLS/concat 経由のローカルファイル読取・SSRF 防止）、`-nostdin`、入力は常に `file:` 絶対パス、ネットワーク入出力禁止。CPU/メモリ/時間の上限 |
| SEC-5 | **ImageMagick は採用しない**（コーダ数が多く攻撃面が広い）。画像は Rust 製デコーダ + OS の ImageIO(HEIC) に限定 |
| SEC-6 | **サプライチェーン**: ダウンロードする ffmpeg/7zz/cfr/モデルは**バージョン固定 + SHA-256 検証**（`fetch.sh`）。pip は `--require-hashes` のロックファイル。Cargo は `Cargo.lock` 固定 + `cargo-deny`/`cargo-audit` を CI に |
| SEC-7 | **Python は隔離**: 専用 venv（`~/Library/Application Support/Flect/env`）、初回のみ通信、実行時はネットワーク不要。AI 機能は設定で無効化可能 |
| SEC-8 | **IPC 入力検証**: Finder→アプリの受け渡しは UTF-8 パスリストのみ。存在確認・正規化・上限数・自プロセスのユーザー所有のみ許可。一時ファイルは `0600`、ディレクトリ `0700`（`$TMPDIR` 配下） |
| SEC-9 | **ハードニング**: Hardened Runtime 有効、不要な entitlement 無し、`settings.json` は `0600`、パス正規化後に書込先を再検証（TOCTOU 低減）、出力は `O_EXCL` 作成 |
| SEC-10 | **更新**: 更新確認は HTTPS + 公開鍵による署名検証（minisign/ed25519）を実装するまで**通知のみ**（自動ダウンロード実行はしない） |
| SEC-11 | **ファジング/テスト**: パス導出・アーカイブ・テキスト変換・プロトコルパーサに `cargo-fuzz` と悪意あるフィクスチャ（zip-slip、巨大画像、壊れた PDF/動画）を用意 |

### 4.4 スコープ外
Mac App Store 配布 ／ Windows/Linux 版 ／ Intel Mac（要確認）／ Quick Look・Spotlight 連携 ／ Developer ID 署名・notarize（未加入のため。加入した場合の差分は §8 に記載）
