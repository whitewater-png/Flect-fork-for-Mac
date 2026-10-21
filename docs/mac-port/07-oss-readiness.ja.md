# Flect-for-Mac — 07 OSS 公開準備チェック

更新: 2026-10-20 ／ 注意: これは技術的な棚卸しで、**法的助言ではありません**。最終判断はメンテナ（と必要なら専門家）が行ってください。

## 1. ライセンスの前提（確認済みの事実）
- 上流 Flect は **Apache-2.0**（`LICENSE`、`Copyright 2026 Konayukiw`）。
- 上流 README の License 節は、派生物に次を求めている: 既存の著作権・ライセンス・帰属表示の維持 / 原作者 Konayukiw への帰属を `README` `NOTICE` `NOTICES` 等に記載 / Apache-2.0 全文の同梱。
- → 対応: `LICENSE` と `NOTICES.md` を維持、**`NOTICE` を新設**（上流への帰属と改変内容を明記、ドラフト）、README に移植中表記を追加。

## 2. 同梱アセット（`legacy/windows/` 配下に移動済み）
| アセット | 場所 | 確認できたこと | 状態 |
| --- | --- | --- | --- |
| Inter（フォント） | `src/App/Assets/Fonts/Inter.ttf` | フォント内メタデータ: *Copyright 2016 The Inter Project Authors*、**SIL OFL 1.1** | ○ ただし `NOTICES.md` に**記載が無い**ので追記が必要 |
| Higure Gothic（フォント） | `…/Higure.ttf`（7.4 MB） | メタデータ: *Copyright 2021 The Zen Project Authors*（positype/Zen-Project）、**SIL OFL 1.1**、制作 Yoshimichi Ohira | ○ 同上。OFL は再配布時に著作権表示とライセンス文の同梱が必要。Reserved Font Name の有無は未確認 |
| アプリアイコン | `…/flect.ico`、`src/Web/assets/Flect.png` | 由来・権利者の記載なし | **要確認**（上流作者の作品と推定。再利用可否は作者に確認） |
| ヒーロー画像 / GIF / プロモ動画 | `…/Introduction/*`（計約 60 MB） | 由来・権利者の記載なし。動画に BGM/素材が含まれる可能性 | **要確認**。Mac 版では使わず、必要なら新規作成を推奨 |
| Web サイト | `src/Web/` | 上流のサイト | Mac 版では不使用。公開リポジトリに残すか要判断 |

→ 方針案: Mac 版のアプリにはアセットを**一切持ち込まず**、新アイコン等を用意する。フォントを使う場合は OFL 全文と著作権表示を `NOTICES.md` に追記。

## 3. Rust 依存のライセンス（`cargo metadata` の実測、2026-10-20）
全 ~70 クレートが次のいずれか: MIT / Apache-2.0 / Zlib / BSD-3-Clause（`encoding_rs` の一部） / **BSL-1.0**（`xxhash-rust`） / Unlicense / 0BSD / Unicode-3.0。**コピーレフト必須のものは無し**（`r-efi` は MIT/Apache と LGPL の選択式、`chardetng` は Apache-2.0 OR MIT）。`core/deny.toml` を実態に合わせて更新済み（ローカルに `cargo-deny` が無いので判定は CI で初回実行）。

## 4. 外部ツール（同梱しない方針）
| ツール | 扱い | 注意 |
| --- | --- | --- |
| ffmpeg | Homebrew から利用者が導入（実機は GPL ビルドの `ffmpeg-full`） | 同梱・再配布しないので GPL 義務は発生しない。README に導入手順を書く |
| 7-Zip (`7zz`) | 同上（`brew install sevenzip`） | 同梱しない |
| CFR (`cfr.jar`) | MIT。取得スクリプトで利用者が取得 | 同梱するなら MIT 表示が必要 |
| Java / Python | 利用者導入 | — |

## 5. リポジトリ衛生
- **要対処: 過去コミットに `core/target/`（ビルド成果物 約 3,000 ファイル、`.git` 366 MB）が混入している。** 原因は私が `.gitignore` を壊したこと（末尾改行なしの行に追記）。今回のコミットで追跡から外し `.gitignore` を修正したが、**履歴には残る**。公開前に履歴の書き換え（対象は私の作業ブランチのみ）か、公開用に新規リポジトリへ履歴なしで移すことを推奨 — 実施前にユーザー確認を取る。
- 秘密情報の簡易スキャン（`git grep` で api key / secret / password / 秘密鍵ヘッダ）: 該当なし（ヒットは `unique_token` 等の無害な語のみ）。
- 作者表記: git 履歴に Konayuki（上流）、yappi（上流コントリビュータ）、Claude（私の作業分）。

## 6. 公開前の TODO
| # | 項目 | 担当 |
| --- | --- | --- |
| 1 | 新コードの著作権者表記とライセンス（Apache-2.0 のままか） | **メンテナが決定** |
| 2 | 公開名（`Flect-for-Mac` が上流と紛らわしくないか）、上流作者への連絡 | メンテナ |
| 3 | バンドル ID の確定（仮 `com.whitewater.flect-for-mac`） | メンテナ |
| 4 | アイコン・画像類の再利用可否確認、または新規作成 | メンテナ（作者確認）/ 私（代替案） |
| 5 | `NOTICES.md` を Mac 版の依存（Rust crates、フォントを使う場合は OFL）に更新 | 私 |
| 6 | README を Mac 版として全面書き直し（導入は `brew install ffmpeg sevenzip`、ソースからビルド） | 私 |
| 7 | `core/target` 履歴問題の解消 | 私（ユーザー確認後） |
| 8 | `SECURITY.md` / `NOTICE` のドラフトを確定、issue テンプレート、CONTRIBUTING 更新 | 私 + メンテナ |
| 9 | 署名なし配布のため、**ソースからのビルド手順**を主導線にする（ダウンロードしたバイナリは Gatekeeper に止められる） | 私 |
