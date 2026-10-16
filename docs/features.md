# CodeQuilt 機能一覧

今の実装でできることと、コマンド・設定の一覧。仕組みは [design.md](design.md)、判断の経緯は [decisions.md](decisions.md) にある。

## 機能

### セッション

- `cq claude` / `cq codex` でエージェントをCodeQuiltのセッションとして起動する。手元の端末にそのまま表示され、Ctrl+\ で切り離しても動き続ける。
- 切り離したセッションは `cq attach`（`cq a`）でどの端末からでも付け直せる。ダッシュボードのペインからも同時に操作できる。
- セッションは名前・id・idの前方一致で指定できる。名前は動いているセッションの中でマシンごとに一意。
- 終了は `cq kill` かダッシュボードの「End session」。エージェント自身に終了させ、10秒で終わらなければ強制終了する。
- ダッシュボードの「New session」では、そのマシンのClaude CodeやCodexで信頼済みのフォルダが一覧に出る。それ以外は「Choose another folder…」で `allowed_roots` の配下から選ぶ。
- 終了したセッションはresumeできる（同じcqのIDのまま、同じディレクトリと名前で、エージェントの会話を引き継いで起動し直す）。boardのEndedの「Resume」か `cq resume ID`。最初のターンの前に終わったセッションはエージェント側に会話がない（Codexは最初のターンで会話を作る）ので、同じID・ディレクトリ・名前でエージェントを新しく起動する。
- セッション同士でメッセージを送り合える。cqで起動したセッション（オーケストレーターを除く）には、cqのMCP（`cq mcp --worker`）が確認なしで使える形で入っていて、エージェントは `list_sessions` / `get_turns` でほかのセッションを見て、`message` で依頼や返事を残せる。メッセージは相手がアイドルになってから `[from 送信元のセッション名 (マシン名:セッションid)]`（名前の無いセッションは `[from マシン名:セッションid]`）の行を先頭に付けて入力される。名前は受け取ったマシンでの呼び方で、そのマシンが知らなければ送信元のマシンに聞き、それでも分からなければidを出す。入れたくないときは `--cq-no-mcp`。
- エージェントが返答で言及した画像・PDF・HTML・Markdownのファイルと、返答の中のmermaidの図や数式（` ```math ` と `$$…$$`）は、ダッシュボードでそのセッションの端末の右に並び、そこから開ける（リモートのセッションも同じ）。TUIでは描画できないものの代わりの表示。

### マシン

- sshで届くマシンを登録すると、そのマシンのセッションも同じ一覧・同じ操作で扱える（Windows・Linux）。
- 接続にはパスワードを聞かれないssh（鍵認証。パスフレーズ付きの鍵はssh-agentに載せる）が必要。鍵の用意はユーザーが行う。ユーザー名・ポート・鍵などは `~/.ssh/config` に書いてもよく、sshがそのまま使う。
- `cq machine add [USER@]HOST` は、daemonと同じ方法でそのマシンにつないでOSを調べ、`cq bridge` がこのcqと同じプロトコルで応えてそのマシンのidと名前を名乗ることを確かめてから登録する。手元のマシンや登録済みのマシンは断る。つながらなければsshの出したエラーを表示して登録しない。
- 各マシンは初めて使うときに自分のid（`~/.codequilt/machine_id`）を作り、どこから見ても同じidで指される。人が使うのは名前で（`名前:セッション`、`scope` の `machine:名前`）、手元のマシンの名前は `config.toml` の `name`（既定はホスト名）、登録したマシンの名前は既定でそのマシンが名乗る名前。どれも `cq machine set` で変えられ、名前を変えてもペインの配置はそのまま残る。
- つながらないマシンは、sshが出した最後のエラーをboardのMachines欄（オフラインの表示にカーソルを置く）と `cq daemon status` に出す。
- リモートのマシンのセッション（ワーカーなど）からも、sshを張ってきたマシン（上流）とそこに登録されたマシンのセッションが見え、メッセージを送れる。上流のマシンでboardなどのクライアントが動いている間だけで、見えるマシンの名前は上流が付けた名前。
- 各マシンのdaemonは必要になったときに自動で起動する。止めたり入れ替えたりしても、セッションは動き続ける。

### ダッシュボード（cq-board）

- **左サイドバー**（開閉できる）：Tabs（タブごとに中のセッションと空のペイン。ファイルのペインは出さない。選ぶとそのタブとペインへ移る）、Machines（マシンごとに、Active（動いているセッションを作業ディレクトリごとにまとめ、ディレクトリの名前の順に並べる）と、既定で折りたたんだEnded（終了したセッションを作業ディレクトリごとにまとめ、最近使ったディレクトリから並べる））の2つの欄。作業ディレクトリはboardのどこでも末尾の名前で出し、カーソルを置くとフルパスが出る。欄や項目は折りたためる。セッションの行はどの欄でも同じで、既定で1行（折りたたみの ＞ の右にエージェント（Claude / Codex）を色分けした斜線のバッジ、名前・id・状態・終了）、＞ で詳細（`machine:`、`cwd:`、`model:`、`context:`）を開く。詳細は欄ごとに別々に開く。名前とidの列の幅は、どの行でも列の右端をドラッグして変えられ、全部の欄で共有される。idはクリックでコピーでき、名前はクリックすると変えられる（ペインの見出しの名前も同じ。Enterで確定、Escで取り消し、空にすると名前を消す。動いているほかのセッションと同じ名前は断る）。左右のサイドバーは上部の右端のボタンで開閉し、幅は境界をドラッグして変えられる。
- **タブとペイン**：メインエリアのタブごとに、ペインを左右・上下に分割して並べる（tmuxのwindowとpaneと同じ）。一覧からセッションを開くと、今のタブに無ければ、フォーカス中のペインが空ならそこへ、空でなければ親と逆の向きに分割して入れる。同じセッションを複数のタブに置ける。ペインの見出しに名前・id・状態と、ペインを閉じる✕（セッションは動き続ける）がある。セッションの終了は左サイドバーの行にホバーすると出るゴミ箱。境界はドラッグで動かせる。見出しを掴んでドラッグすると、別のペイン（落とした位置に近い辺の方向に分割して移る。空のペインも同じ）、タブ（そのタブへ移り、そのタブを表示）、＋（新しいタブへ移る）に動かせ、離したときの結果がプレビューされる。タブの上で少し待つとそのタブが表示され、そこで置き場所を選べる。移す先のタブに同じセッションがあるときや置き場所のない所で離したときは、移さずにトーストで知らせる。ペインがなくなったタブは閉じる。タブもドラッグで並べ替えられ、ドラッグ中は並べ替え後の順に表示される。配置は保存され、終了したセッションのペインは自動で閉じる（見せたファイルのペインは残る）。
- **返答で見せたファイル**：エージェントのターンが終わるたびに、返答から次を拾い、そのセッションのペインの端末の右に縦に、新しいものが下になるように並べる（画像とMarkdownは縮小表示、それ以外はファイル名。カーソルを置くとフルパスが出る）。同じパスは古いものを消して一番下に出し直す。
  - 返答で言及された、画像（png / jpg / jpeg / gif / webp / svg / bmp / ico）・PDF・HTML・Markdownのファイル。パスでもファイル名だけでもよく、ファイル名だけならセッションの作業ディレクトリの下（`.gitignore` されたものと隠しディレクトリを除く）で一致するもののうち最も新しく更新されたもの。存在しないものは出さない。ユーザーの発言は見ない。
  - ` ```mermaid ` ・` ```math ` のブロックと、行頭から始まる `$$ … $$`。中身をそれぞれ1つのファイルにして出す（文中の `$…$` は対象外）。
  - 選ぶとセッションのペインを右に分割して開く（同じタブに開いていればそこへ移る）。見出しにはファイル名と見せたセッションの名前を出し、カーソルを置くとパスが出る。画像はそのまま、PDFはWebViewのビューア、HTMLはスクリプトだけ許したsandboxの中、Markdownは整形して（mermaidは図、数式は組版して）、それ以外はテキストとして表示する（テキストでないものは表示できない旨を出す）。同じパスがもう一度出ると読み直す。
- **承認**：承認の要求や確認ダイアログ（フォルダの信頼、開発用チャネルなど）が出たら、右下にセッション・マシン・cwd・内容・選択肢を示すカードを出す。カードは入力のフォーカスを奪わず（フォーカスのあるカードだけ枠が明るいオレンジ、ほかは暗いオレンジ）、最小化しても右サイドバーのApprovalsに残り、選ぶともう一度出る（`ui.approvals` で最新のカードにフォーカス）。手元の端末で答えてもカードで答えてもよい。承認待ちのセッションのペインは枠の色で知らせる。
- **右サイドバー**（開閉できる）：上にLimits（エージェント（バッジ）ごと・アカウントごとの5hとweeklyのレート制限、使用率とリセット時刻）、下にApprovals（承認待ちの一覧）。
- **コマンドパレット**（Ctrl+Shift+P）：組み込みとプラグインのコマンドを検索して実行する。引数のあるコマンドは既定値つきで入力を求める。パレットから最近実行した5件は「recently used」として新しい順に先頭に出る（検索中も一致したものは先頭）。
- **キーバインド**：すべてのコマンドにキーを割り当てられる。`ui.keybindings` で開く画面で、コマンドとキーと出どころ（default / plugin / user）を検索・一覧し、「bind」を押してからキーを押すと、その行のキーをそのキーに置き換える（1行に1キー）。そのキーが別のコマンドに割り当て済みなら、確認のうえ付け替える。×で外す。引数付きの割り当て（`tab.goTo` の Alt+1〜9 など）は引数ごとに別の行になる。変更は `keybindings.toml` にコメントを保ったまま書き込まれ、直接編集してもよい。
- **起動時の処理**：`ui.toml` の `on_start` に並べたコマンドを起動時に実行する（既定はオーケストレーターの起動）。
- **トースト**：コマンドの結果と失敗は右下に出す。OS通知は出さない。
- **フォント**：`ui.toml` でターミナルのフォントと大きさを変えられる。

### オーケストレーター（cq-orchestrator）

- 他のセッションの承認待ち・質問・ターン終了をまとめて報告し、ユーザーの指示や承認を各セッションに送るオーケストレーター。オーケストレーター自身は代わりに判断しない。
- 頼めば、指定したディレクトリで新しいセッションを起動し、最初の指示を渡す（確認なし）。
- エージェントはClaude CodeかCodex。担当するマシン、モデル、人格、届け方は設定で変えられ、起動時に `KEY=VALUE` で一時的に上書きできる（例 `agent=codex`）。名前の違うオーケストレーターを複数動かせる。
- どのマシンでも、そのマシンで `cq orchestrator start` すれば動く。マシンごとにオーケストレーターを置き、`scope` で自分のマシンだけを担当させることもできる。`scope` は報告するマシンを絞るだけで、オーケストレーターは頼めばどのマシンのセッションにも話しかけられる。設定は `cq orchestrator set` か、オーケストレーターに頼んで変えられ、報告に関わるもの（`scope`、`wake_on`、`batch_seconds`）は動いているオーケストレーターにもすぐ反映される。リモートのオーケストレーターとも、手元のboardからほかのセッションと同じように対話できる。オーケストレーター同士はマシンをまたいでも互いを報告しない。

### プラグイン

- `cq-*` という実行ファイルは `cq *` のサブコマンドとして呼べる。本体の隣にあるものは自動で導入済みになり、宣言したコマンドがパレットやキーやCLI（`cq run`）から使える。

## コマンド

### cq

| コマンド | 内容 |
| --- | --- |
| `cq claude [--cq-name NAME] [--cq-no-mcp] [--cq-orchestrator] [claudeの引数…]` | claudeをセッションとして起動し、この端末に付ける。`--cq-no-mcp` はcqのMCPを入れない。`--cq-orchestrator` はオーケストレーターの印を付け（courierはどのマシンでもこのセッションを報告しない）、cqのMCPを入れない（`cq orchestrator start` が使う） |
| `cq codex [--cq-name NAME] [--cq-no-mcp] [--cq-orchestrator] [codexの引数…]` | codexをセッションとして起動し、この端末に付ける |
| `cq attach TARGET`（`cq a`） | 動いているセッションにこの端末を付ける。Ctrl+\ で切り離す |
| `cq resume ID` | 終了したセッション（cqのIDか一意な前方一致）を同じIDのまま起動し直し、この端末に付ける。エージェント・ディレクトリ・名前は記録から引き、`claude --resume` / `codex resume` で会話を引き継ぐ |
| `cq ls [-a | --all]` | このマシンの動いているセッションの一覧。`--all` は終了したものも出す |
| `cq rename TARGET [NAME] [--clear]` | 名前を付ける（重複は断る）／消す |
| `cq inject TARGET TEXT [--submit] [--quiet-ms N]` | セッションがアイドルで誰も入力していないときに文字を入れる |
| `cq kill TARGET [--force] [--grace SECONDS]` | セッションを終了させる |
| `cq usage [--local] [--json]` | アカウントごとのレート制限と、動いているセッション（モデル、コンテキストの使用率） |
| `cq run NAME [--arg KEY=VALUE]…` | プラグインのコマンドを実行する |
| `cq plugin list \| add NAME \| remove NAME \| refresh` | プラグインの一覧・導入・削除・manifestの再読込 |
| `cq machine list` (`ls`) | 手元のマシンと登録したマシンの一覧（名前、接続先、OS、cqのパス） |
| `cq machine add [USER@]HOST [-p PORT] [-n NAME] [--cq-path P]` | マシンを登録する。HOSTはIPアドレス、ホスト名、`~/.ssh/config` のHostのどれでもよい。つないで確かめてから登録する |
| `cq machine set MACHINE [-n NAME] [-H [USER@]HOST] [-p PORT] [--cq-path P]` | 登録した内容を変える。接続先・ポート・cqのパスを変えたときはつなぎ直して確かめ、別のマシンにつながったら断る。`-p 0` と `--cq-path ""` で既定に戻す。手元のマシンは名前だけ変えられる |
| `cq machine remove MACHINE` (`rm`) | 登録を消す |
| `cq daemon` | daemonを動かす（通常は自動で起動される） |
| `cq daemon status \| stop \| restart` | daemonの状態（ローカルと各リモートマシン）、ローカルのdaemonの停止・再起動。止めてもセッションは動き続ける。リモートのdaemonはそのマシンで `cq daemon restart` する。プロトコルのバージョンが違うdaemonは停止の要求を受け付けないため、止められなかったときはそのPIDを表示して失敗する |
| `cq mcp [--channel] [--worker]` | MCPサーバー（stdio）。全マシンのセッションを扱う。`--channel` はClaude Codeのチャネルとしても動く。`--worker` はワーカー用（`list_sessions` / `get_turns` / `message` だけ） |
| `cq bridge` | 標準入出力をこのマシンのdaemonにつなぐ（`ssh HOST cq bridge` 用） |
| `cq NAME …` | 組み込みにない名前は `cq-NAME …` を実行する（例 `cq orchestrator`、`cq board`） |

`TARGET` は `[マシン名:]名前|id|idの前方一致`。マシン名を省くと全マシンから探し、複数見つかったら断る。内部用に `cq host`、`cq hook`、`cq statusline`、`cq debug-screen` がある。

### cq orchestrator（cq-orchestrator）

| コマンド | 内容 |
| --- | --- |
| `cq orchestrator start [NAME] [KEY=VALUE]… [--detach] [-- エージェントの引数…]` | オーケストレーターを起動する（既定の名前は `orchestrator`）。`KEY=VALUE` は `NAME.toml` のキーをこの起動だけ上書きする（値はTOMLとして読み、読めなければ文字列。空の値は上書きしない）。同名のオーケストレーターが動いていれば何もしない。`--detach` は端末に付けずに起動する |
| `cq orchestrator show [NAME]` | 設定ファイルのパスと中身を出す（無ければ既定値で作る） |
| `cq orchestrator set [NAME] KEY=VALUE…` | 設定を `NAME.toml` に保存する（値の読み方は `start` と同じ。空の値は既定に戻す。コメントは残る）。不正な値なら保存しない。動いているオーケストレーターには `scope`・`wake_on`・`batch_seconds` をすぐ反映し、それ以外は次の起動から効く |

エイリアスの既定で `cq o` は `cq orchestrator` になる。

### cq board（cq-board）

ダッシュボードを開く。

### MCPツール（`cq mcp`）

オーケストレーターはすべてのツールを、ワーカー（`--worker`）は `list_sessions` / `get_turns` / `message` だけを使える。

| ツール | 内容 |
| --- | --- |
| `list_sessions` | 範囲内のセッションの一覧（マシン、名前、cwd、状態、承認待ちのプロンプト、最後の発言と返答の冒頭） |
| `get_turns` | セッションの直近のやり取り |
| `message` | ほかのセッションにメッセージを残す。相手がアイドルになってから、送信元を先頭に付けて入力される（ワーカーも使える） |
| `send` | セッションに文字を送る（名前で指定） |
| `approve` | 承認のプロンプトに答える（プロンプトIDを照合） |
| `launch` | 指定したディレクトリで新しいセッションを起動する（エージェント、マシン、名前、最初の指示を指定できる。ディレクトリは `allowed_roots` の配下かエージェントで信頼済みのものに限る。最初の指示はセッションがアイドルになってから入る） |

### ダッシュボードの組み込みコマンド

| コマンド | 内容 | 既定のキー |
| --- | --- | --- |
| `ui.palette` | コマンドパレット | Ctrl+Shift+P |
| `ui.keybindings` | キーバインドの画面の開閉 | なし |
| `ui.approvals` | 承認待ちを表示 | Ctrl+Shift+A |
| `ui.toggleLeftSidebar` | 左サイドバーの開閉 | なし |
| `ui.toggleRightSidebar` | 右サイドバーの開閉 | なし |
| `ui.newSession` | 新しいセッション（引数 `machine` はマシンの名前） | なし |
| `tab.new` / `tab.close` / `tab.rename` | タブの追加 / 閉じる（セッションは動き続ける） / 名前の変更（引数 `name`） | Ctrl+Shift+T / なし / なし |
| `tab.next` / `tab.prev` | 次 / 前のタブ | Ctrl+Shift+PageDown / PageUp |
| `tab.goTo` | 番号のタブ（引数 `index`） | Alt+1〜9 |
| `pane.splitLeft` / `Right` / `Up` / `Down` | その方向に空のペインを作る | なし / Ctrl+Shift+\ / なし / Ctrl+Shift+- |
| `pane.focusLeft` / `Right` / `Up` / `Down` | その方向のペインへフォーカス | Ctrl+Shift+← / → / ↑ / ↓ |
| `pane.close` | ペインを閉じる（セッションはデタッチ。空のペイン1つだけのタブではタブを閉じる） | Ctrl+Shift+W |
| `pane.zoom` | ペインを最大化 / 戻す | Ctrl+Shift+Z |
| `session.end` / `session.copyId` | フォーカス中のセッションを終了（確認のモーダルを出す） / idのコピー | なし |
| `daemon.restart` | ローカルのdaemonを再起動し、結果をトーストで知らせる（止められなかったときは `cq daemon restart` と同じくPIDと理由を出す） | なし |

### プラグインのコマンド

| コマンド | プラグイン | 内容 |
| --- | --- | --- |
| `orchestrator.start` | orchestrator | オーケストレーターを起動する（引数 `name`、既定 `orchestrator`。`agent` は `claude` か `codex`、空なら設定のまま） |
| `orchestrator.set` | orchestrator | 設定を1つ保存する（引数 `name` と `setting`（`KEY=VALUE`）。`cq orchestrator set` と同じ。boardのマシンのオーケストレーターが対象） |

## 設定

### `~/.codequilt/config.toml`

```toml
name = "desktop"               # this machine's name; default: its host name
allowed_roots = ["C:/Users/me/work"]   # folders under which the board may start sessions (besides trusted ones); default: home

[aliases]
o = "orchestrator"              # built in; user aliases take precedence
os = "orchestrator start main"  # several words are fine

[[machines]]                  # written by `cq machine add` / `set`
id = "3f9a2c1b0d4e"           # the id the machine made for itself (its ~/.codequilt/machine_id)
name = "wsl"                  # what you type: wsl:SESSION
ssh = "me@192.168.1.10"       # [user@]host as given to ssh
port = 2222                   # optional
os = "unix"                   # detected; unix starts `cq bridge` via `sh -lc` so ~/.profile applies
cq_path = "/home/me/.cache/code-quilt/target/debug/cq"   # optional; default "cq"
```

組み込みのサブコマンドとその別名（`a`）はエイリアスで上書きできない。

### `~/.codequilt/ui.toml`

無ければダッシュボードの起動時に次の内容で作られる。

```toml

[[on_start]]                # commands run in order when the board starts
command = "orchestrator.start"
args = { name = "orchestrator" }

[terminal]
font_family = []            # tried before the built-in fallbacks
font_size = 13
```

### `~/.codequilt/keybindings.toml`

```toml
[[binding]]
key = "ctrl+shift+q"
command = "orchestrator.start"
args = { name = "main" }

[[binding]]
key = "ctrl+shift+w"
command = "-pane.close"     # "-" removes a default binding (all of its keys without `key`)
```

- 優先順位はユーザー > プラグインの既定 > 組み込みの既定。1つのキーは1つのコマンドだけを実行する。未知のコマンドや書式の誤りはパレットの下に警告として出る。
- キーは配列に依存しない（`KeyboardEvent.code` で判定）。割り当てたキーはターミナルより先に処理されるので、Ctrl+C などを割り当てるとエージェントに届かなくなる。

### `~/.codequilt/orchestrators/NAME.toml`

無ければ既定値で作られる。`NAME` はオーケストレーターの名前（既定 `orchestrator`、セッション名にもなる）。知らないキーはエラーになる。起動時の `KEY=VALUE` はその起動だけ、`cq orchestrator set` は保存する。

| キー | 既定 | 内容 |
| --- | --- | --- |
| `agent` | `"claude"` | オーケストレーターのエージェント（`claude` か `codex`） |
| `model` | Claudeは `"sonnet"`、Codexはユーザーの既定 | モデル。既定のときはファイルに書かれない |
| `persona` | `"default"` | 人格（指示文）。`default` かファイルのパス |
| `scope` | `[]` | 報告するマシン（`machine:名前`）。空なら全マシン。オーケストレーターのツールはこれによらず全マシンを扱う |
| `delivery` | Claudeは `"channels"`、Codexは `"pty"` | 届け方。`channels` か `pty`。Codexは `pty` だけ。既定のときはファイルに書かれない |
| `wake_on` | `["permission", "question", "stop"]` | オーケストレーターに届けるイベント |
| `allow_approve` | `false` | `approve` ツールを確認なしで使わせる |
| `cwd` | `~/.codequilt/orchestrators/NAME` | オーケストレーターのセッションを動かすディレクトリ |
| `batch_seconds` | `2.5` | まとめて届けるまでの秒数 |

## データの置き場所

`~/.codequilt/` の下に置く。

| パス | 内容 |
| --- | --- |
| `machine_id` | このマシンのid（初めて使うときに作る） |
| `sessions/<id>.json` | セッションのメタデータ（アカウント、返答で見せたファイルの一覧を含む） |
| `blocks/<種類>-<ハッシュ>.md` | 返答から取り出したmermaidの図と数式（中身が同じなら同じファイル。消さない） |
| `logs/` | daemon・セッション・プラグインのログ |
| `plugins/<name>.json` | 導入済みプラグインのmanifest |
| `claude.json`、`settings/<id>.json` | Claude Codeに注入する設定 |
| `orchestrators/` | オーケストレーターの設定と、生成したMCP設定（Claude） |
| `run/` | Linuxのソケット |
