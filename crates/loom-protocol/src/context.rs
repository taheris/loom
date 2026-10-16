//! Read-only package documents captured by the dispatch owner, without filesystem access.

use std::fmt;

use loom_events::identifier::SpecLabel;

/// Phase-specific disclosure; assigned worker goals are supplied separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Planning,
    Decomposition,
    Implementation,
    Review,
}

/// One captured document, not a live path lookup or verification result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub path: String,
    pub body: String,
}

/// Contract and acceptance from the same discovered owner; support stays on demand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub label: SpecLabel,
    pub contract: Document,
    pub acceptance: Document,
    pub support_scope: String,
}

/// Immutable selected context, with new planning anchors explicitly distinguished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    purpose: Purpose,
    packages: Vec<Package>,
    new_anchors: Vec<SpecLabel>,
    snapshot_root: Option<String>,
}

impl Context {
    pub const fn new(
        purpose: Purpose,
        packages: Vec<Package>,
        new_anchors: Vec<SpecLabel>,
    ) -> Self {
        Self {
            purpose,
            packages,
            new_anchors,
            snapshot_root: None,
        }
    }

    #[must_use]
    pub fn with_snapshot_root(mut self, root: String) -> Self {
        self.snapshot_root = Some(root);
        self
    }

    pub fn packages(&self) -> &[Package] {
        &self.packages
    }

    pub fn contract_path(&self, label: &SpecLabel) -> Option<&str> {
        self.packages
            .iter()
            .find(|package| &package.label == label)
            .map(|package| package.contract.path.as_str())
    }

    pub fn package(&self, label: &SpecLabel) -> Option<Self> {
        self.packages
            .iter()
            .find(|package| &package.label == label)
            .map(|package| Self::new(self.purpose, vec![package.clone()], Vec::new()))
    }
}

impl fmt::Display for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "## Applicable Package Snapshot\n\nThese are dispatched contract and acceptance snapshots, not passing verification or publication permission. Uncertain relevance broadens obligations. Assigned worker goals remain mandatory; models and supporting detail load selectively on demand, never as a replacement for those goals."
        )?;
        for package in &self.packages {
            writeln!(
                f,
                "\n### {}\n\nContract and invariants (`{}`):\n\n<contract-snapshot>\n{}\n</contract-snapshot>\n\nDetailed acceptance: `{}`. Supporting context scope: `{}` (retrieve only as needed).",
                package.label,
                package.contract.path,
                package.contract.body,
                package.acceptance.path,
                package.support_scope
            )?;
            if let Some(root) = &self.snapshot_root {
                writeln!(
                    f,
                    "Dispatched acceptance snapshot (retrieve without re-resolving): `{root}/{}/acceptance.md`.",
                    package.label
                )?;
            }
            if matches!(self.purpose, Purpose::Decomposition | Purpose::Review)
                && package.acceptance.path != package.contract.path
            {
                writeln!(
                    f,
                    "\n<acceptance-snapshot>\n{}\n</acceptance-snapshot>",
                    package.acceptance.body
                )?;
            }
        }
        for label in &self.new_anchors {
            writeln!(
                f,
                "\nNew planning anchor `{label}`: no existing package was discovered; author its contract and detailed acceptance under the project's conventions."
            )?;
        }
        Ok(())
    }
}
