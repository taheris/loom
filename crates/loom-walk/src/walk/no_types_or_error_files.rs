//! RS-5: no central `types.rs` / `error.rs` at any crate's `src/`
//! root. Each crate uses nested `<domain>/{type,error}.rs` modules.

use super::util::{immediate_children, rel, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "RS-5 no central types.rs / error.rs at crate root";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let crates_dir = root.join("crates");
    let mut violations = Vec::new();
    for crate_dir in immediate_children(&crates_dir) {
        let src = crate_dir.join("src");
        for forbidden in ["types.rs", "error.rs"] {
            let candidate = src.join(forbidden);
            if candidate.is_file() {
                violations.push(format!(
                    "{}:1 forbidden central `{}` — split into nested `<domain>/{}` modules",
                    rel(&root, &candidate),
                    forbidden,
                    forbidden,
                ));
            }
        }
    }
    verdict_from(RULE, violations)
}
