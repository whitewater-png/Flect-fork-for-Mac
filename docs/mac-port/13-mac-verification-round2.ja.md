# Flect-for-Mac — 13 Mac 実機の確認手順（第 2 回）

第 1 回（[11](11-mac-verification-guide.ja.md) / 結果 [12](12-mac-verification-results.ja.md)）の後に追加・修正したものと、未確認で残っているものの確認手順。
所要時間の目安: 準備 10 分 + 確認 40 分。結果は末尾のテンプレートで報告（○ / △ / × / −）。

## 0. 更新とビルド
```bash
cd ~/Flect-fork-for-Mac            # 実際のフォルダ
git pull origin claude/adoring-fermat-jaa4tq
scripts/build.sh --install
killall Finder
```
**0-A**: `build.sh` が最後まで通る（`built:` と `Quick Actions installed` が出る）。
**0-B**: `cd core && cargo test 2>&1 | tail -5` が `test result: ok`（字幕・背景除去まわりを含む）。

## 1. 設定画面（第 1 回で小さくて切れていた部分の修正確認）
アプリ本体を開く: `open ~/Applications/Flect-for-Mac.app`

| # | 確認 | 期待する結果 |
| --- | --- | --- |
| 1-1 | 初期サイズ | 内容が切れず、下のボタン（初期化 / 保存 / 閉じる）が見える |
| 1-2 | タブ | 一般・フォルダ・画像・動画・テキスト・OCR・メニュー表示、の各タブが開く |
| 1-3 | ラベルの区別 | 画像/動画タブの「リサイズ」「圧縮」「回転」に A / B / C / D が付いて区別できる |
| 1-4 | 値の保存 | 例: 画像のリサイズ A を 80 に変更 →「保存」→ アプリを閉じて開き直しても 80 のまま |
| 1-5 | プリセットの反映 | 1-4 の後、画像を右クリック → リサイズ → 先頭の項目が **80%** と表示される |
| 1-6 | ファイル権限 | `ls -l ~/Library/Application\ Support/Flect-for-Mac/settings.json` が `-rw-------` |
| 1-7 | 言語切替 | 一般 → 言語を English にして保存 → 再起動で英語表示に変わる（確認後に元へ戻す） |
| 1-8 | メニュー表示 | 「メニュー表示」タブで項目（例 `image.rotate.p1`）のチェックを外して保存 → 画像を右クリック → 回転 の選択画面から消える |
| 1-9 | 初期化 | 「初期化」→ 確認 → 保存 で既定値に戻る |
| 1-10 | 範囲外の値 | 画像 WebP 品質に 999 を入れる → 100 に丸められる。文字を入れても壊れない |

## 2. 字幕（ffmpeg の whisper フィルタ）
```bash
ffmpeg -filters | grep whisper                  # 行が出る（ffmpeg-full）
scripts/fetch-whisper-model.sh tiny             # 約 75MB。進捗の後に "installed: ..." と出る
shasum -a 256 ~/Library/Application\ Support/Flect-for-Mac/deps/whisper/ggml-tiny.bin
# → be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21 と一致
```
設定 → 動画 → 字幕のモデルを **Tiny** にして保存してから試す。

| # | 操作 | 期待する結果 |
| --- | --- | --- |
| 2-1 | 声の入った動画を右クリック → 字幕 | 進捗が動き、同じフォルダに `名前.srt` ができる。内容が音声と概ね合う |
| 2-2 | 音声ファイル（m4a / mp3）で同様 | `.srt` ができる |
| 2-3 | 出力を VTT にして再実行 | `名前 (2).vtt` ではなく `名前.vtt` ができ、先頭行が `WEBVTT`、時刻が `00:00:01.500` 形式 |
| 2-4 | 出力を TXT にして再実行 | プレーンテキストになる |
| 2-5 | 言語を `ja`（日本語音声）/ `en` に固定 | その言語で出る。空欄（自動）でも判定される |
| 2-6 | `ja;rm` のような不正な言語を設定して実行 | エラーで止まる（コマンドとして解釈されない） |
| 2-7 | モデルを Small に変えて**未インストールのまま**実行 | 「音声認識モデル ggml-small.bin がありません。scripts/fetch-whisper-model.sh を…」と出る |
| 2-8 | モデルファイルを壊す: `echo x >> ~/Library/Application\ Support/Flect-for-Mac/deps/whisper/ggml-tiny.bin` して実行 | 「チェックサムが想定と一致しないため使用しませんでした」と出て、実行されない（確認後 `fetch-whisper-model.sh tiny` で入れ直す） |
| 2-9 | 無音の動画（`ffmpeg -f lavfi -i anullsrc -t 5 -c:a aac silent.m4a`） | 「音声が見つかりませんでした」。`.srt` が空で残らない |
| 2-10 | 実行中にキャンセル | 数秒で止まり、`.srt` や作業フォルダが残らない |
| 2-11 | 速度 | 1 分の音声が何秒で終わったか（Tiny / Small で）を報告 |
| 2-12 | GPU/CPU | 設定のデバイスを「CPU のみ」にして 2-1 を再実行。結果が出る（速度の差を報告） |

## 3. 背景除去（Vision。AI）
人物や物が写った画像を用意（`~/FlectTest/` に）。

| # | 操作 | 期待する結果 |
| --- | --- | --- |
| 3-1 | 画像 → 背景を削除（設定は既定 = AI、失敗時は色キー） | `名前_nobg.png`（色キーに切り替わった画像は `名前_keyed.png`）ができ、被写体以外が透明（プレビューで市松模様） |
| 3-2 | 複数枚を選んで実行 | すべて処理される |
| 3-3 | 設定 → 画像 → 方式を「色キーのみ」にして、緑背景の画像で実行 | 従来どおり色キーで透明になる |
| 3-4 | 方式を「AI のみ」にして、被写体のない画像（真っ白など）で実行 | 「前景の被写体が見つかりませんでした」と出て失敗する |
| 3-5 | 方式を「AI + 失敗時は色キー」にして 3-4 と同じ画像 | 色キーに切り替わって処理される（結果は画像次第） |
| 3-6 | 大きな画像（6000×4000 程度） | 落ちずに終わる。所要時間を報告 |
| 3-7 | 出力の確認 | 元ファイルは変わっていない。再実行で `_nobg (2).png` ができる |

## 4. 第 1 回で残った項目
| # | 確認 | 内容 |
| --- | --- | --- |
| 4-1 | JAR（未実施） | `scripts/fetch-cfr.sh`（`java -version` が通ること）→ 手元の `.jar` を右クリック → デコンパイル。出力フォルダにソースができる。`sandbox-exec` が拒否された場合のエラー文面（`msg.sandboxFailed`）を報告 |
| 4-2 | 日本語入り PDF の空白 | 空白になった PDF を Finder のプレビューで開いて表示されるか。`pdffonts` が無ければ省略可。Flect のプレビューで空白、Finder のプレビューで表示できるなら、その PDF を共有してほしい（原因調査用） |
| 4-3 | mkv | `.mkv` を右クリックして動画の項目が出る（修正の再確認）。Finder 拡張でオンにした項目だけ出ることに注意 |
| 4-4 | HEIC / 形式の選択画面 | 「PNG に変換」のように表示される |
| 4-5 | PDF 結合 | 2〜3 個の PDF で、順序を入れ替えて結合 → 指定順のページになる。暗号化 PDF を混ぜるとエラーで止まる |
| 4-6 | 古い Quick Action | `~/Library/Services` に残っている旧 PoC の `.workflow` の名前を教えてほしい（削除は私が指示する） |
| 4-7 | トリミング | 設定で精度を「正確」にして実行 → 指定した長さにほぼ一致する |

## 5. セキュリティの確認（OSS 公開前の最終チェックの一部）
テスト用ファイルを作って、**安全に拒否される**ことを確認する。Mac の Terminal で:
```bash
mkdir -p ~/FlectTest/sec && cd ~/FlectTest/sec
python3 - <<'PY'
import zipfile
# ../ を含むエントリ（Zip Slip）と、絶対パスのエントリ
with zipfile.ZipFile("slip.zip", "w") as z:
    z.writestr("../escaped.txt", "x")
    z.writestr("/tmp/flect-abs.txt", "x")
    z.writestr("ok/a.txt", "x")
# 解凍すると巨大になるファイル（約 1GB のゼロ）
with zipfile.ZipFile("bomb.zip", "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("zeros.bin", b"\0" * (1 << 30))
PY
ls -l
```
| # | 操作 | 期待する結果 |
| --- | --- | --- |
| 5-1 | `slip.zip` を右クリック → 展開 | `~/FlectTest/escaped.txt` と `/tmp/flect-abs.txt` が**作られない**。警告が出て、安全なエントリだけ展開される（または全体が拒否される） |
| 5-2 | `bomb.zip` を展開 | 制限に達して中断され、ディスクが埋まらない。作業フォルダが残らない |
| 5-3 | シンボリックリンクを含む tar: `ln -s /etc/passwd link; tar cf sym.tar link` を展開 | リンクが作られず、警告が出る |
| 5-4 | ファイル名に特殊文字（`a b'c;d.png`、先頭が `-` の `-x.png`）で画像変換 | 正常に処理される |
| 5-5 | 通信しないこと | 処理中に別ターミナルで `lsof -i -P \| grep -i flect` | 何も出ない（Flect は通信しない） |
| 5-6 | 署名の状態 | `codesign -dv ~/Applications/Flect-for-Mac.app 2>&1 \| head -5` に `Signature=adhoc` |
| 5-7 | 元ファイルの保全 | 任意の変換の前後で、元ファイルの `shasum -a 256` が変わらない |
| 5-8 | 重複削除の安全性 | 内容が違うが先頭とサイズが同じファイル 2 つ（`head -c 1000000 /dev/zero > a; cp a b; printf x >> b; printf y >> a`）で、設定を「先頭ハッシュのみ」にしても**バイト比較で別物と判定**される（ごみ箱に行かない） |

## 6. 性能の記録（任意。最適化の判断材料）
| 項目 | 条件 | 時間 |
| --- | --- | --- |
| 画像リサイズ | 12MP の JPG 100 枚を 50% | ___ 秒 |
| 重複検出 | 約 1 万ファイルのフォルダ | ___ 秒 |
| 動画圧縮 | 1080p 60 秒、ソフト / VideoToolbox | ___ / ___ 秒 |
| 字幕 | 1 分の音声（Tiny / Small） | ___ / ___ 秒 |

## 報告テンプレート
```
環境: macOS ___ / ffmpeg ___ (ffmpeg-full?) / モデル: ___
0-A ○ 0-B ○
1-1 ○ 1-2 ○ ... 1-10 ○
2-1 ○ ... 2-12 ○
3-1 ○ ... 3-7 ○
4-1 − 4-2 ○ ... 4-7 ○
5-1 ○ ... 5-8 ○
6: 画像 __ 秒 / 重複 __ 秒 / 動画 __ 秒 / 字幕 __ 秒
気づいたこと:
- 
```
結果は `docs/mac-port/14-mac-results-round2.ja.md` にして push してもらうか、チャットに貼ってほしい。エラーは画面の文言かスクリーンショットをそのまま。
