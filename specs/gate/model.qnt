/// Bounded composition of Verify, Evidence and Gate; not a proof of Rust or tools.
module gate {
  pure val Claims = 0.to(7)
  pure val Units = 0.to(4)
  pure val Stages = Set("plan", "worker", "integration", "stabilization", "publication", "standing")
  pure val Callers = Set("standalone", "worker", "integration", "driver", "manual", "rehearsal")
  pure def unit(c) = if (c < 2) 0 else if (c == 2 or c == 3 or c == 6) 1 else if (c == 4) 2 else if (c == 5) 3 else 4
  pure def purpose(c) = if (c == 6) "readiness" else "verification"
  pure def tier(c) = if (c < 2) "check" else if (c == 2 or c == 6) "test" else if (c == 3 or c == 4) "system" else if (c == 5) "judge" else "hook"
  pure def live(u) = u >= 2
  pure def freshClaim(c) = Set(4, 5, 6, 7).contains(c)
  pure def stageFor(caller, requested) = if (Set("driver", "manual", "rehearsal").contains(caller)) "publication"
    else if (caller == "worker") "worker" else if (caller == "integration") "integration" else requested

  type Manifest = { unit: int, input: int, definition: int, campaign: int,
    environment: int, checker: int, members: Set[int], absent: Set[int], purpose: str }
  type Fact = { manifest: Manifest, outcome: str, attempt: int, serial: int, imported: bool,
    signer: int, provenance: bool, environment: bool, verification: bool, intact: bool }
  type Task = { manifest: Manifest, attempt: int }
  type Goal = Criteria(Set[int]) | Remediation(int) | Unassigned
  type Event = { kind: str, manifest: Manifest, claim: int, serial: int }

  var s: {
    subjects: Set[int], inventory: Set[int], changed: Set[int], deferred: Set[int], offered: Set[int],
    inputs: int -> int, definitions: int -> int, campaign: int, environment: int,
    stage: str, caller: str, scope: str, context: bool, parent: bool, target: int, requested: int,
    snapshot: int, structural: bool, valid: bool, bindings: bool, seal: int,
    selected: Set[int], running: Set[Task], facts: Set[Fact], builds: Set[Manifest],
    journal: List[Event], checkpoint: Set[Event], index: Set[Event],
    objects: Set[int], historyAvailable: bool, partial: bool, logs: bool,
    trust: Set[int], attempt: int, alive: bool, owner: str, checkout: str,
    verified: int, reviewed: int, validated: int, serial: int, remote: int,
    accepted: bool, rejected: bool, published: bool, expected: bool, launches: int, reuses: int,
    link: bool, label: int, findings: Set[int], goal: Goal, dispatched: bool
  }

  def manifest(c) = {
    unit: unit(c), input: s.inputs.get(unit(c)), definition: s.definitions.get(unit(c)),
    campaign: s.campaign, environment: s.environment, checker: 0,
    members: s.subjects.filter(x => unit(x) == unit(c)),
    absent: Claims.exclude(s.subjects).filter(x => unit(x) == unit(c)), purpose: purpose(c)
  }
  def events = s.checkpoint.union(s.journal.foldl(Set(), (acc, e) => acc.union(Set(e))))
  def failed(m) = events.exists(e => e.kind == "failure" and e.manifest == m)
  def passed(m) = events.exists(e => e.kind == "pass" and e.manifest == m)
  def disagreement(m) = failed(m) and passed(m)
  def blocked(c) = events.exists(e => e.kind == "confirmed" and e.claim == c and
    not(events.exists(r => r.kind == "resolved" and r.claim == c and r.serial > e.serial and r.manifest.with("campaign", s.campaign) == manifest(c))))
  def history(c) = s.historyAvailable and s.link and events.filter(e => e.kind == "confirmed" and e.claim == c).forall(e => s.objects.contains(e.serial))
  def fresh(f) = not(f.imported) and s.alive and f.attempt == s.attempt and
    not(events.exists(e => e.kind == "failure" and e.manifest == f.manifest and e.serial > f.serial))
  def admissible(c, f) = all {
    f.manifest == manifest(c), f.outcome == "pass", f.intact, f.provenance, f.environment, f.verification,
    history(c), not(blocked(c)), s.offered.contains(c),
    if (s.scope == "target") f.serial > s.requested else true,
    not(f.imported) or s.trust.contains(f.signer),
    if (s.scope == "target" or freshClaim(c) or disagreement(f.manifest) or failed(f.manifest)) fresh(f) else true
  }

  /// Full reference derives requirements from current repository subjects, never inventory or selected targets.
  def referenceRequired(c) = all {
    s.subjects.contains(c), c != 6 or s.offered.contains(c),
    if (s.scope == "target") unit(c) == unit(s.target) and purpose(c) == purpose(s.target) else true,
    if (tier(c) == "hook") s.scope == "diff" else true,
    if (tier(c) == "judge") s.stage == "plan" or s.stage == "standing" or (s.stage == "publication" and s.caller == "driver") else true,
    if (s.scope == "tree" or s.scope == "target" or s.stage == "publication" or s.stage == "standing") true
    else s.changed.contains(c) and not(s.deferred.contains(c))
  }
  def externalFailure(c) = s.facts.exists(f => all {
    f.manifest == manifest(c), not(f.imported), f.attempt == s.attempt, f.outcome == "fail",
    not(s.facts.exists(p => p.manifest == f.manifest and not(p.imported) and p.attempt == f.attempt and
      p.outcome == "pass" and p.serial > f.serial))
  })
  def reference(c) = if (not(referenceRequired(c))) "not-required"
    else if (not(s.valid) or not(s.bindings) or not(history(c)) or not(s.offered.contains(c))) "unavailable"
    else if (blocked(c)) "blocked"
    else if (if (live(unit(c))) externalFailure(c) else s.inputs.get(unit(c)) == 1) "fail"
    else if (purpose(c) == "readiness") "ready" else "pass"

  /// Optimizer groups only after criterion-local applicability, retaining a separate readiness purpose.
  def eligible(c) = s.inventory.contains(c) and (c != 6 or s.offered.contains(c)) and
    (s.scope != "target" or (unit(c) == unit(s.target) and purpose(c) == purpose(s.target))) and
    (tier(c) != "hook" or s.scope == "diff") and
    (tier(c) != "judge" or Set("plan", "standing").contains(s.stage) or (s.stage == "publication" and s.caller == "driver")) and
    (Set("tree", "target").contains(s.scope) or Set("publication", "standing").contains(s.stage) or
      (s.changed.contains(c) and not(s.deferred.contains(c))))
  def required = s.inventory.filter(c => eligible(c))
  def coverage(c) = if (not(eligible(c))) "not-required"
    else if (not(s.valid) or not(s.bindings) or not(history(c)) or not(s.offered.contains(c))) "unavailable"
    else if (blocked(c)) "blocked"
    else if (s.facts.exists(f => admissible(c, f))) { if (purpose(c) == "readiness") "ready" else "pass" }
    else if (s.facts.exists(f => f.manifest == manifest(c) and f.outcome == "fail" and fresh(f) and
      (s.scope != "target" or f.serial > s.requested))) "fail"
    else "fresh-work-needed"
  def complete = required.forall(c => Set("pass", "ready").contains(coverage(c)))
  def hooksReady = s.scope != "diff" or not(s.subjects.contains(7)) or s.facts.exists(f => admissible(7, f))
  def current = s.seal == s.snapshot and s.inventory == s.subjects and s.structural and s.valid and s.bindings and not(s.parent) and hooksReady and
    required.forall(c => s.offered.contains(c))
  def referenceEvidence(c) = s.facts.exists(f => all {
    f.manifest.unit == unit(c), f.manifest.input == s.inputs.get(unit(c)),
    f.manifest.definition == s.definitions.get(unit(c)), f.manifest.campaign == s.campaign,
    f.manifest.environment == s.environment, f.manifest.checker == 0, f.manifest.purpose == purpose(c),
    f.manifest.members == s.subjects.filter(x => unit(x) == unit(c)),
    f.manifest.absent == Claims.filter(x => not(s.subjects.contains(x)) and unit(x) == unit(c)),
    f.outcome == "pass", f.intact, f.provenance, f.environment, f.verification,
    not(f.imported) or s.trust.contains(f.signer),
    if (s.scope == "target" or freshClaim(c) or failed(f.manifest) or disagreement(f.manifest)) all {
      not(f.imported), f.attempt == s.attempt, s.alive,
      not(events.exists(e => e.kind == "failure" and e.manifest == f.manifest and e.serial > f.serial))
    } else true,
    if (s.scope == "target") f.serial > s.requested else true
  })
  def referenceAuthority = all {
    s.structural, s.valid, s.bindings, s.inventory == s.subjects, s.seal == s.snapshot,
    Claims.forall(c => Set("pass", "ready", "not-required").contains(reference(c)) and
      (referenceRequired(c) implies referenceEvidence(c))),
    s.context, not(s.parent), s.scope == "diff", s.stage == "publication",
    Set("driver", "manual").contains(s.caller), s.alive, s.owner == s.checkout,
    s.verified == s.snapshot, s.validated == s.serial,
    if (s.caller == "driver") s.reviewed == s.snapshot else true
  }
  def authority = all {
    current, complete, s.context, not(s.parent), s.scope == "diff", s.stage == "publication",
    Set("driver", "manual").contains(s.caller), s.alive, s.owner == s.checkout,
    s.verified == s.snapshot, s.validated == s.serial,
    if (s.caller == "driver") s.reviewed == s.snapshot else true
  }

  pure val initial = {
    subjects: Claims, inventory: Set(), changed: Claims, deferred: Set(1), offered: Claims,
    inputs: Units.mapBy(_ => 0), definitions: Units.mapBy(_ => 0), campaign: 0, environment: 0,
    stage: "publication", caller: "driver", scope: "diff", context: true, parent: false, target: 0, requested: -1,
    snapshot: 0, structural: false, valid: true, bindings: true, seal: -1,
    selected: Set(), running: Set(), facts: Set(), builds: Set(),
    journal: List(), checkpoint: Set(), index: Set(), objects: Set(), historyAvailable: true,
    partial: false, logs: true, trust: Set(0), attempt: 0, alive: true, owner: "operator", checkout: "operator",
    verified: -1, reviewed: -1, validated: -1, serial: 0, remote: 0,
    accepted: false, rejected: false, published: false, expected: false, launches: 0, reuses: 0, link: true, label: 0, findings: Set(), goal: Unassigned, dispatched: false
  }

  action init = s' = initial
  pure val sealedInitial = List(7, 0, 2, 4, 5, 6).foldl(
    initial.with("structural", true).with("inventory", Claims).with("seal", 0).with("selected", Claims)
      .with("verified", 0).with("reviewed", 0).with("validated", 6),
    (acc, c) => {
      val m = { unit: unit(c), input: 0, definition: 0, campaign: 0, environment: 0, checker: 0,
        members: Claims.filter(x => unit(x) == unit(c)), absent: Set(), purpose: purpose(c) }
      val f = { manifest: m, outcome: "pass", attempt: 0, serial: acc.serial + 1, imported: false,
        signer: 0, provenance: true, environment: true, verification: true, intact: true }
      val e = { kind: "pass", manifest: m, claim: 0, serial: acc.serial + 1 }
      acc.with("facts", acc.facts.union(Set(f))).with("journal", acc.journal.append(e))
        .with("serial", acc.serial + 1).with("launches", acc.launches + 1)
    })
  pure val smallManifest = { unit: 0, input: 0, definition: 0, campaign: 0, environment: 0,
    checker: 0, members: Set(0, 1), absent: Set(), purpose: "verification" }
  pure val smallFact = { manifest: smallManifest, outcome: "pass", attempt: 0, serial: 1, imported: false,
    signer: 0, provenance: true, environment: true, verification: true, intact: true }
  pure val smallEvent = { kind: "pass", manifest: smallManifest, claim: 0, serial: 1 }
  pure val smallInitial = initial.with("subjects", Set(0, 1)).with("inventory", Set(0, 1))
    .with("selected", Set(0, 1)).with("structural", true).with("seal", 0).with("verified", 0)
    .with("reviewed", 0).with("validated", 1).with("serial", 1).with("launches", 1)
    .with("facts", Set(smallFact)).with("journal", List(smallEvent))
  action symbolicInit = any { init, s' = smallInitial }

  action structural = s' = s.with("structural", s.valid and hooksReady).with("accepted", false).with("rejected", not(s.valid and hooksReady))
  action discover = all {
    s.structural,
    s' = s.with("inventory", s.subjects).with("seal", s.snapshot).with("selected", Set()).with("accepted", false)
  }
  action plan = all { current, s' = s.with("selected", required).with("accepted", false) }
  def launchable(c) = all {
    if (c == 7) s.scope == "diff" and s.subjects.contains(7) and not(s.parent) else current and s.selected.contains(c),
    not(s.running.exists(t => t.manifest == manifest(c) and t.attempt == s.attempt)),
    not(s.facts.exists(f => admissible(c, f)))
  }
  action launch(c: int): bool = all {
    launchable(c),
    s' = s.with("running", s.running.union(Set({ manifest: manifest(c), attempt: s.attempt }))).with("launches", s.launches + 1).with("accepted", false)
  }
  action diagnosticLaunch(c: int): bool = all {
    current, s.selected.contains(c), not(s.running.exists(t => t.manifest == manifest(c) and t.attempt == s.attempt)),
    s' = s.with("running", s.running.union(Set({ manifest: manifest(c), attempt: s.attempt })))
      .with("launches", s.launches + 1).with("accepted", false)
  }
  action finish(t: Task, outcome: str): bool = all {
    s.running.contains(t),
    val m = t.manifest
    val f = { manifest: m, outcome: outcome, attempt: t.attempt, serial: s.serial + 1, imported: false,
      signer: 0, provenance: true, environment: true, verification: true, intact: true }
    val e = { kind: if (outcome == "pass") "pass" else "failure", manifest: m, claim: 0, serial: s.serial + 1 }
    s' = s.with("running", s.running.exclude(Set(t))).with("facts", s.facts.union(Set(f)))
      .with("journal", if (Set("pass", "fail").contains(outcome)) s.journal.append(e) else s.journal)
      .with("serial", s.serial + 1).with("accepted", false)
  }
  action attemptLaunch(c: int): bool = if (launchable(c)) launch(c) else s' = s.with("rejected", true).with("accepted", false)
  action execute = { nondet c = Claims.oneOf() attemptLaunch(c) }
  action completion = {
    nondet t = s.running.oneOf()
    nondet effect = Set("pass", "fail").oneOf()
    finish(t, if (live(t.manifest.unit)) effect else if (t.manifest.input == 1) "fail" else "pass")
  }
  action incomplete = {
    nondet t = s.running.oneOf()
    nondet outcome = Set("skip", "timeout", "unavailable").oneOf()
    finish(t, outcome)
  }
  action reuse(c: int): bool = all {
    current, s.selected.contains(c), s.facts.exists(f => admissible(c, f)),
    s' = s.with("reuses", s.reuses + 1).with("accepted", false)
  }
  action importResult(c: int, signer: int, provenance: bool, environment: bool, verification: bool, intact: bool): bool = {
    val f = { manifest: manifest(c), outcome: if (s.inputs.get(unit(c)) == 1) "fail" else "pass", attempt: -1, serial: s.serial,
      imported: true, signer: signer, provenance: provenance, environment: environment, verification: verification, intact: intact }
    val e = { kind: "pass", manifest: f.manifest, claim: c, serial: s.serial + 1 }
    s' = s.with("facts", s.facts.union(Set(f))).with("accepted", false).with("serial", s.serial + 1)
      .with("journal", if (admissible(c, f)) s.journal.append(e) else s.journal)
  }
  action imported = {
    nondet c = Claims.oneOf()
    nondet signer = Set(0, 1).oneOf()
    nondet provenance = Set(true, false).oneOf()
    nondet environment = Set(true, false).oneOf()
    nondet verification = Set(true, false).oneOf()
    nondet intact = Set(true, false).oneOf()
    importResult(c, signer, provenance, environment, verification, intact)
  }
  action build(c: int): bool = all { current, s' = s.with("builds", s.builds.union(Set(manifest(c)))).with("accepted", false) }
  action verify = s' = s.with("verified", if (current and complete) s.snapshot else -1).with("accepted", false)
  action review = s' = s.with("reviewed", if (current and complete and s.verified == s.snapshot) s.snapshot else -1).with("accepted", false)
  action validate = s' = s.with("validated", s.serial).with("accepted", false)
  action attemptPush = s' = s.with("accepted", authority).with("rejected", not(authority)).with("expected", referenceAuthority)
    .with("published", s.published or authority).with("alive", if (authority) false else s.alive)

  action endAttempt(remoteChanged: bool): bool = s' = s.with("alive", false).with("accepted", false)
    .with("remote", if (remoteChanged) s.remote + 1 else s.remote)
  action retry = all {
    not(s.alive), s.attempt < 3,
    s' = s.with("attempt", s.attempt + 1).with("alive", true).with("owner", s.checkout)
      .with("verified", -1).with("reviewed", -1).with("validated", -1).with("accepted", false)
  }
  action changeInput(u: int): bool = all {
    s.inputs.get(u) < 2,
    s' = s.with("inputs", s.inputs.set(u, s.inputs.get(u) + 1)).with("snapshot", s.snapshot + 1).with("accepted", false)
  }
  action diagnostic(c: int): bool = s' = s.with("scope", "target").with("target", c).with("requested", s.serial)
    .with("snapshot", s.snapshot + 1).with("accepted", false)
  action changeEnvironment = all { s.environment < 2,
    s' = s.with("environment", s.environment + 1).with("snapshot", s.snapshot + 1).with("accepted", false) }
  action absent(c: int): bool = s' = s.with("offered", s.offered.exclude(Set(c))).with("snapshot", s.snapshot + 1).with("accepted", false)
  action claimEdit = s' = s.with("bindings", false).with("snapshot", s.snapshot + 1).with("accepted", false)
  action correct = s' = s.with("valid", true).with("bindings", true).with("structural", false).with("accepted", false)
  action malformed = s' = s.with("valid", false).with("structural", false).with("snapshot", s.snapshot + 1).with("accepted", false)
  action definition(u: int): bool = all { s.definitions.get(u) < 2,
    s' = s.with("definitions", s.definitions.set(u, s.definitions.get(u) + 1)).with("snapshot", s.snapshot + 1).with("accepted", false) }
  action changeDefinition = { nondet u = Units.oneOf() definition(u) }
  action membership = {
    nondet c = Claims.oneOf()
    s' = s.with("subjects", if (s.subjects.contains(c)) s.subjects.exclude(Set(c)) else s.subjects.union(Set(c)))
      .with("snapshot", s.snapshot + 1).with("accepted", false)
  }
  action context = {
    nondet stage = Stages.oneOf()
    nondet caller = Callers.oneOf()
    nondet scope = Set("diff", "files", "tree", "target").oneOf()
    nondet valid = Set(true, false).oneOf()
    nondet parent = Set(true, false).oneOf()
    s' = s.with("stage", stageFor(caller, stage)).with("caller", caller).with("scope", scope).with("context", valid)
      .with("parent", parent).with("requested", s.serial).with("snapshot", s.snapshot + 1).with("accepted", false)
  }
  action defer = {
    nondet c = Claims.oneOf()
    s' = s.with("deferred", if (s.deferred.contains(c)) s.deferred.exclude(Set(c)) else s.deferred.union(Set(c)))
      .with("snapshot", s.snapshot + 1).with("accepted", false)
  }
  action campaign = all { s.campaign < 2, s' = s.with("campaign", s.campaign + 1).with("snapshot", s.snapshot + 1).with("accepted", false) }
  action trust = s' = s.with("trust", if (s.trust == Set()) Set(0) else Set()).with("accepted", false)
  action checkout = s' = s.with("checkout", if (s.checkout == "operator") "integration" else "operator").with("accepted", false)

  action confirm(c: int): bool = all {
    s.facts.exists(f => f.manifest == manifest(c) and f.outcome == "fail" and not(f.imported)),
    val e = { kind: "confirmed", manifest: manifest(c), claim: c, serial: s.serial + 1 }
    s' = s.with("journal", s.journal.append(e)).with("objects", s.objects.union(Set(e.serial)))
      .with("serial", s.serial + 1).with("accepted", false)
  }
  action resolve(c: int): bool = all {
    blocked(c), history(c), s.inputs.get(unit(c)) == 2,
    s.facts.exists(f => f.manifest == manifest(c) and f.outcome == "pass" and fresh(f)),
    events.exists(e => e.kind == "replay" and e.claim == c and e.manifest.with("campaign", s.campaign) == manifest(c)),
    val e = { kind: "resolved", manifest: manifest(c), claim: c, serial: s.serial + 1 }
    s' = s.with("journal", s.journal.append(e)).with("serial", s.serial + 1).with("accepted", false)
  }
  action replay(c: int): bool = all {
    blocked(c), history(c), s.inputs.get(unit(c)) == 2,
    val e = { kind: "replay", manifest: manifest(c), claim: c, serial: s.serial + 1 }
    s' = s.with("journal", s.journal.append(e)).with("serial", s.serial + 1).with("accepted", false)
  }
  action consolidate = s' = s.with("checkpoint", events).with("accepted", false)
  action deleteJournal = all { s.journal.foldl(true, (acc, e) => acc and s.checkpoint.contains(e)), s' = s.with("journal", List()).with("accepted", false) }
  action attemptDeleteJournal = if (s.journal.foldl(true, (acc, e) => acc and s.checkpoint.contains(e))) deleteJournal
    else s' = s.with("rejected", true).with("accepted", false)
  action expire = s' = s.with("logs", false).with("accepted", false)
  action rebuild = s' = s.with("index", events).with("accepted", false)
  action recordFinding = all { not(s.valid), s' = s.with("findings", s.findings.union(Set(0))).with("accepted", false) }
  action assignCriteria(refs: Set[int]): bool = {
    val resolved = refs != Set() and refs.subseteq(s.subjects) and s.valid and s.bindings
    s' = s.with("goal", if (resolved) Criteria(refs) else Unassigned).with("dispatched", false)
      .with("rejected", not(resolved)).with("accepted", false)
  }
  action assignRemediation(finding: int): bool = s'
    = s.with("goal", if (s.findings.contains(finding)) Remediation(finding) else Unassigned)
      .with("dispatched", false).with("rejected", not(s.findings.contains(finding))).with("accepted", false)
  action dispatch = {
    val resolved = match s.goal {
      | Criteria(refs) => refs != Set() and refs.subseteq(s.subjects) and s.valid and s.bindings
      | Remediation(finding) => s.findings.contains(finding)
      | Unassigned => false
    }
    s' = s.with("dispatched", resolved).with("rejected", not(resolved)).with("accepted", false)
  }
  action attemptHistoryWrite = s' = s.with("rejected", true).with("accepted", false)
  action restart = s' = s.with("alive", false).with("index", Set()).with("verified", -1).with("reviewed", -1)
    .with("validated", -1).with("accepted", false)
  action remoteAdvance = s' = s.with("remote", s.remote + 1).with("snapshot", s.snapshot + 1).with("accepted", false)
  action interruptedWrite = s' = s.with("partial", true).with("historyAvailable", false).with("accepted", false)
  action recovery = s' = s.with("partial", false).with("historyAvailable", true).with("index", events).with("accepted", false)
  action loseWitness = s' = s.with("objects", Set()).with("accepted", false)
  action relocate = s' = s.with("label", s.label + 1).with("bindings", false).with("snapshot", s.snapshot + 1).with("accepted", false)
  action loseLink = s' = s.with("link", false).with("accepted", false)

  action step = any {
    structural, discover, plan, execute, completion, incomplete, imported, verify, review, validate, attemptPush,
    endAttempt(false), endAttempt(true), retry, claimEdit, malformed, correct, changeDefinition,
    membership, context, defer, campaign, trust, checkout, consolidate, attemptDeleteJournal, expire, rebuild, changeEnvironment,
    { nondet c = Claims.oneOf() diagnostic(c) },
    { nondet c = Claims.oneOf() diagnosticLaunch(c) },
    { nondet c = Claims.oneOf() absent(c) },
    interruptedWrite, recovery, restart, remoteAdvance, loseWitness, relocate, loseLink,
    recordFinding, dispatch, attemptHistoryWrite,
    { nondet refs = Set(Set(), Set(0), Set(0, 99)).oneOf() assignCriteria(refs) },
    { nondet finding = Set(0, 1).oneOf() assignRemediation(finding) },
    { nondet c = Claims.oneOf() reuse(c) },
    { nondet c = Claims.oneOf() build(c) },
    { nondet u = Units.oneOf() changeInput(u) },
    { nondet c = Claims.oneOf() confirm(c) },
    { nondet c = Claims.oneOf() resolve(c) },
    { nondet c = Claims.oneOf() replay(c) }
  }

  val referenceEquivalence = current implies Claims.forall(c =>
    referenceRequired(c) == eligible(c) and
    (Set("pass", "ready", "fail").contains(coverage(c)) implies coverage(c) == reference(c)))
  val cacheSafety = Claims.forall(c => not(Set("pass", "ready").contains(coverage(c))) or
    (referenceEvidence(c) and history(c) and not(blocked(c))))
  val publicationSafety = s.accepted implies s.expected
  val safety = referenceEquivalence and cacheSafety and publicationSafety
  val publicationWitness = s.published
}
