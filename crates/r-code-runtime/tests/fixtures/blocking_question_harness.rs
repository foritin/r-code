use std::io::{BufRead, Write};

fn send(value: &str) {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    writeln!(output, "{value}").expect("write frame");
    output.flush().expect("flush frame");
}

fn request_id(line: &str) -> &str {
    let start = line.find("\"id\":").expect("request id") + 5;
    let tail = &line[start..];
    let end = tail
        .find(|character| character == ',' || character == '}')
        .unwrap_or(tail.len());
    &tail[..end]
}

fn main() {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    while let Some(Ok(line)) = lines.next() {
        let id = request_id(&line).to_string();
        if line.contains("\"method\":\"initialize\"") {
            send(&format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"harnessId\":\"fixture.blocking-question\",\"harnessVersion\":\"1.0.0\"}}}}"
            ));
        } else if line.contains("\"method\":\"harness.start\"") {
            send(
                "{\"jsonrpc\":\"2.0\",\"id\":\"question-1\",\"method\":\"host.questions.ask\",\"params\":{\"text\":\"choose one\",\"options\":[\"a\",\"b\"],\"blocking\":true}}",
            );
            let reply = lines
                .next()
                .expect("question reply line")
                .expect("question reply");
            assert!(reply.contains("\"result\""), "question failed: {reply}");
            send(&format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"started\":true}}}}"
            ));
            send(
                "{\"jsonrpc\":\"2.0\",\"method\":\"harness.event\",\"params\":{\"kind\":\"progress\",\"payload\":{\"afterQuestion\":true}}}",
            );
        } else if line.contains("\"method\":\"harness.cancel\"") {
            send(&format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"acknowledged\":true}}}}"
            ));
            return;
        } else if line.contains("\"method\":\"shutdown\"") {
            send(&format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":null}}"
            ));
            return;
        }
    }
}
