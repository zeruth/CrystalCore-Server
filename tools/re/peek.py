"""peek.py <pid> addr...: read u32s from ffxiv.exe globals (addresses at the preferred base)."""
import ctypes, struct, subprocess, sys

k32 = ctypes.WinDLL("kernel32", use_last_error=True)
pid = int(sys.argv[1])
base = int(subprocess.check_output(
    ["powershell", "-NoProfile", "-Command",
     f"'{{0:x}}' -f ((Get-Process -Id {pid}).Modules | Where-Object ModuleName -eq 'ffxiv.exe').BaseAddress.ToInt64()"],
    text=True).strip(), 16)
h = k32.OpenProcess(0x0010 | 0x0400, False, pid)
for a in sys.argv[2:]:
    addr = int(a, 16) - 0x400000 + base
    buf = ctypes.create_string_buffer(4)
    k32.ReadProcessMemory(h, ctypes.c_void_p(addr), buf, 4, None)
    print(f"{int(a, 16):#x}: {struct.unpack('<I', buf.raw)[0]:#x}")
