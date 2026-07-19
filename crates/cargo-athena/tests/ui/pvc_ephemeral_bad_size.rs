// `size` must be a structurally valid K8s resource quantity — a bad
// string would otherwise fail deep in k8s PVC admission at run time.
#[cargo_athena::ephemeral_pvc(size = "10 Gigs", access_modes = ["ReadWriteOnce"])]
pub struct MyPvc;

fn main() {}
