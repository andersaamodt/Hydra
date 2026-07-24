# Dependency security audit

Audit date: 2026-07-22

Hydra's locked Rust graph was checked against the current RustSec advisory database with `cargo-audit 0.22.2`. The audit found zero known vulnerabilities. The production npm graph was checked with `npm audit --omit=dev`; it found zero vulnerabilities.

RustSec reports one transitive unsoundness warning, `RUSTSEC-2024-0429`, in `glib 0.18.5`. It is present only in the Linux GTK3 graph used by Tauri and concerns `VariantStrIter`; Hydra does not call that API. Tauri's current Linux stack still selects the GTK3 binding family, so Hydra cannot remove or upgrade this package independently without replacing its cross-platform shell. This is an accepted, narrow transitive risk to recheck on every Tauri upgrade, not a suppressed vulnerability.

RustSec also reports unmaintained transitive packages in the GTK3 Linux stack and build-time macro/text-processing dependencies. They are not directly selected by Hydra. The architectural mitigation is to keep all application crates free of unsafe Rust, keep Tauri behind the shell boundary, minimize shell capabilities, and upgrade the shell promptly when its maintained Linux backend changes.

Every first-party Rust crate uses `#![forbid(unsafe_code)]`. The Tauri capability manifest grants only core event handling, deep links, file-open/save dialogs, and the narrowly typed Rust commands exposed by Hydra.
