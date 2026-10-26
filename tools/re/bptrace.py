"""Attach to a running 32-bit ffxiv.exe and log one-shot breakpoint hits.

python bptrace.py <pid> <seconds> addr[=label] ...

Each address gets an int3; on its first hit the byte is restored, EIP rewound, and the hit logged
with the first stack arguments and ECX. Detaches (restoring every byte) after <seconds>.
"""
import ctypes, struct, sys, time
from ctypes import wintypes as W

k32 = ctypes.WinDLL("kernel32", use_last_error=True)
DBG_CONTINUE, DBG_EXCEPTION_NOT_HANDLED = 0x00010002, 0x80010001
EXCEPTION_DEBUG_EVENT, CREATE_PROCESS_DEBUG_EVENT, EXIT_PROCESS_DEBUG_EVENT = 1, 3, 5
BP = {0x80000003, 0x4000001F}  # breakpoint, WOW64 breakpoint
STEP = {0x80000004, 0x4000001E}  # single step, WOW64 single step
MAX_HITS = 20
PROCESS_ALL_ACCESS, THREAD_ALL_ACCESS = 0x1F0FFF, 0x1F03FF
WOW64_CONTEXT_FULL = 0x1000B


class EXCEPTION_RECORD(ctypes.Structure):
    _fields_ = [("ExceptionCode", W.DWORD), ("ExceptionFlags", W.DWORD), ("ExceptionRecord", ctypes.c_void_p),
                ("ExceptionAddress", ctypes.c_void_p), ("NumberParameters", W.DWORD),
                ("ExceptionInformation", ctypes.c_size_t * 15)]


class EXCEPTION_DEBUG_INFO(ctypes.Structure):
    _fields_ = [("ExceptionRecord", EXCEPTION_RECORD), ("dwFirstChance", W.DWORD)]


class DEBUG_EVENT(ctypes.Structure):
    # x64 layout: the union is 8-byte aligned, at offset 16
    _fields_ = [("dwDebugEventCode", W.DWORD), ("dwProcessId", W.DWORD), ("dwThreadId", W.DWORD),
                ("_pad", W.DWORD), ("u", ctypes.c_byte * 160)]


class WOW64_CONTEXT(ctypes.Structure):
    _fields_ = [("ContextFlags", W.DWORD), ("Dr", W.DWORD * 6), ("FloatSave", ctypes.c_byte * 112),
                ("SegGs", W.DWORD), ("SegFs", W.DWORD), ("SegEs", W.DWORD), ("SegDs", W.DWORD),
                ("Edi", W.DWORD), ("Esi", W.DWORD), ("Ebx", W.DWORD), ("Edx", W.DWORD), ("Ecx", W.DWORD),
                ("Eax", W.DWORD), ("Ebp", W.DWORD), ("Eip", W.DWORD), ("SegCs", W.DWORD), ("EFlags", W.DWORD),
                ("Esp", W.DWORD), ("SegSs", W.DWORD), ("Ext", ctypes.c_byte * 512)]


def main():
    pid, seconds = int(sys.argv[1]), float(sys.argv[2])
    # addresses are given at the preferred base 0x400000; the exe is relocated (ASLR)
    import subprocess
    base = int(subprocess.check_output(
        ["powershell", "-NoProfile", "-Command",
         f"'{{0:x}}' -f ((Get-Process -Id {pid}).Modules | Where-Object ModuleName -eq 'ffxiv.exe').BaseAddress.ToInt64()"],
        text=True).strip(), 16)
    delta = base - 0x400000
    print(f"ffxiv.exe at {base:#x} (delta {delta:#x})", flush=True)
    targets = {}
    for a in sys.argv[3:]:
        addr, _, label = a.partition("=")
        targets[int(addr, 16) + delta] = f"{label or addr} ({addr})"
    hproc = k32.OpenProcess(PROCESS_ALL_ACCESS, False, pid)
    if not hproc:
        sys.exit(f"OpenProcess: {ctypes.get_last_error()}")

    def read(addr, n):
        buf, got = ctypes.create_string_buffer(n), ctypes.c_size_t()
        k32.ReadProcessMemory(hproc, ctypes.c_void_p(addr), buf, n, ctypes.byref(got))
        return buf.raw[:got.value]

    def write(addr, data):
        old = W.DWORD()
        k32.VirtualProtectEx(hproc, ctypes.c_void_p(addr), len(data), 0x40, ctypes.byref(old))
        k32.WriteProcessMemory(hproc, ctypes.c_void_p(addr), data, len(data), None)
        k32.VirtualProtectEx(hproc, ctypes.c_void_p(addr), len(data), old.value, ctypes.byref(old))
        k32.FlushInstructionCache(hproc, ctypes.c_void_p(addr), len(data))

    if not k32.DebugActiveProcess(pid):
        sys.exit(f"DebugActiveProcess: {ctypes.get_last_error()}")
    k32.DebugSetProcessKillOnExit(False)
    saved = {}
    hits, rearm = {}, {}
    armed = False
    deadline = time.time() + seconds
    ev = DEBUG_EVENT()
    try:
        while time.time() < deadline:
            if not k32.WaitForDebugEvent(ctypes.byref(ev), 200):
                continue
            status = DBG_CONTINUE
            if ev.dwDebugEventCode == EXIT_PROCESS_DEBUG_EVENT:
                print("process exited", flush=True)
                break
            if ev.dwDebugEventCode == EXCEPTION_DEBUG_EVENT:
                info = EXCEPTION_DEBUG_INFO.from_buffer_copy(bytes(ev.u))
                code = info.ExceptionRecord.ExceptionCode
                addr = info.ExceptionRecord.ExceptionAddress or 0
                if code in BP and not armed:
                    # the attach breakpoint: arm ours now
                    for t in targets:
                        saved[t] = read(t, 1)
                        write(t, b"\xcc")
                    armed = True
                    print(f"armed {len(targets)} breakpoints", flush=True)
                elif code in BP and addr in saved:
                    write(addr, saved[addr])
                    th = k32.OpenThread(THREAD_ALL_ACCESS, False, ev.dwThreadId)
                    ctx = WOW64_CONTEXT()
                    ctx.ContextFlags = WOW64_CONTEXT_FULL
                    k32.Wow64GetThreadContext(th, ctypes.byref(ctx))
                    ctx.Eip = addr
                    hits[addr] = hits.get(addr, 0) + 1
                    if hits[addr] < MAX_HITS:
                        # re-arm after this one instruction (trap flag)
                        ctx.EFlags |= 0x100
                        rearm[ev.dwThreadId] = addr
                    else:
                        saved.pop(addr)
                    k32.Wow64SetThreadContext(th, ctypes.byref(ctx))
                    args = struct.unpack("<5I", read(ctx.Esp, 20).ljust(20, b"\0"))
                    extra = ""
                    if ":m " in targets[addr]:
                        extra = " [arg1]=" + read(args[1], 32).hex()
                    # name:d<off>/<off>/...: follow pointers from ecx, adding each offset then reading
                    # ('/' not ',': PowerShell splits an argument at commas)
                    import re as _re
                    dm = _re.search(r":d([0-9a-fx/]+)", targets[addr])
                    if dm:
                        p, chain = ctx.Ecx, []
                        for off in dm.group(1).split("/"):
                            p = struct.unpack("<I", read(p + int(off, 16), 4).ljust(4, b"\0"))[0]
                            chain.append(f"{p:#x}" + (f"(={p - delta:#x})" if base <= p < base + 0x2000000 else ""))
                        extra += " chain=" + " -> ".join(chain)
                    print(f"{time.strftime('%H:%M:%S')} HIT {targets[addr]} @{addr:#x} ecx={ctx.Ecx:#x} "
                          f"ret={args[0] - delta:#x} args={[hex(x) for x in args[1:]]}{extra}", flush=True)
                    k32.CloseHandle(th)
                elif code in STEP and ev.dwThreadId in rearm:
                    a = rearm.pop(ev.dwThreadId)
                    if a in saved:
                        write(a, b"\xcc")
                elif code in BP:
                    print(f"unmatched breakpoint code={code:#x} addr={addr:#x}", flush=True)
                else:
                    if info.dwFirstChance == 0:
                        print(f"second-chance exception code={code:#x} addr={addr:#x}", flush=True)
                    status = DBG_EXCEPTION_NOT_HANDLED
            k32.ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, status)
    finally:
        for t, b in saved.items():
            write(t, b)
        k32.DebugActiveProcessStop(pid)
        print(f"detached; not hit: {[targets[t] for t in saved if t not in hits]}", flush=True)


main()
