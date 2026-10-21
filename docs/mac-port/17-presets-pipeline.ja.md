# Flect-for-Mac — 17 プリセット・パイプライン・お気に入り（設計メモ）

更新: 2026-10-21 ／ 実装: `core/flect-core/src/{pipeline,presets}.rs`, `core/flect-engine/src/main.rs`, `core/flect-cli/src/{args,main}.rs`, `app/Sources/FlectCore/{Pipeline,Presets,PickerMemory}.swift`, `app/Sources/FlectApp/PresetsView.swift`

3 つの機能は 1 つのデータモデルで表す: **「ステップ」= 1 つのタスク文字列**（`image.resize?scale=50` のような、エンジンの `run.task` と同じ形）。

| 機能 | モデル |
| --- | --- |
| D1 プリセット | 名前付きの **ステップ列**（1〜8 個）。1 個なら従来の `run`、2 個以上ならパイプライン |
| D2 パイプライン | ステップ列をエンジンの新しい op `pipeline` で 1 回に実行 |
| D3 お気に入り／もう一度 | 参照（`task:<カタログ ID>` か `preset:<UUID>`）の短い列と、直前の操作 1 件 |

例の「SNS 1080px / 2MB」は `["image.resize?scale=custom&w=1080&h=1080&keep=1", "image.compress?bytes=2097152"]`（＝ 2 ステップのパイプライン）。

## ステップの文法（Rust `pipeline::parse_step` と Swift `PipelineStep.parse` が同じ規則）
- `名前` または `名前?k=v&k=v`。1 ステップ 1024 バイトまで。キーの重複・未知のキー・空の値は拒否。値は `[A-Za-z0-9.-]` のみ（パーセントエンコード不要な値しか許さない）。
- 使えるタスク（ダイアログも OS 機能も要らない、1 ファイル → 1 ファイルのもの）と必須パラメータ:

| タスク | 入力 → 出力 | パラメータ |
| --- | --- | --- |
| `image.resize` / `video.resize` | 画像→画像 / 動画→動画 | `scale=1..1000`（%） または `scale=custom&w=0..32768&h=0..32768[&keep=0\|1]`（w,h 両方 0 は不可） |
| `image.rotate` / `video.rotate` | 同上 | `deg=-360..360` |
| `image.compress` | 画像→画像 | `bytes=1..2^40` |
| `video.compress` | 動画→動画 | `bytes=...` か `preset=discord` のどちらか一方 |
| `image.convert` | 画像→画像 | `to=PNG\|JPG\|WEBP\|ICO\|GIF\|SVG`（HEIC はアプリの ImageIO なので不可） |
| `image.strip` / `image.chromakey` | 画像→画像 | なし（chromakey はエンジンの色キー。AI 切り抜きはアプリ側なので対象外） |
| `video.convert` | 動画→動画（`M4A`→音声、`GIF`→画像） | `to=MP4\|MOV\|MKV\|M4A\|AVI\|WEBM\|FLV\|GIF` |
| `video.extractAudio` | 動画→音声 | `to=MP3\|M4A\|WAV\|AIF\|AIFF\|AAC\|OGG\|WMA` |
| `audio.convert` | 音声・動画→音声 | 同上 |
| `text.convert` / `text.encode` / `text.lineEndings` / `json.pretty` / `json.sort` | テキスト→テキスト | `to=`（既存の値）／ なし |

- 対象外: フォルダ系・アーカイブ・JAR（出力がフォルダ）、PDF・OCR・HEIC・AI 切り抜き（アプリ側の OS 機能）、`video.trim`/`thumbnail`/`subtitles`（プレビューや重い前提が要る）、`pdf.merge`（複数→1）。
- **連結の検査**: ステップ k の出力の種類（画像/動画/音声/テキスト）が、ステップ k+1 の受け付ける種類に含まれること。最初のステップは実行時にファイルごとに `menu::classify` で入力の種類を確かめ、合わなければそのファイルだけ `msg.pipelineWrongInput` で失敗（他のファイルは続行）。

## D2 エンジン: op `pipeline`（アプリ側オーケストレーションは採らない）
`{"op":"pipeline","steps":["image.resize?scale=50","image.convert?to=JPG","image.strip"],"paths":["/abs/a.png"]}`

アプリ側でエンジンを 3 回呼ぶ案は、中間ファイルが元ファイルの隣（または出力フォルダ）に一度書かれてしまい、消す処理がアプリに要る（＝削除のコードがアプリに増える）、キャンセルや異常終了で中間ファイルが残る、`outputs` に中間ファイルが混ざり Undo の対象が増える、CLI でも同じ処理を書き直す必要がある、という理由で不採用。エンジンの 1 リクエストにすれば CLI もアプリも同じ動きになる。

- `paths` の制限は `run` と同じ。`steps` は 1〜8 個、各 1024 バイト以内（超えると `protocol.badPipeline`）。中身の検査（上の文法と連結）は実行前に行い、失敗なら何も書かずに `error` + `done(failed)`。キー: `msg.pipelineStepNotAllowed` [ステップ]（対象外のタスク）、`msg.pipelineBadStep` [ステップ]（文法・パラメータ違反）、`msg.pipelineIncompatible` [前, 次]（種類が繋がらない）、`msg.pipelineStepCount` [上限]。
- **中間ファイル**: 実行ごとに `$TMPDIR/Flect-for-Mac-pipeline-<一意>` を `mkdir(0700)` で**排他的に作成**（既にあれば失敗＝他人が先に置いたものは使わない）。ファイルごとにその下へ `0700` のサブフォルダ。各中間ステップは設定の出力フォルダ（全項目）をこの絶対パスに差し替えて既存タスクをそのまま実行する。最後のステップだけ、出力フォルダを「**元ファイル**と最後のタスクの出力フォルダ設定」から求めた絶対パスにする。したがって最終出力は従来どおり元ファイルの隣（または設定の出力フォルダ）に、既存の ` (2)` 規則（`commit_new` のハードリンク／排他作成）で書かれ、既存ファイルは上書きされない。
- 掃除: 作業フォルダは Drop で削除（成功・失敗・キャンセル・panic 巻き戻しのいずれでも）。ファイルごとのサブフォルダはそのファイルが終わった時点で削除（動画でディスクを食い続けない）。強制終了で残ったものは、次回のパイプライン開始時に「接頭辞が一致・実ディレクトリ（シンボリックリンクでない）・自分が作った作業フォルダと同じ所有者・24 時間以上前」のものだけ削除。
- 名前: 中間ファイルの名前は各タスクの接尾辞が積み重なる（`photo.png` → `photo_resized.png` → `photo_resized.jpg` → 最終 `photo_resized_clean.jpg`）。どの処理を経たかが名前から分かる。
- 途中のステップが入力をそのまま返した場合（例 `image.compress` の `msg.alreadySmall`）は次のステップにそのまま渡す。最後のステップの結果が作業フォルダ内にある場合（＝最後が素通り）は、作業フォルダから最終の場所へ同じ規則で書き出す。結果が元ファイルそのもの（全ステップが素通り）なら何も作らず `outputs` も送らない（`run` と同じ）。
- イベント: ファイルごとに `status(msg.processing)`、ステップごとに `status(msg.pipelineStage, [i, n])`、`file_step` はステップ内の進捗、`step` はファイル単位。`outputs` は**最終出力だけ**（ファイル 1 件ごと）。途中ステップの `info`/`warn` はそのまま流す（中間ファイル名が出ることがある）。
- キャンセル: ステップの間とファイルの間で確認し、実行中のタスクは既存どおりフラグを見て止まる。`done(cancelled)`。作業フォルダは消える。元ファイルは一切変更しない（読むだけ）。
- 注意（トレードオフ）: 中間ファイルは起動ディスクの `$TMPDIR` に置く。外付けディスク上の大きな動画を連結すると起動ディスクの空きを使う。代わりに「出力先の隣の隠しフォルダ」に置けば容量の問題は消えるが、利用者のフォルダに一時物を作り、権限も出力先フォルダ次第になる。今回は「非公開の一時フォルダ」を優先した。

## D1 プリセットファイル `<データフォルダ>/presets.json`
```json
{"version":1,"presets":[{"id":"6F1C…(UUID)","name":"SNS 1080px / 2MB","steps":["image.resize?scale=custom&w=1080&h=1080&keep=1","image.compress?bytes=2097152"]}]}
```
- `FLECT_HOME` に従う（`settings.json` と同じフォルダ）。書き込みはアプリだけ: 同じフォルダに `0600` の一時ファイルを排他作成 → `rename`（原子的）。CLI とエンジンは読むだけ。
- **読み込みは信用しない入力として全項目を検査**（Rust `presets::parse` と Swift `PresetFile.decode` が同じ規則。`core/flect-core/tests/fixtures/presets/` の正例・負例を両方のテストが読む）: 256 KiB 以内・UTF-8・通常ファイル（FIFO などは開かない）／ トップレベルは `version` と `presets` だけ（未知のキーは拒否）／ `version` は 1（それより新しいファイルは「新しい版で作られた」として**読み取り専用**扱い。アプリは保存を拒否し、上書きしない）／ 100 件まで／ `id` は UUID・重複不可／ `name` は前後の空白なし・1〜64 文字・制御文字や双方向制御・ゼロ幅文字を含まない・大文字小文字を無視して重複不可／ `steps` は 1〜8 個で、上の文法と連結検査に通ること。1 件でも不正ならファイル全体を不正とする（部分的に読んで保存すると、読めなかった分が消えるため）。
- 不正なファイルは**消さない・上書きしない**。アプリは一覧を空として表示し、利用者が新たに保存したときに限り、元のファイルを `presets.json.broken`（既にあれば `presets.json.broken-<日時>`）へ改名してから書く（`settings.json` の退避と同じ考え方）。
- 実行経路: アプリのピッカー（選択に合うプリセットを表示）、`flect run --preset NAME <paths...>`（名前は完全一致、なければ ID）、Quick Action からは `--task preset:<UUID>`（既存の `--task/--input` 契約のまま。プリセット用の Quick Action の自動生成は今回は行わない）。どの経路もプリセットの各ステップを実行直前にもう一度検査する。

## D3 お気に入り・もう一度 `<データフォルダ>/picker.json`
```json
{"version":1,"favorites":["task:image.strip","preset:6F1C…"],"last":"image.resize?scale=50"}
```
- 純粋なロジックは `FlectCore/PickerMemory.swift`（テストあり）。お気に入りは 6 件まで・重複なし・順序あり。`task:` はエンジンのカタログにある ID（実行は `Planner.resolve` 経由、Quick Action と同じ）、`preset:` は存在するプリセットのみ表示。
- 「もう一度」: 直前の操作 1 件。記録するのは (a) 上の文法に通るステップ文字列、(b) `?` を含まないカタログ ID（`Planner` で解決し直す。ダイアログが要るものは再びダイアログ）、(c) `preset:<UUID>`。確認後の破壊的な 2 回目（`folder.removeDuplicate?mode=all` など）は (a)(b) に当たらないので**記録されない**。読み込み時も同じ検査を通し、通らない値は捨てる。
- 表示: 新しい選択の中に、その操作が適用できる区分（カテゴリが一致し、その区分の候補に同じタスクがある）があるときだけ、ピッカーの先頭に「もう一度」と「ピン留め」を出す。

## 文字列
画面の文字列は `strings.ux.{en,ja,zh}.json` の `ux.presets.*` / `ux.picker.*`。エンジンのキー（`msg.pipeline*`, `protocol.badPipeline`）は `strings.{en,ja,zh}.json`。
