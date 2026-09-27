use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub(super) enum Identity {
    Named(String),
    Disabled(bool),
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    pub git_identity: Option<Identity>,
    pub accounts: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Rule {
    pub paths: Vec<PathBuf>,
    pub git_identity: Option<Identity>,
    pub accounts: Option<Vec<String>>,
}

pub(super) fn select(
    defaults: &Selection,
    rules: &[Rule],
    home: &Path,
    workspace: &Path,
) -> Result<(Selection, Vec<usize>)> {
    let mut matching = Vec::new();
    for (index, rule) in rules.iter().enumerate() {
        ensure!(!rule.paths.is_empty(), "workspace rule has no paths");
        let mut depth = None;
        for path in &rule.paths {
            let path = crate::fs_util::expand_host_home(path, home)?;
            ensure!(path.is_absolute(), "workspace rule paths must be absolute");
            let path = super::canonicalize_allow_missing(&path)?;
            if workspace.starts_with(&path) {
                depth = Some(depth.unwrap_or(0).max(path.components().count()));
            }
        }
        if let Some(depth) = depth {
            matching.push((depth, index));
        }
    }
    matching.sort_unstable();
    ensure!(
        matching.windows(2).all(|pair| pair[0].0 != pair[1].0),
        "equally specific workspace rules overlap"
    );
    let mut selected = defaults.clone();
    for (_, index) in &matching {
        let rule = &rules[*index];
        if let Some(identity) = &rule.git_identity {
            selected.git_identity = Some(identity.clone());
        }
        if let Some(accounts) = &rule.accounts {
            selected.accounts = Some(accounts.clone());
        }
    }
    ensure!(
        !matches!(selected.git_identity, Some(Identity::Disabled(true))),
        "git_identity must be an identity id or false"
    );
    if let Some(Identity::Named(name)) = &selected.git_identity {
        ensure!(!name.is_empty(), "Git identity id is empty");
    }
    if let Some(accounts) = &selected.accounts {
        let mut names = std::collections::HashSet::new();
        ensure!(
            accounts
                .iter()
                .all(|name| !name.is_empty() && names.insert(name)),
            "account selection contains empty or duplicate names"
        );
    }
    Ok((
        selected,
        matching.into_iter().map(|(_, index)| index).collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn defaults_and_directory_restrictions_do_not_depend_on_rule_order() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        let workspace = home.join("work/restricted/repository");
        fs::create_dir_all(&workspace).unwrap();
        let defaults: Selection =
            toml::from_str("git_identity = 'personal'\naccounts = ['personal']").unwrap();
        let child: Rule = toml::from_str("paths = ['~/work/restricted']\naccounts = []").unwrap();
        let parent: Rule = toml::from_str(
            "paths = ['~/work', '~/other']\ngit_identity = 'work'\naccounts = ['work']",
        )
        .unwrap();
        let (selection, sources) = select(&defaults, &[child, parent], &home, &workspace).unwrap();
        assert_eq!(selection.git_identity, Some(Identity::Named("work".into())));
        assert_eq!(selection.accounts, Some(vec![]));
        assert_eq!(sources, [1, 0]);
        let rule: Rule = toml::from_str("paths = ['~/work']\ngit_identity = false").unwrap();
        let (outside, _) =
            select(&defaults, &[rule], &home, &home.join("work-other/repo")).unwrap();
        assert_eq!(outside, defaults);
    }

    #[test]
    fn canonical_scope_aliases_are_ambiguous_and_future_directories_need_not_exist() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        fs::create_dir(home.join("work")).unwrap();
        std::os::unix::fs::symlink(home.join("work"), home.join("alias")).unwrap();
        let first: Rule = toml::from_str("paths = ['~/work']\naccounts = []").unwrap();
        let alias: Rule = toml::from_str("paths = ['~/alias']\naccounts = []").unwrap();
        assert!(
            select(
                &Selection::default(),
                &[first, alias],
                &home,
                &home.join("work")
            )
            .is_err()
        );
        let future: Rule =
            toml::from_str("paths = ['~/future']\ngit_identity = false\naccounts = []").unwrap();
        assert_eq!(
            select(&Selection::default(), &[future], &home, &home.join("other"))
                .unwrap()
                .0,
            Selection::default()
        );
    }
}
