//! Tests for `/diagrams` extraction and selection parsing.
//!
//! The parsing half is what silently rots: a transcript is markdown produced by
//! a model, so fences, headings, and unterminated blocks all show up in
//! practice. These use real transcript-shaped input, including the exact
//! flowchart text from the session that motivated the command.

use super::{extract_mermaid_blocks, parse_selection};

#[test]
fn extracts_blocks_with_their_nearest_heading() {
    let markdown = "\
Intro prose.

## 1. The data model

```mermaid
flowchart TD
  A[parent] --> B[child]
```

Some prose in between.

## 4. Update path

```mermaid
flowchart TD
  u1{\"parent_id in body?\"} -->|omitted| u9
  u9 -->|\"parent + active=0\"| u10[\"CASCADE pause\"]
```
";

    let blocks = extract_mermaid_blocks(markdown);
    assert_eq!(blocks.len(), 2, "{blocks:#?}");
    assert_eq!(blocks[0].label.as_deref(), Some("1. The data model"));
    assert!(blocks[0].source.starts_with("flowchart TD"));
    assert!(!blocks[0].source.contains("```"), "fences must be stripped");

    // The label must be the *nearest* preceding heading, not the first one.
    assert_eq!(blocks[1].label.as_deref(), Some("4. Update path"));
    assert!(blocks[1].source.contains("CASCADE pause"));
}

#[test]
fn ignores_non_mermaid_code_blocks() {
    let markdown = "\
```rust
fn main() { println!(\"flowchart TD\"); }
```

```
flowchart TD
  A --> B
```
";
    assert!(
        extract_mermaid_blocks(markdown).is_empty(),
        "only ```mermaid / ```mmd fences count, and a rust block that merely \
         mentions flowchart is not a diagram"
    );
}

/// Fence-language acceptance is delegated to `mermaid::is_mermaid_lang`, so
/// `/diagrams` finds exactly the blocks the inline renderer would have drawn.
/// That function matches `mermaid` and `mermaid`-prefixed langs (e.g. the
/// `mermaid theme=dark` form) but not the `mmd` alias, so neither does this.
#[test]
fn fence_language_matches_the_inline_renderer() {
    let with_attrs =
        extract_mermaid_blocks("```mermaid theme=dark\nflowchart LR\n  A --> B\n```\n");
    assert_eq!(with_attrs.len(), 1, "{with_attrs:#?}");

    assert!(
        extract_mermaid_blocks("```mmd\nflowchart LR\n  A --> B\n```\n").is_empty(),
        "mmd is not accepted by is_mermaid_lang, so the inline path would not \
         have rendered it either"
    );
}

/// A turn interrupted mid-stream leaves an unterminated fence. The diagram is
/// still there and the user asked to see it, so it must not be dropped.
#[test]
fn recovers_an_unterminated_block() {
    let blocks = extract_mermaid_blocks("### Streaming\n\n```mermaid\nflowchart TD\n  A --> B\n");
    assert_eq!(blocks.len(), 1, "{blocks:#?}");
    assert_eq!(blocks[0].label.as_deref(), Some("Streaming"));
    assert!(blocks[0].source.contains("A --> B"));
}

#[test]
fn skips_an_empty_fence() {
    assert!(extract_mermaid_blocks("```mermaid\n\n```\n").is_empty());
}

#[test]
fn indented_fences_still_count() {
    // Models routinely indent a fence inside a list item.
    let blocks =
        extract_mermaid_blocks("- step:\n  ```mermaid\n  flowchart LR\n    A --> B\n  ```\n");
    assert_eq!(blocks.len(), 1, "{blocks:#?}");
}

#[test]
fn selection_defaults_to_everything() {
    assert_eq!(parse_selection("", 3), Ok(vec![0, 1, 2]));
    assert_eq!(parse_selection("all", 2), Ok(vec![0, 1]));
    assert_eq!(parse_selection("ALL", 2), Ok(vec![0, 1]));
}

#[test]
fn selection_supports_last_and_one_based_indices() {
    assert_eq!(parse_selection("last", 3), Ok(vec![2]));
    assert_eq!(parse_selection("2", 3), Ok(vec![1]));
    assert_eq!(parse_selection("3", 3), Ok(vec![2]));
}

/// Out-of-range and typo'd selections must report, never silently widen to
/// "open all 12 diagrams".
#[test]
fn selection_rejects_bad_input() {
    assert!(parse_selection("0", 3).is_err(), "diagrams are 1-based");
    assert!(parse_selection("4", 3).is_err(), "out of range");
    assert!(
        parse_selection("frist", 3).is_err(),
        "typo is not a wildcard"
    );
    assert!(parse_selection("-1", 3).is_err());

    let error = parse_selection("9", 2).unwrap_err();
    assert!(
        error.contains('2'),
        "the error should say how many exist: {error}"
    );
}

#[test]
fn last_on_an_empty_transcript_selects_nothing() {
    assert_eq!(parse_selection("last", 0), Ok(vec![]));
}
