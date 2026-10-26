"""Minimal SqPack + EXD reader for the 672261 install (scratch tool; Phase 4 does this in Rust).

exd.py header <sheet>            columns of a sheet
exd.py rows <sheet> [lang] [N]   first N rows (lang: en, ja, de, fr, or none)
exd.py find <sheet> <col>=<val>  rows whose column equals a value
"""
import struct, sys, zlib, binascii, os

ROOT = r"C:\Users\tyler\Documents\code\CrystalCore\archive\installs\beta-672261\game\sqpack\ffxiv"
CAT = "0a0000"


def jamcrc(s: bytes) -> int:
    return (~binascii.crc32(s)) & 0xFFFFFFFF


def load_index(cat=CAT):
    data = open(os.path.join(ROOT, f"{cat}.win32.index"), "rb").read()
    hdr = struct.unpack_from("<I", data, 0x0C)[0]  # sqpack header size
    files_off, files_size = struct.unpack_from("<II", data, hdr + 0x08)
    table = {}
    for i in range(files_off, files_off + files_size, 16):
        fh, dh, val, _ = struct.unpack_from("<IIII", data, i)
        table[(dh, fh)] = val
    return table


CATS = {"exd": "0a0000", "bg": "020000"}
INDEXES = {}
DATS = {}


def read_file(path: str) -> bytes:
    path = path.lower()
    cat = CATS[path.split("/", 1)[0]]
    if cat not in INDEXES:
        INDEXES[cat] = load_index(cat)
    folder, name = path.rsplit("/", 1)
    val = INDEXES[cat].get((jamcrc(folder.encode()), jamcrc(name.encode())))
    if val is None:
        raise FileNotFoundError(path)
    dat_id = (val >> 1) & 7
    off = (val & ~0xF) * 8
    if (cat, dat_id) not in DATS:
        DATS[(cat, dat_id)] = open(os.path.join(ROOT, f"{cat}.win32.dat{dat_id}"), "rb")
    f = DATS[(cat, dat_id)]
    f.seek(off)
    head = f.read(0x18)
    hsize, ftype, usize, _, _, nblocks = struct.unpack("<IIIIII", head)
    f.seek(off + 0x18)
    blocks = [struct.unpack("<IHH", f.read(8)) for _ in range(nblocks)]
    out = bytearray()
    for boff, _, _ in blocks:
        f.seek(off + hsize + boff)
        bh, _, csize, dsize = struct.unpack("<IIII", f.read(16))
        if csize == 32000:
            out += f.read(dsize)
        else:
            out += zlib.decompress(f.read(csize), -15)
    return bytes(out[:usize])


TYPES = {0: "str", 1: "bool", 2: "i8", 3: "u8", 4: "i16", 5: "u16", 6: "i32", 7: "u32", 9: "f32", 0xB: "u64"}


def header(sheet):
    d = read_file(f"exd/{sheet}.exh")
    assert d[:4] == b"EXHF", d[:4]
    row_size, ncols, npages, nlangs = struct.unpack_from(">HHHH", d, 6)
    variant = d[0x11]
    rows = struct.unpack_from(">I", d, 0x14)[0]
    cols = [struct.unpack_from(">HH", d, 0x20 + 4 * i) for i in range(ncols)]
    p = 0x20 + 4 * ncols
    pages = [struct.unpack_from(">II", d, p + 8 * i) for i in range(npages)]
    p += 8 * npages
    langs = [struct.unpack_from("<H", d, p + 2 * i)[0] for i in range(nlangs)]
    return dict(row_size=row_size, cols=cols, pages=pages, langs=langs, variant=variant, rows=rows)


LANG = {0: "", 1: "_ja", 2: "_en", 3: "_de", 4: "_fr"}


def rows(sheet, lang="en"):
    h = header(sheet)
    code = {"none": 0, "ja": 1, "en": 2, "de": 3, "fr": 4}[lang]
    if code not in h["langs"]:
        code = h["langs"][0]
    for start, _count in h["pages"]:
        d = read_file(f"exd/{sheet}_{start}{LANG[code]}.exd")
        isize = struct.unpack_from(">I", d, 8)[0]
        for i in range(0x20, 0x20 + isize, 8):
            rid, roff = struct.unpack_from(">II", d, i)
            size, _count2 = struct.unpack_from(">IH", d, roff)
            base = roff + 6
            vals = []
            for t, o in h["cols"]:
                if t == 0:
                    so = struct.unpack_from(">I", d, base + o)[0]
                    s = d[base + h["row_size"] + so:]
                    vals.append(s[: s.index(b"\0")].decode("utf-8", "replace"))
                elif t == 1:
                    vals.append(d[base + o] != 0)
                elif t in (2, 3, 4, 5, 6, 7, 9, 0xB):
                    fmt = {2: "b", 3: "B", 4: "h", 5: "H", 6: "i", 7: "I", 9: "f", 0xB: "Q"}[t]
                    vals.append(struct.unpack_from(">" + fmt, d, base + o)[0])
                elif 0x19 <= t <= 0x20:
                    vals.append(bool(d[base + o] & (1 << (t - 0x19))))
                else:
                    vals.append(f"?{t:#x}")
            yield rid, vals


if __name__ == "__main__":
    cmd, sheet = sys.argv[1], sys.argv[2]
    if cmd == "header":
        h = header(sheet)
        print({k: v for k, v in h.items() if k != "cols"})
        for i, (t, o) in enumerate(h["cols"]):
            print(i, TYPES.get(t, hex(t)), hex(o))
    elif cmd == "rows":
        lang = sys.argv[3] if len(sys.argv) > 3 else "en"
        n = int(sys.argv[4]) if len(sys.argv) > 4 else 10
        for k, (rid, v) in enumerate(rows(sheet, lang)):
            if k >= n:
                break
            print(rid, v)
    elif cmd == "find":
        col, val = sys.argv[3].split("=")
        col = int(col)
        for rid, v in rows(sheet, sys.argv[4] if len(sys.argv) > 4 else "en"):
            if str(v[col]) == val:
                print(rid, v)
