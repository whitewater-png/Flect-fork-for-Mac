# Flect-for-Mac — 08 エンジンプロトコル（Swift アプリとの契約）

更新: 2026-10-20 ／ 実装: `core/flect-core/src/protocol.rs`, `core/flect-engine/src/main.rs`

`flect-engine` は子プロセスとして起動し、**stdin に 1 行 1 JSON のリクエスト、stdout に 1 行 1 JSON のイベント**を流す。ローカライズはしない（メッセージは `key` + `args`）。環境変数 `FLECT_HOME` でデータフォルダを上書きできる（既定 `~/Library/Application Support/Flect-for-Mac`）。

## 入力の制限（超えると `protocol.*` エラー）
1 行 1 MiB まで／UTF-8／未知フィールドは拒否／`run` の `paths` は 1〜20,000 件、各 4096 バイト以内の**絶対パス**（NUL 不可）。

## リクエスト
| op | 形 | 意味 |
| --- | --- | --- |
| `run` | `{"op":"run","task":"image.resize?scale=50","paths":["/abs/a.png"]}` | タスクを実行。`task` は `ID?key=value&...`（値は `%XX` パーセントエンコード、UTF-8）。`options` は予約（現在無視） |
| `cancel` | `{"op":"cancel"}` | 実行中のタスクを中断。処理中でも即時に受理され、`done` が `outcome:"cancelled"` で届く。待機中なら何も起きない |
| `export-menu` | `{"op":"export-menu"}` | `menu` イベントを 1 つ返す（`menu.json` と同内容。Quick Action 生成にも使用） |

リクエストは**直列処理**（`run` は 1 つずつ）。

## イベント（`"event"` で判別）
| event | 項目 | 意味 |
| --- | --- | --- |
| `status` | `key`, `args[]` | 現在の作業の説明（例 `msg.processing` [ファイル名]） |
| `step` | `done`, `total` | **ファイル単位**の全体進捗 |
| `file_step` | `permille` (0–1000) | 現在のファイル内の進捗（動画のみ） |
| `info` / `warn` / `error` | `key`, `args[]` | 表示用メッセージ。`error` は 1 ファイルの失敗で、バッチは継続する |
| `needs` | `what`, `paths[]` | アプリに OS 操作を依頼。現在は `what:"trash"` のみ（`paths` をごみ箱へ移動。エンジンは移動後の応答を待たない） |
| `data` | `name`, `data` | 結果の提示用データ（下表） |
| `menu` | `menu` | `export-menu` の応答 |
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
| `folder.removeDuplicate`（プレビュー付き） | `folder.removeDuplicate?dry=1` → `data:duplicates`（Ask）または `data:duplicates_preview`（設定の Keep ルール） | どれを残すか → 実行内容の一覧を見せて**確認** | `mode=oldest\|newest\|all&dry=1` → `data:duplicates_preview` → 確認後 `mode=...`（`dry` なし）→ `needs:trash`。プレビューは何も変更せず、確認時に再スキャンして実行する（その間にファイルが変わっていれば実行時の内容が優先） |
| `folder.removeEmpty` | `data:empty_folders` + `done(msg.emptyFound)` | 削除してよいか | `folder.removeEmpty?confirm=1` → `needs:trash` |
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
`ffmpeg`/`ffprobe`/`7zz`: `/opt/homebrew/bin`, `/usr/local/bin` のみ（他者書込可能なファイル/ディレクトリは拒否）。Java: `/usr/libexec/java_home` の結果または Homebrew の openjdk（`/usr/bin/java` スタブは使わない）。CFR: `<データフォルダ>/deps/cfr.jar` で **SHA-256 が固定値と一致するときだけ**使用（取得は `scripts/fetch-cfr.sh`）。

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
- `msg.duplicatesFound`
- `msg.duplicatesPreview`
- `msg.emptyFound`
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
- `msg.processing`
- `msg.renameFailed`
- `msg.reportSaved`
- `msg.safeNothing`
- `msg.safeRemoved`
- `msg.safeResized`
- `msg.sandboxUnavailable`
- `protocol.badJson`
- `protocol.badPath`
- `protocol.lineTooLong`
- `protocol.notUtf8`
- `protocol.tooManyPaths`
- `task.unimplemented`
- `task.unknown`

旧版と共通で使うキー（35 件）: `dialog.trim.backwards`, `msg.alreadySmall`, `msg.cannotConvertFrom`, `msg.cannotConvertTo`, `msg.comparing`, `msg.decompileEmpty`, `msg.deletedPermanently`, `msg.emptyDeleted`, `msg.emptyFile`, `msg.emptyRecycled`, `msg.hardwareFallback`, `msg.mergedInto`, `msg.merging`, `msg.noAudioTrack`, `msg.noDataRows`, `msg.noDuplicates`, `msg.noDuration`, `msg.noEmpty`, `msg.noPages`, `msg.noRootElement`, `msg.noVideoStream`, `msg.notATable`, `msg.nothingToRemove`, `msg.overTarget`, `msg.readAsSeparated`, `msg.recycled`, `msg.renameDryRun`, `msg.renamed`, `msg.scanningCount`, `msg.separator.tab`, `msg.sjisFailed`, `msg.sjisReplaced`, `msg.targetMissed`, `msg.verifying`, `msg.webpFallback`

## アプリ側の追加機能（エントリポイント・結果・元に戻す）
- 起動経路: Finder Quick Action（`--task/--input`）に加え、Dock/Flect.app へのドロップ、File > Open…（⌘O）、サービス「Flect で処理…」。いずれも `LaunchRequest.validate`（絶対パス・4096 バイト・NUL 不可・20,000 件）を通し、エンジンのカタログにある ID だけを選択肢に出し、実行は `Planner.resolve` 経由（Quick Action と同じ）。
- 実行履歴: `<データフォルダ>/run-history.json`（最新 20 件、1 実行あたり出力 2,000 件まで、権限 0600）。
- 元に戻す: `outputs` で報告されたパスのみが対象。記録時に「入力そのもの／入力を含むフォルダ／実行開始前から存在／シンボリックリンク／ホーム以上」を除外し、実行終了時点の指紋（サイズ・更新時刻・フォルダは内容の件数と最新更新時刻）を保存。取り消し時に再確認し、変更・置換・消失したものは触らず、ファイルごとに結果を報告。移動は `FileManager.trashItem`（ごみ箱。完全削除はしない）。
