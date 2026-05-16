//! Argo Workflows API types.
//!
//! Generated from `proto/cargo_athena/api/v1/argo.proto` by `build.rs`
//! (prost) with serde derives layered on for YAML emission. Downstream
//! crates only ever touch the re-exported types here, so swapping the
//! vendored proto subset for the full upstream Argo schema is a
//! single-crate change.

/// serde `skip_serializing_if` support.
///
/// `prost-build` applies one blanket field attribute to *every* generated
/// field, so we need a single generic predicate that works for every field
/// type prost can emit (scalars, `Option`, `Vec`, `HashMap`, messages).
pub mod ser {
    use std::collections::{BTreeMap, HashMap};

    /// True when a value is "empty" and should be omitted from output.
    pub trait Skip {
        fn skip(&self) -> bool;
    }

    impl Skip for String {
        fn skip(&self) -> bool {
            self.is_empty()
        }
    }
    impl Skip for bool {
        fn skip(&self) -> bool {
            !*self
        }
    }
    impl Skip for i32 {
        fn skip(&self) -> bool {
            *self == 0
        }
    }
    impl Skip for i64 {
        fn skip(&self) -> bool {
            *self == 0
        }
    }
    impl Skip for u32 {
        fn skip(&self) -> bool {
            *self == 0
        }
    }
    impl Skip for u64 {
        fn skip(&self) -> bool {
            *self == 0
        }
    }
    impl<T> Skip for Option<T> {
        fn skip(&self) -> bool {
            self.is_none()
        }
    }
    impl<T> Skip for Vec<T> {
        fn skip(&self) -> bool {
            self.is_empty()
        }
    }
    impl<K, V> Skip for HashMap<K, V> {
        fn skip(&self) -> bool {
            self.is_empty()
        }
    }
    impl<K, V> Skip for BTreeMap<K, V> {
        fn skip(&self) -> bool {
            self.is_empty()
        }
    }

    /// The function named in the generated `skip_serializing_if`.
    pub fn skip<T: Skip>(value: &T) -> bool {
        value.skip()
    }

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// `Vec<ParallelSteps>` -> Argo's bare `[[DagTask]]` (list of lists).
    pub fn ser_steps<S: Serializer>(
        v: &[crate::ParallelSteps],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let outer: Vec<&Vec<crate::DagTask>> = v.iter().map(|p| &p.steps).collect();
        outer.serialize(s)
    }

    /// Argo `[[DagTask]]` -> `Vec<ParallelSteps>`.
    pub fn de_steps<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<crate::ParallelSteps>, D::Error> {
        let outer: Vec<Vec<crate::DagTask>> = Vec::deserialize(d)?;
        Ok(outer
            .into_iter()
            .map(|steps| crate::ParallelSteps { steps })
            .collect())
    }
}

include!(concat!(env!("OUT_DIR"), "/cargo_athena.api.v1.rs"));

/// Argo's `apiVersion` for `Workflow` resources.
pub const API_VERSION: &str = "argoproj.io/v1alpha1";
/// Argo's `kind` for `Workflow` resources.
pub const KIND_WORKFLOW: &str = "Workflow";
/// Argo's `kind` for `WorkflowTemplate` resources.
pub const KIND_WORKFLOW_TEMPLATE: &str = "WorkflowTemplate";
