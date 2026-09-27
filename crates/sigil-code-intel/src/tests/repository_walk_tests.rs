use std::{cell::Cell, collections::BTreeSet};

use super::*;

#[test]
fn raw_entry_budget_stops_before_sorting_or_filtering_an_unbounded_directory() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join(".gitignore"), "ignored-*\n")?;
    let file_type = fs::metadata(temp.path().join(".gitignore"))?.file_type();
    let reads = Cell::new(0usize);
    let opens = Cell::new(0usize);
    let consumed = visit_repository_files_with_reader(
        temp.path(),
        17,
        |_| false,
        |_| panic!("ignored entries must not be visited"),
        &Gitignore::empty(),
        |directory| {
            opens.set(opens.get() + 1);
            let directory = directory.to_path_buf();
            let reads = &reads;
            Ok(std::iter::from_fn(move || {
                reads.set(reads.get() + 1);
                assert!(
                    reads.get() <= 17,
                    "must stop reading before collecting/sorting"
                );
                Some(Ok(RepositoryEntry {
                    path: directory.join(format!("ignored-{}", reads.get())),
                    file_type,
                }))
            }))
        },
    )?;
    assert_eq!(consumed, 17);
    assert_eq!(reads.get(), 17);
    assert_eq!(opens.get(), 1);
    Ok(())
}

#[test]
fn exhausted_raw_budget_does_not_open_pending_child_directories() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let directory_type = fs::metadata(temp.path())?.file_type();
    let reads = Cell::new(0usize);
    let consumed = visit_repository_files_with_reader(
        temp.path(),
        4,
        |_| false,
        |_| panic!("fixture contains only directories"),
        &Gitignore::empty(),
        |directory| {
            assert_eq!(
                directory,
                temp.path(),
                "budget must prevent opening child cursors"
            );
            let directory = directory.to_path_buf();
            let reads = &reads;
            Ok(std::iter::from_fn(move || {
                reads.set(reads.get() + 1);
                assert!(reads.get() <= 4);
                Some(Ok(RepositoryEntry {
                    path: directory.join(format!("child-{}", reads.get())),
                    file_type: directory_type,
                }))
            }))
        },
    )?;
    assert_eq!(consumed, 4);
    assert_eq!(reads.get(), 4);
    Ok(())
}

#[test]
fn raw_budget_counts_errors_and_filtered_entries_then_sorts_only_the_prefix() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("type-source"), "")?;
    let file_type = fs::metadata(temp.path().join("type-source"))?.file_type();
    let mut visited = Vec::new();
    let consumed = visit_repository_files_with_reader(
        temp.path(),
        4,
        |relative| relative == Path::new("filtered"),
        |path| {
            visited.push(
                path.file_name()
                    .expect("fixture file has a name")
                    .to_owned(),
            );
            Ok(true)
        },
        &Gitignore::empty(),
        |directory| {
            let directory = directory.to_path_buf();
            Ok([
                Some("z.py"),
                None,
                Some("filtered"),
                Some("b.py"),
                Some("a.py"),
            ]
            .into_iter()
            .map(move |name| match name {
                Some(name) => Ok(RepositoryEntry {
                    path: directory.join(name),
                    file_type,
                }),
                None => Err(io::Error::other("unreadable directory entry")),
            }))
        },
    )?;
    assert_eq!(consumed, 4);
    assert_eq!(visited, ["b.py", "z.py"]);
    Ok(())
}

fn skip_git_metadata(relative: &Path) -> bool {
    relative
        .components()
        .any(|component| component.as_os_str() == ".git")
}

#[test]
fn repository_policies_preserve_parent_negation_ignore_precedence_and_nested_git_behavior()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("workspace");
    fs::create_dir_all(root.join("nested/.git/info"))?;
    fs::create_dir_all(root.join(".git/info"))?;
    fs::write(temp.path().join(".gitignore"), "parent.py\n")?;
    fs::write(temp.path().join(".ignore"), "ancestor-ignore.py\n")?;
    fs::write(
        root.join(".gitignore"),
        "root.py\n!priority.py\nignore-wins.py\nnested/*.py\n!nested/reinclude.py\n!ancestor-ignore.py\n",
    )?;
    fs::write(root.join(".ignore"), "priority.py\n!ignore-wins.py\n")?;
    fs::write(root.join(".git/info/exclude"), "exclude.py\n")?;
    fs::write(
        root.join("nested/.gitignore"),
        "!near.py\n!parent.py\n!repo-excluded.py\n",
    )?;
    fs::write(root.join("nested/.git/info/exclude"), "repo-excluded.py\n")?;
    for name in [
        "visible.py",
        ".hidden.py",
        "parent.py",
        "root.py",
        "priority.py",
        "ignore-wins.py",
        "exclude.py",
        "ancestor-ignore.py",
        "nested/near.py",
        "nested/reinclude.py",
        "nested/parent.py",
        "nested/across.py",
        "nested/repo-excluded.py",
    ] {
        fs::write(root.join(name), "pass\n")?;
    }
    let mut visited = BTreeSet::new();
    visit_repository_files_with_reader(
        &root,
        256,
        skip_git_metadata,
        |path| {
            if path.extension().is_some_and(|extension| extension == "py") {
                visited.insert(path.strip_prefix(&root)?.to_path_buf());
            }
            Ok(true)
        },
        &Gitignore::empty(),
        read_directory,
    )?;
    let expected = [
        ".hidden.py",
        "ignore-wins.py",
        "nested/near.py",
        "nested/parent.py",
        "nested/reinclude.py",
        "nested/repo-excluded.py",
        "visible.py",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect::<BTreeSet<_>>();
    assert_eq!(visited, expected);

    // Compare to the previous policy engine on a complete tree, independently of our matcher.
    let mut previous = ignore::WalkBuilder::new(&root);
    previous
        .hidden(false)
        .parents(true)
        .require_git(false)
        .git_global(false)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != ".git");
    let previous = previous
        .build()
        .flatten()
        .filter(|entry| {
            entry.file_type().is_some_and(|kind| kind.is_file())
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "py")
        })
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&root)
                .expect("fixture entry stays in its root")
                .to_path_buf()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(visited, previous);
    Ok(())
}

#[test]
fn global_ignore_is_lower_priority_than_repository_negation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join(".gitignore"), "!rescued.py\n")?;
    for name in ["global.py", "rescued.py", "visible.py"] {
        fs::write(temp.path().join(name), "pass\n")?;
    }
    // Inject the same compiled policy that production loads from global Git configuration,
    // without changing process-wide HOME or Git configuration in parallel tests.
    let mut global = GitignoreBuilder::new(temp.path());
    global.add_line(None, "global.py")?;
    global.add_line(None, "rescued.py")?;
    let mut visited = BTreeSet::new();
    visit_repository_files_with_reader(
        temp.path(),
        32,
        |_| false,
        |path| {
            if path.extension().is_some_and(|extension| extension == "py") {
                visited.insert(
                    path.file_name()
                        .expect("fixture file has a name")
                        .to_owned(),
                );
            }
            Ok(true)
        },
        &global.build()?,
        read_directory,
    )?;
    assert_eq!(
        visited,
        ["rescued.py", "visible.py"]
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect()
    );
    let global_file = temp.path().join("global-ignore-policy");
    fs::write(&global_file, "global.py\nrescued.py\n")?;
    let mut previous = ignore::WalkBuilder::new(temp.path());
    previous
        .hidden(false)
        .parents(true)
        .require_git(false)
        .git_global(false)
        .current_dir(temp.path())
        .follow_links(false);
    assert!(previous.add_ignore(&global_file).is_none());
    let previous = previous
        .build()
        .flatten()
        .filter(|entry| {
            entry.file_type().is_some_and(|kind| kind.is_file())
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "py")
        })
        .map(|entry| entry.file_name().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(visited, previous);
    Ok(())
}

#[test]
fn linked_worktree_common_exclude_matches_git_aware_walkbuilder() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("linked-worktree");
    let common = temp.path().join("main/.git");
    let git_dir = common.join("worktrees/linked-worktree");
    fs::create_dir_all(&root)?;
    fs::create_dir_all(common.join("info"))?;
    fs::create_dir_all(&git_dir)?;
    fs::write(
        root.join(".git"),
        format!("gitdir: {}\n", git_dir.display()),
    )?;
    fs::write(git_dir.join("commondir"), "../..\n")?;
    fs::write(common.join("info/exclude"), "excluded.py\nrescued.py\n")?;
    fs::write(root.join(".gitignore"), "!rescued.py\n")?;
    for name in ["excluded.py", "rescued.py", "visible.py"] {
        fs::write(root.join(name), "pass\n")?;
    }
    let collect = || -> Result<BTreeSet<PathBuf>> {
        let mut files = BTreeSet::new();
        visit_repository_files_with_reader(
            &root,
            32,
            skip_git_metadata,
            |path| {
                if path.extension().is_some_and(|extension| extension == "py") {
                    files.insert(path.strip_prefix(&root)?.to_path_buf());
                }
                Ok(true)
            },
            &Gitignore::empty(),
            read_directory,
        )?;
        Ok(files)
    };
    let observed = collect()?;
    assert_eq!(
        observed,
        ["rescued.py", "visible.py"]
            .into_iter()
            .map(PathBuf::from)
            .collect()
    );

    // ignore's git-aware mode resolves the same worktree metadata. Its require_git(false) mode
    // skips probing .git's type, so it cannot serve as the worktree-pointer oracle.
    let mut previous = ignore::WalkBuilder::new(&root);
    previous
        .hidden(false)
        .parents(true)
        .require_git(true)
        .git_global(false)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != ".git");
    let previous = previous
        .build()
        .flatten()
        .filter(|entry| {
            entry.file_type().is_some_and(|kind| kind.is_file())
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "py")
        })
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&root)
                .expect("fixture entry stays in its root")
                .to_path_buf()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(observed, previous);

    fs::write(git_dir.join("commondir"), format!("{}\n", common.display()))?;
    assert_eq!(
        collect()?,
        observed,
        "absolute common directories are supported"
    );
    fs::write(
        root.join(".git"),
        "gitdir: ../main/.git/worktrees/linked-worktree\n",
    )?;
    assert_eq!(
        collect()?,
        observed,
        "relative gitdir pointers are based at the worktree"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn repository_walk_never_follows_file_or_directory_symlinks() -> Result<()> {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("workspace");
    fs::create_dir(&root)?;
    fs::write(temp.path().join("outside.py"), "pass\n")?;
    symlink(temp.path().join("outside.py"), root.join("linked.py"))?;
    symlink(temp.path(), root.join("linked-directory"))?;
    let consumed = visit_repository_files_with_reader(
        &root,
        32,
        |_| false,
        |_| panic!("aliases must be skipped"),
        &Gitignore::empty(),
        read_directory,
    )?;
    assert_eq!(consumed, 2);
    Ok(())
}

#[test]
fn repository_walk_has_no_depth_cutoff_within_its_entry_budget() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut path = temp.path().to_path_buf();
    for _ in 0..40 {
        path.push("d");
        fs::create_dir(&path)?;
    }
    let leaf = path.join("deep.py");
    fs::write(&leaf, "pass\n")?;
    let mut visited = Vec::new();
    let consumed = visit_repository_files_with_reader(
        temp.path(),
        64,
        |_| false,
        |path| {
            visited.push(path.to_path_buf());
            Ok(true)
        },
        &Gitignore::empty(),
        read_directory,
    )?;
    assert_eq!(visited, [leaf]);
    assert_eq!(consumed, 41);
    Ok(())
}
