//! XDG user folders, parsed as data rather than executed as shell.
use std::path::PathBuf;

pub fn shortcut_path(suffix: &str) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    if !home.is_absolute() {
        return None;
    }
    if suffix.is_empty() {
        return Some(home);
    }
    let key = match suffix {
        "Downloads" => "XDG_DOWNLOAD_DIR",
        "Desktop" => "XDG_DESKTOP_DIR",
        "Documents" => "XDG_DOCUMENTS_DIR",
        _ => return None,
    };
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    match std::fs::read_to_string(config.join("user-dirs.dirs")) {
        Ok(contents) => xdg_user_dir(&contents, key, &home),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(home.join(suffix)),
        Err(_) => None,
    }
}

/// Parse the quoted xdg-user-dirs format without executing shell code.
/// Only a leading $HOME is expanded. Invalid or absent entries are not guessed.
fn xdg_user_dir(contents: &str, key: &str, home: &std::path::Path) -> Option<PathBuf> {
    let value = contents.lines().rev().find_map(|line| {
        let (name, value) = line.trim().split_once('=')?;
        (name.trim() == key).then_some(value.trim())
    })?;
    let value = value.strip_prefix('"')?.strip_suffix('"')?;
    let (base, value) = if value == "$HOME" {
        return Some(home.to_path_buf());
    } else if let Some(value) = value.strip_prefix("$HOME/") {
        (Some(home), value.trim_start_matches('/'))
    } else {
        (None, value)
    };
    let mut parsed = String::new();
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        match character {
            '\\' => match chars.next()? {
                escaped @ ('\\' | '"' | '$' | '`') => parsed.push(escaped),
                _ => return None,
            },
            '"' | '$' | '`' => return None,
            character if character.is_control() => return None,
            character => parsed.push(character),
        }
    }
    let path = base.map_or_else(|| PathBuf::from(&parsed), |home| home.join(&parsed));
    path.is_absolute().then_some(path)
}

#[cfg(test)]
mod linux_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn localized_redirected_and_disabled_xdg_folders() {
        let home = Path::new("/home/test");
        assert_eq!(
            xdg_user_dir("XDG_DOWNLOAD_DIR=\"$HOME/下载\"", "XDG_DOWNLOAD_DIR", home),
            Some(home.join("下载"))
        );
        assert_eq!(
            xdg_user_dir(
                "XDG_DESKTOP_DIR=\"/mnt/data/desktop\"",
                "XDG_DESKTOP_DIR",
                home
            ),
            Some(PathBuf::from("/mnt/data/desktop"))
        );
        assert_eq!(
            xdg_user_dir("XDG_DESKTOP_DIR=\"$HOME\"", "XDG_DESKTOP_DIR", home),
            Some(home.to_path_buf())
        );
        assert_eq!(
            xdg_user_dir("XDG_DESKTOP_DIR=\"$HOME//nested\"", "XDG_DESKTOP_DIR", home),
            Some(home.join("nested"))
        );
    }

    #[test]
    fn xdg_parser_is_not_a_shell() {
        for value in [
            "relative",
            "$OTHER/path",
            "$(touch /tmp/never)",
            "`whoami`",
            "${HOME}/x",
            "$HOME/x\" # ignored",
        ] {
            let config = format!("XDG_DOWNLOAD_DIR=\"{value}\"");
            assert!(
                xdg_user_dir(&config, "XDG_DOWNLOAD_DIR", Path::new("/home/test")).is_none(),
                "{value}"
            );
        }
        assert!(xdg_user_dir(
            "# XDG_DOWNLOAD_DIR=\"/tmp/a\"",
            "XDG_DOWNLOAD_DIR",
            Path::new("/home/test")
        )
        .is_none());
        assert_eq!(
            xdg_user_dir(
                r#"XDG_DOWNLOAD_DIR="$HOME/a\$b\\c\"d""#,
                "XDG_DOWNLOAD_DIR",
                Path::new("/home/test")
            ),
            Some(PathBuf::from("/home/test/a$b\\c\"d"))
        );
    }
}
