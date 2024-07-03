extern crate anyhow;
extern crate clap;
extern crate env_logger;
extern crate git2;
extern crate log;
extern crate regex;
extern crate semver;
extern crate tempfile;
extern crate thiserror;

use clap::{Parser, Subcommand, ValueEnum};
use git2::{DescribeFormatOptions, DescribeOptions, DiffOptions, Repository};

use anyhow::{anyhow, Context, Error};
use regex::Regex;
use semver::{BuildMetadata, Prerelease, Version};
use std::{fs, io::Read, path::Path};
use thiserror::Error;
use VersionHookError::Outdated;

#[derive(Error, Debug)]
pub enum VersionHookError {
    #[error("Cargo version `{cargo_ver}` is not up-to-date with repo `{repo_ver}`")]
    Outdated {
        cargo_ver: Version,
        repo_ver: Version,
    },
    #[error("Program returned with error: `{0}`")]
    Other(String),
}

#[derive(Parser, Debug)]
#[command(name = "git-semver")]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum VersioningKindArg {
    PEP440,
    Semver,
    SemverCommit,
}

enum VersioningKind {
    PEP440,
    Semver,
    SemverCommit(String),
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Bump cargo version from latest tag
    Bump {
        path: Vec<String>,
        #[arg(long, value_enum)]
        mode: VersioningKindArg,
        #[arg(long, action)]
        dry_run: bool,
    },
    /// Check if last release was tagged
    CheckTags {},
}

fn replace_version(path: &str, ver: &str) -> anyhow::Result<()> {
    let contents = fs::read_to_string(path)?;
    let re = Regex::new(r#"(?m)^version = ".+""#)
        .context("could not parse version number from Cargo.toml")?;
    let replaced = re
        .replace(&contents, format!(r#"version = "{}""#, ver))
        .into_owned();
    fs::write(path, replaced).with_context(|| format!("Error writing `{}`", path))
}

fn parse_cargo_version(contents: &str) -> Result<Version, String> {
    let re = Regex::new(r#"(?m)^version = "(.+)""#).unwrap();
    let ver_captures = re
        .captures_iter(contents)
        .next()
        .ok_or(String::from("version number not found"))?;
    let version = &ver_captures[1];

    Version::parse(version).or(Err(format!(
        "error parsing version from Cargo.toml {}",
        version
    )))
}

fn get_cargo_version(repo: &Repository) -> anyhow::Result<Version> {
    let cargo_version = match get_cargo_toml(repo) {
        Ok(contents) => parse_cargo_version(&contents),
        Err(err) => Err(format!("Error reading Cargo.toml`: {}", err)),
    };
    cargo_version.map_err(|err| VersionHookError::Other(format!("{}", err)).into())
}

fn open_repository(path: &str) -> anyhow::Result<Repository> {
    Repository::discover(path).context("Error openning repository")
}

fn get_latest_tag(repo: &Repository, abbrv_size: u32) -> anyhow::Result<Version> {
    let mut opts = DescribeOptions::new();
    let opts = opts.describe_tags();

    let mut format_opts = DescribeFormatOptions::new();
    let format_opts = format_opts.abbreviated_size(abbrv_size);

    let version_str = repo
        .describe(opts)
        .context("could not get tag")?
        .format(Some(format_opts))
        .unwrap();

    log::debug!("Found git version string {}", &version_str);
    let version_number = version_str.strip_prefix('v').unwrap_or(&version_str);

    Version::parse(version_number)
        .with_context(|| format!("error parsing version from git tag {}", version_str))
}

fn make_dev_prerelease(
    pre: Prerelease,
    mode: VersioningKind,
    is_dirty: bool,
) -> anyhow::Result<Prerelease> {
    let mk_prerelease_str = |n_commits, mode| -> String {
        match mode {
            VersioningKind::PEP440 => format!("dev{}", n_commits),
            VersioningKind::Semver => format!("dev.{}", n_commits),
            VersioningKind::SemverCommit(base_commit) => {
                format!("dev.{}.g{}", n_commits, base_commit)
            }
        }
    };

    if pre.is_empty() {
        return Ok(Prerelease::new(&mk_prerelease_str(1, mode)).unwrap());
    }
    let pre_str = pre.as_str();
    let pre_parts: Vec<&str> = pre.split('-').collect();

    let (n_commits_from_last_tag, _last_commit) = match pre_parts[..] {
        [n_commits, last_commit] => n_commits
            .parse::<i32>()
            .and_then(|parsed| Ok((parsed, last_commit)))
            .with_context(|| format!("can't create dev prerelease from tag {}", pre_str)),
        _ => Err(VersionHookError::Other("wrong tag format".to_string()).into()),
    }?;
    let new_pre_str = if is_dirty {
        mk_prerelease_str(n_commits_from_last_tag + 1, mode)
    } else {
        mk_prerelease_str(n_commits_from_last_tag, mode)
    };
    Prerelease::new(&new_pre_str)
        .with_context(|| format!("prerelease string {} is not valid", &new_pre_str))
}

// Check if repo is in dirty state (some files were modified)
fn is_repo_dirty(repo: &Repository, filetype: Option<&str>) -> bool {
    for entry in repo.statuses(None).unwrap().into_iter() {
        if let Some(extension) = filetype {
            if let Some(s) = entry.path() {
                if !s.ends_with(extension) {
                    continue;
                }
            } else {
                continue;
            };
        };
        match entry.status() {
            git2::Status::IGNORED | git2::Status::WT_NEW => continue,
            _ => return true,
        }
    }
    false
}

// get cargo.toml from staging area

fn get_cargo_toml(repo: &Repository) -> Result<String, String> {
    let index = repo
        .index()
        .unwrap()
        .get_path(Path::new("Cargo.toml"), 0)
        .unwrap();
    let blob = repo.find_blob(index.id).unwrap();
    let mut content = String::new();
    blob.content()
        .read_to_string(&mut content)
        .or(Err("Error reading file from index.".to_string()))?;
    Ok(content)
}

fn run_sem_ver(
    _paths: &[String],
    dry_run: bool,
    mode_arg: VersioningKindArg,
) -> anyhow::Result<()> {
    let path = String::from("Cargo.toml");
    let repo = open_repository(&path)?;
    log::debug!("Opened repository at {}", &repo.path().to_str().unwrap());
    run_sem_ver_repo(&repo, dry_run, mode_arg, Some("rs"))
}

fn check_rs_files_changed(
    repo: &Repository,
    old_commit: &str,
    new_commit: &str,
) -> Result<bool, git2::Error> {
    let old_commit = repo.revparse_single(old_commit)?;
    let new_commit = repo.find_commit(repo.revparse_single(new_commit)?.id())?;

    let old_tree = old_commit.peel_to_tree()?;
    let new_tree = new_commit.tree()?;

    let mut diff_options = DiffOptions::new();
    diff_options.include_typechange(true).ignore_filemode(false);

    let diff = repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(&mut diff_options))?;

    for delta in diff.deltas() {
        if let Some(file_path) = delta.new_file().path() {
            if file_path.extension().map_or(false, |ext| ext == "rs") {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn run_sem_ver_repo(
    repo: &Repository,
    dry_run: bool,
    mode_arg: VersioningKindArg,
    filetype: Option<&str>,
) -> anyhow::Result<()> {
    let head_ref = get_head_ref(repo);

    log::debug!("repo HEAD is at {}", &head_ref[0..5]);

    let sem_ver = get_latest_tag(repo, 4)?;
    log::debug!("Parsed git version {}", sem_ver);
    let cargo_ver = get_cargo_version(repo)?;
    //let mode = VersioningKind::SemverCommit((&head_ref[0..5]).to_string());

    let is_dirty = is_repo_dirty(repo, filetype);

    let latest_tag_str = get_latest_tag(repo, 0)?.to_string();

    log::debug!("latest tag is {}", &latest_tag_str);

    let changed_from_last_version =
        check_rs_files_changed(repo, &latest_tag_str, "HEAD").unwrap_or(true);

    if !is_dirty && !changed_from_last_version {
        println!("No rust files changed since last tag {}", latest_tag_str);
        return Ok(());
    };

    let mode = match mode_arg {
        VersioningKindArg::PEP440 => VersioningKind::PEP440,
        VersioningKindArg::Semver => VersioningKind::Semver,
        VersioningKindArg::SemverCommit => VersioningKind::SemverCommit(head_ref[0..5].to_string()),
    };
    let new_version = Version {
        major: sem_ver.major,
        minor: sem_ver.minor,
        patch: sem_ver.patch + 1,
        pre: make_dev_prerelease(sem_ver.pre, mode, is_dirty)?,
        build: BuildMetadata::EMPTY,
    };
    if cargo_ver < new_version {
        if dry_run {
            println!("Created version number {} (dry-run)", new_version);
        } else {
            println!("Created version number {}", new_version);
            replace_version(
                repo.workdir().unwrap().join("Cargo.toml").to_str().unwrap(),
                &format!("{}", new_version),
            )?;
        }
        Err(VersionHookError::Outdated {
            cargo_ver,
            repo_ver: new_version,
        }
        .into())
    } else {
        println!("Version number {} is up-to-date", cargo_ver);
        Ok(())
    }
}

fn get_head_ref(repo: &Repository) -> String {
    let revspec = repo.revparse("HEAD").unwrap();
    format!("{}", revspec.from().unwrap().id())
}

fn run_check_tags() -> anyhow::Result<()> {
    let path = String::from(".");
    let repo = open_repository(&path)?;
    match run_check_tags_repo(&repo) {
        Ok(_) => Ok(()),
        Err(s) => Err(VersionHookError::Other(s).into()),
    }
}

fn run_check_tags_repo(repo: &Repository) -> Result<(), String> {
    if !is_repo_dirty(repo, None) {
        println!("No changes detected");
        return Ok(());
    }

    let obj = repo.revparse_single("HEAD:Cargo.toml").unwrap();
    let blob = obj.as_blob().unwrap();
    let mut content = String::new();
    blob.content()
        .read_to_string(&mut content)
        .or(Err("Error reading file from index.".to_string()))?;
    let cargo_version = parse_cargo_version(&content)?;
    log::debug!("Found cargo version {}", &cargo_version);
    let sem_ver = get_latest_tag(repo, 0).map_err(|err| format!("{}", err))?;
    log::debug!("Current repo version {}", &sem_ver);

    if cargo_version.pre.is_empty() && sem_ver < cargo_version {
        return Err("Please tag the release commit before adding new changes.".to_string());
    }
    Ok(())
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Bump {
            path,
            mode,
            dry_run,
        } => run_sem_ver(&path, dry_run, mode),
        Commands::CheckTags {} => run_check_tags(),
    };

    let exit_code = match result {
        Ok(_) => 0,
        Err(err) => {
            eprintln!("{}", err);
            1
        }
    };
    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {

    use git2::{Index, RepositoryInitOptions};
    use std::fs::File;
    use std::io::Write;
    use std::path::Path;
    use tempfile::TempDir;
    use Repository;

    use crate::{run_check_tags_repo, run_sem_ver_repo, VersioningKindArg};

    pub fn commit(repo: &Repository, index: &mut Index, msg: &str) {
        let id = index.write_tree().unwrap();
        let sig = repo.signature().unwrap();
        let tree = repo.find_tree(id).unwrap();
        let parent = repo
            .find_commit(repo.head().unwrap().target().unwrap())
            .unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&parent])
            .unwrap();
    }

    pub fn repo_init() -> (TempDir, Repository) {
        let td = TempDir::new().unwrap();
        let mut opts = RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = Repository::init_opts(td.path(), &opts).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "name").unwrap();
            config.set_str("user.email", "email").unwrap();
            let mut index = repo.index().unwrap();
            let id = index.write_tree().unwrap();

            let tree = repo.find_tree(id).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial\n\nbody", &tree, &[])
                .unwrap();
        }
        (td, repo)
    }

    fn setup_repo(td: &TempDir, repo: &Repository) {
        let mut index = repo.index().unwrap();
        let cargo_contents = "[package]\nname = \"test package\"\nversion = \"0.1.0\"\n";
        for n in 0..8 {
            let name = format!("f{n}.rs");
            File::create(&td.path().join(&name))
                .unwrap()
                .write_all(name.as_bytes())
                .unwrap();
            index.add_path(Path::new(&name)).unwrap();
        }
        let cargotoml_path = td.path().join("Cargo.toml");
        File::create(&cargotoml_path)
            .unwrap()
            .write_all(cargo_contents.as_bytes())
            .unwrap();
        index.add_path(Path::new("Cargo.toml")).unwrap();
        commit(repo, &mut index, "another commit");
        let sig = repo.signature().unwrap();
        repo.tag(
            "0.1.0",
            &repo.revparse_single("HEAD").unwrap(),
            &sig,
            "initial version",
            false,
        )
        .unwrap();
    }

    #[test]
    fn test_clean_repo() {
        let (td, repo) = repo_init();
        setup_repo(&td, &repo);
        assert!(run_check_tags_repo(&repo).is_ok());
        assert!(run_sem_ver_repo(&repo, true, VersioningKindArg::Semver, None).is_ok());
    }

    #[test]
    fn test_dirty_repo() {
        let _ = env_logger::builder().is_test(true).try_init();
        let (td, repo) = repo_init();
        setup_repo(&td, &repo);
        let mut index = repo.index().unwrap();
        File::create(&td.path().join("f0"))
            .unwrap()
            .write_all("new".as_bytes())
            .unwrap();
        index.add_path(Path::new("f0")).unwrap();
        assert!(run_check_tags_repo(&repo).is_ok());
        assert!(run_sem_ver_repo(&repo, true, VersioningKindArg::Semver, Some("rs")).is_ok());
        assert_eq!(
            format!(
                "{}",
                run_sem_ver_repo(&repo, false, VersioningKindArg::Semver, None).unwrap_err()
            ),
            "Cargo version `0.1.0` is not up-to-date with repo `0.1.1-dev.1`".to_string()
        );

        let cargotoml = std::fs::read_to_string(td.path().join("Cargo.toml")).unwrap();
        assert!(cargotoml.contains("0.1.1-dev.1"));
    }

    #[test]
    fn test_dev_commit() {
        let _ = env_logger::builder().is_test(true).try_init();
        let (td, repo) = repo_init();
        setup_repo(&td, &repo);
        let mut index = repo.index().unwrap();
        File::create(&td.path().join("f0.rs"))
            .unwrap()
            .write_all("new".as_bytes())
            .unwrap();
        index.add_path(Path::new("f0.rs")).unwrap();
        commit(&repo, &mut index, "yet another commit");
        assert_eq!(
            format!(
                "{}",
                run_sem_ver_repo(&repo, false, VersioningKindArg::Semver, None).unwrap_err()
            ),
            "Cargo version `0.1.0` is not up-to-date with repo `0.1.1-dev.1`".to_string()
        );

        let cargotoml = std::fs::read_to_string(td.path().join("Cargo.toml")).unwrap();
        assert!(cargotoml.contains("0.1.1-dev.1"));
    }
}
