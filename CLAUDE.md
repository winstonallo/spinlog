# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Musicboxd — a music tracking and rating social app, analogous to Letterboxd but for music.

## Agent workflow

This repo uses an orchestrator/sub-agent pattern. Claude Code acts as the **orchestrating agent**: it breaks work into discrete tasks, spawns sub-agents to execute them, and verifies their outputs before proceeding. Do not do all work in the main context — delegate implementation, testing, and verification to sub-agents.

**Research before implementation.** Before writing any code, spawn research sub-agents to gather all necessary information — explore the codebase, read relevant docs, investigate APIs, identify constraints and edge cases. Only begin implementation once you are confident you have a complete picture. Ambiguity or missing context is a reason to pause, not to guess.

**Plan before coding.** After research, produce a written plan for the feature: what will be built, how it fits into the architecture, what files will change, and how it will be tested. Present the plan to the user and iterate on it until they approve. Do not write implementation code until the plan is signed off.

**Ping the user when input is needed.** If a decision requires user judgment (design choices, conflicting requirements, missing context that research can't resolve), stop and ask before proceeding. Do not make assumptions on the user's behalf for things that matter.

## Platform

Built with **Tauri** (web-first, expandable to desktop/mobile). The frontend runs in the browser via `tauri dev`; native builds come later. There is no deployment workflow yet — add one once the first deployable version exists.

## Testing

Every feature must be covered by tests. Tests must be **breakable** — if you delete or break the feature, the test must fail. Acceptable levels: unit, integration, or e2e. Superficial tests that pass regardless of the feature's correctness are not acceptable.

Every bug fix must include a test that reproduces the bug and fails without the fix. The test proves the bug existed and ensures it cannot regress silently.

## Commands

| Command | Purpose |
|---|---|
| `cargo leptos watch` | Start SSR dev server at http://localhost:9090 with hot-reload |
| `cargo leptos build --release` | Production build (server binary + WASM bundle) |
| `cargo check --features ssr` | Fast SSR type-check (no build output) |
| `cargo check --target wasm32-unknown-unknown --features hydrate` | Fast WASM type-check |
| `npm run tauri dev` | Start Tauri desktop app (runs `cargo leptos watch` internally) |

## Architecture

```
musicboxd/
├── Cargo.toml            # Workspace root + Leptos app package (both in one)
├── src/
│   ├── app.rs            # Leptos components: shell(), App, routes, pages
│   ├── lib.rs            # Crate root: pub mod app + hydrate() WASM entry
│   └── main.rs           # SSR binary entry: Axum server setup
├── public/               # Static assets (copied to target/site/)
└── src-tauri/            # Tauri v2 desktop wrapper (package: musicboxd-tauri)
    ├── src/
    │   ├── main.rs       # Tauri binary entry
    │   └── lib.rs        # tauri::Builder setup
    ├── capabilities/     # Tauri v2 permission definitions
    └── tauri.conf.json   # devUrl → localhost:9090, beforeDevCommand → cargo leptos watch
```

The app is **Leptos 0.7 SSR** with Axum. `cargo leptos watch` compiles two targets simultaneously: a native Rust server binary (SSR, `--features ssr`) and a WASM bundle (client hydration, `--features hydrate`). The server renders HTML on the first request; the WASM bundle hydrates it in the browser. Port 9090. `src-tauri/` is a Tauri v2 wrapper that loads the Leptos server via webview for native builds — not needed for web development.

## Documentation

Keep documentation up to date as you go. When you add or change a feature, update the relevant docs (README, CLAUDE.md, etc.) in the same commit. Do not leave documentation for a later pass.

Function docs should explain the **reasoning** behind a function — why it exists, why it's designed this way, what invariants it assumes — not what it does. The code is the source of truth for what; the doc is the source of truth for why.

Do not add inline comments unless the code is doing something non-obvious that cannot be made clear by restructuring. If a block of code needs explaining, extract it into a well-named function instead of annotating it.

## Skills

Create Claude Code skills (in `.claude/skills/`) for distinct, repeatable aspects of the codebase — e.g. running tests, building, seeding the database, scaffolding a new feature. Each skill should be scoped to one concern and documented with a short description.

Skills are tracked in version control like any other source file. The `.claude/skills/` directory is part of the repo — commit new and updated skills alongside the code changes that motivate them. Do not leave skills outside the repo or in a personal config directory.

## Error handling

Errors must never be silently swallowed. Every error must be handled explicitly and propagated with enough context to identify its origin at every level of the call stack. Use structured error types rather than stringly-typed messages, and add context when wrapping errors as they propagate upward. A log line or a panic is acceptable at a top-level boundary; silently discarding an error is never acceptable.

## Modularity

Keep code modular to minimise merge conflicts when multiple agents work on the codebase simultaneously. Concretely:

- **One module per concern.** Each page, feature, or domain concept belongs in its own file (`src/ratings.rs`, `src/profile.rs`, etc.) rather than piled into a single monolithic file. `app.rs` should only contain the router and top-level shell — not business logic.
- **Small, focused files.** A file that grows beyond ~200 lines is a signal to split it. Extract server fns, components, and types into purpose-named modules as they accumulate.
- **Avoid editing the same file for unrelated changes.** If two features touch different concerns, they must live in different files so agents can work on them in parallel without conflict.
- **Stable public interfaces.** When splitting a module, preserve the public interface so other modules and tests don't need to change. Re-export from the parent module if necessary.

When adding a new feature, create a new module for it rather than appending to an existing file.

## Simplicity

Prefer the simplest solution that fully meets the requirements. Do not add complexity in anticipation of future needs. When a simpler approach requires dropping or limiting functionality, flag the tradeoff explicitly and get approval before proceeding — never silently sacrifice features for the sake of cleaner code.

## Warnings

Zero warnings are tolerated. Compilation must produce no warnings, and `cargo clippy -- -D warnings` must pass at every commit. Fix warnings immediately; do not suppress them with `#[allow(...)]` unless there is a specific, documented reason that fixing the warning is genuinely not possible.

## Code hygiene

Unused code is useless and must be removed. Dead code, unreferenced exports, unused variables, and commented-out blocks all add noise without value. Delete them.

## Branches

Every feature is developed on a dedicated branch, created from `main`. Do not commit feature work directly to `main`.

A feature branch is merged only once its functionality is verified — tests pass, the build succeeds, and the feature behaves as specified. The full merge sequence is:

```
git fetch origin main
git rebase origin/main          # rebase feature branch onto current main
git push origin <branch>        # push rebased branch
git checkout main
git merge --ff-only <branch>    # fast-forward main; never create merge commits
git push origin main
git branch -d <branch>          # delete local branch
git push origin --delete <branch>  # delete remote branch
```

## Commits

Commit regularly. Commits must be **atomic**: each commit must build and pass all tests in isolation. This means `git rebase -i master --exec <build command> --exec <test command>` must always succeed — no commit may leave the repo in a broken state.

If a new change logically belongs to a recent commit, rebase and amend it in rather than creating a separate commit.

**Concurrent agents.** Multiple agents may be working on this codebase simultaneously. Before amending a commit, check whether new commits have landed on top of it. If they have, rebase first, then amend — do not overwrite work that has been added in the meantime.
