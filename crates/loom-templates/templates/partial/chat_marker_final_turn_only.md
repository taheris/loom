**Emit the terminal marker on the final turn only.** This is a multi-turn chat,
not a single-shot worker phase. The "end your response with the marker"
instruction in *Exit Signals* above refers to the **final assistant turn of
the session** — the wrap-up turn after the user signals they are done (e.g.
"thanks, that's all", "stop", explicit goodbye) or after the chat queue is
exhausted.

`plan` ends with `LOOM_COMPLETE`. `inbox` ends with `LOOM_COMPLETE` when no
trusted driver apply handoff is needed, or `LOOM_APPLY: {"proposals":[...]}`
when accepted tune proposals should be applied by the driver. Do **NOT** append
these markers to intermediate turns mid-conversation; end mid-conversation
replies with normal prose (or nothing) and reserve the marker for the
session-close turn. Emitting a marker every turn pollutes the transcript,
misaligns your own self-model ("each reply is the end"), and risks tripping
orchestrator parsers that trust the marker as terminal.
