# Flect-for-Mac

**Finder で右クリックして、変換・圧縮・リサイズなどができます。** [Flect](https://github.com/Konayukiw/Flect)（Windows 版）を、Rust の処理コアと Swift の画面で macOS 向けに作り直したものです。

> **状況: 初回リリース（0.1.0）。** MacBook（macOS 26・Apple Silicon）で 2 回の実機確認を行いました（[`docs/mac-port/`](docs/mac-port/)）。ビルドは ad-hoc 署名で、**公証していません**。既知の制限は [`CHANGELOG.md`](CHANGELOG.md) とリリースノートを参照してください。

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

字幕（音声から SRT/VTT/TXT）は、whisper フィルタ入りの ffmpeg（Homebrew の `ffmpeg-full` など）と、`scripts/fetch-whisper-model.sh` で入れるモデルを使います。未実装: AI による背景除去。

## 必要なもの

- macOS 14 以降・Apple Silicon
- Xcode Command Line Tools（Swift 5.9 以上）、[Rust](https://rustup.rs)
- `scripts/setup-deps.sh` が、同梱しない外部ツール（ffmpeg-full、7-Zip、任意で Java・CFR・字幕モデル）の有無を調べ、確認のうえ Homebrew で入れます（`--check` は確認のみ）。アプリの設定の「必要なツール」タブにも、足りないものとコピーできるコマンドが出ます。手動なら `brew install ffmpeg-full sevenzip`

## ダウンロード（ad-hoc 署名のビルド）

Releases に `Flect-for-Mac-macos-arm64.zip`（Apple Silicon・macOS 14 以降）と `.sha256` を置きます。**公証はしていない**ため、初回起動で macOS が警告します。

```bash
shasum -a 256 -c Flect-for-Mac-macos-arm64.zip.sha256     # ダウンロードの検証
unzip Flect-for-Mac-macos-arm64.zip && mv Flect-for-Mac.app ~/Applications/
xattr -dr com.apple.quarantine ~/Applications/Flect-for-Mac.app
~/Applications/Flect-for-Mac.app/Contents/Resources/flect-tools --out ~/Library/Services   # Quick Action を配置
killall Finder
```
このあと「クイックアクションをオンにする」（下）を行ってください。バイナリを信用したくない場合は、下の手順で自分でビルドしてください。`brew install ffmpeg-full sevenzip` も必要です。

## ビルドとインストール

```bash
git clone https://github.com/whitewater-png/Flect-fork-for-Mac.git
cd Flect-fork-for-Mac
scripts/build.sh --install     # ビルド → ad-hoc 署名 → ~/Applications と Quick Action を配置
killall Finder
```

## クイックアクションをオンにする（初回のみ）

macOS は新しいクイックアクションを**オフ**にするため、オンにするまで右クリックに出ません:

1. **システム設定 → 一般 → ログイン項目と機能拡張** を開き、下の **拡張機能** の **Finder** の右にある (i) ボタンを押します。
2. **Flect: …** の項目（35 個）をすべてオンにします。ほかの項目はそのままで構いません。
3. それでも Flect の項目が出ないときは、`killall Finder` を実行します。

そのあと、Finder でファイルを右クリック →「クイックアクション」→ Flect。設定はアプリ本体を開いて変更します。

## セキュリティ設計

- Swift アプリは画面と OS 機能を担当し、ファイル処理は別プロセスの Rust エンジン（`unsafe` 禁止）が JSON Lines で受け持ちます。実行ごとに 1 プロセスです。
- 外部ツールはシェルを使わず、固定フォルダから引数配列で起動します。ffmpeg のプロトコルは `file,pipe` のみ許可します。
- アーカイブ展開は、絶対パス・`..`・リンク・解凍爆弾を拒否します。
- 出力は排他的に作成し、既存ファイルを上書きしません。
- JAR のデコンパイルは SHA-256 を固定した CFR を、利用可能なら `sandbox-exec` の中で実行します。
- 詳細は [`SECURITY.md`](SECURITY.md) と [`docs/mac-port/02-requirements.ja.md`](docs/mac-port/02-requirements.ja.md)。

## ライセンス

Apache License 2.0。[`LICENSE`](LICENSE)・[`NOTICE`](NOTICE)・[`NOTICES.md`](NOTICES.md) を参照。原作の著作権は Konayukiw にあります。
