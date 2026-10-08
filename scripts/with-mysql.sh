#!/usr/bin/env bash
# Run a command against a throwaway MySQL server, with MABAT_TEST_MYSQL_URL set.
#
#   scripts/with-mysql.sh                                 # cargo test --workspace
#   scripts/with-mysql.sh scripts/with-postgres.sh        # both databases
#   scripts/with-mysql.sh cargo test -p mabat --features mysql --test mysql
#
# Needs the MySQL server binaries (mysqld, mysqladmin). Set MYSQL_BIN to their directory if
# they are not on the PATH. The server's data lives in a temporary directory and is removed
# afterwards.
set -euo pipefail

if [[ -z "${MYSQL_BIN:-}" ]]; then
  if command -v mysqld >/dev/null; then
    MYSQL_BIN=$(dirname "$(command -v mysqld)")
  else
    MYSQL_BIN=$(ls -d /opt/homebrew/opt/mysql/bin /opt/homebrew/opt/mysql@*/bin /usr/sbin 2>/dev/null | head -1 || true)
  fi
fi
if [[ ! -x "${MYSQL_BIN:-}/mysqld" ]]; then
  echo "MySQL server binaries not found; set MYSQL_BIN to the directory containing mysqld" >&2
  exit 1
fi
# mysqladmin is a client program, next to mysqld or on the PATH
MYSQLADMIN=$MYSQL_BIN/mysqladmin
[[ -x "$MYSQLADMIN" ]] || MYSQLADMIN=$(command -v mysqladmin)

PORT=${MABAT_TEST_MYSQL_PORT:-53306}
DIR=$(mktemp -d "${TMPDIR:-/tmp}/mabat-mysql.XXXXXX")
cleanup() {
  "$MYSQLADMIN" --socket="$DIR/mysqld.sock" -u root shutdown >/dev/null 2>&1 || true
  [[ -n "${PID:-}" ]] && wait "$PID" 2>/dev/null || true
  rm -rf "$DIR"
}
trap cleanup EXIT

"$MYSQL_BIN/mysqld" --no-defaults --initialize-insecure --datadir="$DIR/data" >"$DIR/init.log" 2>&1
"$MYSQL_BIN/mysqld" --no-defaults --datadir="$DIR/data" --port="$PORT" --bind-address=127.0.0.1 \
  --socket="$DIR/mysqld.sock" --mysqlx=OFF --log-error="$DIR/server.log" --pid-file="$DIR/mysqld.pid" &
PID=$!
for _ in $(seq 1 100); do
  "$MYSQLADMIN" --socket="$DIR/mysqld.sock" -u root ping >/dev/null 2>&1 && break
  sleep 0.2
done
"$MYSQLADMIN" --socket="$DIR/mysqld.sock" -u root create mabat

export MABAT_TEST_MYSQL_URL="mysql://root@127.0.0.1:$PORT/mabat"
if [[ $# -eq 0 ]]; then
  set -- cargo test --workspace
fi
"$@"
