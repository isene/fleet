# fleet

![Rust](https://img.shields.io/badge/language-Rust-orange) ![Unlicense](https://img.shields.io/badge/license-Unlicense-green) [![Fe2O3](https://img.shields.io/badge/suite-Fe%E2%82%82O%E2%82%83-b7410e)](https://github.com/isene/fe2o3)

Claude Code mission control. One screen for every session on the
machine: who is working, who waits for you, on which workspace, with
what context size. Plus an inbox pane for the folders where handoffs
land (screenshots, phone transfers). Part of the
[Fe2O3](https://github.com/isene/fe2o3) Rust terminal suite.

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
  `idle`, `off` (no process)
- Age of last activity, workspace of its terminal window, context size,
  model, and the last prompt
- Sorted so YOURS floats to the top

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
- `↑` / `↓` select
- `Enter` on a session: show which workspace it is on
- `o` / `Enter` on an inbox item: open it
- `D` on an inbox item: delete it (asks y/n)
- `q` quit

`fleet --list` prints the same information as plain text and exits.

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
```

The defaults are exactly the block above: laptop screenshots in the
home directory, phone items in `~/.transfer`.

## Battery posture

A 2 second tick while open, nothing after `q`. Each tick is one `stat`
per session file (transcript tails are re-read only when mtime
changed), one `readdir` per inbox folder, and one `/proc` sweep for
claude pids.

## Roadmap

- A drop-folder message bus between sessions
- Jump straight to a session's window
- Cost and token rollup per session per day

## License

Public domain (Unlicense). Do what you want with it.
