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

fn convert_html(html: &str, max_chars: usize) -> String {
    let url = Url::parse("https://example.com/releases/").unwrap();
    let args: Arguments = serde_json::from_value(json!({
        "url": url.as_str(), "max_chars": max_chars
    }))
    .unwrap();
    convert(&args, &url, 200, "text/html", html.as_bytes()).unwrap()
}

#[test]
fn web_metadata_limits_preserve_normal_content() {
    let result: Value = serde_json::from_str(&convert_html(
        "<title>Release notes</title><h1>Version 2</h1><a href='../v2'>Details</a>",
        30_000,
    ))
    .unwrap();
    assert_eq!(result["title"], "Release notes");
    assert_eq!(result["links"], json!(["https://example.com/v2"]));
    assert!(result["content"].as_str().unwrap().contains("Version 2"));
    assert_eq!(result["truncated"], false);
}

#[test]
fn web_metadata_limits_bound_unicode_titles() {
    let title = "界".repeat(MAX_TITLE_CHARS * 4);
    let result: Value = serde_json::from_str(&convert_html(
        &format!("<title>{title}</title><p>Release notes</p>"),
        30_000,
    ))
    .unwrap();
    let returned = result["title"].as_str().expect("retain a readable title");
    assert!(!returned.is_empty());
    assert!(
        returned.chars().count() <= MAX_TITLE_CHARS,
        "title contains {} characters",
        returned.chars().count()
    );
    assert_eq!(
        result["truncated"], true,
        "metadata truncation must be visible"
    );
}

#[test]
fn web_metadata_limits_omit_oversized_links_without_shortening_urls() {
    let long_url = format!("https://example.com/{}", "a".repeat(MAX_LINK_CHARS));
    let result: Value = serde_json::from_str(&convert_html(
        &format!("<a href='{long_url}'>Long link</a><a href='/v2'>Release</a>"),
        30_000,
    ))
    .unwrap();
    assert_eq!(
        result["links"],
        json!(["https://example.com/v2"]),
        "omit oversized URLs while preserving valid links unchanged"
    );
    assert_eq!(
        result["truncated"], true,
        "omitted metadata must be visible"
    );
}

#[test]
fn web_metadata_limits_bound_complete_serialized_result() {
    // Each URL is individually within its limit; their combined size plus
    // multibyte content must still fit the complete result's byte budget.
    let mut html = format!("<p>{}</p>", "🦀".repeat(MAX_CONTENT_CHARS));
    for index in 0..MAX_LINKS {
        let prefix = format!("https://example.com/{index}/");
        let url = format!("{prefix}{}", "a".repeat(MAX_LINK_CHARS - prefix.len()));
        html.push_str(&format!("<a href='{url}'>Release {index}</a>"));
    }
    assert!(html.len() < MAX_BYTES);
    let serialized = convert_html(&html, MAX_CONTENT_CHARS);
    assert!(
        serialized.len() <= MAX_RESULT_BYTES,
        "serialized result is {} bytes, maximum is {MAX_RESULT_BYTES}",
        serialized.len()
    );
    let result: Value = serde_json::from_str(&serialized).unwrap();
    assert!(!result["content"].as_str().unwrap().is_empty());
    assert_eq!(result["truncated"], true);
}

#[test]
fn result_byte_limit_accounts_for_json_escaping() {
    let url = Url::parse("https://example.com/").unwrap();
    let args = Arguments {
        url: url.to_string(),
        representation: Representation::Raw,
        max_chars: MAX_CONTENT_CHARS,
    };
    let body = "\u{0001}".repeat(MAX_CONTENT_CHARS);
    let serialized = convert(&args, &url, 200, "text/plain", body.as_bytes()).unwrap();
    assert!(serialized.len() <= MAX_RESULT_BYTES);
    let result: Value = serde_json::from_str(&serialized).unwrap();
    let content = result["content"].as_str().unwrap();
    assert!(!content.is_empty());
    assert!(body.starts_with(content));
    assert_eq!(result["truncated"], true);
}

#[test]
fn result_byte_limit_rejects_oversized_fixed_metadata() {
    let url = Url::parse(&format!(
        "https://example.com/{}",
        "a".repeat(MAX_RESULT_BYTES)
    ))
    .unwrap();
    for representation in [Representation::Raw, Representation::Json] {
        let args = Arguments {
            url: url.to_string(),
            representation,
            max_chars: MAX_CONTENT_CHARS,
        };
        assert!(convert(&args, &url, 200, "application/json", b"{}").is_err());
    }
}

#[test]
fn web_metadata_limits_preserve_boundary_link_and_report_count_limit() {
    let prefix = "https://example.com/";
    let boundary_url = format!("{prefix}{}", "a".repeat(MAX_LINK_CHARS - prefix.len()));
    let mut html = format!("<a href='{boundary_url}'>First</a>");
    for index in 0..MAX_LINKS {
        html.push_str(&format!("<a href='/release/{index}'>Release</a>"));
    }
    let result: Value = serde_json::from_str(&convert_html(&html, DEFAULT_CONTENT_CHARS)).unwrap();
    assert_eq!(result["links"][0], boundary_url);
    assert_eq!(result["links"].as_array().unwrap().len(), MAX_LINKS);
    assert_eq!(result["truncated"], true);
}

#[test]
fn rejects_nonpublic_addresses() {
    for ip in [
        "127.0.0.1",
        "10.0.0.1",
        "169.254.169.254",
        "100.64.0.1",
        "0.0.0.0",
        "224.0.0.1",
        "198.18.0.1",
        "::1",
        "::ffff:127.0.0.1",
        "fc00::1",
        "fe80::1",
        "2001:db8::1",
        "2002:7f00::1",
    ] {
        assert!(!public_ip(ip.parse().unwrap()), "{ip}");
    }
    assert!(public_ip("8.8.8.8".parse().unwrap()));
    assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
}

#[tokio::test]
async fn permission_and_redirect_targets_are_checked() {
    let tool = FetchUrl::new(
        PermissionPolicy::new(vec![], vec![], vec![])
            .with_network_hosts(vec!["example.com".into()]),
    );
    let base = Url::parse("https://example.com/releases").unwrap();
    assert!(tool.check_url(&base).is_ok());
    for target in [
        "https://sub.example.com",
        "https://other.com",
        "http://example.com",
        "https://user:pass@example.com",
        "https://example.com:444",
    ] {
        assert!(tool.check_url(&base.join(target).unwrap()).is_err());
    }
    let denied = FetchUrl::new(PermissionPolicy::new(vec![], vec![], vec![]))
        .call(json!({"url":"https://example.com"}))
        .await
        .unwrap_err();
    assert!(denied.to_string().contains("permission denied"));
}

#[test]
fn html_conversion_and_limits() {
    let url = Url::parse("https://example.com/releases/").unwrap();
    let args: Arguments = serde_json::from_value(json!({"url":url.as_str()})).unwrap();
    let result: Value = serde_json::from_str(&convert(&args, &url, 200, "text/html", b"<title>Versions</title><h1>Release</h1><script>secret</script><style>hidden</style><a href='../v2'>Version 2</a><pre>cargo test</pre>").unwrap()).unwrap();
    assert_eq!(result["title"], "Versions");
    assert_eq!(result["links"][0], "https://example.com/v2");
    let content = result["content"].as_str().unwrap();
    assert!(content.contains("Release") && content.contains("cargo test"));
    assert!(!content.contains("secret") && !content.contains("hidden"));
    let short = Arguments {
        max_chars: 2,
        ..args
    };
    let result: Value =
        serde_json::from_str(&convert(&short, &url, 404, "text/plain", "ééé".as_bytes()).unwrap())
            .unwrap();
    assert_eq!(result["content"], "éé");
    assert_eq!(result["truncated"], true);
    assert!(convert(&short, &url, 200, "application/json", br#"{"v":2}"#).is_err());
    assert!(convert(&short, &url, 200, "application/json", b"invalid").is_err());
}

#[test]
fn representations_preserve_data_and_reject_binary() {
    let url = Url::parse("https://example.com/").unwrap();
    for (representation, mime, body, expected) in [
        (
            "auto",
            "application/json",
            r#"{"version":"2"}"#,
            json!({"version":"2"}),
        ),
        (
            "auto",
            "application/xml",
            "<version>2</version>",
            json!("<version>2</version>"),
        ),
        ("raw", "text/html", "<h1>2</h1>", json!("<h1>2</h1>")),
        ("json", "text/plain", "[1,2]", json!([1, 2])),
    ] {
        let args: Arguments =
            serde_json::from_value(json!({"url":url.as_str(),"representation":representation}))
                .unwrap();
        let result: Value =
            serde_json::from_str(&convert(&args, &url, 200, mime, body.as_bytes()).unwrap())
                .unwrap();
        assert_eq!(result["content"], expected);
        assert_eq!(result["truncated"], false);
    }
    let args: Arguments = serde_json::from_value(json!({"url":url.as_str()})).unwrap();
    assert!(convert(&args, &url, 200, "image/png", b"image").is_err());
    assert!(convert(&args, &url, 200, "text/plain", b"\xff").is_err());
    assert!(convert(&args, &url, 200, "text/plain", b"a\0b").is_err());
}
