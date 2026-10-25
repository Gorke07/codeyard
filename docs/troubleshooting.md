# Troubleshooting

Short answers to the problems people actually hit. Messages are quoted as the app shows them.

## The app does not open or bounces in the Dock (macOS)

Without Full Disk Access, macOS can block reads of Desktop, Documents and Downloads. At the first launch Codeyard opens a short
dialog that takes you to **System Settings ▸ Privacy & Security ▸ Full Disk Access**; run *Help: Grant Full Disk Access…* to see
it again. Unnotarized builds also need **right-click ▸ Open** on first launch, or `xattr -cr /Applications/Codeyard.app`.

## SSH

| What you see | What it means and what to do |
| --- | --- |
| a prompt for a password, passphrase or host key | `ssh` asked and the app forwarded the question; answer it in the window. Nothing is stored. |
| "no agent binary bundled for `<triple>`" | The server's architecture has no matching `codeyard-agent-<triple>` next to the app. Only Linux x86_64 / aarch64 and macOS arm64 are bundled. Use **Remote: Test SSH Connection…** to see the detected architecture. When running the bare binary, keep its `agents/` folder beside it (or set `CODEYARD_AGENT_DIR`). |
| "could not start ssh" | `ssh` is not on the app's `PATH`. macOS and Linux ship it; on Windows install OpenSSH. |
| *Reconnecting* in the status bar | The connection dropped; the agent keeps your terminals and watches alive for 5 minutes. If it gives up you get the reason and a **Reconnect** button; open files and unsaved edits stay in their tabs. |
| *Permission denied (publickey)* | Add the key to the server (**Remote: Set Up Key Login…**) or point the host at the right `IdentityFile` (**Remote: Edit SSH Host…**). |
| An upload or download stopped | A cancelled or failed transfer only removes what it created. Existing folders are never deleted. |
| `ControlPath too long` | The app keeps its control sockets in `/tmp/cy-<uid>` instead of the home folder; if that directory is unsafe it connects without sharing. |

## Language servers

**New folder, no completions or errors?** Language servers are off in folders you have not trusted yet: they can run the
project's build scripts. Run *Workspace: Trust This Folder* (or start any task and answer the prompt); they start at once.

The status bar says once "`<server>` isn't available" (with an install hint on a remote folder: "on `<host>`") and nothing else breaks. Install it (the table in the user guide's *Language
servers* section has one command per language) and run **Developer: Restart Language Servers**. Servers on a remote folder must be
installed **on the server**. Change or disable one with `lsp.servers`; it is ignored in a folder you have not trusted.

## Schemas, spelling and `.http`

- No JSON/YAML hints: check `json.validate.enable`; for remote or offline machines set a local schema (`$schema` with a relative
  path is read from the workspace host) and `json.schemaDownload.enable: false`. **Schemas: Clear Download Cache** forgets the
  cached copies (kept for 7 days).
- A spell dictionary is missing or unreadable: you get one notice; reinstall it with **Spelling: Install Dictionary…**.
- An `.http` request on `localhost` fails over SSH: the request is sent *from the server*, so `localhost` means the server. Set
  `http.runOn` to `local` to send from your own computer. The message names where the request ran.
- "`curl` is not installed on …": install curl on that machine or switch `http.runOn`.

## GitHub pull requests

`GitHub: Pull Requests…` uses `gh` when it is installed and signed in, otherwise the public API (rate limited, no private
repositories): run `gh auth login` or set `GITHUB_TOKEN`. A checkout that refuses because of local changes tells you to commit or
stash first.

## Reporting a problem

If Codeyard panics it writes a report to the `crashes/` folder next to its state database (see `CODEYARD_STATE_DIR` in
[environment.md](environment.md)); nothing is sent anywhere, attach the file to your issue yourself. Include the version
(**About Codeyard** in the gear menu), your OS and what you did. For vulnerabilities see [SECURITY.md](../SECURITY.md).
