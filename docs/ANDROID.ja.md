# Android 版（試験的）

デスクトップ版と同じ受信処理を Android で動かすための試験的なビルドです。
Android には SoapySDR がないため、USB ドングルは別アプリ「Rtl-sdr driver」に担当させ、
同じスマホ内の RTL-TCP サーバー（`127.0.0.1:1234`）から IQ を受け取ります。

## 必要なもの

- USB OTG（USB ホスト）に対応した Android 8.0 以降・arm64 のスマホ
- RTL-SDR ドングル（周波数ずれの少ない TCXO 付きを推奨）、OTG アダプター、350 MHz 帯で使えるアンテナ
- [Rtl-sdr driver](https://f-droid.org/packages/marto.rtl_tcp_andro/)（`marto.rtl_tcp_andro`。Google Play または F-Droid）

## APK の入手とインストール

1. GitHub の **Actions** → **Android APK** で最新の成功した実行を開き、
   **Artifacts** の `jr-radio-receiver-android-arm64` をダウンロードして展開します。
2. APK をスマホに送り、「提供元不明のアプリ」のインストールを許可して開きます。

リポジトリに署名鍵のシークレット（後述）がない場合、ビルドごとに使い捨ての鍵で署名されます。
その場合、新しいビルドを入れる前に古いアプリをアンインストールしてください（設定も消えます）。

## 使い方

1. ドングルをスマホに挿します。
2. アプリの入力方式「RTL-TCP サーバー」で **RTL-SDR ドライバーを起動** を押します。
   初回は USB の使用許可を求められるので許可します。
3. ドライバーが起動したら **受信を開始** を押します。IP アドレスとポートは自動で `127.0.0.1` / `1234` が入ります。
4. 「音声を聴く」をオンにすると、選択中のチャンネルが再生されます。

## 現時点の制限

- 画面を消したりアプリを切り替えたりすると音声の再生と画面更新が止まります。受信中は画面をつけたままにしてください。
- SoapySDR デバイスは選べません。ファイル・フォルダーの選択ボタンも非表示です（パスの手入力は可能）。
- 録音の既定の保存先はアプリ専用フォルダー（`Android/data/io.jrradio.receiver/files/Music/…`）です。
- 録音した内容や聞いた通信の内容を他人に漏らすことは電波法で禁じられています。

## 署名鍵を固定する（任意）

同じ鍵で署名すると、アンインストールせずに上書き更新できます。PC で鍵を作り、

```
keytool -genkeypair -keystore release.jks -alias jrradio -keyalg RSA -keysize 2048 -validity 10000
base64 -w0 release.jks
```

リポジトリの Settings → Secrets and variables → Actions に次を登録します。

| 名前 | 値 |
| --- | --- |
| `ANDROID_KEYSTORE_BASE64` | 上の base64 出力 |
| `ANDROID_KEYSTORE_PASSWORD` | キーストアのパスワード |
| `ANDROID_KEY_ALIAS` | `jrradio` |
| `ANDROID_KEY_PASSWORD` | 鍵のパスワード（キーストアと同じなら省略可） |
