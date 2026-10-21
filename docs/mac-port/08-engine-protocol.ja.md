# Flect-for-Mac — 08 エンジンプロトコル（Swift アプリとの契約）

更新: 2026-10-20 ／ 実装: `core/flect-core/src/protocol.rs`, `core/flect-engine/src/main.rs`

`flect-engine` は子プロセスとして起動し、**stdin に 1 行 1 JSON のリクエスト、stdout に 1 行 1 JSON のイベント**を流す。ローカライズはしない（メッセージは `key` + `args`）。環境変数 `FLECT_HOME` でデータフォルダを上書きできる（既定 `~/Library/Application Support/Flect-for-Mac`）。

## 入力の制限（超えると `protocol.*` エラー）
1 行 1 MiB まで／UTF-8／未知フィールドは拒否／`run` の `paths` は 1〜20,000 件、各 4096 バイト以内の**絶対パス**（NUL 不可）。

## リクエスト
| op | 形 | 意味 |
| --- | --- | --- |
| `run` | `{"op":"run","task":"image.resize?scale=50","paths":["/abs/a.png"]}` | タスクを実行。`task` は `ID?key=value&...`（値は `%XX` パーセントエンコード、UTF-8）。**キー名はデコードせず文字どおりに扱い**、`^[A-Za-z][A-Za-z0-9_]*$` に合わない名前（`%63onfirm`・`+confirm`・空白入りなど）や同じ名前の重複があるとタスク全体を `error task.badOption` [task] + `done(failed)` で拒否する（何も実行しない）。`options` は省略可: `jobs`（並列数、下記）、`trashAck`（`true` のとき、アプリが `needs:trash` の各項目に `trash-ack` で答える。エンジンは 1 項目ずつ渡し、答えを待ってから次を確認する。最大 120 秒待ち、`cancel` でも止まる。知らないキーは無視） |
| `pipeline` | `{"op":"pipeline","steps":["image.resize?scale=50","image.convert?to=JPG","image.strip"],"paths":["/abs/a.png"]}` | 各パスに `steps` を順に適用し、**最後の結果だけ**を元ファイルの隣（または最後のタスクの出力フォルダ）に書く。`steps` は 1〜8 個・各 1024 バイト以内（超えると `protocol.badPipeline`）。使えるタスク・パラメータ・種類の連結は `17-presets-pipeline.ja.md`。中間ファイルは `$TMPDIR` の非公開フォルダ（0700）で、`done` の前に消える。`outputs` は最終出力のみ |
| `cancel` | `{"op":"cancel"}` | 実行中のタスクを中断。処理中でも即時に受理され、`done` が `outcome:"cancelled"` で届く。待機中なら何も起きない |
| `export-menu` | `{"op":"export-menu"}` | `menu` イベントを 1 つ返す（`menu.json` と同内容。Quick Action 生成にも使用） |
| `trash-ack` | `{"op":"trash-ack"}` | `ack:true` の付いた `needs:trash` 項目をアプリが処理し終えた（移動した・断った）ことを伝える。待っていないときは無視される |

リクエストは**直列処理**（`run` は 1 つずつ）。

**並列実行（画像・テキストのファイルタスクのみ）**: 独立して CPU を使うタスク（`image.*`、`text.*`、`json.*`）は、複数ファイルのとき最大 `min(コア数, 4)` 本のワーカーで同時に処理する。動画・音声（ffmpeg が既にマルチスレッド）、アーカイブ、フォルダ系は常に 1 本ずつ。本数は `options.jobs`（run リクエスト、1〜16）→ 環境変数 `FLECT_JOBS` → 既定の順で決まり、`1` なら従来どおり直列。イベント（`status` / `data outputs` / `info` / `warn` / `error` / `step`）は**入力順**に 1 か所から出るため、直列実行と同じ並び・単調な `step` になる。`cancel` は新しいファイルの開始を即座に止め、処理中のファイルは終わらせてその出力を報告してから `cancelled` で終わる。出力名は排他的に作成されるため、 ` (2)` の番号が衝突することはない。

## イベント（`"event"` で判別）
| event | 項目 | 意味 |
| --- | --- | --- |
| `status` | `key`, `args[]` | 現在の作業の説明（例 `msg.processing` [ファイル名]） |
| `step` | `done`, `total` | **ファイル単位**の全体進捗 |
| `file_step` | `permille` (0–1000) | 現在のファイル内の進捗（動画のみ） |
| `info` / `warn` / `error` | `key`, `args[]` | 表示用メッセージ。`error` は 1 ファイルの失敗で、バッチは継続する |
| `needs` | `what`, `paths[]`, `items[]`（省略可） | アプリに OS 操作を依頼。現在は `what:"trash"` のみ（`paths` をごみ箱へ移動。アプリが `trashAck` を指定したときは、エンジンは項目ごとにアプリの応答 `{"op":"trash-ack"}` を待つ。指定しない古いアプリや CLI には待たずに送る）。`trash` では項目ごとに確認した直後に 1 件ずつ送り、`items` に `{path, fingerprint}` を付ける。`fingerprint` はファイルなら `f:dev:ino:size:mtime秒.ナノ秒:ctime秒.ナノ秒`、フォルダなら `d:dev:ino`（lstat、リンクは辿らない）。アプリは移動の直前に lstat し、完全一致したものだけ移動する（フォルダはさらに OS のゴミだけかを再確認）。`items` の無い要求・一致しない項目は移動しない（`items` が無いのはアプリより古いエンジン）。重複の項目には `kept: {path, fingerprint}`（残す側のコピー）も付き、アプリは移動の直前にこちらも完全一致を確認する。run で `options.trashAck` を指定したときだけ `ack:true` が付き、エンジンはその項目への `trash-ack` を待ってから次の項目を確認する（アプリ側に溜まる要求は最大 1 件）。指定しないとき（古いアプリ・CLI）は待たずに送る。`items`・`kept`・`ack` は追加フィールドなので、知らない受け手は無視できる。FAT/exFAT の更新時刻は 2 秒単位のため、その間に同じサイズで書き換えられた場合はフィンガープリントでは検出できない（中身の再比較はエンジンが渡す前に行う） |
| `data` | `name`, `data` | 結果の提示用データ（下表） |
| `menu` | `menu` | `export-menu` の応答 |
| `data`（`name:"removal"`） | `data: {items, leftAlone}` | 削除系タスク（Remove Duplicate / Remove Empty）が終わる直前に 1 回。`items` は削除した・ごみ箱へ渡した項目数（ファイル、または中身ごとの計画フォルダ。ゴミ箱でも完全削除でも同じ数え方）、`leftAlone` はそのまま残した項目数。アプリの履歴用 |
| `done` | `outcome` (`succeeded`/`failed`/`cancelled`), `summary_key`, `summary_args[]` | 1 つの `run` の終わり。必ず 1 回届く |

### `data` の種類
| name | 内容 | 発生元 |
| --- | --- | --- |
| `tree` | `{text}` | `folder.tree` |
| `analyze` | 解析レポート（`totalBytes`, `files`, `folders`, `duplicates{groups,files,reclaimableBytes}`, `largest[]`, `byType[]`, `emptyFolders[]`, `scanSeconds`） | `folder.analyze` |
| `duplicates` | `{groups:[[{path,size,modified}...]...], scanSeconds}` | `folder.removeDuplicate`（`mode` 未指定かつ設定が Ask） |
| `empty_folders` | `{total, targets[], permanent}` | `folder.removeEmpty`（`confirm=1` なし） |
| `rename_preview` | `{lines[]}` | `folder.rename`（ドライラン） |
| `duplicates_preview` | `{mode:"oldest"\|"newest"\|"all", groups:[{keep:{path,size,modified}\|null, remove:[{path,size,modified}...]}...], removeCount, reclaimBytes, permanent}` | `folder.removeDuplicate?...&dry=1`（ドライラン。何も移動・削除しない。`mode` 未指定なら設定の Keep ルール。Ask かつ `mode` 未指定のときは従来どおり `duplicates`） |
| `privacy_report` | `{path, file, format, personal, items:[{kind, level:"high"\|"medium", detail}...]}`（ファイル 1 件につき 1 回） | `image.privacy`（読み取り専用）。`kind` は `gps`/`serial`/`owner`/`camera`/`lens`/`timestamps`/`software`/`thumbnail`/`makernote`/`description`/`xmp`/`iptc`/`comment`/`hidden`/`extra`。1 種類につき 1 項目で、重要度の高い順 |
| `outputs` | `{paths:[絶対パス...]}` | ファイルタスクが**実際に作成した**出力（ファイル 1 件ごとに 1 回）と `folder.analyze?save=1` のレポート。入力をそのまま返した場合（例 `msg.alreadySmall`）は送らない。アプリは結果ウィンドウと「元に戻す（ごみ箱へ）」にだけ使い、画面表示用ではない（`Info` の `msg.*` とは別。後方互換の追加で、知らないアプリは無視してよい） |

## 対話が必要なタスクの流れ（エンジンは質問しない。アプリが聞いて再実行する）
| タスク | 1 回目 | アプリが尋ねる | 2 回目 |
| --- | --- | --- | --- |
| `folder.removeDuplicate`（プレビュー付き） | `folder.removeDuplicate?dry=1` → `data:duplicates`（Ask）または `data:duplicates_preview`（設定の Keep ルール） | どれを残すか → 実行内容の一覧を見せて**確認** | `mode=oldest\|newest\|all&dry=1` → `data:duplicates_preview` → 確認後 `mode=...`（`dry` なし）→ `needs:trash`。プレビューは何も変更せず、確認時に再スキャンして実行する（その間にファイルが変わっていれば実行時の内容が優先）。選択の重複・入れ子（`A` と `A/sub`、同じフォルダの別表記）は 1 回だけ走査し、同じ inode（ハードリンク）は 1 ファイルとして扱う。同じファイルを 2 回含むグループは提示も削除もしない |
| `folder.removeEmpty` | `data:empty_folders` + `done(msg.emptyFound)` | 削除してよいか | `folder.removeEmpty?confirm=1` → `needs:trash`。除外フォルダ・読めない項目・リンクは「中身あり」。無視するのは OS のゴミ（`.DS_Store`・`._*`・`.localized`・`Icon\r`・`desktop.ini`・`Thumbs.db`、設定でオンのときだけ 0 バイトファイル）のみ。`needs:trash` を出す直前／完全削除の直前に各フォルダを再確認し、空でなくなったものは `warn msg.emptySkipped`（[パス]）で外し、完全削除に失敗したものは `warn msg.emptyRemoveFailed`（[パス, 理由]。理由は制御文字を除いた 200 文字以内）で報告する。完全削除はゴミを 1 件ずつ消してから深い順に `remove_dir` のみ（再帰削除はしない） |
| `folder.rename` | （アプリがダイアログで接頭辞/名前/接尾辞を取得） | — | `folder.rename?prefix=..&name=..&suffix=..&numbered=1&dry=0\|1` |
| `pdf.merge` | （アプリが順序ダイアログで並べ替え） | — | `paths` を結合したい順で渡す（2 件以上） |
| `video.trim` | （アプリがプレビューで範囲を取得） | — | `video.trim?start=秒&end=秒` |
| `video.thumbnail` | （同上） | — | `video.thumbnail?t=秒` |

## タスク ID とパラメータ
| タスク | パラメータ |
| --- | --- |
| `image.resize` / `video.resize` | `scale=50`（%）／ `scale=custom&w=&h=&keep=1\|0` |
| `image.rotate` / `video.rotate` | `deg=45`（任意角。小数可） |
| `image.convert` | `to=PNG\|JPG\|WEBP\|ICO\|GIF\|SVG`（`HEIC` はアプリ側） |
| `image.compress` | `bytes=5242880` |
| `image.strip` / `image.chromakey` | なし（設定 `Image.Metadata` / `Image.Background*` を使用） |
| `image.privacy` | なし。読み取り専用で何も書かない。JPEG/PNG/WebP。`data:privacy_report` と `msg.privacyFound`/`msg.privacyClean`（info） |
| `image.makeSafe` | `maxEdge=2048`（長辺の上限 px。省略または 0 でサイズ維持、大きい画像だけ縮小）。個人情報を削除した `_safe` コピーを書く（画像の出力フォルダ設定に従い、既存ファイルは上書きしない）。JPEG/PNG/WebP/GIF。`msg.safeRemoved`/`msg.safeNothing`/`msg.safeResized`（info）と `outputs` |
| `video.convert` | `to=MP4\|MOV\|MKV\|M4A\|AVI\|WEBM\|FLV\|GIF` |
| `video.compress` | `bytes=...` ／ `preset=discord` |
| `video.extractAudio` / `audio.convert` | `to=MP3\|M4A\|WAV\|AIF\|AIFF\|AAC\|OGG\|WMA` |
| `text.convert` | `to=TXT\|JSON\|XML\|CSV\|HTML\|MD` |
| `text.encode` | `to=UTF8\|UTF8BOM\|UTF16\|SJIS` |
| `text.lineEndings` | `to=CRLF\|LF` |
| `json.pretty` / `json.sort` | なし |
| `archive.extract` / `folder.compress` / `jar.decompile` | なし（設定を使用） |
| `folder.analyze` | `save=1` でレポートをフォルダの隣に保存 |

## メニューのプリセット項目
`image.resize.p1` のようなメニュー ID は、アプリ（Quick Action 起動後のウィンドウ）が設定（`Image.Presets.ResizeA` など）を読んで `image.resize?scale=50` のような**実タスク + パラメータ**に解決してからエンジンへ渡す。`*.custom` はアプリがダイアログで値を取得する。`video.compress.discord` は `video.compress?preset=discord`。

## 外部ツールの探索（PATH は信用しない）
`ffmpeg`/`ffprobe`/`7zz`: `/opt/homebrew/bin`, `/usr/local/bin` のみ（他者書込可能なファイル/ディレクトリは拒否）。Java: `/usr/libexec/java_home` の結果または Homebrew の openjdk（`/usr/bin/java` スタブは使わない）。CFR: `<データフォルダ>/deps/cfr.jar` で **SHA-256 が固定値と一致するときだけ**使用（取得は `scripts/fetch-cfr.sh`）。CFR は必ず `/usr/bin/sandbox-exec` の中で動かす（ネットワーク禁止、書き込みはステージングと非公開の一時フォルダ（0700、作業フォルダ内）だけ、起動できるのは JVM のみ。パスは解決済み（`/private/var/...`）で `-D` パラメータとして渡す）。実行前に同じプロファイル・同じパラメータで `/usr/bin/true` を動かして確かめ、`sandbox-exec` が無い／拒否された場合は `error msg.sandboxFailed`（[ファイル名, 理由]）で失敗し、**サンドボックスなしでは実行しない**。

## エンジンが使う `key`（Swift の String Catalog に必要）
旧 Windows 版の `Strings.En.cs` にも存在するキーは翻訳を流用できる。**新規**（翻訳が必要）:

- `msg.archiveBomb`
- `msg.archiveDuplicateEntry`
- `msg.archiveEncrypted`
- `msg.archiveSkippedLink`
- `msg.archiveTooLarge`
- `msg.archiveTooManyEntries`
- `msg.archiveToolFailed`
- `msg.archiveUnsafePath`
- `msg.archiveUnsupported`
- `msg.cancelled`
- `msg.cfrMismatch`
- `msg.cfrMissing`
- `msg.compressEncode`
- `msg.compressStage`
- `msg.decompileFailed`
- `msg.duplicateChanged`
- `msg.duplicatesFound`
- `msg.duplicatesPreview`
- `msg.emptyFound`
- `msg.emptyRemoveFailed`
- `msg.emptySkipped`
- `msg.ffmpegFailed`
- `msg.ffmpegMissing`
- `msg.fileTooLarge`
- `msg.imageDecodeFailed`
- `msg.imageEncodeFailed`
- `msg.imageTooLarge`
- `msg.imageUnsupported`
- `msg.invalidJson`
- `msg.invalidName`
- `msg.ioError`
- `msg.javaMissing`
- `msg.mediaProbeFailed`
- `msg.mediaUnsupported`
- `msg.notAFile`
- `msg.notAFolder`
- `msg.pdfEncrypted`
- `msg.pdfInvalid`
- `msg.pdfNeedTwo`
- `msg.privacyClean`
- `msg.privacyFound`
- `msg.pipelineBadStep`
- `msg.pipelineIncompatible`
- `msg.pipelineStage`
- `msg.pipelineStepCount`
- `msg.pipelineStepNotAllowed`
- `msg.pipelineWrongInput`
- `msg.processing`
- `msg.renameFailed`
- `msg.reportSaved`
- `msg.safeNothing`
- `msg.safeRemoved`
- `msg.safeResized`
- `msg.sandboxFailed`
- `protocol.badJson`
- `protocol.badPipeline`
- `protocol.badPath`
- `protocol.lineTooLong`
- `protocol.notUtf8`
- `protocol.tooManyPaths`
- `task.badOption`
- `task.unimplemented`
- `task.unknown`

旧版と共通で使うキー（35 件）: `dialog.trim.backwards`, `msg.alreadySmall`, `msg.cannotConvertFrom`, `msg.cannotConvertTo`, `msg.comparing`, `msg.decompileEmpty`, `msg.deletedPermanently`, `msg.emptyDeleted`, `msg.emptyFile`, `msg.emptyRecycled`, `msg.hardwareFallback`, `msg.mergedInto`, `msg.merging`, `msg.noAudioTrack`, `msg.noDataRows`, `msg.noDuplicates`, `msg.noDuration`, `msg.noEmpty`, `msg.noPages`, `msg.noRootElement`, `msg.noVideoStream`, `msg.notATable`, `msg.nothingToRemove`, `msg.overTarget`, `msg.readAsSeparated`, `msg.recycled`, `msg.renameDryRun`, `msg.renamed`, `msg.scanningCount`, `msg.separator.tab`, `msg.sjisFailed`, `msg.sjisReplaced`, `msg.targetMissed`, `msg.verifying`, `msg.webpFallback`

## アプリ側の追加機能（エントリポイント・結果・元に戻す）
- 起動経路: Finder Quick Action（`--task/--input`）に加え、Dock/Flect.app へのドロップ、File > Open…（⌘O）、サービス「Flect で処理…」。いずれも `LaunchRequest.validate`（絶対パス・4096 バイト・NUL 不可・20,000 件）を通し、エンジンのカタログにある ID だけを選択肢に出し、実行は `Planner.resolveCatalogID` 経由（Quick Action と同じ。`?` 付きのパラメーターは受け付けない）。Quick Action の `--task` はカタログ ID か `preset:<UUID>` のみ、`--input` は一時フォルダ直下の自分の `flect-sel.*`（通常ファイル・64 MiB 未満）のみ読み込み・unlink し、中身は NUL 区切り（形式 2、`flect-quick-action-format=2`）。改行区切りの旧形式は 1 件かつ改行を含まない場合だけ受け付ける。
- 実行履歴: `<データフォルダ>/run-history.json`（最新 20 件、1 実行あたり出力 2,000 件まで、権限 0600）。
- 元に戻す: `outputs` で報告されたパスのみが対象。記録時に「入力そのもの／入力を含むフォルダ／実行開始前から存在／シンボリックリンク／ホーム以上」を除外し、実行終了時点の指紋（サイズ・更新時刻・フォルダは内容の件数と最新更新時刻）を保存。取り消し時に再確認し、変更・置換・消失したものは触らず、ファイルごとに結果を報告。移動は `FileManager.trashItem`（ごみ箱。完全削除はしない）。

## 出力フォルダとシンボリックリンク

設定の出力フォルダ（Settings > Output）が**シンボリックリンク**、またはその途中にリンクがある場合、エンジンは辿らずに拒否し、そのファイルを `error`（キー `msg.outputFolderLink`）として報告する。`~`・絶対パス・相対パスは実フォルダなら従来どおり使える。基準となるフォルダ（元ファイルのフォルダ、ホーム）より上のリンクと、`/tmp`・`/var`・`/etc` のように `/` 直下にある macOS 標準のエイリアスは対象外。アプリ側の直書き出力（`OutputPath`）も同じ規則。
