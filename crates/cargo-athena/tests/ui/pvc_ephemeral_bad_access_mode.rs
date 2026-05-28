// `access_modes` entries must be valid K8s access modes.
#[cargo_athena::ephemeral_pvc(size = "1Gi", access_modes = ["RW"])]
pub struct MyPvc;

fn main() {}
