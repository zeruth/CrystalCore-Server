"""Run ffxiv.exe 672261's lobby crypto under 32-bit emulation and print test vectors.

MD5:      0x408da0 init (ecx = ctx), 0x40b1f0 update (thiscall ctx, data, len), 0x40b2a0 final (ctx, out)
Blowfish: 0x409a70 init (thiscall bf, key, len), 0x407e80 encrypt / 0x407f40 decrypt (bf, src, dst, len)
"""
import struct, pefile
from unicorn import Uc, UC_ARCH_X86, UC_MODE_32
from unicorn.x86_const import UC_X86_REG_ESP, UC_X86_REG_ECX, UC_X86_REG_EAX

EXE = r"C:\Users\tyler\Documents\code\CrystalCore\archive\installs\beta-672261\game\ffxiv.exe"
RET = 0x10000          # return address: a mapped page the run stops at
STACK = 0x200000       # 1 MB, grows down from STACK + 0x100000
HEAP = 0x400000 - 0x100000  # below the image (base 0x400000)

pe = pefile.PE(EXE)
base = pe.OPTIONAL_HEADER.ImageBase
uc = Uc(UC_ARCH_X86, UC_MODE_32)
uc.mem_map(base, (pe.OPTIONAL_HEADER.SizeOfImage + 0xFFF) & ~0xFFF)
uc.mem_write(base, pe.header)
for s in pe.sections:
    uc.mem_write(base + s.VirtualAddress, s.get_data())
uc.mem_map(RET, 0x1000)
uc.mem_map(STACK, 0x100000)
uc.mem_map(HEAP, 0x100000)
heap_next = HEAP


def alloc(data_or_size):
    global heap_next
    data = bytes(data_or_size) if not isinstance(data_or_size, int) else bytes(data_or_size)
    addr = heap_next
    heap_next += (len(data) + 0xF) & ~0xF
    uc.mem_write(addr, data)
    return addr


def call(fn, ecx, *args):
    sp = STACK + 0x100000 - 0x100
    frame = struct.pack(f"<{1 + len(args)}I", RET, *args)
    sp -= len(frame)
    uc.mem_write(sp, frame)
    uc.reg_write(UC_X86_REG_ESP, sp)
    uc.reg_write(UC_X86_REG_ECX, ecx)
    uc.emu_start(fn, RET)
    return uc.reg_read(UC_X86_REG_EAX)


def md5(data):
    ctx = alloc(0x58)
    buf = alloc(data)
    out = alloc(16)
    call(0x408DA0, ctx)
    call(0x40B1F0, ctx, buf, len(data))
    call(0x40B2A0, ctx, out)
    return bytes(uc.mem_read(out, 16))


def blowfish(key, data, decrypt=False):
    bf = alloc(0x1048)
    k = alloc(key)
    call(0x409A70, bf, k, len(key))
    buf = alloc(data)
    call(0x407F40 if decrypt else 0x407E80, bf, buf, buf, len(data))
    return bytes(uc.mem_read(buf, len(data)))


# The key for the capture: key 0x6ade798e, phrase "Test Ticket Data"
phrase = b"Test Ticket Data".ljust(32, b"\0")
base_key = struct.pack("<III", 0x12345678, 0x6ADE798E, 1000) + phrase
digest = md5(base_key)
print("md5 base key", base_key.hex())
print("digest      ", digest)
print("digest hex  ", digest.hex())

plain = bytes(range(32))
enc = blowfish(digest, plain)
print("bf(digest) encrypt 00..1f ->", enc.hex())
print("bf(digest) decrypt back   ->", blowfish(digest, enc, decrypt=True).hex())
# a key whose every byte is >= 0x80: the signed-char key schedule differs from standard Blowfish here
hk = bytes([0x80 + i for i in range(16)])
print("bf(80..8f) encrypt 00..1f ->", blowfish(hk, plain).hex())
print("bf(00x8)   encrypt 0      ->", blowfish(bytes(8), bytes(8)).hex())
