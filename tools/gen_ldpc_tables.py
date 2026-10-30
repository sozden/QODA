import re, sys
text = open(sys.argv[1], encoding="utf-8").read().splitlines()
out_path = sys.argv[2]

NORMAL = [("1/4",16200),("1/3",21600),("2/5",25920),("1/2",32400),("3/5",38880),("2/3",43200),
          ("3/4",48600),("4/5",51840),("5/6",54000),("8/9",57600),("9/10",58320)]
SHORT = [("1/4",3240),("1/3",5400),("2/5",6480),("1/2",7200),("3/5",9720),("2/3",10800),
         ("3/4",11880),("4/5",12600),("5/6",13320),("8/9",14400)]

def section(prefix, idx):
    start = next(i for i,l in enumerate(text) if l.strip().startswith(f"Table {prefix}.{idx}:"))
    lines = []
    for l in text[start+1:]:
        s = l.strip()
        if s.startswith("Table ") or s.startswith("Annex "):
            break
        if not s or "ETSI" in s or s.startswith("====="):
            continue
        if not re.fullmatch(r"[0-9 ]+", s):
            raise SystemExit(f"unexpected row {prefix}.{idx}: {s!r}")
        lines.append([int(t) for t in s.split()])
    return lines

def rebuild_one(lines, rows, m):
    # Line wraps do not change the order of the numbers; the high-degree rows are
    # as long as the longest text line, the rest have degree 3.
    d1 = max(map(len, lines))
    expected = lambda d1, a: a * d1 + (rows - a) * 3
    tokens = [t for l in lines for t in l]
    if d1 == 3:
        a = 0
    else:
        a, rem = divmod(len(tokens) - 3 * rows, d1 - 3)
        if rem != 0 or not 0 < a <= rows:
            raise SystemExit(f"could not resolve row structure: {len(tokens)} numbers, {rows} rows, d1={d1}")
    assert expected(d1, a) == len(tokens)
    table, p = [], 0
    for r in range(rows):
        d = d1 if r < a else 3
        table.append(tokens[p:p + d]); p += d
    assert all(v < m for row in table for v in row), "address out of range"
    return d1, a, table

def merge_at(lines, i):
    # A number split in two across a page break: "29 734" + "7 8027" -> "29 7347 8027".
    return lines[:i] + [[lines[i][0], int(f"{lines[i][1]}{lines[i+1][0]}"), lines[i+1][1]]] + lines[i+2:]

def rebuild(lines, rows, n, k):
    """Tries the split-number candidates; picks the solution with a uniform check
    degree for normal codes and with the right number count for short codes."""
    m = n - k
    candidates = [None] + [i for i in range(len(lines) - 1) if len(lines[i]) == 2 and len(lines[i+1]) == 2]
    good = []
    for c in candidates:
        attempt = lines if c is None else merge_at(lines, c)
        try:
            d1, a, table = rebuild_one(attempt, rows, m)
        except SystemExit:
            continue
        dmin, dmax = check_degrees(n, k, table)
        if n == 16200 or dmin == dmax:
            good.append((c, d1, a, table))
    if len(good) != 1:
        raise SystemExit(f"no unique solution: candidates {[g[0] for g in good]}")
    c, d1, a, table = good[0]
    if c is not None:
        print(f"  joined split number: {lines[c]} + {lines[c+1]}")
    return d1, a, table

def check_degrees(n, k, table):
    m = n - k; q = m // 360
    deg = [0]*m
    for row in table:
        for j in range(360):
            for x in row:
                deg[(x + j*q) % m] += 1
    return min(deg), max(deg)

def bch(prefix_line):
    i = next(i for i,l in enumerate(text) if l.strip().startswith(prefix_line))
    polys = []
    for l in text[i+1:i+13]:
        m = re.match(r"g(\d+)\(x\)\s+(.*)", l.strip())
        terms = m.group(2).replace(" ", "").split("+")
        exps = sorted(0 if t == "1" else (1 if t == "x" else int(t[1:])) for t in terms)
        polys.append(exps)
    return polys

rs = []
rs.append("//! DVB-S2 LDPC parity bit accumulator addresses and BCH polynomials.")
rs.append("//!")
rs.append("//! ETSI EN 302 307-1, annex B (n = 64800), annex C (n = 16200) and tables 6a/6b.")
rs.append("//! Generated from the standard's text by `tools/gen_ldpc_tables.py`; do not edit by hand.")
rs.append("")
rs.append("#![cfg_attr(rustfmt, rustfmt_skip)]")
rs.append("")
report = []
for prefix, n, rates in (("B", 64800, NORMAL), ("C", 16200, SHORT)):
    for idx, (rate, k) in enumerate(rates, 1):
        lines = section(prefix, idx)
        d1, a, table = rebuild(lines, k // 360, n, k)
        dmin, dmax = check_degrees(n, k, table)
        name = f"{'NORMAL' if n == 64800 else 'SHORT'}_{rate.replace('/', '_')}"
        report.append(f"{name}: k={k} rows={len(table)} ({a}×{d1} + {len(table)-a}×3) check degree {dmin}..{dmax}")
        rs.append(f"pub static {name}: [&[u16]; {len(table)}] = [")
        for row in table:
            rs.append("    &[" + ", ".join(map(str, row)) + "],")
        rs.append("];")
        rs.append("")
for label, line in (("NORMAL", "Table 6a:"), ("SHORT", "Table 6b:")):
    polys = bch(line)
    rs.append(f"/// Exponents of the nonzero terms of polynomials g1..g12.")
    rs.append(f"pub static BCH_{label}: [&[u8]; 12] = [")
    for p in polys:
        rs.append("    &[" + ", ".join(map(str, p)) + "],")
    rs.append("];")
    rs.append("")
    report.append(f"BCH_{label}: " + " | ".join(str(len(p)) for p in polys))
open(out_path, "w", encoding="utf-8", newline="\n").write("\n".join(rs))
print("\n".join(report))
