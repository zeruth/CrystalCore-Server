"""triggers.py: every call of Global::SendClientTrigger (up 0x191, @0xa51200) with a constant
command id: id, the calling function, and the call's arguments as decompiled.

Prints them grouped by command id, to tell the client's commands apart (target, auto-attack,
sheathe, ...) by who sends them.
"""
import collections, json, re, sys

CORPUS = r"C:\Users\tyler\Documents\code\CrystalCore\build\corpus.jsonl"

calls = collections.defaultdict(list)
with open(CORPUS, encoding="utf-8") as f:
    for line in f:
        e = json.loads(line)
        code = e.get("c") or ""
        for m in re.finditer(r"SendClientTrigger\(([^;]*)\);", code):
            args = [a.strip() for a in m.group(1).replace("\n", " ").split(",")]
            first = args[0]
            try:
                cid = int(first, 0)
            except ValueError:
                cid = first
            calls[cid].append((e["addr"], e["name"], ", ".join(args[1:])))

for cid in sorted(calls, key=lambda c: (isinstance(c, str), c if isinstance(c, int) else 0)):
    label = f"{cid:#06x}" if isinstance(cid, int) else cid
    for addr, name, rest in calls[cid][:4]:
        print(f"{label}\t{addr} {name}\t({rest})")
    if len(calls[cid]) > 4:
        print(f"{label}\t... {len(calls[cid]) - 4} more")
