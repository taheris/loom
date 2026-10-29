"""Check generated kernel OID bytes, offsets and lookup order independently."""

import re
import sys
from pathlib import Path

header, generated = (Path(path).read_text() for path in sys.argv[1:])
entries = re.findall(r"\bOID_(\w+),\s*/\*\s*([012][.\d]*)\s*\*/", header)
assert entries and len({name for name, _ in entries}) == len(entries)
expected = {}
for name, dotted in entries:
    parts = [int(part) for part in dotted.split(".")]
    encoded = [40 * parts[0] + parts[1]]
    for number in parts[2:]:
        octets = [number & 127]
        number >>= 7
        while number:
            octets.insert(0, 128 | (number & 127))
            number >>= 7
        encoded.extend(octets)
    expected[name] = bytes(encoded)
assert expected["signed_data"] == bytes.fromhex("2a864886f70d010702")

block = re.search(r"oid_data\[\d+\] = \{(.*?)\n\};", generated, re.S).group(1)
actual = {}
for row in block.strip().splitlines():
    numbers, name = row.split("//")
    actual[name.strip()] = bytes(int(value) for value in numbers.split(",") if value.strip())
assert actual == expected, "generated OID encoding differs"

offsets = dict(re.findall(r"\[OID_(\w+)\] = (\d+),?", generated))
offset = 0
for name, _ in entries:
    assert int(offsets[name]) == offset, f"incorrect offset: {name}"
    offset += len(expected[name])
assert int(offsets["_NR"]) == offset


def lookup_hash(data):
    value = len(data) - 1 + sum(octet * 33 for octet in data)
    return ((value >> 24) ^ (value >> 16) ^ (value >> 8) ^ value) & 255


ordered = sorted(expected, key=lambda name: (
    lookup_hash(expected[name]), len(expected[name]), tuple(reversed(expected[name]))))
rows = re.findall(r"\[\s*(\d+)\] = \{\s*(\d+), OID_(\w+)\s*\}, // (\w+)", generated)
assert len(rows) == len(ordered)
for index, (position, digest, name, hex_bytes) in enumerate(rows):
    assert int(position) == index and name == ordered[index]
    assert int(digest) == lookup_hash(expected[name])
    assert bytes.fromhex(hex_bytes) == expected[name]
print(f"validated {len(entries)} kernel OID encodings, offsets and lookup hashes")
