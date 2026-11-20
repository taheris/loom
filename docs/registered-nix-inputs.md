# Registered but physically absent Nix inputs

## Fresh reproduction

A disposable local Nix store reproduces the reported symptom without a worker,
network access, builds, privileged tracing, or access to any service store:

```bash
bash tests/agent/private-fixture-inputs.sh nix/retain-fixture-inputs.sh
```

The test registers a tiny `die-hook` input in one database, then loads that
registration into a second database sharing the same physical fixture store.
Without roots in the second database, its garbage collector unlinks the input.
The first database still accepts both `--check-validity` and `--realise`, even
though the file remains absent. Registration and ordinary realization therefore
do not establish physical availability.

A fresh Darwin syscall trace confirmed the deletion: the second database's
`nix-store --gc` issued a successful `unlinkat` against the shared store's
`die-hook`. The historical worker's exact triggering command is unknown; this
reproduction establishes a current destructive mechanism, not retrospective
attribution to an unavailable process.

## Repair and controls

Private fixture registration now roots every imported input in that fixture's
own database before launching tests. The shared loader is used by native and
packaged test campaigns and assembled-system fixture preparation. Diverted SDK
stores retain canonical logical identities while copying inputs to their owned
physical store; rooting never replaces that isolation. The loader rejects
physically absent imports and verifies actual stored bytes without acquiring or
repairing inputs. Roots last only as long as the private fixture state; no
service-store roots, credentials, confinement settings or installed-store
approval are changed.

The regression repeats the same experiment through the real loader in shared and
independently copied physical stores: GC and ordinary explicit deletion cannot
remove imported inputs, newly created private outputs remain collectible, and
removing private fixture state leaves the source input intact. A second negative
control refuses the already-absent registered input. The existing
`private-fixture-nix-policy` check runs this regression with actual Nix; wrapper
conformance covers both campaign launchers.

SDK readiness also checks physical path presence, in addition to database
registration. Its CLI regression rejects a registered-but-absent SDK without
acquisition. This is a bounded metadata check, not a full content-integrity
scan, a verification verdict, or permission to repair a shared store.
