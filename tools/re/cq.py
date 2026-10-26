"""Query CrystalCore's Ghidra corpus.

cq.py keys                      the fields of a decompiled entry
cq.py names <regex>             addr + name of functions whose name matches
cq.py grep <regex> [max]        addr + name of functions whose code matches
cq.py show <addr|name> [...]    the decompiled code
cq.py callers <addr>            functions whose code mentions FUN_<addr> or the name at addr
"""
import json, re, sys

CORPUS = r"C:\Users\tyler\Documents\code\CrystalCore\build\corpus.jsonl"


def entries():
    with open(CORPUS, encoding="utf-8") as f:
        for line in f:
            yield json.loads(line)


def code(e):
    return e.get("c") or e.get("code") or e.get("decomp") or ""


def main():
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "keys":
        for e in entries():
            if "error" not in e:
                print({k: (v if len(str(v)) < 80 else str(v)[:80] + "...") for k, v in e.items()})
                return
    if cmd == "names":
        rx = re.compile(args[0])
        for e in entries():
            if rx.search(e["name"]):
                print(e["addr"], e["name"], e.get("size"), "ERR" if "error" in e else "")
    elif cmd == "grep":
        rx, n = re.compile(args[0]), int(args[1]) if len(args) > 1 else 50
        for e in entries():
            if rx.search(code(e)):
                print(e["addr"], e["name"], e.get("size"))
                n -= 1
                if n == 0:
                    break
    elif cmd == "show":
        want = {a.lower().removeprefix("0x").lstrip("0") for a in args}
        for e in entries():
            if e["addr"].lstrip("0") in want or e["name"] in args:
                print(f"// {e['addr']} {e['name']} size {e.get('size')}")
                print(code(e) or e.get("error"))
    elif cmd == "callers":
        addr = args[0].lower().removeprefix("0x")
        name = None
        for e in entries():
            if e["addr"].lstrip("0") == addr.lstrip("0"):
                name = e["name"]
        pats = {f"FUN_{addr.zfill(8)}"} | ({name} if name else set())
        for e in entries():
            c = code(e)
            if any(p in c for p in pats) and e["addr"].lstrip("0") != addr.lstrip("0"):
                print(e["addr"], e["name"])


main()
