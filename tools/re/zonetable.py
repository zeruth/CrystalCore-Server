"""zonetable.py: zone down opcode -> handler, from Client::Network::PacketDispatcher::vf1 @0xba51d0.

The dispatcher switches on a byte read at 0xba6358 + opcode + 1 for opcodes 0x67..0x2eb (the
decompiler's form of a byte index table); each case calls one handler with the body (+0x10).
Prints opcode, case, handler; writes protocol-ready TSV to the path given.
"""
import json, re, sys, pefile

EXE = r"C:\Users\tyler\Documents\code\CrystalCore\archive\installs\beta-672261\game\ffxiv.exe"
CORPUS = r"C:\Users\tyler\Documents\code\CrystalCore\build\corpus.jsonl"

pe = pefile.PE(EXE, fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
code = None
with open(CORPUS, encoding="utf-8") as f:
    for line in f:
        e = json.loads(line)
        if e["addr"] == "ba51d0" or e["addr"] == "00ba51d0":
            code = e["c"]
            break

# case index -> first call in its body
cases, current = {}, []
for ln in code.splitlines():
    m = re.match(r"\s*case (0x[0-9a-f]+|\d+):", ln)
    if m:
        current.append(int(m.group(1), 0))
        continue
    if current:
        call = re.search(r"(FUN_[0-9a-f]{8}|\(\*\*\(code \*\*\)\(\*piVar2 \+ 0x7c\)\)\(piVar2,(\d+))", ln)
        if call:
            target = call.group(1) if call.group(1).startswith("FUN_") else f"module->vf31(kind {call.group(2)})"
            for c in current:
                cases[c] = target
            current = []
        elif "DAT_0137104c + 0x1228" in ln:
            for c in current:
                cases[c] = "framework+0x1228 = body@0, +0x122c = body@4"
            current = []

rows = []
for op in range(0x67, 0x67 + 0x285):
    idx = pe.get_data(0xBA6358 + op + 1 - base, 1)[0]
    if idx in cases:
        rows.append((op, idx, cases[idx]))

for op, idx, h in rows:
    print(f"{op:#06x}\tcase {idx:#04x}\t{h}")
print(f"# {len(rows)} opcodes", file=sys.stderr)
