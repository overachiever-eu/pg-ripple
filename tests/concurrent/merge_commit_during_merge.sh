#!/usr/bin/env bash
# tests/concurrent/merge_commit_during_merge.sh
#
# MERGE-CATCHUP-01: a triple whose transaction commits while a merge is running must survive the
# merge. Before the fix, Step 1 (CREATE main_new AS main ∪ delta) did not see the row and Step 4's
# TRUNCATE delta (which waited for the writer to commit) wiped it: committed data silently lost.
#
# Deterministic: writer B inserts and holds its transaction open; merge A starts (B's row is
# invisible to Step 1) and reaches the delta lock/TRUNCATE, which waits for B; B commits; A ends.
#
# Usage:
#   cargo pgrx start pg18 && bash tests/concurrent/merge_commit_during_merge.sh
#   PSQL="docker exec -i <container> psql -U postgres" bash tests/concurrent/merge_commit_during_merge.sh
#
# Environment: PSQL (psql command without a database; default uses PGHOST/PGPORT/PGUSER),
#   PGHOST (/tmp), PGPORT (28818), PGUSER (current user), HOLD_SECS (3, < merge_lock_timeout_ms).

set -euo pipefail

PGHOST="${PGHOST:-/tmp}"
PGPORT="${PGPORT:-28818}"
PGUSER="${PGUSER:-$(whoami)}"
PSQL="${PSQL:-psql -h $PGHOST -p $PGPORT -U $PGUSER}"
HOLD_SECS="${HOLD_SECS:-3}"
TEST_DB="pg_ripple_merge_commit_race_test"
EX="https://race.test/"

q() { $PSQL -v ON_ERROR_STOP=1 -qAt "$@"; }
cleanup() { q postgres -c "DROP DATABASE IF EXISTS \"$TEST_DB\" WITH (FORCE);" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "=== merge_commit_during_merge.sh ==="
cleanup
q postgres -c "CREATE DATABASE \"$TEST_DB\";" >/dev/null
q "$TEST_DB" -c "CREATE EXTENSION pg_ripple;" >/dev/null

# Enough triples for <pred> to get its own (HTAP) VP table, then merge them into main.
{ echo "SELECT pg_ripple.load_ntriples(\$nt\$"
  for i in $(seq 1 2000); do echo "<${EX}s$i> <${EX}pred> <${EX}o$i> ."; done
  echo "\$nt\$);"; } | q "$TEST_DB" >/dev/null
PRED=$(q "$TEST_DB" -c "SELECT p.id FROM _pg_ripple.predicates p WHERE p.htap AND p.id = pg_ripple.encode_term('${EX}pred', 0::smallint);")
if [[ -z "$PRED" ]]; then echo "SKIP: <${EX}pred> is not an HTAP predicate here"; exit 0; fi
q "$TEST_DB" -c "SELECT pg_ripple.compact();" >/dev/null
# One committed delta row, so the next compact() merges this predicate.
q "$TEST_DB" -c "SELECT pg_ripple.sparql_update('INSERT DATA { <${EX}early> <${EX}pred> <${EX}o> }');" >/dev/null

# B: insert, hold the transaction open, commit.
q "$TEST_DB" <<SQL >/dev/null &
BEGIN;
SELECT pg_ripple.sparql_update('INSERT DATA { <${EX}late> <${EX}pred> <${EX}o> }');
SELECT pg_sleep($HOLD_SECS);
COMMIT;
SQL
B=$!
sleep 1
# A: merge while B is open (HOLD_SECS must stay below pg_ripple.merge_lock_timeout_ms, 5 s default).
q "$TEST_DB" -c "SELECT pg_ripple.compact();" >/dev/null
wait "$B"

LATE=$(q "$TEST_DB" -c "SELECT pg_ripple.sparql_ask('ASK { <${EX}late> <${EX}pred> <${EX}o> }');")
N=$(q "$TEST_DB" -c "SELECT count(*) FROM pg_ripple.sparql('SELECT ?s WHERE { ?s <${EX}pred> ?o }');")
echo "late triple present: $LATE, triples: $N (expect t, 2002)"
if [[ "$LATE" != "t" || "$N" != "2002" ]]; then
    echo "FAIL: a triple committed during the merge was lost"
    exit 1
fi
echo "PASS"
