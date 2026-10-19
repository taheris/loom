# Retained verification checkout recovery

## Confirmed lifecycle failure

Both Wrix's pinned Loom `a351c1bf8b2bfc0d903a63b0b1c47cb698838b0e` and Loom
`e4c9c686` contain this path. Current main `a7866f7b` also retains that
ordering; its newer source-edit reconciliation is preserved by this fix:

1. Trusted verification leaves a continuation awaiting publication.
2. The worker disposition closes the bead. The successful controller tail calls
   `reap_closed_worktree`, which calls `GitClient::remove_worktree`.
3. That method recursively deletes the standalone clone (not a linked Git
   worktree). It previously did not consult retained continuations.
4. Publication calls `validate_verification` against the deleted directory.
5. Only after the publication gates, Git push, and tracker push succeed does the
   handoff append a resolved continuation.

The supplied trace proves the checkout is missing, not which process removed it.
The code establishes an executable premature-deletion path, not forensic
attribution of the operator's particular deletion. Parallel defensive cleanup
also called this unguarded method. Startup sweeping had a continuation snapshot
but removed directories directly, outside continuation-writer synchronization.

Removal now shares a host-owned lifecycle lock with continuation capture and
append. Capture holds it from identity inspection through durable journal
publication. Removal holds it from current retained-state inspection through
recursive deletion. All serial, parallel and startup removal uses that boundary.
A retained continuation is a veto regardless of phase, label, root, bead status,
or whether the directory is already absent. Corrupt history fails closed. Staged
interrupted appends conservatively block all cleanup with a diagnostic; they
must not be discarded as an operator workaround.

Resolution still occurs only after the existing publication handoff. Cleanup
syncs the journal directory and its parent before trusting the current view,
including a resolved rename left visible by an interrupted writer. Failed
resolution before rename leaves the unresolved event authoritative; interrupted
staging remains retained and blocks removal. Failed resolution after rename
cannot authorize removal until that view has been synced successfully. The
kernel releases the lifecycle lock on driver exit. A stale writer cannot append
a continuation after removal because append rechecks the original identity.

Successful publication makes the closed clone eligible for the next ordinary
startup sweep; no journal event or candidate pin is deleted. This change does
not broaden other cleanup admission or publication authority. Do not mix old and
new Loom drivers: the old binary does not honor the lifecycle lock.

## Existing `wx-4eouc.31`: exact operator procedure

There is deliberately **no reconstruction or journal-edit recovery command**. A
clone of retained commits has a different identity and is not evidence that
prior verification or publication happened. Even if every object below exists,
this version cannot safely rebind this continuation to newly allocated storage.
The actionable result for a truly deleted original checkout is a blocker, not an
instruction to bypass publication.

1. Stop all Loom loop/cleanup processes for this Wrix workspace, including old
   pinned binaries. Keep the operator and integration checkouts, their `.git`
   directories, the host safety-history registry referenced by
   `.git/loom-workspace.json`, and `.git/loom-verification-continuations`
   intact. Take a private host-side filesystem snapshot/backup of those surfaces
   and any remaining bead clones before attempting recovery. Preserve staging
   files, journals, Git refs and tracker state; do not run Git GC/prune or reset
   an association. A backup is preservation, not replacement identity evidence.
2. From the host, run these read-only checks (record stdout, stderr and exit
   status in the incident report):

   ```bash
   set -euo pipefail
   cd /home/shaun/src/github.com/taheris/wrix
   bead=wx-4eouc.31
   candidate=69a4de5b084553222d661f5972c8312215e72128
   stat -c '%n device=%d inode=%i' ".loom/beads/$bead" ".loom/beads/$bead/.git"
   ```

   If the path is absent, `stat` must fail. Do not create a replacement there.
   Separately inspect retained Git data:

   ```bash
   set -euo pipefail
   cd /home/shaun/src/github.com/taheris/wrix
   integration=.loom/integration
   pin=refs/loom/verification/wx-4eouc.31/69a4de5b084553222d661f5972c8312215e72128
   git -C "$integration" show-ref --verify "$pin"
   git -C "$integration" cat-file -e fd73edcfef7a9b2ca1aafd4ea25bc0d613195933^{commit}
   git -C "$integration" cat-file -e 69a4de5b084553222d661f5972c8312215e72128^{commit}
   git -C "$integration" cat-file -e 169c1f7788cd99feaf3b1d0ce0ecce1a08c57e79^{commit}
   git -C "$integration" cat-file -e a6aa6009719e9db6dc24d1d07f4087b667252288^{tree}
   git -C "$integration" fsck --connectivity-only --no-reflogs 69a4de5b084553222d661f5972c8312215e72128
   ```

   The pin must equal the candidate, not merely exist. A missing source commit
   or tree is additional unavailable evidence; stop at that failure and retain
   its diagnostic. Object availability alone is not verification evidence.

3. If the _original directories_ were moved aside rather than deleted, have the
   operator identify them without modifying the journal. They must still have
   device `65024`, checkout inode `132521015`, Git-directory inode `132521016`,
   source HEAD `169c1f7788cd99feaf3b1d0ce0ecce1a08c57e79`, and tree
   `a6aa6009719e9db6dc24d1d07f4087b667252288`. Only an operator-reviewed move of
   those very directories back to the original path can restore this identity. A
   copied backup, reclone, inode reuse, or manually edited identity is not
   acceptable. Keep the workspace stopped during inspection/restoration.
4. Deploy the fixed Loom in Wrix (update its Loom pin and rebuild the actual
   command being invoked). If the original independent storage was genuinely
   restored, rerun `loom loop wx-4eouc`; normal identity checks, fresh required
   verification, review and publication gates still apply. Observe successful
   Git and tracker publication and the new durable resolved event before
   expecting ordinary closed-clone cleanup on a subsequent loop invocation.
5. If the original directories were deleted, **do not resume publication**.
   Report:
   `wx-4eouc.31: original verification checkout identity unavailable; continuation remains unresolved; candidate pin retained`.
   Attach the latest record, the missing-path diagnostic and object/pin check
   results to a native tracking issue for provenance-aware reconstruction
   support. That support must check retained objects, allocate independently
   checked storage, retain the original record, durably append recovery
   provenance/new identity, and require fresh verification and ordinary
   publication. It is not implemented by this fix. Until available, this record
   cannot safely be resolved here.

Never delete the continuation directory, reset the association, fabricate an
identity, manually mark the record resolved, or use reconstruction as proof of
past verification/publication. Independently observed remote publication does
not by itself supply the missing local verification-checkout identity.
