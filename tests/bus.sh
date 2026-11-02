#!/bin/bash
# Sends the bus hook messages from the phone and from a local session, and
# checks that a phone "!" line runs only when it is an open move.
#   tests/bus.sh     prints "all cases right" or each wrong one
H=$(cd "$(dirname "$0")/.." && pwd)/hooks/fleet-bus
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
export HOME=$T
S=33333333-3333-4333-8333-333333333333
mkdir -p "$HOME/.fleet/open" "$HOME/.fleet/bus/$S" "$HOME/.fleet/relay/$S"
printf '3\tp\t\n1\t3\t1700000000\t`! /tmp/ok.sh` uploads the pages.\n' > "$HOME/.fleet/open/$S"

wrong=0
# case: where the message comes from, its text, and a word the answer must hold
check() {
    printf '%s\n' "$2" > "$HOME/.fleet/$1/$S/m"
    out=$(printf '{"session_id":"%s","hook_event_name":"UserPromptSubmit"}' "$S" | "$H")
    case $out in
        *"$3"*) ;;
        *) echo "wrong: $1 '$2' gave: $out"; wrong=1 ;;
    esac
}
check relay '! /tmp/ok.sh' '- ! /tmp/ok.sh'
check relay '!  /tmp/ok.sh ' '- !  /tmp/ok.sh'
check relay '! rm -rf ~' 'REFUSED'
check relay $'1 yes\n! /tmp/other.sh' 'REFUSED'
check relay '1 yes' '- 1 yes'
check bus '! anything' '- ! anything'
rm "$HOME/.fleet/open/$S"
check relay '! /tmp/ok.sh' 'REFUSED'
[ $wrong = 0 ] && echo "all cases right"
exit $wrong
