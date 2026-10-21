# Flect-for-Mac — 11 Mac 実機の確認手順書

対象: MacBook Pro（Apple Silicon / macOS 14 以上）。所要時間の目安: 準備 20 分 + 確認 40〜60 分。
目的: CI では確認できない「Mac でしか分からない動作」を確かめて、結果を報告する。
**結果の書き方**は末尾の「報告テンプレート」を使う（○ / △（一部動く）/ ×、気づいたことを一言）。

> CI で確認済み: コンパイル、ユニットテスト、Quick Action の生成と plist の文法、.app の組み立てと署名、エンジンの起動。
> この手順書は、それ以外（実際の起動・表示・OS 機能・外部ツール連携）を確認する。

## 0. 準備（初回のみ）
```bash
# 1. Xcode Command Line Tools（Swift が入っているか）
xcode-select -p || xcode-select --install
swift --version        # 5.9 以上

# 2. Homebrew 経由のツール（ffmpeg / 7-Zip / Rust）
brew install rustup sevenzip ffmpeg
rustup default stable        # 最近の Homebrew では rustup-init ではなくこちら
source "$HOME/.cargo/env" 2>/dev/null || true

# 3. （JAR のデコンパイルを試す場合のみ）Java
brew install openjdk
java -version

# 4. リポジトリを取得してブランチを切り替え
git clone https://github.com/whitewater-png/Flect-fork-for-Mac.git
cd Flect-fork-for-Mac
git checkout claude/adoring-fermat-jaa4tq
```
**確認 0-A**: `which ffmpeg ffprobe 7zz` がすべて表示される（`/opt/homebrew/bin/...`）。
**確認 0-B**: `ffmpeg -hwaccels` の出力に `videotoolbox` がある。

## 1. Rust 側のテスト（5〜10 分）
```bash
cd core && cargo test 2>&1 | tail -15 && cd ..
```
**確認 1**: 最後の行が `test result: ok` で終わる（失敗があれば、失敗したテスト名を報告）。
> JAR のテストは CFR（デコンパイラ）が無いとスキップ／失敗することがある。その場合は 8 章で扱うので、ここでは名前だけ報告。

## 2. ビルドとインストール
```bash
scripts/build.sh --install
killall Finder
```
- 初回は 3〜6 分かかる。`built: .../dist/Flect-for-Mac.app` と `Quick Actions installed ...` が出れば成功。
- アプリは `~/Applications/Flect-for-Mac.app`、Quick Action は `~/Library/Services/` に入る。

**確認 2-A**: `ls ~/Library/Services | head` に `Flect ...workflow` が並ぶ（35 個）。
**確認 2-B**: `open ~/Applications/Flect-for-Mac.app` で設定ウィンドウが開く（初回に「開発元を確認できません」と出たら、システム設定 → プライバシーとセキュリティ → 「このまま開く」）。
**確認 2-C**: ウィンドウが日本語で表示される（言語が「システム」のとき）。

> 失敗したら: エラーメッセージの最後の 20 行をそのまま報告。

## 3. テスト用ファイルを作る
```bash
mkdir -p ~/FlectTest && cd ~/FlectTest
# 画像（macOS 標準の壁紙などから）
cp /System/Library/Desktop\ Pictures/*.heic ~/FlectTest/ 2>/dev/null || true
sips -s format png /System/Library/CoreServices/DefaultDesktop.heic --out photo.png 2>/dev/null || true
# 動画（10 秒のテスト映像と音声）
ffmpeg -y -f lavfi -i testsrc=duration=10:size=1280x720:rate=30 -f lavfi -i sine=frequency=440:duration=10 -c:v libx264 -c:a aac -shortest sample.mp4
# テキスト
printf 'こんにちは\r\nFlect test\r\n' > sample.txt
printf '{"b":1,"a":[3,2,1]}' > sample.json
# 重複ファイルと空フォルダ
mkdir -p dup empty1/empty2 && echo hello > dup/a.txt && cp dup/a.txt dup/b.txt
ls
```
`photo.png` が作れなかった場合は、手持ちの PNG / JPG を `~/FlectTest/` に入れる。

## 4. Finder の右クリック（最重要）
1. Finder で `~/FlectTest/sample.mp4` を右クリック → 「クイックアクション」を開く。
   **確認 4-A**: 動画向けの Flect 項目（リサイズ / 圧縮 / 変換など）が出る。画像・テキスト向け項目は出ない。
2. `photo.png` で同じことをする。**確認 4-B**: 画像向けの項目が出る。
3. 複数ファイル（`sample.txt` と `sample.json` を選択）。**確認 4-C**: テキスト系の項目が出る。
4. 拡張子の違い: `ffmpeg -y -i sample.mp4 -c copy sample.mkv` などで `.mkv` `.webm` `.flv` を作り、右クリックで動画項目が出るか。**確認 4-D**: ○ / × を種類ごとに報告（出ない種類があれば教えてほしい。UTI の調整が必要）。
5. 新しい Quick Action は Finder 拡張の設定で**既定オフ**のことがある。右クリックに出ないときは、システム設定 → ログイン項目と機能拡張 → Finder で Flect の項目を 1 個ずつオンにする（「キーボード → サービス」では既にオン）。
6. 項目が出ないときの再読み込み: `killall Finder` と、システム設定 → キーボード → キーボードショートカット → サービス で Flect 項目にチェックが入っているか確認。

## 5. 画像・テキスト・フォルダ
| # | 操作 | 期待する結果 |
| --- | --- | --- |
| 5-1 | `photo.png` → 画像 → リサイズ → 50% | 進捗ウィンドウが開き、同じフォルダに `photo (2).png` ではなく別名の縮小画像ができる。元ファイルは変わらない |
| 5-2 | 画像 → 変換 → JPG / WEBP | 変換後のファイルができる（既存ファイルを上書きしない） |
| 5-3 | 画像 → 変換 → HEIC | `.heic` ができ、プレビューで開ける（ImageIO を使う） |
| 5-4 | 画像 → カスタムサイズ | ダイアログが出て、幅/高さを入れて実行できる |
| 5-5 | 画像 → 文字認識（OCR） | 文字入りの画像（スクリーンショット等）で、テキストが `.txt` で出る。日本語が読めるか確認 |
| 5-6 | `sample.txt` → テキスト → 改行 → LF / 文字コード → UTF-8 | 変換できる。`file sample*.txt` で確認 |
| 5-7 | `sample.json` → JSON → 整形 / 並べ替え | 整形された JSON ができる |
| 5-8 | `dup` フォルダ → フォルダ → 重複を削除 | 結果ウィンドウに重複グループが出る → 「最も古いものを残す」で、もう一方が**ゴミ箱に移動**される（完全削除ではない） |
| 5-9 | `empty1` フォルダ → 空フォルダを削除 | 確認ダイアログ → 実行でゴミ箱へ移動 |
| 5-10 | フォルダ → ツリー表示 / 分析 | テキストの結果ウィンドウが出て、コピーできる |
| 5-11 | フォルダ → 一括リネーム | 入力ダイアログ → プレビュー → 適用で名前が変わる |
| 5-12 | フォルダ → 圧縮（ZIP） | `.zip` ができる。`unzip -l` で中身を確認 |

## 6. 動画・音声（ffmpeg と VideoToolbox）
| # | 操作 | 期待する結果 |
| --- | --- | --- |
| 6-1 | `sample.mp4` → 動画 → 圧縮 → 5MB | 進捗バーが動き、目標サイズ前後の mp4 ができる |
| 6-2 | 動画 → 変換 → MOV / MKV / WEBM / GIF | それぞれ再生できるファイルができる |
| 6-3 | 動画 → 音声を取り出す → MP3 / M4A | 音声ファイルができる |
| 6-4 | 動画 → トリミング | プレーヤー付きダイアログが出て、開始/終了を指定して実行できる |
| 6-5 | 動画 → サムネイル | 指定時刻の画像ができる |
| 6-6 | 実行中に「キャンセル」 | 数秒以内に止まり、中途半端なファイルが残らない |
| 6-7 | （設定で VideoToolbox を選んだ場合）圧縮 | 速度が出る。失敗したら警告が出てソフトウェアエンコードに切り替わる |

## 7. PDF
`~/FlectTest` に PDF を 2 つ用意する（`cupsfilter` やプレビューで書き出し、または手持ちの PDF）。
| # | 操作 | 期待する結果 |
| --- | --- | --- |
| 7-1 | PDF → プレビュー | PDF が表示される |
| 7-2 | PDF → 画像（PNG / JPG） | ページごとの画像ができる |
| 7-3 | PDF → テキスト | `.txt` ができる（文字が無い PDF では警告） |
| 7-4 | 2 つの PDF を選択 → 結合 | 順序ダイアログ → 結合された PDF ができる（結合はアプリ側の PDFKit が行う） |

## 7.5 字幕（whisper）
```bash
ffmpeg -filters | grep whisper                 # whisper が出ること（ffmpeg-full なら出る）
scripts/fetch-whisper-model.sh small           # モデルを入れる（約 480MB。tiny なら約 75MB で試せる）
```
| # | 操作 | 期待する結果 |
| --- | --- | --- |
| 7.5-1 | 声の入った動画/音声を右クリック → 字幕 | 進捗が動き、同じフォルダに `名前.srt` ができる |
| 7.5-2 | 設定で出力を VTT / TXT に変えて再実行 | `.vtt`（先頭が `WEBVTT`）/ `.txt` ができる |
| 7.5-3 | モデル未インストールで実行 | 「音声認識モデルがありません」と出る |
| 7.5-4 | 設定の言語を `ja` にして日本語音声 | 日本語の字幕になる |
| 7.5-5 | 実行中にキャンセル | 数秒で止まり、`.srt` が残らない |
| 7.5-6 | 速度の目安 | 1 分の音声が何秒で終わったか（モデル別）を報告 |

## 8. アーカイブ・JAR（必要な人のみ）
- ZIP / 7z を右クリック → 展開。展開先は同じフォルダ内の新しい名前のフォルダ。上書きされないこと。
- JAR のデコンパイル: `scripts/fetch-cfr.sh` を実行 → `.jar` を右クリック → デコンパイル。`sandbox-exec` が使えない場合は警告が出てサンドボックスなしで実行される（警告の文面を報告）。

## 9. セキュリティの簡易確認
- 実行後、`ls -l ~/Library/Application\ Support/Flect-for-Mac/settings.json` が `-rw-------`（0600）。
- Quick Action の一時ファイルが残っていない: `ls /tmp /var/folders/*/*/T 2>/dev/null | grep flect-sel`（何も出ない）。
- 既存ファイルと同じ名前の出力が上書きされない（5-1 を 2 回実行すると 2 つできる）。

## 10. 困ったとき
| 症状 | 確認 |
| --- | --- |
| Quick Action が出ない | `killall Finder`、サービス設定でチェック、`~/Library/Services` に `.workflow` があるか |
| 項目を選んでも何も起きない | 「システム設定 → プライバシーとセキュリティ」で Flect の許可待ちが無いか。`open -a Flect-for-Mac` で単体起動できるか |
| 「処理エンジンが見つかりません」 | `ls ~/Applications/Flect-for-Mac.app/Contents/Resources/flect-engine` |
| ffmpeg が無いと言われる | `ls /opt/homebrew/bin/ffmpeg`（固定フォルダだけを探す仕様） |
| 文字認識で「言語が無い」 | 設定画面の OCR 言語を空にするか `ja-JP, en-US` を入れる |

## 報告テンプレート
```
環境: macOS ___ / Mac: ___ / Swift ___
0-A ○ 0-B ○ 1 ○ 2-A ○ 2-B ○ 2-C ○
4-A ○ 4-B ○ 4-C ○ 4-D mp4○ mkv× webm○ flv○
5-1 ○ 5-2 ○ 5-3 ○ ...（番号ごとに ○/△/×）
気づいたこと:
- 
```
エラーは、画面の文言をそのまま貼るか、スクリーンショットを渡してほしい。

> 第 1 回の実施結果は [12](12-mac-verification-results.ja.md)。
