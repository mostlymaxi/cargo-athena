// `pvc!` outside #[container]/#[fragment] is rejected: silently
// missing the volume mount would let user code touch /athena/pvcs
// paths that don't exist in the pod.
#[cargo_athena::ephemeral_pvc(size = "1Gi", access_modes = ["ReadWriteOnce"])]
pub struct MyPvc;

fn helper() {
    let _ = cargo_athena::pvc!(MyPvc);
}

fn main() {
    helper();
}
