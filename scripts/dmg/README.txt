Flect-for-Mac のインストール / Installing Flect-for-Mac
==========================================================

【日本語】
1. 「Flect-for-Mac」を「Applications」へドラッグします。
2. アプリを開きます（Launchpad、または「アプリケーション」フォルダから）。
3. 最初に出る「はじめに」の手順（クイックアクションのインストール → システム設定でオン）を行います。

■ 「開発元を確認できません」などの警告が出て、開けないとき
このアプリは Apple の署名・公証を受けていないため、最初の 1 回だけ許可が必要です。

  macOS 15 以降:
    1. 一度ダブルクリックして開こうとし、出た表示を閉じる（「ゴミ箱に入れる」は押さない）
    2. システム設定 → プライバシーとセキュリティ
    3. 下のほうの「このまま開く」を押す（ログインパスワードを聞かれることがあります。
       「このまま開く」は、開こうとしてから約 1 時間だけ押せます）
    4. 確認の表示が出たら、開くほうを選ぶ
  macOS 14:
    アプリを右クリック（control + クリック）→「開く」→「開く」

心配なときは、リリースページ（https://github.com/whitewater-png/Flect-fork-for-Mac/releases）の SHA-256 と比べてから開いてください。
ターミナルで「shasum -a 256 」（最後に空白）と打ち、ダウンロードした dmg ファイルをターミナルへドラッグして Return を押すと、値が表示されます。

[English]
1. Drag "Flect-for-Mac" onto "Applications".
2. Open the app (Launchpad, or the Applications folder).
3. Follow the "Getting started" steps it shows first (install the Quick Actions, then switch them on in System Settings).

If macOS says the developer cannot be verified and the app will not open:
This app is not signed or notarized by Apple, so you have to allow it once.

  macOS 15 or later:
    1. Try to open it once and close the message (do not click "Move to Trash")
    2. System Settings -> Privacy & Security
    3. Click "Open Anyway" near the bottom (you may be asked for your login password;
       the button is available for about an hour after the attempt)
    4. Choose Open in the confirmation
  macOS 14:
    Right-click (or control-click) the app -> Open -> Open

To be careful, compare the SHA-256 on the release page (https://github.com/whitewater-png/Flect-fork-for-Mac/releases) with the one from
"shasum -a 256 " (note the trailing space) followed by dragging the downloaded dmg into Terminal.
