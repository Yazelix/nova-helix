# Nova Helix Fork

This fork tracks Helix's Steel-enabled line. Native changes are limited to
capabilities unavailable to Steel; editor behavior belongs in Steel plugins
when its APIs can satisfy the contract.

`packages.<system>.yazelix_helix_steel` installs the isolated
`yazelix/bridge-actions.scm` and `yazelix/bridge.scm` modules. The bridge
validates `helix.open_files` and `helix.open_directory` payloads before opening
files or a directory picker in the current instance. The managed workspace and
picker root remain distinct.

The `yazelix/transport` built-in module provides the native mechanism Steel
cannot own safely. `(transport-start token handler)` binds an OS-assigned
`127.0.0.1` port, `(transport-local-addr server)` returns that endpoint, and
`(transport-stop! server)` interrupts any active connection and joins the
listener. Each connection carries one newline-terminated schema-2 JSON request
and response, limited to 64 KiB and authenticated before handoff to the Steel
handler on Helix's editor thread. The handler receives only `request_id`,
`action`, and `payload`; instance selection, token and endpoint publication,
registry state, and managed-session policy remain outside this fork.
`(yzx-helix-start token)` composes that transport with the Steel action handler
and returns the caller-owned server object.

[Steel plugin integration notes](docs/steel-plugin-integration.md) record the
plugin APIs that worked and the lifecycle gaps that kept the transport in Rust.

The `Yazelix Steel Bridge` workflow runs one read-only Ubuntu job on `main`
branch pushes or manual dispatch. It checks lockfile-consistent Steel-enabled
compilation, native transport behavior, filesystem-backed Steel composition,
optional-feature compilation, and formatting. Superseded runs are cancelled,
the job has a 30-minute timeout, and it publishes no artifacts or grammar cache.

The standalone flake pins Nixpkgs at `f830e6112b`, the upstream change that
uses crates.io's static CDN. Nova consumers make this input follow Nova's root
Nixpkgs instead.

## Grammar source delivery

`HELIX-GRAMMAR-SOURCES-001`: Nix evaluation and packaged grammar builds do not
contact Codeberg or SourceHut. `languages.toml` owns grammar URLs, revisions, and subpaths;
`grammar-sources/<revision>.tar.gz` contains each selected source tree,
unmodified, with its upstream license notices. `hashes.json` records the original
Nix source NAR hashes. A fixed-output derivation unpacks and verifies each local
archive at build time. Other hosts retain the upstream fetch path, and standalone
`hx --grammar fetch` retains upstream behavior.

The ten archives occupy 1,111,782 bytes compressed. They contain third-party
sources, not maintained parser changes, and are excluded from the Rust source
build. This replaces evaluation-time Codeberg and SourceHut Git fetches from upstream Steel
baseline `5a8635be`; fixed-output network fetchers alone would still require a
working source server on a cold build.

When changing a bundled revision, fetch the exact upstream Git tree, archive it
under a single `source/` directory, and record `nix hash path` for that tree in
`grammar-sources/hashes.json`. Preserve every file and executable bit, including
licenses, and remove snapshots no longer referenced by `languages.toml`. Use
deterministic archive metadata (zero timestamps and numeric owners). A missing
snapshot fails evaluation instead of falling back to the network. These local
snapshots can be removed when upstream provides an immutable source bundle that
satisfies the same contract.

Run `nix build .#checks.x86_64-linux.codeberg_grammars` (or
`nix build .#checks.aarch64-darwin.codeberg_grammars` on macOS). The check rejects
Codeberg and SourceHut network fetches during evaluation and compiles every bundled
grammar through the production builder, including grammars with source subpaths.
Nova runs this check in its existing Linux and Darwin jobs.

## Downstream LOC Scorecard

Measured against upstream Steel tip `5a8635be` and excluding documentation:

| Surface | Added | Removed | Net |
| --- | ---: | ---: | ---: |
| Config-directory runtime seam | 19 | 0 | +19 |
| CLI completions | 12 | 4 | +8 |
| Nix package export | 1 | 0 | +1 |
| Steel bridge actions | 15 | 0 | +15 |
| Steel request composition | 39 | 0 | +39 |
| Steel plugin package | 6 | 0 | +6 |
| Steel integration test | 68 | 0 | +68 |
| Native transport seam | 384 | 0 | +384 |
| Native transport tests | 167 | 0 | +167 |
| Focused CI workflow | 47 | 0 | +47 |
| Steel component layering | 161 | 50 | +111 |
| Pinned grammar source delivery and check | 40 | 1 | +39 |
| **Total** | **959** | **55** | **+904** |

The source-delivery row includes 12 JSON manifest lines. The ten compressed
third-party source archives are counted separately above, not as owned LOC.

## Upstream Helix README

<div align="center">

<h1>
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="logo_dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="logo_light.svg">
  <img alt="Helix" height="128" src="logo_light.svg">
</picture>
</h1>

[![Build status](https://github.com/helix-editor/helix/actions/workflows/build.yml/badge.svg)](https://github.com/helix-editor/helix/actions)
[![GitHub Release](https://img.shields.io/github/v/release/helix-editor/helix)](https://github.com/helix-editor/helix/releases/latest)
[![Documentation](https://shields.io/badge/-documentation-452859)](https://docs.helix-editor.com/)
[![GitHub contributors](https://img.shields.io/github/contributors/helix-editor/helix)](https://github.com/helix-editor/helix/graphs/contributors)
[![Matrix Space](https://img.shields.io/matrix/helix-community:matrix.org)](https://matrix.to/#/#helix-community:matrix.org)

</div>

![Screenshot](./screenshot.png)

A [Kakoune](https://github.com/mawww/kakoune) / [Neovim](https://github.com/neovim/neovim) inspired editor, written in Rust.

The editing model is very heavily based on Kakoune; during development I found
myself agreeing with most of Kakoune's design decisions.

For more information, see the [website](https://helix-editor.com) or
[documentation](https://docs.helix-editor.com/).

All shortcuts/keymaps can be found [in the documentation on the website](https://docs.helix-editor.com/keymap.html).

[Troubleshooting](https://github.com/helix-editor/helix/wiki/Troubleshooting)

# Features

- Vim-like modal editing
- Multiple selections
- Built-in language server support
- Smart, incremental syntax highlighting and code editing via tree-sitter

Although it's primarily a terminal-based editor, I am interested in exploring
a custom renderer (similar to Emacs) using wgpu.

Note: Only certain languages have indentation definitions at the moment. Check
`runtime/queries/<lang>/` for `indents.scm`.

# Installation

[Installation documentation](https://docs.helix-editor.com/install.html).

[![Packaging status](https://repology.org/badge/vertical-allrepos/helix-editor.svg?exclude_unsupported=1)](https://repology.org/project/helix-editor/versions)

# Contributing

Contributing guidelines can be found [here](./docs/CONTRIBUTING.md).

# Getting help

Your question might already be answered on the [FAQ](https://github.com/helix-editor/helix/wiki/FAQ).

Discuss the project on the community [Matrix Space](https://matrix.to/#/#helix-community:matrix.org) (make sure to join `#helix-editor:matrix.org` if you're on a client that doesn't support Matrix Spaces yet).

# Credits

Thanks to [@jakenvac](https://github.com/jakenvac) for designing the logo!
