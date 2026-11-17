pub const ALIAS_PREFIX: &str = "repos/";

pub fn normalize_alias(alias: &str) -> Option<String> {
    let alias = alias.trim();
    if alias.is_empty()
        || alias.len() > 32
        || alias
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'-'))
    {
        return None;
    }
    Some(alias.to_ascii_uppercase())
}

pub fn parse_github_repository(url: &str) -> Option<String> {
    let path = url
        .trim()
        .trim_end_matches('/')
        .strip_prefix("https://github.com/")?;
    parse_repository_path(path.strip_suffix(".git").unwrap_or(path))
}

pub fn parse_repository_path(path: &str) -> Option<String> {
    let (owner, repository) = path.split_once('/')?;
    if !valid_repository_part(owner) || !valid_repository_part(repository) {
        return None;
    }
    Some(format!("{owner}/{repository}"))
}

fn valid_repository_part(part: &str) -> bool {
    !part.is_empty()
        && !matches!(part, "." | "..")
        && part.len() <= 100
        && part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_paths_and_urls() {
        for path in [
            "../repo",
            "owner/..",
            "owner/.",
            "owner/repo/issues",
            "a/b?x",
            "a/b#x",
            "a/%2e",
        ] {
            assert!(parse_repository_path(path).is_none(), "{path}");
            assert!(parse_github_repository(&format!("https://github.com/{path}")).is_none());
        }
        assert_eq!(
            parse_repository_path("Owner/repo.git").as_deref(),
            Some("Owner/repo.git")
        );
    }
}
