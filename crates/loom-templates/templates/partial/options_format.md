  ```markdown
  ## Options — <one-line summary of the decision, ≤80 chars>

  ### Option 1 — <short title>
  <body paragraph(s) describing the option, naming its cost>

  ### Option 2 — <short title>
  <body, including cost>

  ### Option 3 — <short title>
  <body, including cost>
  ```

  **Rules:**

  - The `## Options` header carries a one-line summary (≤50 chars)
    separated from the word `Options` by em-dash `—` (default),
    en-dash `–`, single hyphen `-`, or double hyphen `--`. Parsers
    tolerate any of these; emit em-dash by default.
  - Each option is `### Option N — <title>` where `N` is 1-based
    sequential. Numbering is required for `-a <int>` lookup to work.
  - Each option body extends from its `### Option N` heading until
    the next `### Option` or the next `##` heading; name the cost
    (churn, debt, coupling, risk).
  - Use contextual options per decision — typically 2–4, each
    naming its cost. Do NOT emit a fixed A/B/C menu.

  `loom msg` parses this format to render the SUMMARY column,
  enumerate options for view mode, and resolve integer fast-replies.
  A malformed block — or one that lives only in your prose, never
  persisted to bead state — breaks fast-reply with `-a <int>` and
  leaves the options invisible to `loom msg`'s queue.
