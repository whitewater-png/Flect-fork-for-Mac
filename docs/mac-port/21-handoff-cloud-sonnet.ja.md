# クラウド Sonnet への引き継ぎ（Mac 検証 D-27 / D-29 まで）

作成: 2026-10-23。ブランチ `claude/mac-d27-checklist-83432d`（先頭 `0749e99`）。リポジトリ: `whitewater-png/Flect-fork-for-Mac`。

## 1. 経緯（ここまでに起きたこと）

- Mac 実機の検証チェックリスト（`19-mac-verification-checklist.ja.md`）の D′ 項目（Web ダウンロード）を、実機で確認してきた。結果は `20-mac-results-pdf-download.ja.md` に記録する運用。
- D-27（URL の無い文字を選んでサービス「Flect でダウンロード」）は、以前は「エラーが出ず設定トップが出るだけ」だった。`b06b8ee` で修正し、**実機で合格**した。
  - URL 無し → 窓に「Web アドレスが見つかりませんでした。」
  - 設定オフ → 「Web ダウンロードはオフです。設定の「ダウンロード」でオンにして保存してから、もう一度お試しください。」が先に出る
- D-29（遅いサーバーで実行し、進捗中にキャンセル）も**実機で合格**。進行中は `pgrep` に yt-dlp が出て、キャンセルの 3 秒後に何も出なくなり、途中ファイルも残らなかった。
- 上記 2 件の記録をコミット・push 済み（`0749e99`）。作者メールは GitHub の非公開設定で拒否されたため、過去コミットと同じ `noreply@anthropic.com` にした（git 設定は変更していない）。

## 2. 現在の状態

| 項目 | 状態 |
| --- | --- |
| ブランチ | push 済み。PR は**未作成** |
| 作業ツリー | この文書以外はクリーン |
| 実機の Flect | `~/Applications/Flect-for-Mac.app` は `b06b8ee` のビルドに入れ替え済み（Quick Action・サービス再登録、`pbs -update` 実施）。`20-…` 末尾の「後始末」にある「`5069726` のビルド」の記述は、この点で古い |
| ビルド方法 | `scripts/build.sh`（`--install` で ~/Applications へ）。この Mac では cargo が PATH に無いため `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"` が要る |
| テスト用サーバー | `scripts/mac-verify/slow_server.py --dir ~/FlectTest --port 8000`（約 40KB/s。`--fast` で通常速度）。今は停止済み |
| テスト用ファイル | `~/FlectTest/`（`Sample.webloc`、`Sample.url`、`Empty.url`、`d29/Slow.webloc` など）は残してある |

## 3. 未確認・残件

1. **D-28**（サービスが出ないときの手順: システム設定 → キーボード → サービス、`pbs -update`）は記録なし。サービス自体は D-26 / D-27 で出ている。
2. **C-7 / C-8 の目視**（実機で向きを目で見る）。CI の描画テストでは確認済み。
3. **D-16 実サイトでの画質上限**。自動確認のみ。
4. **Swift のテスト**（`swift test`）。この Mac は Command Line Tools のみで XCTest が無く未実行。CI の macOS ジョブでは成功している。
5. **E 項目（既存機能への影響の抜き取り）**と **B/C/D の他項目**は、`19-…` の「結果」欄と `20-…` を見て、空欄が無いか確認すること。
6. PR の作成（必要なら）。

## 4. クラウド側でできること / できないこと

- **できる:** 文書の整理、コード読み・差分レビュー、CI の確認、PR 作成（ユーザーの指示があれば）。
- **できない:** Mac 実機の操作（Finder の右クリック、サービス、Quick Action、`pgrep`）、Swift の実行、LAN 内のテスト用サーバー。**実機確認が要る項目は、手順を書いてユーザーに依頼する**こと。実機確認は、画面の文言をそのまま貼ってもらうと判断しやすい。

## 5. 守ること

- **公開（push 済みブランチの公開、PR の公開、リリース）と、重要データの上書き・削除は、先にユーザーに確認する。** 最終決定権はユーザー（リョウ）。
- 取り消せない操作は、黙って実装せず、懸念と代替案を先に出す。
- 実装した本人が査読せず、重要な成果物は別レーン（別セッションやサブエージェント）で見てもらう。
- 同じエラーが 2 回続いたら、3 回目の前に手段を変える。
- コミットは、ユーザーが「コミットして」と言ったときだけ。コミット末尾に `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>` を付ける。

## 6. 参照先

- 検証手順: `docs/mac-port/19-mac-verification-checklist.ja.md`（D′ は D-18〜D-29）
- 結果の記録: `docs/mac-port/20-mac-results-pdf-download.ja.md`
- 実装: `app/Sources/`（サービスの `downloadWithFlect` は `EntryPoints.swift`）
- 検証用スクリプト: `scripts/mac-verify.sh`、`scripts/mac-verify/`
