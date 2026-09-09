//! Host-resolved launch capabilities and secret-reference plumbing.
//!
//! A [`LaunchCapability`] names the declarative process profile it was
//! resolved from (profile bytes are pinned by the trusted plugin package)
//! and carries the resolved executable allowlist, cwd ceiling and
//! environment references. Credentials stay host-owned: plugins only ever
//! see opaque references that the broker resolves when spawning.

use crate::services::authorization::{AuthorizationService, LaunchCapability};

/// A process profile as pinned by a plugin package (subset used for launch
/// resolution; the full constraint schema is T12b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSource {
    pub harness_id: String,
    pub package_digest: String,
    pub profile_name: String,
}

/// Resolve a profile into a launch capability and install it.
pub fn install_profile_capability(
    service: &mut AuthorizationService,
    source: &ProfileSource,
    allowed_executables: Vec<String>,
    cwd_root: Option<String>,
    raw: bool,
    env_references: Vec<String>,
) {
    service.install_capability(LaunchCapability {
        profile: format!("{}/{}", source.harness_id, source.profile_name),
        allowed_executables,
        cwd_root,
        raw,
        env_references,
    });
}

/// A resolved secret reference: an opaque broker handle. The material never
/// leaves the host; the reference is only meaningful inside spawn calls the
/// host itself performs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SecretReference {
    pub broker_id: String,
    pub scope: String,
}

/// Resolve which environment entries a profile may inject: non-secret
/// literals from the profile plus broker references explicitly granted to
/// the task.
pub fn resolve_profile_env(
    profile_literals: std::collections::BTreeMap<String, String>,
    granted_references: &[SecretReference],
) -> Vec<String> {
    let mut names: Vec<String> = profile_literals.into_keys().collect();
    names.extend(
        granted_references
            .iter()
            .map(|reference| reference.broker_id.clone()),
    );
    names.sort();
    names.dedup();
    names
}
