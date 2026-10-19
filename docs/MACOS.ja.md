# JR-CLRR For Mac（試験的）

JR Cconventional-line Radio Receiver（KIRISHIKI Yudai 氏）をもとにした Apple Silicon Mac 用のアプリです。
画面は Android 版（JR-CLRR For Android）と同じ簡略版で、音声の復号は最新（v1.5）の復号器を使います。
SoapySDR と RTL-SDR 用モジュールをアプリに同梱しているので、**Homebrew は不要**です。ドングルを Mac に挿してそのまま使えます。

## 入手とインストール

1. GitHub の **Actions** → **macOS app** で最新の成功した実行を開き、**Artifacts** の `jr-clrr-for-mac-arm64` をダウンロードして展開します。
2. `JR-CLRR-For-Mac-arm64.dmg` を開き、`JR-CLRR For Mac` を「アプリケーション」フォルダーにドラッグします。
3. 初回は Apple の正式な署名（Developer ID・公証）がないため起動が止められます。
   一度開こうとしたあと、**システム設定 → プライバシーとセキュリティ** の下にある「**このまま開く**」を押して起動してください。
   ターミナルを使う場合は `xattr -dr com.apple.quarantine "/Applications/JR-CLRR For Mac.app"` でも開けるようになります。

## 使い方

1. RTL-SDR ドングルを Mac に挿します（USB-C の Mac は変換アダプターを使います）。
2. 受信設定の入力方式が「**SoapySDR デバイス**」になっていることを確認し、「**検索**」を押してドングルを選びます。
3. 聴きたい路線があれば「**路線から選ぶ**」で区間を選び、受信する波を絞ります。
4. 「**受信を開始**」を押します。上部の「音声オン / ミュート中」で消音を切り替えられます。

ラズパイなどの RTL-TCP サーバーに接続したい場合は、入力方式を「RTL-TCP サーバー」にして IP アドレスとポートを入力します。

## Android 版との違い

- 音声の復号は v1.5 の高音質な復号器です（Mac は処理に余裕があるため）。
- ドングルは Mac に直接挿して使います（Rtl-sdr driver は不要）。ファイル・フォルダーの選択ボタンも使えます。
- 録音の初期保存先は `~/Music/JR Cconventional-line Radio Receiver/Recordings` です。

## 注意

- ウインドウを最小化したり完全に隠したりすると、画面の更新や音声の再生が遅れることがあります。受信中はウインドウを表示したままにしてください。
- 近すぎて電波が強い場所では、受信設定の「ゲイン」を手動（20 dB 前後）にして「広帯域入力」がマイナスになるようにします。
- 録音した内容や聞いた通信の内容を他人に漏らすことは電波法で禁じられています。
