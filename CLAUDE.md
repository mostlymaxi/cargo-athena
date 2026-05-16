# cargo-athena — design reference

Condensed architecture/rationale for maintainers and AI sessions. The
README is intentionally lean (user-facing); the *why* lives here.

## Core model

- Compile regular Rust → Argo Workflow YAML. `#[workflow]`/`#[container]`/
  `#[fragment]` proc macros.
- **Type-as-wormhole**: each `#[workflow]`/`#[container]` lowers to a
  **unit-struct type** implementing the `Template` trait
  (`ARGO_NAME`/`INPUTS`/`KIND`/`build`/`run`/`collect`). Name/input
  resolution is done by the compiler (collision-proof); merely referencing
  the type force-links its defining crate. `Template::collect` recurses
  callees by direct **monomorphic** calls, so the reachable closure is
  force-linked across crates with **no `inventory`/DCE** for templates and
  nothing uncalled is emitted. Names are `<crate>-<fn>` (override
  `#[workflow(name=…)]`).
- **Hybrid DAG**: `#[workflow]` bodies are *statically analyzed, not
  executed* — the seam for a future functional promise-graph.
- **`#[workflow]` body contract (strict, fail-loud).** Only
  `let x = template(args);` and `template(args);` are lowered. Every other
  statement (if/match, for/while/loop, macros, method calls, `let`
  with non-ident/tuple patterns, `let…else`) is a hard `compile_error!`
  with a spanned message — never a silently dropped task. Args must be a
  literal, a `#[workflow]` input, a prior `let` binding, or
  `.to_string()/.to_owned()/.into()` on one of those (no lossy
  stringify). `#[fragment]`s/regular fns aren't `Template`s, so calling
  one from a `#[workflow]` fails via the type system (`<x as Template>`
  → "expected type, found function"). Loops/branches will be lowered
  differently later — until then they must error, not mislower.
- **Per-task builder chain.** A task call may be suffixed with, in any
  order, at most one each of `.continue_on(failed|error|failed, error)`
  (→ `DAGTask.continueOn`), `.on_exit(t)` (→ the special `exit` hook,
  `templateRef=t`, no expression), and any number of `.hooks("argo-expr"
  = t, …)` (→ `LifecycleHook{expression, templateRef=t}`, auto-keyed
  `hook1`,`hook2`,… in source order). `peel_builders` strips them off
  the call before `call_parts`; valid on stmt / `let` / tail / `return`
  calls (not on a returned binding). Malformed *known* builder =
  targeted `compile_error!`; an unknown trailing `.foo()` falls through
  to the usual not-a-template-call error. Hook templates are paths,
  force-linked + emitted via the wormhole exactly like callees (added to
  the `collect()` closure). v1 hooks pass no arguments. Argo-validated:
  smoke `pipeline_hooks` SUBMIT-OK on real v4.0.5.
- **`nodeSelector` DOES cascade (empirical, real Argo v4.0.5,
  2026-05-16).** A parent DAG/steps template's `nodeSelector` is merged
  by the Argo controller onto the pods of templates it calls via
  `templateRef` (probe: a leaf WT with no selector got its ancestor
  DAG's `{disktype:doesnotexist}` and went Pending). So the earlier
  "DAG creates no pod ⇒ nodeSelector is a no-op / no inheritance"
  reasoning is WRONG. athena's per-`#[container(node_selector=…)]`
  (leaf-level) is correct; a future `#[workflow(node_selector=…)]` would
  legitimately cascade to every task's pod.
- **`#[workflow]` return values (WORK e2e — proven on real Argo).** Every
  template's serialized fn return value is captured as an output
  **parameter named `return`** (`outputs.parameters.return`): container =
  `valueFrom.path: /athena/result` (the file the bin writes); workflow
  with a return type bubbles its **terminal** task's `return` up via
  `valueFrom.parameter: "{{tasks|steps.<task>.outputs.parameters.return}}"`
  (terminal = tail template call, or a `return`/tail binding ident → its
  producing task). Return-type-but-unresolvable is a `compile_error!`.
  Sibling refs are `{{tasks|steps.<dep>.outputs.parameters.return}}`.
  **CRITICAL — the `return` name + `.parameters.` path are load-bearing:**
  Argo's bare `{{….outputs.result}}` is the *script-stdout alias*, only
  defined for container/script templates, NEVER for dag/steps. We declare
  an explicit param, so the only correct ref is
  `outputs.parameters.<name>`. Using bare `outputs.result` (the old bug)
  *coincidentally* resolved for containers but **failed `failed to
  resolve` for any DAG/steps output across templateRef** — which made
  workflow→X data-deps look impossible. Proven by a 4-way kind isolation
  (real Argo v4.0.5, 2026-05-16): container/DAG × `outputs.result`/
  `outputs.parameters.ret` — only DAG+`outputs.result` fails; the
  `parameters` path resolves for BOTH. Named `return` (not `result`) so
  it can never be confused with the stdout alias again. `examples/e2e`
  exercises a sub-`#[workflow]` return consumed downstream; both it and
  smoke `pipeline_returns` SUBMIT-OK on the live cluster.
- **Resource collection = static AST union, not a trace.** The attribute
  macro sees every branch's tokens, so `host!`/artifact-key decls are
  collected across all `if`/`match`/loop arms. This is *correct* (Argo's
  pod spec is fixed before the pod runs, so the union is the only
  expressible semantics), not just tractable. Keys are literal/const only.
  Cross-function cases use `#[fragment]` + an **`inventory`-resolved**
  transitive closure at emit time (the only place `inventory` is used —
  fragments are genuinely *called*, so no DCE concern). No `build.rs`.
- **Each template = its own `WorkflowTemplate`**, emitted as a multi-doc
  stream; calls are `templateRef`. **Single multi-entrypoint binary**,
  dispatched by `--cargo-athena-template <crate>-<fn>`.

## Binary delivery / runtime

- `cargo athena build` cross-compiles one static-musl `.tar.gz` (target
  matrix from `athena.toml`) into an S3-compatible `ArtifactRepository`.
- `cargo athena emit` injects, into every container template: the binary
  as an input artifact (`s3{}` from `athena.toml`, `archive: none`) + an
  `sh` bootstrap (`uname` → pick `app-<triple>` → `exec … --cargo-athena-
  template <name>`; deserialize inputs → run real body → serialize
  outputs). One template runs on any node arch (resolved at pod start).
- Pod-scoped `emptyDir` mounted at `/athena` (`athena-work`): all athena
  paths live under it (tarball, `/athena/bin`, artifact ports,
  `/athena/result`) so it works on distroless/read-only-rootfs images with
  no `/tmp` dependency, and is shared with Argo init/wait containers.
  `host!` hostPaths are appended after it. `athena.toml` is never read
  in-pod.
- `#[container(image=…)]` is arbitrary/per-container; just needs POSIX
  `sh`/`tar`/`uname`.

## Artifact ports

`load_artifact!`/`save_artifact!("key", …)` (+ `_str` variants): exact S3
object key in the `athena.toml` repo. Producer/consumer decoupled through
the bucket — no DAG dep, no `{{tasks.…}}`, no ordering. Emits an Argo
artifact (`s3{}` + `archive: none`). Same machinery as `host!`:
literal-key only, static-AST-union collected, gated (public form is a
`compile_error!` outside `#[container]`/`#[fragment]`; a `#[workflow]`
using one is a hard error), propagated through the `#[fragment]` closure.

## Hand-owned API (no protobuf/generated types)

`cargo-athena-api` = ~30 plain `#[derive(Serialize,Deserialize)]` structs,
a narrow stable slice of Argo. Generating from proto/CRD (kopium/proto)
cost ~90k LOC of path-exploded types + opaque holes for fidelity we don't
need — **rejected, do not reattempt**. Conformance is guarded
**empirically** by the kind e2e against real Argo; adding an Argo field =
adding a field.

## Supported Argo versions

- Maintained by Argo = 2 most recent minors (release policy: no LTS/stable
  channel; "stable" = latest patch). Today **v4.0** + **v3.7**.
- CI matrix (all **blocking**, no continue-on-error): **v4.0.5**
  (maintained latest), **v3.7.14** (maintained n-1), **v3.6.19**
  (**minimum supported**, EOL but hard-gated).
- **Argo ≤ 3.5 is unsupported and intentionally excluded.** Its
  submit-time validator cannot resolve `{{tasks.X.outputs.*}}` across a
  `templateRef` boundary → instant `Pending→Failed: invalid spec: … failed
  to resolve {{tasks.a.outputs.result}}`. Intrinsic to the one-WT-per-
  `#[container]`/`#[workflow]` wormhole (every multi-step workflow wires
  tasks via `templateRef`). Our emitted YAML is correct (producing WT
  declares `outputs.parameters`) and passes 3.6/3.7/4.0 — **NOT an emit
  bug; do not "fix" by inlining.** Fixed in Argo 3.6.

## e2e / ops

- `scripts/{deploy,e2e-test,teardown}.sh`. 3-node kind (control = Argo
  controller + MinIO tolerated/pinned; 2 workers via taint) + real Argo +
  MinIO. `ARGO_VERSION` env-overridable; `e2e-test.sh` honors
  `ATHENA_SKIP_BUILD=1` + `ATHENA_TARBALL`. Big-CRD installs need
  `kubectl apply --server-side --force-conflicts`.
- `deploy.sh` binds Argo executor RBAC (`workflowtaskresults`
  create/patch) to the workflow SA — `namespace-install.yaml` omits it for
  `default`, else every step 403s.
- Workflow pods run as `[defaults].service_account` from `athena.toml`
  (default `default`), per-template override
  `#[container(service_account=…)]`.
- `ATHENA_E2E_SINGLE=1` → 1-node cluster, for hosts that block kind
  cross-node pod networking (e.g. NixOS default-drop `FORWARD`).
- `e2e-test.sh` has an `EXIT`-trap `dump_diagnostics` (argo get @latest,
  wf `.status.message`, filtered controller logs, failed-pod describe) —
  without it a failure is a bare "exit code 1"; that trap found the ≤3.5
  cause.

## CI / badges

- `.github/workflows/e2e.yml`: on push to `main`, build tarball ONCE
  (cached) → parallel Argo matrix (`fail-fast:false`). `publish.yml` is a
  deliberate stub (crates.io public-only + path-dep versioning blockers).
- Per-version badges: GitHub has no per-matrix-job badge, so each job
  publishes pass/fail via `schneegans/dynamic-badges-action` to gist
  `6c34ed5be0444407c50ccf4597acba1f` (owner `mostlymaxi`); README uses
  shields.io endpoint badges. Secrets `GIST_TOKEN` (PAT, `gist` scope) +
  `BADGE_GIST_ID`. Badge step is `if:`-gated on **both secrets non-empty**
  so it's fully **skipped** (not just continue-on-error) until set — else
  it 404s PATCHing an empty gist id.

## Conventions

- **Commit identity MUST be `Maxi Saparov <max.saparov@gmail.com>`** (not
  `maxi.saparov@`).
- **Example/test layout** (post-cleanup):
  - `examples/basic` — minimal pure example (no tests).
  - `examples/smoke` — broad "all features" fixture; `src/lib.rs` is a
    frozen golden fixture (edits require intentional `UPDATE_EXPECT=1`).
    Bins `smoke`/`smoke-returns`; `tests/e2e.rs` golden+run.
  - `examples/importing` — cross-MODULE + cross-CRATE importing (depends
    on `../smoke`, imports its `pipeline`); empirical proof the
    type-wormhole force-links across both boundaries. `tests/e2e.rs`.
  - `examples/e2e` — the **ONLY** crate the GHA kind e2e builds+submits
    (`cargo-athena-example-e2e`, bin `e2e`; `scripts/e2e-test.sh`,
    `.github/workflows/e2e.yml`). No in-process tests.
  - `crates/cargo-athena/tests/` — `smoke.rs` (in-process
    Collector/Template module assertions) + `ui.rs`/`ui/*` (trybuild
    compile-fail contracts). trybuild builds `ui/*` as crates depending
    on `cargo-athena`.
- Renaming an example crate changes the `<crate>-<fn>` Argo names ⇒ regen
  all goldens with `UPDATE_EXPECT=1`.
- Never `git checkout -- <file>` to clean a probe (nukes uncommitted work).
