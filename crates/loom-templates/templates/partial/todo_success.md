## Todo Success Marker

A successful `loom todo` session ends with exactly one final line using the typed todo protocol:

```text
LOOM_TODO: {"head":"<sha>","fingerprint":"<fingerprint>","work_epic":"<bead-id>","specs":[...]}
```

The JSON shape is `loom-protocol::todo::TodoSuccess`: `head` is the injected `GitSha`, `fingerprint` is the injected `TodoFingerprint`, `work_epic` is the injected `BeadId`, and `specs` is a non-empty list with exactly one entry for each changed spec the driver rendered.

For each spec entry, use `{"label":"<spec>","outcome":"decomposed","beads":[...]}` when you created one or more beads, or `{"label":"<spec>","outcome":"no-work","reason":"<non-empty audit reason>"}` when inspection proves no implementation work is needed. Bead lists and no-work reasons are typed non-empty values in `TodoSuccess`; empty lists, empty reasons, omitted specs, duplicate specs, `Blocked`, and `pending` are not success states.

`LOOM_COMPLETE` and `LOOM_NOOP` are wrong-phase success markers for todo. Use `LOOM_TODO: <json>` for success, or the worker self-report markers above when the session cannot complete.
