"""dis.py <addr> <len>: disassemble ffxiv.exe (preferred base) with capstone."""
import sys, pefile
from capstone import Cs, CS_ARCH_X86, CS_MODE_32

EXE = r"C:\Users\tyler\Documents\code\CrystalCore\archive\installs\beta-672261\game\ffxiv.exe"
pe = pefile.PE(EXE, fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
addr, n = int(sys.argv[1], 16), int(sys.argv[2], 0)
code = pe.get_data(addr - base, n)
for i in Cs(CS_ARCH_X86, CS_MODE_32).disasm(code, addr):
    print(f"{i.address:08x}  {i.mnemonic} {i.op_str}")
