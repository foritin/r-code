//! r-code-harness-native binary: serves the Native harness plugin over
//! stdio using the public SDK.

use r_code_harness_native::session::NativeSession;
use r_code_harness_native::LoopConfig;

#[tokio::main]
async fn main() -> Result<(), r_code_harness_sdk::SdkError> {
    let config = LoopConfig::default();
    r_code_harness_sdk::serve(NativeSession::new(config)).await
}
