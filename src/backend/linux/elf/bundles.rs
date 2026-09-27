use std::io::Read;

use super::*;

impl Discovery {
    pub(super) fn select_bundles(&mut self, paths: &[PathBuf], home: &Path) -> Result<()> {
        for path in paths {
            let path = normalized(&expand_host_home(path, home)?)?;
            self.check(&path)?;
            let root = path.canonicalize().context("resolve runtime bundle")?;
            self.check(&root)?;
            ensure!(root.is_dir(), "runtime bundle must be a directory");
            ensure!(
                !self.workspace.starts_with(&root)
                    && !home.starts_with(&root)
                    && !self.forbidden.iter().any(|path| path.starts_with(&root)),
                "runtime bundle contains workspace, home, credentials or control state: {}",
                root.display()
            );
            ensure!(
                ![
                    "/",
                    "/usr",
                    "/usr/bin",
                    "/usr/sbin",
                    "/usr/lib",
                    "/usr/lib64",
                    "/usr/lib/aarch64-linux-gnu",
                    "/usr/lib/x86_64-linux-gnu",
                    "/usr/libexec",
                    "/usr/share",
                    "/usr/local",
                    "/usr/local/bin",
                    "/usr/local/sbin",
                    "/usr/local/lib",
                    "/usr/local/lib64",
                    "/usr/local/libexec",
                    "/usr/local/share",
                    "/bin",
                    "/sbin",
                    "/lib",
                    "/lib64",
                    "/etc",
                    "/opt",
                    "/var",
                    "/home",
                    "/nix",
                    "/nix/store",
                ]
                .iter()
                .any(|broad| root == Path::new(broad)),
                "select an application bundle, not a broad host directory: {}",
                root.display()
            );
            ensure!(
                crate::backend::linux::find_submounts(&root)?.is_empty(),
                "runtime bundle contains submounts: {}",
                root.display()
            );
            self.link(path.clone(), root.clone())?;
            self.bundles.insert(path, root);
        }
        Ok(())
    }

    pub(super) fn discover_bundles(&mut self) -> Result<()> {
        let mut pending: Vec<_> = self
            .bundles
            .values()
            .cloned()
            .map(|path| (path, 0))
            .collect();
        let mut visited = HashSet::new();
        let mut objects = BTreeSet::new();
        let mut links = Vec::new();
        while let Some((path, depth)) = pending.pop() {
            if !visited.insert(path.clone()) {
                continue;
            }
            ensure!(
                visited.len() <= 100_000 && depth <= 128,
                "runtime bundle tree is too large"
            );
            self.check(&path)?;
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("inspect runtime bundle entry {}", path.display()))?;
            if metadata.is_symlink() {
                let target = path
                    .parent()
                    .context("bundle entry has no parent")?
                    .join(fs::read_link(&path)?);
                let lookup = normalized(&target)?;
                self.check(&lookup)?;
                let canonical = target
                    .canonicalize()
                    .with_context(|| format!("resolve runtime bundle link {}", path.display()))?;
                self.check(&canonical)?;
                ensure!(
                    lookup.canonicalize()? == canonical,
                    "bundle link traverses an alias before '..': {}",
                    path.display()
                );
                links.push((path, lookup, canonical));
            } else if metadata.is_dir() {
                for entry in fs::read_dir(&path)? {
                    ensure!(pending.len() < 100_000, "runtime bundle tree is too large");
                    pending.push((entry?.path(), depth + 1));
                }
            } else {
                ensure!(
                    metadata.is_file(),
                    "runtime bundle contains a special file: {}",
                    path.display()
                );
                ensure!(
                    metadata.nlink() == 1 || (metadata.uid() == 0 && metadata.mode() & 0o022 == 0),
                    "runtime file has mutable hard-link aliases: {}",
                    path.display()
                );
                let mut magic = Vec::new();
                fs::File::open(&path)?.take(4).read_to_end(&mut magic)?;
                if magic == b"\x7fELF" {
                    let bytes = contents(&path)?;
                    let elf = parse(&bytes, &path)?;
                    let names: BTreeSet<_> =
                        [path.file_name().and_then(|name| name.to_str()), elf.soname]
                            .into_iter()
                            .flatten()
                            .collect();
                    for name in names {
                        self.bundled_libraries
                            .entry(name.into())
                            .or_default()
                            .push(path.clone());
                    }
                    objects.insert(path);
                }
            }
        }
        for (path, _, target) in &links {
            if objects.contains(target)
                && let Some(name) = path.file_name().and_then(|name| name.to_str())
            {
                self.bundled_libraries
                    .entry(name.into())
                    .or_default()
                    .push(target.clone());
            }
            if !self.bundles.values().any(|root| target.starts_with(root))
                && !self.files.contains(target)
                && target.is_file()
                && self.authorize(target).is_ok()
            {
                let mut magic = Vec::new();
                fs::File::open(target)?.take(4).read_to_end(&mut magic)?;
                if magic == b"\x7fELF" {
                    objects.insert(target.clone());
                }
            }
        }
        for object in objects {
            if !self.visited.iter().any(|(path, _)| path == &object) {
                self.object(&object, &[])?;
            }
        }
        for (path, lookup, target) in links {
            ensure!(
                self.bundles.values().any(|root| target.starts_with(root))
                    || self.files.contains(&target),
                "runtime bundle link escapes selected resources: {} -> {}; select the target bundle or executable explicitly",
                path.display(),
                target.display()
            );
            self.link(lookup, target)?;
        }
        Ok(())
    }

    pub(super) fn plan(self) -> RuntimePlan {
        let roots: BTreeSet<_> = self.bundles.values().cloned().collect();
        let read_only_paths = roots
            .iter()
            .filter(|path| {
                !roots
                    .iter()
                    .any(|parent| parent != *path && path.starts_with(parent))
            })
            .cloned()
            .chain(
                self.files
                    .into_iter()
                    .filter(|path| !roots.iter().any(|root| path.starts_with(root))),
            )
            .collect();
        let system_links = self
            .links
            .into_iter()
            .filter(|(path, _)| {
                !roots.iter().any(|root| path.starts_with(root))
                    && !self
                        .bundles
                        .keys()
                        .any(|root| path != root && path.starts_with(root))
            })
            .collect();
        RuntimePlan {
            read_only_paths,
            system_links,
            path: BIN.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_plan_omits_child_mounts_and_preserves_only_external_aliases() {
        let root = tempfile::tempdir().unwrap();
        let mut discovery = Discovery::new(root.path(), &root.path().join("home")).unwrap();
        discovery.bundles = [
            ("/opt/current".into(), "/srv/application".into()),
            (
                "/srv/application/data".into(),
                "/srv/application/data".into(),
            ),
        ]
        .into();
        discovery.files = [
            "/srv/application/data/file".into(),
            "/usr/lib/fixture.so".into(),
        ]
        .into();
        discovery.links = [
            ("/opt/current".into(), "/srv/application".into()),
            (
                "/opt/current/data/file".into(),
                "/srv/application/data/file".into(),
            ),
            (
                "/srv/application/data/alias".into(),
                "/srv/application/data/file".into(),
            ),
            (
                "/run/slopbox/bin/tool".into(),
                "/srv/application/bin/tool".into(),
            ),
        ]
        .into();
        let plan = discovery.plan();
        assert_eq!(
            plan.read_only_paths,
            vec![
                PathBuf::from("/srv/application"),
                PathBuf::from("/usr/lib/fixture.so")
            ]
        );
        assert_eq!(
            plan.system_links,
            vec![
                (
                    PathBuf::from("/opt/current"),
                    PathBuf::from("/srv/application")
                ),
                (
                    PathBuf::from("/run/slopbox/bin/tool"),
                    PathBuf::from("/srv/application/bin/tool")
                ),
            ]
        );
    }
}
