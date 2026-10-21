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

Releases に `Flect-for-Mac-macos-arm64.dmg`（おすすめ）と `Flect-for-Mac-macos-arm64.zip`（Apple Silicon・macOS 14 以降）を置きます。どちらにも `.sha256` が付きます。**公証はしていない**ため、初回起動で macOS が警告します。下の手順は、ディスクイメージの中にも図で入っています。

**ディスクイメージから（いちばん簡単）**

1. `.dmg` を開き、**Flect-for-Mac** を **Applications** へドラッグします。
2. Flect-for-Mac を開きます。「開発元を確認できません」などと出たら、最初の 1 回だけ許可します:
   - macOS 15 以降: 一度開こうとして、出た表示を閉じ（「ゴミ箱に入れる」は押さない）、**システム設定 → プライバシーとセキュリティ →「このまま開く」** を押します（パスワードを聞かれます。開こうとしてから約 1 時間だけ押せます）。
   - macOS 14: アプリを右クリック →「開く」→「開く」。
3. アプリは **「はじめに」** タブで始まります。**クイックアクションをインストール** → **Finder の設定を開く** で、Flect の項目をオンにします（詳しくは下）。**「必要なツール」** タブに、一部の項目が使う外部ツール（`brew install ffmpeg-full sevenzip`）が出ます。

**zip から（ターミナル）**

```bash
shasum -a 256 -c Flect-for-Mac-macos-arm64.zip.sha256     # ダウンロードの検証
unzip Flect-for-Mac-macos-arm64.zip && mv Flect-for-Mac.app ~/Applications/
xattr -dr com.apple.quarantine ~/Applications/Flect-for-Mac.app   # macOS の警告を出さない
~/Applications/Flect-for-Mac.app/Contents/Resources/flect-tools --out ~/Library/Services   # アプリのボタンでも可
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

1. アプリの **「はじめに」** タブの **Finder の設定を開く** を押すか、**システム設定 → 一般 → ログイン項目と機能拡張** を自分で開き、下の **拡張機能** の **Finder** の右にある (i) ボタンを押します。
2. **Flect: …** の項目（35 個）をすべてオンにします。ほかの項目はそのままで構いません。
3. それでも Flect の項目が出ないときは、`killall Finder` を実行します。

そのあと、Finder でファイルを右クリック →「クイックアクション」→ Flect。設定はアプリ本体を開いて変更します。

## アップデート

Flect は自分で更新しません。新しいバージョンを知るには:

- **アプリで:** 設定 → **はじめに** → **4. アップデート** → **更新を確認**。GitHub へ 1 回だけ問い合わせ（下記）、新しいリリースがあればバナーと、ダウンロードページを開くボタンが出ます。設定ウィンドウを開いたとき、1 日に 1 回自動で確認することもできます（初期はオフ）。
- **GitHub で:** リポジトリのページで **Watch → Custom → Releases** を選ぶと、新しいリリースのたびに通知（有効にすればメールも）が届きます。フィードリーダーなら `https://github.com/whitewater-png/Flect-fork-for-Mac/releases.atom` を購読できます。

新しいバージョンは、最初と同じ手順で入れます（新しいアプリを Applications へドラッグして、古いものと置き換えます）。設定は引き継がれます。

## 不具合の報告

不具合や質問は、**Issues** タブから issue を作ってください。macOS のバージョン、Flect のバージョン（Finder でアプリの「情報を見る」）、行った操作と起きたことを書いてください。公開したくないファイルは添付しないでください。セキュリティ上の問題は、非公開で報告してください（[SECURITY.md](SECURITY.md) を参照）。

## セキュリティ設計

- Swift アプリは画面と OS 機能を担当し、ファイル処理は別プロセスの Rust エンジン（`unsafe` 禁止）が JSON Lines で受け持ちます。実行ごとに 1 プロセスです。
- 外部ツールはシェルを使わず、固定フォルダから引数配列で起動します。ffmpeg のプロトコルは `file,pipe` のみ許可します。
- アーカイブ展開は、絶対パス・`..`・リンク・解凍爆弾を拒否します。
- 出力は排他的に作成し、既存ファイルを上書きしません。
- JAR のデコンパイルは SHA-256 を固定した CFR を、利用可能なら `sandbox-exec` の中で実行します。
- 通信: アプリは、あなたが始める（またはオンにした）更新の確認を除いて、**自分から通信しません**。確認は `api.github.com` への HTTPS 1 回で、あなたや Mac の情報は何も送りません。返ってきた答えは、このリポジトリの `vX.Y.Z` 形式のリリースのときだけ受け付けます。ダウンロードもインストールも自動では行いません。
- 詳細は [`SECURITY.md`](SECURITY.md) と [`docs/mac-port/02-requirements.ja.md`](docs/mac-port/02-requirements.ja.md)。

## ライセンス

Apache License 2.0。[`LICENSE`](LICENSE)・[`NOTICE`](NOTICE)・[`NOTICES.md`](NOTICES.md) を参照。原作の著作権は Konayukiw にあります。
