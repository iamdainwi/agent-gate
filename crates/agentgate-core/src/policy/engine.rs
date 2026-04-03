use super::condition::{EvalCtx, Expr};
use super::rules::{PolicyFile, PolicyRule, RuleAction};
use crate::ratelimit::TokenBucket;
use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

#[derive(Debug)]
pub enum PolicyDecision {
    Allow,
    Deny { rule_id: String, message: String },
    Redact { rule_id: String, arguments: Value },
    RateLimited { rule_id: String },
}

struct CompiledRule {
    rule: PolicyRule,
    condition: Option<Expr>,
    redact_re: Option<Regex>,
}

/// Pre-computed lookup structure so `evaluate` is O(k) where k is the number
/// of rules that apply to the specific tool being called — not O(N) total rules.
///
/// Rules with `tool == "*"` land in `wildcard`; rules with a concrete tool name
/// land in `by_tool`. During evaluation we check concrete rules first (they are
/// more specific), then wildcards. Indices reference into `rules`.
struct CompiledPolicy {
    rules: Vec<CompiledRule>,
    by_tool: HashMap<String, Vec<usize>>,
    wildcard: Vec<usize>,
}

impl CompiledPolicy {
    fn build(compiled_rules: Vec<CompiledRule>) -> Self {
        let mut by_tool: HashMap<String, Vec<usize>> = HashMap::new();
        let mut wildcard: Vec<usize> = Vec::new();

        for (idx, cr) in compiled_rules.iter().enumerate() {
            if cr.rule.tool == "*" {
                wildcard.push(idx);
            } else {
                by_tool.entry(cr.rule.tool.clone()).or_default().push(idx);
            }
        }

        Self {
            rules: compiled_rules,
            by_tool,
            wildcard,
        }
    }

    /// Iterate rule indices that apply to `tool_name` in evaluation order:
    /// concrete tool rules first, then wildcards.
    fn matching_indices<'a>(&'a self, tool_name: &str) -> impl Iterator<Item = usize> + 'a {
        let specific = self
            .by_tool
            .get(tool_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        specific.iter().chain(self.wildcard.iter()).copied()
    }
}

pub struct PolicyEngine {
    compiled: RwLock<CompiledPolicy>,
    rate_limiters: Mutex<HashMap<String, TokenBucket>>,
}

impl PolicyEngine {
    pub fn load(path: &Path) -> Result<Arc<Self>> {
        let rules = compile_file(path)?;
        Ok(Arc::new(Self {
            compiled: RwLock::new(CompiledPolicy::build(rules)),
            rate_limiters: Mutex::new(HashMap::new()),
        }))
    }

    pub fn reload(&self, path: &Path) -> Result<()> {
        let rules = compile_file(path)?;
        let new_policy = CompiledPolicy::build(rules);
        // unwrap_or_else recovers from lock poisoning — the poisoned lock still holds
        // valid data and we overwrite it immediately, so continuing is safe.
        *self.compiled.write().unwrap_or_else(|e| e.into_inner()) = new_policy;
        Ok(())
    }

    pub fn evaluate(&self, tool_name: &str, arguments: Option<&Value>) -> PolicyDecision {
        let now = chrono::Utc::now();
        let ctx = EvalCtx { arguments, now };
        let policy = self.compiled.read().unwrap_or_else(|e| e.into_inner());

        for idx in policy.matching_indices(tool_name) {
            let cr = &policy.rules[idx];
            if let Some(cond) = &cr.condition {
                if !cond.evaluate(&ctx) {
                    continue;
                }
            }
            match cr.rule.action {
                RuleAction::Allow => return PolicyDecision::Allow,

                RuleAction::Deny => {
                    return PolicyDecision::Deny {
                        rule_id: cr.rule.id.clone(),
                        message: cr
                            .rule
                            .message
                            .clone()
                            .unwrap_or_else(|| format!("Blocked by policy rule '{}'", cr.rule.id)),
                    };
                }

                RuleAction::Redact => {
                    if let (Some(args), Some(re), Some(replacement)) =
                        (arguments, &cr.redact_re, &cr.rule.replacement)
                    {
                        let redacted = redact_value(args, re, replacement);
                        return PolicyDecision::Redact {
                            rule_id: cr.rule.id.clone(),
                            arguments: redacted,
                        };
                    }
                }

                RuleAction::RateLimit => {
                    let allowed = self
                        .rate_limiters
                        .lock()
                        .unwrap()
                        .entry(cr.rule.id.clone())
                        .or_insert_with(|| {
                            TokenBucket::new_with_window(
                                cr.rule.max_calls.unwrap_or(100),
                                cr.rule.window_seconds.unwrap_or(60),
                            )
                        })
                        .try_consume();

                    if !allowed {
                        return PolicyDecision::RateLimited {
                            rule_id: cr.rule.id.clone(),
                        };
                    }
                }
            }
        }

        PolicyDecision::Allow
    }

    /// Apply all `redact` rules' patterns to a result value.
    /// Call this on tool-call results before storing to prevent secrets from leaking into logs.
    pub fn redact_output(&self, value: &Value) -> Value {
        let policy = self.compiled.read().unwrap_or_else(|e| e.into_inner());
        let mut out = value.clone();
        // Redact rules apply to all tools, so we only need wildcard + any with action==Redact.
        for cr in policy.rules.iter() {
            if cr.rule.action == RuleAction::Redact {
                if let (Some(re), Some(replacement)) = (&cr.redact_re, &cr.rule.replacement) {
                    out = redact_value(&out, re, replacement);
                }
            }
        }
        out
    }

    /// Spawn a native filesystem watcher (inotify on Linux, FSEvents on macOS,
    /// ReadDirectoryChangesW on Windows) that reloads the policy on any file
    /// modification. Uses the `notify` crate — no polling, zero idle CPU.
    pub fn spawn_watcher(engine: Arc<Self>, path: PathBuf) {
        tokio::spawn(async move {
            if let Err(e) = run_watcher(engine, path).await {
                tracing::error!("Policy watcher exited with error: {e}");
            }
        });
    }
}

async fn run_watcher(engine: Arc<PolicyEngine>, path: PathBuf) -> Result<()> {
    use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

    // Use a bounded channel so a burst of rapid saves doesn't queue unbounded reloads.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(4);

    // `notify` calls the closure from its internal OS thread, so we use `blocking_send`
    // rather than the async variant — no runtime needed in the callback.
    let mut watcher = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                // React to writes and renames (editors like vim/nano write via rename).
                if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                    let _ = tx.blocking_send(());
                }
            }
        },
        notify::Config::default(),
    )
    .context("Failed to create filesystem watcher")?;

    // Watch the parent directory — some editors (vim, nano) replace the file via
    // a rename, which would cause a watch on the file itself to be silently dropped.
    let watch_dir = path.parent().unwrap_or(path.as_path());
    watcher
        .watch(watch_dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("Failed to watch {}", watch_dir.display()))?;

    tracing::info!(path = %path.display(), "Policy watcher active (event-driven)");

    while rx.recv().await.is_some() {
        // Debounce: drain any follow-up events queued within the same burst
        // before performing I/O (e.g. vim writes a swap file, then the real file).
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        while rx.try_recv().is_ok() {}

        match engine.reload(&path) {
            Ok(()) => tracing::info!("Policy reloaded: {}", path.display()),
            Err(e) => tracing::error!("Policy reload failed: {e}"),
        }
    }

    Ok(())
}

fn compile_file(path: &Path) -> Result<Vec<CompiledRule>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read policy file: {}", path.display()))?;
    let file: PolicyFile = toml::from_str(&content)
        .with_context(|| format!("Invalid TOML in policy file: {}", path.display()))?;
    file.rules.into_iter().map(compile_rule).collect()
}

fn compile_rule(rule: PolicyRule) -> Result<CompiledRule> {
    let condition = rule
        .condition
        .as_deref()
        .map(Expr::parse)
        .transpose()
        .with_context(|| format!("Invalid condition in rule '{}'", rule.id))?;

    let redact_re = rule
        .pattern
        .as_deref()
        .map(|p| {
            Regex::new(p).with_context(|| format!("Invalid redact pattern in rule '{}'", rule.id))
        })
        .transpose()?;

    Ok(CompiledRule {
        rule,
        condition,
        redact_re,
    })
}

fn redact_value(value: &Value, re: &Regex, replacement: &str) -> Value {
    match value {
        Value::String(s) => Value::String(re.replace_all(s, replacement).into_owned()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), redact_value(v, re, replacement)))
                .collect(),
        ),
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|v| redact_value(v, re, replacement))
                .collect(),
        ),
        other => other.clone(),
    }
}
