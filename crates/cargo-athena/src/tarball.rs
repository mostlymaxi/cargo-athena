//! Pure-Rust tar + gzip — the CLI packs and unpacks the per-binary
//! tarball without shelling out to a host `tar` (no `tar` dependency
//! for `cargo athena build`/`publish`/`emulate`).
//!
//! **Argo's `unpack` quirk (proven from Argo v4.0.5 source,
//! `workflow/executor/executor.go:1177-1218`):** the executor extracts
//! the input tarball into a temp dir, then:
//!
//! * if exactly **one** top-level entry → `os.Rename(entry, destPath)`
//!   (so `dest` BECOMES the entry — a directory iff the entry was a
//!   directory, a file iff the entry was a file!);
//! * else → `os.Rename(tmpDir, destPath)` (so `dest` is a directory
//!   whose contents are the tarball's top-level entries).
//!
//! That single-entry rename is **the** footgun: if we packed the binary
//! directly at the top level, a single-target tarball would have Argo
//! rename `app-<triple>` to `/athena/bin` → `/athena/bin` is a FILE
//! (the binary) rather than a directory containing it, and the
//! bootstrap's `/athena/bin/app-<triple>` path breaks.
//!
//! **The fix this module enforces by construction:** every tarball
//! [`create`] writes packs entries under a single top-level **`bin/`**
//! subdirectory. After Argo's unpack, the `bin/` subdir is renamed to
//! `/athena/bin` (the artifact path), so `/athena/bin/app-<triple>`
//! resolves correctly for *both* single- and multi-arch tarballs.
//!
//! The [`extract_argo_compat`] function mirrors Argo's exact unpack
//! semantics on the host so `cargo athena container emulate` stays
//! zero-drift with the real in-pod path.

use std::fs::File;
use std::io;
use std::path::Path;

/// Top-level directory name inside every tarball we produce. Argo's
/// single-entry rename will rename THIS directory to the artifact
/// `path`, guaranteeing `/athena/bin` is a directory (not a file) in
/// both single- and multi-arch cases.
const WRAP_DIR: &str = "bin";

/// Pack `(src_path, archive_name)` entries into a gzipped tar at `out`.
/// Each entry is written under [`WRAP_DIR`] (so the final archive name
/// is `bin/<archive_name>`); see the module doc for why.
///
/// File mode is taken from the source; we set 0o755 on the wrapping
/// directory header so the extracted dir is traversable. The compression
/// level is the default (6) — small win for a few MB of compressed
/// binaries; not worth tuning.
pub fn create(out: &Path, entries: &[(&Path, &str)]) -> io::Result<()> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let f = File::create(out)?;
    let gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    // Deterministic ordering — same input slice → same archive bytes
    // (and downstream `cargo athena publish --tarball` doesn't churn the
    // S3 object on no-op rebuilds).
    let mut sorted: Vec<&(&Path, &str)> = entries.iter().collect();
    sorted.sort_by(|a, b| a.1.cmp(b.1));
    for (src, name) in sorted {
        let arch_name = format!("{WRAP_DIR}/{name}");
        tar.append_path_with_name(src, &arch_name)?;
    }
    tar.into_inner()?.finish()?;
    Ok(())
}

/// Extract a gzipped tarball into `dst`, matching Argo's
/// `executor.unpack` semantics 1:1 (proven from source — see module
/// doc). `dst` is created (or replaced) as either a file or a
/// directory depending on the tarball's top-level layout, exactly as
/// Argo's executor init container would do.
///
/// Used by `cargo athena container emulate` to pre-stage `/athena/bin`
/// on the host bind-mount — zero-drift with the in-pod path.
pub fn extract_argo_compat(src: &Path, dst: &Path) -> io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dst.with_extension("tmpdir");
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(&tmp);
    std::fs::create_dir_all(&tmp)?;

    let f = File::open(src)?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut tar = tar::Archive::new(gz);
    tar.set_preserve_permissions(true);
    tar.unpack(&tmp)?;

    let mut top: Vec<std::path::PathBuf> = std::fs::read_dir(&tmp)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    // Argo also clears `dst` before renaming; mirror that so a stale
    // file/dir at `dst` doesn't cause `rename` to fail.
    let _ = std::fs::remove_dir_all(dst);
    let _ = std::fs::remove_file(dst);
    if top.len() == 1 {
        std::fs::rename(top.remove(0), dst)?;
        let _ = std::fs::remove_dir(&tmp);
    } else {
        std::fs::rename(&tmp, dst)?;
    }
    Ok(())
}
