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

## 残り（あなたの Mac での実行が必要）
| § | 項目 | 手順 |
| --- | --- | --- |
| 1 | Quick Action 起動・出し分け・複数選択 | `poc/mac/quick-action.md` |
| 2 | Finder Sync の可否 | 別途 PoC（Xcode が必要）。まず 1 の結果次第 |
| 3 | Vision OCR | `swift poc/mac/ocr.swift 画像 ja-JP,zh-Hans,en-US` |
| 4 | HEIC 書込 | `swift poc/mac/heic.swift in.png out.heic 0.8` |
| 6 | ffmpeg/VideoToolbox | `poc/mac/check-env.sh` |
| 8 | Whisper 系の速度比較 | P6 で実施 |

結果（出力のコピペで可）を貼ってくれれば、この文書に反映するわ。
