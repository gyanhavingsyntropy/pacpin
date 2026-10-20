# Verification reports and local evidence

The dated Markdown reports summarize commands, assertions, results and limits.
Machine-specific paths in public reports are anonymized. Raw `*-evidence/`
directories (terminal recordings, screenshots, fixture databases and archives)
remain local and ignored because they may include host details or authentication
test output. A report mentioning a local artifact does not imply it is published.

Reproduce checks with the scripts under `scripts/` in the user-specified
disposable VM. Some existing scripts intentionally target a Garuda live overlay
and refuse other guests; adapt them without bypassing isolation guards.
Authentication fixtures use the standard public live-ISO test account, not
a host credential. Never put private passwords, keys or tokens in fixtures.
