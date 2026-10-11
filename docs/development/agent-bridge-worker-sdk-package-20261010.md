# Tasker TypeScript worker SDK packaging alignment (maintained fork)

This change applies to the maintained `Weintraub-B/tasker-core` fork, not the upstream Tasker release. It updates generated SDK packaging files without changing worker runtime logic.

## Problem reproduced

The checked-in `crates/tasker-ts/package.json` declares `@tasker-systems/tasker@0.1.10`, but its `package-lock.json` still declared the obsolete `@tasker-systems/worker@0.1.0` package with legacy `ffi-napi` and `ref-napi` dependencies. A clean NPM install against the declared package was not reproducibly locked.

The checked-in NAPI-generated `crates/tasker-ts/index.d.ts` also omitted `BootstrapConfig.completionTimeoutMs`, even though the authoritative Rust `src-rust/bridge.rs` includes `completion_timeout_ms: Option<u32>` and the TypeScript bootstrap code passes that property. The TypeScript ESM build succeeded, but declaration generation failed with TS2339.

## Source-aligned regeneration

From `crates/tasker-ts/`:

```bash
npm install --package-lock-only --ignore-scripts --offline --no-audit --no-fund
npm ci --ignore-scripts --no-audit --no-fund

# In an environment with cc but without repository-configured clang/mold:
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=cc \
CARGO_NET_OFFLINE=true CARGO_BUILD_JOBS=2 RUSTFLAGS='-C linker=cc' \
npm run build:napi

npm run build
npm run typecheck
npm pack --dry-run --json --ignore-scripts
```

The native invocation above was also configured with a shared local `CARGO_TARGET_DIR`; do not commit host-specific cargo flags or linker paths. `index.js` and `index.d.ts` are NAPI-generated files and are kept together. Do not hand-edit them to bypass the native contract.

## Acceptance evidence (2026-10-10 PDT)

- Clean locked npm install: **PASS**, with `--ignore-scripts`, no npm audit/network application hooks
- `napi build --platform`: **PASS** from maintained-fork Rust source, Linux x64, local `cc` override
- Rust-generated `index.d.ts` now includes `BootstrapConfig.completionTimeoutMs`
- TypeScript `npm run build` and `npm run typecheck`: **PASS**
- Node direct native import smoke (no worker started): `getVersion() === "0.1.8"`, `healthCheck() === true`, **28 exports**
- npm pack dry-run: `@tasker-systems/tasker@0.1.10`, **13 files**, including `dist/index.js`, `dist/index.d.ts` and `tasker_ts.linux-x64-gnu.node`
- Compiled native addon SHA-256 (pre-merge working tree): `75c22982ae11dc3373775854b38f083d09bd79873dfa1cd7366ed325d26b1b2e`
- Rust crate version (`0.1.8`) differs from npm SDK release package version (`0.1.10`); do not falsely label them identical
- `npm ci --offline` initially failed because a locked tarball was not cached; a regular integrity-checked `npm ci` succeeded

## Release limits

This source/packaging repair is not publication and not a live service upgrade. There is **no proof of deterministic binary bit-for-bit reproducibility across hosts**, cryptographic CI provenance, independently reviewed release, complete Tasker Rust suite, or full live rollback rehearsal. Existing server-only Tasker release candidate `tasker-native-recovery-612a04fb-rc1` must not be represented as a full server+worker package.

After merging the maintained fork, a **new candidate** may pair an exact fork main commit, server binary hash, NAPI package hashes, and worker bundle source-commit receipt. It must be revalidated with the Bridge read-only preflight. Full production cutover remains a separate guarded operation requiring explicit approval.
