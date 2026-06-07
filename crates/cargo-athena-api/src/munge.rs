//! Pure name / path / env-var derivers shared by `cargo-athena-core`
//! (emit side) and `cargo-athena-macros` (proc-macro / user-build side).
//!
//! Every helper here is a deterministic `&str -> String` (or
//! `(&str, &str) -> String`) — no I/O, no randomness, no dependence on
//! process-local state. Same input MUST produce the same output across
//! process boundaries, because emit-side (e.g. `Template::build`
//! stamping a Volume name onto the WorkflowTemplate) and user-build-
//! side (e.g. the proc macro baking `host!("/p")` into a literal mount
//! path) BOTH call into here and the two need to agree byte-for-byte.
//!
//! Lives in `cargo-athena-api` because both `core` (runtime) and
//! `macros` (proc-macro) can depend on `api` (`api` is pure-types +
//! these pure functions; no heavyweight deps). Prior versions of
//! athena mirrored the formulas in each crate and pinned them with
//! algorithm tests; that drift risk is gone now that there's a single
//! source.

/// In-pod root for `host!("/p")` mounts — the macro never lets the
/// user pick the in-container path (`host!("/")` would otherwise
/// overlay the host root over the container fs). Each `host!` lands
/// at `<this>/<fnv-hex-of-literal>`.
pub const ATHENA_MOUNTS_DIR: &str = "/athena/mounts";

/// In-pod root for `pvc!(Type)` mounts. Same shape as
/// [`ATHENA_MOUNTS_DIR`]: each PVC lands at `<this>/<fnv-hex-of-argo-
/// name>`, never at a user-chosen path, so two crates declaring the
/// same explicit PVC name can't accidentally overlay each other's
/// directories.
pub const ATHENA_PVCS_DIR: &str = "/athena/pvcs";

/// Where Argo input artifact ports land in-pod.
pub const ATHENA_IN_DIR: &str = "/athena/artifacts/in";

/// Where Argo output artifact ports land in-pod (Argo collects them
/// from here after the container exits).
pub const ATHENA_OUT_DIR: &str = "/athena/artifacts/out";

/// FNV-1a 64-bit hash of `input`, rendered as 16 lowercase hex
/// chars. Fixed initial state (no `DefaultHasher` random seed) so
/// emit-side and proc-macro-side produce identical output for the
/// same input in two different process invocations.
///
/// Determinism is load-bearing for every athena Volume name / mount
/// path that's keyed on a literal. Swapping in a different hash
/// silently breaks every existing deployment.
///
/// 16 hex = 64 bits — collision-resistant well past any plausible
/// per-binary literal count; and short enough (16 chars) that a
/// `host-` / `pvc-` prefix + this fits DNS-1123's 63-char Volume
/// name limit with room to spare.
pub fn fnv_1a_64_hex(input: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET;
    for b in input.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    format!("{h:016x}")
}

/// `<crate>-<fn>` argo names: lowercases, swaps `_` for `-`, trims
/// leading/trailing `-` so that idiomatic Rust names like `fn
/// _unused_helper()` or `fn foo_()` don't produce DNS-1123-invalid
/// Argo template names (`-foo` / `foo-`, both rejected by k8s).
/// Internal `__` becomes `--` and is kept (valid).
pub fn kebab(s: &str) -> String {
    let s = s.replace('_', "-").to_ascii_lowercase();
    s.trim_matches('-').to_string()
}

/// DNS-1123-safe version-tag suffix. Appended to `WorkflowTemplate`
/// names (`<base>-<tag>`), reused as the S3 binary-key path segment
/// (`{pkg}/<tag>/{bin}.tar.gz`) and the `cargo.athena/tag` label value,
/// so the one coordinate is identical everywhere it appears. A semver
/// `1.2.3` becomes `1-2-3`; a dev tag `dev-foo` stays `dev-foo`;
/// pre-release/build metadata flattens (`1.0.0-rc.1+build` ->
/// `1-0-0-rc-1-build`). Lowercases, maps every non-`[a-z0-9]` char to
/// `-`, collapses runs of `-`, trims leading/trailing `-`.
///
/// NOTE: distinct from the `cargo.athena/version` *label* value, which
/// keeps the raw semver — dots are legal in a label value but not
/// idiomatic in a k8s resource name. This deriver is for the *name* /
/// key-segment, where the kebab form is required. Returns `""` only for
/// input with no `[a-z0-9]` at all (a pathological dev tag); callers
/// that accept user tags reject an empty result.
pub fn version_tag(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    // Start `true` so a leading run of separators produces no leading
    // `-`; the run-collapse + trailing trim fall out of the same flag.
    let mut prev_dash = true;
    for c in raw.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Prefix that marks a dev-channel version tag. The CLI mints dev tags
/// as `dev-<slot>`; everything else (a `kebab(semver)`) is a release.
/// Both the producer (`format!("{DEV_PREFIX}{slot}")`) and every consumer
/// ([`channel_of`]) key off this one symbol, so the channel contract
/// can't drift across crates.
pub const DEV_PREFIX: &str = "dev-";

/// `dev` if `tag` is a dev slot, else `release`. The single home of the
/// `dev-`-prefix → channel rule: core (label stamping + the config-free
/// probe) and the CLI (gitinfo) all call this instead of re-spelling
/// `starts_with`, so a dev binary can't silently report `release`.
pub fn channel_of(tag: &str) -> &'static str {
    if tag.starts_with(DEV_PREFIX) {
        "dev"
    } else {
        "release"
    }
}

/// Resolve a binary's sealed version tag from its build-time-baked value:
/// the baked tag munged (in case a plain `cargo build` baked a raw /
/// dotted / empty value, bypassing the CLI's gitinfo munge), else
/// `kebab(semver)`. Idempotent on an already-kebab tag. The single owner
/// of "what tag did this binary seal?", shared by `BuildCtx::collect` and
/// the config-free probe path so the two can never disagree.
pub fn seal_tag(baked: Option<&str>, semver: &str) -> String {
    baked
        .map(version_tag)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| version_tag(semver))
}

/// The S3 object key of a binary's tarball: `{pkg}/<tag>/{bin}.tar.gz`.
/// The ONE owner of the publish ↔ emit ↔ prune contract — the in-binary
/// emit side, the `publish` uploader, and `prune` all call this, so the
/// key segments / extension can't drift and orphan a tarball.
pub fn binary_key(pkg: &str, tag: &str, bin: &str) -> String {
    format!("{pkg}/{tag}/{bin}.tar.gz")
}

/// `cargo.athena/*` provenance label keys: stamped on every emitted
/// `WorkflowTemplate` (core's `athena_labels`) and selected on by `cargo
/// athena prune`. Shared consts so the emit side and the prune selector
/// reference one spelling — a rename moves both, where a silent typo
/// would make `prune` match zero templates and look like success.
pub const LABEL_PKG: &str = "cargo.athena/pkg";
pub const LABEL_VERSION: &str = "cargo.athena/version";
pub const LABEL_BIN: &str = "cargo.athena/bin";
pub const LABEL_TOOLCHAIN: &str = "cargo.athena/toolchain";
pub const LABEL_TAG: &str = "cargo.athena/tag";
pub const LABEL_CHANNEL: &str = "cargo.athena/channel";
pub const LABEL_COMMIT: &str = "cargo.athena/commit";
pub const LABEL_DIRTY: &str = "cargo.athena/dirty";

/// In-pod mount path for a `host!("/p")` literal.
pub fn host_mount_path(host_path: &str) -> String {
    format!("{ATHENA_MOUNTS_DIR}/{}", fnv_1a_64_hex(host_path))
}

/// K8s Volume name for a host-path mount. `host-` (5) + 16 hex = 21
/// chars, fits DNS-1123 (max 63).
pub fn host_volume_name(host_path: &str) -> String {
    format!("host-{}", fnv_1a_64_hex(host_path))
}

/// In-pod mount path for a PVC, keyed on its argo name.
pub fn pvc_mount_path(argo_name: &str) -> String {
    format!("{ATHENA_PVCS_DIR}/{}", fnv_1a_64_hex(argo_name))
}

/// K8s Volume name for a PVC mount. `pvc-` (4) + 16 hex = 20 chars,
/// fits DNS-1123.
pub fn pvc_volume_name(argo_name: &str) -> String {
    format!("pvc-{}", fnv_1a_64_hex(argo_name))
}

/// In-pod file path for an input artifact named `name`.
pub fn in_artifact_path(name: &str) -> String {
    format!("{ATHENA_IN_DIR}/{name}")
}

/// In-pod file path for an output artifact named `name`.
pub fn out_artifact_path(name: &str) -> String {
    format!("{ATHENA_OUT_DIR}/{name}")
}

/// Pod env var name a `secret!`/`secret_opt!` decl gets. Derived
/// deterministically from the K8s `(secret_name, key)` pair so the
/// emit-side (declares the matching `secretKeyRef` envFrom) and the
/// run-side (reads via `std::env::var`) agree. Uppercased, non-
/// alphanumerics flattened to `_`, halves separated by `__` to stay
/// distinguishable.
pub fn secret_env_name(name: &str, key: &str) -> String {
    let mut s = String::from("ATHENA_SEC_");
    push_munged(&mut s, name);
    s.push_str("__");
    push_munged(&mut s, key);
    s
}

fn push_munged(out: &mut String, input: &str) {
    for c in input.chars() {
        out.push(if c.is_ascii_alphanumeric() {
            c.to_ascii_uppercase()
        } else {
            '_'
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_pins_known_value() {
        // FNV-1a 64-bit with the standard offset/prime, lowercase hex.
        // Pinning a known input -> known output so an accidental swap
        // to a different hash function fails LOUD here, not silently
        // in every user's cluster (emit-side and proc-macro-side hash
        // the same literal in different process invocations).
        assert_eq!(fnv_1a_64_hex("/var/lib"), "5b8d11771a6f946b");
    }

    #[test]
    fn fnv_no_canonicalization() {
        // Two strings that Linux resolves identically MUST hash to
        // different Volumes — the user wrote two distinct literals,
        // k8s handles path resolution at mount time.
        assert_ne!(fnv_1a_64_hex("/var/lib"), fnv_1a_64_hex("//var/lib"));
        assert_ne!(fnv_1a_64_hex("/var/lib"), fnv_1a_64_hex("/var/lib/"));
        assert_ne!(fnv_1a_64_hex("/var/lib"), fnv_1a_64_hex("/var//lib"));
    }

    #[test]
    fn fnv_hex_is_16_chars() {
        for input in ["", "/", "x", "/very/deep/nested/path/that/keeps/going"] {
            assert_eq!(fnv_1a_64_hex(input).len(), 16);
        }
    }

    #[test]
    fn host_volume_name_fits_dns_1123() {
        for path in [
            "/",
            "/etc/myapp",
            "/very/deeply/nested/path/that/keeps/going/forever/and/ever/and/ever",
            "/has spaces and weird chars: !@#$%",
        ] {
            let n = host_volume_name(path);
            assert!(n.len() <= 63, "{n:?} exceeds DNS-1123 label limit");
            assert_eq!(n.len(), 21); // host- + 16 hex
            assert!(n.starts_with("host-"));
            assert!(n.chars().next().unwrap().is_ascii_alphabetic());
            assert!(
                n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "{n:?} contains non-DNS-1123 chars"
            );
        }
    }

    #[test]
    fn host_mount_path_agrees_with_volume_name_suffix() {
        for path in ["/", "/etc/myapp", "/var/lib"] {
            let v = host_volume_name(path);
            let m = host_mount_path(path);
            let suffix = v.strip_prefix("host-").unwrap();
            assert_eq!(
                format!("{ATHENA_MOUNTS_DIR}/{suffix}"),
                m,
                "host-{path} volume + mount disagree on hash suffix"
            );
        }
    }

    #[test]
    fn kebab_lowercases_and_hyphenates() {
        assert_eq!(kebab("run_a_container"), "run-a-container");
        assert_eq!(kebab("RunFoo"), "runfoo");
    }

    #[test]
    fn kebab_preserves_digits() {
        assert_eq!(kebab("fetch2"), "fetch2");
        assert_eq!(kebab("step_1_of_3"), "step-1-of-3");
        assert_eq!(kebab("v1_handler"), "v1-handler");
    }

    #[test]
    fn kebab_trims_leading_and_trailing_underscores() {
        assert_eq!(kebab("_unused"), "unused");
        assert_eq!(kebab("foo_"), "foo");
    }

    #[test]
    fn kebab_keeps_internal_double_underscore() {
        // Internal `__` is intentional (e.g. macro-generated names);
        // becomes `--` and stays.
        assert_eq!(kebab("inner__bar"), "inner--bar");
    }

    #[test]
    fn version_tag_pins_known_values() {
        // Pinned so an accidental change to the suffix scheme fails LOUD
        // here — the tag is part of every versioned WT name AND the S3
        // binary key, so a silent change would orphan deployed templates.
        assert_eq!(version_tag("1.2.3"), "1-2-3");
        assert_eq!(version_tag("0.0.0"), "0-0-0");
        assert_eq!(version_tag("1.0.0-rc.1+build"), "1-0-0-rc-1-build");
        assert_eq!(version_tag("dev-foo"), "dev-foo");
        assert_eq!(version_tag("dev-a1b2c3d"), "dev-a1b2c3d");
    }

    #[test]
    fn channel_of_keys_off_dev_prefix() {
        assert_eq!(channel_of("dev-foo"), "dev");
        assert_eq!(channel_of("dev-a1b2c3d"), "dev");
        assert_eq!(channel_of("1-2-3"), "release");
        assert_eq!(channel_of("0-0-0"), "release");
    }

    #[test]
    fn seal_tag_normalizes_and_falls_back() {
        // A baked tag wins, munged to DNS-1123 form.
        assert_eq!(seal_tag(Some("dev-foo"), "9.9.9"), "dev-foo");
        assert_eq!(seal_tag(Some("1.2.3"), "9.9.9"), "1-2-3");
        // An empty / all-symbol baked tag falls back to kebab(semver).
        assert_eq!(seal_tag(Some(""), "1.2.3"), "1-2-3");
        assert_eq!(seal_tag(Some("+++"), "1.2.3"), "1-2-3");
        // No baked tag (plain `cargo build`) -> kebab(semver).
        assert_eq!(seal_tag(None, "1.2.3"), "1-2-3");
    }

    #[test]
    fn binary_key_pins_the_layout() {
        assert_eq!(
            binary_key("myapp", "1-2-3", "app"),
            "myapp/1-2-3/app.tar.gz"
        );
        assert_eq!(
            binary_key("myapp", "dev-foo", "app"),
            "myapp/dev-foo/app.tar.gz"
        );
    }

    #[test]
    fn version_tag_is_dns_1123_safe() {
        for raw in ["1.2.3", "1.0.0-rc.1+build.5", "dev-Foo_Bar", "  weird +.+ "] {
            let t = version_tag(raw);
            assert!(!t.is_empty(), "{raw:?} -> empty tag");
            assert!(
                t.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{t:?} contains non-DNS-1123 chars"
            );
            assert!(!t.starts_with('-') && !t.ends_with('-'), "{t:?} edge dash");
            assert!(!t.contains("--"), "{t:?} has an uncollapsed run");
        }
    }

    #[test]
    fn secret_env_name_uppercases_and_separates() {
        assert_eq!(
            secret_env_name("github-creds", "token"),
            "ATHENA_SEC_GITHUB_CREDS__TOKEN"
        );
        assert_eq!(
            secret_env_name("my.secret-name", "api.key"),
            "ATHENA_SEC_MY_SECRET_NAME__API_KEY"
        );
    }

    #[test]
    fn secret_env_name_is_valid_posix_env_var() {
        // POSIX env var names: `[a-zA-Z_][a-zA-Z_0-9]*`. Output must
        // always satisfy this regardless of user input — non-
        // alphanumerics flatten to `_`, prefix is `ATHENA_SEC_` so
        // the first-char rule is met, halves are uppercased.
        let valid_env = |s: &str| {
            let mut cs = s.chars();
            cs.next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        for (name, key) in [
            ("foo", "bar"),
            ("my-secret", "db.password"),
            ("name with spaces", "key/with/slashes"),
            ("-leading-dash", "trailing.dot."),
            ("123-numeric-start", "ok"),
        ] {
            let env = secret_env_name(name, key);
            assert!(valid_env(&env), "{env} is not a valid POSIX env var");
        }
    }
}
