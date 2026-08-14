#!/usr/bin/env bash
# PreToolUse guard for the shell tool: stop the failure mode where the agent
# loses a long command's output and then tries to "wait" for it.
#
# The observed sequence, which cost ~35 idle minutes in one session:
#
#   1. `cargo test --workspace` is started with the default 120 s tool timeout.
#      The run takes ~120 s, so the tool returns with truncated or empty output
#      while the process is still alive.
#   2. The agent concludes it must wait and sends `sleep 115; echo W10`.
#   3. That line goes to the *same* terminal, which is still busy. bash is not
#      reading stdin while a foreground job runs, so the line sits in the tty
#      buffer. So do the next eighteen.
#   4. When cargo finally exits, bash drains the buffer and runs every queued
#      `sleep 115` back to back: W10, W11, W12 ... W28.
#
# Nothing in that chain is recoverable by the agent, because each `sleep` looks
# like a command that legitimately timed out. The only cure is to never enter
# the loop, so this guard rejects the three inputs that lead into it:
#
#   R1  sleep-based waiting (a long sleep, or any loop containing a sleep)
#   R2  piping cargo build output through a filter (the tool then returns
#       nothing at all — see steering shell-environment.md)
#   R3  starting cargo while another cargo holds the target-dir lock
#   R4  a cargo build/test with no timeout headroom and no background handle,
#       i.e. step 1 above
#
# Fails OPEN on anything unexpected: a guard that blocks every shell call
# because jq changed its output shape would be worse than the problem.

set -uo pipefail

trap 'exit 0' ERR

payload=$(cat) || exit 0
command -v jq >/dev/null 2>&1 || exit 0

cmd=$(printf '%s' "$payload" | jq -r '.tool_input.command // ""' 2>/dev/null) || exit 0
# `control_bash_process` with action=stop, and anything else without a command
# line, has nothing to check.
[ -n "$cmd" ] || exit 0

action=$(printf '%s' "$payload" | jq -r '.tool_input.action // ""' 2>/dev/null) || action=""

# Accept whichever spelling the client uses; an unknown one leaves this at 0,
# which R4 handles with a second-chance marker rather than a hard loop.
tmo=$(printf '%s' "$payload" | jq -r '
    [.tool_input.timeout, .tool_input.timeoutMs, .tool_input.timeout_ms]
    | map(select(type == "number")) | first // 0' 2>/dev/null) || tmo=0

bg=$(printf '%s' "$payload" | jq -r '.tool_input.run_in_background // false' 2>/dev/null) || bg=false
# A background process tool call is a handle by construction: its output is read
# later with get_process_output, not by waiting.
[ "$action" = "start" ] && bg=true

block() {
    printf 'bash-serialization-guard: %s\n' "$1" >&2
    shift
    printf '%s\n' "$@" >&2
    exit 2
}

# --- R1: no sleep-based waiting --------------------------------------------
# `|| true` on every match: pipefail plus the ERR trap would otherwise turn a
# "no match" into a silent exit 0 and skip the remaining rules.
sleeps=$(printf '%s' "$cmd" | grep -oE '(^|[^[:alnum:]_./-])sleep[[:space:]]+[0-9]+' | grep -oE '[0-9]+$' | sort -n | tail -1 || true)
has_loop=$(printf '%s' "$cmd" | grep -cE '(^|[^[:alnum:]_])(while|until)[[:space:]]' || true)

if [ -n "$sleeps" ] && { [ "$sleeps" -ge 5 ] || [ "${has_loop:-0}" -gt 0 ]; }; then
    block 'sleep is not how you wait for a command here.' \
        '  A sleep occupies the terminal, so it cannot observe another terminal, and' \
        '  if that terminal is busy the line just queues up behind the running job.' \
        '' \
        '  Wait inside one tool call instead:' \
        '    execute_bash(command="cd /home/totoshko88/Documents/RustConn && cargo test --workspace > /tmp/rc-test.log 2>&1", timeout=900000)' \
        '  then read /tmp/rc-test.log with the file-reading tool.' \
        '' \
        '  Or take a handle and poll the filesystem, never the clock:' \
        '    execute_bash(command="cd /home/totoshko88/Documents/RustConn && rm -f /tmp/rc-test.log /tmp/rc-test.rc && nohup sh -c \x27cargo test --workspace > /tmp/rc-test.log 2>&1; echo $? > /tmp/rc-test.rc\x27 >/dev/null 2>&1 &")' \
        '  The run is done exactly when /tmp/rc-test.rc exists. Read it with the' \
        '  file-reading tool between other work; do not sit in the shell.' \
        '' \
        '  Or hand the whole verification to the rust-quality-check sub-agent.'
fi

# --- R2: never pipe cargo build output -------------------------------------
cargo_verbs='build|test|clippy|check|run|bench|doc|nextest|machete|audit'
if printf '%s' "$cmd" | grep -qE "cargo([[:space:]]+\+[^[:space:]]+)?[[:space:]]+($cargo_verbs)[^|;&]*\|[[:space:]]*(head|tail|grep|rg|egrep|fgrep|less|more|awk|sed|wc|sort|uniq|cut|column)"; then
    block 'piped cargo output is the main way this shell tool returns nothing.' \
        '  Redirect to a file and read the file:' \
        '    ... cargo clippy --all-targets > /tmp/rc-clippy.log 2>&1' \
        '  then read /tmp/rc-clippy.log with the file-reading tool (it takes line ranges).'
fi

starts_cargo=0
if printf '%s' "$cmd" | grep -qE "cargo([[:space:]]+\+[^[:space:]]+)?[[:space:]]+($cargo_verbs)"; then
    starts_cargo=1
fi

# --- R3: one cargo at a time ----------------------------------------------
# `pgrep -x` matches the process name, so it can never match this guard itself
# the way `pgrep -f cargo` would.
if [ "$starts_cargo" -eq 1 ]; then
    running=$(pgrep -x cargo || true)
    if [ -n "$running" ]; then
        block 'another cargo is already running; both would block on the same target-dir lock and look hung.' \
            "  pid(s): $(printf '%s' "$running" | tr '\n' ' ')" \
            '  Inspect with: pgrep -af cargo' \
            '  Wait for it (its log file is the cheapest signal) rather than starting a second run.'
    fi
fi

# --- R4: give a long cargo run somewhere to put its output -----------------
# Without headroom the tool returns mid-run, the output is lost, and R1 is the
# usual next move. `cargo fmt` and `cargo metadata` are excluded: they are fast
# and take no lock.
if [ "$starts_cargo" -eq 1 ] && [ "$bg" != "true" ] && [ "${tmo:-0}" -lt 300000 ]; then
    marker_dir=${TMPDIR:-/tmp}/rustconn-bash-guard
    mkdir -p "$marker_dir" 2>/dev/null || marker_dir=""
    if [ -n "$marker_dir" ]; then
        find "$marker_dir" -type f -mmin +60 -delete 2>/dev/null || true
        key=$(printf '%s' "$cmd" | sha1sum | cut -d' ' -f1 || true)
        # Second chance: if the identical command was already refused once, the
        # agent has been told. Letting it through beats a deadlock if `timeout`
        # is spelled differently in this client and can never be seen here.
        if [ -n "$key" ] && [ -e "$marker_dir/$key" ]; then
            exit 0
        fi
        [ -n "$key" ] && : > "$marker_dir/$key" 2>/dev/null || true
    fi

    block 'a cargo build/test with the default 120 s timeout, which is shorter than this workspace'"'"'s test run.' \
        '  It will return while the process is still alive, the output will be lost,' \
        '  and the terminal will stay busy — that is how the sleep chain starts.' \
        '' \
        '  Pass explicit headroom and a log file:' \
        '    timeout=900000, command="... > /tmp/rc-run.log 2>&1"' \
        '  or set run_in_background=true and read the log with the file-reading tool,' \
        '  or delegate to the rust-quality-check sub-agent.'
fi

exit 0
