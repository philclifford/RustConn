#!/bin/bash
# Reject Rust-only string escapes inside translatable literals.
#
# WHY THIS EXISTS
# ---------------
# po/update-pot.sh extracts strings with `xgettext --language=C`, because
# xgettext has no native Rust parser and the `gettext("...")` call syntax is
# identical to C. That works for the call syntax, but NOT for the string
# literal body: Rust and C do not share the same escape grammar.
#
# Rust writes a codepoint as `\u{2019}` (braces, variable width).
# C writes it as `\u2019` / `\U0001F600` (no braces, fixed width).
#
# So when xgettext sees a Rust literal like
#
#     i18n("Don\u{2019}t show this page at startup")
#
# it does not recognise `\u{...}` as an escape and copies those seven
# characters into the .pot verbatim. The resulting msgid is
#
#     Don\u{2019}t show this page at startup     <- what translators receive
#
# but at runtime rustc has already decoded the literal, so i18n() looks up
#
#     Don't show this page at startup            <- what gettext is asked for
#
# The two never match. The lookup silently falls through to the msgid itself,
# so the string renders untranslated in EVERY locale while the .po files look
# 100% complete. There is no compiler error, no gettext warning, and
# `msgfmt --check` passes — which is exactly why this needs a dedicated guard.
# It bit us once already (welcome-screen checkbox in split_view/bridge.rs).
#
# FIX WHEN THIS FIRES
# -------------------
# Put the character directly in the source literal instead of escaping it:
#
#     i18n("Don't show this page at startup")     # ASCII — project convention
#
# The codebase uses the ASCII apostrophe in translatable strings (see
# i18n("Don't Save") in rustconn/src/alert.rs), so prefer that for apostrophes.
# Then regenerate the template with po/update-pot.sh.
#
# Usage: ./scripts/check-i18n-escapes.sh        (exit 0 = clean, 1 = violations)

set -euo pipefail

cd "$(dirname "$0")/.."

# Keywords must stay in sync with the --keyword flags in po/update-pot.sh.
KEYWORDS='i18n|i18n_f|ni18n|ni18n_f|gettext|ngettext|pgettext|npgettext'

# Mirrors the extraction scope of po/update-pot.sh: `find rustconn/src -name '*.rs'`.
mapfile -t sources < <(find rustconn/src -name '*.rs' -type f | sort)

if [ ${#sources[@]} -eq 0 ]; then
    echo "error: no Rust sources found under rustconn/src" >&2
    exit 1
fi

# A translatable literal is the first string argument after a translation
# keyword. Reported as file:line so editors can jump straight to it.
findings=$(
    KEYWORDS="$KEYWORDS" python3 - "${sources[@]}" <<'PY'
import os
import re
import sys

call = re.compile(
    r"\b(?:" + os.environ["KEYWORDS"] + r")\s*!?\s*\(\s*"
    r"(?:[A-Za-z0-9_:\s&*.]*?,\s*)?"        # optional leading ctxt/count arg
    r'"((?:[^"\\]|\\.)*)"',                 # first string literal
    re.DOTALL,
)

violations = []
for path in sys.argv[1:]:
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
    for match in call.finditer(text):
        literal = match.group(1)
        if r"\u{" not in literal:
            continue
        line = text.count("\n", 0, match.start(1)) + 1
        violations.append(f"{path}:{line}: {literal}")

print("\n".join(violations), end="")
PY
)

if [ -n "$findings" ]; then
    echo "FAIL: Rust-only \\u{...} escape inside a translatable string." >&2
    echo "xgettext --language=C cannot decode it, so the extracted msgid will" >&2
    echo "never match the runtime lookup and the string stays untranslated in" >&2
    echo "every locale. Use the literal character instead." >&2
    echo >&2
    printf '%s\n' "$findings" >&2
    exit 1
fi

echo "OK: no \\u{...} escapes in translatable strings (${#sources[@]} files scanned)"
