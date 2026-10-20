# Retained verification checkout recovery

## Lifecycle boundary

Earlier drivers could remove a closed bead clone before its verification
continuation reached publication. Closure alone is not cleanup admission.
Capture, recovery snapshot updates and removal now share a host lifecycle lock.
Unresolved snapshots in the controlling `.loom/state.db` veto removal. Dirty
work, stashes and branches not proven redundant with integration are preserved
even when the database is missing or explicitly reset. Corrupt state produces a
diagnostic rather than cleanup permission.

The state database stores transient operation snapshots, not proof of completed
verification or publication. Git candidate pins and real checkouts remain Git
payloads. Resolution follows the ordinary Git and tracker publication handoff;
it does not independently grant evidence coverage. Do not mix old and new
running drivers: old versions do not honor the current lifecycle boundary.

## Recovery on another machine or after checkout loss

1. Stop other Loom drivers and preserve remaining checkouts, `.git` directories,
   candidate pins, tracker records, and local state. Do not run Git GC/prune or
   overwrite a dirty clone to make recovery pass.
2. Upgrade the actual invoked Loom binary. Run `loom init` once to import
   retired local stores under the workspace initialization lock. Failed imports
   leave their inputs intact for diagnosis; imported verification facts grant no
   positive coverage. No external home-directory registry or old inode identity
   is required to establish the current host invocation.
3. Synchronize the candidate Git objects into the controlling operator or
   integration repository. Object presence proves availability, not
   verification. For example, inspect the pin and commit read-only:

   ```bash
   git -C .loom/integration show-ref --verify refs/loom/verification/<bead>/<candidate>
   git -C .loom/integration cat-file -e '<candidate>^{commit}'
   git -C .loom/integration fsck --connectivity-only --no-reflogs '<candidate>'
   ```

4. Resume the owning `loom loop` root. Recovery can allocate an independent
   missing clone from the available candidate, or inspect existing replacement
   storage without overwriting it. Changed or dirty source is retained for
   repair. Rebinding invalidates publication-rebase reuse and requires fresh
   verification and the ordinary review/publication gates. An invocation still
   checks that its current directories have not changed during an operation.
5. If candidate objects are unavailable, keep remaining work and synchronize
   them before retrying. Alternatively, after preserving work and deciding to
   restart from the current canonical assignment, `loom init --rebuild` resets
   transient snapshots and evidence. It is not a publication or cleanup override
   and does not reopen independent human holds.

Never fabricate verification evidence, manually mark a snapshot resolved, or
interpret reconstruction, an empty database, or remote publication as proof of
local verification. A fresh checkout and missing local history are cold state,
not a permanent failure and not a passing result.
