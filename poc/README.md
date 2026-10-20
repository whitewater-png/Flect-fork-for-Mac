# P0 PoC kit

`docs/mac-port/03-design-plan.ja.md` §9 の未検証事項を潰すための使い捨てコード。製品コードではない。

| § | 項目 | ファイル | 実行場所 | 状態 |
| --- | --- | --- | --- | --- |
| 5 | Rust の PNG/JPEG/WebP(lossy)/ICO/GIF/SVG/リサイズ/任意角回転 | `rust-imaging/` | Linux/Mac | **実行済み**（結果は `docs/mac-port/04-p0-results.ja.md`） |
| 3 | Vision OCR（日/中/英） | `mac/ocr.swift` | Mac | 未実行 |
| 4 | ImageIO HEIC 書込 | `mac/heic.swift` | Mac | 未実行 |
| 2,6 | ffmpeg/VideoToolbox、ツール有無、Finder 拡張状態 | `mac/check-env.sh` | Mac | 未実行 |
| 1 | Quick Action → アプリ起動 | `mac/quick-action.md`（手順） | Mac | 未実行 |
| 7 | C# vs Rust ベンチ | — | — | この環境に dotnet 無し。Mac に Windows 版は動かないため**実施不可**。代替: 絶対値で目標設定（結果文書参照） |
