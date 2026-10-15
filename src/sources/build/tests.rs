use super::recipe::Optimization;
use super::*;

fn recipe(extra: &str) -> Recipe {
    toml::from_str(&format!("asset='src.tar'\nformat='source'\nbinaries={{app='bin/app'}}\n[build]\ncommands=[['cc','main.c','-o','app']]\noutput='stage'\nscratch_mib=8\n{extra}")).unwrap()
}
fn source() -> Source {
    Source {
        kind: Kind::Github,
        url: String::new(),
        token_env: None,
        packages: BTreeMap::new(),
    }
}

#[test]
fn default_optimization_inherits_system_without_adding_flags() {
    let recipe = recipe("");
    validate(&recipe, &source()).unwrap();
    let build = recipe.build.unwrap();
    assert_eq!(build.optimization, Optimization::default());
    let generated = profile(&build, Path::new("/etc/makepkg.conf"));
    assert!(generated.contains("source_makepkg_config '/etc/makepkg.conf'"));
    assert!(!generated.contains("CFLAGS+="));
    assert!(!generated.contains("RUSTFLAGS+="));
    assert!(!generated.contains("MAKEFLAGS="));
}

#[test]
fn explicit_optimizations_are_scoped_and_preserve_existing_flags() {
    let recipe = recipe("jobs=2\n[build.optimization]\ncpu='native'\nlevel=2\nlto=false\n");
    validate(&recipe, &source()).unwrap();
    let generated = profile(
        recipe.build.as_ref().unwrap(),
        Path::new("/tmp/custom ' profile.conf"),
    );
    assert!(generated.contains("CFLAGS+=\" -march=native\""));
    assert!(generated.contains("RUSTFLAGS+=\" -C target-cpu=native\""));
    assert!(generated.contains(" -O2"));
    assert!(generated.contains("CARGO_BUILD_JOBS=2"));
    assert!(!generated.contains("-Ofast"));
}

#[test]
fn invalid_build_policies_are_rejected_before_execution() {
    for extra in [
        "jobs=0",
        "scratch_inodes=0",
        "[build.optimization]\nlevel=4",
    ] {
        assert!(validate(&recipe(extra), &source()).is_err(), "{extra}");
    }
    let mut recipe = recipe("");
    recipe.build.as_mut().unwrap().output = "../outside".into();
    assert!(validate(&recipe, &source()).is_err());
    recipe.build.as_mut().unwrap().output = "stage".into();
    recipe.format = Format::Archive;
    assert!(validate(&recipe, &source()).is_err());
    recipe.format = Format::Source;
    recipe.build = None;
    assert!(validate(&recipe, &source()).is_err());
}

#[test]
fn commands_are_literal_and_only_known_placeholders_are_accepted() {
    let mut recipe = recipe("");
    recipe.build.as_mut().unwrap().commands = vec![vec![
        "cc".into(),
        "{output}/app".into(),
        "$(touch /tmp/no)".into(),
    ]];
    validate(&recipe, &source()).unwrap();
    let quote = crate::utils::shell_quote("$(touch /tmp/no)");
    assert_eq!(quote, "'$(touch /tmp/no)'");
    recipe.build.as_mut().unwrap().commands[0][1] = "{unknown}".into();
    assert!(validate(&recipe, &source()).is_err());
    recipe.build.as_mut().unwrap().commands = vec![vec![]];
    assert!(validate(&recipe, &source()).is_err());
}

#[test]
fn git_sources_have_distinct_integrity_and_revision_rules() {
    let mut recipe = recipe("");
    recipe.build.as_mut().unwrap().git = Some("https://github.com/example/app.git".into());
    assert!(validate(&recipe, &source()).is_err());
    recipe.asset.clear();
    validate(&recipe, &source()).unwrap();
    recipe.build.as_mut().unwrap().revision = Some("a".repeat(40));
    assert!(validate(&recipe, &source()).is_err());
    recipe.tag = Some("v1.0.0".into());
    validate(&recipe, &source()).unwrap();
    recipe.build.as_mut().unwrap().git = Some("file:///etc".into());
    assert!(validate(&recipe, &source()).is_err());
    recipe.build.as_mut().unwrap().git = Some("https://user:password@example.com/repo.git".into());
    assert!(validate(&recipe, &source()).is_err());
}
