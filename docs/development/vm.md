# Disposable VM selection and access

Use the test VM specified by the user.

## Access details

Use the VM selected by the user; obtain its access details from that user or
an ignored local profile. Personal VM names, UUIDs, storage paths, ports and
credentials are intentionally excluded from the public repository.

Keep a local access profile in `docs/development/vm.local.md` if needed. Use a
regular guest account, isolated fixture state and a real terminal for UI checks.
Verify the guest identity before package operations, export evidence, then shut
down a VM started for testing unless the user requests otherwise. Do not shut
down an already-running user VM without authorization.
