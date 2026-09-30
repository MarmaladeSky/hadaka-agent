// Copyright 2026 David Akermann
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use super::*;

#[test]
fn assembles_interleaved_calls_and_streams_text() {
    let mut turn = StreamedTurn::default();
    let mut output = Vec::new();
    turn.push(
        json!({"choices": [{"index": 0, "delta": {"content": "Checking…", "tool_calls": [
            {"index": 1, "id": "b", "function": {"name": "echo", "arguments": "{\"text\":"}},
            {"index": 0, "id": "a", "function": {"name": "ec", "arguments": "{"}}
        ]}}]}),
        &mut |text| {
            output.extend_from_slice(text.as_bytes());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(String::from_utf8(output.clone()).unwrap(), "Checking…");
    turn.push(
        json!({"choices": [{"index": 0, "delta": {"tool_calls": [
        {"index": 0, "function": {"name": "ho", "arguments": "\"text\":\"one\"}"}},
        {"index": 1, "function": {"arguments": "\"two\"}"}}
    ]}, "finish_reason": "tool_calls"}]}),
        &mut |text| {
            output.extend_from_slice(text.as_bytes());
            Ok(())
        },
    )
    .unwrap();
    let result = turn.finish().unwrap();
    assert_eq!(result.tool_calls[0].id, "a");
    assert_eq!(result.tool_calls[0].name, "echo");
    assert_eq!(result.tool_calls[0].arguments, r#"{"text":"one"}"#);
    assert_eq!(result.tool_calls[1].arguments, r#"{"text":"two"}"#);
}

#[test]
fn rejects_unfinished_or_truncated_responses() {
    assert!(StreamedTurn::default().finish().is_err());
    let turn = StreamedTurn {
        finish_reason: Some("length".into()),
        ..Default::default()
    };
    assert!(turn.finish().unwrap_err().to_string().contains("length"));
}

#[test]
fn accepts_null_optional_tool_fields() {
    let mut turn = StreamedTurn::default();
    let mut output = Vec::new();
    turn.push(
        json!({"choices": [{"index": 0, "delta": {
        "content": "hello", "tool_calls": null
    }, "finish_reason": "stop"}]}),
        &mut |text| {
            output.extend_from_slice(text.as_bytes());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(turn.finish().unwrap().content, "hello");

    let mut turn = StreamedTurn::default();
    turn.push(
        json!({"choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "id": "call", "function": null}
        ]}}]}),
        &mut |text| {
            output.extend_from_slice(text.as_bytes());
            Ok(())
        },
    )
    .unwrap();
    turn.push(
        json!({"choices": [{"index": 0, "delta": {"tool_calls": [
        {"index": 0, "function": {"name": "echo", "arguments": "{}"}}
    ]}, "finish_reason": "tool_calls"}]}),
        &mut |text| {
            output.extend_from_slice(text.as_bytes());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(turn.finish().unwrap().tool_calls[0].id, "call");
}
