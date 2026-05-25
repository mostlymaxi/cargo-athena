//! Rooted at `pipeline_fields`. Golden pins the `a.field` emit lowering:
//! `{{=toJSON(fromJSON(tasks['m'].outputs.parameters['return'])['id'])}}`.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_fields);
}
