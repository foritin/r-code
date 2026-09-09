//! T23 — review and scoped rollback.
//!
//! Rollback fixtures preserve later/user edits, handle conflicts without
//! partial writes and recover an interrupted multi-file rejection;
//! unassigned external changes stay in the ordinary review set.

use r_code_runtime::services::review::ReviewService;

#[test]
fn rejection_restores_before_state_and_preserves_later_user_edits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    std::fs::create_dir_all(&root).expect("dirs");
    std::fs::write(root.join("a.txt"), b"original").expect("original");

    let mut review = ReviewService::new(&root);
    review
        .record_change("u1", "a.txt", b"original", b"agent-edit")
        .expect("record");
    std::fs::write(root.join("a.txt"), b"agent-edit").expect("apply change");

    // The user edits AFTER the agent's change: rejection must refuse rather
    // than clobber the user's newer work.
    std::fs::write(root.join("a.txt"), b"agent-edit + user polish").expect("user edit");
    let error = review.begin_rejection("u1").expect_err("conflict");
    assert!(
        matches!(error, r_code_runtime::services::review::ReviewError::Conflict(path) if path == "a.txt")
    );
    // Nothing was written: the user's file is intact.
    assert_eq!(
        std::fs::read(root.join("a.txt")).unwrap(),
        b"agent-edit + user polish"
    );

    // Without later edits the rejection restores the original.
    std::fs::write(root.join("a.txt"), b"agent-edit").expect("reset to after state");
    let rejection = review.begin_rejection("u1").expect("begin");
    review.apply_rejection(&rejection).expect("apply");
    assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"original");
}

#[test]
fn multi_file_rejections_are_atomic_no_partial_writes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    std::fs::create_dir_all(root.join("src")).expect("dirs");
    std::fs::write(root.join("src/one.rs"), b"one-before").expect("one");
    std::fs::write(root.join("src/two.rs"), b"two-before").expect("two");

    let mut review = ReviewService::new(&root);
    review
        .record_change("u1", "src/one.rs", b"one-before", b"one-after")
        .expect("record one");
    review
        .record_change("u1", "src/two.rs", b"two-before", b"two-after")
        .expect("record two");
    std::fs::write(root.join("src/one.rs"), b"one-after").expect("apply one");
    std::fs::write(root.join("src/two.rs"), b"two-after").expect("apply two");

    // two.rs gets a later user edit: the whole rejection is refused and
    // one.rs must NOT be partially restored.
    std::fs::write(root.join("src/two.rs"), b"two-after+user").expect("user edit");
    assert!(review.begin_rejection("u1").is_err());
    assert_eq!(
        std::fs::read(root.join("src/one.rs")).unwrap(),
        b"one-after",
        "no partial write"
    );

    // With no conflicts both files restore atomically.
    std::fs::write(root.join("src/two.rs"), b"two-after").expect("reset");
    let rejection = review.begin_rejection("u1").expect("begin");
    let applied = review.apply_rejection(&rejection).expect("apply both");
    assert_eq!(applied, 2);
    assert_eq!(
        std::fs::read(root.join("src/one.rs")).unwrap(),
        b"one-before"
    );
    assert_eq!(
        std::fs::read(root.join("src/two.rs")).unwrap(),
        b"two-before"
    );
}

#[test]
fn interrupted_multi_file_rejection_recovers_idempotently() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    std::fs::create_dir_all(&root).expect("dirs");
    std::fs::write(root.join("f1.txt"), b"b1").expect("f1");
    std::fs::write(root.join("f2.txt"), b"b2").expect("f2");
    std::fs::write(root.join("f3.txt"), b"b3").expect("f3");

    let mut review = ReviewService::new(&root);
    review
        .record_change("u1", "f1.txt", b"b1", b"a1")
        .expect("1");
    review
        .record_change("u1", "f2.txt", b"b2", b"a2")
        .expect("2");
    review
        .record_change("u1", "f3.txt", b"b3", b"a3")
        .expect("3");
    for (file, after) in [("f1.txt", "a1"), ("f2.txt", "a2"), ("f3.txt", "a3")] {
        std::fs::write(root.join(file), after).expect("apply");
    }

    let rejection = review.begin_rejection("u1").expect("begin");

    // Simulate an interruption: f1 restored manually (crash after the first
    // inverse write), f2/f3 untouched.
    std::fs::write(root.join("f1.txt"), b"b1").expect("crash after first restore");

    // Recovery re-runs the rejection: the applied entry is skipped
    // (idempotent hash guard), the rest complete.
    let applied = review.apply_rejection(&rejection).expect("recover");
    assert!(applied >= 2, "remaining files restored");
    assert_eq!(std::fs::read(root.join("f1.txt")).unwrap(), b"b1");
    assert_eq!(std::fs::read(root.join("f2.txt")).unwrap(), b"b2");
    assert_eq!(std::fs::read(root.join("f3.txt")).unwrap(), b"b3");

    // A second run reports completion rather than re-applying.
    let error = review.apply_rejection(&rejection).expect_err("complete");
    assert!(matches!(
        error,
        r_code_runtime::services::review::ReviewError::AlreadyComplete(_)
    ));
}

#[test]
fn unassigned_external_changes_stay_in_the_review_set() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("project");
    std::fs::create_dir_all(&root).expect("dirs");
    std::fs::write(root.join("owned.txt"), b"before").expect("owned");
    std::fs::write(root.join("external.txt"), b"user-made").expect("external");

    let mut review = ReviewService::new(&root);
    review
        .record_change("u1", "owned.txt", b"before", b"after")
        .expect("record owned");
    review.record_external_changes(&["external.txt".into()]);

    // The external change has no owner: rejecting u1 leaves it alone.
    std::fs::write(root.join("owned.txt"), b"after").expect("apply owned");
    let rejection = review.begin_rejection("u1").expect("begin");
    review.apply_rejection(&rejection).expect("apply");
    assert_eq!(std::fs::read(root.join("owned.txt")).unwrap(), b"before");
    assert_eq!(
        std::fs::read(root.join("external.txt")).unwrap(),
        b"user-made",
        "external change retained for ordinary review"
    );

    // Accepting a unit's changes drops them from the review surface.
    review
        .record_change("u2", "owned.txt", b"before", b"after-2")
        .expect("u2");
    std::fs::write(root.join("owned.txt"), b"after-2").expect("apply u2");
    review.accept("u2");
    assert!(review.changes_for("u2").is_empty());
    assert_eq!(
        std::fs::read(root.join("owned.txt")).unwrap(),
        b"after-2",
        "accept keeps content"
    );
}
