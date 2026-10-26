"""ordertable.py: the Order (Sapphire's ActorControl, down 0x142-0x144) handler @0xa55f80 switches
on a byte table @0xa5bfb0 indexed by the category; prints category -> case for those that have one.
"""
import pefile

EXE = r"C:\Users\tyler\Documents\code\CrystalCore\archive\installs\beta-672261\game\ffxiv.exe"
pe = pefile.PE(EXE, fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
table = pe.get_data(0xA5BFB0 - base, 0x200)
default = max(set(table[:0x200]), key=table[:0x200].count)
for cat, case in enumerate(table):
    if case != default:
        print(f"category {cat:#06x} -> case {case}")
print(f"(all others -> case {default})")
