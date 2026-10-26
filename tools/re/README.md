# Reverse-engineering tools

Python scripts used to read the protocol out of ffxiv.exe 672261 (the client repo's Ghidra/Rhipper
corpus and install). Paths to `CrystalCore/` are hard-coded at the top of each script. Addresses are
always the exe's preferred base (0x400000); the live client is relocated by ASLR, and the scripts
that touch it rebase.

| script | what it does |
|---|---|
| `cq.py` | the decompiled corpus (`CrystalCore/build/corpus.jsonl`): `names <regex>`, `grep <regex> [max]`, `show <addr...>`, `callers <addr>` |
| `callers_many.py` | callers of several functions in one pass over the corpus |
| `containing.py` | the function containing an address (e.g. a return address from a trace) |
| `disx.py` | capstone disassembly of the exe at an address |
| `opcodes.py` | opcode → vtable slot of a decompiled `*DownCallbackInterface::vf1` dispatcher |
| `zonetable.py` | zone down opcode → handler, from `PacketDispatcher::vf1` @0xba51d0 and its byte table |
| `oracle32.py` | runs the client's own MD5/Blowfish under 32-bit Unicorn for test vectors |
| `bptrace.py` | attaches to the running `ffxiv.exe` (Win32 debug API) and logs breakpoint hits: `name:m` dumps arg 1, `name:d2a4/4/4` follows a pointer chain from ecx. One-shot per hit, re-armed by single-step, up to 20 hits each. Detach cleanly (let it time out); killing it can take the client down |
| `peek.py` | reads u32 globals from the running client (no debugger) |
| `exd.py` | SqPack + EXD reader (`header <sheet>`, `rows <sheet> [lang] [n]`, `find <sheet> <col>=<val>`); `crystal-data` is the Rust version |
| `lgb.py` | lists a layout's layers and objects (`lgb.py ffxiv/sea_s1/twn/s1t2/level planmap.lgb`) |

Findings with solid evidence go into `CrystalCore/data/names.json` (and `structs.json`), which
Rhipper applies to the Ghidra program.
