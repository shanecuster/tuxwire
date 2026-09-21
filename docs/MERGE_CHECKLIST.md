# Merge checklist — `feature/freshrss` → `master`

Written at the end of Phase 7, with `feature/freshrss` fully up to date
with `master` (`git merge-base master feature/freshrss` ==
`git rev-parse master`, both `063e24d`) and migration `004` free on
`master`. **Re-check everything below at the actual time of merge** —
this file describes the state as of today, not a guarantee that holds
until someone runs it. Nothing on this page has been run as part of
writing it; every command is here for a human to execute deliberately,
not something this session executed on your behalf.

---

## 1. Confirm the branch is still up to date with `master`

```bash
git fetch origin master
git log --oneline master..feature/freshrss | wc -l   # commits only on this branch
git log --oneline feature/freshrss..master           # commits only on master, if any
```

If the second command prints anything, `master` has moved since this
branch diverged — rebase or merge `master` into `feature/freshrss` first
(or merge `master` in, if this branch's history should stay intact) and
re-run every check below against the result, not against today's
snapshot.

## 2. Confirm the next migration number doesn't collide

```bash
git ls-tree -r master --name-only -- migrations/
ls migrations/
```

As of today: `master` has `001`–`003`; this branch adds `004_freshrss_tables.sql`
only. If `master` has gained a `004_*.sql` of its own in the meantime
(only possible if step 1 found `master` has moved), this branch's
migration needs renumbering to the next free number before merging —
never let two different migrations claim the same number.

## 3. Decide: does `freshrss` join `default` features?

`Cargo.toml` currently has:

```toml
[features]
freshrss = ["reqwest/form"]
```

Not part of `[features] default = [...]` — there isn't one, so no
feature is on by default today, `freshrss` included. Two real options at
merge time, deliberately left as a decision for then rather than decided
now (per `docs/ARCHITECTURE.md`'s "Deferred" list):

- **Leave it opt-in** (`cargo build --features freshrss` to get it): the
  default binary anyone builds stays identical to today's `master` in
  every way, including binary size and the dependency set actually
  compiled. FreshRSS users add one flag (or one line to their own build
  script/packaging).
- **Add it to `default`**: every build includes FreshRSS support (still
  inert unless `freshrss.toml` enables it at runtime — see "off means
  identical to master" in ARCHITECTURE.md, which is about *runtime*
  behavior and holds either way). Simpler for anyone building from
  source, slightly larger binary and dependency set for everyone,
  including people who will never use it.

If the decision is "yes, join `default`," change:

```toml
[features]
default = ["freshrss"]
freshrss = ["reqwest/form"]
```

and re-run the full build/test/clippy matrix (step 5) after making that
change specifically, since a *default* `cargo build`/`cargo test` now
means something different than it did during development.

## 4. Feature-off build against a copy of a real database

Confirms the "off means identical to master" guarantee actually holds
against real, non-synthetic data — not just the in-memory-database unit
tests. **Copy, never the original:**

```bash
cp ~/.local/share/tuxwire/tuxwire.db /tmp/tuxwire-merge-check.db
sqlite3 /tmp/tuxwire-merge-check.db ".schema" > /tmp/schema-before.txt

cargo build   # no --features flag -- the master-equivalent build
env HOME=$(mktemp -d) true   # (placeholder; see below for the real run)
```

Then point a throwaway `HOME` at the *copy*, not `~`, and run the
freshly-built (feature-off) binary against it once, quit immediately,
and diff the schema before/after:

```bash
t=$(mktemp -d)
mkdir -p "$t/.local/share/tuxwire"
cp /tmp/tuxwire-merge-check.db "$t/.local/share/tuxwire/tuxwire.db"
env HOME="$t" ./target/debug/tuxwire   # press q immediately after it opens
sqlite3 "$t/.local/share/tuxwire/tuxwire.db" ".schema" > /tmp/schema-after.txt
diff /tmp/schema-before.txt /tmp/schema-after.txt
```

`diff` should print nothing at all. Any difference here is a real bug in
the "off means identical" guarantee, not something to explain away —
find the root cause before merging, per the project's own root-cause
rule.

Clean up afterward: `rm -rf /tmp/tuxwire-merge-check.db "$t" /tmp/schema-before.txt /tmp/schema-after.txt`.

## 5. Full build/test/clippy matrix, one more time

```bash
cargo build && cargo test
cargo build --features freshrss && cargo test --features freshrss
cargo clippy --all-targets
cargo clippy --all-targets --features freshrss
```

All four should be exactly what they were at the end of Phase 7: the
feature-off build/test/clippy output identical to `master`'s own
baseline, the feature-on output clean except the already-documented,
intentionally-unwired dead-code warnings (`skip_weights`, `theme.text_dim`,
and the `#[allow(dead_code)]`-annotated JSON-mirroring fields in
`freshrss/client.rs`).

## 6. Secret scan, one more time

```bash
git log -p master..feature/freshrss | grep -iE "passwd|password|auth="
```

Should turn up only field names, prose, and the fixture's obvious
`FIXTURE-AUTH-TOKEN-NOT-REAL-...` placeholder — never a real credential.
(Confirmed clean as of Phase 7; re-run since new commits could theoretically
land on this branch between now and the actual merge.)

## 7. The merge

```bash
git checkout master
git pull   # only if step 1 found master had moved and you rebased/merged it in
git merge --no-ff feature/freshrss
```

`--no-ff` (not a fast-forward) is deliberate: it keeps this whole
feature's commit history as one identifiable unit in `master`'s log, one
merge commit that can be pointed to (or reverted) as a whole, rather than
folding every individual commit into `master`'s linear history
indistinguishably.

## 8. How to revert, if it turns out not to be wanted

Merging with `--no-ff` (step 7) is exactly what makes this simple: the
merge itself is one commit.

```bash
git revert -m 1 <merge-commit-sha>
```

`-m 1` tells `git revert` which parent side of the merge to treat as
"mainline" — parent 1 is `master` as it was right before the merge, so
this produces a new commit that undoes everything the merge introduced,
without rewriting any history that's already been pushed/shared (unlike
`git reset --hard`, which this checklist does not recommend for a merge
that's already been pushed).

If the merge hasn't been pushed anywhere yet, a plain
`git reset --hard <master-commit-before-the-merge>` is also fine and
simpler — but confirm nothing else has been built on top of the merge
commit first, and prefer `git revert` once anyone else could have pulled
it.
