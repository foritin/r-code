//! P03 — host.process.read routing and page validation.

use r_code_harness_protocol::rpc::{error_code, RpcId, RpcRequest};
use r_code_harness_protocol::{
    HostService, ProcessOutputFrame, ProcessOutputStream, ProcessReadReply, ProcessReadRequest,
    RunIdentity,
};
use r_code_kernel::ports::{GenerationToken, ProcessService, RunGuard, ServiceError};
use r_code_kernel::testing::{
    FakeModelService, FakeProcessService, FakeToolService, MemoryJournal,
};
use r_code_runtime::plugins::catalog::HOST_API;
use r_code_runtime::plugins::router::{
    service_for_method, supported_requested_services, RouterServiceAvailability,
};
use r_code_runtime::plugins::{HostRouter, IgnoreQuestions};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct OnePageProcess {
    reply: Mutex<Option<ProcessReadReply>>,
    reads: Mutex<Vec<ProcessReadRequest>>,
}

impl OnePageProcess {
    fn returning(reply: ProcessReadReply) -> Self {
        Self {
            reply: Mutex::new(Some(reply)),
            reads: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl ProcessService for OnePageProcess {
    async fn open(
        &self,
        _token: GenerationToken,
        _profile: &str,
        _arguments: Vec<String>,
        _cwd: Option<String>,
    ) -> Result<String, ServiceError> {
        Err(ServiceError::Unsupported("open".into()))
    }

    async fn read(
        &self,
        _token: GenerationToken,
        request: ProcessReadRequest,
    ) -> Result<ProcessReadReply, ServiceError> {
        self.reads.lock().unwrap().push(request);
        self.reply
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| ServiceError::Failure("missing scripted page".into()))
    }

    async fn write(
        &self,
        _token: GenerationToken,
        _handle: &str,
        _data: Vec<u8>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unsupported("write".into()))
    }

    async fn close(
        &self,
        _token: GenerationToken,
        _handle: &str,
    ) -> Result<Option<i32>, ServiceError> {
        Err(ServiceError::Unsupported("close".into()))
    }
}

fn identity() -> RunIdentity {
    RunIdentity {
        task_id: "task-p03".into(),
        branch_id: "branch-p03".into(),
        run_id: "run-p03".into(),
        attempt_id: "attempt-p03".into(),
        generation: 1,
    }
}

fn request(cursor: u64, max_bytes: u32) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".into(),
        id: RpcId::Number(1),
        method: "host.process.read".into(),
        params: Some(serde_json::json!({
            "handle": "run-p03:raw-handle",
            "cursor": cursor,
            "maxBytes": max_bytes,
            "waitMs": 0
        })),
    }
}

fn make_router(reply: ProcessReadReply) -> (HostRouter, Arc<OnePageProcess>) {
    make_router_with_grants(reply, vec![HostService::ProcessRead])
}

fn make_router_with_grants(
    reply: ProcessReadReply,
    grants: Vec<HostService>,
) -> (HostRouter, Arc<OnePageProcess>) {
    let processes = Arc::new(OnePageProcess::returning(reply));
    let router = HostRouter::new(
        identity(),
        RunGuard::new("run-p03", 1),
        grants,
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        processes.clone(),
        Arc::new(MemoryJournal::new()),
        Arc::new(IgnoreQuestions),
    );
    (router, processes)
}

#[tokio::test]
async fn non_empty_terminal_page_without_exit_fails_closed() {
    for frames in [
        vec![ProcessOutputFrame::Data {
            sequence: 0,
            stream: ProcessOutputStream::Stdout,
            data_base64: "eA==".into(),
        }],
        vec![ProcessOutputFrame::Eof {
            sequence: 0,
            stream: ProcessOutputStream::Stdout,
        }],
        vec![
            ProcessOutputFrame::Data {
                sequence: 0,
                stream: ProcessOutputStream::Stdout,
                data_base64: "eA==".into(),
            },
            ProcessOutputFrame::Eof {
                sequence: 1,
                stream: ProcessOutputStream::Stdout,
            },
        ],
    ] {
        let next_cursor = frames.len() as u64;
        let (router, _) = make_router(ProcessReadReply {
            frames,
            next_cursor,
            terminal: true,
            exit_code: None,
        });
        let error = router
            .handle_request(request(0, 64))
            .await
            .expect_err("a non-empty terminal page must end in Exit");
        assert_eq!(error.code, error_code::INTERNAL);
    }
}

#[tokio::test]
async fn empty_terminal_reread_and_exit_none_are_valid_but_metadata_mismatch_is_not() {
    let (router, _) = make_router(ProcessReadReply {
        frames: Vec::new(),
        next_cursor: 7,
        terminal: true,
        exit_code: Some(9),
    });
    let empty: ProcessReadReply = serde_json::from_value(
        router
            .handle_request(request(7, 64))
            .await
            .expect("empty terminal reread is valid"),
    )
    .unwrap();
    assert!(empty.frames.is_empty());
    assert_eq!(empty.next_cursor, 7);
    assert_eq!(empty.exit_code, Some(9));

    let (router, _) = make_router(ProcessReadReply {
        frames: vec![ProcessOutputFrame::Exit {
            sequence: 3,
            exit_code: None,
        }],
        next_cursor: 4,
        terminal: true,
        exit_code: None,
    });
    router
        .handle_request(request(3, 64))
        .await
        .expect("Exit(None) agrees with terminal metadata None");

    let (router, _) = make_router(ProcessReadReply {
        frames: vec![ProcessOutputFrame::Exit {
            sequence: 3,
            exit_code: None,
        }],
        next_cursor: 4,
        terminal: true,
        exit_code: Some(0),
    });
    let mismatch = router
        .handle_request(request(3, 64))
        .await
        .expect_err("Exit(None) must not disagree with terminal exitCode");
    assert_eq!(mismatch.code, error_code::INTERNAL);
}

#[tokio::test]
async fn same_cursor_is_a_lost_response_safe_reread_of_the_same_page() {
    let reply = ProcessReadReply {
        frames: vec![ProcessOutputFrame::Data {
            sequence: 5,
            stream: ProcessOutputStream::Stdout,
            data_base64: "eHl6".into(),
        }],
        next_cursor: 6,
        terminal: false,
        exit_code: None,
    };
    let (router, processes) = make_router(reply.clone());
    let first = router.handle_request(request(5, 64)).await.unwrap();
    let replay = router.handle_request(request(5, 64)).await.unwrap();
    assert_eq!(first, serde_json::to_value(&reply).unwrap());
    assert_eq!(replay, first);
    let reads = processes.reads.lock().unwrap();
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[0], reads[1]);
    assert_eq!(reads[0].handle, "raw-handle");
    assert_eq!(reads[0].cursor, 5);
}

fn empty_page(cursor: u64) -> ProcessReadReply {
    ProcessReadReply {
        frames: Vec::new(),
        next_cursor: cursor,
        terminal: false,
        exit_code: None,
    }
}

#[tokio::test]
async fn request_scope_grants_and_bounds_fail_before_the_process_service() {
    assert_eq!(
        service_for_method("host.process.read"),
        Some(HostService::ProcessRead)
    );

    let (ungranted, service) = make_router_with_grants(empty_page(0), Vec::new());
    let denied = ungranted
        .handle_request(request(0, 1))
        .await
        .expect_err("ProcessRead grant is mandatory");
    assert_eq!(denied.code, error_code::PROTOCOL_VIOLATION);
    assert!(service.reads.lock().unwrap().is_empty());

    let (router, service) = make_router(empty_page(0));
    let mut cross_run = request(0, 1);
    cross_run.params.as_mut().unwrap()["handle"] = serde_json::json!("run-other:raw-handle");
    let denied = router
        .handle_request(cross_run)
        .await
        .expect_err("cross-run handle must fail before read");
    assert_eq!(denied.code, error_code::RUN_MISMATCH);
    assert!(service.reads.lock().unwrap().is_empty());

    for params in [
        serde_json::json!({"handle":"run-p03:h","cursor":0,"maxBytes":0}),
        serde_json::json!({"handle":"run-p03:h","cursor":0,"maxBytes":262145}),
        serde_json::json!({"handle":"run-p03:h","cursor":0,"maxBytes":1,"waitMs":30001}),
        serde_json::json!({"handle":"run-p03:h","cursor":0,"maxBytes":1,"unknown":true}),
        serde_json::json!({"handle":"h","cursor":0,"maxBytes":1}),
    ] {
        let (router, service) = make_router(empty_page(0));
        let error = router
            .handle_request(RpcRequest {
                jsonrpc: "2.0".into(),
                id: RpcId::Number(2),
                method: "host.process.read".into(),
                params: Some(params),
            })
            .await
            .expect_err("invalid request must fail");
        assert!(matches!(
            error.code,
            error_code::INVALID_PARAMS | error_code::RUN_MISMATCH
        ));
        assert!(service.reads.lock().unwrap().is_empty());
    }
}

async fn assert_page_rejected(reply: ProcessReadReply, cursor: u64, max_bytes: u32) {
    let (router, _) = make_router(reply);
    let error = router
        .handle_request(request(cursor, max_bytes))
        .await
        .expect_err("corrupt process page must fail closed");
    assert_eq!(error.code, error_code::INTERNAL);
}

#[tokio::test]
async fn gap_stale_base64_sequence_eof_size_and_terminal_corruption_fail_closed() {
    let data = |sequence, stream, data: &str| ProcessOutputFrame::Data {
        sequence,
        stream,
        data_base64: data.to_string(),
    };
    let eof = |sequence, stream| ProcessOutputFrame::Eof { sequence, stream };

    let cases = vec![
        ProcessReadReply {
            frames: vec![data(1, ProcessOutputStream::Stdout, "eA==")],
            next_cursor: 2,
            terminal: false,
            exit_code: None,
        },
        ProcessReadReply {
            frames: vec![data(0, ProcessOutputStream::Stdout, "eA==")],
            next_cursor: 0,
            terminal: false,
            exit_code: None,
        },
        ProcessReadReply {
            frames: vec![data(0, ProcessOutputStream::Stdout, "***")],
            next_cursor: 1,
            terminal: false,
            exit_code: None,
        },
        ProcessReadReply {
            frames: vec![data(0, ProcessOutputStream::Stdout, "")],
            next_cursor: 1,
            terminal: false,
            exit_code: None,
        },
        ProcessReadReply {
            frames: vec![
                eof(0, ProcessOutputStream::Stdout),
                data(1, ProcessOutputStream::Stdout, "eA=="),
            ],
            next_cursor: 2,
            terminal: false,
            exit_code: None,
        },
        ProcessReadReply {
            frames: vec![
                eof(0, ProcessOutputStream::Stderr),
                eof(1, ProcessOutputStream::Stderr),
            ],
            next_cursor: 2,
            terminal: false,
            exit_code: None,
        },
        ProcessReadReply {
            frames: vec![ProcessOutputFrame::Exit {
                sequence: 0,
                exit_code: Some(0),
            }],
            next_cursor: 1,
            terminal: false,
            exit_code: Some(0),
        },
        ProcessReadReply {
            frames: vec![ProcessOutputFrame::Exit {
                sequence: 0,
                exit_code: Some(0),
            }],
            next_cursor: 1,
            terminal: true,
            exit_code: Some(1),
        },
        ProcessReadReply {
            frames: Vec::new(),
            next_cursor: 0,
            terminal: false,
            exit_code: Some(0),
        },
    ];
    for reply in cases {
        assert_page_rejected(reply, 0, 64).await;
    }
    assert_page_rejected(
        ProcessReadReply {
            frames: vec![data(0, ProcessOutputStream::Stdout, "eHk=")],
            next_cursor: 1,
            terminal: false,
            exit_code: None,
        },
        0,
        1,
    )
    .await;
    assert_page_rejected(
        ProcessReadReply {
            frames: vec![data(u64::MAX, ProcessOutputStream::Stdout, "eA==")],
            next_cursor: u64::MAX,
            terminal: false,
            exit_code: None,
        },
        u64::MAX,
        64,
    )
    .await;
}

#[tokio::test]
async fn planning_safe_grants_deny_process_read_and_backend_remains_explicitly_unsupported() {
    assert_eq!(HOST_API.major, 1);
    // P23.4: the advertised minor is the floor that governs the newest
    // guarantee set — the final Wave 3 single-process minor. The 1.2
    // effect-fields floor beneath it stays proven by negotiation in s23.
    assert_eq!(
        HOST_API.minor,
        r_code_runtime::plugins::catalog::SINGLE_PROCESS_MIN_API_MINOR
    );
    assert_eq!(
        HOST_API.minor,
        r_code_harness_protocol::manifest::SINGLE_PROCESS_MIN_API_MINOR
    );
    let requested = [
        HostService::ToolsList,
        HostService::ProcessOpen,
        HostService::ProcessRead,
        HostService::ProcessWrite,
        HostService::ProcessClose,
    ];
    let grants = supported_requested_services(
        &requested,
        RouterServiceAvailability {
            model_stream: true,
            tools: true,
            context: true,
            artifacts: true,
            plan_publish: true,
            questions: true,
            approvals: true,
            checkpoints: true,
            completion: true,
            sandbox_activated: false,
        },
    );
    assert_eq!(grants, [HostService::ToolsList]);
    assert!(!grants.contains(&HostService::ProcessRead));

    let default_backend = FakeProcessService::default();
    let unsupported = default_backend
        .read(
            RunGuard::new("run-p03", 1).token(),
            ProcessReadRequest {
                handle: "raw-handle".into(),
                cursor: 0,
                max_bytes: 1,
                wait_ms: None,
            },
        )
        .await;
    assert_eq!(
        unsupported,
        Err(ServiceError::Unsupported("host.process.read".into()))
    );

    let run_manager = include_str!("../src/run_manager.rs");
    assert!(
        run_manager.matches("HostService::ProcessRead").count() >= 2,
        "planning and safe-mode deny/assert lists must both mention ProcessRead"
    );
}
