//! Rooted at `pipeline_pvc`. Golden pins:
//!   * `WorkflowSpec.volumeClaimTemplates` carries one ephemeral PVC
//!     entry for `BuildCache` (size, access_modes, storage_class).
//!   * Each consumer container's `volumes` has both
//!     `persistentVolumeClaim` entries (ephemeral + external) with
//!     deterministic `claimName`s and matching `volumeMounts` at the
//!     hashed `/athena/pvcs/<…>` paths.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_pvc);
}
