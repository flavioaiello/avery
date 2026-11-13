# AGENTS.md

## Priority

Security-first. Correctness-first. Simplicity-first.

These rules are constraints, not preferences. When uncertain, preserve system invariants over convenience, abstraction, or throughput. Apply them throughout the repository; only explicitly stated exceptions apply.

## Authority And Ownership

Authority must remain local, explicit, bounded, transferable, revocable, and observable. Define each subsystem's authority, owned state and resources, authorized operations, and transfer, revocation, and cleanup boundaries explicitly. Owning a resource does not confer authority over another subsystem.

Never introduce hidden centralized coordination, singleton services, globally serialized paths, globally authoritative mutable state, retry ownership, cache accumulation, or stateful recovery.

Prefer choreography over orchestration, deletion over addition, deterministic behavior over convenience, bounded resources over peak throughput, explicit ownership over shared mutable state, and compiler-enforced invariants over comments.

Do not generalize before multiple concrete call sites exist. Do not add speculative framework layers or split cohesive runtime logic without a real ownership, feature, platform, or crate boundary.

## Mandatory Refactors

Every codebase change, including fixes and features, must integrate the requested behavior directly into the affected design and remove what it supersedes.

No compatibility code or shims: no legacy fallbacks, deprecated aliases, compatibility wrappers, version bridges, historical-format readers or writers, or parallel old/new implementations retained for compatibility.

Update all affected callers, implementations, contracts, tests, fixtures, configuration, and documentation together. Delete obsolete code and dependencies.

Permit breaking changes within scope while preserving product invariants, required external semantics, and security properties. Implement required external standards directly through the current design; required native protocol behavior is not a reason to retain a superseded implementation.

Reject unsupported wire and storage versions without fallback, mutation, or silent reinterpretation. Preserve incompatible durable data; refactoring never authorizes resetting or deleting it.

Identify deployment and data-transition requirements explicitly, including affected participants and durable formats, required sequencing, and operator actions. Do not hide transitions in fallback or recovery paths.

Choose the smallest complete refactor. Keep diffs local, reversible, and operationally justified; avoid unrelated restructuring. Reversibility does not require retaining legacy implementations.

## Failure And Security

Assume crash-stop failures, duplicate, delayed, or replayed delivery, stale reads, partial visibility or I/O, deserialization failure, cancellation at every async boundary, and adversarial input.

Maintain zero trust: explicit authorization only. Never introduce unauthenticated communication paths.

All protocol behavior must be deterministic, replay-safe, bounded in memory, and bounded in time. Separate safety guarantees from progress assumptions: safety must hold through failures; any conditions needed for successful progress must be explicit and must not become safety prerequisites. Bounded local completion may be rejection, timeout, or cancellation; it does not promise successful delivery.

Use local monotonic elapsed time for operation deadlines and resource lifetimes; expiry bounds local work and never grants authority.

Resource exhaustion is a correctness failure. Every queue, cache, retry loop, channel, parser, allocator path, and peer-derived allocation must have explicit bounds per operation and in aggregate across concurrent operations and retained state. Bound memory, work, elapsed time, and resource counts at their owning subsystem and process. Enforce bounds before allocation or admission, define behavior at capacity, and release resources on completion, failure, and cancellation. Individually bounded entries do not justify unbounded totals or any prohibited retry ownership, cache accumulation, or recovery state.

Treat async boundaries, cancellation, retries, backpressure, and queue limits as correctness boundaries.

For security or protocol changes, explicitly reason about malformed input, replay, nonce/timestamp validation, resource exhaustion, cancellation safety, races, cryptographic misuse, domain separation, and deterministic behavior.

## Protocols And Serialization

Wire formats must be explicit, versioned, bounded, deterministic, and replay-safe. Identify whether a format is repository-owned or a required native external protocol; storage formats owned by this repository follow the same rules. For every format, use explicit codecs, cap peer-derived allocations, handle unknown fields explicitly according to the format's contract, and test parser failure paths.

Repository-owned formats require length-prefixed framing and canonical encodings.

Implement native external protocols using their required framing, encoding, version identification or negotiation, and field semantics directly. Do not prepend repository-specific lengths or versions or impose a different encoding on a native protocol. Keep repository-owned envelopes and payloads subject to the repository-owned format rules. Native protocol conformance does not exempt authentication, authorization, replay safety, determinism, or resource bounds, or authorize compatibility implementations.

Never use unbounded decode allocation, reflection-driven serialization, parser side effects, implicit schema evolution, or panics on deserialization failure.

## Rust Style

Match existing repository idioms. Prefer straightforward Rust, cohesive files, narrow visibility, domain newtypes, typestate, enum state machines, exhaustive matches, actor ownership, capability tokens, parse-don't-validate boundaries, sealed invariant-sensitive traits, stable facades, and typed errors.

Avoid hidden control flow, implicit invariants, shared mutable state, broad traits, speculative generics, cross-module symbolic indirection, Arc<Mutex<_>> sprawl, unnecessary macros, boolean state combinations, raw primitive identifiers, repeated downstream validation, and panic paths in services or protocols.

Public APIs must be minimal, explicit, domain-oriented, and difficult to misuse.

## Module Layout

Keep lib.rs as the crate surface. Never use mod.rs; a subsystem that outgrows one file becomes config.rs beside config/. Enforce rejection of every mod.rs under src/ in build.rs. The sole layout exception is tests/common/mod.rs, so shared integration-test helpers are not compiled as their own test target; it grants no lint or safety exemption.

The module root carries the subsystem facade, not just a module manifest: mod declarations, reviewed re-exports, and public caller-facing types, together with their owned state and invariants. Keep internal files under config/ with the narrowest visibility that compiles. Descendants can read private ancestor items, so facade state needs no pub(super). When the inherent method surface outgrows the root, move it to config/api.rs; keep the reviewed surface and owned state in the root.

Organize tests by scope. Keep unit tests for an individual implementation file at the bottom of that file in a #[cfg(test)] mod tests module. Keep white-box tests of interactions across a subsystem in that subsystem's tests.rs, such as config/tests.rs, declared from the subsystem root with #[cfg(test)] mod tests;. Keep integration and smoke tests under tests/, exercising public interfaces. Keep actors, messages, facades, errors, and domain types together when they evolve together.

Prefer cohesive subsystem files such as config.rs over fragmented symbolic layouts such as config/error.rs, config/msg.rs, config/actor.rs, config/client.rs, and config/types.rs.

## Source Comments And Documentation

Unless explicitly requested, do not add inline comments, rustdoc, or explanatory prose in source files. Without such a request, comments are limited to section markers, generated-code notices, and explicit override markers. An override marker does not itself authorize an exception.

This source restriction does not prohibit required documentation updates or change, security, deployment, data-transition, and validation reports. Keep those outside source comments unless source documentation was explicitly requested.

## Dependencies, Lints, And Unsafe Code

Before adding dependencies, verify maintenance, transitive tree, license compatibility, operational necessity, and whether stdlib or a narrower crate is enough. Do not add dependencies for trivial helpers, cosmetic APIs, single-use abstractions, type aliases, or convenience wrappers. Proc macros require explicit justification.

Configure lints only in Cargo.toml [lints], including its Rust and Clippy subtables. Do not duplicate source-level lint gates, weaken lint levels, add broad #[allow(...)], or bypass failures mechanically. Prefer deleting dead code, reducing visibility, domain types, and simpler control flow.

Minimum enforced lints: dead_code, unused_imports, unused_variables, unused_mut, unused_must_use, unreachable_pub, unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo, clippy::dbg_macro, clippy::unimplemented.

Use targeted cast warnings. Do not enable clippy::pedantic or full clippy::restriction.

Unsafe Rust must be narrow, explicit, and covered by targeted regression tests. This requirement does not authorize relaxing unsafe_code or adding source-level lint overrides. Tests and test helpers have no automatic lint or unsafe-code exemption; existing overrides do not authorize new or broader exceptions.

## Execution And Validation

Before modifying code, verify targets, inspect surrounding invariants and analogous paths, and understand failure/recovery behavior.

If tests or tooling fail, inspect root cause. Do not blind-retry, mutate unrelated code, manipulate tests, suppress assertions, weaken harnesses, fake completion states, overwrite user changes, or expand scope.

For protocol, runtime, storage, networking, concurrency, parser, serialization, or security-sensitive changes, add/update tests in the same change. Cover regressions, parser/serialization fuzzing where practical, concurrency stress where relevant, cancellation safety, and bounded resource behavior.

Validation scales with risk in addition to, not instead of, these required commands:

```
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo check --all-targets --all-features
```

For untrusted-input or network-facing systems, also run:

```
cargo audit
cargo deny check
```

Report checks actually run, their outcomes, and any blockers or checks not completed. Do not claim validation that did not run or pass.
