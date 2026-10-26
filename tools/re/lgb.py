"""lgb.py <bg level path> [file ...]: list LGB layers and instance objects (type, id, name, position).

LGB1 → one LGP1 chunk → layers → instance objects; offsets are relative to the start of the table
or entry that holds them (as in later clients' layout files).
"""
import struct, sys
from collections import Counter
import exd


def cstr(d, at):
    end = d.index(b"\0", at)
    return d[at:end].decode("utf-8", "replace")


def parse(d):
    assert d[:4] == b"LGB1", d[:4]
    chunk = 0xC
    assert d[chunk:chunk + 4] == b"LGP1"
    # after "LGP1" and its size: group id, name offset, layers offset, layer count (relative to chunk+8)
    base = chunk + 8
    group_id, name_off, layers_off, layer_count = struct.unpack_from("<iiii", d, base)
    layers = []
    lt = base + layers_off
    for i in range(layer_count):
        lo = lt + struct.unpack_from("<i", d, lt + 4 * i)[0]
        layer_id, lname_off, objs_off, obj_count = struct.unpack_from("<Iiii", d, lo)
        objs = []
        ot = lo + objs_off
        for j in range(obj_count):
            oo = ot + struct.unpack_from("<i", d, ot + 4 * j)[0]
            typ, inst, oname = struct.unpack_from("<IIi", d, oo)
            tx, ty, tz, rx, ry, rz, sx, sy, sz = struct.unpack_from("<9f", d, oo + 12)
            objs.append(dict(type=typ, id=inst, name=cstr(d, oo + oname) if oname else "",
                             pos=(tx, ty, tz), rot=(rx, ry, rz), at=oo))
        layers.append(dict(id=layer_id, name=cstr(d, lo + lname_off), objs=objs))
    return layers


if __name__ == "__main__":
    path = sys.argv[1].strip("/")
    files = sys.argv[2:] or ["planmap.lgb", "planevent.lgb"]
    for f in files:
        d = exd.read_file(f"bg/{path}/{f}")
        layers = parse(d)
        print(f"== {f}: {len(layers)} layers, types {dict(Counter(o['type'] for l in layers for o in l['objs']))}")
        for l in layers:
            for o in l["objs"]:
                if o["type"] in (40, 41, 42, 43):
                    x, y, z = o["pos"]
                    print(f"  type {o['type']} id {o['id']} {o['name']!r} at ({x:.1f}, {y:.1f}, {z:.1f}) layer {l['name']!r}")
