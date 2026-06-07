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

use cargo_athena::{AthenaConfig, ContainerRunMeta, S3Ref, api, serde_json};
use std::io::Write;
use std::process::exit;

#[derive(clap::Args)]
#[command(after_help = "\
Tip: `cargo athena describe <BINARY> -w <TEMPLATE>` lists the template's \
expected inputs (name + Rust type) plus a copy-pasteable submit line.")]
pub struct SubmitArgs {
    #[command(flatten)]
    bin: crate::binsrc::BinSel,
    /// Template to submit - a `#[workflow]` (the whole DAG) or one
    /// `#[container]`. `<crate>-<fn>` kebab, or the `#[..(name = "…")]`
    /// override (the short `<fn>` form works too). Default: the binary's
    /// root template. `cargo athena ls` lists them.
    #[arg(short = 'w', long = "workflow", value_name = "TEMPLATE")]
    workflow: Option<String>,
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
    /// Workflow priority (int32). Higher = scheduled first when the
    /// controller hits its parallelism limit. No default; Argo treats
    /// absence as 0.
    #[arg(long, value_name = "N")]
    priority: Option<i32>,
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

/// Run the workflow binary in a `CARGO_ATHENA_*` mode; return its stdout
/// parsed as JSON (callers `from_value` into the concrete type).
fn from_bin(
    src: &crate::binsrc::BinarySource,
    env: &str,
    val: &str,
    what: &str,
) -> serde_json::Value {
    let out = src.run_mode(env, val, what);
    serde_json::from_slice(&out).unwrap_or_else(|e| die(&format!("could not parse {what} ({e})")))
}

// ---- cluster transport ----------------------------------------------------

/// Minimal slice of the cluster we need: read a WorkflowTemplate,
/// apply (create/update) one, and create the Workflow.
trait Cluster {
    fn get_template(&self, ns: &str, name: &str) -> Option<serde_json::Value>;
    fn apply_template(&self, ns: &str, wt: &api::WorkflowTemplate);
    fn submit_workflow(&self, ns: &str, wf: &api::Workflow) -> String;
    /// Names of WorkflowTemplates matching a k8s label selector (e.g.
    /// `cargo.athena/pkg=foo,cargo.athena/tag=dev-bar`).
    fn list_templates(&self, ns: &str, selector: &str) -> Vec<String>;
    fn delete_template(&self, ns: &str, name: &str);
    fn describe(&self) -> String;
}

fn connect(a: &SubmitArgs) -> Box<dyn Cluster> {
    connect_to(a.argo_server.clone(), a.insecure)
}

/// Transport auto-select shared by `submit` and `prune`: an explicit /
/// `$ARGO_SERVER` URL picks the Argo Server REST API, else the kube API.
fn connect_to(argo_server: Option<String>, insecure: bool) -> Box<dyn Cluster> {
    let server = argo_server
        .or_else(|| std::env::var("ARGO_SERVER").ok())
        .filter(|s| !s.trim().is_empty());
    match server {
        Some(s) => Box::new(ArgoServer::new(&s, insecure)),
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
        let name = wt
            .metadata
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_default();
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

    fn list_templates(&self, ns: &str, selector: &str) -> Vec<String> {
        let api = self.api(ns, "WorkflowTemplate", "workflowtemplates");
        let lp = kube::api::ListParams::default().labels(selector);
        let list = self
            .rt
            .block_on(api.list(&lp))
            .unwrap_or_else(|e| die(&format!("list workflowtemplates ({selector}): {e}")));
        list.items
            .into_iter()
            .filter_map(|o| o.metadata.name)
            .collect()
    }

    fn delete_template(&self, ns: &str, name: &str) {
        let api = self.api(ns, "WorkflowTemplate", "workflowtemplates");
        self.rt
            .block_on(api.delete(name, &kube::api::DeleteParams::default()))
            .unwrap_or_else(|e| die(&format!("delete workflowtemplate/{name}: {e}")));
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
        let name = wt
            .metadata
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_default();
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
        v["metadata"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn list_templates(&self, ns: &str, selector: &str) -> Vec<String> {
        let v = self.send(
            self.req(
                reqwest::Method::GET,
                &format!("/api/v1/workflow-templates/{ns}"),
            )
            .query(&[("listOptions.labelSelector", selector)]),
            "list workflowtemplates",
        );
        v["items"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i["metadata"]["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn delete_template(&self, ns: &str, name: &str) {
        self.send(
            self.req(
                reqwest::Method::DELETE,
                &format!("/api/v1/workflow-templates/{ns}/{name}"),
            ),
            &format!("delete workflowtemplate/{name}"),
        );
    }

    fn describe(&self) -> String {
        format!("Argo Server {}", self.base)
    }
}

// ---- orchestration --------------------------------------------------------

pub fn submit(a: SubmitArgs) {
    let src = a.bin.resolve();
    // Confirm it's a cargo-athena binary (+ protocol) and learn its root
    // template (the default when no `-w` is given).
    let info = src.probe();
    // The binary's identity is sealed at build time; surface the channel
    // so a dev build deployed to a shared cluster is never a surprise
    // (the apply step below still confirms before any cluster write).
    if info.channel == "dev" {
        eprintln!(
            "note: deploying a DEV build (tag `{}`); its WorkflowTemplates are \
             version-suffixed and won't collide with releases.",
            info.version_tag
        );
    }
    let template = a.workflow.clone().unwrap_or(info.default_template);

    // 1. Introspect every reachable template (names, params+types, the
    //    binary S3 coords). Resolve the root.
    let metas: Vec<ContainerRunMeta> =
        serde_json::from_value(from_bin(&src, "CARGO_ATHENA_LIST", "1", "template list"))
            .unwrap_or_else(|e| die(&format!("could not parse template list ({e})")));
    // Accept either the full `<crate>-<fn>` name or the short form (no
    // package prefix) - the binary's package is already known.
    let root = metas
        .iter()
        .find(|m| m.name == template || m.name == format!("{}-{}", m.package, template))
        .unwrap_or_else(|| {
            die(&format!(
                "no template named {template:?} (see `cargo athena ls`)"
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
        &src,
        "CARGO_ATHENA_EMIT_JSON",
        "1",
        "emitted templates",
    ))
    .unwrap_or_else(|e| die(&format!("could not parse emitted templates ({e})")));

    let ns = a
        .namespace
        .clone()
        .or_else(|| {
            std::env::var("ARGO_NAMESPACE")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| AthenaConfig::load().defaults.namespace.clone())
        .unwrap_or_else(|| "default".to_string());
    let sa = a
        .service_account
        .clone()
        .unwrap_or_else(|| AthenaConfig::load().defaults.service_account.clone());

    let cluster = connect(&a);
    eprintln!("cluster: {} (namespace {ns})", cluster.describe());

    // 5. Register / drift-check every reachable WorkflowTemplate.
    let st = crate::feedback::step(format!(
        "Checking {} WorkflowTemplate(s) against cluster",
        wts.len()
    ));
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
    st.finish();
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
        let st = crate::feedback::step(format!("Applying {} template(s)", to_apply.len()));
        for (wt, _) in &to_apply {
            cluster.apply_template(&ns, wt);
        }
        st.finish();
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
    // The Workflow must reference the VERSIONED root WT resource. The
    // emitted set (`wts`) carries the build-time-sealed `<base>-<tag>` on
    // `metadata.name`, but the inner template name == `spec.entrypoint`
    // stays on the base, so find the root WT by its (base) entrypoint and
    // use that WT's `metadata.name` verbatim — core's emit transform is
    // the single source of truth; the CLI never recomputes the tag.
    let tpl_name = wts
        .iter()
        .find(|wt| wt.spec.as_ref().is_some_and(|s| s.entrypoint == root.name))
        .and_then(|wt| wt.metadata.as_ref().map(|m| m.name.clone()))
        .unwrap_or_else(|| {
            die(&format!(
                "emitted WorkflowTemplate set has no root entrypoint {:?}",
                root.name
            ))
        });
    let wf = api::Workflow {
        api_version: api::API_VERSION.to_string(),
        kind: api::KIND_WORKFLOW.to_string(),
        metadata: Some(api::ObjectMeta {
            generate_name: format!("{tpl_name}-"),
            namespace: ns.clone(),
            ..Default::default()
        }),
        spec: Some(api::WorkflowSpec {
            workflow_template_ref: Some(api::WorkflowTemplateRef {
                name: tpl_name.clone(),
                cluster_scope: false,
            }),
            arguments: (!params.is_empty()).then(|| api::Arguments {
                parameters: params,
                ..Default::default()
            }),
            service_account_name: sa.clone(),
            node_selector,
            priority: a.priority,
            ..Default::default()
        }),
    };

    if !confirm(
        &format!("submit `{tpl_name}` (workflowTemplateRef) in `{ns}` as serviceAccount `{sa}`?"),
        a.yes,
    ) {
        die("aborted (not submitted)");
    }
    let st = crate::feedback::step(format!("Creating Workflow `{tpl_name}`"));
    let name = cluster.submit_workflow(&ns, &wf);
    if name.is_empty() {
        drop(st);
        die("submit returned no workflow name");
    }
    st.finish();
    eprintln!("\nwatch:  argo get -n {ns} {name}");
    // The created name on stdout — scriptable (`W=$(cargo athena submit …)`).
    println!("{name}");
}

// ---- prune (delete a version's WorkflowTemplates + S3 binary) --------------

/// `cargo athena prune <tag>` — remove one deployed version of this
/// binary's template-set: every `WorkflowTemplate` carrying the
/// `cargo.athena/{pkg,tag}` labels, plus the `{pkg}/<tag>/{bin}.tar.gz`
/// S3 tarball (unless `--keep-binary`). `pkg`/`bin` come from the probed
/// binary; the selector always pins both pkg AND tag, so it can never
/// fan out into an accidental mass-delete.
#[derive(clap::Args)]
pub struct PruneArgs {
    /// Version to remove: a dev slot (`dev-foo`), a release tag
    /// (`0-6-0`), or a raw semver (`0.6.0`, normalized to `0-6-0`).
    #[arg(value_name = "TAG")]
    tag: String,
    #[command(flatten)]
    bin: crate::binsrc::BinSel,
    /// Keep the S3 binary tarball; remove only the WorkflowTemplates.
    #[arg(long = "keep-binary")]
    keep_binary: bool,
    /// Kubernetes namespace. Default: `$ARGO_NAMESPACE` →
    /// `[defaults].namespace` → `default`.
    #[arg(short = 'n', long)]
    namespace: Option<String>,
    /// Prune via this Argo Server URL instead of the kube API (else
    /// `$ARGO_SERVER`; absent ⇒ kubeconfig/in-cluster).
    #[arg(long = "argo-server", value_name = "URL")]
    argo_server: Option<String>,
    /// Skip TLS verification talking to the Argo Server.
    #[arg(long = "insecure-skip-tls-verify")]
    insecure: bool,
    /// Assume "yes" for the delete confirmation.
    #[arg(short = 'y', long)]
    yes: bool,
}

pub fn prune(a: PruneArgs) {
    let src = a.bin.resolve();
    let info = src.probe();
    let (pkg, bin) = (info.package, info.bin);
    // Accept a raw semver ("0.6.0") or a kebab tag ("dev-foo") — normalize
    // to the exact form the labels + S3 key carry.
    let tag = api::munge::version_tag(&a.tag);
    if tag.is_empty() {
        die(&format!("invalid tag {:?} (normalizes to empty)", a.tag));
    }

    let ns = a
        .namespace
        .clone()
        .or_else(|| {
            std::env::var("ARGO_NAMESPACE")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| AthenaConfig::load().defaults.namespace.clone())
        .unwrap_or_else(|| "default".to_string());

    let cluster = connect_to(a.argo_server.clone(), a.insecure);
    eprintln!("cluster: {} (namespace {ns})", cluster.describe());

    // Every WorkflowTemplate carrying this exact (pkg, tag). Both pinned,
    // so no broad-selector mass-delete is possible.
    let selector = format!("cargo.athena/pkg={pkg},cargo.athena/tag={tag}");
    let names = cluster.list_templates(&ns, &selector);

    // The S3 binary for this tag: repo coords from athena.toml, key from
    // (pkg, tag, bin) — the same `{pkg}/<tag>/{bin}.tar.gz` the binary
    // bakes and `publish` uploads.
    let cfg = AthenaConfig::load();
    let repo = &cfg.artifact_repository.s3;
    let s3 = S3Ref {
        endpoint: repo.endpoint.clone(),
        bucket: repo.bucket.clone(),
        region: repo.region.clone(),
        insecure: repo.insecure,
        key: format!("{pkg}/{tag}/{bin}.tar.gz"),
    };

    if names.is_empty() && a.keep_binary {
        eprintln!("nothing to prune: no WorkflowTemplates match {selector}.");
        return;
    }

    eprintln!("\nprune `{pkg}` @ tag `{tag}` from `{ns}`:");
    if names.is_empty() {
        eprintln!("  (no matching WorkflowTemplates)");
    }
    for n in &names {
        eprintln!("  WorkflowTemplate  {n}");
    }
    if !a.keep_binary {
        eprintln!("  s3 binary         s3://{}/{}", s3.bucket, s3.key);
    }

    if !confirm("\nproceed with deletion?", a.yes) {
        die("aborted (nothing deleted)");
    }

    for n in &names {
        let st = crate::feedback::step(format!("Deleting WorkflowTemplate {n}"));
        cluster.delete_template(&ns, n);
        st.finish();
    }
    if !a.keep_binary {
        let st = crate::feedback::step(format!("Deleting s3://{}/{}", s3.bucket, s3.key));
        let deleted = crate::emulate::s3_delete(&s3);
        st.finish();
        if !deleted {
            eprintln!("  (S3 binary was already absent)");
        }
    }
    eprintln!(
        "pruned {} WorkflowTemplate(s){} for `{pkg}` @ `{tag}`.",
        names.len(),
        if a.keep_binary {
            ""
        } else {
            " + the S3 binary"
        }
    );
}
