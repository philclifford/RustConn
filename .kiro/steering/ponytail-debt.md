---
inclusion: manual
description: "Збирає всі відкладені спрощення (`// ponytail:`) у крейтах в один леджер технічного боргу, щоб «later» не стало «never»."
---

Run the ledger and review it:

```bash
scripts/ponytail-ledger.sh
```

The script does the collection: it finds every `// ponytail:` marker across the
workspace crates, reassembles markers that wrap onto following comment lines,
groups by crate, and flags the ones that appear to state a ceiling without an
upgrade path. It never edits code.

Your job is the part it cannot do:

1. **Read the flagged ones.** `FLAG` is a heuristic — it fires when the text has
   no `;` and no sentence break, which is where the two halves normally separate.
   Confirm whether the marker really is missing an upgrade path, and say which.
2. **Question the `OK` ones too.** The script cannot tell whether a stated ceiling
   is still honest. "fine for <20 knocks" was written when 20 was generous; if the
   code now runs it per-host in a loop, the marker is stale even though it parses
   as complete.
3. **Report, do not fix.** Name the markers worth acting on and why. Editing code
   is a separate, explicit request.

Do not re-derive the ledger with your own `grep`. That is what the script replaced.
