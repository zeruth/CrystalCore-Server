"""consts.py <addr[:f|:d]>...: a float (f, the default) or double (d) constant from ffxiv.exe's
data (preferred base), as the recomp's REFG(float/double, addr) reads it."""
import struct, sys, pefile

EXE = r"C:\Users\tyler\Documents\code\CrystalCore\archive\installs\beta-672261\game\ffxiv.exe"
pe = pefile.PE(EXE, fast_load=True)
base = pe.OPTIONAL_HEADER.ImageBase
for arg in sys.argv[1:]:
    a, _, kind = arg.partition(":")
    addr = int(a, 16)
    if kind == "d":
        v = struct.unpack("<d", pe.get_data(addr - base, 8))[0]
    else:
        v = struct.unpack("<f", pe.get_data(addr - base, 4))[0]
    print(f"{addr:#x} {kind or 'f'}: {v!r}")
