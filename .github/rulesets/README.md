# Repository rulesets (source of truth)

GitHub does **not** auto-sync these files — they are the reproducible,
reviewable record of the rulesets configured on this repo. Edit the JSON
*and* re-apply with the commands below so the live config and this
directory never drift.

## `protect-main.json` — "protect main"

Branch ruleset on the **default branch** (`~DEFAULT_BRANCH`, currently
`main`). Enforces, for everyone (no bypass actors):

- **PR-only** — no direct pushes to `main`; merges restricted to
  **squash/rebase** (`required_linear_history` + `allowed_merge_methods`,
  matching the repo's merge-commit-off setting).
- **Required status checks** (strict — branch must be up to date):
  `clippy`, `test`, `build`, and the three blocking e2e legs
  `e2e (argo v4.0.5 | v3.7.14 | v3.6.19)`. `integration_id: 15368` pins
  them to the GitHub Actions app.
- `required_approving_review_count: 0` — deliberate for a solo
  maintainer (a value ≥ 1 would block your own PRs, since GitHub forbids
  self-approval; ruleset bypass is all-or-nothing and would also skip
  the CI gate). Non-collaborators still cannot self-merge — that comes
  from repo permissions, not the review count.
- **Restrict deletions** + **block force pushes** on `main`.

### Apply / update

```sh
# create (first time)
gh api --method POST repos/mostlymaxi/cargo-athena/rulesets \
  --input .github/rulesets/protect-main.json

# list to find the id
gh api repos/mostlymaxi/cargo-athena/rulesets \
  -q '.[] | "\(.id)  \(.name)"'

# update in place after editing the JSON
gh api --method PUT repos/mostlymaxi/cargo-athena/rulesets/<id> \
  --input .github/rulesets/protect-main.json
```
