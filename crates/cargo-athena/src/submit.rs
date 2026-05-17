//! `cargo athena submit` — submit a `#[workflow]` (or a single
//! `#[container]`) to a real cluster, with the safety rails you'd
//! otherwise do by hand.
//!
//! It's `argo submit --from workflowtemplate/<name>` with pre-flight:
//! the args are **type-checked** against the template's real signature,
//! the **binary tarball** is confirmed uploaded, and every reachable
//! `WorkflowTemplate` is **registered + drift-checked** (create/update
//! with a y/N prompt) before the run is created. The created Workflow's
//! name is printed to stdout (scriptable).
//!
//! Transport mirrors the `argo` CLI: if `--argo-server`/`$ARGO_SERVER`
//! is set we POST to the Argo Server REST API; otherwise we create the
//! CR straight through the **Kubernetes API** via `kube` (kubeconfig or
//! in-cluster — it already handles client-certs/tokens and the
//! exec-credential plugins for EKS/GKE/AKS). All of this is behind the
//! `cli` feature; library consumers use `default-features = false`.

use cargo_athena::{AthenaConfig, ContainerRunMeta, api, serde_json};
use std::io::Write;
use std::process::exit;

#[derive(clap::Args)]
pub struct SubmitArgs {
    /// Template to submit — a `#[workflow]` (the whole DAG) or one
    /// `#[container]`. Full name (`<crate>-<fn>` kebab, or the
    /// `#[..(name = "…")]` override); list with
    /// `cargo athena workflow ls` / `container ls`.
    template: String,
    #[command(flatten)]
    pkg: crate::emulate::PkgSel,
    /// A workflow input: `-a name=value` (parsed as JSON if it parses,
    /// else a string). Repeatable.
    #[arg(short = 'a', long = "arg", value_name = "NAME=VALUE")]
    args: Vec<String>,
    /// JSON object of the inputs (merged under `-a`).
    #[arg(long = "input-file", value_name = "FILE")]
    input_file: Option<std::path::PathBuf>,
    /// Kubernetes namespace. Default: `$ARGO_NAMESPACE` →
    /// `[defaults].namespace` → `default`.
    #[arg(short = 'n', long)]
    namespace: Option<String>,
    /// ServiceAccount for the run. Default: `[defaults].service_account`.
    #[arg(long = "service-account")]
    service_account: Option<String>,
    /// Root-scoped `nodeSelector` on the submitted Workflow — `k=v`,
    /// repeatable (Argo applies it to every pod).
    #[arg(long = "node-selector", value_name = "K=V")]
    node_selector: Vec<String>,
    /// Submit via this Argo Server URL instead of the kube API (else
    /// `$ARGO_SERVER`; absent ⇒ kubeconfig/in-cluster).
    #[arg(long = "argo-server", value_name = "URL")]
    argo_server: Option<String>,
    /// Skip TLS verification talking to the Argo Server.
    #[arg(long = "insecure-skip-tls-verify")]
    insecure: bool,
    /// Re-apply every `WorkflowTemplate` even if unchanged.
    #[arg(long)]
    update: bool,
    /// Skip the "is the binary tarball uploaded?" pre-flight.
    #[arg(long = "skip-binary-check")]
    skip_binary_check: bool,
    /// Assume "yes" for every prompt (create/update templates + submit).
    #[arg(short = 'y', long)]
    yes: bool,
}

fn die(m: &str) -> ! {
    eprintln!("cargo athena submit: {m}");
    exit(2);
}

fn confirm(prompt: &str, assume_yes: bool) -> bool {
    if assume_yes {
        return true;
    }
    eprint!("{prompt} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).ok();
    matches!(s.trim(), "y" | "Y" | "yes" | "Yes")
}

/// Spawn the user binary in a `CARGO_ATHENA_*` mode; return its stdout
/// parsed as JSON (callers `from_value` into the concrete type).
fn from_bin(
    pkg: Option<&str>,
    bin: Option<&str>,
    env: &str,
    val: &str,
    what: &str,
) -> serde_json::Value {
    let out = crate::cargo_run(pkg, bin)
        .env(env, val)
        .output()
        .unwrap_or_else(|e| die(&format!("failed to spawn `cargo run`: {e}")));
    if !out.status.success() || out.stdout.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        die(&format!(
            "could not get {what} from your workflow binary \
             (run from the crate, or pass --package/--bin)"
        ));
    }
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| die(&format!("could not parse {what} ({e})")))
}

// ---- cluster transport ----------------------------------------------------

/// Minimal slice of the cluster we need: read a WorkflowTemplate,
/// apply (create/update) one, and create the Workflow.
trait Cluster {
    fn get_template(&self, ns: &str, name: &str) -> Option<serde_json::Value>;
    fn apply_template(&self, ns: &str, wt: &api::WorkflowTemplate);
    fn submit_workflow(&self, ns: &str, wf: &api::Workflow) -> String;
    fn describe(&self) -> String;
}

fn connect(a: &SubmitArgs) -> Box<dyn Cluster> {
    let server = a
        .argo_server
        .clone()
        .or_else(|| std::env::var("ARGO_SERVER").ok())
        .filter(|s| !s.trim().is_empty());
    match server {
        Some(s) => Box::new(ArgoServer::new(&s, a.insecure)),
        None => Box::new(KubeApi::new()),
    }
}

// --- Kubernetes API (kube-rs; the universal path) ---

struct KubeApi {
    client: kube::Client,
    rt: tokio::runtime::Runtime,
}

impl KubeApi {
    fn new() -> Self {
        let rt = crate::emulate::rt();
        let client = rt
            .block_on(kube::Client::try_default())
            .unwrap_or_else(|e| {
                die(&format!(
                    "no Kubernetes config ({e}). Set up kubeconfig/in-cluster, \
                     or use --argo-server/$ARGO_SERVER."
                ))
            });
        Self { client, rt }
    }

    fn api(&self, ns: &str, kind: &str, plural: &str) -> kube::Api<kube::api::DynamicObject> {
        let ar = kube::api::ApiResource {
            group: "argoproj.io".into(),
            version: "v1alpha1".into(),
            api_version: "argoproj.io/v1alpha1".into(),
            kind: kind.into(),
            plural: plural.into(),
        };
        kube::Api::namespaced_with(self.client.clone(), ns, &ar)
    }
}

impl Cluster for KubeApi {
    fn get_template(&self, ns: &str, name: &str) -> Option<serde_json::Value> {
        let api = self.api(ns, "WorkflowTemplate", "workflowtemplates");
        let got = self
            .rt
            .block_on(api.get_opt(name))
            .unwrap_or_else(|e| die(&format!("get workflowtemplate/{name}: {e}")));
        got.map(|o| serde_json::to_value(o).expect("DynamicObject is JSON"))
    }

    fn apply_template(&self, ns: &str, wt: &api::WorkflowTemplate) {
        let api = self.api(ns, "WorkflowTemplate", "workflowtemplates");
        let name = wt.metadata.as_ref().map(|m| m.name.clone()).unwrap_or_default();
        let body = serde_json::to_value(wt).expect("WorkflowTemplate is JSON");
        self.rt
            .block_on(api.patch(
                &name,
                &kube::api::PatchParams::apply("cargo-athena").force(),
                &kube::api::Patch::Apply(&body),
            ))
            .unwrap_or_else(|e| die(&format!("apply workflowtemplate/{name}: {e}")));
    }

    fn submit_workflow(&self, ns: &str, wf: &api::Workflow) -> String {
        let api = self.api(ns, "Workflow", "workflows");
        let obj: kube::api::DynamicObject =
            serde_json::from_value(serde_json::to_value(wf).expect("Workflow is JSON"))
                .expect("Workflow → DynamicObject");
        let created = self
            .rt
            .block_on(api.create(&kube::api::PostParams::default(), &obj))
            .unwrap_or_else(|e| die(&format!("create workflow: {e}")));
        created.metadata.name.unwrap_or_default()
    }

    fn describe(&self) -> String {
        "kube API (kubeconfig/in-cluster)".into()
    }
}

// --- Argo Server REST API ---

struct ArgoServer {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
    rt: tokio::runtime::Runtime,
}

impl ArgoServer {
    fn new(server: &str, insecure: bool) -> Self {
        let base = if server.contains("://") {
            server.trim_end_matches('/').to_string()
        } else {
            format!("https://{}", server.trim_end_matches('/'))
        };
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(insecure)
            .build()
            .unwrap_or_else(|e| die(&format!("http client: {e}")));
        Self {
            base,
            token: std::env::var("ARGO_TOKEN").ok().filter(|t| !t.is_empty()),
            http,
            rt: crate::emulate::rt(),
        }
    }

    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self.http.request(m, format!("{}{path}", self.base));
        if let Some(t) = &self.token {
            r = r.header(reqwest::header::AUTHORIZATION, t);
        }
        r
    }

    fn send(&self, rb: reqwest::RequestBuilder, what: &str) -> serde_json::Value {
        self.rt.block_on(async {
            let resp = rb
                .send()
                .await
                .unwrap_or_else(|e| die(&format!("{what}: {e} (is $ARGO_SERVER reachable?)")));
            let st = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if !st.is_success() {
                die(&format!("{what}: HTTP {st}\n{body}"));
            }
            serde_json::from_str(&body).unwrap_or(serde_json::Value::Null)
        })
    }
}

impl Cluster for ArgoServer {
    fn get_template(&self, ns: &str, name: &str) -> Option<serde_json::Value> {
        self.rt.block_on(async {
            let resp = self
                .req(
                    reqwest::Method::GET,
                    &format!("/api/v1/workflow-templates/{ns}/{name}"),
                )
                .send()
                .await
                .unwrap_or_else(|e| die(&format!("get workflowtemplate/{name}: {e}")));
            match resp.status() {
                reqwest::StatusCode::NOT_FOUND => None,
                s if s.is_success() => Some(resp.json().await.unwrap_or(serde_json::Value::Null)),
                s => die(&format!("get workflowtemplate/{name}: HTTP {s}")),
            }
        })
    }

    fn apply_template(&self, ns: &str, wt: &api::WorkflowTemplate) {
        let name = wt.metadata.as_ref().map(|m| m.name.clone()).unwrap_or_default();
        let exists = self.get_template(ns, &name).is_some();
        if exists {
            self.send(
                self.req(
                    reqwest::Method::PUT,
                    &format!("/api/v1/workflow-templates/{ns}/{name}"),
                )
                .json(&serde_json::json!({ "namespace": ns, "name": name, "template": wt })),
                &format!("update workflowtemplate/{name}"),
            );
        } else {
            self.send(
                self.req(
                    reqwest::Method::POST,
                    &format!("/api/v1/workflow-templates/{ns}"),
                )
                .json(&serde_json::json!({ "namespace": ns, "template": wt })),
                &format!("create workflowtemplate/{name}"),
            );
        }
    }

    fn submit_workflow(&self, ns: &str, wf: &api::Workflow) -> String {
        let v = self.send(
            self.req(reqwest::Method::POST, &format!("/api/v1/workflows/{ns}"))
                .json(&serde_json::json!({ "namespace": ns, "workflow": wf })),
            "submit workflow",
        );
        v["metadata"]["name"].as_str().unwrap_or_default().to_string()
    }

    fn describe(&self) -> String {
        format!("Argo Server {}", self.base)
    }
}

// ---- orchestration --------------------------------------------------------

pub fn submit(a: SubmitArgs) {
    let (pkg, bin) = a.pkg.resolve();

    // 1. Introspect every reachable template (names, params+types, the
    //    binary S3 coords). Resolve the root.
    let metas: Vec<ContainerRunMeta> = serde_json::from_value(from_bin(
        pkg.as_deref(),
        bin.as_deref(),
        "CARGO_ATHENA_LIST",
        "1",
        "template list",
    ))
    .unwrap_or_else(|e| die(&format!("could not parse template list ({e})")));
    let root = metas
        .iter()
        .find(|m| m.name == a.template)
        .unwrap_or_else(|| {
            die(&format!(
                "no template named {:?} (see `cargo athena workflow ls` / `container ls`)",
                a.template
            ))
        });

    // 2. Type-check args against the root's real signature (shared with
    //    `emulate` — missing/unknown/wrong-kind, one CLI-style report).
    let vals = crate::emulate::parse_args(a.input_file.as_deref(), &a.args);
    if let Err(report) = crate::emulate::validate_args(root, &vals) {
        die(&report);
    }

    // 3. The binary tarball must be uploaded or the pods can't
    //    bootstrap. The key is identical for every container in the
    //    binary, so one check suffices.
    if !a.skip_binary_check
        && let Some(ba) = metas.iter().find_map(|m| m.binary_artifact.as_ref())
        && !crate::emulate::s3_exists(&ba.s3)
    {
        die(&format!(
            "binary tarball not found at s3://{}/{} — run `cargo athena build` \
             and upload it first (or pass --skip-binary-check)",
            ba.s3.bucket, ba.s3.key
        ));
    }

    // 4. The deterministic WorkflowTemplate set (structured, for the
    //    register/drift checks).
    let wts: Vec<api::WorkflowTemplate> = serde_json::from_value(from_bin(
        pkg.as_deref(),
        bin.as_deref(),
        "CARGO_ATHENA_EMIT_JSON",
        "1",
        "emitted templates",
    ))
    .unwrap_or_else(|e| die(&format!("could not parse emitted templates ({e})")));

    let ns = a
        .namespace
        .clone()
        .or_else(|| std::env::var("ARGO_NAMESPACE").ok().filter(|s| !s.is_empty()))
        .or_else(|| AthenaConfig::load().defaults.namespace.clone())
        .unwrap_or_else(|| "default".to_string());
    let sa = a
        .service_account
        .clone()
        .unwrap_or_else(|| AthenaConfig::load().defaults.service_account.clone());

    let cluster = connect(&a);
    eprintln!("→ {} (namespace {ns})", cluster.describe());

    // 5. Register / drift-check every reachable WorkflowTemplate.
    let mut to_apply: Vec<(&api::WorkflowTemplate, &str)> = Vec::new();
    for wt in &wts {
        let name = wt.metadata.as_ref().map(|m| m.name.as_str()).unwrap_or("");
        match cluster.get_template(&ns, name) {
            None => to_apply.push((wt, "create")),
            Some(live) => {
                let live_spec: api::WorkflowSpec =
                    serde_json::from_value(live.get("spec").cloned().unwrap_or_default())
                        .unwrap_or_default();
                if a.update || Some(&live_spec) != wt.spec.as_ref() {
                    to_apply.push((wt, "drift"));
                }
            }
        }
    }
    if !to_apply.is_empty() {
        eprintln!("\nWorkflowTemplates needing apply in `{ns}`:");
        for (wt, why) in &to_apply {
            let n = wt.metadata.as_ref().map(|m| m.name.as_str()).unwrap_or("");
            eprintln!("  {why:<6} {n}");
        }
        if !confirm(
            &format!("\napply {} WorkflowTemplate(s) to `{ns}`?", to_apply.len()),
            a.yes,
        ) {
            die("aborted (templates not applied)");
        }
        for (wt, _) in &to_apply {
            cluster.apply_template(&ns, wt);
        }
        eprintln!("applied {} template(s).", to_apply.len());
    } else {
        eprintln!("all {} template(s) up to date.", wts.len());
    }

    // 6. Build + create the Workflow (workflowTemplateRef → root).
    let params: Vec<api::Parameter> = vals
        .iter()
        .map(|(k, v)| api::Parameter {
            name: k.clone(),
            value: Some(serde_json::to_string(v).expect("JSON-encodable")),
            ..Default::default()
        })
        .collect();
    let node_selector = a
        .node_selector
        .iter()
        .map(|kv| {
            let (k, v) = kv
                .split_once('=')
                .unwrap_or_else(|| die(&format!("--node-selector expects k=v, got {kv:?}")));
            (k.to_string(), v.to_string())
        })
        .collect();
    let wf = api::Workflow {
        api_version: api::API_VERSION.to_string(),
        kind: api::KIND_WORKFLOW.to_string(),
        metadata: Some(api::ObjectMeta {
            generate_name: format!("{}-", a.template),
            namespace: ns.clone(),
            ..Default::default()
        }),
        spec: Some(api::WorkflowSpec {
            workflow_template_ref: Some(api::WorkflowTemplateRef {
                name: a.template.clone(),
                cluster_scope: false,
            }),
            arguments: (!params.is_empty()).then(|| api::Arguments {
                parameters: params,
                ..Default::default()
            }),
            service_account_name: sa.clone(),
            node_selector,
            ..Default::default()
        }),
    };

    if !confirm(
        &format!(
            "submit `{}` (workflowTemplateRef) in `{ns}` as serviceAccount `{sa}`?",
            a.template
        ),
        a.yes,
    ) {
        die("aborted (not submitted)");
    }
    let name = cluster.submit_workflow(&ns, &wf);
    if name.is_empty() {
        die("submit returned no workflow name");
    }
    eprintln!("submitted ✓  (watch: argo get -n {ns} {name})");
    // The created name on stdout — scriptable (`W=$(cargo athena submit …)`).
    println!("{name}");
}
