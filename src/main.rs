extern crate clap;
extern crate env_logger;
extern crate git2;
extern crate log;
extern crate regex;
extern crate semver;
extern crate tempfile;

use clap::{Parser, Subcommand, ValueEnum};
use git2::{DescribeFormatOptions, DescribeOptions, Repository};

use regex::Regex;
use semver::{BuildMetadata, Prerelease, Version};
use std::{fs, io::Read};

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

fn replace_version(path: &str, ver: &str) -> Result<(), String> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let re = Regex::new(r#"(?m)^version = ".+""#).unwrap();
            let replaced = re
                .replace(&contents, format!(r#"version = "{}""#, ver))
                .into_owned();
            match fs::write(path, replaced) {
                Ok(_) => Ok(()),
                Err(err) => Err(format!("Error writing `{}`: {}", path, err)),
            }
        }
        Err(err) => Err(format!("Error reading `{}`: {}", path, err)),
    }
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

fn get_cargo_version(path: &str) -> Result<Version, String> {
    match fs::read_to_string(path) {
        Ok(contents) => parse_cargo_version(&contents),
        Err(err) => Err(format!("Error reading `{}`: {}", path, err)),
    }
}

fn open_repository(path: &str) -> Result<Repository, String> {
    match Repository::discover(path) {
        Ok(repo) => Ok(repo),
        Err(err) => Err(format!("Error openning repository: {}", err)),
    }
}

fn get_latest_tag(repo: &Repository, abbrv_size: u32) -> Result<Version, String> {
    let mut opts = DescribeOptions::new();
    let opts = opts.describe_tags();

    let mut format_opts = DescribeFormatOptions::new();
    let format_opts = format_opts.abbreviated_size(abbrv_size);

    let version_str = repo
        .describe(&opts)
        .or(Err(format!("could not get tag")))?
        .format(Some(&format_opts))
        .unwrap();

    log::debug!("Found git version string {}", &version_str);
    let version_number = if version_str.chars().next().unwrap() == 'v' {
        &version_str[1..]
    } else {
        &version_str
    };
    let parsed_ver = Version::parse(version_number).or(Err(format!(
        "error parsing version from git tag {}",
        version_str
    )));
    parsed_ver
}

fn make_dev_prerelease(pre: Prerelease, mode: VersioningKind) -> Result<Prerelease, String> {
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
    let pre_parts: Vec<&str> = pre.split("-").collect();

    let (n_commits_from_last_tag, _last_commit) = match pre_parts[..] {
        [n_commits, last_commit] => match n_commits.parse::<i32>() {
            Ok(value) => Ok((value, last_commit)),
            Err(_) => Err(()),
        },
        _ => Err(()),
    }
    .or(Err(format!(
        "can't create dev prerelease from tag {}",
        pre_str
    )))?;
    let new_pre_str = mk_prerelease_str(n_commits_from_last_tag + 1, mode);
    Prerelease::new(&new_pre_str).or(Err(format!(
        "prerelease string {} is not valid",
        &new_pre_str
    )))
}

// Check if repo is in dirty state (some files were modified)
fn is_repo_dirty(repo: &Repository) -> bool {
    for entry in repo.statuses(None).unwrap().into_iter() {
        match entry.status() {
            git2::Status::IGNORED | git2::Status::WT_NEW => continue,
            _ => return true,
        }
    }
    return false;
}

fn run_sem_ver(
    paths: &Vec<String>,
    dry_run: bool,
    mode_arg: VersioningKindArg,
) -> Result<(), String> {
    let path = String::from("Cargo.toml");
    let repo = open_repository(&path)?;
    log::debug!("Opened repository at {}", &repo.path().to_str().unwrap());
    run_sem_ver_repo(&repo, dry_run, mode_arg)
}

fn run_sem_ver_repo(
    repo: &Repository,
    dry_run: bool,
    mode_arg: VersioningKindArg,
) -> Result<(), String> {
    let head_ref = get_head_ref(&repo);
    let path = String::from("Cargo.toml");

    if !is_repo_dirty(&repo) {
        println!("No changes detected. Exiting.");
        return Ok(());
    }

    log::debug!("repo HEAD is at {}", &head_ref[0..5]);

    let sem_ver = get_latest_tag(&repo, 4)?;
    log::debug!("Parsed git version {}", sem_ver);
    let cargo_ver = get_cargo_version(&path)?;
    //let mode = VersioningKind::SemverCommit((&head_ref[0..5]).to_string());
    let mode = match mode_arg {
        VersioningKindArg::PEP440 => VersioningKind::PEP440,
        VersioningKindArg::Semver => VersioningKind::Semver,
        VersioningKindArg::SemverCommit => {
            VersioningKind::SemverCommit((&head_ref[0..5]).to_string())
        }
    };
    let new_version = Version {
        major: sem_ver.major,
        minor: sem_ver.minor,
        patch: sem_ver.patch + 1,
        pre: make_dev_prerelease(sem_ver.pre, mode)?,
        build: BuildMetadata::EMPTY,
    };
    if cargo_ver <= new_version {
        if dry_run {
            println!("Created version number {} (dry-run)", new_version);
            Ok(())
        } else {
            println!("Created version number {}", new_version);
            replace_version(&path, &format!("{}", new_version))
        }
    } else {
        println!("Version number {} is up-to-date", cargo_ver);
        Ok(())
    }
}

fn get_head_ref(repo: &Repository) -> String {
    let revspec = repo.revparse("HEAD").unwrap();
    format!("{}", revspec.from().unwrap().id())
}

fn run_check_tags() -> Result<(), String> {
    let path = String::from(".");
    let repo = open_repository(&path)?;
    run_check_tags_repo(&repo)
}

fn run_check_tags_repo(repo: &Repository) -> Result<(), String> {
    if !is_repo_dirty(&repo) {
        println!("No changes detected");
        return Ok(());
    }

    let obj = repo.revparse_single(&"HEAD:Cargo.toml").unwrap();
    let blob = obj.as_blob().unwrap();
    let mut content = String::new();
    blob.content()
        .read_to_string(&mut content)
        .or(Err(format!("Error reading file from index.")))?;
    let cargo_version = parse_cargo_version(&content)?;
    log::debug!("Found cargo version {}", &cargo_version);
    let sem_ver = get_latest_tag(&repo, 0)?;
    log::debug!("Current repo version {}", &sem_ver);

    if cargo_version.pre.is_empty() {
        if sem_ver < cargo_version {
            return Err(format!(
                "Please tag the release commit before adding new changes."
            ));
        }
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

    use git2::RepositoryInitOptions;
    use std::fs::File;
    use std::io::Write;
    use std::path::Path;
    use tempfile::TempDir;
    use Repository;

    use crate::{run_check_tags_repo, run_sem_ver_repo, VersioningKindArg};

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
            let name = format!("f{n}");
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
        let id = index.write_tree().unwrap();
        let sig = repo.signature().unwrap();
        let tree = repo.find_tree(id).unwrap();
        let parent = repo
            .find_commit(repo.head().unwrap().target().unwrap())
            .unwrap();
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "another commit",
            &tree,
            &[&parent],
        )
        .unwrap();
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
        assert!(run_sem_ver_repo(&repo, true, VersioningKindArg::Semver).is_ok());
    }

    #[test]
    fn test_dirty_repo() {
        let (td, repo) = repo_init();
        setup_repo(&td, &repo);
        let mut index = repo.index().unwrap();
        File::create(&td.path().join("f0"))
            .unwrap()
            .write_all("new".as_bytes())
            .unwrap();
        index.add_path(Path::new("f0")).unwrap();
        assert!(run_check_tags_repo(&repo).is_ok());
        assert!(run_sem_ver_repo(&repo, true, VersioningKindArg::Semver).is_ok());
        assert!(false);
    }
}
