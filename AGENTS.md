# AGENTS.md — Coding agent guidance for tarzan-rs

## Before declaring a task done

Run every step. CI runs the same checks; skipping them will fail CI.

1. `cargo fmt` — format the code. (`cargo fmt --check` is the CI gate.)
2. `cargo build` — must succeed.
3. `cargo test` — all tests must pass.
4. `cargo clippy --all-targets -- -D warnings` — no lints, no warnings.

## Style and tooling

- Follow `rustfmt` defaults; do not add `rustfmt.toml` overrides without
  discussion.
- When removing functionality, delete the code entirely. Do not comment it out.
- When using rust's string formatting, prefer to directly capture the argument
  (e.g. use `println!("{variable}");` over `println!("{}", variable);`).
- Warnings are errors (`-D warnings`). Fix all warnings before finishing.
- Use `tracing` (not `log`) for instrumentation. The workspace already depends
  on `tracing` and `tracing-subscriber`; do not add `env_logger` to new crates.
- Use `clap` in derive mode for CLI tooling.
- Prefer workspace-level dependency versions declared in the root `Cargo.toml`
  `[workspace.dependencies]` table rather than pinning versions in leaf crates.
- The workspace uses `resolver = "3"`. Keep feature unification in mind when
  adding dependencies.
- Favor using best practices for maintainability (e.g. minimize redundancy, use
  `#[expect(xyz)]` instead of `#[allow(xyz)]` including a comment about why).
  Features from the most recent rust version can be used.
- New logic should have unit tests in the same file (`#[cfg(test)]` module) or
  in a `tests/` directory.
- commits should follow the Conventional Commits specification. Include your model
  as the last item in the subject line in parenthesis.
