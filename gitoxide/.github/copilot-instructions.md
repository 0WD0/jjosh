# Copilot Instructions for Gitoxide

This repository contains `gitoxide` - a pure Rust implementation of Git. This document provides guidance for GitHub Copilot when working with this codebase.

## Project Overview

- **Language**: Rust (MSRV documented in gix/Cargo.toml)
- **Structure**: Cargo workspace with multiple crates (gix-*, gitoxide-core, etc.)
- **Main crates**: `gix` (library entrypoint), `gitoxide` binary (CLI tools: `gix` and `ein`)
- **Purpose**: Provide a high-performance, safe Git implementation with both library and CLI interfaces

## Development Practices

### Test-First Development
- Protect against regression and make implementing features easy
- Use containers to test elaborate user interactions
- Keep it practical - the Rust compiler handles mundane things
- Use git itself as reference implementation; run same tests against git where feasible
- Never use `.unwrap()`, not even in tests. Use `quick_error!()` or `Box<dyn std::error::Error>` instead
- Use `.expect("why")` with context explaining why expectations should hold

### Error Handling
- Handle all errors, never `unwrap()`
- Provide error chains making it easy to understand what went wrong
- Use `thiserror` for libraries generally
- Binaries may use `anyhow::Error` exhaustively (user-facing errors)

### Commit Messages
Follow "purposeful conventional commits" style:
- Use conventional commit prefixes ONLY if message should appear in changelog
- Breaking changes MUST use suffix `!`: `change!:`, `remove!:`, `rename!:`
- Features/fixes visible to users: `feat:`, `fix:`
- Refactors/chores: no prefix (don't affect users)
- Examples:
  - `feat: add Repository::foo() to do great things. (#234)`
  - `fix: don't panic when calling foo() in a bare repository. (#456)`
  - `change!: rename Foo to Bar. (#123)`

### Code Style
- Follow existing patterns in the codebase
- Use `gix_hash::ObjectId` and `gix_hash::oid` to prepare for SHA256 support
- No `.unwrap()` - use `.expect("context")` with clear reasoning
- Prefer references in plumbing crates to avoid expensive clones
- Use `gix_features::threading::*` for interior mutability primitives

### Async Usage
- Provide async clients as opt-in using feature toggles
- Server-side: support async out of the box with conditional compilation
- Use `blocking` to make `Read` and `Iterator` async when needed
- Long-running operations support interruption via `gix_features::interrupt`

### Path Handling
- Paths are byte-oriented in git (even on Windows via MSYS2 abstraction)
- Use `os_str_bytes` to convert git paths to `OsStr`/`Path` or use custom types

## Building and Testing

### Quick Commands
- `just test` - Run all tests, clippy, journey tests, and try building docs
- `just check` - Build all code in suitable configurations
- `just clippy` - Run clippy on all crates
- `cargo test` - Run unit tests only

### Build Variants
- `cargo build --release` - Default build (big but pretty, ~2.5min)
- `cargo build --release --no-default-features --features lean` - Lean build (~1.5min)
- `cargo build --release --no-default-features --features small` - Minimal deps (~46s)

### Test Best Practices
- Run tests before making changes to understand existing issues
- Use `GIX_TEST_IGNORE_ARCHIVES=1` when testing on macOS/Windows
- Journey tests validate CLI behavior end-to-end

## Architecture Decisions

### Plumbing vs Porcelain
- **Plumbing crates**: Low-level, take references, expose mutable parts as arguments
- **Porcelain (gix)**: High-level, convenient, may clone Repository for user convenience
- Platforms: cheap to create, keep reference to Repository
- Caches: more expensive, clone Repository or free of lifetimes

### Options vs Context
- Use `Options` for branching behavior configuration (can be defaulted)
- Use `Context` for data required for operation (cannot be defaulted)

### Default Trait Implementations
- Can change only if effect is contained within caller's process
- Changing default file version is a breaking change

## Crate Organization

### Stability Tiers
1. **Production Grade** (Tier 1-2): `gix-lock`, `gix-tempfile`
2. **Stabilization Candidates**: Feature-complete, need more use before 1.0
3. **Initial Development**: Usable but possibly incomplete
4. **Very Early/Idea**: Minimal implementation or placeholders

### Common Crates
- `gix`: Main library entrypoint (porcelain)
- `gix-object`, `gix-ref`, `gix-config`: Core git data structures
- `gix-odb`, `gix-pack`: Object database and pack handling
- `gix-diff`, `gix-merge`, `gix-status`: Operations
- `gitoxide-core`: Shared CLI functionality

## Documentation
- High-level docs: README.md, CONTRIBUTING.md, DEVELOPMENT.md
- Crate status: crate-status.md
- Stability guide: STABILITY.md
- Always update docs if directly related to code changes

## CI and Releases
- Ubuntu-latest git version is the compatibility target
- `cargo smart-release` for releases (driven by commit messages)
- Split breaking changes into separate commits per affected crate
- First commit: breaking change only; second commit: adaptations

## When Suggesting Changes
1. Understand the plumbing vs porcelain distinction
2. Check existing patterns in similar crates
3. Follow error handling conventions strictly
4. Ensure changes work with feature flags (small, lean, max, max-pure)
5. Consider impact on both library and CLI users
6. Test against real git repositories when possible
