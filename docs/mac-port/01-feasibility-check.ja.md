# Flect macOS 版 — 01 可否チェック
関連: [01 可否チェック](01-feasibility-check.ja.md) / [02 要件定義](02-requirements.ja.md) / [03 設計計画](03-design-plan.ja.md)

対象: `whitewater-png/Flect-fork-for-Mac`（上流 Konayukiw/Flect、Apache-2.0）／調査コミット `1231b9d`
作成日: 2026-10-20 ／ 開発ブランチ: `claude/adoring-fermat-jaa4tq`

**確定済みの方針（ユーザー回答）**
1. **Mac 専用に作り替える**（Windows 版の維持はしない）
2. **Apple Developer Program 未加入**（自分の Mac で使うだけ → ad-hoc 署名・ローカルビルド前提）
3. **全機能を実装**する。**Rust 等で高速化**してよい。ただし**情報セキュリティ上、安全側であること**が前提
4. 構成は **Swift（UI・Finder・OS 連携）＋ Rust（処理コア）**

> 本書の「Mac 側 API/ライブラリの挙動」は、このクラウド環境（Linux）では実機検証できないため**推定を含む**。リポジトリ内コードの調査結果（場所・行数・Windows 依存）は実際に grep/wc で確認済み。推定箇所は [03 設計計画](03-design-plan.ja.md) §9 の PoC で潰す。

---

## 1. 可否チェックの結論

**可能。ただし「改修」ではなく「再構築」になる。** 理由と、それでも現実的な根拠:

| 層 | 現状 | Mac での扱い |
| --- | --- | --- |
| Finder 連携 `src/Shell`（C++ COM、約 2,300 行） | Windows 専用 API（IContextMenu / IExplorerCommand / MSIX） | **流用不可**。Swift で新規 |
| GUI `src/App/Gui`（WPF、XAML 3,093 行 + C# 2,493 行） | WPF 専用 | **流用不可**。SwiftUI/AppKit で新規 |
| 処理ロジック `src/App/Main`+`Tasks`（C# 7,471 行） | .NET。Windows 依存は約 12 箇所のみ | **仕様として流用**（Rust に再実装。ロジックの設計・閾値・命名規則・メニュー定義が資産） |
| i18n（EN/JA/ZH、1,467 行） | C# 文字列 | String Catalog に変換して流用 |
| 外部ツール（ffmpeg / 7-Zip / CFR / Python） | exe 同梱 | Mac 版バイナリに差し替え |
| 配布（Inno Setup / MSIX / PowerShell） | Windows 専用 | 全面置換 |

**根拠となる資産（ここが移植を現実的にしている）**
- ワーカーとシェルの契約が OS 非依存: `Flect --task <id> --input <選択リスト>`（`Request.cs` / `Launcher.cpp`）。
- メニュー定義が `MenuCatalog.cs`（リーフ 67・カテゴリ 8）に集約。
- 既存の `evals/Evals`（xunit: Folder/Image/Video/Document/Update）が**振る舞いの仕様書**として使える。
- 重い処理の大半は外部ネイティブ（ffmpeg / PDFium / 7-Zip）で、言語非依存。

**正直な注意**
- 「Rust にすれば全部速くなる」わけではない。動画圧縮・PDF 描画は ffmpeg/PDFium のネイティブ性能で決まり、Rust 化しても変わらない（推定）。Rust が効くのは **フォルダ走査・重複検出（並列ハッシュ）・画像リサイズ（SIMD）・テキスト/JSON/アーカイブ処理**と、**メモリ安全性**（セキュリティ要件への貢献）。速度目標は P0 で C# 版と実測比較して決める。
- ffmpeg・7-Zip・PDFium・libwebp 等は C/C++ のままなので、Rust 化では安全にならない。**サブプロセス分離・入力検証・プロトコル制限**で守る（[02 要件定義](02-requirements.ja.md) §4.3）。
- この環境は Linux のため、Swift の UI / Finder 連携は**ここでは動作確認できない**。Rust コアは Linux で完全にビルド・テスト可能、Swift 側は GitHub Actions の `macos-14` で CI ビルド、最終の Finder 実機確認はユーザーの Mac で行う。

---

## 2. 現状アーキテクチャ（調査結果）

```
Explorer 右クリック
 └ C++ Shell 拡張 (src/Shell)
     選択を分類 (Selection.cpp: Folder/Pdf/Image/Video/Audio/Text/Jar/Archive、拡張子・アニメGIF・allJson 判定)
     メニュー生成 (MenuBuilder.cpp / Labels.h)  ← HKCU\Software\Flect から言語・非表示項目・プリセットを取得
     選択を UTF-8(BOM) の一時ファイルへ → Flect.exe --task "image.resize.p1" --input <file> (Launcher.cpp)
 └ C# ワーカー (src/App, WPF)
     Request.Parse → Factory → Runner → Task.Configure()[ダイアログ]/RunAsync()[進捗]/Present()
     外部: ffmpeg/ffprobe, 7za/7z, cfr.jar(+Java), Python venv(rembg, faster-whisper)
     lib : Magick.NET, PDFtoImage(PDFium), PdfPig, System.IO.Hashing, UTF.Unknown
     設定: %APPDATA%\Flect\settings.json ＋ メニュー用の一部を HKCU\Software\Flect にミラー(ShellBridge.cs)
```

---

## 3. Windows 依存の洗い出し（Mac 版での置換先）

| # | 場所（実在確認済み） | Windows 依存 | Mac 版での置換 |
| --- | --- | --- | --- |
| 1 | `Main/Common/Recycler.cs` | `SHFileOperationW` | Swift `FileManager.trashItem`（UI 側。Rust は「削除要求」をイベントで返す or 小さな Swift ヘルパー） |
| 2 | `Main/Common/ExplorerOrder.cs` | `StrCmpLogicalW` | Rust: 自然順比較（`natord` 等 or 自前）。Finder 準拠は `localizedStandardCompare`（Swift） |
| 3 | `Main/Config/ShellBridge.cs` | レジストリへ言語/非表示/プリセットを公開 | 不要化。`menu.json` と設定は単一ディレクトリの JSON を Swift が直接読む |
| 4 | `Main/Config/Theme.cs`, `Gui/Backdrop.cs` | レジストリ/DWM | SwiftUI 標準（外観・Vibrancy は OS 任せ） |
| 5 | `Main/Common/Tools.cs` | `.exe` 名、PATH 探索 | `deps/` 同梱を最優先、無ければ `/opt/homebrew/bin` 等の**固定候補のみ**探索（PATH 無条件信頼はしない） |
| 6 | `Main/Common/PythonHost.cs`, `Image/BackgroundRemover.cs`, `Subtitles/SubtitleSupport.cs` | `py.exe`、`Scripts\python.exe`、CUDA DLL | `python3`、`env/bin/python3`。CUDA は無効化（Mac は CPU/CoreML/MLX 系） |
| 7 | `Main/Media/Accelaration.cs`, `Encoders.cs` | レジストリで GPU 指紋、NVENC/QSV/AMF | `h264_videotoolbox` / `hevc_videotoolbox` を ffmpeg で実機プローブ。指紋は ffmpeg バイナリ + `sysctl`/`system_profiler` |
| 8 | `Main/Ocr/OcrLanguages.cs`, `Tasks/Ocr/Ocr.cs` | WinRT OCR、言語パック導入に PowerShell | **Apple Vision**（`VNRecognizeTextRequest`）。Swift 側で実行。言語パック導入 UI は不要 |
| 9 | `Main/Image/ImageIo.cs` | HEIC 書込を WIC + HEVC 拡張 | **ImageIO**（`CGImageDestination`, public.heic）— OS 標準で書込可（Swift 側） |
| 10 | `Main/Common/Locations.cs` | `%APPDATA%` | `~/Library/Application Support/Flect`（`FLECT_HOME` 上書きは維持） |
| 11 | `Main/Update/Updater.cs` | レジストリ Run キー、`setup.exe`、上流 URL | `SMAppService` or 起動時チェックのみ。**自動インストールはしない**（[02](02-requirements.ja.md) §4.3 SEC-10）。URL は fork へ |
| 12 | `Main/Folder/Scanner.cs` | `desktop.ini`/`Thumbs.db` 除外 | `.DS_Store`、`._*`、`.Spotlight-V100`、`.Trashes`、`.fseventsd` を追加。APFS 大小文字非区別・NFD 正規化を考慮 |
| 13 | `Gui/Dialogs/Trim`, `Thumbnail` | `MediaElement` | `AVPlayerView`/`AVKit`（Swift ネイティブなので問題解消） |
| 14 | `Gui/Dialogs/PdfPreview`, `PdfOrder` | WPF BitmapImage | `PDFKit`（`PDFView`） |
| 15 | `Main/I18n/Strings.*`, `evals/i18n/check.mjs` | 「Explorer」「ごみ箱」「スタートメニュー」「Windows 言語パック」 | String Catalog 化し文言を Mac 向けに修正。欠落チェックは流用 |
| 16 | `Tasks/Text/TextConvert.cs` | 改行「CRLF (Windows)」表記 | 文言のみ。処理は同じ |
| 17 | `scripts/*.ps1`, `setup/`, `src/Web/install.ps1`, `.github/workflows` | PowerShell/Inno/MSIX | bash（`fetch.sh`/`build.sh`）+ `macos-14` CI |
| 18 | `evals/Evals` | `net10.0-windows`、`System.Speech` | Rust 統合テスト（golden fixtures）へ移植 |
