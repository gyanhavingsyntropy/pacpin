# Test VM selection and reference access profiles

## Select the target for the current work

- Use the disposable VM specified by the user. An applicable VM choice already
  given in the session remains valid; do not repeatedly request it.
- If no applicable target is known, ask for the VM identity, distribution and
  access method/details needed for testing. Continue independent code review,
  unit checks and builds while awaiting access. No default distribution,
  hypervisor, VM name or server is implied by this document.
- Verify the selected target is the disposable guest before package operations.
  Keep the host home, package database and caches isolated from test mutations.
  Run pacpin, AUR helpers and makepkg as the regular guest user; elevate only
  native package operations and necessary guest fixture administration.
- Adapt existing fixture scripts to that guest without removing isolation or
  disposable-guest guards. Record identity, distribution, access method, relevant
  versions, commands, results and verification limits. Report unavailable checks
  as incomplete instead of silently switching targets.
- Use a real guest terminal emulator for UI/input/progress checks and inspect
  rendered screen and scrollback. Export evidence before discarding the guest.
- Shut down a VM you started for testing and confirm it has stopped, unless the
  user asks to keep it running. Do not shut down an already-running user VM
  without authorization.

## Access details

Use the VM selected by the user; obtain its access details from that user or
an ignored local profile. Personal VM names, UUIDs, storage paths, ports and
credentials are intentionally excluded from the public repository.

Keep a local access profile in `docs/development/vm.local.md` if needed. Use a
regular guest account, isolated fixture state and a real terminal for UI checks.
Verify the guest identity before package operations, export evidence, then shut
down a VM started for testing unless the user requests otherwise. Do not shut
down an already-running user VM without authorization.
