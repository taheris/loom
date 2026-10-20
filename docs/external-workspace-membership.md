# External workspace membership

[Workspaces](../specs/workspaces.md#external-workspace-membership) owns this
interface. It groups checkouts for an external consumer; it does not change
Loom's native execution, evidence, history or cleanup authority.

## Configure through Loom

The consumer independently registers the operator first, then invokes:

```text
loom --workspace /path/to/operator init --workspace-associator /absolute/path/to/adapter
```

No manual Git configuration or environment-variable discovery is required.
Reissuing the flag explicitly replaces the selection. Ordinary `loom init`
retains it. To explicitly disable it:

```text
loom --workspace /path/to/operator init --no-workspace-associator
```

Configuration is a trusted host action: choosing the program authorizes host
code execution as the invoking user. Do not select a repository/worker-writable
adapter. The program must be an absolute, regular executable owned by the user
or root and not writable by group/others; symlink programs and unsafe ancestors
are refused. Packaged adapters should use their direct immutable executable path
and declared tool closure.

Selection is stored at the operator's `.git/loom-workspace-associator.json`.
This owner-private (0600), singly linked regular file has a strict versioned
internal encoding. Use the command, not file edits. Explicit disabling retains
an encoded disabled selection. The file is configuration, not a workspace ID,
registry or execution capability. Integration invocations resolve the operator's
selection. Execution clones cannot provide one; host Git storage remains
excluded from worker mounts. `LOOM_INSIDE` prevents adapter selection/execution.

Selection is synchronized and atomically renamed under the initialization lock
before admission. A missing program or rejected admission does not erase it. A
retry without flags still fails closed. Only genuine absence or explicitly
disabled configuration enables standalone operation. Malformed, unreadable,
unsafe or unsupported selections are errors, not absence.

A crash can leave `.git/loom-workspace-associator.staging`. Any staging object
blocks admission and configuration, even if the final selection is absent or
valid. Stop competing initialization, inspect and preserve the evidence, then
explicitly remove the known staging object before retrying configuration. Loom
does not infer success from residue or automatically delete it. A failure after
rename may leave the final selection installed; inspect it and explicitly
reselect/disable rather than assuming rollback.

## Adapter contract

Loom directly executes exactly:

```text
/absolute/path/to/adapter --operator /absolute/operator --checkout /absolute/participant
```

There is no shell expansion, identity argument, role payload or stdout protocol.
The current directory is the operator checkout. Execution is host-side as the
invoking user, outside worker containers. Stdin is null. The adapter must be
noninteractive and must not depend on Loom clearing or pinning its ambient
environment: its packaged wrapper must consistently select the host-user store
and declared Git/tool closure. If alternate stores are supported, their
selection must be explicit and stable.

The adapter must:

1. Resolve an **existing** operator registration. Refuse an unknown operator
   without enrolling it. A resolve-or-register API is insufficient.
2. Derive the expected identity from that registration.
3. Under its own serialization, associate an unregistered target, confirm
   matching membership, or reject conflicting membership without rebind.
4. Validate actual Git/filesystem participants around its operation. Registry
   locking alone does not prevent pathname replacement. Refuse uncertainty or
   bound retries. Paths locate participants; remote/path/name equality does not
   establish membership.
5. Make retry idempotent. A supported move may refresh recorded locations;
   confirmation-only means no new operator membership, not necessarily no
   writes.

When operator and checkout are the same participant, this is confirmation-only.
The selected adapter is trusted code: Loom cannot prove an arbitrary provider's
identity policy from exit status. Independent registrations must be refused by
the provider, and real provider interoperability requires separate acceptance.

Exit zero accepts membership. Any other status, signal, execution error,
timeout, output overflow or cleanup failure blocks subsequent preparation. The
budget is 30 seconds and at most 16 KiB on each captured pipe. Bounded stderr is
included in rejection diagnostics; adapters must not print secrets. Stdout has
no semantic meaning. On timeout or capture failure Loom kills the owned process
group and reaps the direct child before returning; cancellation uses the
existing process-owner cleanup path. On ordinary exit the adapter must leave no
background work. Adapters must not detach work from the owned group.

A timeout may occur after external membership was committed. Loom preserves that
membership and the checkout rather than attempting a rollback. There is no
transaction across external registration, Git metadata and native SQLite state.

## Preparation and authority

Operator confirmation precedes integrated initialization. Fresh integration
cloning is necessary before target admission; rerere, hooks and native state
creation wait for admission. Existing integration admission also precedes native
rebuild and Git reconciliation. Fresh/reused bead preparation admits the target
before identity/policy/hook changes, mount preparation, stashing and
fetch/alignment. Tune clones admit before checkout/identity configuration.
Failures retain checkouts and dirty work for inspection; retry never treats
checkout existence as prior admission.

Native checkout observations and selection are checked around execution. This
narrows replacement windows; it is not an atomic filesystem/registry transaction
or protection against arbitrary same-user mutation after a check.

External identity does not choose `.loom/state.db`, authorize mounts, establish
verification equivalence, admit evidence, or authorize deletion. Native cleanup
never invokes this adapter. Consumers own stale external-registration retention;
this interface does not introduce an unregister callback, inventory/retention
service, Wrix context delivery or execution bridge.

## Moves and linked worktrees

Before this interface, Loom already resolved host context per invocation from
the nested layout, shared the controlling native state, and rejected changes to
a previously issued native capability. There was no persistent native key/path
registry to relocate. This change preserves that model.

Moving the intact operator tree with its nested integration/execution clones
retains layout and native state. Adapter executables must remain available, and
the consumer must recognize retained membership at the new location. Reused bead
creation repairs the stale absolute `origin` to the current integration path
after admission, allowing subsequent fetch/alignment. An operation holding old
paths fails recheck. Independently moving integration outside the layout is not
supported. Copies or replacements between invocations are not rejected by
persistent native identity pins, because Loom has none.

Loom's host `Association` and adapter configuration require real Git
directories. Some standalone GitClient inspection can operate on linked
worktrees, but that does not make them valid host associations or execution
clones. Membership loading never follows a `.git` pointer to find executable
selection. Bead linked-worktree/shared/indirect-storage checks remain in force.
Shared-Git interoperability needs its own native contract review, not an adapter
bypass.

## Acceptance scope

The new CLI tests exercise real `loom init`, disposable Git clones and native
SQLite records. Driver tests exercise real execution preparation, moves,
replacement refusal, dirty-work recovery and native cleanup. Process component
tests cover bounded output, closed stdin, timeout cleanup and idempotent retry.
The fixture adapter has its own refusal/retry conformance test, but uses simple
checkout-local test membership, not a production host identity registry.

These tests establish Loom-side behavior only. They do not establish Weld
interoperability, physical host-user registry security, Darwin execution or a
complete Wrix worker launch. Existing native isolation and production suites
remain required. No adapter or driver is installed or activated by these tests.
See the
[implementation verification report](external-workspace-membership-verification.md)
for executed checks, negative controls and retained local evidence.
