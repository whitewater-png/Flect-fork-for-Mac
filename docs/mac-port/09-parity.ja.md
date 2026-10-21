# Flect-for-Mac — 09 機能パリティ表（旧 Windows 版 87 項目）

更新: 2026-10-20 ／ 項目は `core/flect-core/src/menu.rs` のリーフ ID（旧 `MenuCatalog.cs` と diff で一致を確認済み）。

| 結果 | 項目数 |
| --- | --- |
| Rust エンジンで実装済み | **81** |
| Swift アプリ側で実装予定（OS 機能） | **6** |
| 未実装（AI 機能） | **0**（背景除去は Vision。実機で要検証） |
| 合計 | 87 |

| 系統 | 担当 | 件数 | ID |
| --- | --- | --- | --- |
| `folder` | Rust エンジン（Linux でテスト済み） | 6 | `folder.removeDuplicate`, `folder.removeEmpty`, `folder.tree`, `folder.rename`, `folder.analyze`, `folder.compress` |
| `archive` | Rust エンジン（Linux でテスト済み） | 1 | `archive.extract` |
| `pdf` | Rust エンジン（Linux でテスト済み） | 1 | `pdf.merge` |
| `pdf` | Swift アプリ（OS 機能。Mac 実機で要検証） | 4 | `pdf.preview`, `pdf.convert.PNG`, `pdf.convert.JPG`, `pdf.convert.TXT` |
| `image` | Rust エンジン（Linux でテスト済み） | 19 | `image.resize.p1`, `image.resize.p2`, `image.resize.custom`, `image.compress.p1`, `image.compress.p2`, `image.compress.p3`, `image.compress.p4`, `image.compress.custom`, `image.rotate.p1`, `image.rotate.p2`, `image.rotate.p3`, `image.convert.PNG`, `image.convert.JPG`, `image.convert.WEBP`, `image.convert.ICO`, `image.convert.GIF`, `image.convert.SVG`, `image.strip`, `image.chromakey` |
| `image` | Swift アプリ（OS 機能。Mac 実機で要検証） | 2 | `image.convert.HEIC`, `image.ocr` |
| `video` | Rust エンジン（Linux でテスト済み） | 29 | `video.resize.p1`, `video.resize.p2`, `video.resize.custom`, `video.trim`, `video.thumbnail`, `video.compress.discord`, `video.compress.p1`, `video.compress.p2`, `video.compress.p3`, `video.compress.custom`, `video.rotate.p1`, `video.rotate.p2`, `video.rotate.p3`, `video.convert.MP4`, `video.convert.MOV`, `video.convert.MKV`, `video.convert.M4A`, `video.convert.AVI`, `video.convert.WEBM`, `video.convert.FLV`, `video.convert.GIF`, `video.extractAudio.MP3`, `video.extractAudio.M4A`, `video.extractAudio.WAV`, `video.extractAudio.AIF`, `video.extractAudio.AIFF`, `video.extractAudio.AAC`, `video.extractAudio.OGG`, `video.extractAudio.WMA` |
| `video` | Rust エンジン（ffmpeg の whisper フィルタ。実機で要検証） | 1 | `video.subtitles` |
| `audio` | Rust エンジン（Linux でテスト済み） | 8 | `audio.convert.MP3`, `audio.convert.M4A`, `audio.convert.WAV`, `audio.convert.AIF`, `audio.convert.AIFF`, `audio.convert.AAC`, `audio.convert.OGG`, `audio.convert.WMA` |
| `audio` | Rust エンジン（同上） | 1 | `audio.subtitles` |
| `text` | Rust エンジン（Linux でテスト済み） | 12 | `text.convert.TXT`, `text.convert.JSON`, `text.convert.XML`, `text.convert.CSV`, `text.convert.HTML`, `text.convert.MD`, `text.encode.UTF8`, `text.encode.UTF8BOM`, `text.encode.UTF16`, `text.encode.SJIS`, `text.lineEndings.CRLF`, `text.lineEndings.LF` |
| `json` | Rust エンジン（Linux でテスト済み） | 2 | `json.pretty`, `json.sort` |
| `jar` | Rust エンジン（Linux でテスト済み） | 1 | `jar.decompile` |

## 注記
- 「実装済み」は **Linux 上のテストで確認済み**という意味。ffmpeg の一部（VideoToolbox を使う経路）、7zz 実物、`sandbox-exec` を使う JAR 実行は Mac でしか確認できない。
- `image.convert.HEIC`、`pdf.preview`、`pdf.convert.*`（PNG/JPG/TXT）、`image.ocr` は OS 標準の ImageIO / PDFKit / Vision を使うため Swift 側（エンジンは `msg.imageUnsupported` を返す）。
- `image.chromakey` は色キー方式のみ（旧 `ChromaKeyOnly` モード）。AI 背景除去（rembg）は未実装で、設定が AI モードでも色キーで動く。
- 画像のリサイズ・変換などでは**元ファイルのメタデータ（EXIF/ICC）は出力に引き継がれない**（再エンコードのため）。EXIF の向きは画素に反映する。旧版（ImageMagick）はプロファイルを保持していた。意図的な差分。
- 旧版との挙動差は他に: 動画の `-threads` 段階的縮退は「既定→4→1」に簡略化、プロセス優先度は `nice -n 5`。
- `pdf.merge` は実機確認の結果、アプリ側（PDFKit）の実装に切り替えた。Rust エンジンの `pdf.merge`（と依存の `lopdf`）は、使われなくなったため削除した。
- 字幕は ffmpeg の `whisper` フィルタ（whisper.cpp）で生成する。Python 不要。この Mac の `ffmpeg`（Homebrew の `ffmpeg-full` 9.0.2）では `ffmpeg -filters` に `whisper` がある。標準の `ffmpeg` に入っているかは未確認。モデルは `scripts/fetch-whisper-model.sh` で入れる（SHA-256 固定、エンジンは固定ハッシュ以外を使わない）。Linux の CI には whisper 付き ffmpeg がないため、実際の文字起こしは Mac でのみ確認できる。
- AI 背景除去（rembg）は引き続き未実装（設定が AI モードでも色キーで動く）。
- `image.chromakey`（背景を削除）は、設定が AI のとき Swift アプリが Vision（`VNGenerateForegroundInstanceMaskRequest`、macOS 14+）で前景を切り出し、`名前_nobg.png` を出力する。AI が失敗した画像は、設定が「AI + 失敗時は色キー」ならエンジンの色キーに切り替わる。「色キーのみ」はエンジンが処理する。Mac でのみ確認できる。
- 実機確認の手順（第 2 回）: [13](13-mac-verification-round2.ja.md)。
- 字幕: 実機（第 2 回）で、GPU 自動の Tiny は 60 秒の音声が 1.5 秒、Small は 4.9 秒。CPU のみは大幅に遅く（7.4 秒の音声に 26 秒、負荷も高い）、通常は GPU 自動を使う。出力は cue を 1 から振り直し、音声の長さを超える終了時刻は丸め、範囲外の cue は捨てる。VTT に cue 番号は付けない。
- 背景除去の出力名は、AI が `_nobg.png`、色キーが `_keyed.png`。
- 出力先フォルダー（Mac 版で追加）: `settings.json` の `Output` セクション（`Image` / `Video` / `Audio` / `Text` / `Pdf` / `Ocr` / `ArchiveExtract` / `ArchiveCompress` / `Report`）。Windows 版には同等の設定がなく、キーは Mac 版独自（Windows の設定ファイルには無いので読み込み時は空＝元の場所）。空欄は元のファイルの隣。相対パスは元ファイルのフォルダーが基準、`~` と絶対パスも可。`.` と `..` は文字列として解決する（シンボリックリンクは追跡しない）。フォルダーは無ければ作る。既存ファイルは上書きせず ` (2)` を付ける。フォルダー圧縮で、圧縮対象の中を出力先にすることは拒否する。リンクできない場所（別ボリューム、exFAT など）への出力は、排他的に作成したファイルへのコピーで確定する。`Code.Decompile.OutputPath`（JAR 逆コンパイル）は従来どおり別設定。
- zip のファイル名: UTF-8 として読めない名前は Shift_JIS、次に GBK で読み直す（アーカイブ単位で 1 つの判定。Shift_JIS で半角カナにしか読めず GBK で読める場合は GBK）。どちらにも合わなければ従来どおり CP437。
