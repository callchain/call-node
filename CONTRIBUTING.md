# Contributing to call-node

Thank you for your interest in contributing to Callchain!

## Getting Started

1. Fork the repository and create a branch from `dev`
2. Make your changes
3. Run `cargo test --workspace` and `cargo clippy --workspace -- -D warnings`
4. Commit with a clear message and submit a pull request to `dev`

## Commit Messages

We follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add shielded transaction verification
fix: resolve consensus timeout on validator rotation
docs: update RPC method documentation
test: add integration test for payment flow
refactor: simplify balance state management
```

## Code Style

- `cargo fmt --all` — format all code before committing
- `cargo clippy --workspace` — fix all clippy warnings
- No `unwrap()` in production code — use proper error handling
- No `println!` in production code — use `tracing::info!`/`tracing::warn!`

## Pull Requests

- One logical change per PR
- Include tests for new functionality
- Update documentation if behavior changes
- All CI checks must pass

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
