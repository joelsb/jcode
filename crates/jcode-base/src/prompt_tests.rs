use super::*;

/// Verify the default system prompt does NOT identify as "Claude Code"
/// It's fine to say "powered by Claude" but not "Claude Code" (Anthropic's product)
#[test]
fn test_default_system_prompt_no_claude_code_identity() {
    let prompt = DEFAULT_SYSTEM_PROMPT.to_lowercase();

    assert!(
        !prompt.contains("claude code"),
        "DEFAULT_SYSTEM_PROMPT should NOT identify as 'Claude Code'. Found in system_prompt.md"
    );
    assert!(
        !prompt.contains("claude-code"),
        "DEFAULT_SYSTEM_PROMPT should NOT contain 'claude-code'. Found in system_prompt.md"
    );
}

/// The shell prompt module must always reach the model: it is the fix for
/// trailing-`grep` exit 1 being mistaken for a broken command. On macOS it must
/// also warn about bash 3.2, because `/bin/bash` there predates bash 4 builtins.
#[test]
fn shell_prompt_module_is_always_present_and_platform_aware() {
    let (prompt, _) = build_system_prompt_split_with_capabilities(
        None,
        &[],
        false,
        None,
        None,
        PromptCapabilities { mermaid: false },
    );
    assert!(
        prompt.static_part.contains(SHELL_PROMPT_BASE),
        "shell guidance must be in the static system prompt"
    );
    assert!(prompt.static_part.contains("reports exit 1 on *no match*"));

    let has_macos_block = prompt.static_part.contains(SHELL_PROMPT_MACOS);
    assert_eq!(
        has_macos_block,
        cfg!(target_os = "macos"),
        "bash 3.2 warning must appear exactly on macOS"
    );

    let module = shell_prompt();
    assert_eq!(module.contains("bash 3.2"), cfg!(target_os = "macos"));
}

#[test]
fn mermaid_prompt_module_follows_capability() {
    let (enabled, _) = build_system_prompt_split_with_capabilities(
        None,
        &[],
        false,
        None,
        None,
        PromptCapabilities { mermaid: true },
    );
    assert!(enabled.static_part.contains(MERMAID_PROMPT));

    let (disabled, _) = build_system_prompt_split_with_capabilities(
        None,
        &[],
        false,
        None,
        None,
        PromptCapabilities { mermaid: false },
    );
    assert!(!disabled.static_part.contains("Mermaid diagrams"));
    assert!(!disabled.static_part.contains("fenced `mermaid` code block"));
}

/// Verify skill prompts don't accidentally introduce "Claude Code" identity
#[test]
fn test_skill_prompt_integration() {
    // Test that a skill prompt is properly appended and doesn't break anything
    let skill_prompt = "You are helping with a debugging task.";
    let prompt = build_system_prompt(Some(skill_prompt), &[]);

    // The prompt should contain our default system prompt
    assert!(prompt.contains("Your name is Jcode."));

    // The prompt should contain the skill prompt
    assert!(prompt.contains(skill_prompt));

    // The base prompt parts (excluding user-provided instruction files) should NOT contain
    // "Claude Code". We check DEFAULT_SYSTEM_PROMPT separately since user files may
    // legitimately contain it.
    let default_lower = DEFAULT_SYSTEM_PROMPT.to_lowercase();
    assert!(
        !default_lower.contains("claude code"),
        "DEFAULT_SYSTEM_PROMPT should NOT identify as 'Claude Code'"
    );
}

#[test]
fn skill_description_collapses_whitespace_without_clipping_short_text() {
    assert_eq!(
        clip_skill_description("  Build\n\tand   test the project.  "),
        "Build and test the project."
    );
}

#[test]
fn skill_description_clips_to_the_character_limit_with_ellipsis() {
    let clipped = clip_skill_description(&"a".repeat(SKILL_DESC_MAX_CHARS + 20));

    assert_eq!(clipped.chars().count(), SKILL_DESC_MAX_CHARS);
    assert!(clipped.ends_with('…'));
}

#[test]
fn skill_description_clipping_is_utf8_safe() {
    let clipped = clip_skill_description(&"ж".repeat(SKILL_DESC_MAX_CHARS + 20));

    assert_eq!(clipped.chars().count(), SKILL_DESC_MAX_CHARS);
    assert_eq!(
        clipped
            .chars()
            .filter(|character| *character == 'ж')
            .count(),
        SKILL_DESC_MAX_CHARS - 1
    );
    assert!(clipped.ends_with('…'));
}

#[test]
fn full_and_split_prompt_builders_use_the_same_one_line_skill_descriptions() {
    let skills = vec![SkillInfo {
        name: "example".to_string(),
        description: format!("First line\n\t{}", "д".repeat(SKILL_DESC_MAX_CHARS + 20)),
        model_invocable: true,
    }];
    let expected = build_available_skills_section(&skills).expect("skills section");

    let (full, full_info) = build_system_prompt_full(None, &skills, false, None, None);
    let (split, split_info) = build_system_prompt_split(None, &skills, false, None, None);

    assert!(full.contains(&expected));
    assert!(split.static_part.contains(&expected));
    assert_eq!(full_info.skills_chars, expected.len());
    assert_eq!(split_info.skills_chars, expected.len());
    assert!(expected.contains("First line "));
    assert!(!expected.contains("First line\n"));
    let entry = expected
        .lines()
        .find(|line| line.starts_with("- `/example `"))
        .expect("example skill entry");
    assert!(entry.ends_with('…'));
}

#[test]
fn test_load_agents_md_files_uses_sandboxed_global_files() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let temp = tempfile::TempDir::new().unwrap();
    crate::env::set_var("JCODE_HOME", temp.path());
    std::fs::create_dir_all(temp.path().join("external")).unwrap();

    std::fs::write(
        temp.path().join("external/AGENTS.md"),
        "sandboxed global agents instructions",
    )
    .unwrap();

    let project_dir = tempfile::TempDir::new().unwrap();
    let (content, info) = load_agents_md_files_from_dir(Some(project_dir.path()));

    assert!(info.has_global_agents_md);
    let content = content.expect("global instructions content");
    assert!(content.contains("# Global Instructions (~/AGENTS.md)"));
    assert!(!content.contains("~/.AGENTS.md"));
    assert!(content.contains("sandboxed global agents instructions"));

    let sandboxed_home = temp.path().join("external");
    let (content, info) = load_agents_md_files_from_dir(Some(&sandboxed_home));
    let content = content.expect("deduplicated home instructions");
    assert!(info.has_project_agents_md);
    assert!(!info.has_global_agents_md);
    assert!(content.contains("# Project Instructions (AGENTS.md)"));
    assert!(!content.contains("# Global Instructions (~/AGENTS.md)"));
    assert_eq!(
        content
            .matches("sandboxed global agents instructions")
            .count(),
        1
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn agents_md_same_canonical_file_is_loaded_only_as_project_instructions() {
    let project_dir = tempfile::TempDir::new().unwrap();
    let agents_md = project_dir.path().join("AGENTS.md");
    std::fs::write(&agents_md, "shared instructions").unwrap();

    let (content, info) = load_agents_md_files_from_dirs(project_dir.path(), Some(&agents_md));
    let content = content.expect("project instructions");

    assert!(info.has_project_agents_md);
    assert!(!info.has_global_agents_md);
    assert!(content.contains("# Project Instructions (AGENTS.md)"));
    assert!(!content.contains("# Global Instructions (~/AGENTS.md)"));
    assert_eq!(content.matches("shared instructions").count(), 1);
}

#[test]
fn agents_md_distinct_project_and_global_files_are_both_loaded() {
    let project_dir = tempfile::TempDir::new().unwrap();
    let global_dir = tempfile::TempDir::new().unwrap();
    let global_agents_md = global_dir.path().join("AGENTS.md");
    std::fs::write(project_dir.path().join("AGENTS.md"), "project instructions").unwrap();
    std::fs::write(&global_agents_md, "global instructions").unwrap();

    let (content, info) =
        load_agents_md_files_from_dirs(project_dir.path(), Some(&global_agents_md));
    let content = content.expect("project and global instructions");

    assert!(info.has_project_agents_md);
    assert!(info.has_global_agents_md);
    assert!(content.contains("project instructions"));
    assert!(content.contains("global instructions"));
}

#[test]
fn agents_md_missing_global_file_keeps_project_instructions() {
    let project_dir = tempfile::TempDir::new().unwrap();
    let global_dir = tempfile::TempDir::new().unwrap();
    let missing_global_agents_md = global_dir.path().join("missing-AGENTS.md");
    std::fs::write(project_dir.path().join("AGENTS.md"), "project only").unwrap();

    let (content, info) =
        load_agents_md_files_from_dirs(project_dir.path(), Some(&missing_global_agents_md));
    let content = content.expect("project instructions");

    assert!(info.has_project_agents_md);
    assert!(!info.has_global_agents_md);
    assert!(content.contains("project only"));
    assert!(!content.contains("# Global Instructions (~/AGENTS.md)"));
}

#[cfg(unix)]
#[test]
fn agents_md_symlink_alias_is_deduplicated_by_canonical_file_path() {
    use std::os::unix::fs::symlink;

    let project_dir = tempfile::TempDir::new().unwrap();
    let global_dir = tempfile::TempDir::new().unwrap();
    let global_agents_md = global_dir.path().join("AGENTS.md");
    std::fs::write(&global_agents_md, "symlinked instructions").unwrap();
    symlink(&global_agents_md, project_dir.path().join("AGENTS.md")).unwrap();

    let (content, info) =
        load_agents_md_files_from_dirs(project_dir.path(), Some(&global_agents_md));
    let content = content.expect("project instructions through symlink");

    assert!(info.has_project_agents_md);
    assert!(!info.has_global_agents_md);
    assert_eq!(content.matches("symlinked instructions").count(), 1);
}

#[test]
fn test_session_context_includes_time_timezone_and_system_info() {
    let context = build_session_context(None);
    assert!(context.contains("# Session Context"));
    assert!(context.contains("Time: "));
    assert!(context.contains("Timezone: UTC"));
    assert!(context.contains("OS: "));
    assert!(context.contains("Architecture: "));
    assert!(context.contains("Jcode version: "));
    assert!(!context.contains("Working directory: "));
    assert!(!context.contains("Git:"));
}

#[test]
fn test_split_prompt_does_not_inject_session_context_per_turn() {
    let (split, _info) = build_system_prompt_split(None, &[], false, None, None);
    assert!(!split.dynamic_part.contains("# Session Context"));
    assert!(!split.dynamic_part.contains("Time: "));
    assert!(!split.dynamic_part.contains("Timezone: UTC"));
}

#[test]
fn sponsored_discovery_is_not_injected_into_the_system_prompt() {
    let (split, _) = build_system_prompt_split(None, &[], false, None, None);
    assert!(!split.static_part.contains("Discoverable Tools"));
    assert!(!split.static_part.contains("integration_tools"));
}

#[test]
fn test_prompt_overlay_files_are_loaded_from_project_and_global_jcode_dirs() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let temp = tempfile::TempDir::new().unwrap();
    crate::env::set_var("JCODE_HOME", temp.path());
    std::fs::create_dir_all(temp.path()).unwrap();
    std::fs::write(
        temp.path().join("prompt-overlay.md"),
        "global prompt overlay instructions",
    )
    .unwrap();

    let project_dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(project_dir.path().join(".jcode")).unwrap();
    std::fs::write(
        project_dir.path().join(".jcode/prompt-overlay.md"),
        "project prompt overlay instructions",
    )
    .unwrap();

    let direct = load_prompt_overlay_files_from_dir(Some(project_dir.path()));

    assert!(direct.0.is_some(), "expected prompt overlay content");
    let direct_content = direct.0.unwrap();
    assert!(
        direct_content.contains("project prompt overlay instructions"),
        "expected project prompt overlay content"
    );
    assert!(
        direct_content.contains("global prompt overlay instructions"),
        "expected global prompt overlay content"
    );

    let (prompt, info) = build_system_prompt_full(None, &[], false, None, Some(project_dir.path()));
    assert!(prompt.contains("project prompt overlay instructions"));
    assert!(prompt.contains("global prompt overlay instructions"));
    assert!(info.prompt_overlay_chars > 0);

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn test_preferred_tools_files_are_loaded_from_project_and_global_jcode_dirs() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let temp = tempfile::TempDir::new().unwrap();
    crate::env::set_var("JCODE_HOME", temp.path());
    std::fs::create_dir_all(temp.path()).unwrap();
    std::fs::write(
        temp.path().join("preferred-tools.md"),
        "global preferred tools instructions",
    )
    .unwrap();

    let project_dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(project_dir.path().join(".jcode")).unwrap();
    std::fs::write(
        project_dir.path().join(".jcode/preferred-tools.md"),
        "project preferred tools instructions",
    )
    .unwrap();

    let direct = load_preferred_tools_files_from_dir(Some(project_dir.path()));

    assert!(direct.0.is_some(), "expected preferred tools content");
    let direct_content = direct.0.unwrap();
    assert!(
        direct_content.contains("Project Preferred Tools (.jcode/preferred-tools.md)"),
        "expected project preferred tools section heading"
    );
    assert!(
        direct_content.contains("project preferred tools instructions"),
        "expected project preferred tools content"
    );
    assert!(
        direct_content.contains("Global Preferred Tools (~/.jcode/preferred-tools.md)"),
        "expected global preferred tools section heading"
    );
    assert!(
        direct_content.contains("global preferred tools instructions"),
        "expected global preferred tools content"
    );

    let (prompt, info) = build_system_prompt_full(None, &[], false, None, Some(project_dir.path()));
    assert!(prompt.contains("project preferred tools instructions"));
    assert!(prompt.contains("global preferred tools instructions"));
    assert!(info.preferred_tools_chars > 0);

    let (split, split_info) =
        build_system_prompt_split(None, &[], false, None, Some(project_dir.path()));
    assert!(
        split
            .static_part
            .contains("project preferred tools instructions")
    );
    assert!(
        split
            .static_part
            .contains("global preferred tools instructions")
    );
    assert!(split_info.preferred_tools_chars > 0);

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn test_swarm_prompt_prefers_project_then_global_then_default() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let temp = tempfile::TempDir::new().unwrap();
    crate::env::set_var("JCODE_HOME", temp.path());
    std::fs::create_dir_all(temp.path()).unwrap();

    let project_dir = tempfile::TempDir::new().unwrap();

    // No override files: built-in default.
    let prompt = load_swarm_prompt(Some(project_dir.path()));
    assert_eq!(prompt, DEFAULT_SWARM_PROMPT.trim());

    // Global override wins over the default.
    std::fs::write(temp.path().join("swarm-prompt.md"), "global swarm routing").unwrap();
    let prompt = load_swarm_prompt(Some(project_dir.path()));
    assert_eq!(prompt, "global swarm routing");

    // Project override wins over global.
    std::fs::create_dir_all(project_dir.path().join(".jcode")).unwrap();
    std::fs::write(
        project_dir.path().join(".jcode/swarm-prompt.md"),
        "project swarm routing",
    )
    .unwrap();
    let prompt = load_swarm_prompt(Some(project_dir.path()));
    assert_eq!(prompt, "project swarm routing");

    // A blank project file falls through to global instead of going empty.
    std::fs::write(project_dir.path().join(".jcode/swarm-prompt.md"), "   \n").unwrap();
    let prompt = load_swarm_prompt(Some(project_dir.path()));
    assert_eq!(prompt, "global swarm routing");

    if let Some(prev_home) = prev_home {
        crate::env::set_var("JCODE_HOME", prev_home);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
}

#[test]
fn test_default_swarm_prompt_mentions_model_and_list_models() {
    assert!(DEFAULT_SWARM_PROMPT.contains("list_models"));
    assert!(DEFAULT_SWARM_PROMPT.contains("model"));
    assert!(DEFAULT_SWARM_PROMPT.contains("effort"));
    assert!(DEFAULT_SWARM_PROMPT.contains("only the root session may spawn agents"));
    assert!(DEFAULT_SWARM_PROMPT.contains("swarm-deep"));
}

#[test]
fn test_non_selfdev_prompt_leaves_selfdev_guidance_to_the_tool_schema() {
    let prompt = build_system_prompt(None, &[]);
    assert!(!prompt.contains("Self-Development Access"));
    assert!(!prompt.contains("You have access to the `selfdev` tool in all sessions"));
    assert!(!prompt.contains("You are working on the jcode codebase itself."));
}

#[test]
fn test_selfdev_prompt_uses_full_selfdev_instructions() {
    let prompt = build_system_prompt_with_selfdev(None, &[], true);
    assert!(prompt.contains("You are working on the jcode codebase itself."));
    assert!(prompt.contains("launched from the TUI/root jcode context"));
    assert!(prompt.contains("selfdev build target=tui"));
    assert!(!prompt.contains("Self-Development Access"));
}

#[test]
fn test_selfdev_prompt_uses_desktop_focus_for_desktop_working_dir() {
    let desktop_dir = std::path::Path::new("/tmp/jcode/crates/jcode-desktop2/src");
    let (prompt, _info) = build_system_prompt_full(None, &[], true, None, Some(desktop_dir));
    assert!(prompt.contains("launched from the jcode-desktop2"));
    assert!(prompt.contains("selfdev build target=desktop2"));
    assert!(!prompt.contains("launched from the TUI/root jcode context"));
}

#[test]
fn test_split_selfdev_prompt_defaults_to_tui_focus_for_repo_root() {
    let repo_dir = std::path::Path::new("/tmp/jcode");
    let (split, _info) = build_system_prompt_split(None, &[], true, None, Some(repo_dir));
    assert!(
        split
            .static_part
            .contains("launched from the TUI/root jcode context")
    );
    assert!(split.static_part.contains("selfdev build target=tui"));
}

#[test]
fn test_selfdev_prompt_prefers_publish_flow_for_active_builds() {
    let prompt = build_system_prompt_with_selfdev(None, &[], true);
    assert!(prompt.contains("selfdev build"));
    assert!(prompt.contains("cancel-build"));
    assert!(prompt.contains("selfdev reload"));
    assert!(prompt.contains("fallback when `selfdev build` is not appropriate"));
    assert!(prompt.contains("scripts/dev_cargo.sh build --profile selfdev -p jcode --bin jcode"));
    assert!(prompt.contains("remote build host is configured"));
    assert!(prompt.contains("Do not wait for user input"));
}

#[test]
fn test_selfdev_prompt_template_placeholders_are_resolved() {
    let static_prompt = build_selfdev_prompt_static();
    let dynamic_prompt = build_selfdev_prompt();
    assert!(!static_prompt.contains("__DEBUG_SOCKET_BLOCK__"));
    assert!(!dynamic_prompt.contains("__DEBUG_SOCKET_BLOCK__"));
    assert!(!static_prompt.contains("__SELFDEV_PRODUCT_FOCUS__"));
    assert!(!dynamic_prompt.contains("__SELFDEV_PRODUCT_FOCUS__"));
    assert_eq!(static_prompt, dynamic_prompt);
}

#[test]
fn split_prompt_estimated_tokens_is_positive_when_populated() {
    let (split, _info) = build_system_prompt_split(None, &[], false, None, None);
    assert!(split.chars() > 0);
    assert!(split.estimated_tokens() > 0);
}

#[test]
fn swarm_effort_directive_is_appended_only_for_swarm_sentinel() {
    assert!(is_swarm_effort("swarm"));
    assert!(is_swarm_effort("  Swarm "));
    assert!(!is_swarm_effort("xhigh"));

    let mut split = SplitSystemPrompt {
        static_part: "base".to_string(),
        dynamic_part: String::new(),
    };
    append_swarm_effort_directive(&mut split, Some("xhigh"));
    assert!(!split.dynamic_part.contains("Swarm Effort"));

    append_swarm_effort_directive(&mut split, Some("swarm"));
    assert!(split.dynamic_part.contains("# Swarm Effort"));
    assert!(split.dynamic_part.contains("swarm` tool"));

    // None / empty effort should not inject.
    let mut other = SplitSystemPrompt::default();
    append_swarm_effort_directive(&mut other, None);
    assert!(other.dynamic_part.is_empty());
}

#[test]
fn swarm_deep_effort_injects_task_graph_directive() {
    use crate::prompt::is_deep_swarm_effort;

    assert!(is_swarm_effort("swarm-deep"));
    assert!(is_deep_swarm_effort("swarm-deep"));
    assert!(is_deep_swarm_effort("  Swarm-Deep "));
    assert!(!is_deep_swarm_effort("swarm"));
    assert!(!is_deep_swarm_effort("xhigh"));

    // Deep sentinel injects the DAG-first task-graph directive, not the light one.
    let mut split = SplitSystemPrompt::default();
    append_swarm_effort_directive(&mut split, Some("swarm-deep"));
    assert!(split.dynamic_part.contains("# Deep Task Graph"));
    assert!(split.dynamic_part.contains("swarm task_graph"));
    assert!(!split.dynamic_part.contains("# Swarm Effort"));

    // Light sentinel still injects the fan-out directive, not the deep one.
    let mut light = SplitSystemPrompt::default();
    append_swarm_effort_directive(&mut light, Some("swarm"));
    assert!(light.dynamic_part.contains("# Swarm Effort"));
    assert!(!light.dynamic_part.contains("# Deep Task Graph"));
}

#[test]
fn classify_effort_distinguishes_reasoning_from_swarm_modes() {
    use crate::prompt::{EffortKind, classify_effort, is_swarm_mode_effort};

    // Plain reasoning levels are not swarm modes.
    for level in ["none", "minimal", "low", "medium", "high", "xhigh", "max"] {
        assert_eq!(classify_effort(level), EffortKind::Reasoning, "{level}");
        assert!(!is_swarm_mode_effort(level), "{level}");
    }

    assert_eq!(classify_effort("swarm"), EffortKind::SwarmLight);
    assert_eq!(classify_effort("swarm-deep"), EffortKind::SwarmDeep);
    assert!(is_swarm_mode_effort("swarm"));
    assert!(is_swarm_mode_effort("  Swarm-Deep "));
    assert!(EffortKind::SwarmLight.is_swarm_mode());
    assert!(EffortKind::SwarmDeep.is_swarm_mode());
    assert!(!EffortKind::Reasoning.is_swarm_mode());
}

#[test]
fn test_selfdev_prompt_uses_desktop2_focus_for_desktop2_working_dir() {
    let desktop2_dir = std::path::Path::new("/tmp/jcode/crates/jcode-desktop2/src");
    let (prompt, _info) = build_system_prompt_full(None, &[], true, None, Some(desktop2_dir));
    assert!(prompt.contains("launched from the jcode-desktop2"));
    assert!(prompt.contains("selfdev build target=desktop2"));
    assert!(!prompt.contains("launched from the TUI/root jcode context"));
}

#[test]
fn project_system_prompt_file_replaces_default_base_prompt() {
    use crate::prompt::load_base_system_prompt;

    let dir = std::env::temp_dir().join(format!("jcode-sysprompt-{}", std::process::id()));
    let jcode_dir = dir.join(".jcode");
    std::fs::create_dir_all(&jcode_dir).unwrap();
    std::fs::write(
        jcode_dir.join("system-prompt.md"),
        "You are a custom agent.\n",
    )
    .unwrap();

    assert_eq!(
        load_base_system_prompt(Some(&dir)),
        "You are a custom agent."
    );

    let (prompt, _info) = build_system_prompt_full(None, &[], false, None, Some(&dir));
    assert!(prompt.contains("You are a custom agent."));
    assert!(!prompt.contains("Jcode is open source"));

    // Empty override falls back to the built-in default.
    std::fs::write(jcode_dir.join("system-prompt.md"), "   \n").unwrap();
    assert_eq!(load_base_system_prompt(Some(&dir)), DEFAULT_SYSTEM_PROMPT);

    std::fs::remove_dir_all(&dir).ok();
}

/// A skill that opts out via `disable-model-invocation: true` must not appear in
/// the rendered "Available Skills" section, while normal skills still do.
///
/// This guards a real regression: `SkillFrontmatter` used to parse only
/// `name`/`description`/`allowed-tools`, so serde silently dropped the
/// `disable-model-invocation` key. The flag therefore looked honoured (it is, in
/// Claude Code) while every opted-out skill was still advertised to the model,
/// costing ~2.5k prompt tokens here and inviting unrequested self-invocation.
/// Names below are real skills that carry the flag on this setup.
#[test]
fn opted_out_skills_are_excluded_from_available_skills_section() {
    let skills = vec![
        SkillInfo {
            name: "caveman".to_string(),
            description: "opted out of model invocation".to_string(),
            model_invocable: false,
        },
        SkillInfo {
            name: "dain-jira".to_string(),
            description: "also opted out".to_string(),
            model_invocable: false,
        },
        SkillInfo {
            name: "browse".to_string(),
            description: "normal model-invocable skill".to_string(),
            model_invocable: true,
        },
    ];

    let (split, info) = build_system_prompt_split(None, &skills, false, None, None);
    let rendered = format!("{}\n\n{}", split.static_part, split.dynamic_part);

    assert!(
        rendered.contains("/browse"),
        "model-invocable skills must still be listed"
    );
    assert!(
        !rendered.contains("/caveman"),
        "`disable-model-invocation: true` skill leaked into the system prompt"
    );
    assert!(
        !rendered.contains("/dain-jira"),
        "`disable-model-invocation: true` skill leaked into the system prompt"
    );
    assert!(
        info.skills_chars > 0,
        "skills section should still be accounted for"
    );

    // The non-split builder shares the same renderer; assert it too so a future
    // divergence between the two paths cannot silently re-expose opted-out skills.
    let (full, _) = build_system_prompt_full(None, &skills, false, None, None);
    assert!(full.contains("/browse"));
    assert!(!full.contains("/caveman"));
}

/// When every skill opts out, the section must be omitted entirely rather than
/// emitting an empty "Available Skills" header with no entries.
#[test]
fn available_skills_section_is_omitted_when_all_skills_opt_out() {
    let skills = vec![SkillInfo {
        name: "caveman".to_string(),
        description: "opted out".to_string(),
        model_invocable: false,
    }];

    let (split, info) = build_system_prompt_split(None, &skills, false, None, None);

    assert!(!split.static_part.contains("# Available Skills"));
    assert_eq!(info.skills_chars, 0);
}

/// Frontmatter parsing must actually read the flag. Without this, the filter
/// above is correct but never triggers in production.
#[test]
fn skill_frontmatter_parses_disable_model_invocation() {
    let dir = tempfile::tempdir().unwrap();
    let opted_out = dir.path().join("caveman");
    std::fs::create_dir_all(&opted_out).unwrap();
    std::fs::write(
        opted_out.join("SKILL.md"),
        "---\nname: caveman\ndescription: opted out\ndisable-model-invocation: true\n---\n\nbody\n",
    )
    .unwrap();

    let normal = dir.path().join("browse");
    std::fs::create_dir_all(&normal).unwrap();
    std::fs::write(
        normal.join("SKILL.md"),
        "---\nname: browse\ndescription: normal\n---\n\nbody\n",
    )
    .unwrap();

    let mut registry = crate::skill::SkillRegistry::default();
    registry.load_from_dir_for_test(dir.path()).unwrap();

    assert!(!registry.get("caveman").unwrap().model_invocable);
    assert!(registry.get("browse").unwrap().model_invocable);
}

/// Parse the exact frontmatter shapes that exist on disk, including a YAML
/// folded scalar (`description: >`) with the flag placed *after* the folded
/// block. The other tests use the inline `description: text` form; a parser that
/// handled only that form would pass them while silently failing on the real
/// `~/.jcode/skills/caveman/SKILL.md`, so this pins the real-world shape.
#[test]
fn real_world_frontmatter_shapes_parse_the_flag() {
    let dir = tempfile::tempdir().unwrap();

    // Verbatim shape of ~/.jcode/skills/caveman/SKILL.md
    let folded = dir.path().join("caveman");
    std::fs::create_dir_all(&folded).unwrap();
    std::fs::write(
        folded.join("SKILL.md"),
        "---\nname: caveman\ndescription: >\n  Ultra-compressed communication mode. Cuts token usage ~75% by dropping\n  filler, articles, and pleasantries while keeping full technical accuracy.\ndisable-model-invocation: true\n---\n\nbody\n",
    )
    .unwrap();

    // Verbatim shape of ~/.jcode/skills/grill-me/SKILL.md
    let inline = dir.path().join("grill-me");
    std::fs::create_dir_all(&inline).unwrap();
    std::fs::write(
        inline.join("SKILL.md"),
        "---\nname: grill-me\ndescription: A relentless interview to sharpen a plan or design.\ndisable-model-invocation: true\n---\n\nbody\n",
    )
    .unwrap();

    let mut registry = crate::skill::SkillRegistry::default();
    registry.load_from_dir_for_test(dir.path()).unwrap();

    assert!(
        !registry.get("caveman").unwrap().model_invocable,
        "folded-scalar description must not break flag parsing"
    );
    assert!(!registry.get("grill-me").unwrap().model_invocable);
}

/// Report the real prompt cost of the skills installed on this machine.
/// Ignored by default (it reads the developer's own `~/.jcode/skills`), but it
/// is the only check that measures the actual shipped prompt rather than
/// synthetic fixtures. Run with:
///   cargo test -p jcode-base real_installed_skill_cost -- --ignored --nocapture
#[test]
#[ignore]
fn real_installed_skill_cost() {
    let registry = crate::skill::SkillRegistry::load_global().unwrap();
    let all = registry.list();
    let infos: Vec<SkillInfo> = all
        .iter()
        .map(|s| SkillInfo {
            name: s.name.clone(),
            description: s.description.clone(),
            model_invocable: s.model_invocable,
        })
        .collect();

    let (_, info) = build_system_prompt_with_context(None, &infos, false);
    let opted_out = all.iter().filter(|s| !s.model_invocable).count();

    println!(
        "installed={} opted_out={} visible={} skills_chars={} ~tokens={}",
        all.len(),
        opted_out,
        all.len() - opted_out,
        info.skills_chars,
        info.skills_chars / 4
    );
    for skill in all.iter().filter(|s| s.model_invocable) {
        println!("  visible: /{}", skill.name);
    }
}
