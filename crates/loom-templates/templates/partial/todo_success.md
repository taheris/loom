## Todo Success Marker

A successful `loom todo` session ends with exactly one final logical terminal using the typed todo protocol (compact or pretty-printed strict JSON):

```text
LOOM_TODO: {"head":"<sha>","fingerprint":"<fingerprint>","work_epic":"<bead-id>","title":"<final work epic title>","specs":[...]}
```

The JSON shape is `loom-protocol::todo::TodoSuccess`: `head` is the injected `GitSha`, `fingerprint` is the injected `TodoFingerprint`, `work_epic` is the injected `BeadId`, `title` is the required non-empty final title for that work epic, and `specs` is a non-empty list with exactly one entry for each changed spec the driver rendered. The title should summarize the coherent implementation work the child beads represent; do not use driver mechanics or a raw changed-spec list as the title.

Write this line directly as normal assistant text in the final assistant response. Do not generate it via `bash`, `python`, `echo`, `printf`, or any other tool: the driver parses assistant text only and ignores tool output when looking for the terminal marker. Do not wrap the marker in a code fence, and do not add follow-up prose after it.

For each spec entry, use `{"label":"<spec>","outcome":"decomposed","beads":[...],"assignments":[{"task":"<child-id>","criteria":[{"spec_label":"<injected-label>","criterion_id":"<injected-criterion-id>"}]}]}` when you created one or more beads, or `{"label":"<spec>","outcome":"no-work","reason":"<non-empty audit reason>"}` when inspection proves no implementation work is needed. Propose one nonempty criterion assignment per implementation child, using only the injected criterion IDs; never compute IDs, scrape descriptions for bindings, or write acceptance metadata with `bd`. Include existing drafts in the complete roster. Rust resolves proposals against preflight acceptance and persists reference-only metadata. Bead lists, assignment lists, criterion sets, and no-work reasons are typed non-empty values in `TodoSuccess`; empty lists, empty reasons, omitted specs, duplicate specs, `Blocked`, and `pending` are not success states.

Dedicated decision children, including malformed-brief repair items, are **not implementation tasks**. They cannot satisfy `Decomposed.beads`, assignment proposals or criterion coverage. Account for all owned candidates separately through decision admission; an unreported candidate prevents finalization. Do not copy briefs onto the epic. Only actual decomposition prerequisites justify waiting on the work epic; implementation-only decisions leave a complete valid roster free to finalize.

Ordinary task creation, labels, descriptions, and dependency edges remain agent-owned. Rust alone resolves the supplied criterion-ID proposals and persists accepted reference metadata; no second Beads JSON schema is authored here.

Illustrative success payloads below use sample IDs, not assignments to copy into your batch. Substitute the injected batch identity, complete changed-spec roster, actual implementation children and supplied criterion IDs. The first is a decomposed roster; the second is an independently audited no-work roster. In the first example a
staged decision affects only implementation work, not decomposition itself:

```text loom-example
LOOM_CLARIFY: {"decisions":["lm-decision.1"]}
LOOM_TODO: {
  "head": "{{ todo_head }}",
  "fingerprint": "{{ todo_fingerprint }}",
  "work_epic": "{{ work_epic }}",
  "title": "Implement the example contract",
  "specs": [{"label":"example","outcome":"decomposed","beads":["lm-work.1"],"assignments":[{"task":"lm-work.1","criteria":[{"spec_label":"example","criterion_id":"criterion-0000000000000001"}]}]}]
}
```

```text loom-example
LOOM_TODO: {"head":"{{ todo_head }}","fingerprint":"{{ todo_fingerprint }}","work_epic":"{{ work_epic }}","title":"Audit the example contract","specs":[{"label":"example","outcome":"no-work","reason":"Representative implementation and verifier inspection confirm the wording-only change requires no implementation."}]}
```

`LOOM_COMPLETE` and `LOOM_NOOP` are wrong-phase success markers for todo. Use `LOOM_TODO: <json>` as final assistant text for success, or Waiting/Retry/Blocked when the session cannot complete. Nonterminal Clarify records may precede any of these terminals; they never replace the terminal.
