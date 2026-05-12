MAZE = [
    b"###################",
    b"#o...............o#",
    b"#.###.###.###.###.#",
    b"#.................#",
    b"#.###.###.###.###.#",
    b"#........#........#",
    b"####.###.#.###.####",
    b"#.................#",
    b"#.###.### ###.###.#",
    b"#.....#GG GG#.....#",
    b"#.###.#######.###.#",
    b"#.................#",
    b"####.###.#.###.####",
    b"#........#........#",
    b"#.###.###.###.###.#",
    b"#.................#",
    b"#.###.#.....#.###.#",
    b"#o....#..P..#....o#",
    b"#.###############.#",
    b"#.................#",
    b"###################",
]
ROWS = len(MAZE); COLS = len(MAZE[0])
for i, r in enumerate(MAZE):
    if len(r) != COLS:
        print(f"row {i} width {len(r)} != {COLS}")
def is_open(c, r):
    if c<0 or r<0 or c>=COLS or r>=ROWS: return False
    return MAZE[r][c:c+1] != b'#'
start = None
for r, row in enumerate(MAZE):
    for c, ch in enumerate(row):
        if ch == ord('P'): start = (c, r)
visited = {start}; frontier = [start]
while frontier:
    new = []
    for (c, r) in frontier:
        for (dc, dr) in [(0,-1),(0,1),(-1,0),(1,0)]:
            nc, nr = c+dc, r+dr
            if (nc,nr) in visited or not is_open(nc, nr): continue
            visited.add((nc, nr)); new.append((nc, nr))
    frontier = new
all_open = {(c,r) for r,row in enumerate(MAZE) for c,ch in enumerate(row) if ch != ord('#')}
unreachable = all_open - visited
print(f"Open: {len(all_open)}, Reachable from P: {len(visited)}")
if unreachable:
    print(f"UNREACHABLE: {sorted(unreachable)}")
else:
    print("MAZE FULLY CONNECTED ✓")
