# Flect-for-Mac — 10 Swift アプリの状況と Mac 実機チェックリスト

更新: 2026-10-20 ／ 対象コミット: `da72ca6`（CI 全ジョブ緑）

## CI で確認できていること（macos-14 / Swift 5.10）
- `swift build` 成功（FlectCore + FlectApp）、`swift test` 11 件成功（イベント解析・行バッファ・起動要求・出力パス・設定・プランナー・翻訳キー網羅・ダミーエンジンの往復/クラッシュ）。
- Rust エンジン/ツールのリリースビルド、Quick Action 35 件の生成と `plutil -lint`。
- CI が最初に見つけた不具合（修正済み）: `\r\n` が 1 文字扱いで入力リストが分割されない／設定ファイルが 0644 になる（要件は 0600）。

## 実装状況
| 範囲 | 状態 |
| --- | --- |
| 起動引数（`--task` / `--input`）、進捗、キャンセル、ごみ箱（`needs:trash`） | 実装・コンパイル確認済み。実機の挙動は未確認 |
| 選択（プリセット、形式）、ダイアログ（ピクセル、目標サイズ、リネーム、PDF 順序、トリム、サムネイル） | 実装・コンパイル確認済み。UI の見た目は未確認 |
| 結果画面（tree / analyze / 重複 / 空フォルダ / リネームプレビュー） | 同上 |
| HEIC（ImageIO）、PDF 画像化・テキスト（PDFKit）、OCR（Vision） | 同上。実機で要検証 |
| 設定画面 | 主要項目のみ（言語、削除方法、閉じる動作、OCR）。メニュー表示切替・プリセット編集・詳細項目は未実装 |
| 字幕（whisper）、AI 背景除去 | 未実装 |
| `scripts/build.sh`（.app 組み立て、ad-hoc 署名、Quick Action 配置） | 作成済み。Mac で未実行 |

## Mac 実機で確認してほしいこと（順番に）
1. `brew install rustup sevenzip ffmpeg` ののち `rustup-init`、`cd core && cargo test`。
2. `scripts/build.sh --install` → `killall Finder` → Finder で画像を右クリック → クイックアクション → Flect の項目が出るか。
3. 画像のリサイズ、動画の変換（VideoToolbox を含む）、フォルダの重複削除（ごみ箱に移動されるか）、PDF の画像化、OCR、HEIC 変換。
4. `swift poc/mac/ocr.swift` / `heic.swift` が通るか。mkv / webm / flv が右クリック対象になるか（UTI）。
5. JAR のデコンパイル（`java -version` と `scripts/fetch-cfr.sh`）。`sandbox-exec` が拒否される場合はエラー（`msg.sandboxFailed`）になり、サンドボックスなしでは実行しない。

## 残作業
設定画面の残り、字幕と背景除去、README / NOTICES / CONTRIBUTING の書き直し、ライセンス・名称・bundle id の決定、アセット（アイコン等）の新規作成。

## 更新（第 1 回の実機確認後）
実機で見つかった 5 件（SDK 選択、PDF 結合、トリミング画面の落ち、エンジンへの書き込み、`menu.to`）と mkv の対象判定は 4e97d4f で修正済み。結果の一覧は [12](12-mac-verification-results.ja.md)。残る課題: 設定画面の初期サイズとラベルの重複（本更新で修正）、日本語入り自作 PDF のプレビューが空白（原因未確定）、JAR の確認。
