// The PVC `name` override shares the DNS-1123 subdomain validator with
// `#[container]`/`#[workflow]` — a doubled `.` (empty label) is
// rejected (the old bespoke check let it through to k8s admission).
#[cargo_athena::ephemeral_pvc(name = "a..b", size = "1Gi", access_modes = ["ReadWriteOnce"])]
pub struct MyPvc;

fn main() {}
