# overachiever-eu/pg-ripple — Overachiever Coach KG fork

**Upstream:** [trickle-labs/pg-ripple](https://github.com/trickle-labs/pg-ripple) (public, default branch `main`)  
**This fork:** [overachiever-eu/pg-ripple](https://github.com/overachiever-eu/pg-ripple)  
**Consumer:** Overachiever Coach KG (`/workspace/overachiever-coach-kg`) — parallel to the Overachiever app; never tied to the app DB.

## Policy: thin fork

- Keep the fork **thin**: land only patches we need for the coach KG, with clear provenance and tests.
- Improve **as the project goes** — do not wait for upstream, and do **not** depend on the upstream published image long-term.
- Prefer small, reviewable commits; sync from `upstream/main` periodically; upstream PRs when a patch is generally useful.
- Default image tags for our builds: `oa-pg-ripple:dev` (local) and, when GHCR push is configured, `ghcr.io/overachiever-eu/pg-ripple:dev`.

## Patch backlog (owned here)

Ordered by priority for the coach KG spike (see also coach-kg `BLOCKERS.md` / `docs/SHACL.md`).

| # | Patch | Why | Notes |
|---|--------|-----|--------|
| 1 | **ORDER BY alpha / lexical sort** | SPARQL `ORDER BY ?var` / `ORDER BY STR(?)` returns dictionary/insertion order in 0.136.0; `DESC` only reverses it. Unusable for UI lists (classes, labels). | **Merged to `main`** via PR #1 (`589b6ca5`, FORK-ORDERBY-01). Repro was in coach-kg `docs/SPIKE.md`. |
| 2 | **SPARQL `SUM` / decimal aggregates** | `SUM(?x)` / `AVG(?x)` fail with `function pg_ripple.numeric_type_code_spi(bigint) does not exist` after swapping to a post-0.136.0 build while keeping an older 0.136.0 catalog. | **This PR** (FORK-SUM-01): one-shot catalog repair + regress test. |
| 3 | **Cross-graph SHACL** | `validate(graph)` is graph-local. Enum / class individuals typed only in the ontology graph cannot satisfy `sh:class` in a tenant graph. | Today we work around with `sh:nodeKind` + `sh:in` for enums. Want: optional import/union graphs or class lookup across configured graphs. |
| 4 | **`load_shacl(graph IRI)`** | In 0.136.0 `load_shacl` takes Turtle **text**, not a graph IRI (some docs imply IRI). We dual-load shapes into a named graph *and* the catalog. | Want: `load_shacl` from an already-loaded named graph IRI so SPARQL and the catalog stay single-source. |
| 5 | **File LOAD allowlist UX** | `*_file` loaders need `pg_ripple.copy_rdf_allowed_paths` **and** paths under PGDATA. Mounted `/kg/…` fails even with allowlist. GUC is `Sighup` (docs sometimes say `Suset`). | Want: clearer errors, documented allowlist+PGDATA contract, and/or safe load-from-bind-mount for local/dev. Coach-kg currently loads via client-side `cat` → `load_turtle_into_graph(text, …)`. |
| 7 | **SHACL numeric ranges** | `sh:minInclusive/maxInclusive/minExclusive/maxExclusive` were parsed but never enforced: the bound literal was looked up as an IRI, missed, and the check was skipped. Coach-kg slot bounds (sets ≤ 20, load ≤ 1000 kg) were API-only. | **Fixed** (FORK-SHACL-RANGE-01), see below. |
| 6 | **Write-time SHACL `minCount`** | Offline `validate()` checks `sh:minCount`; sync `insert_triple()` cannot see absence on a single insert. | Want: write-time / deferred checks that enforce required cardinality (batch flush, txn-end validate, or async mode with clear semantics). |

## FORK-ORDERBY-01: lexical / value ORDER BY

**Root cause (0.136.0):** `translate_order_by` emitted `ORDER BY <column>` where the column is the
BIGINT dictionary ID. Dictionary IDs are IDENTITY values, so IRIs and strings came back in load
order and `DESC` merely reversed it. Only inline-encoded integers/dates happened to sort correctly
(their bit packing is order-preserving). Non-variable ORDER BY expressions (`STR(?l)`, `LCASE(?l)`,
`?v * 2`) were silently dropped.

**Fix** (`src/sparql/translate/filter/filter_dispatch.rs`, `sqlgen.rs`, `translate/distinct.rs`):
each ORDER BY condition becomes a composite key following SPARQL 1.1 §15.1:

1. term category: blank node < IRI < numeric < boolean < date/dateTime < other literals < quoted triples;
2. numeric value (inline xsd:integer + dictionary decimal/double/float/derived integer types);
3. chronological value (inline UTC dateTime/date + dictionary dateTime with offsets);
4. lexical form / IRI string in codepoint order (`COLLATE "C"`, i.e. `fn:compare` codepoint
   collation: upper-case sorts before lower-case — use `ORDER BY LCASE(?l)` for case-insensitive lists);
5. dictionary ID as the deterministic tie-breaker.

Also: `STR()`, `LCASE()`, `UCASE()` ORDER BY expressions are supported directly; other expressions
(arithmetic, `CONCAT`, BIND variables) are evaluated once in a fenced (`OFFSET 0`) inner query, and
terms minted during the statement fall back to the SPI decoders (`decode_id`, `decode_numeric_spi`)
because they are invisible to the statement snapshot. Raw aggregate outputs (`COUNT`, `SUM`,
`GROUP_CONCAT`) are ordered by their SQL value. `SELECT DISTINCT … ORDER BY` is ordered in an outer
query. Unbound values keep the previous placement (last for ASC, first for DESC).

Regression test: `tests/pg_regress/sql/sparql_order_by_lexical.sql`.

## FORK-PLAN-CACHE-01: no cached plans with unresolved IRIs

**Symptom (coach-kg open item 4):** the first run of an integration test against a fresh graph failed
with "routine not found"; reruns passed. **Root cause:** the SPARQL→SQL translator encodes constant
IRIs once; an IRI that is not in the dictionary yet (a fresh graph, user or predicate) becomes a
`FALSE` condition. That translation went into the per-backend plan cache keyed by query text, so on
the same (pooled) connection the identical query kept returning nothing after the IRI was written.

**Fix:** `Ctx.encode_iri` records a miss (`unresolved_iri`), `Translation.cacheable` is false for such a
translation and `sparql/plan.rs` skips `plan_cache::put_canonical` for it (both SELECT entry points).
Fully resolved queries are cached exactly as before. Regression test
`tests/pg_regress/sql/sparql_plan_cache_unresolved.sql`: SELECT on unknown IRIs → 0, INSERT, the
identical SELECT → 1 (was 0 on the previous image).

## FORK-SHACL-RANGE-01: numeric range constraints

**Root cause (0.136.0):** `src/shacl/constraints/relational.rs` resolved the bound (`20`,
`"1000.0"^^xsd:decimal`) with `dictionary::lookup_iri`, which never matches a literal, and returned
early ("open world"), so every range constraint was silently skipped in `validate()`.

**Fix:** numeric bounds (bare Turtle numbers or typed/plain numeric literals) are compared by value
against each decoded value node (`numeric_value`); a non-numeric value against a numeric bound is a
violation (SHACL Core §4.3: comparison not possible → violation). Non-numeric bounds (dates, strings)
keep the previous dictionary comparison. Unit tests: `range_tests` in `relational.rs`; regression:
`tests/pg_regress/sql/shacl_numeric_range.sql` (inclusive edges conform, exclusive edges, 99 sets,
2000 kg, negative load and a string value are violations). `w3c_shacl_conformance` and
`shacl_new_constraints` outputs unchanged.

**Fast rebuild:** `docker/Dockerfile.overlay` compiles only pg_ripple and layers the new `.so` +
extension SQL onto the previous fork image (other extensions unchanged):

```bash
sudo docker build -f docker/Dockerfile.overlay \
  --build-arg BASE=ghcr.io/overachiever-eu/pg-ripple:dev@sha256:<previous> -t oa-pg-ripple:dev .
```

**Image (2026-10-09):** overlay on `dev@sha256:96326a28…` →
`ghcr.io/overachiever-eu/pg-ripple:dev` and `:shacl-range-01`,
digest `sha256:002df4644ffe3dd36043b5f8255ddcce6b8984d6d6a340f9b31785da7ea238f4` (main `f3deeaf1`).

## FORK-NESTED-OPT: OPTIONAL → INNER JOIN from SHACL hints is opt-in

**Symptom (coach-kg):** `<s> :l ?x . OPTIONAL { <s> :p ?e . OPTIONAL { ?e rdfs:label ?el } }` returned
`?e` = null although `<s> :p <e>` exists. It looked like a nested-OPTIONAL bug.

**Root cause:** OPT-INNER-01 (`shacl_right_is_mandatory` in `src/sparql/translate/bgp.rs`) turned an
OPTIONAL into an INNER JOIN whenever every predicate on its right side has `sh:minCount ≥ 1` in *any*
loaded shape. A shape's minCount only binds that shape's focus nodes (and stored data need not be
valid), so the inner `?e rdfs:label ?el` became mandatory for every `?e`, the inner join emptied, and the
outer OPTIONAL then returned null. Nesting only made it visible.

**Fix:** the promotion only runs when the new GUC `pg_ripple.shacl_optional_promotion` is on (default
off); the GUC is part of the plan-cache key. Regression: `tests/pg_regress/sql/sparql_optional_shacl_promotion.sql`
(shape with minCount on rdfs:label targeting another class; nested OPTIONAL keeps the outer binding,
binds both when present, flat OPTIONAL keeps unlabelled subjects and uses LEFT JOIN; opt-in still
promotes). `shacl_query_hints` and `shacl_sparql_hints` outputs unchanged.

**Image (2026-10-09):** overlay on `dev@sha256:d2392acc…` → `ghcr.io/overachiever-eu/pg-ripple:dev` and
`:nested-opt-01`, digest `sha256:dc44f7ee7ee57c6aab976534de0a40e0672fdb42b864bfbb77587e01efe8cbf8` (main `01aa1fd5`).

## FORK-WCOJ-CYCLE-01 / FORK-JOIN-NULLSAFE-01: star reads plan as hash joins

**Symptom (coach-kg, Oct 2026):** the whole-node read `SELECT ?s ?p ?o WHERE { GRAPH <g> { { SELECT ?s WHERE { ?s :user <u> ; :partOf ?w } } ?s ?p ?o } }`
took 150–1000 ms on a tenant with ~5,800 triples (27 ms with a hash join), and made a 26-workout history import
take 42 s. Coach-kg works around it (flat star + `pg_ripple.wcoj_enabled = off` and `enable_nestloop = off` scoped to
the read, coach-kg #297/#300); these patches fix the translation itself.

**FORK-WCOJ-CYCLE-01 (`src/sparql/wcoj/mod.rs`):** `detect_cyclic_bgp` linked every pair of variables *within* a
triple pattern, so one `?s ?p ?o` was already a triangle and every star with an unbound predicate counted as
cyclic. That switched on the WCOJ planner hint (`enable_hashjoin = off`, `join_collapse_limit = 1`). Now a GYO
reduction (alpha-acyclicity of the hypergraph of pattern variable sets): stars, chains and patterns that share
two variables are acyclic; a triangle (with or without arms) stays cyclic. Unit tests in the module.

**FORK-JOIN-NULLSAFE-01 (`src/sparql/translate/join.rs`, `Fragment::merge_certain`):** every SPARQL Join merged
shared variables as `(l IS NULL OR l = r)` + `COALESCE(l, r)`, which Postgres can only run as a nested loop. The
null-safe form is only needed when a side may leave the variable unbound. `certain_vars` computes the variables a
pattern binds in every solution (conservative: BGP/path terms, Join/Lateral union, LeftJoin/Minus left side, Union
intersection, Values columns without UNDEF, Project filter; Group/Service/others nothing), and shared variables
certain on both sides join on plain `=`. Same results: for non-null values both forms are equal. Unit tests for
sub-SELECT + star, OPTIONAL, UNION and VALUES/UNDEF.

**Image:** not built or deployed yet (needs the coach-kg owner's go-ahead).

## How we build the image

Upstream publishes via `.github/workflows/release.yml` → `docker/build-push-action` on `Dockerfile` to `ghcr.io/trickle-labs/pg-ripple`. Local equivalent:

```bash
# From repo root (multi-stage: Rust/pgrx pg_ripple + pg_trickle + pg_tide + PostGIS + pgvector → postgres:18-bookworm)
docker build -t oa-pg-ripple:dev .
# optional GHCR tag / push (needs packages:write + docker login ghcr.io)
docker tag oa-pg-ripple:dev ghcr.io/overachiever-eu/pg-ripple:dev
echo "$GHCR_TOKEN" | docker login ghcr.io -u giovannidegani --password-stdin
docker push ghcr.io/overachiever-eu/pg-ripple:dev
```

Also: `just docker-build tag=dev` → `docker build -t pg-ripple:dev .`

**Deps (see Dockerfile ARGs / `.versions.toml`):** Rust via image (`rust:1-bookworm`; toolchain pin in tree is 1.95.0), `cargo-pgrx` 0.18.0, PostgreSQL 18 headers, clang, PostGIS 3.5.6, pgvector 0.8.2, pg_trickle 0.68.0, pg_tide 0.33.0. Full batteries-included build is large (Rust compile + several extensions) — expect a long first build.

## Coach KG compose

`overachiever-coach-kg/docker-compose.yml` defaults to `ghcr.io/overachiever-eu/pg-ripple:dev` pinned by digest (fork image with ORDER BY fix). Upstream `0.136.0` remains documented as an alternative.

## Sync

```bash
git fetch upstream
git merge upstream/main   # or rebase; resolve carefully around our patches
```

## Build status (box, 2026-10-05 CEST)

**ORDER BY fix on `main`:** merged via PR #1 (`589b6ca5`, FORK-ORDERBY-01).

Full batteries-included rebuild + GHCR push (after disk prune + apt retry):

```text
oa-pg-ripple:dev                       sha256:4ef618cf85d2…
ghcr.io/overachiever-eu/pg-ripple:dev   same id
GHCR digest: sha256:806dbbe29cbb3d7542cf93bb09edf76f42bc76299560f14190174418e97c62f5
```

```bash
cd /workspace/pg-ripple   # on main
echo "$GHCR_TOKEN" | sudo docker login ghcr.io -u giovannidegani --password-stdin
sudo docker build -t oa-pg-ripple:dev -t ghcr.io/overachiever-eu/pg-ripple:dev .
sudo docker push ghcr.io/overachiever-eu/pg-ripple:dev
```

Coach KG compose pins that digest. Verified: ontology class labels
`ORDER BY ?l LIMIT 6` → Body metric, Coach config, Constraint, Exercise,
External import, Food item (was load-order on unpatched 0.136.0).

Earlier notes: first `dev` build used image id `d42de6bf…` (pre-ORDERBY);
an incremental `orderby-lexical` overlay was used for testing before the
full rebuild. Transient `deb.debian.org` HTTP 500s inside the build
container may require a retry.
