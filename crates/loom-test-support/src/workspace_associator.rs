//! An external-process membership fixture, not an implementation of a host identity registry.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Status-only adapter with checkout-local test registrations and fault controls beside its program.
pub struct Fixture {
    pub program: PathBuf,
    pub root: PathBuf,
}

impl Fixture {
    /// Create the adapter outside the disposable checkouts so a checkout move retains its selection.
    ///
    /// # Errors
    /// Returns fixture file creation failures.
    pub fn new(root: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(root)?;
        let program = root.join("adapter");
        std::fs::write(&program, crate::bash_script(SCRIPT))?;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            program,
            root: root.to_path_buf(),
        })
    }

    /// Establish independent fixture membership; the adapter never registers an unknown operator.
    ///
    /// # Errors
    /// Returns fixture registration write failures.
    pub fn register(checkout: &Path, membership: &str) -> io::Result<()> {
        std::fs::write(checkout.join(".git/fixture-membership"), membership)
    }

    /// Reject this target before association, preserving subsequent preparation's negative control.
    ///
    /// # Errors
    /// Returns fault-control write failures.
    pub fn reject(&self, target: &Path) -> io::Result<()> {
        std::fs::write(
            self.root.join("reject"),
            target.as_os_str().as_encoded_bytes(),
        )
    }
}

const SCRIPT: &str = r#"set -euo pipefail
[[ "$#" -eq 4 && "$1" == --operator && "$3" == --checkout ]] || exit 64
operator="$2"
checkout="$4"
root="${BASH_SOURCE[0]%/*}"
[[ -d "$operator/.git" && ! -L "$operator/.git" ]] || exit 65
[[ -d "$checkout/.git" && ! -L "$checkout/.git" ]] || exit 65
[[ -f "$operator/.git/fixture-membership" ]] || { printf 'unknown operator\n' >&2; exit 20; }
membership="$(< "$operator/.git/fixture-membership")"
if [[ -f "$root/reject" && "$checkout" == "$(< "$root/reject")" ]]; then
    printf 'fixture admission rejected\n' >&2
    exit 21
fi
if [[ -f "$checkout/.git/fixture-membership" ]]; then
    [[ "$(< "$checkout/.git/fixture-membership")" == "$membership" ]] || { printf 'conflicting membership\n' >&2; exit 22; }
else
    printf '%s' "$membership" > "$checkout/.git/fixture-membership"
fi
printf '%s\t%s\n' "$operator" "$checkout" >> "$root/calls"
if [[ -f "$root/action" ]]; then
    source "$root/action"
fi
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn fixture_adapter_confirms_operator_refuses_conflicts_and_retries_idempotently()
    -> io::Result<()> {
        let root = tempfile::tempdir()?;
        let adapter = Fixture::new(&root.path().join("fixture"))?;
        let operator = root.path().join("operator");
        let target = root.path().join("target");
        for path in [&operator, &target] {
            std::fs::create_dir_all(path.join(".git"))?;
        }
        let invoke = || {
            Command::new(&adapter.program)
                .arg("--operator")
                .arg(&operator)
                .arg("--checkout")
                .arg(&target)
                .output()
        };
        assert_eq!(invoke()?.status.code(), Some(20));
        assert!(!operator.join(".git/fixture-membership").exists());
        Fixture::register(&operator, "first")?;
        assert!(invoke()?.status.success());
        assert!(invoke()?.status.success());
        assert_eq!(
            std::fs::read_to_string(target.join(".git/fixture-membership"))?,
            "first"
        );
        Fixture::register(&target, "independent")?;
        assert_eq!(invoke()?.status.code(), Some(22));
        assert_eq!(
            std::fs::read_to_string(target.join(".git/fixture-membership"))?,
            "independent"
        );
        Ok(())
    }
}
