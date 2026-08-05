# Agent Guidelines

Shared Yazelix workflow and release policy live in the main repository:

- https://github.com/luccahuguet/yazelix/blob/main/AGENTS.md
- In sibling local checkouts, read `../yazelix/AGENTS.md` first.

Only Yazelix Helix-specific guidance belongs here.

## Local Scope

- Keep the downstream delta small and reviewable.
- Prefer Steel plugins for editor behavior and Rust only for capabilities that
  Steel cannot provide.
- Keep standalone Helix usable without Yazelix.
- Do not put Zellij, Yazi, or main Yazelix runtime policy in this repository.

## Verification

- `cargo fmt --all -- --check`
- `cargo check -p helix-term`
- Run focused tests for each retained native seam or Steel plugin.
- Verify the Nix package after its downstream packaging is restored.

## Integration

Main Yazelix owns managed-session policy and consumes released commits from
this repository. Publish and verify this child before updating the main lock.
