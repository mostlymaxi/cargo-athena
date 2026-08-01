// `#[workflow(name = "...")]` gets the same DNS-1123 subdomain check
// as `#[container]` / the PVC macros — a leading `.` means an empty
// label, which k8s rejects at admission.
#[cargo_athena::workflow(name = ".foo")]
fn pipeline() {}

fn main() {}
