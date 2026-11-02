"""Sum inclusive samples per function from a demangled macOS `sample` call tree.

usage: python3 scripts/profile_inclusive.py SAMPLE.txt [TOP] [REGEX]
Record:  sample <pid> 8 -file s.txt && rustfilt -i s.txt -o s.dm.txt
"""
import collections
import re
import sys

totals = collections.Counter()
line_re = re.compile(r"^[\s+!:|]*(\d+)\s+(.+?)\s+\(in ")
keep = re.compile(sys.argv[3] if len(sys.argv) > 3 else "dagger_workflow_core|serde_jcs|sqlite3_exec")
for line in open(sys.argv[1], errors="replace"):
    if line.startswith("Sort by top"):
        break
    match = line_re.match(line)
    if not match:
        continue
    count, symbol = int(match.group(1)), match.group(2)
    symbol = re.sub(r"::h[0-9a-f]{16}$", "", symbol)
    symbol = re.sub(r"<[^<>]*(<[^<>]*(<[^<>]*>[^<>]*)*>[^<>]*)*>", "<>", symbol)
    if keep.search(symbol):
        totals[symbol[:140]] += count
for symbol, count in totals.most_common(int(sys.argv[2]) if len(sys.argv) > 2 else 40):
    print(count, symbol)
