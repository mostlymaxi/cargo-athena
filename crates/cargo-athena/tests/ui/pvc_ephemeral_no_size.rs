// `#[ephemeral_pvc]` requires `size`.
#[cargo_athena::ephemeral_pvc(access_modes = ["ReadWriteOnce"])]
pub struct MyPvc;

fn main() {}
