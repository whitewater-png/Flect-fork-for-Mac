# Flect-for-Mac — 04 P0 結果（進行中）

更新: 2026-10-20 ／ 関連: [03 設計計画](03-design-plan.ja.md) §9、`poc/`

## 確定した決定事項
- アプリ名 `Flect-for-Mac`／最低 macOS 14／Apple Silicon (arm64) 専用
- バンドルID は未指定のため仮 `com.whitewater.flect-for-mac`（要確認）

## §9-5 Rust 画像スタックの実現性 — **○（Linux x86_64 で実行確認）**
`poc/rust-imaging`（`cargo run --release`、合成 2000×1500 RGBA、1回計測）

| 機能 | 使用 crate | 結果 | 時間 | 備考 |
| --- | --- | --- | --- | --- |
| PNG 書込（アルファ） | image | OK | 23 ms | |
| JPEG 書込 q80 | image | OK | 69 ms | |
| WebP lossless | image | OK | 34 ms | |
| WebP **lossy** | webp（libwebp, C） | OK | 236 ms | image crate 単体は lossless のみ → libwebp 必須（C 依存は限定的に許容） |
| WebP デコード | webp | OK | 21 ms | |
| ICO 256×256 | image | OK | 37 ms | 複数サイズ同梱は未検証（実装時に確認） |
| GIF 書込（静止） | image | OK | 61 ms | アニメ GIF の読書きは未検証 |
| SVG→ラスタ | resvg | OK | 0.8 ms | SVG **書込**は現行どおり PNG 埋め込みで実装可 |
| リサイズ 2000→1000 | fast_image_resize | OK | 15.6 ms | image crate の Lanczos3（61.5 ms）の約 4 倍速 |
| 任意角回転（角は透明） | imageproc | OK | 127 ms | 角の alpha=0 を確認 |

読み取れること: 画像の主要機能は Rust で賄える。リサイズは SIMD 版で約 4 倍（この1回計測・x86 Linux での値。**M4 Max での値ではない**ので傾向としてのみ扱う）。HEIC は ImageIO（Swift）、lossy WebP は libwebp が必要。

## §9-7 C# vs Rust ベンチ — **実施不可（代替方針）**
この環境に dotnet が無く、Windows 版は Mac で動かせないため同条件比較ができない。速度目標は相対比ではなく**絶対値**で置く案: 例「1 万ファイルのフォルダ走査+重複検出が X 秒以内」「4000×3000 JPEG のリサイズが Y ms 以内」。X, Y は P1 完了後に Mac 実機で測って確定する。

## 実機結果（2026-10-20、ユーザーの Mac: macOS 26.6.2 / arm64 / M4 Max / Xcode あり）
`poc/mac/check-env.sh` の出力より（ユーザー提供ログから転記。こちらで再実行はしていない）。

| § | 項目 | 結果 | 備考 |
| --- | --- | --- | --- |
| 6 | ffmpeg + VideoToolbox | **○** | ffmpeg 9.0.2（Homebrew `ffmpeg-full`、GPL ビルド）。`h264_videotoolbox` / `hevc_videotoolbox` とも 320x240 の実エンコード成功。`prores_videotoolbox` もあり |
| 6 | ffmpeg の追加機能 | 発見 | `--enable-whisper`（ffmpeg の whisper フィルタ）、`libwebp`、`libtesseract` 入り。字幕生成を Python なしにできる**可能性**（モデル入手と実速度は未検証） |
| 2 | Finder Sync | △ | 他社製が 9 件登録され 8 件が有効 → 機構自体は使える。**ad-hoc 署名の自作拡張が読み込まれるかは未検証** |
| 1 | Quick Action が右クリックに出るか | **×（原因未特定）** | 手作りの `.workflow` では出なかった。保存場所・受け取り設定・システム設定側の有効化・macOS 26 の仕様のどれかが原因候補。`poc/mac/diagnose-quickaction.sh` と `make-quickaction.sh` で切り分ける |
| 3, 4 | Vision OCR / HEIC 書込 | 未実行 | 「permission denied」は、スクリプトを直接実行したため（実行権限なし）。権限と shebang を付与済みなので `swift poc/mac/ocr.swift …` または `poc/mac/ocr.swift …` で実行可 |
| — | 開発ツール | 注意 | `cargo`/`rustc`/`7zz` は未導入（`brew install rustup sevenzip` が必要）。`/usr/bin/java` は macOS のスタブの可能性があり、`java -version` で実体を確認 |

## 残り（あなたの Mac での実行が必要）
| § | 項目 | 手順 |
| --- | --- | --- |
| 1 | Quick Action の切り分け | `bash poc/mac/diagnose-quickaction.sh` の出力を貼る → `bash poc/mac/make-quickaction.sh` → `killall Finder` → 右クリックで確認 |
| 3 | Vision OCR | `swift poc/mac/ocr.swift ~/Desktop/test.png ja-JP,zh-Hans,en-US` |
| 4 | HEIC 書込 | `swift poc/mac/heic.swift in.png out.heic 0.8` |
| 8 | Whisper 系の速度比較 | P6 で実施（ffmpeg whisper フィルタも候補に追加） |
