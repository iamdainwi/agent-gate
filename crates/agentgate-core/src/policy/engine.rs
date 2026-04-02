use super::condition::{EvalCtx, Expr};
use super::rules::{PolicyFile, PolicyRule, RuleAction};
use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

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

struct TokenBucket {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64,
    last_refill: Instant,
}

impl TokenBucket {
    fn new(max_calls: u64, window_seconds: u64) -> Self {
        let max = max_calls as f64;
        Self {
            tokens: max,
            max_tokens: max,
            refill_rate: max / window_seconds.max(1) as f64,
            last_refill: Instant::now(),
        }
    }

    fn try_consume(&mut self) -> bool {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = Instant::now();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

pub struct PolicyEngine {
    compiled: RwLock<Vec<CompiledRule>>,
    rate_limiters: Mutex<HashMap<String, TokenBucket>>,
}

impl PolicyEngine {
    pub fn load(path: &Path) -> Result<Arc<Self>> {
        let rules = compile_file(path)?;
        Ok(Arc::new(Self {
            compiled: RwLock::new(rules),
            rate_limiters: Mutex::new(HashMap::new()),
        }))
    }

    pub fn reload(&self, path: &Path) -> Result<()> {
        let rules = compile_file(path)?;
        *self.compiled.write().unwrap() = rules;
        Ok(())
    }

    pub fn evaluate(&self, tool_name: &str, arguments: Option<&Value>) -> PolicyDecision {
        let now = chrono::Utc::now();
        let ctx = EvalCtx { arguments, now };
        let rules = self.compiled.read().unwrap();

        for cr in rules.iter() {
            if !tool_matches(&cr.rule.tool, tool_name) {
                continue;
            }
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
                            TokenBucket::new(
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

    pub fn spawn_watcher(engine: Arc<Self>, path: PathBuf) {
        tokio::spawn(async move {
            let mut last_modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            loop {
                interval.tick().await;
                let current = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                if current != last_modified {
                    match engine.reload(&path) {
                        Ok(()) => tracing::info!("Policy reloaded: {}", path.display()),
                        Err(e) => tracing::error!("Policy reload failed: {e}"),
                    }
                    last_modified = current;
                }
            }
        });
    }
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

fn tool_matches(pattern: &str, tool_name: &str) -> bool {
    pattern == "*" || pattern == tool_name
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
