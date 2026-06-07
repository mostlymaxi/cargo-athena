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
    if gate
        && let Some(state) = &st
        && !on_release
        && !yes
    {
        eprint!(
            "warning: not on a release branch (on {:?}); this builds a DEV \
             version, not a clean semver release.\nProceed? [y/N] ",
            state.branch
        );
        use std::io::Write;
        let _ = std::io::stderr().flush();
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).ok();
        if !matches!(s.trim(), "y" | "Y" | "yes" | "Yes") {
            eprintln!("aborted.");
            exit(1);
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
