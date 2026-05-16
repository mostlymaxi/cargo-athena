//! Cross-module + cross-CRATE importing. `importing_pipeline` composes:
//!   * `cargo_athena_example_smoke::pipeline` — a workflow from *another
//!     crate*, used exactly like a local one (the wormhole force-links the
//!     whole upstream closure across the crate boundary), and
//!   * `local::module_pipeline` — a workflow in *another module* of this
//!     crate, composed the same way.
//! If the type-wormhole leaked at either boundary, the emitted stream
//! would be missing templates and the golden would fail.

use cargo_athena::{container, workflow};
// Cross-CRATE import of an upstream workflow — used like a local one.
use cargo_athena_example_smoke::pipeline;

#[container]
pub fn consumer_step(note: String) -> String {
    format!("consumed:{note}")
}

#[container]
pub fn finalize(x: String) {
    println!("finalize {x}");
}

pub mod local {
    //! A workflow living in another *module* of this crate, composing a
    //! crate-root container — same wormhole path as the cross-crate case.
    use cargo_athena::{container, workflow};

    #[container]
    pub fn local_step(tag: String) -> String {
        format!("local:{tag}")
    }

    #[workflow]
    pub fn module_pipeline() {
        let s = local_step("m".to_string());
        crate::finalize(s); // cross-module: compose a crate-root template
    }
}

#[workflow]
pub fn importing_pipeline() {
    let n = consumer_step("hello".to_string());
    pipeline(); // cross-CRATE workflow -> workflow (the wormhole)
    local::module_pipeline(); // cross-MODULE workflow -> workflow
    finalize(n); // depends on consumer_step's output
}
