# fleet

![Rust](https://img.shields.io/badge/language-Rust-orange) ![Release](https://badgen.net/github/release/isene/fleet) ![Unlicense](https://img.shields.io/badge/license-Unlicense-green) [![Fe2O3](https://img.shields.io/badge/suite-Fe%E2%82%82O%E2%82%83-b7410e)](https://github.com/isene/fe2o3)

<img src="img/fleet.svg" align="left" width="150" height="150">

Claude Code mission control. One screen for every session on the
machine: who is working, who waits for you, on which workspace, with
what context size. Plus an inbox pane for the folders where handoffs
land (screenshots, phone transfers). Part of the
[Fe2O3](https://github.com/isene/fe2o3) Rust terminal suite.

<br clear="left"/>

## Sessions that work together

Sessions send each other messages, and act on them, with no relay from
you. One session finds a fault in another project and writes to the
session that owns it. That session gets the message when its turn ends,
or when fleet wakes it at its prompt. Then it starts on the job.

You stop carrying messages between your own sessions. More gets done at
once, and you steer less of it by hand.

What keeps you in charge:

- every message shows in the INBOX pane, and `M` lists them all
- auto-wake is set per session with `a`, at most 4 times in 30 minutes
- a session gets its messages once per turn, so two sessions cannot keep
  answering each other
- fleet never types into the window you are working in

Set it up under [Message bus](#message-bus).

## Why

Running several Claude Code sessions in parallel means losing track of
them. Which one finished and waits for an answer? Which workspace is it
on? Did that screenshot from the phone arrive? fleet answers all of
that at a glance, in one TUI.

## What it shows

**Sessions** (from `~/.claude/projects` transcripts):

- Tag (from [CC-sessions](https://github.com/isene/CC-sessions)
  bookmarks when present, else the project directory name)
- State: `working` (Claude has the turn), `YOURS` (waiting for you),
  `CAPPED` (a usage limit refused the model; needs `/model` or credits),
  `idle`, `off` (no process)
- Age of last activity, workspace of its terminal window, context size,
  model, and the last prompt
- Sorted so CAPPED and YOURS float to the top
- Bookmarked sessions older than the recent window stay listed at the
  bottom as `older` (dim); unbookmarked old ones are dropped

**Inbox** (configurable watch folders):

- Files that recently arrived, newest first
- `o` opens the selected item, `D` deletes it (with confirm)

## Install

```sh
git clone https://github.com/isene/fleet
cd fleet
cargo build --release
ln -s "$PWD/target/release/fleet" ~/bin/fleet
```

## Keys

- `TAB` switch between sessions and inbox
- `↑` / `↓` select (shown as a background bar, colors kept)
- `Enter` on a session: raise its own glass (one of several stacked on
  a workspace) and jump to that workspace (xdotool key injection),
  or, when it has no window, resume it in a new glass terminal (detached
  through `setsid`, so it outlives fleet). A session on a gateway model
  (`cck`: Kimi through OpenRouter) is resumed through `cck`, so it stays
  on that model
- `m` on a session: type a message, Enter drops it on the bus
- `y` on a session: copy its session id to the clipboard, to hand another
  Claude session so it can read what happened there
- `w` on a session: set the workspace its glass opens on (1-9, `0` for
  10, `-` clears). The prompt is prefilled with the current value.
  Saved to `~/.fleetrc`, applied next time you resume it
- `b` on a session: pick its glass background with `prism`, preloaded
  with the current colour. Saved to `~/.fleetrc`, applied next resume
  via the `GLASS_BG` env
- `t` on a session: set its window title, `#tag` when left empty. An open
  window is renamed at once. A resumed one starts with the title, and
  Claude Code is told to keep it (`CLAUDE_CODE_DISABLE_TERMINAL_TITLE`)
- `k` stop the selected session (SIGTERM; `K` forces): it goes "off"
  and stays resumable with Enter. A parked session loses its parking
  here, since "off" is what the row should say
- `p` park the selected session, or bring it back. A parked session
  stays listed, drops out of the YOURS and working counts, and sorts to
  the top as a block that does not move. Work clears it: the moment that
  session is busy again it shows as working, and the parking is gone
- `a` auto-wake for the selected session, on or off (shown as `»`).
  When a bus message lands for it, fleet asks it to check its messages
  by itself. Only while Claude waits at its prompt, never in the window
  you are typing in, and at most 4 times in 30 minutes
- `c` today's token rollup per session (Esc back)
- `v` popup with your open moves, from all sessions. A session that
  needs something from you starts a line of its answer with
  `Your move:`, and puts what you must decide in a table with a
  `Question` column. fleet lists those lines and table rows from each
  session's last answer, and the header counts them. Up and Down move
  the bar, and Enter jumps to that row's session. Your next prompt to
  a session clears its rows
- `o` / `Enter` on an inbox item: open it. A program that runs in a
  terminal (an editor, a PDF reader) gets a glass of its own
- `o` / `Enter` on a `msg` row: hand the message to the session it is
  for. A live session is told to read its mail right away; one that is
  off or old is resumed in a fresh glass first, then told once it is up
- `<` on a `msg` row: clear that message. Delivery clears the rest by
  itself, so this is for one nothing will collect: a wrong tag, a
  session that is gone, or something you handled another way
- `d` flag for deletion (dark-red row, advances): inbox items, and
  idle/off sessions (transcript plus its subagent sidecar)
- `<` delete everything flagged
- `M` popup with the full bus message log (pending messages show as
  dim rows in the INBOX pane)
- `?` popup help with all keys
- `q` quit

Colors follow the Claude Code statusline: bookmark tags magenta, model
bold blue, ages gray, and context size on the statusline's own scale:
green under 50 % of the window, yellow under 75 %, red above.

`fleet --list` prints sessions and inbox as plain text; `fleet --today`
prints the rollup; `fleet --moves` prints your open moves. `fleet --wake
<tag>` types the check-messages prompt into that open session, the same
as Enter on an inbox row. All four exit immediately.

## Message bus

A message to a session is a file. `~/.fleet/bus/<addr>/` is the
mailbox, where `<addr>` is the session's CC-sessions bookmark tag (or
its raw session id). fleet's `m` key writes there; so can any Claude
session, which makes the path convention the whole API.

Delivery: the `hooks/fleet-bus` UserPromptSubmit hook injects pending
messages as context on the receiving session's next user prompt, then
deletes them. Install:

```sh
ln -s "$PWD/hooks/fleet-bus" ~/.claude/hooks/fleet-bus
```

and add it to `~/.claude/settings.json` under `hooks.UserPromptSubmit`:

```json
{ "type": "command", "command": "~/.claude/hooks/fleet-bus" }
```

When idle the hook is one stat of a usually absent directory.

The same hook can also run under `hooks.Stop`. A session that is already
working then gets its messages when its turn ends, with no prompt from
you. It delivers once per turn, so two sessions cannot keep answering
each other. The shell test in front skips Python when no mailbox holds
anything:

```json
{ "type": "command", "timeout": 10,
  "command": "sh -c 'for f in \"$HOME\"/.fleet/bus/*/* \"$HOME\"/.fleet/relay/*/*; do [ -f \"$f\" ] && exec \"$HOME\"/.claude/hooks/fleet-bus; done; exit 0'" }
```

## Configuration

`~/.fleetrc`, plain text, `#` comments. Any `inbox` line replaces the
built-in watches, so the tool adapts to where YOUR items land:

```
# inbox <label> <dir> <glob>
inbox scrots ~           *_scrot.png
inbox phone  ~/.transfer *
recent_days 7      # sessions younger than this are listed
idle_mins 30       # older than this and a live session shows "idle"
inbox_days 3       # inbox items younger than this are shown
ctx_window_k 1000  # context window; CTX goes yellow at 50 %, red at 75 %

# session <tag> <ws> [bg]   where a resumed session's glass opens.
#   ws is 1-based (- for none); bg is BARE hex, no leading # (the file
#   reads # as a comment). Set both live from the TUI with w and b.
session system 1
session asm    2
session rust   3
session DI     6 1a1a2e
```

The defaults are the inbox/recent/idle/inbox_days block above: laptop
screenshots in the home directory, phone items in `~/.transfer`. The
`session` lines are per-machine and easiest to set with `w` / `b` in
the TUI.

## Battery posture

A 2 second tick while open, nothing after `q`. Each tick is one `stat`
per session file (transcript tails are re-read only when mtime
changed) and one `readdir` per inbox folder. The token rollup reads
whole transcripts, so it runs only on demand (`c` or `--today`), never
on the tick.

Two costs were measured and cut, from 170 wakeups a second to 21:

- The workspace map asked X for two properties per window and waited for
  each answer, ~370 blocking round trips a tick on a busy desktop. The
  requests are queued now, and the replies read in one pass.
- The `/proc` sweep for claude pids reads one `comm` per process on the
  machine. It runs every 10 seconds now; in between, each known pid is
  checked with a single `stat`, so a session that ends still drops out
  at once and one that starts appears within ten seconds.

## License

Public domain (Unlicense). Do what you want with it.
