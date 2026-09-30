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
fn empty_deltas_and_answers_preserve_output_shape() {
    for format in [OutputFormat::Human, OutputFormat::Json] {
        for verbose in [false, true] {
            let (mut output, mut diagnostics) = (Vec::new(), Vec::new());
            let mut renderer = Renderer {
                output: &mut output,
                diagnostics: &mut diagnostics,
                format,
                verbose,
            };
            renderer.emit(AgentEvent::Text("")).unwrap();
            renderer
                .emit(AgentEvent::AssistantComplete(&AssistantTurn::default()))
                .unwrap();
            renderer.emit(AgentEvent::Complete("")).unwrap();
            if matches!(format, OutputFormat::Json) && !verbose {
                assert_eq!(
                    serde_json::from_slice::<Value>(&output).unwrap(),
                    json!({"type":"result", "text":""})
                );
            } else {
                assert!(output.is_empty());
            }
            assert!(diagnostics.is_empty());
        }
    }
}
