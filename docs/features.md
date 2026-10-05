# CodeQuilt 機能一覧

今の実装でできることと、コマンド・設定の一覧。仕組みは [design.md](design.md)、判断の経緯は [decisions.md](decisions.md) にある。

## 機能

### セッション

- `cq claude` / `cq codex` でエージェントをCodeQuiltのセッションとして起動する。手元の端末にそのまま表示され、Ctrl+\ で切り離しても動き続ける。
- 切り離したセッションは `cq attach`（`cq a`）でどの端末からでも付け直せる。ダッシュボードのタイルからも同時に操作できる。
- セッションは名前・id・idの前方一致で指定できる。名前は動いているセッションの中でマシンごとに一意。
- 終了は `cq kill` かダッシュボードの「End session」。エージェント自身に終了させ、10秒で終わらなければ強制終了する。
- 終了したセッションはresumeできる（エージェント側のセッションを引き継いで起動し直す）。

### マシン

- sshで届くマシンを登録すると、そのマシンのセッションも同じ一覧・同じ操作で扱える（Windows・Linux）。
- 各マシンのdaemonは必要になったときに自動で起動する。止めたり入れ替えたりしても、セッションは動き続ける。

### ダッシュボード（cq-board）

- **セッション一覧**（左）：マシンごとに、動いているセッション（Active）と終了したセッション（Ended、既定で折りたたみ）を分けて出す。各行は既定で1行（名前・id・状態）で、▸ で詳細（`machine:`、`cwd:`、`model:`、`context:`、`tokens:`）を開く。idはクリックでコピーできる。
- **ピン留め**：`ui.toml` の `pinned` に書いた名前のセッション（既定は `queen`）を上部にも出す。📌で個別に付け外しできる。
- **タイル**：セッションをタブとして開き、左右・上下に分割して並べる。配置は保存される。存在しないセッションや終了したセッションのタイルは自動で閉じる。
- **承認ダイアログ**：承認の要求や確認ダイアログ（フォルダの信頼、開発用チャネルなど）が出たら、セッション・マシン・cwd・内容・選択肢を示すダイアログを出す。手元の端末で答えても、ダイアログで答えてもよい。
- **使用量**（右、開閉できる）：エージェントごと・アカウントごとの5hとweeklyのレート制限（使用率、リセット時刻）。
- **コマンドパレット**（Ctrl+Shift+P）：組み込みとプラグインのコマンドを検索して実行する。引数のあるコマンドは既定値つきで入力を求める。
- **キーバインド**：すべてのコマンドに `keybindings.toml` でキーを割り当てられる。
- **起動時の処理**：`ui.toml` の `on_start` に並べたコマンドを起動時に実行する（既定はQueenの起動）。
- **通知**：ターン終了や承認待ちをOS通知で知らせる。コマンドの結果と失敗は右下に出す。
- **フォント**：`ui.toml` でターミナルのフォントと大きさを変えられる。

### Queen（cq-queen）

- 他のセッションの承認待ち・質問・ターン終了をまとめて報告し、ユーザーの指示や承認を各セッションに送るオーケストレーター。Queen自身は代わりに判断しない。
- `@名前 本文` と書くと、Queenを介さずそのセッションに直接送る。
- 担当するマシン、モデル、人格、届け方は設定で変えられる。名前の違うQueenを複数動かせる。

### プラグイン

- `cq-*` という実行ファイルは `cq *` のサブコマンドとして呼べる。本体の隣にあるものは自動で導入済みになり、宣言したコマンドがパレットやキーやCLI（`cq run`）から使える。

## コマンド

### cq

| コマンド | 内容 |
| --- | --- |
| `cq claude [--cq-name NAME] [--cq-settings FILE] [claudeの引数…]` | claudeをセッションとして起動し、この端末に付ける。`--cq-settings` はhooksを足す設定ファイル |
| `cq codex [--cq-name NAME] [codexの引数…]` | codexをセッションとして起動し、この端末に付ける |
| `cq attach TARGET`（`cq a`） | 動いているセッションにこの端末を付ける。Ctrl+\ で切り離す |
| `cq ls` | このマシンのセッションの一覧 |
| `cq rename TARGET [NAME] [--clear]` | 名前を付ける（重複は断る）／消す |
| `cq inject TARGET TEXT [--submit] [--quiet-ms N]` | セッションがアイドルで誰も入力していないときに文字を入れる |
| `cq kill TARGET [--force] [--grace SECONDS]` | セッションを終了させる |
| `cq usage [--local] [--json]` | アカウントごとのレート制限と、セッションごとのトークン |
| `cq run NAME [--arg KEY=VALUE]…` | プラグインのコマンドを実行する |
| `cq plugin list \| add NAME \| remove NAME \| refresh` | プラグインの一覧・導入・削除・manifestの再読込 |
| `cq machine list \| add [HOST] [--label L] [--cq-path P] [--login-shell] \| remove ID` | リモートマシンの一覧・登録・削除。HOSTを省くと `~/.ssh/config` から選ぶ |
| `cq daemon` | daemonを動かす（通常は自動で起動される） |
| `cq daemon status \| stop \| restart [--machine ID]` | daemonの状態・停止・再起動。止めてもセッションは動き続ける |
| `cq mcp [--scope machine:ID]… [--channel]` | MCPサーバー（stdio）。`--channel` はClaude Codeのチャネルとしても動く |
| `cq bridge` | 標準入出力をこのマシンのdaemonにつなぐ（`ssh HOST cq bridge` 用） |
| `cq NAME …` | 組み込みにない名前は `cq-NAME …` を実行する（例 `cq queen`、`cq board`） |

`TARGET` は `[machine:]名前|id|idの前方一致`。内部用に `cq host`、`cq hook`、`cq statusline`、`cq debug-screen` がある。

### cq queen（cq-queen）

| コマンド | 内容 |
| --- | --- |
| `cq queen start [NAME] [--detach] [-- エージェントの引数…]` | Queenを起動する（既定の名前は `queen`）。同名のQueenが動いていれば何もしない。`--detach` は端末に付けずに起動する |
| `cq queen config [NAME]` | 設定ファイルのパスを出す（無ければ既定値で作る） |

エイリアスの既定で `cq q` は `cq queen` になる。

### cq board（cq-board）

ダッシュボードを開く。

### MCPツール（`cq mcp`）

| ツール | 内容 |
| --- | --- |
| `list_sessions` | 範囲内のセッションの一覧（マシン、名前、cwd、状態、承認待ちのプロンプト、最後の発言と返答の冒頭） |
| `get_turns` | セッションの直近のやり取り |
| `send` | セッションに文字を送る（名前で指定） |
| `approve` | 承認のプロンプトに答える（プロンプトIDを照合） |

### ダッシュボードの組み込みコマンド

| コマンド | 内容 | 既定のキー |
| --- | --- | --- |
| `ui.palette` | コマンドパレット | Ctrl+Shift+P |
| `ui.nextTile` / `ui.prevTile` | 次 / 前のタイル | Ctrl+Shift+→ / ← （PageDown / PageUp も） |
| `ui.closeTile` | タイルを閉じる（デタッチ） | Ctrl+Shift+W |
| `ui.splitRight` / `ui.splitDown` | 右 / 下に分割して次のセッションを開く | Ctrl+Shift+\ / Ctrl+Shift+- |
| `ui.approvals` | 承認待ちを表示 | Ctrl+Shift+A |
| `ui.openSession` | 番号のセッションを開く（引数 `index`） | Alt+1〜9 |
| `ui.toggleUsage` | 使用量の表示を開閉 | なし |
| `ui.newSession` | 新しいセッション（引数 `machine`） | なし |
| `session.end` / `session.pin` / `session.copyId` | フォーカス中のセッションを終了 / ピン留めの切り替え / idのコピー | なし |
| `daemon.restart` | daemonを再起動（引数 `machine`） | なし |

### プラグインのコマンド

| コマンド | プラグイン | 内容 |
| --- | --- | --- |
| `queen.start` | queen | Queenを起動する（引数 `name`、既定 `queen`） |

## 設定

### `~/.codequilt/config.toml`

```toml
allowed_roots = ["C:/Users/me/work"]   # where sessions may be started from the board; default: home

[aliases]
q = "queen"                 # built in; user aliases take precedence
qs = "queen start main"     # several words are fine

[[machines]]
id = "wsl"
label = "wsl"
ssh = "wsl"                                   # host as known to ssh
cq_path = "/home/me/.cache/code-quilt/target/debug/cq"   # optional; default "cq"
login_shell = true                            # run `cq bridge` via `sh -lc` so ~/.profile applies
```

組み込みのサブコマンドとその別名（`a`）はエイリアスで上書きできない。

### `~/.codequilt/ui.toml`

無ければダッシュボードの起動時に次の内容で作られる。

```toml
pinned = ["queen"]          # session names also shown at the top of the list

[[on_start]]                # commands run in order when the board starts
command = "queen.start"
args = { name = "queen" }

[terminal]
font_family = []            # tried before the built-in fallbacks
font_size = 13
```

### `~/.codequilt/keybindings.toml`

```toml
[[binding]]
key = "ctrl+shift+q"
command = "queen.start"
args = { name = "main" }

[[binding]]
key = "ctrl+shift+w"
command = "-ui.closeTile"   # "-" removes a default binding (all of its keys without `key`)
```

- 優先順位はユーザー > プラグインの既定 > 組み込みの既定。上書きや未知のコマンドはパレットの下に警告として出る。
- キーは配列に依存しない（`KeyboardEvent.code` で判定）。割り当てたキーはターミナルより先に処理されるので、Ctrl+C などを割り当てるとエージェントに届かなくなる。

### `~/.codequilt/queens/NAME.toml`

無ければ既定値で作られる。

| キー | 既定 | 内容 |
| --- | --- | --- |
| `agent` | `"claude"` | Queenのエージェント |
| `model` | `"sonnet"` | モデル |
| `persona` | `"default"` | 人格（指示文）。`default` かファイルのパス |
| `scope` | `[]` | 担当するマシン（`machine:ID`）。空なら全マシン |
| `delivery` | `"channels"` | 届け方。`channels` か `pty` |
| `wake_on` | `["permission", "question", "stop"]` | Queenに届けるイベント |
| `allow_approve` | `false` | `approve` ツールを確認なしで使わせる |
| `cwd` | `~/.codequilt/queens/NAME` | Queenのセッションを動かすディレクトリ |
| `batch_seconds` | `2.5` | まとめて届けるまでの秒数 |

## データの置き場所

`~/.codequilt/` の下に置く。

| パス | 内容 |
| --- | --- |
| `sessions/<id>.json` | セッションのメタデータ |
| `logs/` | daemon・セッション・プラグインのログ |
| `plugins/<name>.json` | 導入済みプラグインのmanifest |
| `claude.json`、`settings/<id>.json` | Claude Codeに注入する設定 |
| `queens/` | Queenの設定と、生成したMCP設定・hooks設定 |
| `run/` | Linuxのソケット |
