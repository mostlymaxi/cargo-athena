//! Build-time version-tag resolution for `cargo athena build`/`publish`.
//!
//! This is where the version identity is *sealed*: it reads git state,
//! applies the two release gates, and produces the tag + provenance the
//! caller bakes into the binary (via compile-time `ATHENA_*` env exported
//! before `cargo zigbuild`) and reuses as the S3 key segment. `emit` /
//! `submit` never recompute it — they read it back off the artifact.

use cargo_athena::api::munge;
use std::process::{Command, exit};

/// Branches that mint a clean semver (release) tag. Off these → dev. A
/// non-repo build (e.g. an unpacked source tarball in CI) is treated as
/// release too. (Hardcoded for now; a `[templates] release_branch`
/// override is a clean follow-up.)
const RELEASE_BRANCHES: &[&str] = &["main", "master"];

/// Resolved build identity, ready to bake + use as the S3 key segment.
pub struct BuildTag {
    /// DNS-1123 tag: `kebab(semver)` (release) or `dev-<slot>` (dev).
    pub tag: String,
    /// `release` | `dev`.
    pub channel: &'static str,
    /// Short commit sha, when built inside a git repo.
    pub commit: Option<String>,
    /// Whether the working tree had uncommitted changes.
    pub dirty: bool,
}

struct GitState {
    branch: String,
    commit: String,
    dirty: bool,
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_state() -> Option<GitState> {
    // Distinguish "real repo" from "no git / not a repo" cleanly.
    if git(&["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return None;
    }
    Some(GitState {
        branch: git(&["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default(),
        commit: git(&["rev-parse", "--short", "HEAD"]).unwrap_or_default(),
        dirty: git(&["status", "--porcelain"])
            .map(|s| !s.is_empty())
            .unwrap_or(false),
    })
}

/// Channel a kebab tag implies: dev tags are `dev-…`, everything else
/// (a `kebab(semver)`) is a release.
fn channel_of(tag: &str) -> &'static str {
    if tag.starts_with("dev-") {
        "dev"
    } else {
        "release"
    }
}

/// For a consumer command's SOURCE build (`emit`/`submit`/… run with no
/// prebuilt `[BINARY]`): bake the SAME version tag `build`/`publish` would,
/// so a dev tree's emitted names + S3 key match a prior `publish` —
/// WITHOUT the user having to export `ATHENA_VERSION_TAG`. The CLI is
/// building the binary here, so it seals the tag (exactly like `build`)
/// rather than letting the plain-`cargo build` fallback stamp the release
/// form. Mirrors `resolve`'s dev outcome (`dev-<commit>` + provenance).
///
/// No-op when `ATHENA_VERSION_TAG` is already set (the binary bakes it),
/// or on a release tree (clean + on a release branch) — there the binary's
/// own `kebab(CARGO_PKG_VERSION)` fallback already equals the release tag.
/// NOT gated: `emit`/`submit` never hard-fail on a dirty tree or prompt
/// off-main (those gates guard the *distributed artifact*, i.e.
/// `build`/`publish`).
pub fn export_source_build_tag() {
    if std::env::var_os("ATHENA_VERSION_TAG").is_some_and(|v| !v.is_empty()) {
        return;
    }
    let Some(st) = git_state() else { return };
    let off_road = st.dirty || !RELEASE_BRANCHES.contains(&st.branch.as_str());
    if !off_road {
        return;
    }
    let slot = if st.commit.is_empty() {
        "local".to_string()
    } else {
        st.commit.clone()
    };
    let tag = format!("dev-{}", munge::version_tag(&slot));
    // SAFETY: single-threaded, set before any cargo child is spawned.
    unsafe {
        std::env::set_var("ATHENA_VERSION_TAG", &tag);
        if !st.commit.is_empty() {
            std::env::set_var("ATHENA_GIT_COMMIT", &st.commit);
        }
        if st.dirty {
            std::env::set_var("ATHENA_GIT_DIRTY", "true");
        }
    }
}

/// Seal the slot named by a consumer `--dev-tag` (on `submit`/`emit`) into
/// the source build, symmetric with `build`/`publish`'s `--dev-tag`. Forces
/// the dev channel: `Some(Some("maxi"))` -> `dev-maxi`, bare `Some(None)` ->
/// `dev-<commit>`. No-op when the flag is absent. Sets `ATHENA_VERSION_TAG`
/// (which the later `export_source_build_tag` then respects), so the
/// compile bakes the named slot.
pub fn export_dev_tag(flag: Option<Option<String>>) {
    let st = git_state();
    let slot = match flag {
        None => return,
        Some(Some(v)) => v,
        Some(None) => st
            .as_ref()
            .map(|s| s.commit.clone())
            .filter(|c| !c.is_empty())
            .unwrap_or_default(),
    };
    let slot = match munge::version_tag(&slot) {
        s if s.is_empty() => "local".to_string(),
        s => s,
    };
    // SAFETY: single-threaded, set before any cargo child is spawned.
    unsafe {
        std::env::set_var("ATHENA_VERSION_TAG", format!("dev-{slot}"));
        if let Some(s) = &st {
            if !s.commit.is_empty() {
                std::env::set_var("ATHENA_GIT_COMMIT", &s.commit);
            }
            if s.dirty {
                std::env::set_var("ATHENA_GIT_DIRTY", "true");
            }
        }
    }
}

/// Resolve the build tag, applying the two **separate** release gates:
///   - `--allow-dirty` (HARD): a dirty tree would bake uncommitted code
///     into the binary, so building one without the flag is a hard error.
///   - non-release branch (SOFT): warn + confirm (`--yes` bypass).
///
/// `dev_tag`: `None` = not forced; `Some(None)` = bare `--dev-tag` (slot
/// defaults to the short commit); `Some(Some(v))` = `--dev-tag v`. Any
/// `Some` forces the dev channel. A clean build on a release branch with
/// no `--dev-tag` is the ONLY path to a `kebab(semver)` release tag.
///
/// `gate` is false for a `--print` dry run (compute the would-be tag, but
/// don't hard-fail / prompt). A pre-set `ATHENA_VERSION_TAG` in the
/// environment is the explicit escape hatch (CI): it wins verbatim, skips
/// the gates, and git is consulted only for best-effort provenance.
pub fn resolve(
    semver: &str,
    dev_tag: Option<Option<String>>,
    allow_dirty: bool,
    yes: bool,
    gate: bool,
) -> BuildTag {
    let st = git_state();
    let commit_of = |st: &Option<GitState>| {
        st.as_ref()
            .map(|s| s.commit.clone())
            .filter(|c| !c.is_empty())
    };

    // Explicit override (CI / power user): `ATHENA_VERSION_TAG=… cargo
    // athena publish` forces the tag verbatim, no gates.
    if let Ok(t) = std::env::var("ATHENA_VERSION_TAG")
        && !t.is_empty()
    {
        let tag = munge::version_tag(&t);
        // Reject a value that normalizes to empty (all-symbol) — baking
        // tag="" would yield a `<base>-` name + a `{pkg}//{bin}` key.
        if tag.is_empty() {
            eprintln!(
                "error: ATHENA_VERSION_TAG={t:?} normalizes to an empty tag \
                 (no [a-z0-9] characters)."
            );
            exit(2);
        }
        return BuildTag {
            channel: channel_of(&tag),
            tag,
            commit: commit_of(&st),
            dirty: st.as_ref().map(|s| s.dirty).unwrap_or(false),
        };
    }

    let dirty = st.as_ref().map(|s| s.dirty).unwrap_or(false);
    // Not a git repo → treat as release (the documented fallback).
    let on_release = st
        .as_ref()
        .map(|s| RELEASE_BRANCHES.contains(&s.branch.as_str()))
        .unwrap_or(true);
    let commit = commit_of(&st);

    // Gate 1 (hard): uncommitted changes would be baked into the binary.
    if gate && dirty && !allow_dirty {
        eprintln!(
            "error: working tree has uncommitted changes that would be baked \
             into the binary.\n  Commit them for a clean build, or pass \
             --allow-dirty to build a dev version."
        );
        exit(2);
    }

    let is_release = on_release && !dirty && dev_tag.is_none();
    if is_release {
        return BuildTag {
            tag: munge::version_tag(semver),
            channel: "release",
            commit,
            dirty,
        };
    }

    // Dev channel. Gate 2 (soft): off the release branch → warn + confirm.
    // Only PROMPT on a TTY; non-interactively (CI) proceed to the dev
    // build (the intended off-main outcome) with a printed warning, so a
    // plain `build`/`publish` on a feature branch / detached HEAD never
    // hangs on a closed stdin or hard-fails. (The HARD dirty gate above
    // still protects via --allow-dirty.)
    if gate
        && let Some(state) = &st
        && !on_release
        && !yes
    {
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            use std::io::Write;
            eprint!(
                "warning: not on a release branch (on {:?}); this builds a DEV \
                 version, not a clean semver release.\nProceed? [y/N] ",
                state.branch
            );
            let _ = std::io::stderr().flush();
            let mut s = String::new();
            std::io::stdin().read_line(&mut s).ok();
            if !matches!(s.trim(), "y" | "Y" | "yes" | "Yes") {
                eprintln!("aborted.");
                exit(1);
            }
        } else {
            eprintln!(
                "warning: not on a release branch (on {:?}); building a DEV \
                 version (non-interactive — pass --yes to silence).",
                state.branch
            );
        }
    }

    // Dev slot: explicit `--dev-tag v` (kebabed), else the short commit,
    // else `local`. Prefix is fixed `dev-` so the channel derives back
    // unambiguously (core checks `tag.starts_with("dev-")`).
    let slot_raw = match dev_tag {
        Some(Some(v)) => v,
        _ => commit.clone().unwrap_or_default(),
    };
    let slot = {
        let s = munge::version_tag(&slot_raw);
        if s.is_empty() {
            commit
                .clone()
                .map(|c| munge::version_tag(&c))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "local".to_string())
        } else {
            s
        }
    };
    BuildTag {
        tag: format!("dev-{slot}"),
        channel: "dev",
        commit,
        dirty,
    }
}
