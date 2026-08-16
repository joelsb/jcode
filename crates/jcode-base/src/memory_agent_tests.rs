use super::*;
use crate::memory::MemoryCategory;

#[test]
fn infer_candidate_tag_uses_repeated_non_stopword() {
    let tag =
        infer_candidate_tag("scheduler retries failed jobs and scheduler metrics update dashboard");
    assert_eq!(tag.as_deref(), Some("scheduler"));
}

#[test]
fn apply_cluster_assignment_links_members() {
    let mut graph = MemoryGraph::new();
    let mut a = MemoryEntry::new(MemoryCategory::Fact, "A");
    a.embedding = Some(vec![1.0, 0.0]);
    let id_a = graph.add_memory(a);

    let mut b = MemoryEntry::new(MemoryCategory::Fact, "B");
    b.embedding = Some(vec![0.0, 1.0]);
    let id_b = graph.add_memory(b);

    let stats = apply_cluster_assignment(
        &mut graph,
        "project",
        &[id_a.clone(), id_b.clone()],
        Utc::now(),
    );

    assert_eq!(stats.clusters_touched, 1);
    assert_eq!(stats.member_links, 2);
    assert_eq!(graph.clusters.len(), 1);

    let cluster_id = graph
        .clusters
        .keys()
        .next()
        .expect("cluster id")
        .to_string();
    assert!(
        graph
            .get_edges(&id_a)
            .iter()
            .any(|e| e.target == cluster_id && matches!(e.kind, EdgeKind::InCluster))
    );
    assert!(
        graph
            .get_edges(&id_b)
            .iter()
            .any(|e| e.target == cluster_id && matches!(e.kind, EdgeKind::InCluster))
    );
}

#[test]
fn apply_confidence_updates_batches_boost_and_decay() {
    let _guard = crate::storage::lock_test_env();
    let old = std::env::var("JCODE_HOME").ok();
    let dir = std::env::temp_dir().join(format!(
        "jcode-conf-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    crate::env::set_var("JCODE_HOME", &dir);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let manager = crate::memory::MemoryManager::new().with_project_dir("/tmp/jcode-conf-batch");

        let mut keep_entry = MemoryEntry::new(MemoryCategory::Fact, "verified memory")
            .with_embedding(vec![1.0, 0.0]);
        keep_entry.confidence = 0.5; // below cap so a boost is observable
        let keep = manager.remember_project(keep_entry).unwrap();
        let stale = manager
            .remember_project(
                MemoryEntry::new(MemoryCategory::Fact, "rejected memory")
                    .with_embedding(vec![0.0, 1.0]),
            )
            .unwrap();

        let conf_before = |id: &str| {
            manager
                .load_project_graph()
                .unwrap()
                .get_memory(id)
                .unwrap()
                .confidence
        };
        let keep_before = conf_before(&keep);
        let stale_before = conf_before(&stale);

        let (boosted, decayed) = apply_confidence_updates(
            &manager,
            std::slice::from_ref(&keep),
            std::slice::from_ref(&stale),
        );
        assert_eq!(boosted, 1, "one verified memory boosted");
        assert_eq!(decayed, 1, "one rejected memory decayed");

        let keep_after = conf_before(&keep);
        let stale_after = conf_before(&stale);
        assert!(keep_after > keep_before, "verified confidence should rise");
        assert!(
            stale_after < stale_before,
            "rejected confidence should fall"
        );
    }));

    match old {
        Some(v) => crate::env::set_var("JCODE_HOME", v),
        None => crate::env::remove_var("JCODE_HOME"),
    }
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(p) = result {
        std::panic::resume_unwind(p);
    }
}

#[test]
fn should_run_rerank_cadence_and_overrides() {
    // First rerank of a session always fires.
    assert!(should_run_rerank(0, None, 3, false));
    assert!(should_run_rerank(5, None, 3, false));

    // Topic change always fires, even mid-cadence.
    assert!(should_run_rerank(4, Some(3), 3, true));

    // Cadence floor: with cadence=3, must wait 3 turns since last rerank.
    assert!(!should_run_rerank(4, Some(3), 3, false)); // 1 turn since -> gated
    assert!(!should_run_rerank(5, Some(3), 3, false)); // 2 turns since -> gated
    assert!(should_run_rerank(6, Some(3), 3, false)); // 3 turns since -> fire
    assert!(should_run_rerank(10, Some(3), 3, false)); // well past -> fire

    // cadence <= 1 disables gating (every turn fires).
    assert!(should_run_rerank(4, Some(3), 1, false));
    assert!(should_run_rerank(4, Some(3), 0, false));
}

fn mem(content: &str) -> MemoryEntry {
    MemoryEntry::new(MemoryCategory::Fact, content)
}

#[test]
fn dynamic_gate_cuts_tail_at_score_gap() {
    // RRF-style descending scores with a sharp gap after the second item.
    let cands = vec![
        (mem("a"), 0.0163_f32),
        (mem("b"), 0.0161),
        (mem("c"), 0.0100), // big drop -> tail cut here
        (mem("d"), 0.0098),
        (mem("e"), 0.0097),
    ];
    let out = dynamic_gate_select(cands, 5);
    assert_eq!(out.len(), 2, "should keep only the two close-scoring items");
    assert_eq!(out[0].0.content, "a");
    assert_eq!(out[1].0.content, "b");
}

#[test]
fn dynamic_gate_keeps_top1_even_when_isolated() {
    // A lone strong candidate followed by far-weaker ones: keep exactly 1.
    let cands = vec![
        (mem("a"), 0.0200_f32),
        (mem("b"), 0.0100),
        (mem("c"), 0.0090),
    ];
    let out = dynamic_gate_select(cands, 5);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0.content, "a");
}

#[test]
fn dynamic_gate_respects_max_k_on_flat_scores() {
    // All scores ~equal: gate would keep all, but max_k caps the count.
    let cands: Vec<_> = (0..8)
        .map(|i| (mem(&format!("m{i}")), 0.0160_f32))
        .collect();
    let out = dynamic_gate_select(cands, 5);
    assert_eq!(out.len(), 5, "capped at max_k even when no gap appears");
}

#[test]
fn dynamic_gate_empty_input_returns_empty() {
    let out = dynamic_gate_select(Vec::new(), 5);
    assert!(out.is_empty());
}

// ---------------------------------------------------------------------------
// Regression: an unreachable judge must never destroy memory.
//
// 2026-08-04..15 the sidecar was pinned to a model id that no longer existed
// (`claude-haiku-4-5-20241022`), so every rerank 404'd. The rerank surfaced
// nothing, which put EVERY candidate into `rejected_ids`, which decayed them,
// which walked confidence under the 0.15 prune floor. The global graph went
// 2 -> 0 memories at 2026-08-10T09:35:34 with no error visible to the user.
// ---------------------------------------------------------------------------

#[test]
fn decay_targets_empty_when_judge_did_not_run() {
    let rejected = vec!["mem_a".to_string(), "mem_b".to_string()];
    assert!(
        decay_targets(false, &rejected).is_empty(),
        "judge failure must not be read as rejection"
    );
}

#[test]
fn decay_targets_pass_through_on_real_verdict() {
    let rejected = vec!["mem_a".to_string()];
    assert_eq!(
        decay_targets(true, &rejected),
        rejected.as_slice(),
        "a real verdict still decays what it rejected"
    );
}

#[test]
fn repeated_judge_failures_never_prune_the_store() {
    let _guard = crate::storage::lock_test_env();
    let old = std::env::var("JCODE_HOME").ok();
    let dir = std::env::temp_dir().join(format!(
        "jcode-judge-outage-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    crate::env::set_var("JCODE_HOME", &dir);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let manager =
            crate::memory::MemoryManager::new().with_project_dir("/tmp/jcode-judge-outage");

        // A memory old enough for the pruner to consider (age >= 24h).
        let mut entry = MemoryEntry::new(MemoryCategory::Fact, "load-bearing fact")
            .with_embedding(vec![1.0, 0.0]);
        entry.created_at = Utc::now() - chrono::Duration::hours(72);
        entry.confidence = 0.5;
        let id = manager.remember_project(entry).unwrap();

        // Replay the real outage: 67 consecutive turns where the judge never
        // answered, so the candidate lands in rejected_ids every time.
        let rejected = vec![id.clone()];
        for _ in 0..67 {
            let targets = decay_targets(false, &rejected);
            apply_confidence_updates(&manager, &[], targets);
        }

        let after = manager
            .load_project_graph()
            .unwrap()
            .get_memory(&id)
            .map(|e| e.confidence);
        assert_eq!(
            after,
            Some(0.5),
            "confidence must be untouched by 67 judge failures"
        );

        let (retired, deleted) = prune_low_confidence(&manager).unwrap();
        assert_eq!(
            (retired, deleted),
            (0, 0),
            "nothing may be retired or deleted after a judge outage"
        );
        assert!(
            manager
                .load_project_graph()
                .unwrap()
                .get_memory(&id)
                .is_some(),
            "the memory must survive the outage"
        );
    }));

    match old {
        Some(v) => crate::env::set_var("JCODE_HOME", v),
        None => crate::env::remove_var("JCODE_HOME"),
    }
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(p) = result {
        std::panic::resume_unwind(p);
    }
}

// ---------------------------------------------------------------------------
// A: garbage collection on effective_confidence (age half-life).
//
// Context for anyone changing these: the ADR-0021 fix (never decay without a
// judge verdict) removed the only force that moved raw confidence downward, so
// nothing was ever deleted again - 43 consecutive judge rejections were needed
// to drop one memory from 1.0 to the 0.15 floor. GC now runs on the per-category
// age half-life that already existed in `effective_confidence()`.
//
// Because this is the one path that deletes data the user never asked to
// remove, the tests below pin BOTH directions: stale things go, used things
// stay.
// ---------------------------------------------------------------------------

/// A memory created `age_days` ago and last touched `idle_days` ago.
///
/// Retention keys off idle time, so these two are deliberately separate: the
/// interesting cases are old-but-in-use vs old-and-abandoned.
fn aged_idle(
    category: MemoryCategory,
    age_days: i64,
    idle_days: i64,
    accesses: u32,
) -> MemoryEntry {
    let now = Utc::now();
    let mut e = MemoryEntry::new(category, "content for gc test");
    e.created_at = now - chrono::Duration::days(age_days);
    e.updated_at = now - chrono::Duration::days(idle_days);
    e.access_count = accesses;
    e
}

/// Old and untouched since creation.
fn aged(category: MemoryCategory, age_days: i64, accesses: u32) -> MemoryEntry {
    aged_idle(category, age_days, age_days, accesses)
}

#[test]
fn gc_keeps_everything_inside_the_age_floor() {
    let now = Utc::now();
    let mut fresh = MemoryEntry::new(MemoryCategory::Fact, "written minutes ago");
    fresh.created_at = now - chrono::Duration::hours(2);
    fresh.confidence = 0.0; // even at rock bottom
    assert_eq!(
        gc_action(&fresh, now),
        GcAction::Keep,
        "a burst of fresh extractions must never be collected before it can be used"
    );
}

#[test]
fn gc_retires_a_stale_unused_fact() {
    let now = Utc::now();
    // Facts have a 30-day half-life; 120 days is 4 half-lives => ~0.0625.
    let stale = aged(MemoryCategory::Fact, 120, 0);
    assert!(
        stale.effective_confidence_at(now) < PRUNE_CONFIDENCE_FLOOR,
        "sanity: a 120-day-old untouched fact should be under the floor"
    );
    assert_eq!(gc_action(&stale, now), GcAction::Retire);
}

#[test]
fn gc_keeps_a_used_memory_beside_an_expired_one() {
    // THE test for option A: age alone must not evict something in active use.
    //
    // This failed on the first implementation, which scored retention with
    // effective_confidence() (decay from created_at, plus an access-count
    // boost). Two same-age memories were both retired even when one had 400
    // accesses, because the boost is logarithmic (max ~2.1x) and cannot offset
    // exponential decay (0.03x at 150 days). Retention now decays from
    // updated_at, so recency - not the access tally - is what keeps a memory.
    let now = Utc::now();

    let abandoned = aged_idle(MemoryCategory::Fact, 150, 150, 0);
    let still_in_use = aged_idle(MemoryCategory::Fact, 150, 1, 400);

    assert_eq!(
        gc_action(&abandoned, now),
        GcAction::Retire,
        "same age, untouched for 150 days -> retire"
    );
    assert_eq!(
        gc_action(&still_in_use, now),
        GcAction::Keep,
        "same age, used yesterday -> must survive"
    );
}

#[test]
fn gc_never_expires_a_memory_that_keeps_being_used() {
    // A memory reinforced regularly must survive indefinitely. Decaying from
    // created_at would kill it on its birthday no matter how useful it is.
    let now = Utc::now();
    let ancient_but_active = aged_idle(MemoryCategory::Fact, 3650, 2, 50);
    assert_eq!(
        gc_action(&ancient_but_active, now),
        GcAction::Keep,
        "ten years old but used two days ago -> keep"
    );
}

#[test]
fn gc_respects_category_half_lives() {
    let now = Utc::now();
    // At 200 days: facts (30d) and entities (60d) are long gone, preferences
    // (90d) are borderline-dead, corrections (365d) must still be alive.
    assert_eq!(
        gc_action(&aged(MemoryCategory::Fact, 200, 0), now),
        GcAction::Retire
    );
    assert_eq!(
        gc_action(&aged(MemoryCategory::Entity, 200, 0), now),
        GcAction::Retire
    );
    assert_eq!(
        gc_action(&aged(MemoryCategory::Correction, 200, 0), now),
        GcAction::Keep,
        "user corrections are the highest-value category and must outlive the rest"
    );
}

#[test]
fn gc_retires_before_it_deletes() {
    let now = Utc::now();
    let mut stale = aged(MemoryCategory::Fact, 400, 0);

    // First sweep: still active -> retire, never delete outright.
    assert_eq!(gc_action(&stale, now), GcAction::Retire);

    // Retired just now: inside the grace window, so it stays on disk.
    stale.active = false;
    stale.updated_at = now;
    assert_eq!(
        gc_action(&stale, now),
        GcAction::Keep,
        "a freshly retired memory must be recoverable, not deleted"
    );

    // Retired and untouched past the grace window: now it can go.
    stale.updated_at = now - chrono::Duration::days(RETIRED_GRACE_DAYS + 1);
    assert_eq!(gc_action(&stale, now), GcAction::Delete);
}

#[test]
fn gc_reinforcement_rescues_a_retired_memory() {
    // Retirement is reversible: touching a retired entry resets its clock, so
    // the next sweep leaves it alone instead of deleting it.
    let now = Utc::now();
    let mut retired = aged(MemoryCategory::Fact, 400, 0);
    retired.active = false;
    retired.updated_at = now - chrono::Duration::days(RETIRED_GRACE_DAYS + 5);
    assert_eq!(gc_action(&retired, now), GcAction::Delete);

    retired.reinforce("session_rescue", 0);
    assert_eq!(
        gc_action(&retired, now),
        GcAction::Keep,
        "reinforcement must reset the deletion clock"
    );
}

#[test]
fn gc_sweep_retires_and_deletes_on_disk() {
    let _guard = crate::storage::lock_test_env();
    let old = std::env::var("JCODE_HOME").ok();
    let dir = std::env::temp_dir().join(format!(
        "jcode-gc-sweep-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    crate::env::set_var("JCODE_HOME", &dir);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let manager = crate::memory::MemoryManager::new().with_project_dir("/tmp/jcode-gc-sweep");
        let now = Utc::now();

        // Old but used yesterday: must survive the sweep.
        let mut keep = aged_idle(MemoryCategory::Fact, 150, 1, 400);
        keep.embedding = Some(vec![1.0, 0.0]);
        let keep_id = manager.remember_project(keep).unwrap();

        let mut retire = aged(MemoryCategory::Fact, 150, 0);
        retire.embedding = Some(vec![0.0, 1.0]);
        let retire_id = manager.remember_project(retire).unwrap();

        let mut doomed = aged(MemoryCategory::Fact, 400, 0);
        doomed.embedding = Some(vec![0.5, 0.5]);
        doomed.active = false;
        doomed.updated_at = now - chrono::Duration::days(RETIRED_GRACE_DAYS + 1);
        let doomed_id = manager.remember_project(doomed).unwrap();

        let (retired, deleted) = prune_low_confidence(&manager).unwrap();
        assert_eq!(retired, 1, "exactly the stale unused fact is retired");
        assert_eq!(deleted, 1, "exactly the long-retired memory is deleted");

        let graph = manager.load_project_graph().unwrap();
        assert!(
            graph.get_memory(&keep_id).is_some_and(|e| e.active),
            "the used memory stays active"
        );
        assert!(
            graph.get_memory(&retire_id).is_some_and(|e| !e.active),
            "the stale memory is retired but still on disk (recoverable)"
        );
        assert!(
            graph.get_memory(&doomed_id).is_none(),
            "the long-retired memory is gone"
        );
    }));

    match old {
        Some(v) => crate::env::set_var("JCODE_HOME", v),
        None => crate::env::remove_var("JCODE_HOME"),
    }
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(p) = result {
        std::panic::resume_unwind(p);
    }
}

// ---------------------------------------------------------------------------
// C: both write paths share one gate.
//
// Session-end extraction used to build entries and call remember_project
// directly - no similarity check, no reinforcement, no contradiction check -
// while the mid-session path had all three. That put the weakest validation on
// the highest-volume producer (one real session ended with a 13-memory burst).
//
// These tests run ingest_extracted_memories() with an OFFLINE sidecar, which is
// the honest configuration for a unit test: the contradiction check needs an
// LLM and correctly returns Err without one, so the dedup/reinforce behaviour
// below is exactly what runs when the judge is unreachable.
// ---------------------------------------------------------------------------

fn extracted(content: &str) -> crate::sidecar::ExtractedMemory {
    crate::sidecar::ExtractedMemory {
        category: "fact".to_string(),
        content: content.to_string(),
        trust: "medium".to_string(),
    }
}

/// Run `f` with an isolated JCODE_HOME **and real embeddings enabled**.
///
/// Embeddings are off by default under `cfg(test)`
/// (`should_generate_embedding_for_entry`), which silently makes
/// `find_similar` return nothing - so a dedup test written without this opt-in
/// passes or fails for reasons that have nothing to do with dedup. The first
/// version of these tests did exactly that and reported "no duplicate written"
/// as a code bug when the real cause was an unembedded fixture.
fn with_memory_home<T>(tag: &str, f: impl FnOnce() -> T) -> T {
    let _guard = crate::storage::lock_test_env();
    let old = std::env::var("JCODE_HOME").ok();
    let old_emb = std::env::var("JCODE_TEST_ALLOW_MEMORY_EMBEDDINGS").ok();
    crate::env::set_var("JCODE_TEST_ALLOW_MEMORY_EMBEDDINGS", "1");
    let dir = std::env::temp_dir().join(format!(
        "jcode-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    crate::env::set_var("JCODE_HOME", &dir);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));

    match old {
        Some(v) => crate::env::set_var("JCODE_HOME", v),
        None => crate::env::remove_var("JCODE_HOME"),
    }
    match old_emb {
        Some(v) => crate::env::set_var("JCODE_TEST_ALLOW_MEMORY_EMBEDDINGS", v),
        None => crate::env::remove_var("JCODE_TEST_ALLOW_MEMORY_EMBEDDINGS"),
    }
    let _ = std::fs::remove_dir_all(&dir);
    match result {
        Ok(v) => v,
        Err(p) => std::panic::resume_unwind(p),
    }
}

#[test]
fn session_end_reinforces_instead_of_duplicating() {
    with_memory_home("ingest-dedup", || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let manager =
                crate::memory::MemoryManager::new().with_project_dir("/tmp/jcode-ingest-dedup");
            let sidecar = crate::sidecar::Sidecar::new();

            let fact = "Production frontend logs to /var/log/nginx/clients_error.log, not clientes_error.log.";
            let batch = vec![extracted(fact)];

            // First session ends: the memory is new.
            let first =
                ingest_extracted_memories(&manager, &sidecar, &batch, "session_one").await;
            assert_eq!(first.stored, 1, "first sighting is stored");
            assert_eq!(first.reinforced, 0);

            // Second session ends with the SAME fact. Before option C this
            // stored a duplicate; now it must reinforce.
            let second =
                ingest_extracted_memories(&manager, &sidecar, &batch, "session_two").await;
            assert_eq!(second.stored, 0, "no duplicate is written");
            assert_eq!(second.reinforced, 1, "the existing memory is reinforced");

            let all = manager.list_all().unwrap();
            assert_eq!(all.len(), 1, "still exactly one memory on disk");
            let entry = &all[0];
            assert!(
                entry.strength >= 2,
                "reinforcement must raise strength (got {})",
                entry.strength
            );
            assert!(
                !entry.reinforcements.is_empty(),
                "reinforcement must be recorded for provenance"
            );
        });
    });
}

#[test]
fn reinforcement_resets_the_gc_clock() {
    // C feeding A: reinforcement touches updated_at, which is what retention
    // decays from, so a repeatedly-seen memory can never age out.
    with_memory_home("ingest-gc", || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let manager =
                crate::memory::MemoryManager::new().with_project_dir("/tmp/jcode-ingest-gc");
            let sidecar = crate::sidecar::Sidecar::new();

            let fact = "Kumo delist block is 17 hardcoded IPs in config_msft_delist.yaml, not data-driven.";
            let id = ingest_extracted_memories(&manager, &sidecar, &[extracted(fact)], "s1")
                .await
                .stored_ids
                .first()
                .cloned()
                .expect("stored");

            // Backdate it so it is well past the retention floor.
            {
                let mut graph = manager.load_project_graph().unwrap();
                let entry = graph.get_memory_mut(&id).unwrap();
                entry.created_at = Utc::now() - chrono::Duration::days(200);
                entry.updated_at = entry.created_at;
                manager.save_project_graph(&graph).unwrap();
            }

            let now = Utc::now();
            {
                let graph = manager.load_project_graph().unwrap();
                assert_eq!(
                    gc_action(graph.get_memory(&id).unwrap(), now),
                    GcAction::Retire,
                    "an abandoned 200-day-old fact is due for retirement"
                );
            }

            // Seeing the same fact again must rescue it.
            let again = ingest_extracted_memories(&manager, &sidecar, &[extracted(fact)], "s2").await;
            assert_eq!(again.reinforced, 1);

            let graph = manager.load_project_graph().unwrap();
            assert_eq!(
                gc_action(graph.get_memory(&id).unwrap(), now),
                GcAction::Keep,
                "reinforcing a stale memory must save it from the next sweep"
            );
        });
    });
}

#[test]
fn ingest_stores_distinct_facts_separately() {
    // Guard the other direction: dedup must not swallow genuinely different
    // memories, which would silently lose knowledge.
    with_memory_home("ingest-distinct", || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let manager =
                crate::memory::MemoryManager::new().with_project_dir("/tmp/jcode-ingest-distinct");
            let sidecar = crate::sidecar::Sidecar::new();

            let batch = vec![
                extracted("Kumo delist block is 17 hardcoded IPs in config_msft_delist.yaml."),
                extracted("User wants a 48-hour cooldown per IP and provider, not 24 hours."),
                extracted("boto3 reads AWS credentials from $HOME/.aws/credentials as www-data."),
            ];
            let out = ingest_extracted_memories(&manager, &sidecar, &batch, "session_mixed").await;
            assert_eq!(out.stored, 3, "three unrelated facts must all be kept");
            assert_eq!(out.reinforced, 0);
            assert_eq!(manager.list_all().unwrap().len(), 3);
        });
    });
}
