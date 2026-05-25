//! Rooted at `pipeline_secrets`. Golden pins `secret!`/`secret_opt!`
//! emit shape (env entries with `valueFrom.secretKeyRef`) — including
//! fragment-carried secrets (`with_db_creds` pulls db-creds/password
//! onto every container that calls it).

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_secrets);
}
