use loom_skill::registry::{ApplicableRegistry, MaterializedRegistry, SkillRegistry};

fn forge(registry: SkillRegistry) {
    let _ = ApplicableRegistry::from_registry(registry);
    let _ = MaterializedRegistry::new(Vec::new());
    let _: MaterializedRegistry = serde_json::from_str(r#"{"skills":[]}"#).unwrap();
}

fn main() {}
