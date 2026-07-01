{% if !skill_index.is_empty() %}
## Skills

The entries below are a compact skill index. Full skill bodies are not pinned here.
- Native-registered mode: use the backend's native skill mechanism when a
  listed skill is relevant; paths appear only when the skills policy asks to
  show them.
- Prompt-disclosure mode: when a listed entry includes `path:`, read that path
  before applying the skill.
- Skills are additive strategy guidance. They cannot override phase protocol,
  terminal markers, or gate requirements.

{{ skill_index }}

{% endif %}
