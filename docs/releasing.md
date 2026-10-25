# Releasing

The `release.yml` workflow builds and publishes a release when a `vX.Y.Z` tag is pushed. It runs four jobs:

1. `verify` checks that the tag, the Cargo version and the changelog agree, then runs `arch-check`, `docs-check` and `licenses.sh`.
2. `macos` builds the SSH agents, the app, the DMG and the tar.gz, and uploads them with the release notes as artifacts.
3. `windows` builds the ZIP and the setup EXE from those agents and uploads them as an artifact.
4. `publish` merges the checksums, verifies every file against them and creates the release in one step.

You can run the whole workflow without a tag (Actions, release, Run workflow): it builds and checks everything, keeps the files
as artifacts and creates no release. Do that before the first tag of a new platform or a changed script.

Nothing is published before all of them succeed, so a failed Windows build leaves no half release: fix it and push the tag
again (delete the tag first with `git push origin :refs/tags/vX.Y.Z`; no release exists yet to remove). The steps below are
the same procedure by hand, for when the workflow cannot be used.

## Steps

1. Run all gates (`scripts/check.sh --strict --budgets`, see `testing.md`), including `scripts/docs-check.sh` (it fails if the changelog has no section for the new version).
2. Bump the version: `version` in the root `Cargo.toml` and every path dependency's `version = "x.y.z"` in
   `crates/*/Cargo.toml` (`cargo-deny` rejects wildcard dependencies):
   `sed -i '' 's/version = "OLD", path/version = "NEW", path/' crates/*/Cargo.toml` (macOS `sed`; GNU `sed` takes `-i` without
   the `''`).
3. Add a `## [x.y.z] - date` section to `CHANGELOG.md` (move the "Unreleased" entries into it).
4. Commit, tag `vX.Y.Z` (annotated), push `main` and the tag.
5. Build the agents:
   `cargo zigbuild --profile release-agent -p agent --target x86_64-unknown-linux-musl` (and `aarch64-…`), plus
   `cargo build --profile release-agent -p agent` for macOS; copy them to `agents/codeyard-agent-<triple>`.
6. `AGENTS_DIR="$PWD/agents" scripts/bundle-macos.sh` builds the app, the bundle and the DMG.
7. Collect `dist/`: `Codeyard-X.Y.Z-macos-arm64.dmg`, `codeyard-X.Y.Z-macos-arm64.tar.gz` (binary, `agents/` and `licenses/`) and
   `SHA256SUMS.txt`.
8. Release notes: the CHANGELOG section plus the install paragraph (the app is not notarized yet: right-click →
   Open, or `xattr -cr /Applications/Codeyard.app`).
9. `gh release create vX.Y.Z dist/* --title "vX.Y.Z" --notes-file notes.md --verify-tag`. The title style is `v0.1.0`.
10. Delete the untracked `agents/`, `dist/` and `notes.md`.
11. **Windows files.** The Windows ZIP and setup EXE are only built by the `windows` job of `release.yml`, which needs a Windows
    machine. The macOS steps above publish **no** Windows assets, so without the workflow the README's Windows download
    links point at an older release or at nothing. Either run the job's steps on a Windows PC
    (copy `agents/` there, `$env:AGENTS_DIR = (Resolve-Path agents).Path; ./scripts/bundle-windows.ps1`, then
    `gh release upload vX.Y.Z target/windows/*.zip target/windows/*-setup.exe` plus a `SHA256SUMS-windows.txt`), or say in the
    release notes that this version has no Windows build.

## Licence texts

Each release artifact ships the files listed in `scripts/licenses.sh` (the project licences, `THIRD_PARTY_NOTICES.md`,
`THIRD_PARTY_CRATES.md` and the licences of icons, grammars, dictionary and vendored crates): in `Contents/Resources/licenses/`
and the DMG's `Licenses/` on macOS, and in a `licenses/` folder in the tar.gz, the Windows ZIP and the installer. The script
fails when a file is missing or the Windows script or installer lacks one. After a dependency change run
`scripts/crate-licenses.py` to refresh `THIRD_PARTY_CRATES.md`.

## Windows installer

`installer/windows/codeyard.iss` (Inno Setup) installs per user without admin rights, optionally adds PATH, a desktop
shortcut and "Open with Codeyard". `scripts/bundle-windows.ps1` builds the setup executable when Inno Setup is
installed. It has not been run on Windows yet.
