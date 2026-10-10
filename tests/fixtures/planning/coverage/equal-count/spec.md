# Queue

## Lifecycle

[Acceptance](tests.md#lifecycle).

| State   | Event  | Contract                             |
| ------- | ------ | ------------------------------------ |
| Queued  | Cancel | Removes the job before execution     |
| Running | Cancel | Stops the job and releases its lease |
