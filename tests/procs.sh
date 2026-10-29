#!/bin/bash
# Starts two fake claude processes and checks that fleet sees both as
# running: one named by --resume, one found by the folder it runs in.
# This is the only part of fleet that differs between Linux and macOS.
#   tests/procs.sh [fleet]     prints "all cases right" or each wrong one
F=$(cd "$(dirname "${1:-target/release/fleet}")" && pwd)/$(basename "${1:-fleet}")
T=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null; rm -rf "$T"' EXIT
export HOME=$T/home
mkdir -p "$T/bin" "$T/resumed" "$T/plain"
printf '#include <unistd.h>\nint main(void) { pause(); return 0; }\n' > "$T/claude.c"
cc -o "$T/bin/claude" "$T/claude.c" || exit 1

A=11111111-1111-4111-8111-111111111111
B=22222222-2222-4222-8222-222222222222
# The project folder Claude Code would use for a folder: its real path
# with every character that is not a letter or digit turned into '-'.
project() { printf '%s' "$(cd "$1" && pwd -P)" | tr -c 'A-Za-z0-9' '-'; }
for x in "$A resumed" "$B plain"; do
    set -- $x
    d=$HOME/.claude/projects/$(project "$T/$2")
    mkdir -p "$d"
    printf '{"type":"user","sessionId":"%s","cwd":"%s","timestamp":"%s","message":{"role":"user","content":"hello"}}\n' \
        "$1" "$T/$2" "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)" > "$d/$1.jsonl"
done

(cd "$T/resumed" && exec "$T/bin/claude" --resume $A) &
(cd "$T/plain" && exec "$T/bin/claude") &
sleep 1
out=$("$F" --list)
bad=0
# fleet names a session with no bookmark after its folder.
for name in resumed plain; do
    row=$(grep "^$name " <<< "$out")
    case $row in
        "") echo "WRONG: $name is not listed"; bad=1 ;;
        *" off "*|*" older "*) echo "WRONG: $name is not seen running: $row"; bad=1 ;;
    esac
done
[ $bad = 0 ] && echo "all cases right" || { echo "$out"; exit 1; }
