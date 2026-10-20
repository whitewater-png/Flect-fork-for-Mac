# Flect macOS 版 — 03 設計計画書
関連: [01 可否チェック](01-feasibility-check.ja.md) / [02 要件定義](02-requirements.ja.md) / [03 設計計画](03-design-plan.ja.md)

## 5. 設計

### 5.1 全体構成

```
Finder 右クリック（Quick Actions / Services、将来 Finder Sync）
   └─ Flect.app (Swift: SwiftUI + AppKit)       … UI・OS 連携・設定・i18n・ダイアログ
        │  起動引数 / URL: --task <id> --input <list>（現行契約を踏襲）
        ├─ 事前ダイアログ（Configure）→ ジョブ JSON を組立て
        └─ flect-engine (Rust, 別プロセス, JSON Lines over stdio)
              ├─ ffmpeg / ffprobe   （argv、プロトコル制限）
              ├─ 7zz               （RAR/7z。出力は検証後に移動）
              ├─ java + cfr.jar    （JAR）
              └─ python venv       （rembg / 字幕）
   OS ネイティブは Swift 側: Trash, Vision OCR, ImageIO(HEIC), AVKit, PDFKit, NSPasteboard
```

**責務の線引き**
- **Rust エンジン**: ジョブ実行・パス導出・一時ファイル/原子的リネーム・設定読書き・メニューカタログ（唯一の正）・フォルダ/重複/ツリー/解析・アーカイブ・テキスト/JSON・画像変換/圧縮/回転・PDF 結合/変換・ffmpeg コマンド計画（圧縮ラダー）・外部プロセス管理・キャンセル。メッセージは**キー+引数**で返し、ローカライズは UI。
- **Swift アプリ**: 進捗/ダイアログ/設定画面/テーマ/言語、Finder 連携、ごみ箱、Vision OCR、HEIC エンコード/デコード、動画/PDF プレビュー。

**エンジン ⇄ UI プロトコル（案）**
- UI→エンジン: `{"op":"run","task":"image.resize.p1","paths":[…],"options":{…}}` / `{"op":"cancel"}` / `{"op":"export-menu"}`
- エンジン→UI: `status` / `step{done,total}` / `info|warn|error{key,args}` / `needs{"trash":[paths]}`（ごみ箱要求）/ `needs{"ocr":path}`（Vision 要求）/ `done{outcome,summary}`
- 各行にサイズ上限・UTF-8 検証・未知フィールド拒否。

### 5.2 リポジトリ構成（Mac 専用化）

```
core/                 Rust workspace
  flect-core/         ジョブ・パス・設定・メニュー・各タスク（forbid(unsafe)）
  flect-media/        ffmpeg 計画/実行/進捗パース、圧縮ラダー（現 Media/Compression の移植）
  flect-engine/       bin: JSON Lines サーバ
  fixtures/ tests/    evals の golden 化 + 悪意フィクスチャ
app/                  Swift（XcodeGen の project.yml で Xcode プロジェクト生成）
  Flect/              アプリ本体（SwiftUI）
  FlectFinder/        Finder Sync appex（任意・§5.3）
  Resources/          Localizable.xcstrings, フォント(Inter, Higure), アイコン(.icns)
scripts/              fetch.sh（固定版+SHA256）, build.sh, make-workflows.sh, install.sh
legacy/windows/       現行 Windows ソースを移動（仕様参照用。パリティ達成後に削除）
docs/mac-port/        本書（分割: 01-check, 02-requirements, 03-design）
```
Windows ソースは削除せず `legacy/windows/` へ退避してから段階的に移植する（`git mv` で履歴維持、移植完了後に削除）。

### 5.3 Finder 連携（未加入ゆえ現実解を優先）
| 案 | 長所 | 短所 | 判断 |
| --- | --- | --- | --- |
| A. Quick Actions（`~/Library/Services/*.workflow`） | 署名・entitlement 不要。対象ファイル型で出し分け可（`NSSendFileTypes`/UTI） | フラット。67 項目は多い | **第一選択** |
| B. アプリの `NSServices` | Info.plist だけで宣言 | Services サブメニュー内で深い | 補助 |
| C. Finder Sync 拡張 | 動的・階層メニュー（Windows 版に最も近い） | サンドボックス/署名/ユーザー有効化が必要。ad-hoc や無料 Apple ID で安定動作するか**未検証** | **PoC 結果次第**（P0 で判定、ダメなら採用しない） |

- A の粒度: 操作単位（例: 「Flect: 画像をリサイズ…」「動画を圧縮…」）で約 20〜25 個に集約し、プリセット選択は起動後の小ウィンドウで行う（Windows 11 版 modern メニューの「操作→ポップアップ」設計と同じ発想）。`.workflow` は `scripts/make-workflows.sh` が `menu.json` から生成。
- 選択の分類は `menu.json` に「対象拡張子/UTI」を持たせ、Rust エンジンが唯一の正とする（C++ 側の二重管理を解消）。アニメ GIF 判定など動的判定は起動後に行い、不適合は丁寧にエラー。
- 起動: Quick Action 内の Run Shell Script → `open -n -a Flect --args --task … --input …`（パスリストは `0600` の一時ファイル）。

### 5.4 Rust の技術選定（候補。バージョンは実装時に最新を確認）
| 領域 | 方針 |
| --- | --- |
| 画像デコード/エンコード | `image` crate（PNG/JPEG/GIF/ICO）、`webp`（libwebp、lossy 用）、`resvg`（SVG 読込）、`fast_image_resize`（SIMD リサイズ）、`img-parts`/EXIF でメタデータ削除。HEIC は Swift/ImageIO |
| 並列/ハッシュ | `rayon`、`xxhash`/`blake3`（重複は サイズ→ハッシュ→バイト比較 を現行どおり） |
| アーカイブ | `zip`/`tar`/`flate2` ＋ 7z/RAR は 7zz サブプロセス（SEC-3） |
| テキスト | `encoding_rs`、`chardetng`（UTF.Unknown 相当）、`serde_json`（キーソート規則を移植）、`csv`、`quick-xml` |
| PDF | 結合 `lopdf`、描画/テキスト抽出は PDFium（`pdfium-render`、同梱 dylib はハッシュ固定）。**代替**: 描画を Swift `PDFKit` に任せる |
| 非同期/CLI | `tokio` 最小構成、`serde` + `serde_json` |
| 品質 | `clippy -D warnings`、`cargo-deny`、`cargo-audit`、`cargo-fuzz`、`insta`（golden） |

### 5.5 外部ツール（`scripts/fetch.sh`、バージョン固定 + SHA-256）
ffmpeg/ffprobe（arm64 静的ビルド。入手元と GPL/LGPL を P0 で確定）／ 7-Zip 公式 `7zz`（macOS）／ `cfr.jar` ／ Java は利用者導入 ／ Python 3 は利用者導入（システム or Homebrew）。同梱バイナリは Hardened Runtime 下で個別に ad-hoc 署名。

### 5.6 配布（未加入前提）
- **ローカルビルド運用が正**: `scripts/build.sh` が Cargo ビルド → `xcodebuild` → `Flect.app` を `~/Applications` へ配置 → ad-hoc 署名 → Quick Action を `~/Library/Services` に生成。自分でビルドしたアプリは quarantine が付かず Gatekeeper に止められない。
- GitHub Releases の `.dmg` 配布は**非対応**（ダウンロードで quarantine が付き、notarize 無しだと起動を止められるため）。将来 Developer ID を取得した場合は §8 の差分を適用。
- アンインストール: `scripts/uninstall.sh`（アプリ・Services・`Application Support` の削除手順を明示）。

### 5.7 i18n
`Strings.{En,Ja,Zh}.cs` を `Localizable.xcstrings` へ変換（キー維持）。エンジンはキー+引数のみ出力。`evals/i18n/check.mjs` は xcstrings 向けに改修して CI（Linux で実行可）に残す。

### 5.8 CI（GitHub Actions）
- `ubuntu-latest`: `cargo fmt/clippy/test/deny/audit`、i18n チェック、（可能な範囲の）ファズ短時間実行
- `macos-14`: `swift`/`xcodebuild` ビルド、アプリ起動なしのユニットテスト、`fetch.sh` のハッシュ検証、エンジン E2E（ffmpeg 取得して実行）
- 既存 Windows ワークフローは `legacy/` と共に削除（または手動のみに格下げ）

---

---

## 6. フェーズ計画

| Phase | 内容 | 検証可能な場所 | 完了条件 |
| --- | --- | --- | --- |
| **P0 PoC / 仕様凍結**（最優先） | ①Quick Action→Flect.app 起動 ②Finder Sync が ad-hoc/Personal Team で動くか ③Vision OCR（日/中/英） ④ImageIO HEIC 書込 ⑤Rust の WebP(lossy)/SVG/ICO/任意角回転 ⑥ffmpeg + VideoToolbox ⑦ffmpeg 静的ビルドの入手元/ライセンス確定 ⑧`evals/Evals` を golden 仕様へ抽出 ⑨C# 版と Rust の簡易ベンチ | 一部ユーザーの Mac | §9 の全項目が○/×確定、速度目標を数値化 |
| P1 基盤 | `legacy/windows/` 退避、Rust workspace、プロトコル、設定、メニュー定義と `menu.json`、パス導出/ワークスペース、CI | Linux | `cargo test` 緑、`export-menu` 出力 |
| P2 純 Rust タスク | フォルダ全般、アーカイブ(ZIP/TAR/tar.gz)、テキスト/JSON、PDF 結合 | Linux | golden テスト緑、悪意フィクスチャ拒否 |
| P3 画像 | リサイズ/圧縮/回転/変換/メタデータ削除/色キー背景除去 | Linux | golden 緑（HEIC/OCR は Swift 側 P5/P6） |
| P4 動画・音声 | ffmpeg 計画（リサイズ/トリム/サムネ/圧縮ラダー/回転/変換/音声抽出）、7zz(RAR/7z)、CFR | Linux（ffmpeg を取得） | 実ファイルで E2E 緑 |
| P5 Swift アプリ | 進捗・全ダイアログ・設定・i18n・ごみ箱・ImageIO・PDFKit/AVKit・Quick Actions 生成・`build.sh` | macos-14 CI + ユーザー実機 | Finder から主要タスクが完走（実機チェックリスト） |
| P6 OCR / AI | Vision OCR、rembg、字幕（faster-whisper は Mac で GPU 非対応のため CPU。`whisper.cpp`/`mlx-whisper` 代替を比較） | 実機 | FR-5/6 の該当機能が動作 |
| P7 仕上げ | Finder Sync（P0 で可なら）、更新確認、README(Mac)/NOTICES、Web 更新 | 実機 | 全 FR 達成、ドキュメント完備 |

並行: P5 は P1 のプロトコル確定後に着手でき、P2〜P4（Rust）と並列に進められる。

---

---

## 7. リスクと対策
| # | リスク | 対策 |
| --- | --- | --- |
| R1 | 全面書き直しで**既存挙動の取りこぼし** | `evals/Evals` を golden 化、`legacy/windows/` を仕様参照として残す、パリティ表で進捗管理 |
| R2 | Finder Sync が未署名/ad-hoc で動かない | Quick Actions を第一選択にしてあるので機能は損なわない（P0 で判定） |
| R3 | Rust の画像機能が ImageMagick に劣る（WebP lossy、ICO 多サイズ、GIF アニメ等） | P0 で範囲確認。不足分は libwebp 等の限定的 C バインディング + ImageIO で補う（ImageMagick は不採用） |
| R4 | Rust 化しても速くならない領域 | 実測ゲート（P0）。効果が無い箇所は無理に Rust 化せず外部ツールを直接利用 |
| R5 | ffmpeg の入手元/ライセンス/信頼性 | 入手元を固定し SHA-256 検証。自前ビルド手順も記録 |
| R6 | Python 依存(AI)のサプライチェーン | ロック+ハッシュ検証、無効化スイッチ、初回のみ通信 |
| R7 | **この環境で Mac 実機検証ができない** | CI(macos-14) + 実機チェックリスト。段階リリースごとにユーザーが確認 |
| R8 | 7-Zip/ffmpeg 等 C/C++ 脆弱性 | プロセス分離・フラグ制限・バージョン固定+定期更新・入力検証（[02](02-requirements.ja.md) §4.3） |
| R9 | 工数が大きい | フェーズごとに動く成果物を出し、Must 機能から順に実装 |

---

---

## 8. 将来 Developer Program に加入した場合の差分（参考）
Developer ID 署名 + Hardened Runtime + `notarytool` による notarize、`.dmg` 配布、Finder Sync の安定運用、Sparkle 等による署名付き自動更新、Homebrew Cask 化。

---

---

## 9. P0 で確定すべき未検証事項（推定を事実にしない）
1. Quick Action（および `NSServices`）で、拡張子/UTI による出し分けと複数選択が期待どおり動くか
2. Finder Sync 拡張が ad-hoc / 無料 Apple ID（Personal Team）で読み込まれるか
3. Vision の対応言語（日本語・中国語簡体字・英語）の認識品質
4. ImageIO での HEIC 書込（アルファ有無、品質指定）
5. Rust での WebP(lossy)、SVG→ラスタ、ICO 複数サイズ、任意角回転の品質/性能
6. ffmpeg arm64 静的ビルドの入手元・ライセンス・VideoToolbox 有効性
7. C# 版 vs Rust 版のベンチ（重複検出、走査、リサイズ）— 速度目標の根拠
8. `faster-whisper` の Mac 速度 vs `whisper.cpp`/`mlx-whisper`
9. ~~アプリのバンドルID/名称・最低 macOS・Intel 要否~~ → **確定（2026-10-20）**: アプリ名 `Flect-for-Mac`、最低 macOS 14、Apple Silicon (arm64) 専用。バンドルID は未指定のため仮に `com.whitewater.flect-for-mac` とし、変更可（要確認）
