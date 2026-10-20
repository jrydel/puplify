//! Notion export: one page per transcript under an existing parent page that
//! has been shared with an internal integration.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::oauth::http;
use crate::config::NotionConfig;
use crate::transcript::Transcript;

const API: &str = "https://api.notion.com/v1";
const NOTION_VERSION: &str = "2022-06-28";
/// Maximum characters in one rich-text element (Notion counts UTF-16 code units).
const RICH_TEXT_MAX: usize = 2000;
/// Maximum rich-text elements in one block.
const RICH_TEXT_ELEMENTS_MAX: usize = 100;
/// Maximum blocks per request.
const CHILDREN_MAX: usize = 100;

/// Extracts the 32-hex page id from a raw id (with or without dashes) or a Notion URL.
fn extract_page_id(input: &str) -> Result<String> {
    let input = input.trim();
    let rest = input.split('#').next().unwrap_or(input);
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));

    // `?p=<id>` links (peek view) carry the page id in the query string.
    let from_query = query.split('&').find_map(|kv| kv.strip_prefix("p="));
    let segment = from_query.or_else(|| path.rsplit('/').find(|s| !s.is_empty())).unwrap_or("");

    // A slug like `My-Page-1429989fe8ac4effbc8f57f56486db54` or a dashed UUID: dropping
    // dashes leaves the id as the final 32 characters.
    let compact: Vec<char> = segment.chars().filter(|&c| c != '-').collect();
    if compact.len() >= 32 {
        let tail: String = compact[compact.len() - 32..].iter().collect();
        if tail.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(tail.to_ascii_lowercase());
        }
    }
    bail!("could not find a Notion page id in {input:?}; paste the page URL or its 32-character id")
}

/// Splits text into rich-text elements of at most `RICH_TEXT_MAX` UTF-16 units each.
fn rich_text_chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut units = 0;
    for c in text.chars() {
        let len = c.len_utf16();
        if units + len > RICH_TEXT_MAX {
            chunks.push(std::mem::take(&mut current));
            units = 0;
        }
        current.push(c);
        units += len;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Paragraph blocks for one line of text (more than one only for absurdly long lines).
fn paragraph_blocks(text: &str) -> Vec<Value> {
    rich_text_chunks(text)
        .chunks(RICH_TEXT_ELEMENTS_MAX)
        .map(|group| {
            let rich_text: Vec<Value> = group.iter().map(|c| json!({ "type": "text", "text": { "content": c } })).collect();
            json!({ "object": "block", "type": "paragraph", "paragraph": { "rich_text": rich_text } })
        })
        .collect()
}

/// First request's children, plus the remaining batches to append afterwards.
fn split_batches(mut blocks: Vec<Value>) -> (Vec<Value>, Vec<Vec<Value>>) {
    let rest = blocks.split_off(blocks.len().min(CHILDREN_MAX));
    (blocks, rest.chunks(CHILDREN_MAX).map(<[Value]>::to_vec).collect())
}

async fn check(resp: reqwest::Response, what: &str) -> Result<Value> {
    if resp.status().is_success() {
        return Ok(resp.json().await?);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    match serde_json::from_str::<Value>(&body).ok().and_then(|v| v["message"].as_str().map(str::to_string)) {
        Some(message) => bail!("{what}: {message} (HTTP {})", status.as_u16()),
        None => bail!("{what}: HTTP {}: {}", status.as_u16(), body.chars().take(300).collect::<String>()),
    }
}

/// Creates a page for the transcript; returns its URL.
pub async fn export(cfg: &NotionConfig, transcript: &Transcript) -> Result<String> {
    let token = cfg.token.trim();
    if token.is_empty() {
        bail!("Notion integration token is not set (see README: Notion setup)");
    }
    if cfg.parent_page_id.trim().is_empty() {
        bail!("Notion parent page is not set (see README: Notion setup)");
    }
    let parent = extract_page_id(&cfg.parent_page_id)?;

    let mut blocks = paragraph_blocks(&transcript.header_line());
    for line in transcript.body_lines() {
        blocks.extend(paragraph_blocks(&line));
    }
    let (first, rest) = split_batches(blocks);

    let client = http()?;
    let title: String = rich_text_chunks(&transcript.title).into_iter().next().unwrap_or_default();
    let page = check(
        client
            .post(format!("{API}/pages"))
            .bearer_auth(token)
            .header("Notion-Version", NOTION_VERSION)
            .json(&json!({
                "parent": { "page_id": parent },
                "properties": { "title": { "title": [{ "type": "text", "text": { "content": title } }] } },
                "children": first,
            }))
            .send()
            .await?,
        "creating Notion page (is the parent page shared with the integration?)",
    )
    .await?;
    let id = page["id"].as_str().context("no page id in Notion response")?;
    for batch in rest {
        check(
            client
                .patch(format!("{API}/blocks/{id}/children"))
                .bearer_auth(token)
                .header("Notion-Version", NOTION_VERSION)
                .json(&json!({ "children": batch }))
                .send()
                .await?,
            "appending to Notion page",
        )
        .await?;
    }
    Ok(page["url"].as_str().unwrap_or(id).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "1429989fe8ac4effbc8f57f56486db54";

    #[test]
    fn page_id_from_raw_ids() {
        assert_eq!(extract_page_id(ID).unwrap(), ID);
        assert_eq!(extract_page_id("  1429989f-e8ac-4eff-bc8f-57f56486db54 ").unwrap(), ID);
        assert_eq!(extract_page_id("1429989FE8AC4EFFBC8F57F56486DB54").unwrap(), ID);
    }

    #[test]
    fn page_id_from_urls() {
        for url in [
            format!("https://www.notion.so/My-Meeting-Notes-{ID}"),
            format!("https://www.notion.so/My-Meeting-Notes-{ID}?pvs=4"),
            format!("https://www.notion.so/workspace/My-Meeting-Notes-{ID}#section"),
            format!("https://www.notion.so/{ID}/"),
            "https://www.notion.so/1429989f-e8ac-4eff-bc8f-57f56486db54".to_string(),
            format!("https://www.notion.so/workspace?p={ID}&pm=s"),
            format!("https://acme.notion.site/Notes-{ID}"),
        ] {
            assert_eq!(extract_page_id(&url).unwrap(), ID, "{url}");
        }
    }

    #[test]
    fn page_id_rejects_garbage() {
        assert!(extract_page_id("https://www.notion.so/workspace").is_err());
        assert!(extract_page_id("not-an-id").is_err());
        assert!(extract_page_id("zzzz989fe8ac4effbc8f57f56486db54").is_err());
    }

    fn content_len(block: &Value) -> Vec<usize> {
        block["paragraph"]["rich_text"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["text"]["content"].as_str().unwrap().encode_utf16().count())
            .collect()
    }

    #[test]
    fn rich_text_splits_at_2000_units_without_losing_text() {
        let text = "a".repeat(4500);
        let chunks = rich_text_chunks(&text);
        assert_eq!(chunks.iter().map(|c| c.len()).collect::<Vec<_>>(), [2000, 2000, 500]);
        assert_eq!(chunks.concat(), text);
        assert_eq!(rich_text_chunks(&"a".repeat(2000)).len(), 1);
        assert!(rich_text_chunks("").is_empty());
    }

    #[test]
    fn rich_text_counts_utf16_units() {
        // 😀 is two UTF-16 units: 1000 of them fill a chunk exactly, the next starts a new one.
        let text = "😀".repeat(1001);
        let chunks = rich_text_chunks(&text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].encode_utf16().count(), 2000);
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn long_line_becomes_one_block_with_multiple_elements() {
        let blocks = paragraph_blocks(&"x".repeat(4500));
        assert_eq!(blocks.len(), 1);
        assert_eq!(content_len(&blocks[0]), [2000, 2000, 500]);
        assert!(paragraph_blocks("").is_empty());
    }

    #[test]
    fn blocks_are_batched_by_100() {
        let blocks: Vec<Value> = (0..250).map(|i| json!(i)).collect();
        let (first, rest) = split_batches(blocks);
        assert_eq!(first.len(), 100);
        assert_eq!(rest.iter().map(Vec::len).collect::<Vec<_>>(), [100, 50]);
        assert_eq!(rest[1][49], json!(249));

        let (first, rest) = split_batches((0..100).map(|i| json!(i)).collect());
        assert_eq!((first.len(), rest.len()), (100, 0));
        let (first, rest) = split_batches(Vec::new());
        assert_eq!((first.len(), rest.len()), (0, 0));
    }
}
