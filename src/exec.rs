use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Talked to from an app bundle, Diffy inherits launchd's minimal PATH and
/// cannot see Homebrew or per-user installs. These cover the usual places
/// `gh` and `claude` land.
const EXTRA_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"];

/// Builds a command for a tool that may live outside launchd's PATH. The
/// widened PATH is also handed to the child, because Claude Code shells out
/// to `gh` and `git` itself.
pub fn command(program: &str) -> Command {
    let dirs = search_dirs();
    let mut command = match resolve(program, &dirs) {
        Some(path) => Command::new(path),
        None => Command::new(program),
    };
    if let Ok(path) = env::join_paths(&dirs) {
        command.env("PATH", path);
    }
    command
}

fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect())
        .unwrap_or_default();
    dirs.extend(EXTRA_DIRS.iter().map(PathBuf::from));
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
        dirs.push(home.join(".claude/local"));
        // A launchd PATH skips the shell profile, so the mise shims directory
        // (where mise puts every tool it manages, `gh` included) is invisible.
        let mise = env::var_os("MISE_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share/mise"));
        dirs.push(mise.join("shims"));
    }
    dirs
}

fn resolve(program: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_dirs_cover_gui_launch_installs() {
        let dirs = search_dirs();
        assert!(dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(dirs.iter().any(|dir| dir.ends_with(".local/bin")));
        let mise = env::var_os("MISE_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env::var_os("HOME").unwrap()).join(".local/share/mise")
            });
        assert!(dirs.contains(&mise.join("shims")));
    }

    #[test]
    fn resolve_finds_binaries_outside_the_first_path_entry() {
        let dirs = [PathBuf::from("/nonexistent"), PathBuf::from("/bin")];
        assert_eq!(resolve("sh", &dirs), Some(PathBuf::from("/bin/sh")));
        assert_eq!(resolve("diffy-no-such-program", &dirs), None);
    }
}
