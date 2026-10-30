"""sapphire_actions.py: data/672261/actions.json, what the server needs of player actions beyond
the Action sheet's columns: combos, and the statuses actions put on (damage and healing over time,
stat changes).

Sources, in order of trust:
  1. the beta's own Action descriptions (2.0's numbers): "Combo Action: X  Combo Potency: N",
     "Additional Effect: ... Potency: N / Cure Potency: N", "Duration: Ns";
  2. Sapphire's data/actions/player.json (3.3): the status ids actions put on and their modifiers,
     for actions whose name is the same in the beta.

A damage- or heal-over-time effect the beta names takes the beta Status row of the action's own name
(Aero -> Status "Aero"), or Sapphire's, when its name matches the beta's.

python tools/sapphire_actions.py [sapphire_root]
"""
import json, os, re, sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "re"))
import exd  # noqa: E402

SAPPHIRE = sys.argv[1] if len(sys.argv) > 1 else r"C:\Users\tyler\Documents\code\3rdparty\Sapphire"
OUT = os.path.join(os.path.dirname(__file__), "..", "data", "672261", "actions.json")


def plain(text: str) -> str:
    """The description with the client's markup (0x02 ... 0x03, nested) taken out."""
    prev = None
    while prev != text:
        prev = text
        text = re.sub(r"\x02[^\x02\x03]*\x03", " ", text)
    text = re.sub(r"[\x00-\x1f\ufffd]", " ", text)
    return re.sub(r"\s+", " ", text).strip()


def main():
    actions = {rid: v for rid, v in exd.rows("Action", "en")}
    statuses = {rid: v for rid, v in exd.rows("Status", "en")}
    status_by_name = {}
    for rid, v in statuses.items():
        if v and v[0]:
            status_by_name.setdefault(v[0], rid)
    by_name = {}
    for rid, v in actions.items():
        if v and v[0]:
            by_name.setdefault(v[0], rid)
    lut = json.load(open(os.path.join(SAPPHIRE, "data", "actions", "player.json"), encoding="utf-8"))

    out = {}
    for rid, v in sorted(actions.items()):
        name, desc = v[0], plain(v[1] if isinstance(v[1], str) else "")
        if not name:
            continue
        entry = {}
        m = re.search(r"Combo Action: (.+?) Combo Potency: (\d+)", desc)
        if m and m.group(1) in by_name:
            entry["combo_from"] = by_name[m.group(1)]
            entry["combo_potency"] = int(m.group(2))
        sap = lut.get(str(rid))
        sap = sap if sap and sap.get("name") == name else None
        effects = []
        extra = desc.split("Additional Effect:", 1)[1] if "Additional Effect:" in desc else ""
        tick = None
        if re.search(r"damage over time|Venom|Bleed|Poison", extra, re.I):
            tick = "damage"
        elif re.search(r"Regen", extra):
            tick = "heal"
        if tick:
            pm = re.search(r"(?:Cure )?Potency: (\d+)", extra)
            dm = re.search(r"Duration: (\d+)s", extra)
            sid = status_by_name.get(name)
            sap_status = None
            if sap:
                for s in sap["statuses"]["target"]:
                    if any(md["modifier"] in ("TickDamage", "TickHeal") for md in s.get("modifiers", [])):
                        sap_status = s
            if sid is None and sap_status and statuses.get(sap_status["id"]):
                sid = sap_status["id"]
            duration = int(dm.group(1)) * 1000 if dm else (sap_status["duration"] if sap_status else 0)
            if sid is not None and pm and duration:
                effects.append({"on": "target", "id": sid, "duration_ms": duration, "tick": tick,
                                "potency": int(pm.group(1))})
        # Sapphire's other statuses (stat changes) when the beta has the status by the same id
        if sap:
            for side in ("caster", "target"):
                for s in sap["statuses"][side]:
                    if any(e["id"] == s["id"] for e in effects) or not statuses.get(s["id"]):
                        continue
                    mods = {md["modifier"]: md["value"] for md in s.get("modifiers", [])
                            if md["modifier"] not in ("TickDamage", "TickHeal")}
                    effect = {"on": "self" if side == "caster" else "target", "id": s["id"],
                              "duration_ms": s["duration"], "modifiers": mods}
                    # a tick the beta's text does not spell out: Sapphire's potency for it
                    for md in s.get("modifiers", []):
                        if md["modifier"] in ("TickDamage", "TickHeal"):
                            effect["tick"] = "damage" if md["modifier"] == "TickDamage" else "heal"
                            effect["potency"] = md["value"]
                    effects.append(effect)
        if effects:
            entry["statuses"] = effects
        if entry:
            entry["name"] = name
            out[str(rid)] = entry
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=1, sort_keys=True)
        f.write("\n")
    print(f"{len(out)} actions -> {os.path.normpath(OUT)}")


main()
