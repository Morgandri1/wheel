# Copyright Morgan Metz
# Licensed under the PolyForm Noncommercial License 1.0.0.
# See the LICENSE file or https://polyformproject.org/licenses/noncommercial/1.0.0
"""Replace exactly one WHOLE LINE in a unit file, or refuse.

Used by mutate-native.sh. Whole-line rather than substring because these unit files discuss their
own directives in their commentary -- `ProtectSystem=strict` appears in prose a few lines above
where it is set -- so a substring match is ambiguous exactly where the mutation matters most. And
exactly-once rather than replace-all because a mutation that silently missed the line it meant to
break would report a green rehearsal as proof that a gate works.
"""

import sys


def main(argv: list[str]) -> int:
    if len(argv) != 4:
        print("usage: mutate-edit.py <file> <old-line> <new-lines>", file=sys.stderr)
        return 2
    path, old, new = argv[1], argv[2], argv[3]
    with open(path) as handle:
        lines = handle.read().split("\n")
    hits = [i for i, line in enumerate(lines) if line == old]
    if len(hits) != 1:
        print(
            f"mutate-native: the line {old!r} occurs {len(hits)} times in {path}, "
            "so this mutation would not apply",
            file=sys.stderr,
        )
        return 1
    lines[hits[0]:hits[0] + 1] = new.split("\n")
    with open(path, "w") as handle:
        handle.write("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
