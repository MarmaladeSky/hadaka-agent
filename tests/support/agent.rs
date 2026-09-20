//! Test the real loop with a local endpoint that production builds cannot use.
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{agent::Agent, cli::OutputFormat, config::Config, model::DeepSeek, tools::Tools};

struct Mock {
    endpoint: String,
    requests: mpsc::Receiver<Value>,
    thread: thread::JoinHandle<()>,
}

impl Mock {
    fn start(replies: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = endpoint(&listener);
        let (tx, requests) = mpsc::channel();
        let thread = thread::spawn(move || {
            for reply in replies {
                let mut stream = accept(&listener);
                tx.send(read_request(&mut stream)).unwrap();
                for fragment in reply.as_bytes().chunks(7) {
                    if stream.write_all(fragment).is_err() {
                        break;
                    }
                }
            }
        });
        Self {
            endpoint,
            requests,
            thread,
        }
    }

    fn agent(&self, extra: &str) -> Agent {
        agent(&self.endpoint, extra)
    }

    fn finish(self) -> Vec<Value> {
        self.thread.join().unwrap();
        self.requests.try_iter().collect()
    }
}

fn endpoint(listener: &TcpListener) -> String {
    format!("http://{}/chat/completions", listener.local_addr().unwrap())
}

fn agent(endpoint: &str, extra: &str) -> Agent {
    let config = config(extra);
    let model = DeepSeek::for_test(config.enabled_provider().unwrap(), endpoint.into()).unwrap();
    Agent::new(model, config.system_prompt, config.max_turns)
}

fn config(extra: &str) -> Config {
    let mut source: toml::Table = toml::from_str(include_str!("../../agent.example.toml")).unwrap();
    let provider = source.get_mut("providers").unwrap().as_array_mut().unwrap()[0]
        .as_table_mut()
        .unwrap();
    provider.insert("api_key".into(), toml::Value::String("test-key".into()));
    provider.insert("enabled".into(), toml::Value::Boolean(true));
    source.extend(toml::from_str::<toml::Table>(extra).unwrap());
    toml::from_str(&source.to_string()).unwrap()
}

fn accept(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(15)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(15)))
                    .unwrap();
                return stream;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for DeepSeek request"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept: {error}"),
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Value {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, "POST /chat/completions HTTP/1.1\r\n");
    let mut length = None;
    let mut authenticated = false;
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
        if lower == "authorization: bearer test-key\r\n" {
            authenticated = true;
        }
    }
    assert!(authenticated, "expected DeepSeek API key header");
    let mut body = vec![0; length.unwrap()];
    reader.read_exact(&mut body).unwrap();
    let request: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(request["stream"], true);
    assert_eq!(request["model"], "deepseek-flash");
    assert_eq!(request["thinking"]["type"], "disabled");
    request
}

fn event(delta: Value, reason: Option<&str>) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices": [{"index": 0, "delta": delta, "finish_reason": reason}]})
    )
}

fn http(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn answer(text: &str) -> String {
    http(&format!(
        ": keep-alive\n\n{}{}data: [DONE]\n\n",
        event(json!({"content": text}), None),
        event(json!({}), Some("stop"))
    ))
}

fn calls(calls: Vec<(&str, &str, &str)>) -> String {
    let deltas: Vec<Value> = calls.into_iter().enumerate().map(|(index, (id, name, arguments))| {
        json!({"index": index, "id": id, "type": "function", "function": {"name": name, "arguments": arguments}})
    }).collect();
    http(&format!(
        "{}data: [DONE]\n\n",
        event(json!({"tool_calls": deltas}), Some("tool_calls"))
    ))
}

#[tokio::test]
async fn single_task_streams_plain_answer() {
    let mock = Mock::start(vec![answer("Hello, 世界!")]);
    let mut output = Vec::new();
    mock.agent("")
        .run("hello", &Tools::new(), &mut output)
        .await
        .unwrap();
    assert_eq!(output, "Hello, 世界!\n".as_bytes());
    let requests = mock.finish();
    assert_eq!(requests[0]["messages"][0]["role"], "system");
    assert_eq!(requests[0]["messages"][1]["content"], "hello");
    assert_eq!(requests[0]["tools"][0]["function"]["name"], "echo");
}

#[tokio::test]
async fn text_is_flushed_before_the_response_finishes() {
    struct Output {
        text: Vec<u8>,
        flushed: mpsc::Sender<()>,
    }
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.text.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            if !self.text.is_empty() {
                let _ = self.flushed.send(());
            }
            Ok(())
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = endpoint(&listener);
    let first = event(json!({"content": "first"}), None);
    let last = format!("{}data: [DONE]\n\n", event(json!({}), Some("stop")));
    let (flushed, received) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut stream = accept(&listener);
        read_request(&mut stream);
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{first}", first.len() + last.len()).unwrap();
        stream.flush().unwrap();
        received
            .recv_timeout(Duration::from_secs(5))
            .expect("text was buffered until completion");
        stream.write_all(last.as_bytes()).unwrap();
    });
    let mut output = Output {
        text: Vec::new(),
        flushed,
    };
    agent(&endpoint, "")
        .run("hello", &Tools::new(), &mut output)
        .await
        .unwrap();
    server.join().unwrap();
    assert_eq!(output.text, b"first\n");
}

#[tokio::test]
async fn agent_reads_a_file_range() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.txt");
    std::fs::write(&path, "first\nsecond\nthird\n").unwrap();
    let arguments = json!({"path": path, "offset": 1, "limit": 1}).to_string();
    let mock = Mock::start(vec![
        calls(vec![("read", "read_file", &arguments)]),
        answer("Read second line"),
    ]);
    let mut output = Vec::new();
    mock.agent("")
        .run("Read the second line", &Tools::new(), &mut output)
        .await
        .unwrap();
    let requests = mock.finish();
    let result: Value =
        serde_json::from_str(requests[1]["messages"][3]["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["content"], "2: second\n");
    assert_eq!(result["next_offset"], 2);
    assert_eq!(output, b"Read second line\n");
}

#[tokio::test]
async fn verbose_diagnostics_include_input_arguments_and_results() {
    let mock = Mock::start(vec![
        calls(vec![
            ("ok", "echo", r#"{"text":"hello 世界"}"#),
            ("bad", "echo", "{"),
        ]),
        answer("Finished"),
    ]);
    let mut output = Vec::new();
    let mut diagnostics = Vec::new();
    mock.agent("")
        .run_with_diagnostics(
            "Run both tools",
            &Tools::new(),
            &mut output,
            &mut diagnostics,
        )
        .await
        .unwrap();
    let logs = String::from_utf8(diagnostics).unwrap();
    let mut remaining = logs.as_str();
    for expected in [
        "input:\nRun both tools",
        "system prompt:",
        "output (turn 1/20):",
        "tool call: echo (id: ok)",
        "params:\n{\n  \"text\": \"hello 世界\"\n}",
        "tool result: echo (id: ok)\nhello 世界",
        "tool call: echo (id: bad)\nparams:\n{",
        "tool result: echo (id: bad)\nError: tool arguments must be valid JSON",
        "output (turn 2/20):",
    ] {
        let index = remaining
            .find(expected)
            .unwrap_or_else(|| panic!("missing {expected:?} in {remaining:?}"));
        remaining = &remaining[index + expected.len()..];
    }
    assert_eq!(output, b"Finished\n");
    mock.finish();
}

#[test]
fn human_tool_result_formats_read_file_content_without_json() {
    let result = super::agent::human_tool_result(
        r#"{"content":"1: hello\n","total_lines":3,"has_more":true,"next_offset":1}"#,
    );
    assert_eq!(result, "1: hello\n(3 total lines, more available.)");
    assert!(!result.contains("{\"content\""));
}

#[tokio::test]
async fn json_format_emits_one_event_object_per_line() {
    let mock = Mock::start(vec![answer("JSON done")]);
    let mut output = Vec::new();
    mock.agent("")
        .run_with_options(
            "JSON task",
            &Tools::new(),
            &mut output,
            OutputFormat::Json,
            true,
        )
        .await
        .unwrap();
    let events: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["type"], "input");
    assert_eq!(events[0]["text"], "JSON task");
    assert_eq!(events[1]["type"], "system_prompt");
    assert_eq!(events[2]["type"], "tools");
    assert_eq!(events[2]["tools"][0]["function"]["name"], "echo");
    assert_eq!(events[3]["type"], "turn");
    assert_eq!(events[4]["type"], "output");
    assert_eq!(events[4]["text"], "JSON done");
    mock.finish();
}

#[tokio::test]
async fn quiet_mode_emits_only_the_final_result() {
    let mock = Mock::start(vec![answer("Final answer")]);
    let mut output = Vec::new();
    mock.agent("")
        .run_with_options(
            "Quiet task",
            &Tools::new(),
            &mut output,
            OutputFormat::Human,
            false,
        )
        .await
        .unwrap();
    assert_eq!(output, b"Final answer\n");
    mock.finish();
}

#[tokio::test]
async fn quiet_json_mode_emits_only_a_result_event() {
    let mock = Mock::start(vec![answer("Final JSON answer")]);
    let mut output = Vec::new();
    mock.agent("")
        .run_with_options(
            "Quiet JSON task",
            &Tools::new(),
            &mut output,
            OutputFormat::Json,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&output).unwrap(),
        json!({"type":"result", "text":"Final JSON answer"})
    );
    mock.finish();
}

#[tokio::test]
async fn tool_results_preserve_call_ids_and_errors() {
    let mock = Mock::start(vec![
        calls(vec![
            ("a", "echo", r#"{"text":"hello"}"#),
            ("b", "echo", "{"),
            ("c", "unknown", "{}"),
        ]),
        answer("Recovered"),
    ]);
    let mut output = Vec::new();
    mock.agent("")
        .run("use tools", &Tools::new(), &mut output)
        .await
        .unwrap();
    assert_eq!(output, b"Recovered\n");
    let requests = mock.finish();
    let messages = requests[1]["messages"].as_array().unwrap();
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["tool_calls"].as_array().unwrap().len(), 3);
    for (offset, id) in ["a", "b", "c"].iter().enumerate() {
        assert_eq!(messages[3 + offset]["tool_call_id"], *id);
        assert_eq!(messages[3 + offset]["role"], "tool");
    }
    assert_eq!(messages[3]["content"], "hello");
    assert!(
        messages[4]["content"]
            .as_str()
            .unwrap()
            .starts_with("Error:")
    );
    assert!(
        messages[5]["content"]
            .as_str()
            .unwrap()
            .contains("unknown tool")
    );
}

#[tokio::test]
async fn tasks_have_independent_context_and_turn_budgets() {
    let mock = Mock::start(vec![answer("first reply"), answer("second reply")]);
    let mut agent = mock.agent("max_turns = 1");
    let tools = Tools::new();
    let mut output = Vec::new();
    agent.run("first task", &tools, &mut output).await.unwrap();
    agent.run("second task", &tools, &mut output).await.unwrap();
    assert_eq!(output, b"first reply\nsecond reply\n");
    let requests = mock.finish();
    assert_eq!(requests[1]["messages"].as_array().unwrap().len(), 2);
    assert_eq!(requests[1]["messages"][1]["content"], "second task");
}

#[tokio::test]
async fn turn_limit_stops_the_loop() {
    let mock = Mock::start(vec![calls(vec![("a", "echo", r#"{"text":"hello"}"#)])]);
    let error = mock
        .agent("max_turns = 1")
        .run("loop", &Tools::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("turn limit reached (1)"));
    assert_eq!(mock.finish().len(), 1);
}

#[tokio::test]
async fn malformed_incomplete_and_http_errors_fail_the_invocation() {
    for (reply, expected) in [
        (http("data: not-json\n\n"), "invalid JSON"),
        (
            http(&event(json!({"content": "partial"}), None)),
            "missing [DONE]",
        ),
        (http("data: [DONE]\n\n"), "missing finish_reason"),
        (
            http(&event(
                json!({"tool_calls": [{"index": 0, "id": "a", "function": {
                    "name": "echo", "arguments": "{\"text\":\"must not execute\"}"
                }}]}),
                Some("tool_calls"),
            )),
            "missing [DONE]",
        ),
        (
            http(&format!(
                "{}data: [DONE]\n\n",
                event(json!({}), Some("length"))
            )),
            "finish_reason: length",
        ),
        (
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            "401",
        ),
    ] {
        let mock = Mock::start(vec![reply]);
        let error = mock
            .agent("")
            .run("task", &Tools::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        assert_eq!(mock.finish().len(), 1);
    }
}

#[tokio::test]
async fn mcp_discovers_pages_routes_calls_and_reaps_subprocess() {
    let mock = Mock::start(vec![
        calls(vec![("a", "mcp_fixture__second", r#"{"text":"hi"}"#)]),
        answer("MCP done"),
    ]);
    let fixture = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(format!("mcp_fixture{}", std::env::consts::EXE_SUFFIX));
    assert!(
        fixture.exists(),
        "run cargo build --example mcp_fixture first"
    );
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let config = config(&format!(
        "[[mcp_servers]]\nname = 'fixture'\ncommand = {:?}\nargs = []\nenv = {{ FIXTURE_PID_FILE = {:?}, FIXTURE_HANG_ON_EOF = '1', FIXTURE_STDERR = 'fixture diagnostic' }}",
        fixture.to_str().unwrap(),
        pid_file.to_str().unwrap(),
    ));
    let mut tools = Tools::new();
    tools.connect(&config.mcp_servers).await.unwrap();
    let result = mock.agent("").run("use MCP", &tools, &mut Vec::new()).await;
    tools.shutdown().await.unwrap();
    result.unwrap();
    let requests = mock.finish();
    let names: Vec<&str> = requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "echo",
            "read_file",
            "text_editor",
            "mcp_fixture__echo",
            "mcp_fixture__second"
        ]
    );
    let result = requests[1]["messages"][3]["content"].as_str().unwrap();
    assert!(result.contains("second: hi\nsecond content block"));
    assert!(result.contains(r#""tool":"second""#));
    #[cfg(target_os = "linux")]
    assert!(
        !PathBuf::from(format!(
            "/proc/{}",
            std::fs::read_to_string(pid_file).unwrap()
        ))
        .exists()
    );
}
