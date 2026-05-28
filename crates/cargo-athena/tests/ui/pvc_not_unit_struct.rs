// `#[ephemeral_pvc]` / `#[external_pvc]` require a unit struct.
#[cargo_athena::ephemeral_pvc(size = "1Gi", access_modes = ["ReadWriteOnce"])]
pub struct MyPvc {
    name: String,
}

fn main() {}
