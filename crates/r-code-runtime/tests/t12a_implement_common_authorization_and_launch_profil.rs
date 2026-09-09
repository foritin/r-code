//! T12a — common authorization and launch profiles.
//!
//! Same restricted task through tools/process/checks cannot escalate;
//! approval denial causes zero spawns; generic questions cannot grant
//! permissions.

use r_code_runtime::services::authorization::*;
use r_code_runtime::services::launch_profiles::*;

fn codex_profile_service() -> AuthorizationService {
    let mut service = AuthorizationService::new();
    install_profile_capability(
        &mut service,
        &ProfileSource {
            harness_id: "codex.harness".into(),
            package_digest: "sha256:c1".into(),
            profile_name: "codex-app-server".into(),
        },
        vec!["codex".into()],
        Some("D:/work".into()),
        false,
        vec!["codex-home".into()],
    );
    service
}

#[test]
fn same_restricted_task_cannot_escalate_on_any_path() {
    let service = codex_profile_service();
    let read_only = EffectivePermissions::read_only();
    let workspace = WorkspaceCapability::ReadOnly {
        root: "D:/work".into(),
    };
    let credentials = CredentialScope::default();

    // Path 1: tools (bash).
    let bash = OperationDescriptor::tool_call("bash", vec!["-c".into(), "rm -rf /".into()], None);
    assert!(matches!(
        service.authorize(&bash, &workspace, &read_only, &credentials),
        AuthorizationDecision::Denied(DenyReason::ProcessesDisabled)
    ));

    // Path 2: managed processes (even with an installed covering profile).
    let codex = OperationDescriptor::process_launch(
        "codex",
        vec!["app-server".into()],
        Some("D:/work".into()),
    );
    assert!(matches!(
        service.authorize(&codex, &workspace, &read_only, &credentials),
        AuthorizationDecision::Denied(DenyReason::ProcessesDisabled)
    ));

    // Raw profiles are additionally rejected in any restricted mode, even
    // when processes are nominally allowed.
    let mut raw_service = AuthorizationService::new();
    install_profile_capability(
        &mut raw_service,
        &ProfileSource {
            harness_id: "raw.harness".into(),
            package_digest: "sha".into(),
            profile_name: "raw".into(),
        },
        vec!["cli".into()],
        None,
        true,
        vec![],
    );
    let restricted_but_processful = EffectivePermissions {
        ceiling: r_code_harness_protocol::services::PermissionCeiling::ApprovalRequired,
        allow_processes: true,
        allow_network: false,
    };
    assert!(matches!(
        raw_service.authorize(
            &OperationDescriptor::process_launch("cli", vec![], None),
            &workspace,
            &restricted_but_processful,
            &credentials
        ),
        AuthorizationDecision::Denied(DenyReason::RawProfileRestricted)
    ));

    // Path 3: verification preparation.
    let cargo = OperationDescriptor::verification_preparation(
        "cargo",
        vec!["test".into()],
        Some("D:/work".into()),
    );
    assert!(matches!(
        service.authorize(&cargo, &workspace, &read_only, &credentials),
        AuthorizationDecision::Denied(DenyReason::ProcessesDisabled)
    ));
}

#[test]
fn approval_denial_results_in_zero_spawns() {
    let service = codex_profile_service();
    let permissions = EffectivePermissions::approval_required();
    let workspace = WorkspaceCapability::WriteWithin {
        root: "D:/work".into(),
    };
    let descriptor = OperationDescriptor::process_launch(
        "codex",
        vec!["app-server".into()],
        Some("D:/work".into()),
    );

    let decision = service.authorize(
        &descriptor,
        &workspace,
        &permissions,
        &CredentialScope::default(),
    );
    let resolved = apply_approval_decision(decision, false);
    assert!(
        matches!(resolved, AuthorizationDecision::Denied(_)),
        "a denied approval must leave the operation unauthorized"
    );

    // cwd ceilings are enforced for write workspaces too (denied before any
    // spawn, whether as a capability or workspace violation).
    let outside = OperationDescriptor::process_launch("codex", vec![], Some("E:/other".into()));
    assert!(matches!(
        service.authorize(
            &outside,
            &workspace,
            &permissions,
            &CredentialScope::default()
        ),
        AuthorizationDecision::Denied(_)
    ));
}

#[test]
fn generic_questions_can_never_grant_permissions() {
    // The only permission-flipping API is `apply_approval_decision`, which
    // takes a host-issued decision over a pending operation. There is no
    // input from question answering that reaches authorization state.
    let service = codex_profile_service();
    let read_only = EffectivePermissions::read_only();
    let workspace = WorkspaceCapability::ReadOnly {
        root: "D:/work".into(),
    };
    let descriptor = OperationDescriptor::tool_call("bash", vec!["ls".into()], None);

    // Even "answering" a question with the most permissive string changes
    // nothing: authorize() depends only on descriptor + capability state.
    let answer_texts = ["yes", "grant full access", "sudo"];
    for text in answer_texts {
        let _ = text; // a question answer is not an input to authorize()
        assert!(matches!(
            service.authorize(
                &descriptor,
                &workspace,
                &read_only,
                &CredentialScope::default()
            ),
            AuthorizationDecision::Denied(_)
        ));
    }
}

#[test]
fn full_permission_codex_launch_flows_through_the_profile() {
    let service = codex_profile_service();
    let workspace = WorkspaceCapability::WriteWithin {
        root: "D:/work".into(),
    };
    let descriptor = OperationDescriptor::process_launch(
        "codex",
        vec!["app-server".into(), "--config".into()],
        Some("D:/work/project".into()),
    );
    assert_eq!(
        service.authorize(
            &descriptor,
            &workspace,
            &EffectivePermissions::full(),
            &CredentialScope::default()
        ),
        AuthorizationDecision::Allowed
    );

    // Environment resolution merges profile literals with granted broker
    // references; secret material never appears.
    let mut literals = std::collections::BTreeMap::new();
    literals.insert("CODEX_HOME".to_string(), "D:/work/.codex".to_string());
    let env = resolve_profile_env(
        literals,
        &[SecretReference {
            broker_id: "codex-auth".into(),
            scope: "codex".into(),
        }],
    );
    assert_eq!(
        env,
        vec!["CODEX_HOME".to_string(), "codex-auth".to_string()]
    );
    let capability = service
        .capability("codex.harness/codex-app-server")
        .expect("installed");
    assert!(!capability.raw);
}
