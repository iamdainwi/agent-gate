use super::token_bucket::TokenBucket;
use crate::config::RateLimitConfig;
use std::collections::HashMap;
use std::sync::Mutex;

pub enum RateLimitDecision {
    Allow,
    GlobalLimitExceeded { retry_after_secs: u64 },
    ToolLimitExceeded { tool: String, retry_after_secs: u64 },
}

pub struct RateLimiter {
    config: RateLimitConfig,
    global: Mutex<TokenBucket>,
    per_tool: Mutex<HashMap<String, TokenBucket>>,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        let global_bucket = TokenBucket::new(config.global_max_calls_per_minute);
        Self {
            config,
            global: Mutex::new(global_bucket),
            per_tool: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, tool_name: &str) -> RateLimitDecision {
        {
            let mut global = self.global.lock().unwrap();
            if !global.try_consume() {
                let retry = global.retry_after_secs();
                return RateLimitDecision::GlobalLimitExceeded {
                    retry_after_secs: retry,
                };
            }
        }

        let mut per_tool = self.per_tool.lock().unwrap();
        let bucket = per_tool
            .entry(tool_name.to_string())
            .or_insert_with(|| TokenBucket::new(self.config.per_tool_max_calls_per_minute));

        if !bucket.try_consume() {
            let retry = bucket.retry_after_secs();
            return RateLimitDecision::ToolLimitExceeded {
                tool: tool_name.to_string(),
                retry_after_secs: retry,
            };
        }

        RateLimitDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(global: u64, per_tool: u64) -> RateLimitConfig {
        RateLimitConfig {
            global_max_calls_per_minute: global,
            per_tool_max_calls_per_minute: per_tool,
            per_agent_max_calls_per_minute: 200,
        }
    }

    #[test]
    fn per_tool_throttles_after_limit() {
        let limiter = RateLimiter::new(config_with(1000, 2));
        assert!(matches!(limiter.check("bash"), RateLimitDecision::Allow));
        assert!(matches!(limiter.check("bash"), RateLimitDecision::Allow));
        assert!(matches!(
            limiter.check("bash"),
            RateLimitDecision::ToolLimitExceeded { .. }
        ));
    }

    #[test]
    fn global_throttles_across_tools() {
        let limiter = RateLimiter::new(config_with(2, 1000));
        assert!(matches!(
            limiter.check("read_file"),
            RateLimitDecision::Allow
        ));
        assert!(matches!(
            limiter.check("write_file"),
            RateLimitDecision::Allow
        ));
        assert!(matches!(
            limiter.check("bash"),
            RateLimitDecision::GlobalLimitExceeded { .. }
        ));
    }

    #[test]
    fn different_tools_have_independent_buckets() {
        let limiter = RateLimiter::new(config_with(1000, 1));
        assert!(matches!(limiter.check("tool_a"), RateLimitDecision::Allow));
        assert!(matches!(limiter.check("tool_b"), RateLimitDecision::Allow));
        assert!(matches!(
            limiter.check("tool_a"),
            RateLimitDecision::ToolLimitExceeded { .. }
        ));
        assert!(matches!(
            limiter.check("tool_b"),
            RateLimitDecision::ToolLimitExceeded { .. }
        ));
    }
}
