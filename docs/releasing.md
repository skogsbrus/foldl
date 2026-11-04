# Releasing

`foldl-core` and `foldl` are published to crates.io together, at the same
version, by the [`publish`](../.github/workflows/publish.yml) workflow.
Only the repository owner runs it, by hand, on `main`.

## Steps

1. In a pull request, bump the version in both places:
   - `version` under `[workspace.package]` in `Cargo.toml`
   - the `foldl-core` dependency's `version` in `crates/foldl/Cargo.toml`

   If dependencies changed, run `scripts/licenses` so that
   `THIRD_PARTY_LICENSES.md` matches `Cargo.lock`; CI checks it.
2. Merge it once CI passes.
3. Under **Actions → publish → Run workflow**, choose `main` and run it.

The workflow:

1. Stops unless it runs on `main` and was started by the owner.
2. Stops if the two versions differ, if `v<version>` is already tagged,
   or if either crate already has that version on crates.io. A version
   can be published only once.
3. Builds foldl and checks that its dependencies' licenses are allowed
   and listed.
4. Runs `cargo publish --workspace`, `foldl-core` first.
5. Tags the commit `v<version>` and pushes the tag.

If publishing fails after `foldl-core` is out, fix the cause and rerun:
step 2 then reports `foldl-core` as published. Publish `foldl` by hand
with `cargo publish -p foldl`, then push the tag.

## The `Cargo publish` environment

The token, `CARGO_REGISTRY_TOKEN`, is a secret of the `Cargo publish`
environment, so only jobs that name the environment can read it. The
workflow's own checks live in the workflow file, which anyone who can push
a branch could change on that branch. The environment's settings are what
hold regardless:

- **Deployment branches:** `main` only.
- **Required reviewers:** the owner. Each run then waits for an approval
  before it can read the token.

Use a crates.io token scoped to publishing `foldl` and `foldl-core` only,
and rotate it if it may have been seen.
