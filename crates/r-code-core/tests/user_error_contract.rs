//! M1-01.A1：Rust 侧与前端共享同一错误契约 fixture。
//!
//! 断言语义与 `src-tauri/frontend/scripts/user-error-contract.test.mjs` 保持一致：
//! 同一 payload 反序列化后必须得到相同 code/args；含 debug_detail 的 case 序列化
//! 回 JSON 后字段不丢；unknown code 解析安全，不需要注册表。

use r_code_core::UserFacingError;
use serde::Deserialize;
use serde_json::Value;

const FIXTURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/product-experience/fixtures/user-error-cases.json"
);

#[derive(Deserialize)]
struct FixtureCase {
    name: String,
    payload: Value,
}

#[derive(Deserialize)]
struct Fixture {
    #[allow(dead_code)]
    schema_version: String,
    cases: Vec<FixtureCase>,
}

fn load_fixture() -> Vec<FixtureCase> {
    let raw = std::fs::read_to_string(FIXTURE_PATH)
        .unwrap_or_else(|error| panic!("read shared user error fixture: {error}"));
    let fixture: Fixture =
        serde_json::from_str(&raw).expect("parse shared user error fixture");
    fixture.cases
}

#[test]
fn shared_fixture_decodes_to_identical_code_and_args() {
    for case in load_fixture() {
        let error: UserFacingError = serde_json::from_value(case.payload.clone())
            .unwrap_or_else(|error| panic!("case {} should decode: {error}", case.name));

        assert_eq!(
            error.code,
            case.payload["code"].as_str().expect("fixture code is string"),
            "case {} code mismatch",
            case.name
        );
        let expected_args = case
            .payload
            .get("args")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        let actual_args = serde_json::to_value(&error.args).expect("serialize args");
        assert_eq!(actual_args, expected_args, "case {} args mismatch", case.name);
    }
}

#[test]
fn debug_detail_survives_round_trip_but_stays_optional() {
    for case in load_fixture() {
        let error: UserFacingError = serde_json::from_value(case.payload.clone())
            .unwrap_or_else(|error| panic!("case {} should decode: {error}", case.name));
        let reserialized = serde_json::to_value(&error).expect("re-serialize");

        match case.payload.get("debug_detail") {
            Some(expected) => assert_eq!(
                reserialized.get("debug_detail"),
                Some(expected),
                "case {} loses debug detail",
                case.name
            ),
            None => assert!(
                reserialized.get("debug_detail").is_none(),
                "case {} invents debug detail",
                case.name
            ),
        }
        // code/args 部分与原始 payload 完全一致
        assert_eq!(reserialized["code"], case.payload["code"], "case {}", case.name);
        let empty = Value::Object(Default::default());
        assert_eq!(
            reserialized.get("args").cloned().unwrap_or(empty.clone()),
            case.payload.get("args").cloned().unwrap_or(empty),
            "case {} args round trip",
            case.name
        );
    }
}

#[test]
fn unknown_code_never_requires_registry_and_displays_as_code() {
    let error: UserFacingError = serde_json::from_value(
        load_fixture()
            .into_iter()
            .find(|case| case.name == "unknown_code")
            .expect("fixture keeps an unknown-code case")
            .payload,
    )
    .expect("unknown code decodes safely");
    // 未注册 code 的 Display 就是 code 本身——前端据此走 errors.unknown 降级。
    assert_eq!(error.to_string(), "future.module_unknown_error_xz");
}
