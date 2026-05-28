// `pvc!` in a #[workflow] is rejected: workflows are DAGs, not pods.
#[cargo_athena::ephemeral_pvc(size = "1Gi", access_modes = ["ReadWriteOnce"])]
pub struct MyPvc;

#[cargo_athena::workflow]
fn wf() {
    let _ = cargo_athena::pvc!(MyPvc);
}

fn main() {}
