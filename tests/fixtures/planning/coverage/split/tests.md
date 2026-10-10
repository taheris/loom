# Queue acceptance

## Success Criteria

### Admission

- Accepted jobs receive a nonempty ID. [test?](queue_assigns_id)
- Duplicate submission returns the original ID and leaves exactly one queued
  job. [test?](queue_deduplicates_submission)

### Cancellation

- Cancelling a queued job prevents execution; cancelling a running job stops it
  and releases its lease. [test?](queue_cancellation_by_state)

## Verification strategy

Use state-specific native tests for duplicate admission and cancellation. Each
assertion observes the queue or lease, not just a successful return value.
Pending bindings declare the prospective tests; they do not claim execution.
