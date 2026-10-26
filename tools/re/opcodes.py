"""opcodes.py <dispatcher addr> [opcode var]: opcode -> vtable slot of a decompiled *DownCallbackInterface::vf1.

The dispatcher reads the IPC header's u16 opcode @2 into a variable, then (switch cases and/or
if-chains) calls `(**(code **)(*(int *)this + OFF))(this, ..., header + 0x10)` per opcode.
Labels are collected from `case N:` and `<var> == N` until the next slot call, which they map to.
"""
import json, re, sys

CORPUS = r"C:\Users\tyler\Documents\code\CrystalCore\build\corpus.jsonl"
addr = sys.argv[1].lower().lstrip("0x") if sys.argv[1].startswith("0x") else sys.argv[1].lower()
code = None
with open(CORPUS, encoding="utf-8") as f:
    for line in f:
        e = json.loads(line)
        if e["addr"].lstrip("0") == addr.lstrip("0"):
            code = e.get("c")
            break
if not code:
    sys.exit("not found")

var = sys.argv[2] if len(sys.argv) > 2 else None
if var is None:
    m = re.search(r"(\w+) = \*\(u?short \*\)\(\w+ \+ 2\);", code)
    var = m.group(1) if m else "uVar1"

case_re = re.compile(r"case (0x[0-9a-f]+|\d+):")
cmp_re = re.compile(rf"\b{re.escape(var)} == (0x[0-9a-f]+|\d+)")
call_re = re.compile(r"\(\*\*\(code \*\*\)\(\*\(int \*\)this \+ (0x[0-9a-f]+|\d+)\)\)")

table, pending = {}, []
for ln in code.splitlines():
    for m in case_re.finditer(ln):
        pending.append(int(m.group(1), 0))
    for m in cmp_re.finditer(ln):
        pending.append(int(m.group(1), 0))
    m = call_re.search(ln)
    if m and pending:
        off = int(m.group(1), 0)
        for op in pending:
            table.setdefault(op, off // 4)
        pending = []

print(f"# opcode var {var}, {len(table)} opcodes")
for op in sorted(table):
    print(f"{op:#06x} slot {table[op]}")
