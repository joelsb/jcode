use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

const MAX_ENTRIES: usize = 100;
const DEFAULT_IGNORE: &[&str] = &[
    "node_modules",
    "__pycache__",
    ".git",
    "dist",
    "build",
    "target",
    ".next",
    ".nuxt",
    "venv",
    ".venv",
    "coverage",
    ".cache",
];

pub struct LsTool;

impl LsTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct LsInput {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    ignore: Option<Vec<String>>,
}

struct DirEntry {
    name: String,
    is_dir: bool,
    depth: usize,
}

#[async_trait]
impl Tool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }

    fn description(&self) -> &str {
        "List directory contents."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "path": {
                    "type": "string",
                    "description": "Directory path."
                },
                "ignore": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Ignore patterns."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: LsInput = serde_json::from_value(input)?;

        let base_path = params.path.clone().unwrap_or_else(|| ".".to_string());
        let base = ctx.resolve_path(Path::new(&base_path));
        let ignore_extra = params.ignore.clone();
        // Cloned for the blocking closure: the ignore file is resolved relative
        // to the session working directory, not to the directory being listed,
        // so that `ls some/subdir` obeys the same project-level rules.
        let working_dir = ctx.working_dir.clone();

        if !base.exists() {
            return Err(anyhow::anyhow!("Directory not found: {}", base_path));
        }

        if !base.is_dir() {
            return Err(anyhow::anyhow!("Not a directory: {}", base_path));
        }

        let (entries, suppressed) = tokio::task::spawn_blocking(move || {
            let mut ignore_patterns: Vec<String> =
                DEFAULT_IGNORE.iter().map(|s| s.to_string()).collect();
            if let Some(extra) = ignore_extra {
                ignore_patterns.extend(extra);
            }

            let mut entries: Vec<DirEntry> = Vec::new();
            let mut suppressed = 0usize;
            collect_entries(
                &base,
                0,
                &ignore_patterns,
                working_dir.as_deref(),
                &mut entries,
                MAX_ENTRIES,
                &mut suppressed,
            )?;
            Ok::<_, anyhow::Error>((entries, suppressed))
        })
        .await??;

        let truncated = entries.len() >= MAX_ENTRIES;

        let mut output = String::new();
        output.push_str(&format!("{}/\n", base_path));

        for entry in &entries {
            let indent = "  ".repeat(entry.depth);
            let suffix = if entry.is_dir { "/" } else { "" };
            output.push_str(&format!("{}{}{}\n", indent, entry.name, suffix));
        }

        if truncated {
            output.push_str(&format!("\n... truncated at {} entries", MAX_ENTRIES));
        }

        let file_count = entries.iter().filter(|e| !e.is_dir).count();
        let dir_count = entries.iter().filter(|e| e.is_dir).count();
        output.push_str(&format!(
            "\n{} files, {} directories",
            file_count, dir_count
        ));

        // Say when entries were withheld. A silently short listing reads as
        // "the directory contains this and nothing else", which is a plausible
        // wrong answer; naming the count and the file keeps the omission visible.
        if suppressed > 0 {
            let name = ctx
                .working_dir
                .as_deref()
                .and_then(jcode_tool_core::agent_ignore::ignore_file_for)
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .unwrap_or_else(|| ".claudeignore".to_string());
            output.push_str(&format!(
                "\n{suppressed} entr(ies) hidden by {name} (suppression, not absence)"
            ));
        }

        Ok(ToolOutput::new(output))
    }
}

fn collect_entries(
    dir: &Path,
    depth: usize,
    ignore: &[String],
    working_dir: Option<&Path>,
    entries: &mut Vec<DirEntry>,
    max: usize,
    suppressed: &mut usize,
) -> Result<()> {
    if entries.len() >= max {
        return Ok(());
    }

    let mut items: Vec<_> = std::fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();

    // Cache file_type from DirEntry (uses cached readdir data, no extra stat on most platforms)
    // Then sort using cached values instead of calling is_dir() in the comparator
    let mut typed_items: Vec<(std::fs::DirEntry, bool)> = items
        .drain(..)
        .map(|e| {
            let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            (e, is_dir)
        })
        .collect();

    typed_items.sort_by(|(a, a_dir), (b, b_dir)| match (*a_dir, *b_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.file_name().cmp(&b.file_name()),
    });

    for (item, is_dir) in typed_items {
        if entries.len() >= max {
            break;
        }

        let name = item.file_name().to_string_lossy().to_string();

        if ignore.iter().any(|p| {
            glob::Pattern::new(p)
                .map(|pat| pat.matches(&name))
                .unwrap_or(false)
                || name == *p
        }) {
            continue;
        }

        if name.starts_with('.') && name != "." && name != ".." {
            continue;
        }

        // Project-level `.claudeignore`. Checked after the cheap name-based
        // filters above so the common case never builds a path, and applied to
        // directories too: pruning here stops the walk from descending, which
        // is where the real cost of a large vendored tree would come from.
        if jcode_tool_core::agent_ignore::is_ignored(working_dir, &item.path()) {
            *suppressed += 1;
            continue;
        }

        entries.push(DirEntry {
            name: name.clone(),
            is_dir,
            depth: depth + 1,
        });

        if is_dir && depth < 5 {
            let path = item.path();
            collect_entries(
                &path,
                depth + 1,
                ignore,
                working_dir,
                entries,
                max,
                suppressed,
            )?;
        }
    }

    Ok(())
}
