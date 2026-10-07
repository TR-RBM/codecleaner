# Security policy

## What codecleanup does to your files

codecleanup rewrites the source files under `--in` in place and writes Markdown files and an index under `--docs`. It reads language plugins from `~/.config/codecleanup/plugins/` and from the folder given with `--plugins`. It does nothing else: no network, no other files.

## What counts as a vulnerability

A vulnerability is a case where the tool does more than that, or does it in a way the documentation rules out. Examples:

- crafted content in a source file, a doc or an index makes the tool write, replace or delete a file outside of `--in` and `--docs`;
- a restore reads a file outside of the docs location;
- a WebAssembly plugin reaches anything other than the bytes it is handed: files, the network, the environment, or memory of the host;
- crafted input makes the tool run code, or corrupts its memory.

These are known limits, not vulnerabilities:

- **Plugins are trusted to classify correctly.** The tool replaces what a plugin calls a comment. A wrong or malicious plugin can therefore destroy code in the files you run the tool on. Install only plugins you trust.
- **A plugin can run forever.** WebAssembly plugins have no time limit.
- **A restore inserts what the docs contain.** Whoever can write to your docs folder can put text of their choice into your source files with the next `--restore`. Keep the docs folder as protected as the source.
- **Lexers are approximations.** A language construct that is classified wrongly is a bug, and a normal public issue is the right place for it, even though the result can be lost code.
- **Release files are not signed.** The release carries a `SHA256SUMS` file, which detects a damaged download but not a replaced release.

If you are unsure, report it.

## How to report

Report privately. Do not open a public issue, and do not publish details, before a fix is available.

Send the report by e-mail to the maintainer, Tim Richter, at `info@richter-it-service.eu`. Include:

- the version (`codecleanup --version`) and how it was installed;
- the operating system and its version;
- what you did, what you expected and what happened, with the exact command and the smallest input that shows it;
- any plugin involved that is not built in;
- whether the problem is already known to others.

Do not include real credentials or private data.

## What happens next

The maintainer confirms that the report arrived, examines it, and tells you whether it is regarded as a vulnerability and why. A confirmed vulnerability is fixed in a new release, and the notes of that release describe it and, if you wish, name you. Please allow time for a fix before you publish.

You get an answer within 72 hours of your report. That answer confirms that the report arrived and says what happens next; it is not a promise that a fix exists by then.

## Supported versions

Fixes are made on the current release. Earlier releases receive no fixes.
