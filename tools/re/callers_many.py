"""callers_many.py addr...: for each FUN_<addr>, the functions whose code calls it (one pass)."""
import json, sys

CORPUS = r"C:\Users\tyler\Documents\code\CrystalCore\build\corpus.jsonl"
names = {f"FUN_{a.lower().zfill(8)}": a for a in sys.argv[1:]}
found = {a: [] for a in sys.argv[1:]}
with open(CORPUS, encoding="utf-8") as f:
    for line in f:
        e = json.loads(line)
        c = e.get("c") or ""
        for n, a in names.items():
            if n in c and e["addr"].lstrip("0") != a.lstrip("0"):
                found[a].append(f"{e['addr']} {e['name']}")
for a, callers in found.items():
    print(f"== {a}: {', '.join(callers) or '-'}")
