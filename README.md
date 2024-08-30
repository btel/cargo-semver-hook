# cargo git-semver

Cargo command to automatically update version number.


## Usage

It's designated to be used with [pre-commit](https://pre-commit.com/) but can be also used independently of it.

* automatically bump package version in Cargo.toml to match the latest git tag

  ```
  cargo git-semver bump
  ```

* check whether a git tag was created f

  ```
  cargo git-semver check-tags
  ```

## pre-commit configuration

Install pre-commit and add the following configuration to your project dir:

```
# .pre-commit-config.yaml
repos:
    -   repo: https://github.com/btel/cargo-semver-hook
        rev: 0.6.0
        hooks:
          - id: git-semver-bump
            args: ['--mode', 'pep440']
            files: '^(python|src|doc|Cargo\.toml|pyproject\.toml)'
            always_run: false
          - id: git-semver-check-tags
```