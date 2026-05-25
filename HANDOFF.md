# Handoff — cargo-athena

A practical orientation for new agents joining this codebase. **`CLAUDE.md`
is the canonical design reference (the *why*); this file is workflow,
conventions, and gotchas (the *how*).**

---

## What this is

`cargo-athena` compiles annotated Rust into Argo Workflow YAML. Three proc
macros (`#[workflow]`, `#[container]`, `#[fragment]`) statically analyze
your code and emit one Argo `WorkflowTemplate` per template + a single
multi-entrypoint binary that runs every container in-pod. You write
ordinary Rust functions; Argo runs them.

**The big insight.** Every `#[workflow]` / `#[container]` lowers to a
**unit-struct type** implementing the `Template` trait. The type *is* the
cross-crate identity (the "wormhole") — name/input resolution is done by
the compiler, callees are pulled in by direct monomorphic calls in
`Template::collect`, and the reachable closure is force-linked across
crates with no `inventory` / DCE games. You'll see this pattern
everywhere; if you don't internalize it first, the macro output will
look mysterious.

**The hybrid DAG.** `#[workflow]` bodies are *statically analyzed, not
executed* — `analyze_workflow` reads the body's `let x = template(args);`
sequence and builds a DAG. This is the seam for a future functional
promise-graph; for now the body grammar is intentionally strict (anything
outside the supported shape is a spanned `compile_error!`).

---

## Repo layout

```
crates/
├── cargo-athena-api/       ~30 hand-written serde structs (Argo subset).
│                           Deliberately NOT proto/kopium-generated —
│                           don't reattempt, that was rejected.
├── cargo-athena-core/      Runtime + Template trait + Collector + emit.
│                           Single ~1.5kLOC lib.rs.
├── cargo-athena-macros/    The proc macros. Split into 10 modules
│                           (lib.rs is a 66-line entry-point shim).
└── cargo-athena/           Facade lib + the `cargo athena` CLI bin
                            (default `cli` feature).

examples/
├── basic/             Minimal pure example.
├── smoke/             Frozen golden fixture; edits need UPDATE_EXPECT=1.
├── importing/         Cross-MODULE + cross-CRATE importing test.
├── e2e/               THE ONLY crate the GHA kind e2e builds+submits.
└── getting-started/   Runnable example wired into the docs site.

scripts/                Kind cluster: deploy.sh, e2e-test.sh, teardown.sh.
docs/                   mdBook source (the user-facing docs site).
CLAUDE.md               Canonical design reference. Read it.
CONTAINER.md / WORKFLOW.md   Macro docs (symlinked into the macros crate;
                              do NOT delete those symlinks — load-bearing
                              for `cargo publish`).
athena.toml             Repo-root config (S3, target matrix, defaults).
```

### The macros crate is split into 10 modules

```
crates/cargo-athena-macros/src/
├── lib.rs           Entry points (one #[proc_macro_attribute] each).
├── utils.rs         Shared helpers: name munging, BodyScan, DeclRewrite.
├── attrs.rs         ContainerArgs/WorkflowArgs + parsers + token lowerings.
├── ghost.rs         The never-run type-check ghost for #[workflow] bodies.
├── container.rs     #[container]::expand
├── workflow.rs      #[workflow]::expand
├── fragment.rs      #[fragment]::expand
├── analyze.rs       Arg/Node types + analyze_workflow/analyze_stmts.
├── conditional.rs   if/else → when-gated synthetic wrappers.
└── node_tokens.rs   Per-task quote! builder (the arg_value helper lives here).
```

Cross-module items are `pub(crate)`. `analyze.rs` ↔ `conditional.rs` mutually
recurse (`analyze_stmts` ↔ `synth_if`), which Rust handles freely.

---

## Two worlds, one binary

Every workflow crate produces ONE binary. Argo decides which world it's
in via env/argv. Both go through `cargo_athena::entrypoint::<Root>()`:

- **Emit world** (driven by `cargo athena emit`): walks the closure from
  the root `Template`, prints multi-doc Argo YAML to stdout. Reads
  `athena.toml`. Never runs container bodies.
- **Run world** (driven by Argo in-pod via `--cargo-athena-template
  <name>`): looks the template up in the runner table, deserializes
  inputs (env vars + `CARGO_ATHENA_INPUT`), calls the real body,
  serializes the output. Never reads `athena.toml`.

Plus three side modes the CLI uses: `CARGO_ATHENA_LIST` (every template's
metadata as JSON, for `container ls` / `workflow ls`),
`CARGO_ATHENA_DESCRIBE=<name>` (one template's metadata, for
`container emulate` / `describe`), `CARGO_ATHENA_EMIT_JSON` (the
structured WorkflowTemplate set, for `cargo athena submit`'s drift
detection).

**Binary delivery.** `cargo athena publish` cross-compiles a multi-arch
musl tarball and uploads it to S3 (the `athena.toml`-configured artifact
repo). Each container template injects an `sh` bootstrap that `uname`s,
picks the right `app-<triple>`, and `exec`s it with
`--cargo-athena-template <name>`. The init container Argo runs auto-
extracts the tarball — **no `tar` in the user's image** (just `sh` and
`uname`).

---

## Development workflow

### Local verification gates (run before pushing)

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS=-Dwarnings cargo doc --no-deps -p cargo-athena-macros -p cargo-athena
cargo fmt --check
mdbook build docs
```

**All of these gate the PR except rustdoc and mdbook** — those aren't in
CI but should pass locally. CI runs clippy, test, and the full kind+Argo
e2e matrix (v4.0.5, v3.7.14, v3.6.19, all blocking).

### Goldens are byte-identical

If a refactor changes a golden, that's a bug signal, not a sign you
should regenerate. Only run `UPDATE_EXPECT=1 cargo test` when you've
intentionally changed Argo emit semantics. Smoke goldens
(`examples/smoke/tests/golden/`) cover every macro feature.

### Live e2e cluster

A kind cluster + Argo + MinIO setup runs locally for fast iteration.
**Don't tear it down between runs** unless something's broken. Re-deploy
with:

```bash
scripts/deploy.sh                  # ARGO_VERSION env-overridable
scripts/e2e-test.sh                # builds, uploads, submits, waits
scripts/teardown.sh                # only when actually done
```

`ATHENA_E2E_SINGLE=1` for hosts that block kind cross-node pod networking
(NixOS default-drop FORWARD). `ATHENA_SKIP_BUILD=1` + `ATHENA_TARBALL=<path>`
to skip the cross-compile.

### Sanity-check on real Argo

When you genuinely don't know if Argo will accept something, `argo submit
--from workflowtemplate/<name>` on the kind cluster is the empirical
arbiter. The Argo Go source at `/home/maxi/build/argo-workflows` is the
secondary source of truth (CLAUDE.md cites specific file:line pairs).
Don't trust the Argo docs — they're sparse and sometimes wrong.

---

## Conventions

### Commit & PR
- **`main` is branch-protected.** Every change goes via PR; never commit
  directly to main.
- **Terse commits, no AI trailer.** No `Co-Authored-By: Claude`, no
  `Generated with Claude Code` lines.
- **Don't chain independent PRs.** Repo rebase-merges; chained PRs cause
  post-merge conflict toil. Chain only on a true code dependency.
- **One PR can have many commits.** Pushing additional commits to a PR
  branch re-runs CI; that's the normal way to iterate.

### Code style
- Don't write a docstring just because a thing has a name. Write one when
  the WHY is non-obvious. The codebase already has dense rationale
  comments where they matter — match that style, don't dilute it.
- `pub(crate)` for cross-module items inside the macros crate. The only
  truly public surface is the three proc-macro entry points.
- Attribute naming convention: anything that's spec-scoped (only fires
  on the *submitted* workflow, inert as a nested templateRef) carries
  the **`_if_root`** suffix (`on_exit_if_root`, `ttl_if_root`,
  `pod_gc_if_root`, `active_deadline_if_root`, `node_selector_if_root`).
  Template-level attrs that work per-pod even when nested do NOT carry
  the suffix.

### Argo
- Output parameter is always named **`return`**, not `result` (`result`
  is Argo's script-stdout alias and only works on container/script
  templates — using it on dag/steps fails to resolve across templateRef).
- Cross-template refs go through **`outputs.parameters.return`**, never
  bare `outputs.result`. The structured templateRef form survives the
  one-WT-per-template wormhole; name-strings don't.
- All parameter values are JSON-encoded (**Regime B**) — strings as
  `"v"`, numbers bare, bools bare. The run-side `from_str`-else-String
  handles both.

---

## Project memory

Persistent memory lives at
`/home/maxi/.claude/projects/-home-maxi-build-cargo-argo/memory/`.

Useful files to read at session start (especially before doing
non-trivial work):

- `MEMORY.md` — the index. Always loaded automatically into context.
- `architecture.md` — extended design rationale (mirrors CLAUDE.md).
- `roadmap.md` — local backlog (the user wants this kept OUT of the repo).
- `commit-style.md`, `pr-chaining.md`, `main-locked.md` — process rules.
- `nix-toolchain.md`, `nix-shell-tools.md` — Nix dev shell + missing-tool
  recovery (`nix-shell -p <tool>` rather than skipping a check).
- `no-gold-plating.md` — propose simple-subset before bespoke half-measure.

**Memory is a point-in-time snapshot.** Verify against the current code
before asserting as fact — `git log`, `git blame`, or just `rg` the
codebase. If a memory's claim contradicts what you see, trust the code
and update the memory.

---

## Common foot-guns

### Don't reattempt these (they were tried and rejected)

- **Don't replace `cargo-athena-api` with proto/kopium-generated types.**
  Tried, abandoned: ~90k LOC of opaque generated code for marginal
  conformance gain. The hand-owned ~30-struct serde subset is the right
  cost/benefit tradeoff; conformance is empirically guarded by the kind
  e2e against real Argo.
- **Don't add a `build.rs` for resource parsing.** The `host!`/artifact
  collection is a *static AST union* in the attribute macros (sees every
  branch). That's the only expressible semantics (Argo's pod spec is
  fixed before the pod runs); a runtime trace would miss branches.
- **Don't reach for `inventory` to track templates.** Templates use the
  type-as-wormhole + monomorphic `collect()` recursion. `inventory` is
  used ONLY for `#[fragment]` (those are genuinely called, no DCE risk).
- **Don't try to lower `match`, `for`, `while`, `loop` in `#[workflow]`.**
  They're hard `compile_error!`s today by design — they'll be lowered
  differently later (when the promise-graph lands). Until then, errors
  beat mistranslations.

### Subtle gotchas

- **The Norway problem.** YAML 1.1 (Argo's Go parser among them) reads
  bare `y`/`yes`/`n`/`no`/`on`/`off`/`true`/`false`/`null`/`~` as
  bool/null. `check_yaml_safe_names` rejects argument names matching
  these. If you add a new place that emits a user-controlled name into
  YAML, gate it.
- **`nodeSelector` is boundary-scoped, not cascading.** Argo's pod-build
  lookup is `tmpl → immediate-boundary → wfSpec`. A selector on a parent
  dag does NOT cascade through nested sub-workflows. Use
  `node_selector_if_root` (lands on `wfSpec`) for "every pod in the
  run" semantics.
- **`fan_out` aggregates are type-heterogeneous.** Argo's
  `aggregatedJSONValueList` keeps objects/arrays as native JSON but
  double-encodes scalars (string/number/bool). The `Arg::FanAgg`
  lowering emits a kind-aware re-norm that handles both. Don't "simplify"
  it — proven empirically on v4.0.5.
- **`outputs.result` vs `outputs.parameters.return`.** The former is
  Argo's script-stdout alias and only works on container/script
  templates. The latter is the explicit parameter we declare. Cross-
  templateRef refs MUST use `outputs.parameters.return`.
- **`serde_yaml` is archived/EOL.** We use the maintained fork
  `serde_norway` (YAML 1.1-aware emitter, byte-identical output). Don't
  swap back.

### Don't lose work

- **Never `git checkout -- <file>` to clean a probe.** It nukes
  uncommitted work. Use a fresh branch or `git stash` instead.
- **Don't push while a cache-warming CI run is in flight.** Check
  `gh run list` first; key-changing pushes during a producer run cause
  cache-action timing issues.
- **Don't skip a check because a tool is missing.** `nix-shell -p <tool>`
  is the canonical way to fetch it (mdbook, gh, kubectl, kind,
  argo-workflows, minio-client, jq are all available).
- **Don't delete the `CONTAINER.md` / `WORKFLOW.md` symlinks under
  `crates/cargo-athena-macros/`.** They're load-bearing for `cargo
  publish` (the `include_str!` reads through them; `cargo package`
  dereferences symlinks). Repointing to `../../../*.md` puts the file
  outside the package and breaks publish.

### Don't enable destructive operations without authorization

- No `git push --force`, no `git reset --hard`, no `rm -rf` against user
  files. Always confirm before anything that touches shared state
  (pushes, PR comments, deploys to cluster).

---

## Supported Argo versions

CI matrix, all blocking, no continue-on-error:
- **v4.0.5** (maintained latest)
- **v3.7.14** (maintained n-1)
- **v3.6.19** (minimum supported, EOL but hard-gated)

**Argo ≤ 3.5 is unsupported and intentionally excluded** — its submit-time
validator can't resolve `{{tasks.X.outputs.*}}` across a `templateRef`
boundary (intrinsic to the one-WT-per-template wormhole model). Our
emitted YAML is correct and passes 3.6/3.7/4.0 — don't "fix" by
inlining.

---

## Workflow for a typical change

1. **Read.** Check `MEMORY.md` and `CLAUDE.md` for relevant context.
   `git log --oneline -20` to see recent work. `git status` and
   `git diff main` to see in-flight changes (you may not be the only
   agent working on this repo).
2. **Plan.** Non-trivial changes deserve an `ExitPlanMode` plan or at
   least an explicit list of steps before editing. For refactors, map
   the change as a sequence of commits before starting.
3. **Branch.** `git checkout -b <type>/<short-name>` off `main`.
4. **Iterate.** Make a change, run the local gates, commit, repeat.
   Each commit should leave the tree green.
5. **Push & open PR.** `git push -u origin <branch>` then
   `gh pr create --title ... --body ...`. CI runs on the PR.
6. **Verify CI.** `gh pr checks` to watch. If CI fails, push fixes;
   each push re-runs CI.

---

## Where to dig deeper

- **`CLAUDE.md`** — the canonical design reference. Every architectural
  decision, every Argo gotcha, every empirical probe documented.
- **`README.md`** — user-facing (lean by design).
- **`docs/src/`** — the mdBook docs site (`mdbook serve docs` to
  preview).
- **`crates/cargo-athena-macros/CONTAINER.md` / `WORKFLOW.md`** — full
  macro semantics (symlinks to the repo-root versions).
- **`examples/smoke/src/lib.rs`** — the frozen-golden feature catalog.
  Want to know how a feature is used? Look here first.
- **`examples/e2e/src/lib.rs`** — the live-validated pipeline (kind
  e2e submits this).
- **`/home/maxi/build/argo-workflows`** — the Argo Go source, cloned
  locally for empirical proof of Argo behavior.

Welcome aboard — read `CLAUDE.md`, skim the smoke fixture, and you'll
have the mental model in an hour.
