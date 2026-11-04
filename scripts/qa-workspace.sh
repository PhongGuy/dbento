#!/usr/bin/env bash
# Builds a QA workspace in <dir>: the Chinook SQLite database and four connections that,
# sorted by name, are chinook (0), nopg (1), prod (2) and ro (3).
set -euo pipefail

if [ $# -ne 1 ]; then
    echo "usage: $0 <dir>" >&2
    exit 2
fi
dir=$1
root=$(cd "$(dirname "$0")/.." && pwd)

mkdir -p "$dir/data"
rm -f "$dir/data/chinook.db"
python3 -c 'import sqlite3,sys; sqlite3.connect(sys.argv[1]).executescript(open(sys.argv[2],encoding="utf-8-sig").read())' \
    "$dir/data/chinook.db" "$root/datasets/chinook/Chinook_Sqlite.sql"

# nopg points at the discard port, so connecting to it always fails.
cat > "$dir/dbento.toml" <<'TOML'
dbento = 1

[connections.chinook]
engine = "sqlite"
path = "data/chinook.db"
color = "green"

[connections.nopg]
engine = "postgres"
host = "127.0.0.1"
port = 9
database = "x"
user = "x"
read_only = true

[connections.prod]
engine = "sqlite"
path = "data/chinook.db"
color = "red"

[connections.ro]
engine = "sqlite"
path = "data/chinook.db"
read_only = true
TOML
