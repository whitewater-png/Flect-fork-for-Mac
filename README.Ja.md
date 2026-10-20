# Flect-for-Mac

**Finder で右クリックして、変換・圧縮・リサイズなどができます。** [Flect](https://github.com/Konayukiw/Flect)（Windows 版）を、Rust の処理コアと Swift の画面で macOS 向けに作り直したものです。

> **状況: プレリリース。** macOS 14（Apple Silicon）の CI でビルドとテストは通っていますが、Finder との連携や OS 固有の機能は実機での確認がまだです。確認手順は [`docs/mac-port/11-mac-verification-guide.ja.md`](docs/mac-port/11-mac-verification-guide.ja.md)。署名・公証済みの配布物はなく、自分でビルドして使います。

English: [README.md](README.md)

## できること

元のファイルは変更しません。結果は元ファイルの隣に新しいファイルとして作られます（同名があれば `名前 (2).拡張子`）。削除はごみ箱へ移動します。

| 分野 | 内容 |
| --- | --- |
| 画像 | リサイズ（％・ピクセル）、目標サイズへ圧縮、回転、形式変換（PNG/JPG/WEBP/HEIC/ICO/GIF/SVG）、文字認識（OCR）、メタデータ削除、背景色の透過 |
| 動画・音声 | リサイズ、トリミング、サムネイル、圧縮（Discord 向け含む）、回転、変換、音声の取り出し（VideoToolbox 利用可） |
| PDF | プレビュー、結合、PNG/JPG/テキストへ変換 |
| テキスト・JSON | 形式変換、文字コード・改行の変換、整形、キーの並べ替え |
| フォルダ | 重複ファイル検出（バイト比較で確認）、空フォルダ削除、ツリー、分析、一括リネーム、圧縮 |
| アーカイブ・コード | 安全な展開（ZIP/7z/tar など）、JAR のデコンパイル（CFR） |

未実装: 音声からの字幕生成、AI による背景除去。

## 必要なもの

- macOS 14 以降・Apple Silicon
- Xcode Command Line Tools（Swift 5.9 以上）、[Rust](https://rustup.rs)
- `brew install ffmpeg sevenzip`（動画・音声と 7z 用。JAR を使うなら `openjdk` も）

## ビルドとインストール

```bash
git clone https://github.com/whitewater-png/Flect-fork-for-Mac.git
cd Flect-fork-for-Mac
scripts/build.sh --install     # ビルド → ad-hoc 署名 → ~/Applications と Quick Action を配置
killall Finder
```

Finder でファイルを右クリック →「クイックアクション」→ Flect。設定はアプリ本体を開いて変更します。

## セキュリティ設計

- Swift アプリは画面と OS 機能を担当し、ファイル処理は別プロセスの Rust エンジン（`unsafe` 禁止）が JSON Lines で受け持ちます。実行ごとに 1 プロセスです。
- 外部ツールはシェルを使わず、固定フォルダから引数配列で起動します。ffmpeg のプロトコルは `file,pipe` のみ許可します。
- アーカイブ展開は、絶対パス・`..`・リンク・解凍爆弾を拒否します。
- 出力は排他的に作成し、既存ファイルを上書きしません。
- JAR のデコンパイルは SHA-256 を固定した CFR を、利用可能なら `sandbox-exec` の中で実行します。
- 詳細は [`SECURITY.md`](SECURITY.md) と [`docs/mac-port/02-requirements.ja.md`](docs/mac-port/02-requirements.ja.md)。

## ライセンス

Apache License 2.0。[`LICENSE`](LICENSE)・[`NOTICE`](NOTICE)・[`NOTICES.md`](NOTICES.md) を参照。原作の著作権は Konayukiw にあります。
