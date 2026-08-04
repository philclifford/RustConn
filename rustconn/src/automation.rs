//! Automation manager for terminal sessions
//!
//! This module provides "Expect"-like functionality for terminal sessions,
//! allowing automatic responses to specific text patterns in the output.
//! Pattern matching logic is delegated to `ExpectEngine` from `rustconn-core`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::glib;
use gtk4::glib::ControlFlow;
use rustconn_core::automation::{ExpectEngine, ExpectRule};
use uuid::Uuid;
use vte4::prelude::*;
use vte4::{Format, Terminal};

/// Shared state for automation engine
struct AutomationState {
    /// The expect engine that handles pattern matching and priority sorting
    engine: ExpectEngine,
    /// Per-rule creation timestamps for timeout tracking
    created_at: HashMap<Uuid, Instant>,
    /// Last content to detect changes
    last_content: String,
    /// Counter for polling cycles
    poll_count: u32,
}

/// Manages automation for a terminal session
///
/// The `state` field holds the shared automation state that is accessed by the
/// polling timer. Even though it's not directly read after construction, it must
/// be kept alive to prevent the `Rc` from being dropped while the timer is active.
pub struct AutomationSession {
    /// Shared state accessed by the polling timer callback.
    /// Kept alive to maintain the `Rc` reference count.
    state: Rc<RefCell<AutomationState>>,
}

impl AutomationSession {
    /// Returns the number of remaining rules
    #[must_use]
    pub fn remaining_triggers(&self) -> usize {
        self.state.borrow().engine.len()
    }

    /// Returns whether all rules have been processed
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.state.borrow().engine.is_empty()
    }

    /// Creates a new automation session from pre-resolved expect rules
    ///
    /// Rules should already have variable substitution applied to their responses.
    pub fn new(terminal: Terminal, rules: Vec<ExpectRule>) -> Self {
        tracing::info!("AutomationSession: Created with {} rules", rules.len());
        for rule in &rules {
            // The response is deliberately NOT logged, only its length: a rule
            // may answer a credential prompt, and `tracing` output is not run
            // through the redaction that session logs get (`sanitize_output`).
            tracing::info!(
                "AutomationSession: Rule id={}, pattern='{}', response_len={}, priority={}, one_shot={}",
                rule.id,
                rule.pattern,
                rule.response.len(),
                rule.priority,
                rule.one_shot,
            );
        }

        let now = Instant::now();
        let mut created_at = HashMap::new();
        for rule in &rules {
            created_at.insert(rule.id, now);
        }

        let engine = match ExpectEngine::from_rules(rules) {
            Ok(engine) => engine,
            Err(e) => {
                tracing::error!("AutomationSession: Failed to build engine: {e}");
                ExpectEngine::new()
            }
        };

        let state = Rc::new(RefCell::new(AutomationState {
            engine,
            created_at,
            last_content: String::new(),
            poll_count: 0,
        }));

        // Start polling timer to check terminal content
        let state_clone = state.clone();
        let terminal_weak = terminal.downgrade();

        glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(terminal) = terminal_weak.upgrade() else {
                return ControlFlow::Break;
            };

            Self::check_terminal_content(&terminal, &state_clone);

            // Continue polling while we have rules
            let has_rules = !state_clone.borrow().engine.is_empty();
            if has_rules {
                ControlFlow::Continue
            } else {
                tracing::debug!("AutomationSession: No more rules, stopping polling");
                ControlFlow::Break
            }
        });

        Self { state }
    }

    /// Process escape sequences in response string
    fn process_escapes(s: &str) -> String {
        let mut result = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.peek() {
                    Some('n') => {
                        result.push('\n');
                        chars.next();
                    }
                    Some('r') => {
                        result.push('\r');
                        chars.next();
                    }
                    Some('t') => {
                        result.push('\t');
                        chars.next();
                    }
                    Some('\\') => {
                        result.push('\\');
                        chars.next();
                    }
                    _ => result.push(c),
                }
            } else {
                result.push(c);
            }
        }

        result
    }

    fn check_terminal_content(terminal: &Terminal, state: &Rc<RefCell<AutomationState>>) {
        let mut state_ref = state.borrow_mut();

        // Skip if no rules left
        if state_ref.engine.is_empty() {
            return;
        }

        state_ref.poll_count += 1;

        // Remove expired rules (check every 50 polls ≈ 5 seconds to avoid
        // cloning created_at HashMap on every 100ms tick)
        if state_ref.poll_count.is_multiple_of(50) {
            let now = Instant::now();
            let created_at_snapshot = state_ref.created_at.clone();
            let expired_count = state_ref
                .engine
                .remove_expired_individual(now, &created_at_snapshot);
            if expired_count > 0 {
                // Clean up created_at entries for removed rules
                let active_ids: std::collections::HashSet<Uuid> =
                    state_ref.engine.rules().iter().map(|r| r.id).collect();
                state_ref.created_at.retain(|id, _| active_ids.contains(id));
                tracing::info!(
                    "AutomationSession: Removed {} expired rules, {} remaining",
                    expired_count,
                    state_ref.engine.len()
                );
            }
        }

        if state_ref.engine.is_empty() {
            return;
        }

        // Get terminal dimensions
        let row_count = terminal.row_count();

        // Read content using text_range_format for the entire visible area
        let content = if let (Some(text), _) = terminal.text_range_format(
            Format::Text,
            0,             // start row
            0,             // start col
            row_count - 1, // end row (last visible row)
            -1,            // end col (-1 = end of line)
        ) {
            text.to_string()
        } else {
            String::new()
        };

        // Check if content changed
        let content_changed = content != state_ref.last_content;

        // Log periodically
        if state_ref.poll_count.is_multiple_of(500) {
            let (cursor_col, cursor_row) = terminal.cursor_position();
            tracing::debug!(
                "AutomationSession: Poll #{}, cursor at ({}, {}), content len {}",
                state_ref.poll_count,
                cursor_row,
                cursor_col,
                content.len()
            );
        }

        // Skip pattern matching if content hasn't changed
        if !content_changed {
            return;
        }

        state_ref.last_content = content.clone();

        // Collect matches: (rule_id, response, one_shot)
        let mut matches: Vec<(Uuid, String, bool)> = Vec::new();

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }

            // Use engine's match_line which handles trimming and priority
            if let Some(compiled) = state_ref.engine.match_line(line) {
                let rule = &compiled.rule;

                // Skip if we already matched this rule in this cycle
                if matches.iter().any(|(id, _, _)| *id == rule.id) {
                    continue;
                }

                tracing::info!(
                    "AutomationSession: MATCHED rule '{}' (id={}) on line '{}'",
                    rule.pattern,
                    rule.id,
                    line.trim()
                );

                let response = Self::process_escapes(&rule.response);
                // Length only — see the note in `new()`: a response may carry a
                // password, and tracing output is not redacted.
                tracing::info!(
                    "AutomationSession: Sending response for rule id={} ({} bytes)",
                    rule.id,
                    response.len()
                );

                matches.push((rule.id, response, rule.one_shot));
            }
        }

        // Remove one-shot rules that matched
        for &(id, _, one_shot) in &matches {
            if one_shot {
                state_ref.engine.remove_by_id(id);
                state_ref.created_at.remove(&id);
            }
        }

        // Drop borrow before sending
        drop(state_ref);

        // Send responses
        for (_, response, _) in matches {
            terminal.feed_child(response.as_bytes());
        }
    }
}

/// Helper to convert `ExpectRule` list with variable substitution into ready-to-use rules
///
/// This performs variable substitution on response strings and filters out
/// disabled rules and rules with invalid patterns.
pub fn prepare_rules_from_config(
    rules: &[ExpectRule],
    var_manager: &rustconn_core::variables::VariableManager,
) -> Vec<ExpectRule> {
    let mut prepared = Vec::new();

    for rule in rules {
        if !rule.enabled {
            continue;
        }

        // Validate pattern
        if rule.validate_pattern().is_err() {
            tracing::warn!(
                pattern = %rule.pattern,
                "Skipping expect rule with invalid regex"
            );
            continue;
        }

        // Substitute ${VAR} references in the response text
        let resolved_response = var_manager
            .substitute_for_command(
                &rule.response,
                rustconn_core::variables::VariableScope::Global,
            )
            .unwrap_or_else(|e| {
                tracing::warn!(
                    response = %rule.response,
                    error = %e,
                    "Variable substitution failed in expect response, using raw text"
                );
                rule.response.clone()
            });

        prepared.push(ExpectRule {
            id: rule.id,
            pattern: rule.pattern.clone(),
            response: resolved_response,
            priority: rule.priority,
            timeout_ms: rule.timeout_ms,
            enabled: true,
            one_shot: rule.one_shot,
        });
    }

    prepared
}
