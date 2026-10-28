//! MERGE-CATCHUP-01 (OA fork, Oct 28 2026): delta rows committed during a merge.
//!
//! `merge_predicate` Step 1 builds `main_new` from delta under its own snapshot, so a write that
//! committed after it (e.g. a transaction that inserted before the merge began and committed while
//! the merge waited for a lock) is not in `main_new`, and Step 4's `TRUNCATE delta` dropped those
//! rows without them ever reaching main: committed triples silently lost (seen on a preview import:
//! every rdf:type triple of one 194-workout batch, all other triples kept).

use pgrx::prelude::*;

/// Inside the swap transaction, after the new main is in place and before delta is truncated:
/// block new delta writes until the transaction ends (reads continue), then copy every delta row
/// whose statement id is not in the new main. Returns the number of rows copied.
pub(crate) fn catch_up_delta(pred_id: i64, main: &str, delta: &str, dedup_on_merge: bool) -> i64 {
    Spi::run_with_args(&format!("LOCK TABLE {delta} IN EXCLUSIVE MODE"), &[])
        .unwrap_or_else(|e| pgrx::error!("merge: lock delta (catch-up) error: {e}"));
    let catch_up_sql = if dedup_on_merge {
        format!(
            "WITH ins AS ( \
                 INSERT INTO {main} (s, o, g, i, source) \
                 SELECT DISTINCT ON (d.s, d.o, d.g) d.s, d.o, d.g, d.i, d.source \
                 FROM {delta} d \
                 WHERE NOT EXISTS (SELECT 1 FROM {main} m WHERE m.i = d.i) \
                   AND NOT EXISTS (SELECT 1 FROM {main} m \
                                   WHERE m.s = d.s AND m.o = d.o AND m.g = d.g) \
                 ORDER BY d.s, d.o, d.g, d.i ASC \
                 RETURNING 1) \
             SELECT count(*)::bigint FROM ins"
        )
    } else {
        format!(
            "WITH ins AS ( \
                 INSERT INTO {main} (s, o, g, i, source) \
                 SELECT d.s, d.o, d.g, d.i, d.source \
                 FROM {delta} d \
                 WHERE NOT EXISTS (SELECT 1 FROM {main} m WHERE m.i = d.i) \
                 RETURNING 1) \
             SELECT count(*)::bigint FROM ins"
        )
    };
    let caught_up: i64 = Spi::get_one_with_args::<i64>(&catch_up_sql, &[])
        .unwrap_or_else(|e| pgrx::error!("merge: delta catch-up error: {e}"))
        .unwrap_or(0);
    if caught_up > 0 {
        pgrx::log!("merge: vp_{pred_id}: {caught_up} delta rows committed during the merge kept");
    }
    caught_up
}
