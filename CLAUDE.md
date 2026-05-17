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
- **Ghost type-check (data-flow IS compiler-enforced).** Every
  `#[container]`/`#[workflow]` also emits `impl Ident { #[doc(hidden)]
  pub fn __athena_sig(<real args>) -> <real ret> { unimplemented!() } }`
  (never run; `pub`+hidden ⇒ resolves cross-crate/module like the
  wormhole type). `#[workflow]` additionally emits a hidden never-called
  `fn __athena_tc_<ident>(<wf inputs>) -> <wf ret> { <body> }` where the
  body is the *faithful* workflow body with builder chains stripped and
  every `C(args)` → `C::__athena_sig(args)`. So rustc fully type-checks
  arg/arity/field/return flow on the analyzed body (a bad `a.field`,
  wrong type, or consuming a non-returning `#[workflow]` is now a
  **compile error** — this caught two latent fixture bugs). The body is
  *faithful* (real move semantics): fan-out needs an explicit `.clone()`
  — which is *correct*, Argo copies the output param into each consumer.
  Borrows can't cross the wormhole (serde `DeserializeOwned`/`Serialize`
  on container I/O already forbids `&T`); `&a`/`&mut a` ⇒ ghost type
  mismatch — nothing special needed. In args, `.clone()`/`.to_owned()`
  are type-preserving (allowed on binding/input; emit = receiver);
  `.to_string()`/`.into()` are **literal-only** (on a binding they'd
  change the Rust type while the emit still passes the raw serialized
  param → silent ghost↔Argo mismatch). `__athena_sig`/the ghost are also
  what make calling a `#[fragment]`/regular fn from `#[workflow]` fail.
- **`a.field` (struct-field access).** A named-field chain `a.b.c` on a
  binding/input lowers to Argo expr-templating
  `{{=toJSON(fromJSON(<src>)['b']['c'])}}` where `<src>` is bracket-form
  `tasks['dep'].outputs.parameters['return']` / `steps[...]` /
  `inputs.parameters['name']` (bracket = hyphen/keyword-safe; the plain
  whole-binding ref stays non-expr `{{tasks.dep…}}`). Binding source ⇒
  DAG dep (like `Ref`); input ⇒ none. **`toJSON(fromJSON(..))` is the
  *universal-safe* form** (NOT bare `jsonpath`): athena's run-side is
  `serde_json::from_str` else `Value::String` (core ~820), so it
  reconstructs every field type — JSON-quoted strings, numbers, nested
  structs/arrays — faithfully; bare `jsonpath` is lossy for string
  fields. The ghost has already type-checked the field path exists & the
  type matches the consumer, so the lowering is purely mechanical &
  safe-by-construction. v1 = **named fields only**; tuple `a.0` / index
  `a[i]` are targeted `compile_error!`s (deferred). Empirically:
  jsonpath/fromJSON round-trip proven on real Argo v4.0.5, and athena's
  emitted `pipeline_fields` SUBMIT-OK.
- **`.fan_out` (list → Argo `withParam`).** `let b =
  a.fan_out(|x| C(x, lit));` lowers `C` to a task with `withParam:
  {{tasks.<a>.outputs.parameters.return}}` (or `{{inputs.parameters.<a>}}`
  if `a` is a workflow input), `dependencies:[a]`, the closure param → the
  `{{item}}` arg (other args resolve normally; `{{item.field}}` for a
  field-chain on the param). `AthenaList<T>` is a **ghost-only** trait
  (`cargo-athena-core`, blanket `impl` for `Vec<T>`/`[T;N]`, body
  `unimplemented!()`, re-exported via facade) injected into the ghost as
  `use ::cargo_athena::AthenaList;` so `a.fan_out(|x| C::__athena_sig(x,…))`
  type-checks element/closure/result. The aggregated output is consumed as
  a **normal** `{{tasks.<b>.outputs.parameters.return}}` ref → `Vec<U>`:
  **empirically proven** on real Argo v4.0.5 that the `withParam`
  aggregate of valid-JSON returns is already a clean JSON array (no
  base64, no `toJSON`/`fromJSON` renormalize — the run-side
  `from_str`-else-`String` contract makes it universally
  `from_str::<Vec<U>>`-able), and athena's emitted `pipeline_fanout`
  SUBMIT-OK. The closure body MUST be a template call (else
  `compile_error!`); the `fan_out` source must be a prior binding or
  workflow input.
- **`#[workflow]` body contract (strict, fail-loud).** Only
  `let x = template(args);`, `template(args);`, and `if`/`else`/`else if`
  (see next bullet) are lowered. Every other statement (`match`,
  for/while/loop, macros, method calls, `let` with non-ident/tuple
  patterns, `let…else`) is a hard `compile_error!` with a spanned message
  — never a silently dropped task. Args must be a literal, a
  `#[workflow]` input, a prior `let` binding, or
  `.to_string()/.to_owned()/.into()` on one of those (no lossy
  stringify). `#[fragment]`s/regular fns aren't `Template`s, so calling
  one from a `#[workflow]` fails via the type system (`<x as Template>`
  → "expected type, found function"). Remaining loops/`match` will be
  lowered differently later — until then they must error, not mislower.
- **`if`/`else`/`else if` → synthesized `when`-gated wrapper workflows.**
  A whole `if` chain lowers to ONE synthetic `#[workflow]`
  `<crate>-<fn>-if<k>` whose DAG has one `when`-gated task per arm
  (callee = a per-arm synthetic sub-`#[workflow]` `…-if<k>-arm<j>`),
  gates mutually exclusive **by construction**
  (`(!c₀ && … && !cᵢ₋₁ && cᵢ)`, else = all-negated). Captured free vars
  (idents bound outside the chain ∩ parent scope; whole binding/input
  only) become the wrapper+arm INPUTS (validated by the YAML guard);
  parent passes the matching refs and consumes the wrapper exactly like
  a returning sub-workflow. **Value-`if`** (`let x = if c {…} else {…};`
  / tail `if`): the wrapper declares `outputs.parameters.return` via
  **`valueFrom.expression`** = a right-folded status-ternary
  (`tasks['arm0'].status == 'Succeeded' ? tasks['arm0']…return : …`) —
  Argo short-circuits over the Skipped arm (**proven kind v4.0.5**); Rust
  itself forces an `else` + same arm type (the ghost inherits this free,
  since `GhostRewrite` is a generic `VisitMut` that already keeps the
  `if` inline with calls→`__athena_sig` — **zero ghost changes**).
  Condition → closed `WhenExpr` (`== != < <= > >=`, `&& || !`; operands:
  binding/input/`a.field`/literal — kind-preserving so a string compares
  as JSON-quoted `"v"`, numbers/bools bare, `.field` via the same
  `{{=toJSON(fromJSON(..))}}`; all proven on v4.0.5); single
  parenthesized `render` is the only `when` producer (valid-by-
  construction — no expr engine). Out-of-grammar conditions / value-`if`
  without `else` = targeted `compile_error!`. Synthetic structs have no
  ghost/sig-shim/`run` (never called from Rust, never run in-pod);
  force-linked via the parent's `collect`. Smoke `pipeline_if` SUBMIT-OK
  on real Argo v4.0.5 (13-template synthetic chain).
- **Nested calls.** A template *call* in argument position
  (`foo(bar())`) lowers `bar` to its own task and wires `foo`'s arg to
  `{{tasks.bar.outputs.parameters.return}}` + a dep — recursive
  (`foo(bar(baz()))`), reusing the existing `Arg::Ref` path. A call in an
  `if` *condition* (`if foo() > 3`) is **hoisted to a parent task**
  (`__athena_cond_N`) — Rust evaluates the condition unconditionally, so
  the call runs in the parent DAG and is captured into the wrapper like
  any binding; identical call exprs within one `if` hoist once
  (`hoist_cond`/`hoist_operand` over the cond grammar). Not applied
  inside a `fan_out` closure (item scope) in v1. Ghost type-checks the
  nesting free (`Foo::__athena_sig(Bar::__athena_sig())`). Smoke
  `pipeline_nested` SUBMIT-OK on real Argo v4.0.5.
- **Regime B — all param values consistently JSON-encoded.**
  `expr_to_arg`'s `Lit` arm emits `serde_json::to_string(&lit)` (str →
  `"v"`, int/float → `7`/`1.5`, bool → `true`); task-output refs were
  already JSON (`Value::to_string`), so *every* Argo param value is now
  uniform JSON. The run-side is unchanged (`from_str` else `String` →
  `from_value::<T>`), so bodies are unaffected. Wins: (1) fixes a latent
  bug — a `String` literal `"7"` used to emit raw `7` and deserialize
  back as a *number* (`from_value::<String>` fails); (2) **retired** the
  old `yaml_value_unsafe` literal ban + its trybuild — a JSON `"no"` is
  the scalar `"no"` (quoted), not the YAML-1.1 bare bool, so the hazard
  is gone by construction; (3) lets attribute injection always use
  `{{=fromJSON(...)}}`. `serde_json` is a `cargo-athena-macros` dep
  (encode at macro time). Ripple: every literal-arg golden regenerated.
- **`#[container]` attribute param injection (concat).**
  `image`/`service_account` and `node_selector` *values* are
  `Option<syn::Expr>`/`BTreeMap<String, syn::Expr>` (deluxe has a
  built-in `ParseMetaItem for syn::Expr`). `inject_lower` (before
  `container`): a lone string literal is **verbatim** (a hand-written
  `{{…}}` passes straight through — power-user escape hatch); a
  `+`-concat of string literals and `arg` / `arg.named.field` operands
  lowers each operand to `{{=fromJSON(inputs.parameters['arg'](['f'])*)}}`
  (the *raw* value — **no** outer `toJSON`, since this injects into an
  Argo-native string field, not athena's run-side). Empirically proven
  on real Argo v4.0.5: `{{=fromJSON}}` is honored in `image`,
  `serviceAccountName`, AND `nodeSelector` (value *and* key) and
  unwraps a JSON string to its raw scalar; athena's emitted `pipeline`
  (combine) + `pipeline_inject` SUBMIT-OK. **node_selector keys are
  literal by design** (the `String` map-key type enforces it) — not an
  Argo limitation (Argo *does* substitute keys), a deliberate choice.
  `name` (static template id) and `on_exit_if_root` (a path) are
  non-string targets, excluded by nature. **Type guard:** a hidden
  never-run
  `__athena_inject_check_<fn>(<real args>)` asserts each injected
  operand is `::cargo_athena::Injectable` — a `#[doc(hidden)]` marker
  impl'd ONLY for `String`/`str`/the numeric primitives (NOT `Display`:
  a type's `Display` ≠ its `serde_json`→`fromJSON` raw form). Non-arg
  ident / non-`Injectable` / tuple-index field / non-concat expr =
  targeted `compile_error!` (trybuild `ctr_inject_*`).
- **Per-task builder chain.** A task call may be suffixed, in any order,
  with:
  - `.continue_on(failed|error|failed, error)` (≤1) → `DAGTask.continueOn`;
  - `.on_exit(t)` (≤1) → the special `exit` hook (no expression);
  - `.on_success(t)` / `.on_failure(t)` / `.on_error(t)` (repeatable) →
    athena-generated `LifecycleHook.expression` = `<scope>['<task>'].status
    == "Succeeded|Failed|Error"` where `<scope>` is `tasks` (dag) or
    `steps`. **Bracket form is load-bearing** — kebab task names have
    hyphens, so `tasks.x` is invalid; `tasks['x']` works (kind-proven
    v4.0.5, hyphenated name fires);
  - `.hook_if("raw-argo-expr" = t, …)` (repeatable) → verbatim Argo
    expression escape hatch.
  Any hook target may be `t` or `t(args)` (args resolved like task args:
  literal / `#[workflow]` input / prior binding; param names from the
  hook tmpl's `INPUTS` → `LifecycleHook.arguments`). Non-exit hooks are
  auto-keyed `hook1`,`hook2`,… in source order. `peel_builders` strips
  the chain before `call_parts`; valid on stmt/`let`/tail/`return`
  (not a returned binding); malformed *known* builder = targeted
  `compile_error!`, unknown `.foo()` falls through to not-a-template-call.
  Hook templates are force-linked/emitted via the wormhole (in
  `collect()`). Argo-validated: smoke `pipeline_hooks`/`pipeline_onexit`
  SUBMIT-OK + `.on_failure` fires on real v4.0.5.
- **Whole-workflow exit handler: `#[workflow(on_exit_if_root=t)]` /
  `#[container(on_exit_if_root=t)]` → `Template::ON_EXIT` (default
  `None`).** Renamed from `on_exit` 2026-05-17 (user) to (a) say the
  semantic — it only fires when this workflow is the submitted run's
  root — and (b) not be visually confused with the per-task
  `.on_exit(t)` builder (a *different*, always-fires task hook, peeled
  by `peel_builders`; that one keeps the name `on_exit`). **emit puts
  it on EACH template's own `spec.hooks.exit.templateRef`** (every
  template with `on_exit_if_root`, not just root): `Collector::add::<T>()`
  records `ARGO_NAME→ON_EXIT` in `exits`; `emit()` injects per-WT.
  `templateRef` form — NOT the legacy `spec.onExit` string (EMPIRICAL
  kind v4.0.5: `spec.onExit:<name>` REJECTED — resolves only a *local*
  `templates[]` name; `spec.hooks.exit.templateRef` SUBMIT-OK + handler
  pod runs `EXITRAN`; structured templateRef survives the wormhole,
  name-strings don't). Argo runs exit hooks **workflow-scoped**: only
  the *submitted* workflow's hook fires (proven on v4.0.5 via BOTH
  `argo submit --from workflowtemplate/<X>` AND a `workflowTemplateRef`
  Workflow). A templateRef'd sub-workflow's own hook stays inert when
  nested (probed: root OUTER_EXIT + SUBRAN, no INNER_EXIT) — but submit
  that sub-WT directly and its own hook fires (the point of putting it
  on every WT). The `--with-workflow` runnable Workflow also keeps its
  own explicit `hooks` for the root (redundant, zero-regression).
  Handler templates force-linked via `collect()`.
- **`nodeSelector` DOES cascade (empirical, real Argo v4.0.5,
  2026-05-16).** A parent DAG/steps template's `nodeSelector` is merged
  by the Argo controller onto the pods of templates it calls via
  `templateRef` (probe: a leaf WT with no selector got its ancestor
  DAG's `{disktype:doesnotexist}` and went Pending). So the earlier
  "DAG creates no pod ⇒ nodeSelector is a no-op / no inheritance"
  reasoning is WRONG. athena's per-`#[container(node_selector=…)]`
  (leaf-level) is correct.
- **`#[workflow(node_selector = { "k" = "v" })]` — IMPLEMENTED, but
  LITERALS-ONLY (keys *and* values; `BTreeMap<String,String>` in
  `WorkflowArgs`, NO `inject_lower`/`Injectable`/`syn::Expr`). Set on
  the dag/steps `api::Template` (both build_body branches); cascades to
  every task pod (proven 2026-05-17: emitted `pipeline_ns` golden
  submitted on live v4.0.5 → leaf `fetch` pod got both the static
  `kubernetes.io/arch=amd64` AND a `-p region=` resolved
  `{{workflow.parameters.region}}` cascaded onto `.spec.nodeSelector`).**
  Why no injection (unlike `#[container]`): a `#[workflow]` is a DAG not
  a pod. Two probes (2026-05-16/17) proved (a) a template-scoped
  `{{=fromJSON(inputs.parameters.X)}}` on a dag template is cascaded
  **raw** to the child pod → k8s rejects the literal label; (b)
  `serviceAccountName` does NOT cascade from a dag template (stays
  `default`) — so no `#[workflow]` `service_account`/`image` either;
  (c) `{{workflow.parameters.X}}` is the ONLY interpolation that
  survives the cascade and is **always root-scoped**: a sub-`templateRef`
  workflow whose dag `nodeSelector` used `{{workflow.parameters.subp}}`
  errored because the *submitted root* (not the sub's `subp` input) is
  what's resolved. So dynamic values are a documented eyes-open escape
  hatch (raw `{{workflow.parameters.foo}}` literal; user owns
  root-scoping). Fixture: smoke `pipeline_ns` + bin `smoke-ns` + golden
  `pipeline_ns.yaml` + `emit_pipeline_ns` test. Synthesized `if`
  wrappers (`emit_synth`) intentionally do NOT re-stamp it (scope:
  literals-only, single user template; transitive cascade through synth
  dags is an untested Argo edge, not claimed).
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
- **emit default = templates only (2026-05-17, user-directed).**
  `Collector::emit::<E>(with_workflow: bool)` emits just the
  deterministic, stable-named `WorkflowTemplate`s by default
  (`kubectl apply`-able, GitOps-clean, idempotent); the recommended run
  path is `argo submit --from workflowtemplate/<root>`. `with_workflow`
  appends the convenience runnable `Workflow` (`generateName`,
  `workflowTemplateRef`→root) for `kubectl create -f -` demos.
  `entrypoint()` reads `CARGO_ATHENA_WITH_WORKFLOW=1` (set by
  `cargo athena emit --with-workflow` on the child); `scripts/e2e-test.sh`
  passes `--with-workflow` (it splits + `argo submit`s that doc). All
  emit goldens regenerated (no Workflow doc); smoke
  `pipeline_with_workflow.yaml` + the in-process `smoke.rs` cover the
  `with_workflow=true` shape. **`on_exit_if_root` under templates-only:**
  see the dedicated exit-handler bullet above — every template with
  `on_exit_if_root` carries `spec.hooks.exit` on its OWN WorkflowTemplate
  (`pipeline_onexit.yaml` shows it); Argo fires only the submitted
  workflow's hook (workflow-scoped, proven via `--from` and
  `workflowTemplateRef`).

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
  (cached) → parallel Argo matrix (`fail-fast:false`).
- `.github/workflows/publish.yml` (LIVE since repo went public): on a
  `v*` tag (or manual dispatch) `cargo publish` in dependency order
  **api → macros → core → cargo-athena**. (`cargo-athena-cli` was
  merged into `cargo-athena` 2026-05-17: it is now lib + the
  `cargo athena` CLI bin behind a default `cli` feature, so
  `cargo install cargo-athena` ships the subcommand; library-only
  consumers use `default-features = false`. The abandoned
  `cargo-athena-cli` 0.1.x stays on crates.io but gets no new
  releases.) The old
  "path-dep versioning" blocker was already moot — `version.workspace =
  true` + `[workspace.dependencies]` give every internal dep
  `{ path, version = "0.1.0" }`, which `cargo publish` accepts. Needs
  repo secret `CARGO_REGISTRY_TOKEN`; a `v<tag>` must equal
  `[workspace.package].version`. **Load-bearing: `WORKFLOW.md`/
  `CONTAINER.md` are symlinked into `crates/cargo-athena-macros/`**
  (`include_str!("../WORKFLOW.md")`) — the canonical files stay at repo
  root (README links + doc website unaffected), and `cargo package`
  dereferences the symlinks so the published crate carries the content
  (proven via `cargo publish --dry-run`). Do NOT delete those symlinks
  or repoint the include_str at `../../../` (that breaks publish — the
  file would be outside the package). After `cargo publish` it also
  cuts a **GitHub Release** (`softprops/action-gh-release@v2`,
  `generate_release_notes: true` — GitHub diffs commits since the prev
  tag), so the job has `permissions: contents: write`; release step is
  `if: github.ref_type == 'tag'` and after publish (skipped if publish
  failed). **0.1.0 shipped to crates.io 2026-05-17.**
- Each crate has its own `README.md` (auto-detected by cargo →
  crates.io page); the facade `cargo-athena` one is the fuller landing
  page, the rest are short stubs pointing at it. README has crates.io +
  docs.rs badges. NOTE: crate-README/metadata changes only show on
  crates.io at the **next** version — 0.1.0 is immutable.
- Per-version badges: GitHub has no per-matrix-job badge, so each job
  publishes pass/fail via `schneegans/dynamic-badges-action` to gist
  `6c34ed5be0444407c50ccf4597acba1f` (owner `mostlymaxi`); README uses
  shields.io endpoint badges. Secrets `GIST_TOKEN` (PAT, `gist` scope) +
  `BADGE_GIST_ID`. Badge step is `if:`-gated on **both secrets non-empty**
  so it's fully **skipped** (not just continue-on-error) until set — else
  it 404s PATCHing an empty gist id. Gate uses **`!cancelled()`, NOT
  `always()`** (do not change back): a concurrency-superseded run is
  `cancelled`, and with `always()` its badge step ran with
  `job.status=='cancelled'` → wrote `failing` for every version,
  clobbering the badge (then CDN/camo cached red) until the next run.
  `!cancelled()` still publishes on genuine pass/fail. (2026-05-17: this
  is why the v4 badge showed red right after the v0.2.0 commit+tag
  double-push — the cancelled `main` run, not a real failure; e2e
  v4.0.5 passed.)

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
