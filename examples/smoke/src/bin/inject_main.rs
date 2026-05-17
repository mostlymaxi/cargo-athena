//! Rooted at `pipeline_inject`. Golden pins attribute param injection:
//! a struct field (`m.id`) lowered into the container `image` via
//! `{{=fromJSON(inputs.parameters['m'])['id']}}`.

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_smoke::pipeline_inject>();
}
