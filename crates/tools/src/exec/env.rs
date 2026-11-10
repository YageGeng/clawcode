//! The environment an exec child starts with.

use std::collections::BTreeMap;

use config::{EnvInherit, ExecEnvPolicy};
use protocol::{ProductIdentity, SessionId, TurnId};

/// Variables kept by `inherit = "core"` on Unix, as in codex.
#[cfg(not(windows))]
const CORE_VARS: &[&str] = &[
    "PATH", "SHELL", "TMPDIR", "TEMP", "TMP", "HOME", "LANG", "LC_ALL",
    "LC_CTYPE", "LOGNAME", "USER",
];

/// Variables kept by `inherit = "core"` on Windows.
#[cfg(windows)]
const CORE_VARS: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "USERNAME",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
];

/// Names removed when `ignore_default_excludes` is false.
const DEFAULT_EXCLUDES: &[&str] = &["*KEY*", "*SECRET*", "*TOKEN*"];

/// Values every exec child gets so tools print plain, unpaged output.
const FORCED_ENV: [(&str, &str); 10] = [
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("LANG", "C.UTF-8"),
    ("LC_CTYPE", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("COLORTERM", ""),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GH_PAGER", "cat"),
    ("CLAWCODE_CI", "1"),
];

/// Derives a child environment from the configured policy.
pub struct ExecEnvironment<'a> {
    policy: &'a ExecEnvPolicy,
}

impl<'a> ExecEnvironment<'a> {
    /// Borrows the policy for one or more builds.
    #[must_use]
    pub const fn new(policy: &'a ExecEnvPolicy) -> Self {
        Self { policy }
    }

    /// Applies codex's order: inherit, default excludes, excludes, `set`,
    /// `include_only`; then the forced values and the Session identity win.
    pub fn build(
        &self,
        parent: impl IntoIterator<Item = (String, String)>,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> BTreeMap<String, String> {
        let mut env: BTreeMap<String, String> = parent
            .into_iter()
            .filter(|(name, _value)| self.admits(name))
            .collect();
        env.extend(self.policy.set.clone());
        env.retain(|name, _value| self.included(name));
        env.extend(Self::forced(session_id, turn_id));
        env
    }

    /// Whether an exported name survives the whole policy; shell snapshots use
    /// this so startup files cannot restore variables the policy removes.
    #[must_use]
    pub fn keeps(&self, name: &str) -> bool {
        self.admits(name) && self.included(name)
    }

    /// Variables that must win over a sourced snapshot: the `set` values that
    /// `include_only` admits, as in `build`, and the forced ones.
    #[must_use]
    pub fn overrides(
        &self,
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> BTreeMap<String, String> {
        let mut overrides: BTreeMap<String, String> = self
            .policy
            .set
            .iter()
            .filter(|(name, _value)| self.included(name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        overrides.extend(Self::forced(session_id, turn_id));
        overrides
    }

    /// The inherit, default-exclude, and exclude steps for one name.
    fn admits(&self, name: &str) -> bool {
        let policy = self.policy;
        let inherited = match policy.inherit {
            EnvInherit::All => true,
            EnvInherit::Core => {
                CORE_VARS.iter().any(|core| core.eq_ignore_ascii_case(name))
            }
            EnvInherit::None => false,
        };
        inherited
            && (policy.ignore_default_excludes
                || !DEFAULT_EXCLUDES
                    .iter()
                    .any(|pattern| pattern_matches(pattern, name)))
            && !policy
                .exclude
                .iter()
                .any(|pattern| pattern_matches(pattern, name))
    }

    /// The `include_only` step for one name; an empty list keeps everything.
    fn included(&self, name: &str) -> bool {
        self.policy.include_only.is_empty()
            || self
                .policy
                .include_only
                .iter()
                .any(|pattern| pattern_matches(pattern, name))
    }

    /// Variables that must survive a shell snapshot restoring its own exports.
    pub fn forced(
        session_id: &SessionId,
        turn_id: &TurnId,
    ) -> BTreeMap<String, String> {
        let mut forced: BTreeMap<String, String> = FORCED_ENV
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        forced.insert(
            ProductIdentity::SESSION_ID_ENV.to_string(),
            session_id.as_str().to_string(),
        );
        forced.insert(
            ProductIdentity::TURN_ID_ENV.to_string(),
            turn_id.as_str().to_string(),
        );
        forced
    }
}

/// Matches `name` against a `*`-only glob without regard to ASCII case.
fn pattern_matches(pattern: &str, name: &str) -> bool {
    let pattern = pattern.to_ascii_uppercase();
    let name = name.to_ascii_uppercase();
    let parts: Vec<&str> = pattern.split('*').collect();
    let [first, middle @ .., last] = parts.as_slice() else {
        return pattern == name;
    };
    if parts.len() == 1 {
        return pattern == name;
    }
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    for part in middle {
        match rest.find(part) {
            Some(index) => rest = rest.get(index + part.len()..).unwrap_or(""),
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}
