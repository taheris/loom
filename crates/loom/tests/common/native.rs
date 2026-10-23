use std::path::PathBuf;
use std::process::Command;

pub fn fixture(system: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(root.join(".gitignore"), "/.loom/\n").unwrap();
    std::fs::write(root.join("README.md"), "# Native input\n").unwrap();
    std::fs::write(
        root.join("verification.nix"),
        include_str!("../../../../nix/verification.nix"),
    )
    .unwrap();
    let bash = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("bash"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let store: PathBuf = bash.components().take(4).collect();
    let builder = serde_json::to_string(bash.to_str().unwrap()).unwrap();
    let store = serde_json::to_string(store.to_str().unwrap()).unwrap();
    std::fs::write(root.join("flake.nix"), format!(r#"{{
      outputs = {{ self }}: let
        checked = import ./verification.nix;
        source = checked.resource {{ root = ./.; origin = "README.md"; }};
      in {{ checks.{system}.heading = checked.check {{
        resources = [ source ];
        evaluation = map (origin: checked.resource {{ root = ./.; inherit origin; }}) [ "flake.nix" "verification.nix" ];
        build = sources: builtins.derivation {{
          name = "native-heading";
          system = "{system}";
          builder = builtins.appendContext {builder} {{ {store} = {{ path = true; }}; }};
          src = builtins.head sources;
          args = [ "-c" "read -r heading < \"$src\"; [[ \"$heading\" == '# '* ]] && echo success > \"$out\"" ];
        }};
      }}; }};
    }}"#)).unwrap();
    std::fs::create_dir(root.join("specs")).unwrap();
    std::fs::write(root.join("specs/native.md"), format!("## Success Criteria\n- Actual native check [system](nix build .#checks.{system}.heading)\n")).unwrap();
    dir
}
