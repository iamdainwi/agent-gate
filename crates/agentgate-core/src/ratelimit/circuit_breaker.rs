use crate::config::CircuitBreakerConfig;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug)]
enum CircuitState {
    Closed,
    Open { opened_at: Instant },
    HalfOpen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitStateKind {
    Closed,
    Open,
    HalfOpen,
}

pub enum CircuitDecision {
    Allow { is_probe: bool },
    Open { retry_after_secs: u64 },
}

struct ToolState {
    circuit: CircuitState,
    error_window: VecDeque<Instant>,
}

pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    tools: Mutex<HashMap<String, ToolState>>,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            tools: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, tool_name: &str) -> CircuitDecision {
        let mut tools = self.tools.lock().unwrap();
        let state = tools
            .entry(tool_name.to_string())
            .or_insert_with(|| ToolState {
                circuit: CircuitState::Closed,
                error_window: VecDeque::new(),
            });

        match &state.circuit {
            CircuitState::Closed => CircuitDecision::Allow { is_probe: false },

            CircuitState::Open { opened_at } => {
                let cooldown = Duration::from_secs(self.config.cooldown_seconds);
                if opened_at.elapsed() >= cooldown {
                    state.circuit = CircuitState::HalfOpen;
                    tracing::info!(tool = %tool_name, "Circuit half-open, allowing probe");
                    CircuitDecision::Allow { is_probe: true }
                } else {
                    let remaining = (cooldown - opened_at.elapsed()).as_secs().saturating_add(1);
                    CircuitDecision::Open {
                        retry_after_secs: remaining,
                    }
                }
            }

            CircuitState::HalfOpen => {
                // Probe already in flight; reject until outcome resolves.
                CircuitDecision::Open {
                    retry_after_secs: self.config.cooldown_seconds,
                }
            }
        }
    }

    pub fn on_success(&self, tool_name: &str) {
        let mut tools = self.tools.lock().unwrap();
        if let Some(state) = tools.get_mut(tool_name) {
            if matches!(state.circuit, CircuitState::HalfOpen) {
                state.circuit = CircuitState::Closed;
                state.error_window.clear();
                tracing::info!(tool = %tool_name, "Circuit closed after successful probe");
            }
        }
    }

    pub fn on_error(&self, tool_name: &str) {
        let mut tools = self.tools.lock().unwrap();
        let state = tools
            .entry(tool_name.to_string())
            .or_insert_with(|| ToolState {
                circuit: CircuitState::Closed,
                error_window: VecDeque::new(),
            });

        let now = Instant::now();
        let window = Duration::from_secs(self.config.window_seconds);

        if matches!(state.circuit, CircuitState::HalfOpen) {
            state.circuit = CircuitState::Open { opened_at: now };
            tracing::warn!(tool = %tool_name, "Circuit re-opened after failed probe");
            return;
        }

        state
            .error_window
            .retain(|t| now.duration_since(*t) < window);
        state.error_window.push_back(now);

        if state.error_window.len() >= self.config.error_threshold {
            state.circuit = CircuitState::Open { opened_at: now };
            tracing::warn!(
                tool = %tool_name,
                errors = state.error_window.len(),
                threshold = self.config.error_threshold,
                "Circuit opened"
            );
        }
    }

    pub fn state_kind(&self, tool_name: &str) -> CircuitStateKind {
        let tools = self.tools.lock().unwrap();
        match tools.get(tool_name).map(|s| &s.circuit) {
            None | Some(CircuitState::Closed) => CircuitStateKind::Closed,
            Some(CircuitState::Open { .. }) => CircuitStateKind::Open,
            Some(CircuitState::HalfOpen) => CircuitStateKind::HalfOpen,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(threshold: usize, window: u64, cooldown: u64) -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            error_threshold: threshold,
            window_seconds: window,
            cooldown_seconds: cooldown,
        }
    }

    #[test]
    fn opens_after_threshold_errors() {
        let cb = CircuitBreaker::new(cfg(3, 30, 60));
        assert_eq!(cb.state_kind("bash"), CircuitStateKind::Closed);
        cb.on_error("bash");
        cb.on_error("bash");
        assert_eq!(cb.state_kind("bash"), CircuitStateKind::Closed);
        cb.on_error("bash");
        assert_eq!(cb.state_kind("bash"), CircuitStateKind::Open);
    }

    #[test]
    fn open_circuit_rejects_calls() {
        let cb = CircuitBreaker::new(cfg(1, 30, 60));
        cb.on_error("bash");
        assert!(matches!(cb.check("bash"), CircuitDecision::Open { .. }));
    }

    #[test]
    fn successful_probe_closes_circuit() {
        let cb = CircuitBreaker::new(cfg(1, 30, 0));
        cb.on_error("bash");
        // cooldown = 0 means it immediately transitions to HalfOpen on next check
        let decision = cb.check("bash");
        assert!(matches!(
            decision,
            CircuitDecision::Allow { is_probe: true }
        ));
        cb.on_success("bash");
        assert_eq!(cb.state_kind("bash"), CircuitStateKind::Closed);
    }

    #[test]
    fn failed_probe_reopens_circuit() {
        let cb = CircuitBreaker::new(cfg(1, 30, 0));
        cb.on_error("bash");
        cb.check("bash"); // transitions to HalfOpen
        cb.on_error("bash");
        assert_eq!(cb.state_kind("bash"), CircuitStateKind::Open);
    }

    #[test]
    fn errors_for_different_tools_are_independent() {
        let cb = CircuitBreaker::new(cfg(2, 30, 60));
        cb.on_error("bash");
        cb.on_error("bash");
        assert_eq!(cb.state_kind("bash"), CircuitStateKind::Open);
        assert_eq!(cb.state_kind("read_file"), CircuitStateKind::Closed);
    }
}
