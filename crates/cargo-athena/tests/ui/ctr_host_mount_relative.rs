// `host_mount` paths must be absolute (k8s rejects relative hostPath /
// mountPath at admission).
#[cargo_athena::container(host_mount = [
    { host_path = "/data", mount_path = "data" },
])]
fn f() {}

fn main() {}
