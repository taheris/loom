# Queue

## Admission

[Admission acceptance](tests.md#admission).

Accepted jobs receive an ID. Duplicate submissions return the original ID
without enqueuing another job.

## Lifecycle

[Cancellation acceptance](tests.md#cancellation).

| State   | Event  | Contract                             |
| ------- | ------ | ------------------------------------ |
| Queued  | Cancel | Removes the job before execution     |
| Running | Cancel | Stops the job and releases its lease |

## Out of Scope

- Retrying failed jobs; retries are a separate owner.
