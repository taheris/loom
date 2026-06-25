{% match workspace_recovery %}{% when Some with (recovery) %}

## Workspace Recovery

A dirty bead workspace was preserved before this dispatch. This context is separate from retry failure state; inspect it before normal implementation work and decide deliberately how the saved work relates to this bead.

**Pre-stash status:**

```text
{{ recovery.pre_stash_status }}
```

- **Recovery stash selector at dispatch**: `{{ recovery.stash.selector }}`
- **Stable recovery stash commit**: `{{ recovery.stash.commit }}`
- **Recovery stash message**: `{{ recovery.stash.message }}`
- **Target integration tip**: `{{ recovery.integration_tip }}`
- **Workspace alignment**: {{ recovery.alignment }}

Inspect the stable stash commit, not a moving stash selector, before editing:

```bash
git stash show --stat {{ recovery.stash.commit }}
git stash show -p {{ recovery.stash.commit }}
```

Then intentionally choose one recovery path: apply the stash, cherry-pick relevant hunks, leave it unapplied for a justified follow-up, or drop it when it is irrelevant after inspection. Do not silently ignore preserved local work.

{% if recovery.alignment.is_conflict() %}Alignment is in conflict. Treat this as agent-owned merge-conflict recovery: inspect the conflict files, resolve them, and continue, abort, or retry the rebase as appropriate before normal implementation work. Conflict files rendered by the driver:
{% for file in recovery.alignment.conflict_files() %}- `{{ file }}`
{% endfor %}
Use `LOOM_CLARIFY` with a persisted Options block if the conflict requires a human decision.

{% endif %}In your final prose before the terminal marker, include one short line naming how you handled the recovery stash (applied, partly cherry-picked, left for follow-up, dropped as irrelevant, or needs clarification) and why. That prose is accountability for reviewers; the driver does not parse it and does not reject `LOOM_COMPLETE` solely because the stash still exists.
{% when None %}{% endmatch %}
