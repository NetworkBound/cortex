pub mod agents;
pub mod agui;
pub mod app_state;
pub mod brain;
mod commands;
pub mod connectivity;
pub mod git;
pub mod gateway;
pub mod infra_config;
pub mod lanes;
pub mod mcp;
pub mod hooks;
pub mod redact;
pub mod memory;
pub mod mobile_server;
pub mod monitors;
pub mod observability;
pub mod orchestrator;
pub mod preview;
pub mod pricing;
pub mod projects;
pub mod prp;
pub mod repo_map;
pub mod retrieval;
pub mod skills;
pub mod sys;
pub mod terminal;
pub mod usage;
pub mod watch_mode;
pub mod websearch;
pub mod worktrees;

use crate::agents::gateway_remote::GatewayRemoteAgent;
use crate::agents::local_cli::GenericCliAgent;
use crate::agents::ollama::OllamaAgent;
use crate::app_state::AppState;
use crate::observability::tracing_store::TracingStore;
use std::sync::Arc;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub fn run() {
    init_tracing();

    AppState::seed_keychain_if_empty();
    crate::agents::roles::seed_defaults_if_missing();
    crate::commands::workflows::seed_default_workflows();

    let state = AppState::new();
    // Ordered failover candidates (LAN → tailnet → public tunnel). The first
    // entry is what we point at synchronously below, so normal startup is
    // unchanged; a background task (spawned in `.setup`) keeps the live value
    // pointed at the most-preferred *reachable* candidate.
    let gateway_candidates = AppState::gateway_base_url_candidates();
    {
        let mut cfg = AppState::load_config_from_env();
        // Pin the immediate base URL to the first candidate so it agrees with
        // the failover list's preference order (no network I/O on startup).
        if let Some(first) = gateway_candidates.first() {
            cfg.gateway_base_url = first.clone();
        }
        *state.config.write() = cfg;
    }

    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    {
        let cfg = state.config.read();
        let mut reg = state.registry.write();

        // Standalone (no-homelab) build variant: when compiled with
        // `--features standalone` AND launched in cloud mode (persisted via
        // Settings → Providers to ~/.cortex/runtime-mode.json, or the legacy
        // CORTEX_RUNTIME_MODE=cloud env var), register the direct provider
        // adapters and SKIP the gateway — the app talks to providers directly. The
        // default build never compiles this block, so it stays bit-identical
        // to today. Registration happens once here, so switching modes in
        // Settings takes effect on the next launch.
        #[cfg(feature = "standalone")]
        let cloud_mode = cfg.runtime_mode == "cloud";
        #[cfg(not(feature = "standalone"))]
        let cloud_mode = false;

        #[cfg(feature = "standalone")]
        if cloud_mode {
            reg.register(Arc::new(crate::agents::AnthropicDirectAgent::new()));
            reg.register(Arc::new(crate::agents::OpenAIDirectAgent::new()));
        }

        // The gateway is the default orchestrator. In cloud mode we do NOT register
        // it — the user signs in to providers directly.
        if !cloud_mode {
            reg.register(Arc::new(GatewayRemoteAgent::new(
                cfg.gateway_base_url.clone(),
                api_key,
                cfg.gateway_model.clone(),
            )));
        }
        // Register every local AI-maker CLI from the data-driven catalog
        // (`agents::ALL_CLI_SPECS`): Claude, OpenAI Codex, Gemini, Qwen Code,
        // Grok Build, aider, Mistral Vibe. Each becomes a `GenericCliAgent`
        // whose `available` flag reflects whether its binary is resolvable, so
        // routing / the picker / cost_router skip a CLI that isn't installed.
        // The framework is in the DEFAULT build (no `standalone` gate) — auth is
        // each CLI's own login, fully local, no homelab. Adding a new maker's
        // CLI is just a new static CliSpec + a row in ALL_CLI_SPECS.
        for spec in crate::agents::ALL_CLI_SPECS {
            reg.register(Arc::new(GenericCliAgent::new(spec)));
        }
        // Local Ollama adapter — streams directly from the configured Ollama
        // server, bypassing the gateway. `available` reflects a non-empty URL.
        reg.register(Arc::new(OllamaAgent::new(
            cfg.ollama_base_url.clone(),
            cfg.ollama_model.clone(),
        )));
        // Group A — one generic OpenAI-compatible adapter per per-token API
        // provider (Groq, Together, Fireworks, DeepSeek, Mistral, xAI, …). Each
        // reports `available` from whether its key is present in the KeyVault /
        // env, so routing skips a provider the user hasn't configured. Always-on
        // (no homelab / standalone gate): the framework is data-driven and the
        // adapters are inert without a key. Chat-only, like the direct adapters.
        for spec in crate::agents::PROVIDERS {
            reg.register(Arc::new(crate::agents::OpenAiCompatAgent::new(spec)));
        }
        // Group C — local-runtime endpoint adapters (LM Studio, vLLM, llama.cpp,
        // TabbyAPI, Text-Gen-WebUI). Each probes its localhost port at
        // health-check / run time; an unreachable runtime is simply skipped.
        // Free/local, optional dummy key. TabbyAPI & TGWebUI share :5000 — the
        // listening one answers, the other reports unhealthy. Chat-only.
        for spec in crate::agents::RUNTIMES {
            reg.register(Arc::new(crate::agents::LocalRuntimeAgent::new(spec)));
        }
        // E2E only: deterministic fake-LLM adapter the probe drives through
        // the REAL chat_send pipeline (focus-chain flow). Never registered in
        // a normal launch.
        if crate::commands::e2e::e2e_enabled() {
            reg.register(Arc::new(crate::agents::E2eFakeAgent::new()));
        }
    }

    let tracing_store = TracingStore::open_default().unwrap_or_else(|e| {
        tracing::warn!("tracing store init failed: {e}; using in-memory");
        TracingStore::in_memory()
    });

    // Lanes whose watcher died with the previous process would show "running"
    // forever — stamp them interrupted before anything reads the table.
    if let Ok(n) = crate::lanes::LaneStore::new(tracing_store.shared_connection())
        .mark_stale_interrupted()
    {
        if n > 0 {
            tracing::info!("marked {n} stale lane run(s) interrupted from a previous session");
        }
    }

    crate::observability::crash::install_panic_hook(
        tracing_store.shared_connection(),
        env!("CARGO_PKG_VERSION").to_string(),
    );

    let _ = crate::observability::audit::prune_old(90);

    // Shared handle to the live config for the gateway failover loop, captured
    // before `state` is moved into `.manage`.
    let failover_config = state.config.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    use tauri_plugin_global_shortcut::ShortcutState;
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    if shortcut_matches_omnibar(shortcut) {
                        toggle_omnibar(app);
                    }
                })
                .build(),
        )
        .manage(state)
        .manage(tracing_store)
        .invoke_handler(tauri::generate_handler![
            commands::chat::chat_send,
            commands::chat::approve_run,
            commands::chat::stop_run,
            commands::chat::set_current_mode,
            commands::approvals::add_approval_rule,
            commands::approvals::list_auto_approve,
            commands::approvals::add_auto_approve,
            commands::approvals::remove_auto_approve,
            commands::approvals::get_approval_policy,
            commands::approvals::set_approval_policy,
            commands::agents::list_agents,
            commands::agents::check_agent_health,
            commands::agents::list_local_cli_providers,
            commands::agents::cli_provider_login,
            commands::memory::list_memory_files,
            commands::memory::list_memory_sources,
            commands::memory::get_memory_entry,
            commands::memory::write_memory_entry,
            commands::memory::create_memory_entry,
            commands::memory::search_memory,
            commands::chat_history::list_claude_chats,
            commands::chat_history::get_claude_chat,
            commands::chat_history::search_claude_chats,
            commands::chat_meta::get_chat_meta,
            commands::chat_meta::set_chat_meta,
            commands::chat_meta::list_chat_meta,
            commands::memory_bridge::import_claude_mem,
            commands::settings::get_gateway_config,
            commands::settings::set_gateway_api_key,
            commands::settings::update_gateway_config,
            commands::settings::get_provider_config,
            commands::settings::set_provider_key,
            commands::settings::validate_provider_key,
            commands::settings::set_provider_default_model,
            commands::settings::set_runtime_mode,
            commands::observability::recent_traces,
            commands::observability::trace_events,
            commands::observability::homelab_health,
            commands::observability::recent_issues,
            commands::observability::recent_audit,
            commands::observability::search_sessions,
            commands::crash::recent_crashes,
            commands::crash::record_js_crash,
            commands::diagnostics::export_diagnostics,
            commands::projects::list_projects,
            commands::projects::open_vault_note,
            commands::projects::set_active_project,
            commands::projects::project_files,
            commands::projects::list_rules,
            commands::projects::cortexignore_status,
            commands::search::search_project,
            commands::search::find_files,
            commands::prp::list_prps,
            commands::prp::get_prp,
            commands::prp::create_prp,
            commands::prp::advance_prp_stage,
            commands::prp::run_prp_gates,
            commands::prp::prp_progress,
            commands::project_doc::agents_md_stack,
            commands::project_doc::agents_md_merged,
            commands::profiles::list_profiles,
            commands::profiles::apply_profile,
            commands::profiles::apply_profile_v2,
            commands::profiles::get_agent_instructions,
            commands::profiles::set_agent_instructions,
            commands::profiles::approve_plan,
            commands::roles::list_roles,
            commands::roles::get_role,
            commands::roles::set_role,
            commands::roles::delete_role,
            commands::roles::apply_role_to_agent,
            commands::brain::brain_snapshot,
            commands::brain::set_obsidian_vault,
            commands::ui_prefs::read_ui_prefs,
            commands::ui_prefs::write_ui_prefs,
            commands::brain_import::import_to_brain,
            commands::brain_toc::brain_toc,
            commands::memory_dedupe::find_duplicate_memory,
            commands::knowledge_graph::build_knowledge_graph,
            commands::dep_graph::build_dep_graph,
            commands::memory_stats::memory_stats,
            commands::memory_stats::sync_memory,
            commands::sessions::load_session_messages,
            commands::sessions::record_message,
            commands::sessions::bootstrap_project_session,
            commands::session_summary::summarize_session,
            commands::cost_tracker::cost_estimate,
            commands::usage::usage_summary,
            commands::usage::gateway_status,
            commands::account_usage::account_usage,
            commands::worktrees::list_worktrees,
            commands::worktrees::create_worktree,
            commands::worktrees::remove_worktree,
            commands::worktrees::assign_worktree_session,
            commands::repo_map::repo_map,
            commands::repo_map::repo_map_text,
            commands::repo_map::repo_symbols,
            commands::repo_watcher::start_repo_watcher,
            commands::repo_watcher::stop_repo_watcher,
            commands::repo_watcher::repo_watcher_status,
            commands::repo_watcher::repo_watcher_reset,
            commands::agui::start_agui_server,
            commands::agui::stop_agui_server,
            commands::watch_mode::start_watch_mode,
            commands::watch_mode::stop_watch_mode,
            commands::watch_mode::is_watch_mode_active,
            commands::monitors::start_monitors,
            commands::monitors::stop_monitors,
            commands::monitors::list_monitors,
            commands::updater::check_updates,
            commands::selfupdate::check_release_update,
            commands::selfupdate::apply_release_update,
            commands::selfupdate::relaunch_app,
            commands::windows::open_secondary_window,
            commands::workspace::export_workspace,
            commands::workspace::import_workspace,
            commands::workspace_presets::list_workspace_presets,
            commands::workspace_presets::save_workspace_preset,
            commands::workspace_presets::delete_workspace_preset,
            commands::checkpoints::create_checkpoint,
            commands::checkpoints::list_checkpoints,
            commands::checkpoints::restore_checkpoint,
            commands::checkpoints::restore_last_checkpoint,
            commands::checkpoints::diff_checkpoint,
            commands::checkpoints::delete_checkpoint,
            commands::checkpoints::prune_checkpoints,
            commands::sandbox::get_sandbox_tier,
            commands::sandbox::set_sandbox_tier,
            commands::sandbox::classify_shell_command,
            commands::trust::get_trust_status,
            commands::trust::trust_project,
            commands::trust::untrust_project,
            commands::trust::get_trust_matrix,
            commands::trust::set_trust_matrix,
            commands::focus_chain::load_focus_chain,
            commands::focus_chain::save_focus_chain,
            commands::focus_chain::clear_focus_chain,
            commands::context::estimate_context_breakdown,
            commands::condense::condense_history,
            commands::context::fetch_url,
            commands::context::git_working_diff,
            commands::context::project_diagnostics,
            commands::context::recent_terminal_output,
            commands::context_picker::suggest_context,
            commands::hooks::list_hooks,
            commands::snippets::list_snippets,
            commands::snippets::get_snippet,
            commands::snippets::save_snippet,
            commands::snippets::delete_snippet,
            commands::skills::list_skills,
            commands::skills::get_skill,
            commands::skills::expand_skill,
            commands::skills::save_skill,
            commands::snapshots::create_snapshot,
            commands::snapshots::list_snapshots,
            commands::snapshots::rollback_snapshot,
            commands::snapshots::delete_snapshot,
            commands::snapshots::prune_snapshots,
            commands::teams::list_teams,
            commands::teams::get_team,
            commands::teams::create_team,
            commands::teams::update_team_worker,
            commands::teams::run_team,
            commands::teams::delete_team,
            commands::ultimate::ultimate_chat_run,
            commands::ultimate::ultimate_list_models,
            commands::themes::list_themes,
            commands::themes::get_active_theme,
            commands::themes::set_active_theme,
            commands::themes::set_bg_image,
            commands::shell_run::shell_exec,
            commands::terminal::terminal_open,
            commands::terminal::terminal_write,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_close,
            commands::terminal::terminal_list_active,
            commands::notify::desktop_notify,
            commands::ide_export::export_ide_configs,
            commands::inline_completion::inline_complete,
            commands::inline_assist::inline_assist,
            commands::edit_predictor::predict_next_edit,
            commands::commit_suggest::suggest_commit_message,
            commands::ask_router::ask_router,
            commands::smart_stage::smart_stage,
            commands::share::share_chat_as_markdown,
            commands::keyvault::vault_list,
            commands::keyvault::vault_get,
            commands::keyvault::vault_set,
            commands::keyvault::vault_remove,
            commands::webhooks::list_webhooks,
            commands::webhooks::add_webhook,
            commands::webhooks::update_webhook,
            commands::webhooks::delete_webhook,
            commands::webhooks::test_webhook,
            commands::webhooks::fire_event,
            commands::git::git_history,
            commands::git::git_show,
            commands::git::git_commit_files,
            commands::git::git_commit_file_diff,
            commands::git::git_working_status,
            commands::git::git_stage_file,
            commands::git::git_unstage_file,
            commands::git::git_discard_changes,
            commands::git::git_commit,
            commands::git::git_file_diff,
            commands::git_push::git_commit_staged,
            commands::git_push::git_push,
            commands::git_pull::git_pull,
            commands::git_setup::validate_git_url,
            commands::git_setup::clone_git_repo,
            commands::git_setup::validate_obsidian_vault,
            commands::git_setup::set_git_server_url,
            commands::git_setup::set_git_server_cloned_path,
            commands::git_stash::git_stash_list,
            commands::git_stash::git_stash_apply,
            commands::git_stash::git_stash_pop,
            commands::git_stash::git_stash_drop,
            commands::git_stash::git_stash_save,
            commands::git_stash::git_stash_show,
            commands::config_files::read_config_file,
            commands::config_files::write_config_file,
            commands::config_watcher::stop_config_watcher,
            commands::config_watcher::config_watcher_status,
            commands::threads::list_threads,
            commands::threads::save_thread,
            commands::threads::delete_thread,
            commands::preview::list_dev_servers,
            commands::preview::start_preview_watcher,
            commands::preview::stop_preview_watcher,
            commands::editor::save_file_text,
            commands::editor::read_file_text,
            commands::apply_edits::apply_edit_blocks,
            commands::auto_test::get_test_command,
            commands::auto_test::set_test_command,
            commands::auto_test::run_test_command,
            commands::model_roles::get_model_roles,
            commands::model_roles::set_model_roles,
            commands::lint::get_lint_command,
            commands::lint::detect_lint,
            commands::lint::set_lint_command,
            commands::lint::run_lint,
            commands::manifest::get_manifest,
            commands::manifest::add_to_manifest,
            commands::manifest::drop_from_manifest,
            commands::microagents::list_microagents,
            commands::chatgpt_import::import_chatgpt_export,
            commands::local_brain::local_brain_suggest,
            commands::local_brain::extract_terms_diagnostic,
            commands::local_brain::compute_repo_map_command,
            commands::local_brain::repo_map_cache_stats,
            commands::local_brain::repo_map_cache_clear,
            commands::fragments::list_fragments,
            commands::fragments::save_fragment,
            commands::voice::voice_transcribe,
            commands::tools::list_tools,
            commands::tools::get_tool,
            commands::tools::save_tool,
            commands::tools::delete_tool,
            commands::tools::invoke_tool,
            commands::tools::test_tool,
            commands::gateway_capabilities::gateway_capabilities,
            commands::gateway_capabilities::list_gateway_models,
            commands::models::list_models,
            commands::cookbook::cookbook_host_specs,
            commands::cookbook::cookbook_recommendations,
            commands::cookbook::cookbook_pull_model,
            commands::cookbook::cookbook_active_pulls,
            commands::deep_research::deep_research,
            commands::deep_research::deep_research_active,
            commands::deep_research::list_research_reports,
            commands::deep_research::read_research_report,
            commands::routines::list_routines,
            commands::routines::save_routine,
            commands::routines::delete_routine,
            commands::routines::set_routine_enabled,
            commands::routines::run_routine_now,
            commands::routines::list_routine_runs,
            commands::routines::routine_run_as_session,
            commands::eval_harness::list_eval_tasks,
            commands::eval_harness::list_eval_reports,
            commands::eval_harness::run_eval,
            commands::eval_harness::eval_active,
            commands::multi_provider::run_provider_lanes,
            commands::multi_provider::list_lane_runs,
            commands::multi_provider::stop_lane_run,
            commands::multi_provider::delete_lane_run,
            commands::multi_provider::reattach_lane_run,
            commands::lane_review::lane_review,
            commands::lane_review::merge_lane_run,
            commands::semantic_search::semantic_memory_search,
            commands::workflows::list_workflows,
            commands::workflows::get_workflow,
            commands::workflows::save_workflow,
            commands::workflows::delete_workflow,
            commands::workflows::run_workflow,
            commands::backup::create_backup,
            commands::backup::list_backups,
            commands::backup::restore_backup,
            commands::backup::delete_backup,
            commands::spaces::list_spaces,
            commands::spaces::save_space,
            commands::spaces::delete_space,
            commands::spaces::space_files,
            commands::test_runner::run_tests,
            commands::test_gen::generate_tests,
            commands::refactor_suggester::suggest_refactors,
            commands::doc_gen::generate_docs,
            commands::explain::explain_code,
            commands::explain::save_explanation,
            commands::arch_diagram::generate_arch_diagram,
            commands::retrieve::retrieve,
            commands::rerank::rerank,
            commands::custom_slashes::list_custom_slashes,
            commands::custom_slashes::save_custom_slash,
            commands::custom_slashes::delete_custom_slash,
            commands::changelog::generate_changelog,
            commands::project_metrics::project_metrics,
            commands::project_doc_gen::generate_project_doc,
            commands::bookmarks::list_bookmarks,
            commands::bookmarks::add_bookmark,
            commands::bookmarks::update_bookmark,
            commands::bookmarks::delete_bookmark,
            commands::bookmarks::touch_bookmark,
            commands::ai_debugger::debug_error,
            commands::conflict_resolver::resolve_conflicts,
            commands::conflict_resolver::stage_resolved_files,
            commands::dep_audit::audit_deps,
            commands::duck::duck_question,
            commands::duck::save_duck_transcript,
            commands::daily_journal::daily_journal,
            commands::daily_journal::save_journal,
            commands::gitea_backup::gitea_get_settings,
            commands::gitea_backup::gitea_set_settings,
            commands::gitea_backup::gitea_backup,
            commands::gitea_backup::gitea_backup_now,
            commands::recipe_gallery::list_recipes,
            commands::recipe_gallery::get_recipe,
            commands::recipe_gallery::save_recipe,
            commands::recipe_gallery::delete_recipe,
            commands::recipe_gallery::install_recipe_from_url,
            commands::model_arena::arena_send,
            commands::model_arena::arena_vote,
            commands::model_arena::arena_leaderboard,
            commands::batch_runner::batch_run,
            commands::manager_process::manager_decompose,
            commands::manager_process::manager_run_step,
            commands::manager_process::manager_validate,
            commands::channels::list_channels,
            commands::channels::get_channel,
            commands::channels::create_channel,
            commands::channels::delete_channel,
            commands::channels::post_message,
            commands::mcp::mcp_list_servers,
            commands::mcp::mcp_save_server,
            commands::mcp::mcp_delete_server,
            commands::mcp::mcp_connect,
            commands::mcp::mcp_disconnect,
            commands::mcp::mcp_call_tool,
            commands::e2e::e2e_config,
            commands::e2e::e2e_write_snapshot,
            commands::e2e::e2e_make_clone_fixture,
            commands::e2e::e2e_cleanup_clone_fixture,
            commands::e2e::e2e_make_history_fixture,
            commands::e2e::e2e_cleanup_history_fixture,
            commands::e2e::e2e_delete_session,
        ])
        .setup(move |app| {
            tracing::info!("cortex started");
            if let Some(win) = tauri::Manager::get_webview_window(app, "main") {
                // Clear cached webview data ONLY when the app version changed
                // (i.e. right after an auto-update, to drop stale bundled
                // assets). Doing this on every launch — as it did originally —
                // also wiped localStorage, silently resetting every client-side
                // preference each start: the onboarding flag, sidebar/nav
                // widths, the active theme, prompt history. Gating on version
                // keeps the post-update cache-bust while letting prefs persist.
                if webview_data_should_clear() {
                    tracing::info!("app version changed — clearing stale webview data");
                    let _ = win.clear_all_browsing_data();
                }
            }
            // Blank-window watchdog: the main window ships hidden
            // (`visible: false`) and the frontend calls `.show()` after its
            // first paint to kill the WebView2 blank-gray flash. If that signal
            // never arrives (load failure / hung IPC), reveal it anyway after a
            // short grace period so the app can never get stuck invisible.
            {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                    if let Some(win) =
                        tauri::Manager::get_webview_window(&app_handle, "main")
                    {
                        if !win.is_visible().unwrap_or(true) {
                            let _ = win.show();
                        }
                    }
                });
            }
            crate::observability::homelab::spawn_pollers(app.handle().clone());
            register_omnibar_shortcut(app.handle());
            // Kick off the localhost dev-server preview sniffer so the
            // preview tab is populated as soon as the user opens it.
            if let Err(e) = crate::preview::start(app.handle().clone()) {
                tracing::warn!("preview watcher failed to start: {e:#}");
            }
            // Hot-reload watcher for ~/.cortex/*.json. Emits `config-changed`
            // window events that future panels (snippets, trust matrix, …)
            // can subscribe to for cache invalidation.
            if let Err(e) = crate::commands::config_watcher::start(app.handle().clone()) {
                tracing::warn!("config watcher failed to start: {e:#}");
            }
            // Periodic Gitea backup loop. No-op until the user enables it
            // in the GiteaBackupPanel — reads settings each tick so the
            // toggle takes effect without a restart.
            crate::commands::gitea_backup::spawn_scheduler(app.handle().clone());
            // Scheduled agents ("Routines"): ticks every 30s, runs due
            // routines through the gateway, records each run. No-op until the
            // user creates + enables a routine in the RoutinesPanel.
            crate::commands::routines::spawn_scheduler(app.handle().clone());
            // Embedded mobile HTTP/WebSocket server. Serves the mobile web
            // client and bridges chat / ultimate / projects / models to the
            // same backend the desktop app uses. Loopback-only (127.0.0.1) on
            // port 8788 (override: CORTEX_MOBILE_PORT); meant to sit behind
            // `tailscale serve`. Spawned onto Tauri's tokio runtime; a bind
            // failure (e.g. port clash) is logged and never blocks startup.
            {
                use tauri::Manager;
                let app_state = app.state::<AppState>().inner().clone();
                let store = app.state::<TracingStore>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = crate::mobile_server::spawn(app_state, store).await {
                        tracing::warn!("mobile server failed to start: {e:#}");
                    }
                });
            }
            // Resilient gateway endpoint failover. Best-effort, time-boxed
            // probes; never blocks startup. Runs an immediate resolve then
            // re-checks every ~60s, switching `Config.gateway_base_url` to the
            // most-preferred reachable candidate (LAN → tailnet → tunnel).
            {
                let candidates = gateway_candidates.clone();
                let cfg = failover_config.clone();
                tauri::async_runtime::spawn(async move {
                    crate::connectivity::run_failover_loop(candidates, cfg).await;
                });
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // Reap monitor child processes on shutdown so we don't leak
            // long-running tails or `npm test --watch` workers when the
            // window is closed.
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                // `stop_all` is async — block on a tiny ad-hoc runtime so we
                // don't depend on an outer tokio context being live here.
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                if let Ok(rt) = rt {
                    rt.block_on(crate::monitors::stop_all());
                }
            }
        });
}

/// True when the running app version differs from the one recorded at the last
/// webview-data clear (or on first launch). Used to limit `clear_all_browsing_data`
/// to post-update launches instead of every start, so client-side prefs survive.
fn webview_data_should_clear() -> bool {
    use std::fs;
    let current = env!("CARGO_PKG_VERSION");
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let path = home.join(".cortex").join("webview-version");
    let changed = fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .as_deref()
        != Some(current);
    if changed {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, current);
    }
    changed
}

/// Build the minimal [`AppState`] + [`TracingStore`] needed to run Cortex's
/// backend WITHOUT a GUI — used by the `cortex-serve` headless binary so the
/// embedded mobile server can run on a server/VM. Mirrors the registry
/// population in [`run`] (gateway adapter + every catalog CLI/provider/runtime/
/// Ollama adapter), minus the Tauri window/plugin/setup machinery.
///
/// NOTE on parity: this duplicates the adapter-registration block from [`run`]
/// rather than sharing it, because that block lives inline inside the GUI setup
/// path. If the registration list in [`run`] changes, update it here too.
pub fn build_headless_state() -> (AppState, TracingStore) {
    AppState::seed_keychain_if_empty();

    let state = AppState::new();
    let gateway_candidates = AppState::gateway_base_url_candidates();
    {
        let mut cfg = AppState::load_config_from_env();
        if let Some(first) = gateway_candidates.first() {
            cfg.gateway_base_url = first.clone();
        }
        *state.config.write() = cfg;
    }

    let api_key = AppState::get_gateway_api_key().unwrap_or_default();
    {
        let cfg = state.config.read();
        let mut reg = state.registry.write();

        #[cfg(feature = "standalone")]
        let cloud_mode = cfg.runtime_mode == "cloud";
        #[cfg(not(feature = "standalone"))]
        let cloud_mode = false;

        #[cfg(feature = "standalone")]
        if cloud_mode {
            reg.register(Arc::new(crate::agents::AnthropicDirectAgent::new()));
            reg.register(Arc::new(crate::agents::OpenAIDirectAgent::new()));
        }

        if !cloud_mode {
            reg.register(Arc::new(GatewayRemoteAgent::new(
                cfg.gateway_base_url.clone(),
                api_key,
                cfg.gateway_model.clone(),
            )));
        }
        for spec in crate::agents::ALL_CLI_SPECS {
            reg.register(Arc::new(GenericCliAgent::new(spec)));
        }
        reg.register(Arc::new(OllamaAgent::new(
            cfg.ollama_base_url.clone(),
            cfg.ollama_model.clone(),
        )));
        for spec in crate::agents::PROVIDERS {
            reg.register(Arc::new(crate::agents::OpenAiCompatAgent::new(spec)));
        }
        for spec in crate::agents::RUNTIMES {
            reg.register(Arc::new(crate::agents::LocalRuntimeAgent::new(spec)));
        }
        if crate::commands::e2e::e2e_enabled() {
            reg.register(Arc::new(crate::agents::E2eFakeAgent::new()));
        }
    }

    let store = TracingStore::open_default().unwrap_or_else(|e| {
        tracing::warn!("tracing store init failed: {e}; using in-memory");
        TracingStore::in_memory()
    });

    (state, store)
}

pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}

fn omnibar_shortcut() -> tauri_plugin_global_shortcut::Shortcut {
    use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space)
}

fn shortcut_matches_omnibar(shortcut: &tauri_plugin_global_shortcut::Shortcut) -> bool {
    *shortcut == omnibar_shortcut()
}

fn register_omnibar_shortcut(app: &tauri::AppHandle) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    let sc = omnibar_shortcut();
    match app.global_shortcut().register(sc) {
        Ok(()) => tracing::info!("registered omnibar shortcut: Ctrl+Shift+Space"),
        Err(e) => tracing::warn!("failed to register omnibar shortcut: {e}"),
    }
}

fn toggle_omnibar(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Some(win) = app.get_webview_window("omnibar") else {
        tracing::warn!("omnibar window not found — was it declared in tauri.conf.json?");
        return;
    };
    let visible = win.is_visible().unwrap_or(false);
    if visible {
        let _ = win.hide();
    } else {
        let _ = win.show();
        let _ = win.set_focus();
    }
}
