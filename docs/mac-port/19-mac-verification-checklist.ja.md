# Flect-for-Mac — 19 Mac 実機の確認チェックリスト（PDF ページ操作・Web ダウンロード）

対象: Apple Silicon の Mac / macOS 14 以上。所要時間の目安: 準備 15 分 + 確認 40〜60 分。
対象コミット: ______（`git rev-parse --short HEAD` か、Actions の実行ページに出るコミット）
実施日: ______　実施者: ______

使い方: 上から順に進め、各行の「結果」欄に **○**（期待どおり）／**△**（一部だけ動く・気になる点あり）／**×**（動かない）／**－**（省略）を書く。
△ と × は、画面の文言をそのまま（またはスクリーンショットで）メモする。最後の「F. 報告」にまとめる。

> このチェックリストの範囲: 2026-10 に追加した **PDF のページ操作**（分割・抜き出し・回転・削除）と **Web ダウンロード**、
> およびそれらが既存機能を壊していないことの抜き取り。それ以外の機能は [11 の手順書](11-mac-verification-guide.ja.md) と
> [12](12-mac-verification-results.ja.md)・[14](14-mac-results-round2.ja.md) の結果を参照。

## 0. どこまで自動で確認済みか（人がやらなくてよいところ）

| 内容 | 確認の方法 | 見る場所 |
| --- | --- | --- |
| Swift のコンパイルと全ユニットテスト | GitHub Actions の macOS ジョブ | Actions → CI → 「Swift app (macOS)」 |
| ページ指定の解析（全角入力を含む） | 同上（`PageSpecTests`） | 同上 |
| PDF の分割・抜き出し・回転・削除を**実物の PDFKit** で | 同上（`PdfPageToolsTests`。ページごとに大きさを変えた PDF で、どのページが残るか・回転・元の文書が変わらないこと・書き出して読み直せることを検査） | 同上 |
| Quick Action の生成（41 個）と plist の文法、`.app` の組み立て・署名・起動 | 同上 | 同上 |
| Web ダウンロードの通し確認（作ったエンジン ＋ 実物の yt-dlp / ffmpeg ＋ ローカルサーバー） | 同上の最後のステップ。手元でも `scripts/mac-verify.sh` で同じものが動く | 同上 |
| Rust のテスト・clippy・依存の監査 | GitHub Actions の ubuntu ジョブ | Actions → CI |

→ **A-1** で CI がすべて緑であることだけ確認する。残りは、画面を見て操作しないと分からないものを以下で確認する。

---

## A. 準備と入手

| # | 操作 | 期待する結果 | 結果 |
| --- | --- | --- | --- |
| A-1 | ブラウザで <https://github.com/whitewater-png/Flect-fork-for-Mac/actions> を開き、ブランチ `claude/exciting-brown-3b2qqy` の**最新の CI** を見る | 3 つのジョブ（Rust core / Dependency audit / Swift app (macOS)）がすべて緑。macOS ジョブの最後の「Web download end to end」も緑 | |
| A-2 | **アプリを入手する**（下の「入手方法」のどちらか） | `Flect-for-Mac.app` が手元にできる | |
| A-3 | `.app` を `~/Applications`（または `/Applications`）に置き、検疫属性を外す: `xattr -dr com.apple.quarantine ~/Applications/Flect-for-Mac.app` | エラーが出ない（ad-hoc 署名なので初回は必要。右クリック → 開く でもよい） | |
| A-4 | `open ~/Applications/Flect-for-Mac.app` | 設定ウィンドウが開く。「開発元を確認できません」が出たら、システム設定 → プライバシーとセキュリティ → 「このまま開く」 | |
| A-5 | 外部ツールを入れる: `brew install ffmpeg-full sevenzip yt-dlp`（yt-dlp は Web ダウンロードのときだけ必要） | `which ffmpeg ffprobe 7zz yt-dlp` がすべて `/opt/homebrew/bin/...`（Intel Mac は `/usr/local/bin`） | |
| A-6 | アプリの「はじめに」で **クイックアクションをインストール** → **Finder の設定を開く** → Flect の項目をすべてオン。`killall Finder` | `ls ~/Library/Services \| grep -c '^Flect '` が **41**（古い版が入っていて 38 のままなら、もう一度インストール） | |

### 入手方法（どちらか一方）

**方法 1（おすすめ・ビルドツール不要）: GitHub でビルドしたものをダウンロード**
1. <https://github.com/whitewater-png/Flect-fork-for-Mac/actions/workflows/release.yml> を開く。
2. 右の **Run workflow** → Branch に `claude/exciting-brown-3b2qqy` を選んで実行（5〜10 分）。
3. 完了した実行のページ下の **Artifacts** から `Flect-for-Mac-macos-arm64` をダウンロードして展開。中に `.zip`・`.dmg`・`.sha256` が入っている。
4. 照合: `cd ~/Downloads/Flect-for-Mac-macos-arm64 && shasum -a 256 -c *.sha256`（`OK` と出る）。
5. `.dmg` を開いて `Flect-for-Mac.app` を Applications にドラッグ、または `.zip` を展開して `~/Applications` へ。
> `ci.yml` が付ける `Flect-for-Mac-adhoc-build` は、`.app` フォルダをそのまま上げたもので、ダウンロード時に実行権限が落ちることがあるため使わない。

**方法 2: 自分でビルド**（Xcode Command Line Tools と Rust が必要。[11 の 0 章](11-mac-verification-guide.ja.md)）
```bash
git clone https://github.com/whitewater-png/Flect-fork-for-Mac.git && cd Flect-fork-for-Mac
git checkout claude/exciting-brown-3b2qqy
scripts/build.sh --install && killall Finder
```

---

## B. 自動確認（`scripts/mac-verify.sh`）

リポジトリを取得してある場合（方法 2、または `git clone` だけして）:
```bash
cd Flect-fork-for-Mac            # 既に clone 済みのフォルダ
git checkout claude/exciting-brown-3b2qqy && git pull
scripts/mac-verify.sh
```
- 何もインストールしない（足りないものは `brew` のコマンドを表示するだけ）。外部のサイトへは接続しない（ダウンロードの確認は `127.0.0.1` のローカルサーバーだけを使う）。
- `~/FlectTest` にテスト用のファイル（動画・画像・テキスト・JSON・重複ファイル・空フォルダ・PDF 2 つ）を作る。
- 終わったら `scripts/mac-verify.sh --clean` で `~/FlectTest` を消せる。

| # | 確認 | 期待する結果 | 結果 |
| --- | --- | --- | --- |
| B-1 | 「1. Environment」の表 | macOS・チップ・各ツールのパスとバージョンが出る（**この表をそのまま F の報告に貼る**）。`Quick Actions installed` が 41 | |
| B-2 | 「2. Test files」 | `sample.mp4 photo.png sample.txt sample.json dup empty1 five-pages.pdf two-pages.pdf original-five-pages.pdf` が並ぶ | |
| B-3 | 同じ章の「PDF check」 | `five-pages.pdf: 5 page(s)` と、各ページの `text:` が `=== PAGE n of 5 ===`。出ない（`could not make the sample PDFs`）ときは、5 ページ以上の PDF を `five-pages.pdf` という名前で `~/FlectTest` に置く | |
| B-4 | 「3. Web download, end to end」 | 最後に `-> download check: ALL PASSED`（`26 of 26 checks passed` など、全項目が PASS） | |
| B-5 | B-4 が失敗したとき | `FAIL` と書かれた行を**そのまま**報告（どの項目が落ちたかで原因が分かる） | |

B-4 が確認していること: 設定オフで拒否される／`file://` や許可されていない画質は実行前に拒否される／動画・音声(MP3)が保存される／同じ URL を再取得しても**既存ファイルは触られず、新規として報告されない**（= 「元に戻す」の対象にならない）／音声の取得が同じ URL の動画を消さない／途中のファイルが残らない／遅いサーバーからの取得を**キャンセルすると数秒で止まり、`yt-dlp` や `ffmpeg` のプロセスが残らない**／エンジンが固定フォルダ（`/opt/homebrew/bin`）から外部ツールを見つける。

---

## C. PDF のページ操作

準備: `~/FlectTest/five-pages.pdf`（5 ページ、各ページに `PAGE n of 5`）。比較用の無傷のコピーが `original-five-pages.pdf`。
操作は **Finder で右クリック → クイックアクション → `Flect: PDF …`**、または **`Flect-for-Mac.app` にドロップ / ファイル → 開く... → 一覧から選ぶ**。どちらでも同じ結果になる。
結果の PDF の中身は **プレビューで開いて**確認する（`swift scripts/mac-verify/pdf-info.swift ファイル.pdf` を使うと、枚数と各ページの大きさ・回転・先頭の文字がまとめて見える）。

| # | 操作 | 期待する結果 | 結果 |
| --- | --- | --- | --- |
| C-1 | `five-pages.pdf` 1 つを右クリック | クイックアクションに `Flect: PDF Split`・`Flect: PDF Rotate`・`Flect: PDF Delete pages...` が出る（ほかに Preview・Convert 系）。2 つの PDF を選ぶと `Merge` も出る | |
| C-2 | 分割 → **1 ページずつ** | 進捗ウィンドウ → `five-pages_p1.pdf` … `five-pages_p5.pdf` の 5 ファイル。それぞれ 1 ページで、`PAGE 1`…`PAGE 5` の順 | |
| C-3 | 分割 → **ページを抜き出す** → `1-2, 5` | ダイアログに説明と例（`1-3, 5, 8-`）が出る。実行すると `five-pages_pages.pdf` が 1 つできて、**3 ページ（PAGE 1・2・5 の順）** | |
| C-4 | 同じダイアログで `0`、`3-2`、`abc`、空欄を入れる | 「ページ番号か範囲をカンマで区切って…」と出て、**実行ボタンが押せない** | |
| C-5 | 同じダイアログで `9`（5 ページを超える）を入れて実行 | 「指定したページがこの文書（5 ページ）に合いません」のエラー。**ファイルは作られない** | |
| C-6 | 全角で `１－３、５` を入力 | 実行できて、`PAGE 1・2・3・5` の 4 ページの PDF ができる（`、` `－` `，` `；` も使える） | |
| C-7 | 回転 → **右へ 90°** | `five-pages_rot90.pdf`。プレビューで開くと**全ページが時計回りに 90°** 回っている。ページ数は 5 のまま | |
| C-8 | 回転 → **180°** と **左へ 90°** | `five-pages_rot180.pdf`（上下逆）、`five-pages_rot270.pdf`（反時計回りに 90°）。**向きが逆なら × として報告** | |
| C-9 | **ページを削除** → `2, 4-` | `five-pages_removed.pdf`。**2 ページ（PAGE 1 と PAGE 3）** | |
| C-10 | ページを削除 → `1-5`（全ページ） | 「…すべてのページを削除すると何も残らないため、ファイルを作成しませんでした」。ファイルは作られない | |
| C-11 | 元のファイルが変わっていない | `shasum -a 256 ~/FlectTest/five-pages.pdf ~/FlectTest/original-five-pages.pdf` の 2 つが**同じ値** | |
| C-12 | 同じ操作（例: 回転 90°）をもう一度 | 上書きされず、`five-pages_rot90 (2).pdf` のように別名で増える | |
| C-13 | `five-pages.pdf` と `two-pages.pdf` を**両方選んで**「ページを抜き出す」→ `1-3` | `five-pages` は 3 ページできる。`two-pages` は 2 ページしかないのでエラー表示。**片方の失敗でもう片方の結果は消えない**（実行全体は失敗扱いで終わるが、作れた分のファイルは残り、結果のウィンドウに出る） | |
| C-14 | パスワード付き PDF を用意して回転する（プレビューで開く → ファイル → 書き出す → 「暗号化」にチェックして、**開くためのパスワード**を設定） | 「この PDF は暗号化されているため…」のエラー。ファイルは作られない | |
| C-15 | 結果のウィンドウの **「元に戻す」** | **今回できたファイルだけ**がゴミ箱へ移動する。`five-pages.pdf` などの元ファイルは残る | |
| C-16 | 設定 → 出力先 → **PDF** にフォルダ（例 `~/FlectTest/out`）を入れて保存し、C-2 を実行 | その フォルダに出力される。空欄に戻すと元ファイルの隣に戻る | |
| C-17 | ファイル → 最近の実行... | 実行した項目が「分割」「回転」「ページを削除」などの名前で並ぶ | |
| C-18 | （任意）501 ページ以上の PDF で「1 ページずつ」 | 「500 ページを超えています。…『ページを抜き出す』を使ってください」。ファイルは作られない | |

---

## D. Web ダウンロード

> **権利のあるコンテンツだけで確認する。** この章の標準は、自分の Mac の `127.0.0.1` で配るサンプル動画（`~/FlectTest/sample.mp4`）で、外部のサイトには触れない。
> 実サイトでの確認（D-16）は、**自作の動画か、再配布が許されたライセンス（CC など）のもの**だけで行う。
>
> ローカルサーバーの起動（別のターミナルで。Ctrl-C で止める）:
> - 速い: `python3 scripts/mac-verify/slow_server.py --fast`　→ URL は `http://127.0.0.1:8000/sample.mp4`
> - わざと遅い（キャンセルの確認用・約 40 KB/s）: `python3 scripts/mac-verify/slow_server.py`

| # | 操作 | 期待する結果 | 結果 |
| --- | --- | --- | --- |
| D-1 | **ファイル → URL からダウンロード...**（⌘D）。設定はまだオフのまま | 「Web ダウンロードはオフです。設定の『ダウンロード』でオンにして保存してから、もう一度お試しください」 | |
| D-2 | 設定 → **ダウンロード** タブを開く | 「保存する権利のあるコンテンツだけを…利用の責任はご自身にあります」の注意書きとチェックボックス。**既定はオフ** | |
| D-3 | yt-dlp が**未導入**のままチェックをオンにして保存 → ⌘D | 「yt-dlp がインストールされていません。Terminal で次のコマンドを実行してから…: `brew install yt-dlp`」。導入済みなら `brew uninstall yt-dlp` で試してから戻す（面倒なら － でよい） | |
| D-4 | 設定 → **必要なツール** タブ | `yt-dlp` が**任意**として並び、未導入／インストール済みが正しく出る。コマンドをコピーでき、「再確認」で状態が変わる | |
| D-5 | yt-dlp を入れ（`brew install yt-dlp`）、⌘D | ダイアログが開く。URL 欄、画質（最高画質／1080p まで／720p まで／480p まで／音声のみ（MP3））、**注意書きが常に見える** | |
| D-6 | URL 欄に `ftp://x`、`example.com/v`、空欄、途中にスペースの入った URL を入れる | **「ダウンロード」ボタンが押せない**。`http://127.0.0.1:8000/sample.mp4`（http か https で始まる）を入れると押せる | |
| D-7 | 速いサーバーの URL で **最高画質** → ダウンロード | 進捗が動いて完了。**`~/Downloads/sample [sample].mp4`** ができる。結果のウィンドウに 1 ファイル。ターミナルの `ls ~/Downloads` に `.part` が無い | |
| D-8 | 同じ URL で **音声のみ（MP3）** | `~/Downloads/sample [sample] (audio).mp3` ができる。**D-7 の `.mp4` が残っている**（消えていない） | |
| D-9 | D-8 の結果のウィンドウで **「元に戻す」** | ゴミ箱に行くのは**直前の実行で作った `(audio).mp3` だけ**。D-7 の `.mp4` は残る | |
| D-10 | D-7 と同じ URL・同じ「最高画質」でもう一度 | 「…はすでに保存先にあったため、そのままにしました」の警告。**結果のウィンドウに新しいファイルは 0 個**（= 「元に戻す」の対象が無い）。`.mp4` の中身・更新日時は変わらない | |
| D-11 | ターミナルで `cp ~/FlectTest/sample.mp4 ~/FlectTest/slow1.mp4`。速いサーバーを止めて**遅いサーバー**を起動（同じポート 8000。上の「ローカルサーバーの起動」）。`http://127.0.0.1:8000/slow1.mp4` をダウンロード開始し、進捗が出たら **キャンセル**（別名にするのは、同名のファイルが既にあると取得が省略されてキャンセルを試せないため） | 数秒で止まり、キャンセルされた表示になる。`~/Downloads` に `slow1` で始まるファイルや `.part` が**無い**。`pgrep -fl 'yt-dlp\|ffmpeg'` が**何も出さない** | |
| D-12 | 設定 → 出力先 → **Web ダウンロード** に `~/FlectTest/dl` を入れて保存し、D-7 を実行 | `~/FlectTest/dl` に保存される。空欄に戻すと `~/Downloads` に戻る | |
| D-13 | 設定のチェックを**オンにしただけで保存せずに**⌘D → ダウンロード | 「Web ダウンロードは設定でオフになっています」（既知の仕様: エンジンは**保存済み**の設定を読む）。保存してから再度試すと動く | |
| D-14 | 日本語・English・中文に切り替えて、ダイアログ・メニュー・メッセージを見る | 3 言語とも文言が出る（英語のままの欄や `download.…` のようなキー名の表示が**無い**） | |
| D-15 | ファイル → 最近の実行... | ダウンロードの実行が「ダウンロード」の名前で並ぶ | |
| D-16 | （任意・実サイト）**自作または CC の動画 1 本**で、最高画質と 480p まで | mp4 で保存される。`ffprobe -v error -select_streams v:0 -show_entries stream=height -of csv=p=0 ファイル.mp4` が、480p 指定のとき **480 以下** | |
| D-17 | （任意）ffmpeg が無い状態 | 「動画と音声の結合に ffmpeg が必要です…」。**ffmpeg を外すのが面倒なら － でよい**（`brew uninstall` は他の機能に影響するので注意） | |

---

## E. 既存機能への影響（抜き取り）

| # | 操作 | 期待する結果 | 結果 |
| --- | --- | --- | --- |
| E-1 | 画像 `photo.png` → リサイズ 50% | 縮小された画像ができる。元は変わらない | |
| E-2 | `two-pages.pdf` と `five-pages.pdf` を選んで **結合** | 順序ダイアログ → 7 ページの `…_merged.pdf` | |
| E-3 | PDF → **PNG**（画像化） | ページごとの画像ができる | |
| E-4 | `sample.mp4` → 変換 → MOV（または圧縮） | 進捗が動いて完了。キャンセルも効く | |
| E-5 | `dup` フォルダ → 重複を削除 | 結果の確認 → ゴミ箱へ移動（完全削除ではない） | |
| E-6 | 設定の全タブ（一般／フォルダ／画像／動画／テキスト／出力先／OCR／**ダウンロード**）を開く | すべて表示できる。出力先に「PDF」「Web ダウンロード」の欄がある | |
| E-7 | アプリを終了して起動し直す | 設定（ダウンロードのオン／オフ、出力先）が保たれている | |
| E-8 | `ls -l ~/Library/Application\ Support/Flect-for-Mac/settings.json` | `-rw-------`（0600） | |

---

## F. 報告

下をコピーして埋め、このやり取りに貼る。△ と × は、**画面の文言・実行した手順・ターミナルの出力**を添える（スクリーンショット可）。

```
対象コミット: ______    入手方法: Release の成果物 / 自分でビルド
環境: macOS ___ / ___ (Apple M_) / ffmpeg ___ / yt-dlp ___   ← mac-verify.sh の表を貼ってもよい
A: A-1 _ A-2 _ A-3 _ A-4 _ A-5 _ A-6 _
B: B-1 _ B-2 _ B-3 _ B-4 _        (B-4 が × なら FAIL の行を貼る)
C: C-1 _ C-2 _ C-3 _ C-4 _ C-5 _ C-6 _ C-7 _ C-8 _ C-9 _ C-10 _ C-11 _ C-12 _ C-13 _ C-14 _ C-15 _ C-16 _ C-17 _ C-18 _
D: D-1 _ D-2 _ D-3 _ D-4 _ D-5 _ D-6 _ D-7 _ D-8 _ D-9 _ D-10 _ D-11 _ D-12 _ D-13 _ D-14 _ D-15 _ D-16 _ D-17 _
E: E-1 _ E-2 _ E-3 _ E-4 _ E-5 _ E-6 _ E-7 _ E-8 _
気づいたこと:
- 
```

---

## G. 困ったとき

| 症状 | 確認すること |
| --- | --- |
| 「開発元を確認できません」と出て開けない | `xattr -dr com.apple.quarantine ~/Applications/Flect-for-Mac.app`、または右クリック → 開く |
| 右クリックに `Flect: PDF …` が出ない | クイックアクションを**もう一度インストール**（古い版は 38 個で、PDF の 3 項目が無い）。`killall Finder`。システム設定 → ログイン項目と機能拡張 → Finder で Flect の項目がオンか。`ls ~/Library/Services \| grep -c '^Flect '` が 41 か |
| ⌘D で何も起きない／別のダイアログが出る | アプリを前面にして、メニューの「ファイル」に「URL からダウンロード...」があるか。無ければ古い版（A-2 で入手し直す） |
| 「yt-dlp が見つからない」と言われる | `ls /opt/homebrew/bin/yt-dlp`（Intel Mac は `/usr/local/bin/yt-dlp`）。Flect は**この 2 つのフォルダだけ**を探す（`PATH` は見ない）。`which yt-dlp` が別の場所を指しているなら、`brew install yt-dlp` で入れ直す |
| ダウンロードが「失敗しました: ERROR: …」 | yt-dlp のメッセージがそのまま出ている。`yt-dlp --version` を報告し、可能ならターミナルで `yt-dlp -- <URL>` を試した結果も添える。古い yt-dlp は `brew upgrade yt-dlp` で直ることが多い |
| `mac-verify.sh` が `PDF ... could not make` と言う | `cupsfilter` が使えない環境。手持ちの 5 ページ以上の PDF を `~/FlectTest/five-pages.pdf` として置く |
| C・D の途中で状態が分からなくなった | `scripts/mac-verify.sh --clean` で `~/FlectTest` を消し、もう一度 `scripts/mac-verify.sh`。`~/Downloads` に作ったサンプルは手で消す |

---

## 付録: よく使うコマンド

```bash
# PDF の枚数・各ページの大きさ・回転・先頭の文字
swift scripts/mac-verify/pdf-info.swift ~/FlectTest/five-pages_pages.pdf
# 元のファイルが変わっていないか
shasum -a 256 ~/FlectTest/five-pages.pdf ~/FlectTest/original-five-pages.pdf
# yt-dlp / ffmpeg のプロセスが残っていないか（何も出なければ OK）
pgrep -fl 'yt-dlp|ffmpeg'
# 動画の高さ（画質の上限の確認）
ffprobe -v error -select_streams v:0 -show_entries stream=height -of csv=p=0 "$HOME/Downloads/…mp4"
# ダウンロードの通し確認だけを、別のエンジンで試す
python3 scripts/mac-verify/download_e2e.py ~/Applications/Flect-for-Mac.app/Contents/Resources/flect-engine
```
