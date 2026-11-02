#!/bin/bash
# Sends the bus hook messages from the phone and from a local session, and
# checks that a phone "!" line runs only when it is an open move, that the
# hook runs it and hands over its output, and that a Stop leaves it waiting.
#   tests/bus.sh     prints "all cases right" or each wrong one
H=$(cd "$(dirname "$0")/.." && pwd)/hooks/fleet-bus
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
export HOME=$T
S=33333333-3333-4333-8333-333333333333
mkdir -p "$HOME/.fleet/open" "$HOME/.fleet/bus/$S" "$HOME/.fleet/relay/$S"
printf '#!/bin/sh\necho pages up in "$PWD"\n' > "$T/ok.sh"
printf '#!/bin/sh\ntouch "%s/ran"\n' "$T" > "$T/bad.sh"
chmod +x "$T/ok.sh" "$T/bad.sh"
printf '3\tp\t\n1\t3\t1700000000\t`! %s/ok.sh` uploads the pages.\n' "$T" > "$HOME/.fleet/open/$S"

wrong=0
hook() {  # event: what the hook prints for that event
    printf '{"session_id":"%s","hook_event_name":"%s","cwd":"%s"}' "$S" "$1" "$T" | "$H"
}
# case: where the message comes from, its text, and a word the answer must hold
check() {
    printf '%s\n' "$2" > "$HOME/.fleet/$1/$S/m"
    out=$(hook UserPromptSubmit)
    case $out in
        *"$3"*) ;;
        *) echo "wrong: $1 '$2' gave: $out"; wrong=1 ;;
    esac
}
check relay "! $T/ok.sh" "pages up in $T"
check relay "!  $T/ok.sh " 'exit 0'
check relay $'1 yes\n! '"$T/ok.sh" '- The user ran this from the phone'
check relay "! $T/bad.sh" 'REFUSED'
[ -e "$T/ran" ] && { echo "wrong: a refused command ran"; wrong=1; }
check relay '! rm -rf ~' 'REFUSED'
check relay $'1 yes\n! /tmp/other.sh' 'REFUSED'
check relay '1 yes' '- 1 yes'
check bus '! anything' '- ! anything'
# a turn that ends leaves a phone command for the prompt fleet types
printf '! %s/ok.sh\n' "$T" > "$HOME/.fleet/relay/$S/m"
out=$(hook Stop)
[ -z "$out" ] && [ -e "$HOME/.fleet/relay/$S/m" ] || { echo "wrong: Stop gave: $out"; wrong=1; }
out=$(hook UserPromptSubmit)
[[ $out == *"pages up"* ]] || { echo "wrong: the prompt after Stop gave: $out"; wrong=1; }
rm "$HOME/.fleet/open/$S"
check relay "! $T/ok.sh" 'REFUSED'
[ $wrong = 0 ] && echo "all cases right"
exit $wrong
