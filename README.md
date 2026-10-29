# ClipDeck

Windows 11 / Linux用Rustクリップボード履歴アプリ。egui/eframeのOpenGLバックエンドで描画します。

## 実行・ビルド

```powershell
cargo run --release
```

Linuxでも同じコマンドで実行できます。保存先はWindowsでは`%LOCALAPPDATA%\ClipDeck`、Linuxでは`$XDG_DATA_HOME/ClipDeck`（未設定時は`~/.local/share/ClipDeck`）です。

Windows版はWindows上の`x86_64-pc-windows-msvc`でビルドします。GitHub ActionsもWindows/MSVCとLinuxの両方を検証します。

起動後はウィンドウを表示せずタスクトレイで待機します。トレイアイコンをクリックすると履歴画面を表示します。

## 対応機能

- テキスト、画像、Explorerでコピーしたファイル一覧（CF_HDROP、Windowsのみ）の履歴
- 履歴検索・種別フィルター・固定・削除・クリップボード復元
- URLの既定アプリ起動、既存パスの起動、Explorerで選択、親フォルダを開く
- 最大300件（固定項目は上限対象外）、画像は16メガピクセルまで
- 300ms間隔のクリップボード監視
- 絶対パス文字列をコピーすると、マウスカーソルがあるディスプレイ中央にパス操作ダイアログを表示
- ダイアログでパスを編集しながら、対象と親フォルダの存在をリアルタイム検証

## パスダイアログ

- `Enter`: パスを既定アプリで開く
- `Backspace`: 親フォルダを開く
- `Ctrl+Enter`: 設定したアプリ（初期値`code`）で開く。ExcelファイルはWindows版Excelを読み取り専用で開く
- `Ctrl+Backspace`: 親フォルダを設定したアプリで開く
- `Ctrl+C`: UNCパスをクリップボードへコピー（Linuxでは元の絶対パス）
- `Esc`: ダイアログを閉じてトレイ待機へ戻る

## 制約

- Linuxではファイル一覧形式の取得・復元に未対応です。
- 画像はPNGでアプリ専用ディレクトリに保存します。
- Explorer由来のファイル・フォルダは**内容ではなくパス**を記録します。元のファイルが移動・削除された場合、復元先で利用できません。
- 仮想ファイル形式（Outlook添付ファイル等のCFSTR_FILEDESCRIPTOR）は未対応です。
- クリップボードが短時間に更新された場合、中間の履歴は取得できません。
- 履歴はローカルに平文保存されます。機密情報をコピーする端末では監視停止や履歴削除を検討してください。
