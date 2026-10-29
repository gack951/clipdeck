# ClipDeck

Windows 11 / Linux用Rustクリップボード履歴アプリ。egui/eframeのwgpuバックエンドで描画します。

## 実行

```powershell
cargo run --release
```

Linuxでも同じコマンドで実行できます。保存先はWindowsでは`%LOCALAPPDATA%\ClipDeck`、Linuxでは`$XDG_DATA_HOME/ClipDeck`（未設定時は`~/.local/share/ClipDeck`）です。

## 対応機能

- テキスト、画像、Explorerでコピーしたファイル一覧（CF_HDROP、Windowsのみ）の履歴
- 履歴検索・種別フィルター・固定・削除・クリップボード復元
- URLの既定アプリ起動、既存パスの起動、Explorerで選択、親フォルダを開く
- 最大300件（固定項目は上限対象外）、画像は16メガピクセルまで
- 300ms間隔のクリップボード監視

## 制約

- Linuxではファイル一覧形式の取得・復元に未対応です。
- 画像はPNGでアプリ専用ディレクトリに保存します。
- Explorer由来のファイル・フォルダは**内容ではなくパス**を記録します。元のファイルが移動・削除された場合、復元先で利用できません。
- 仮想ファイル形式（Outlook添付ファイル等のCFSTR_FILEDESCRIPTOR）は未対応です。
- クリップボードが短時間に更新された場合、中間の履歴は取得できません。
- 履歴はローカルに平文保存されます。機密情報をコピーする端末では監視停止や履歴削除を検討してください。
