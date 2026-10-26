"""containing.py addr...: the corpus function containing each address, and its code."""
import json, sys

CORPUS = r"C:\Users\tyler\Documents\code\CrystalCore\build\corpus.jsonl"
want = [int(a, 16) for a in sys.argv[1:]]
with open(CORPUS, encoding="utf-8") as f:
    for line in f:
        e = json.loads(line)
        start = int(e["addr"], 16)
        for a in want:
            if start <= a < start + (e.get("size") or 0):
                print(f"// {a:#x} is in {e['addr']} {e['name']} size {e.get('size')}")
                print(e.get("c") or e.get("error"))
