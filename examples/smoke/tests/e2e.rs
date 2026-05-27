//! Golden: spawn the compiled `smoke` bins and pin their emit/run output.
//!
//!   cargo test -p cargo-athena-example-smoke                 # assert
//!   UPDATE_EXPECT=1 cargo test -p cargo-athena-example-smoke # refresh goldens

use std::path::PathBuf;
use std::process::Command;

const BIN_PIPELINE: &str = env!("CARGO_BIN_EXE_smoke");
const BIN_RETURNS: &str = env!("CARGO_BIN_EXE_smoke-returns");
const BIN_HOOKS: &str = env!("CARGO_BIN_EXE_smoke-hooks");
const BIN_ONEXIT: &str = env!("CARGO_BIN_EXE_smoke-onexit");
const BIN_FIELDS: &str = env!("CARGO_BIN_EXE_smoke-fields");
const BIN_FANOUT: &str = env!("CARGO_BIN_EXE_smoke-fanout");
const BIN_IF: &str = env!("CARGO_BIN_EXE_smoke-if");
const BIN_NESTED: &str = env!("CARGO_BIN_EXE_smoke-nested");
const BIN_INJECT: &str = env!("CARGO_BIN_EXE_smoke-inject");
const BIN_NS: &str = env!("CARGO_BIN_EXE_smoke-ns");
const BIN_RETRY: &str = env!("CARGO_BIN_EXE_smoke-retry");
const BIN_TTL: &str = env!("CARGO_BIN_EXE_smoke-ttl");
const BIN_DEADLINE: &str = env!("CARGO_BIN_EXE_smoke-deadline");
const BIN_ASYNC: &str = env!("CARGO_BIN_EXE_smoke-async");
const BIN_SECRETS: &str = env!("CARGO_BIN_EXE_smoke-secrets");
const BIN_POD_ATTRS: &str = env!("CARGO_BIN_EXE_smoke-pod-attrs");
const BIN_MUTEX: &str = env!("CARGO_BIN_EXE_smoke-mutex");
const BIN_ARTIFACT: &str = env!("CARGO_BIN_EXE_smoke-artifact");
const BIN_ARTIFACT_WF: &str = env!("CARGO_BIN_EXE_smoke-artifact-wf");
const BIN_ARTIFACT_CLONE: &str = env!("CARGO_BIN_EXE_smoke-artifact-clone");
const BIN_TOLERATIONS: &str = env!("CARGO_BIN_EXE_smoke-tolerations");
const BIN_AFFINITY: &str = env!("CARGO_BIN_EXE_smoke-affinity");
const BIN_POD_SPEC_PATCH: &str = env!("CARGO_BIN_EXE_smoke-pod-spec-patch");
const BIN_IMAGE_PULL_SECRETS: &str = env!("CARGO_BIN_EXE_smoke-image-pull-secrets");
const BIN_INJECT_ATTR: &str = env!("CARGO_BIN_EXE_smoke-inject-attr");
const BIN_BOUNDARY_SCHEDULING: &str = env!("CARGO_BIN_EXE_smoke-boundary-scheduling");
const BIN_PARALLELISM: &str = env!("CARGO_BIN_EXE_smoke-parallelism");

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

/// Compare `actual` against `tests/golden/<name>`, or rewrite it when
/// `UPDATE_EXPECT` is set.
fn assert_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_EXPECT").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        eprintln!("updated golden {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {} — run `UPDATE_EXPECT=1 cargo test`",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "\n{name} drifted from its golden. If intended, refresh with \
         `UPDATE_EXPECT=1 cargo test -p cargo-athena-example-smoke`.\n"
    );
}

fn run_bin(bin: &str, envs: &[(&str, &str)]) -> String {
    run_bin_argv(bin, envs, &[])
}

fn run_bin_argv(bin: &str, envs: &[(&str, &str)], argv: &[&str]) -> String {
    let mut cmd = Command::new(bin);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.args(argv);
    let out = cmd.output().expect("failed to spawn smoke binary");
    assert!(
        out.status.success(),
        "{bin} exited with {:?}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("binary stdout was not UTF-8")
}

/// Emit mode: the multi-doc WorkflowTemplate stream for `pipeline`
/// (templates only — the default).
#[test]
fn emit_pipeline() {
    assert_golden("pipeline.yaml", &run_bin(BIN_PIPELINE, &[]));
}

/// `--with-workflow`: the same stream **plus** the convenience runnable
/// `Workflow` (generateName, workflowTemplateRef → root).
#[test]
fn emit_pipeline_with_workflow() {
    assert_golden(
        "pipeline_with_workflow.yaml",
        &run_bin(BIN_PIPELINE, &[("CARGO_ATHENA_WITH_WORKFLOW", "1")]),
    );
}

/// `#[workflow]` return values: `sub_pipeline` (returns its tail call's
/// result) is consumed by `pipeline_returns`. Pins the emitted
/// `outputs.parameters.result` block + `{{tasks.r.outputs.result}}` wiring.
#[test]
fn emit_pipeline_returns() {
    assert_golden("pipeline_returns.yaml", &run_bin(BIN_RETURNS, &[]));
}

/// Per-task builders: pins `continueOn`, the `exit` hook, and an
/// expression hook on the emitted DAG tasks (+ hook templates emitted).
#[test]
fn emit_pipeline_hooks() {
    assert_golden("pipeline_hooks.yaml", &run_bin(BIN_HOOKS, &[]));
}

/// `#[workflow(on_exit_if_root=…)]` -> the template's own
/// `spec.hooks.exit`, plus a per-task `.on_exit(record("done"))` hook
/// carrying arguments.
#[test]
fn emit_pipeline_onexit() {
    assert_golden("pipeline_onexit.yaml", &run_bin(BIN_ONEXIT, &[]));
}

/// `a.field` lowering: pins the `{{=toJSON(fromJSON(...)['id'])}}`
/// expr-templating + the DAG dep on the producing task.
#[test]
fn emit_pipeline_fields() {
    assert_golden("pipeline_fields.yaml", &run_bin(BIN_FIELDS, &[]));
}

/// `.fan_out`: pins the `withParam` over the list + `{{item}}` arg +
/// the DAG dep on the producer + the aggregated `Vec` consumed after.
#[test]
fn emit_pipeline_fanout() {
    assert_golden("pipeline_fanout.yaml", &run_bin(BIN_FANOUT, &[]));
}

/// `if`/`else`/`else if` lowering: synthesized `when`-gated wrapper
/// workflows + a value-`if` whose `outputs.parameters.return` selects the
/// taken branch via a status-ternary `valueFrom.expression`.
#[test]
fn emit_pipeline_if() {
    assert_golden("pipeline_if.yaml", &run_bin(BIN_IF, &[]));
}

/// Nested-call lowering: a template call in argument position
/// (recursive → output ref + dep) and a call hoisted out of an `if`
/// condition to a parent task.
#[test]
fn emit_pipeline_nested() {
    assert_golden("pipeline_nested.yaml", &run_bin(BIN_NESTED, &[]));
}

/// Attribute param injection: a struct field lowered into `image`
/// (`{{=fromJSON(inputs.parameters['m'])['id']}}`) + (via `pipeline`)
/// `combine`'s injected image/service_account/node_selector.
#[test]
fn emit_pipeline_inject() {
    assert_golden("pipeline_inject.yaml", &run_bin(BIN_INJECT, &[]));
}

/// `#[workflow(node_selector=…)]`: literal key+value set on the dag
/// template (Argo cascades it to every `templateRef`'d task pod),
/// including the raw `{{workflow.parameters.region}}` escape-hatch
/// literal. No `"lit" + arg` injection — workflows are DAGs, not pods.
#[test]
fn emit_pipeline_ns() {
    assert_golden("pipeline_ns.yaml", &run_bin(BIN_NS, &[]));
}

/// `#[container(retry(..), timeout=…)]` + `#[workflow(retry(..))]`:
/// template-level Argo `retryStrategy`/`timeout` on the producing WT
/// (container) and the workflow's own dag template.
#[test]
fn emit_pipeline_retry() {
    assert_golden("pipeline_retry.yaml", &run_bin(BIN_RETRY, &[]));
}

/// `#[workflow(ttl(..), pod_gc(..))]`: WorkflowSpec-scoped Argo
/// `ttlStrategy`/`podGC` stamped on the workflow's own WorkflowTemplate
/// `spec` (same per-WT plumbing as `on_exit_if_root`).
#[test]
fn emit_pipeline_ttl() {
    assert_golden("pipeline_ttl.yaml", &run_bin(BIN_TTL, &[]));
}

/// Per-template `Template.activeDeadlineSeconds`: `active_deadline = 600`
/// (int) on a container WT, `active_deadline = "1h30m"` (humantime →
/// 5400) on the workflow's dag template.
#[test]
fn emit_pipeline_deadline() {
    assert_golden("pipeline_deadline.yaml", &run_bin(BIN_DEADLINE, &[]));
}

/// `CARGO_ATHENA_DESCRIBE`: the binary reports a `ContainerRunMeta`
/// for one `#[container]`, derived from the *same* `Template::build()`
/// that `emit` uses — the zero-drift contract `cargo athena container
/// emulate` consumes (image, params→env, the binary/host!/artifact
/// ports, the scratch dir, the result path).
#[test]
fn describe_fetch() {
    assert_golden(
        "describe_fetch.json",
        &run_bin(
            BIN_NS,
            &[("CARGO_ATHENA_DESCRIBE", "cargo-athena-example-smoke-fetch")],
        ),
    );
}

/// `CARGO_ATHENA_LIST`: every reachable template's metadata as a JSON
/// array — what `container ls` / `workflow ls` render as a table.
#[test]
fn list_all() {
    assert_golden(
        "list_all.json",
        &run_bin(BIN_NS, &[("CARGO_ATHENA_LIST", "1")]),
    );
}

/// Same, rooted at `pipeline_if` — pins `synthetic: true` on the
/// athena-generated `if`/`else` wrapper + arm sub-workflows (what
/// `workflow ls` hides unless `--include-synthetic`).
#[test]
fn list_if() {
    assert_golden(
        "list_if.json",
        &run_bin(BIN_IF, &[("CARGO_ATHENA_LIST", "1")]),
    );
}

/// Run mode: a container that returns a value (JSON to stdout).
#[test]
fn run_mode_transform() {
    // `fn transform(data: String, factor: i64)` -> positional argv.
    let out = run_bin_argv(
        BIN_PIPELINE,
        &[(
            "CARGO_ATHENA_TEMPLATE",
            "cargo-athena-example-smoke-transform",
        )],
        &[r#""hello""#, "4"],
    );
    assert_golden("run_transform.txt", &out);
}

/// Run mode raw-string fallback: argv for a `String` param may be
/// either the JSON-quoted form (`"hello"`, what Argo sends in Regime
/// B) or the bare unquoted form (`hello`, friendlier for hand-typed
/// shell invocations). Both must decode to the same value.
#[test]
fn run_mode_transform_unquoted_string() {
    let out = run_bin_argv(
        BIN_PIPELINE,
        &[(
            "CARGO_ATHENA_TEMPLATE",
            "cargo-athena-example-smoke-transform",
        )],
        &["hello", "4"], // `hello` is NOT valid JSON; fallback path
    );
    // Identical golden as the quoted-form test above: the decoder must
    // produce the same `data: String = "hello"` from both inputs.
    assert_golden("run_transform.txt", &out);
}

/// `async fn` `#[container]` — emitted YAML is identical to a sync
/// container (asyncness only affects the in-pod execution path, not
/// the WorkflowTemplate shape).
#[test]
fn emit_pipeline_async() {
    assert_golden("pipeline_async.yaml", &run_bin(BIN_ASYNC, &[]));
}

/// `env` + `host_mount` + `annotations` attrs (container) and
/// `annotations` (workflow). Container side exercises all three with
/// injection on env/annotations values; workflow side puts plain +
/// `{{workflow.parameters.tier}}`-templated annotations on the dag
/// template's `metadata.annotations`.
#[test]
fn emit_pipeline_pod_attrs() {
    assert_golden("pipeline_pod_attrs.yaml", &run_bin(BIN_POD_ATTRS, &[]));
}

/// `secret!`/`secret_opt!`: pin the `env[].valueFrom.secretKeyRef`
/// entries on the container template (including the
/// fragment-propagated `db-creds/password`).
#[test]
fn emit_pipeline_secrets() {
    assert_golden("pipeline_secrets.yaml", &run_bin(BIN_SECRETS, &[]));
}

/// `mutexes` (template-level on a `#[container]`, injecting from
/// `inputs.parameters['shard']`) + `mutexes` (template-level literal
/// on a `#[workflow]`) + `mutexes_if_root` (root-only WorkflowSpec
/// scope, injecting from `workflow.parameters['env']`). Pins the
/// `Template.synchronization` and `WorkflowSpec.synchronization` emit
/// shape across both macros.
#[test]
fn emit_pipeline_mutex() {
    assert_golden("pipeline_mutex.yaml", &run_bin(BIN_MUTEX, &[]));
}

/// `Artifact<T>` DAG-wired flow: pins the producer's
/// `outputs.artifacts.return` (templated `s3.key`, no archive so
/// Argo's default tar+gzip applies), the consumer's
/// `inputs.artifacts`, and the workflow's
/// `arguments.artifacts.return.from` wiring across the templateRef
/// wormhole. Live-validated on Argo v4.0.5 (probe step, 2026-05-25).
#[test]
fn emit_pipeline_artifact() {
    assert_golden("pipeline_artifact.yaml", &run_bin(BIN_ARTIFACT, &[]));
}

/// Workflow-level `Artifact<T>` flow: a sub-workflow that returns
/// `Artifact<Meta>` (bubbled via `outputs.artifacts.return.from`) fed
/// into a sibling sub-workflow that accepts `Artifact<Meta>` as input
/// (forwarded via `inputs.artifacts.<name>` to a downstream container).
/// Pins the workflow-side artifact bubble + forward shapes that the
/// container-only `pipeline_artifact` doesn't exercise.
#[test]
fn emit_pipeline_artifact_wf() {
    assert_golden("pipeline_artifact_wf.yaml", &run_bin(BIN_ARTIFACT_WF, &[]));
}

/// `Artifact<T>` fan-out via `.clone()`: one producer feeding two
/// consumers. Pins that both consumers' `arguments.artifacts.m.from`
/// point at the same upstream artifact, same idiom as parameter-typed
/// `.clone()` fan-out.
#[test]
fn emit_pipeline_artifact_clone() {
    assert_golden(
        "pipeline_artifact_clone.yaml",
        &run_bin(BIN_ARTIFACT_CLONE, &[]),
    );
}

/// `tolerations` (template-level, `Template.Tolerations` with
/// `inputs.parameters['kind']` injection) + `tolerations_if_root`
/// (root-only `WorkflowSpec.Tolerations` with `workflow.parameters
/// ['role']` injection). Pins the 5-tuple of (key, op, value, effect,
/// secs) on both tiers.
#[test]
fn emit_pipeline_tolerations() {
    assert_golden("pipeline_tolerations.yaml", &run_bin(BIN_TOLERATIONS, &[]));
}

/// `affinity` (template-level, opaque YAML `Template.Affinity`
/// preference) + `affinity_if_root` (root-only `WorkflowSpec.Affinity`
/// requirement with embedded `{{workflow.parameters.role}}`). Pins
/// the parsed YAML serializes back cleanly through serde_norway.
#[test]
fn emit_pipeline_affinity() {
    assert_golden("pipeline_affinity.yaml", &run_bin(BIN_AFFINITY, &[]));
}

/// `pod_spec_patch` (template-level, `Template.PodSpecPatch` with
/// `inputs.parameters['cpu_limit']` injection) + `pod_spec_patch
/// _if_root` (root-only `WorkflowSpec.PodSpecPatch` with
/// `workflow.parameters['grace']` injection). Argo concats both at
/// pod-creation and strategic-merges onto every pod in the run.
#[test]
fn emit_pipeline_pod_spec_patch() {
    assert_golden(
        "pipeline_pod_spec_patch.yaml",
        &run_bin(BIN_POD_SPEC_PATCH, &[]),
    );
}

/// Root-only `WorkflowSpec.ImagePullSecrets`. Pins that the two
/// literal Secret names from `image_pull_secrets_if_root` land on
/// the root WorkflowTemplate's `spec.imagePullSecrets`.
#[test]
fn emit_pipeline_image_pull_secrets() {
    assert_golden(
        "pipeline_image_pull_secrets.yaml",
        &run_bin(BIN_IMAGE_PULL_SECRETS, &[]),
    );
}

/// `parallelism = 2` lands on the root workflow template's
/// `Template.parallelism`; `parallelism_if_root = 4` lands on
/// `WorkflowSpec.parallelism`.
#[test]
fn emit_pipeline_parallelism() {
    assert_golden("pipeline_parallelism.yaml", &run_bin(BIN_PARALLELISM, &[]));
}

/// `#[inject("<argo expr>")]` per-arg attribute. Pins:
/// 1. `smart_retry`'s `inputs.parameters` declares only `payload`
///    (the two inject args are NOT inputs.parameters entries).
/// 2. `container.args[]` carries three positional slots in fn
///    declaration order: `{{inputs.parameters.payload}}`,
///    `{{retries}}`, `"{{pod.name}}"`.
/// 3. The workflow body's `smart_retry("hello".to_string())` call
///    passes only the `payload` arg.
#[test]
fn emit_pipeline_inject_attr() {
    assert_golden("pipeline_inject_attr.yaml", &run_bin(BIN_INJECT_ATTR, &[]));
}

/// `boundary_tolerations` + `boundary_affinity` on `#[workflow]` —
/// land on the dag/steps template's own `Template.Tolerations` +
/// `Template.Affinity`. Literal-only (Argo's boundary-copy makes
/// per-arg injection unsafe at this tier). Pins the dag template's
/// scheduling block.
#[test]
fn emit_pipeline_boundary_scheduling() {
    assert_golden(
        "pipeline_boundary_scheduling.yaml",
        &run_bin(BIN_BOUNDARY_SCHEDULING, &[]),
    );
}

/// Run mode: with Argo's secretKeyRef env vars planted by the
/// executor, `rt::secret_value` reads them back; `secret_opt!`
/// returns `None` when the optional env is unset.
#[test]
fn run_mode_use_secrets() {
    // `fn use_secrets(label: String)` -> one positional argv.
    let out = run_bin_argv(
        BIN_SECRETS,
        &[
            (
                "CARGO_ATHENA_TEMPLATE",
                "cargo-athena-example-smoke-use-secrets",
            ),
            // The env names mirror what the macro emits in the WT —
            // ATHENA_SEC_<munged-secret>__<munged-key> (uppercased,
            // non-alphanumerics → _).
            ("ATHENA_SEC_API_TOKENS__API", "tok123"),
            ("ATHENA_SEC_DB_CREDS__PASSWORD", "dbpw"),
            // ATHENA_SEC_DEBUG_CREDS__TRACE deliberately unset:
            // `secret_opt!` returns None.
        ],
        &[r#""hi""#],
    );
    assert_golden("run_use_secrets.txt", &out);
}

/// Run mode: drives the async-fn container body via the macro-built
/// `cargo_athena::__async::block_on` (single-thread tokio runtime).
/// Proves the runtime is wired correctly + the body's `.await` lands.
#[test]
fn run_mode_async_delay() {
    let out = run_bin_argv(
        BIN_ASYNC,
        &[("CARGO_ATHENA_TEMPLATE", "cargo-athena-example-smoke-delay")],
        &[r#""hi""#],
    );
    assert_golden("run_async_delay.txt", &out);
}

/// Run mode: a container whose real body branches and uses `host!`.
#[test]
fn run_mode_branchy() {
    let out = run_bin_argv(
        BIN_PIPELINE,
        &[(
            "CARGO_ATHENA_TEMPLATE",
            "cargo-athena-example-smoke-branchy",
        )],
        &[r#""fast""#],
    );
    assert_golden("run_branchy.txt", &out);
}
