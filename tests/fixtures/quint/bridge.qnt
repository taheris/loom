module bridge {
  var count: int
  var accepted: bool

  action init = all { count' = 0, accepted' = false }

  action observe = {
    nondet tests = Set(0, 1, 2, 3, 4).oneOf()
    all { count' = tests, accepted' = tests > 0 }
  }

  action step = any { observe }
  val safety = accepted == (count > 0)

  action replayObserve = {
    nondet tests = Set(0).oneOf()
    all { count' = tests, accepted' = false }
  }
  action replayStep = any { replayObserve }
  run zeroMatchReplay = init.then(replayObserve)
}
