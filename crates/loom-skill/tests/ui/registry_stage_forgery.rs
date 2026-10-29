use loom_skill::registry::{ApplicableRegistry, MaterializedRegistry, SkillRegistry};

fn forge(registry: SkillRegistry) {
    let _ = ApplicableRegistry::from_registry(registry);
    let _ = MaterializedRegistry::new(Vec::new());
    deserialize::<MaterializedRegistry>();
}

fn deserialize<T: for<'de> serde::Deserialize<'de>>() {}

fn main() {}
