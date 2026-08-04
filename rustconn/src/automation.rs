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
use zeroize::Zeroize;

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

impl Drop for AutomationState {
    fn drop(&mut self) {
        // Scrub any remaining rule responses that may contain credentials
        // resolved from the vault (issue #257). Without this, a session that
        // closes before all one-shot rules fire leaves passwords in freed memory.
        self.engine.zeroize_responses();
    }
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
        // Responses are wrapped in `Zeroizing` so that credentials resolved into
        // them (issue #257) are scrubbed from memory as soon as they are sent.
        let mut matches: Vec<(Uuid, zeroize::Zeroizing<String>, bool)> = Vec::new();

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

                // Escapes were already expanded by `prepare_rules_from_config`,
                // before substitution — doing it here as well would reinterpret
                // backslashes that came out of a resolved variable.
                let response = zeroize::Zeroizing::new(rule.response.clone());
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

        // Remove one-shot rules that matched and zeroize their stored response
        // so the credential does not linger in the freed allocation.
        for (id, _, one_shot) in &matches {
            if *one_shot {
                if let Some(rule) = state_ref.engine.get_rule_mut(*id) {
                    rule.response.zeroize();
                }
                state_ref.engine.remove_by_id(*id);
                state_ref.created_at.remove(id);
            }
        }

        // Drop borrow before sending
        drop(state_ref);

        // Send responses — `Zeroizing` scrubs the String on drop.
        for (_, response, _) in matches {
            terminal.feed_child(response.as_bytes());
        }
    }
}

/// Resolves an `ExpectRule` list into rules ready to hand to [`AutomationSession`].
///
/// Escape sequences are expanded and `${VAR}` references substituted, in that
/// order; disabled rules, rules with an invalid regex, and rules whose response
/// could not be substituted are dropped.
///
/// `var_manager` is expected to carry the connection's built-in `${password}`,
/// `${username}`, `${host}` and `${port}` alongside the global variables — see
/// `window::protocols::automation_variables`, which assembles it. Without them
/// those four resolve against nothing (issue #257).
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

        // Escapes first, substitution second. The other order would run the
        // resolved values through `process_escapes` as well, so a password
        // containing a backslash (`pa\ss` → `pa` + an unknown escape, `a\nb` →
        // an embedded newline) would be silently rewritten before it was sent.
        let template = AutomationSession::process_escapes(&rule.response);

        // Substitute ${VAR} references in the response text.
        let resolved_response = match var_manager.substitute_for_terminal_input(
            &template,
            rustconn_core::variables::VariableScope::Global,
        ) {
            Ok(substitution) => {
                if !substitution.unresolved.is_empty() {
                    // The names, never the values: a resolved response may be a
                    // credential and `tracing` output is not redacted.
                    tracing::warn!(
                        rule_id = %rule.id,
                        pattern = %rule.pattern,
                        unresolved = %substitution.unresolved.join(", "),
                        "Expect response references undefined variables; they are sent \
                         verbatim. The password and username placeholders need the \
                         connection to have a password source and a username configured"
                    );
                }
                substitution.text
            }
            Err(e) => {
                // Previously this fell back to the raw template, which typed the
                // literal `${password}` into the session. Sending nothing leaves
                // the prompt to the user, who can still answer it by hand.
                tracing::warn!(
                    rule_id = %rule.id,
                    pattern = %rule.pattern,
                    error = %e,
                    "Variable substitution failed in expect response; skipping rule"
                );
                continue;
            }
        };

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

#[cfg(test)]
mod tests {
    use rustconn_core::variables::{Variable, VariableManager};

    use super::*;

    /// The pattern the built-in "Sudo Password" template ships with.
    const SUDO_PATTERN: &str = r"\[sudo\] password for \w+:";

    fn manager_with(name: &str, value: &str) -> VariableManager {
        let mut manager = VariableManager::new();
        manager.set_global(Variable::new(name, value));
        manager
    }

    fn sudo_rule(response: &str) -> ExpectRule {
        ExpectRule::new(SUDO_PATTERN, response)
            .with_priority(10)
            .with_timeout(30_000)
    }

    /// Issue #257: the stock template used to answer the prompt with a bare
    /// newline because nothing defined `${password}`.
    #[test]
    fn sudo_template_resolves_the_connection_password() {
        let manager = manager_with("password", "hunter2");
        let prepared = prepare_rules_from_config(&[sudo_rule("${password}\n")], &manager);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].response, "hunter2\n");
    }

    /// A password made entirely of the characters `substitute_for_command`
    /// rejects has to reach the prompt unchanged — nothing here goes to a shell.
    #[test]
    fn a_password_full_of_shell_metacharacters_survives() {
        let manager = manager_with("password", "a;b|c&d`e$f(g)h<i>j!k");
        let prepared = prepare_rules_from_config(&[sudo_rule("${password}\n")], &manager);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].response, "a;b|c&d`e$f(g)h<i>j!k\n");
    }

    /// Escapes are expanded on the template before substitution, so a backslash
    /// inside the resolved value is not reinterpreted.
    #[test]
    fn a_backslash_in_the_password_is_not_treated_as_an_escape() {
        let manager = manager_with("password", r"pa\nss");
        // The literal two characters `\` and `n` as they arrive from config.toml.
        let prepared = prepare_rules_from_config(&[sudo_rule(r"${password}\n")], &manager);

        assert_eq!(prepared.len(), 1);
        // The template's trailing `\n` became a real newline; the one inside the
        // password did not.
        assert_eq!(prepared[0].response, "pa\\nss\n");
    }

    /// The template's own escape sequence still has to be expanded.
    #[test]
    fn the_template_escape_sequence_becomes_a_real_newline() {
        let manager = VariableManager::new();
        let prepared = prepare_rules_from_config(&[sudo_rule(r"yes\n")], &manager);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].response, "yes\n");
    }

    /// An undefined reference is kept verbatim rather than blanked, so the
    /// warning and the session agree about what happened.
    #[test]
    fn an_undefined_reference_is_left_in_place() {
        let manager = VariableManager::new();
        let prepared = prepare_rules_from_config(&[sudo_rule("${password}\n")], &manager);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].response, "${password}\n");
    }

    /// A value carrying a line break would submit the answer early, so the rule
    /// is dropped instead of sending the raw template (which used to type the
    /// literal `${password}` at the prompt).
    #[test]
    fn a_rule_whose_value_contains_a_newline_is_dropped() {
        let manager = manager_with("password", "first\nsecond");
        let prepared = prepare_rules_from_config(&[sudo_rule("${password}\n")], &manager);

        assert!(prepared.is_empty());
    }

    #[test]
    fn disabled_rules_and_invalid_patterns_are_dropped() {
        let manager = VariableManager::new();
        let rules = vec![
            sudo_rule("ok\n").with_enabled(false),
            ExpectRule::new("[unclosed", "ok\n"),
            sudo_rule("kept\n"),
        ];

        let prepared = prepare_rules_from_config(&rules, &manager);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].response, "kept\n");
    }

    /// Priority, timeout, id and the one-shot flag are carried through, since
    /// `AutomationSession` keys its expiry bookkeeping on them.
    #[test]
    fn rule_metadata_is_preserved() {
        let manager = manager_with("password", "hunter2");
        let original = sudo_rule("${password}\n");
        let id = original.id;

        let prepared = prepare_rules_from_config(&[original], &manager);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].id, id);
        assert_eq!(prepared[0].pattern, SUDO_PATTERN);
        assert_eq!(prepared[0].priority, 10);
        assert_eq!(prepared[0].timeout_ms, Some(30_000));
        assert!(prepared[0].one_shot);
    }
}
