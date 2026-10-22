# Flect-for-Mac — 16 リリースチェックリスト（v0.1.0）

更新: 2026-10-21。○ 済 ／ □ 未了。「あなた」の項目は、GitHub の設定や Mac での操作が必要なもの。

## A. コードと文書（私が準備済み）
| # | 項目 | 状態 |
| --- | --- | --- |
| A1 | main の CI が全ジョブ緑（Rust / cargo-deny / Swift ビルド・テスト / .app 組み立て） | ○（PR #2 のマージ時点） |
| A2 | ライセンス表記（LICENSE、NOTICE、NOTICES.md、アイコンの扱い） | ○ |
| A3 | `CHANGELOG.md`、`docs/release/v0.1.0.md`（リリースノート。タグ名と同じファイル名で自動添付） | ○ |
| A4 | README（英・日）の状況表記、インストール手順、ダウンロード手順 | ○ |
| A5 | `SECURITY.md`（private vulnerability reporting を案内） | ○ |
| A6 | `release.yml`（`v*` タグで .app をビルド、zip と SHA-256 を Release に添付、ノートは A3 を使用） | ○（動作確認は下の B1） |
| A7 | 依存ツールの案内（`setup-deps.sh`、アプリの「必要なツール」タブ） | ○ |

## B. リリース前の確認
| # | 項目 | 担当 | 状態 |
| --- | --- | --- | --- |
| B1 | `release.yml` を手動実行（Actions → Release → Run workflow）して、成果物の zip と .sha256 ができるか見る（タグなしなので Release は作られない） | 私 / あなた | □ |
| B2 | B1 の zip を **別のフォルダに展開して**、`xattr -dr com.apple.quarantine` → 起動 → 設定ウィンドウとアイコンを確認 | あなた | □ |
| B3 | B2 の .app で Quick Action を生成（`flect-tools --out ~/Library/Services`）→ Finder で右クリック → 実行（画像リサイズなど 1 件） | あなた | □ |
| B4 | `scripts/setup-deps.sh --check` と通常モード、設定の「必要なツール」タブの見た目 | あなた | □ |
| B5 | 手順書 13 の未実施項目（JAR のデコンパイルなど、第 2 回で未確認のもの）があれば実施 | あなた | □ |
| B6 | Mac の旧 PoC（`~/Library/Services/Flect PoC.workflow`）を削除 | あなた | □ |
| B7 | ダウンロード機能（yt-dlp）を追加した版: yt-dlp 未導入で「必要なツール」に案内が出る／設定の「ダウンロード」が既定でオフ／yt-dlp を同梱・自動取得していない（`NOTICES.md` の記載と一致）／Stirling-PDF の独自ライセンス部分（`app/proprietary/`、`engine/`、`frontend/src/processor/` ほか）のコードを含めていない | 私 / あなた | □ |

## C. GitHub の設定（あなた）
| # | 項目 | 状態 |
| --- | --- | --- |
| C1 | Private vulnerability reporting を有効化 | ○ |
| C2 | CodeQL（Default setup）を有効化 | ○ |
| C3 | リポジトリの About に説明文と Topics（例: `macos`, `finder`, `quick-actions`, `rust`, `swift`, `ffmpeg`）を入れる | □ |
| C4 | main のブランチ保護（推奨）: PR 必須、CI の 3 ジョブ（Rust / deny / Swift）を必須チェックにする。Settings → Branches / Rules | □ |
| C5 | Dependabot（Settings → Advanced Security → Dependabot alerts / security updates）を有効化（推奨） | □ |
| C6 | Releases に「Latest」が付くのを確認後、Settings → General → Features で Discussions / Issues の要否を決める | □ |

## D. 公開（合図があってから私が行う）
1. 作業ブランチ `claude/adoring-fermat-jaa4tq`（旧履歴）の削除は、あなたの合図の後。取り消せないので、タグの動作確認が済んでから。
2. タグ `v0.1.0` を push（`git tag -a v0.1.0 -m "Flect-for-Mac 0.1.0" && git push origin v0.1.0`）。`release.yml` が動き、Release に zip・.sha256・リリースノートが載る。
3. 公開後の確認: Release のページ、zip のダウンロードと `shasum -a 256 -c`、README のリンク切れ。

## E. 公開後
- 利用者からの報告（Issues / 脆弱性報告）の窓口は GitHub。返信の目安はベストエフォート。
- 今後の候補: Homebrew tap、更新確認、Intel 対応、メニューの並べ替え。
