# treetop

An htop-style view of a [treehouse](https://github.com/kunchenguid/treehouse) worktree pool.
Mark trees, then return them to the pool or destroy them, with git's view of each tree on screen so nothing unpushed goes by accident.

treetop only talks to treehouse through its CLI (`treehouse status --json`, `return`, `destroy`), so a treehouse upgrade keeps working without a fork.

## Install

On Linux (x86_64 or arm64):

```sh
curl -fsSL https://raw.githubusercontent.com/anthonyb8/treetop/main/install.sh | sh
```

It installs the latest release to `~/.local/bin` when that is on PATH, else `/usr/local/bin`, after checking the download against its published SHA-256.
Set `TREETOP_VERSION=v0.1.0` before `sh` to install a specific release.
From source: `cargo install --git https://github.com/anthonyb8/treetop`.

## Release

Bump `version` in `Cargo.toml`, then push a matching tag:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

`.github/workflows/release.yml` refuses a tag that does not match `Cargo.toml`, runs the tests, builds static musl binaries for x86_64 and arm64, and publishes them with checksums as a GitHub release.

## Use

Run `treetop` from anywhere inside a pooled repository.

| Key | Does |
|---|---|
| `j` `k` / arrows, `g` `G` | move |
| Space | mark or unmark the tree under the cursor |
| `u` | clear the marks |
| `/` | filter by number, branch or holder; Enter keeps it, Esc clears it |
| `r` | return the marked trees, or the one under the cursor, to the pool |
| `D` | destroy them from disk |
| Enter | quit and print the tree's path |
| `q`, Esc | quit |

The columns are what a destroy would lose.
CHANGED counts uncommitted files and UNPUSHED counts commits on no remote; `?` means git could not read a held tree, which deserves a look before it goes.
PROCS counts processes treehouse found running inside the tree.

The list refreshes every 10 seconds and after every action.
`treehouse status` scans every process on the machine, so a shorter interval would keep a core busy.

## What the actions run

Both suspend the screen and run in the terminal, so treehouse's own prompts work.

- **Return** stops the tree's stack with `harness stack down` when the harness is on PATH, then runs `treehouse return`, which still asks before discarding uncommitted changes.
- **Destroy** prints `treehouse destroy`'s dry run with every `--include-*` flag, asks again, and only then stops the stack and runs it with `--yes`.

The tree treetop was started in is never a target, because returning or destroying it would kill the shell standing in it.

## cd on Enter

A program cannot change its parent shell's directory, so Enter prints the path on stdout and draws the UI on stderr.
Wrap it to jump in:

```sh
tp() { local dir; dir=$(treetop) || return; [[ -n $dir ]] && cd "$dir"; }
```
