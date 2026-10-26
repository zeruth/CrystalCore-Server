"""zonenames.py: the client's zone down opcodes (zonetable.py) next to Sapphire's names for the same
numbers (src/common/Network/PacketDef/ServerIpcs.h; its pre-3.0 numbering agrees with this build
for spawn 0x190, InitZone 0x19a and the event family 0x1cc-0x1df). Writes
protocol/672261/zone_down.tsv: opcode, dispatcher case, handler, Sapphire name, and how many opcodes
share the case (a group of sizes: EventPlay 2..255 and the like).

Sapphire's names are a guess per opcode until a handler confirms it: the client is the authority.
"""
import collections, os, re, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
SAPPHIRE = r"C:\Users\tyler\Documents\code\3rdparty\Sapphire\src\common\Network\PacketDef\ServerIpcs.h"
OUT = os.path.join(HERE, "..", "..", "protocol", "672261", "zone_down.tsv")

names = collections.defaultdict(list)
for m in re.finditer(r"^\s*(\w+)\s*=\s*(0x[0-9A-Fa-f]+)\s*,", open(SAPPHIRE, encoding="utf-8", errors="replace").read(), re.M):
    names[int(m.group(2), 16)].append(m.group(1))

table = subprocess.run([sys.executable, os.path.join(HERE, "zonetable.py")], capture_output=True, text=True, check=True).stdout
rows = []
for line in table.splitlines():
    op, case, handler = line.split("\t")
    rows.append((int(op, 16), int(case.split()[1], 16), handler))
per_case = collections.Counter(case for _, case, _ in rows)

os.makedirs(os.path.dirname(OUT), exist_ok=True)
with open(OUT, "w", encoding="utf-8", newline="\n") as f:
    f.write("# zone down opcodes of ffxiv.exe 672261: PacketDispatcher::vf1 @0xba51d0 (tools/re/zonenames.py)\n")
    f.write("opcode\tcase\thandler\tsapphire\tsizes\n")
    for op, case, handler in rows:
        f.write(f"{op:#06x}\t{case:#04x}\t{handler}\t{'/'.join(names.get(op, [])) or '-'}\t{per_case[case]}\n")
named = sum(1 for op, _, _ in rows if op in names)
print(f"{len(rows)} opcodes, {named} with a Sapphire name -> {os.path.normpath(OUT)}")
