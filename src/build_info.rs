pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = env!("XJPCODE_GIT_SHA");
pub const BUILD_PROFILE: &str = env!("XJPCODE_BUILD_PROFILE");
pub const BUILD_TARGET: &str = env!("XJPCODE_BUILD_TARGET");
pub const CLI_VERSION: &str = env!("XJPCODE_BUILD_IDENTITY");

pub const SOURCE_PROVIDER: &str = "xjp-codebase";
pub const CANONICAL_REPOSITORY: &str = "https://github.com/RuoqiJin/xjpcode.git";
pub const DEFAULT_BRANCH: &str = "master";
pub const MIRROR_REPOSITORY: &str = "https://github.com/RuoqiJin/xjpcode.git";

#[derive(Debug, Clone, Copy)]
pub struct BuildInfo {
    pub identity: &'static str,
    pub version: &'static str,
    pub git_sha: &'static str,
    pub git_dirty: bool,
    pub profile: &'static str,
    pub target: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct SourceAuthority {
    pub provider: &'static str,
    pub canonical_repository: &'static str,
    pub default_branch: &'static str,
    pub mirror_repository: &'static str,
}

pub fn current() -> BuildInfo {
    BuildInfo {
        identity: CLI_VERSION,
        version: VERSION,
        git_sha: GIT_SHA,
        git_dirty: env!("XJPCODE_GIT_DIRTY") == "true",
        profile: BUILD_PROFILE,
        target: BUILD_TARGET,
    }
}

pub fn source_authority() -> SourceAuthority {
    SourceAuthority {
        provider: SOURCE_PROVIDER,
        canonical_repository: CANONICAL_REPOSITORY,
        default_branch: DEFAULT_BRANCH,
        mirror_repository: MIRROR_REPOSITORY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_provenance_has_a_full_source_sha() {
        assert_eq!(GIT_SHA.len(), 40);
        assert!(GIT_SHA.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(CLI_VERSION.contains(GIT_SHA));
        assert_eq!(current().identity, CLI_VERSION);
    }

    #[test]
    fn source_authority_is_codebase_master() {
        let authority = source_authority();
        assert_eq!(authority.provider, "xjp-codebase");
        assert_eq!(authority.default_branch, "master");
        assert_eq!(
            authority.canonical_repository,
            "https://github.com/RuoqiJin/xjpcode.git"
        );
    }
}
