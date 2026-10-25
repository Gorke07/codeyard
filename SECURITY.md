# Security policy

## Supported versions

Only the latest release receives security fixes.

## Reporting a vulnerability

Please do **not** open a public issue. Use GitHub's private reporting:
<https://github.com/doancan/codeyard/security/advisories/new>. Include the version, what you did, what you expected and what
happened; a minimal file or configuration that reproduces it helps most.

You can expect an acknowledgement within a few days and a fix or a clear answer as soon as the problem is understood.

## What counts

Codeyard runs programs on your behalf (terminals, tasks, language servers, debuggers, `ssh`, `git`, `curl`), so the areas that
matter most are:

- opening an untrusted folder or file causing code to run without the Workspace Trust prompt;
- the SSH agent bridge, askpass socket, file transfer and `~/.ssh/config` editing;
- the `.http` client sending files or environment variables the user did not approve;
- schema, spell and parser code handling hostile input (crashes, unbounded memory or time).

## Design facts a reviewer may want

- No telemetry and no accounts. Network requests happen only when you start them (see
  [architecture](docs/architecture.md#stack)).
- Passwords and passphrases for `ssh` are asked in the window through a private Unix socket and are never written to disk, the
  environment or process arguments ([remote.md](docs/remote.md#authentication-prompts)).
- Until a folder is trusted: project settings cannot change `terminal.shell`, `lsp.servers` or schema associations, language
  servers do not start, schema downloads are limited to SchemaStore and `git status` runs with `core.fsmonitor` off
  (details: [architecture](docs/architecture.md#workspace-trust-in-one-place)).

## Updates and signatures

Every release has `SHA256SUMS.txt` (the SHA-256 of every other asset, including `RELEASE_NOTES.md`) and `SHA256SUMS.txt.sig`, an
Ed25519 signature over that file. The in-app updater accepts nothing that this signature does not cover. The public key is
`scripts/update-public-key.pem` (hex `47bc1afc0c2c70a4a72ffd0b5ba535397bd9ca83551dfc30a12d64d7cf96d67c`). To check a download, run
`scripts/verify-release.sh <tag>`; the steps are in [docs/releasing.md](docs/releasing.md#verify-a-download). If you suspect the
signing key is compromised, or find a release whose signature does not verify, report it privately as described above and do not
install it.
