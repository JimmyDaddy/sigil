use sigil_kernel::{CompactionConfig, CompactionThresholdStatus};
use sigil_runtime::{
    ContextWindowSource, effective_compaction_config, resolve_context_window_tokens,
};

use super::formatting::{format_token_compact, format_token_count, ratio_to_percent};
use super::runtime_status::ResolvedUsageCostCurrency;
use super::{AppState, TimelineRole};

impl AppState {
    pub(crate) fn context_usage_line(&self) -> String {
        let resolved = self.resolved_context_window();
        match resolved.tokens {
            Some(cap) if cap > 0 => format!(
                "ctx: {}% · prompt {} / {} {} · {}",
                self.context_usage_percent(cap),
                format_token_compact(self.runtime.stats.last_prompt_tokens),
                format_token_compact(cap as u64),
                context_window_source_label(resolved.source),
                self.context_usage_hint(cap)
            ),
            _ => format!(
                "ctx: n/a · prompt {} · set fallback_context_window_tokens",
                format_token_compact(self.runtime.stats.last_prompt_tokens)
            ),
        }
    }

    pub(crate) fn compaction_policy_line(&self) -> String {
        let resolved = self.resolved_context_window();
        match resolved.tokens {
            Some(cap) if cap > 0 => format!(
                "policy: {} {} · soft {}% ({}) · hard {}% ({})",
                context_window_source_label(resolved.source),
                format_token_count(cap as u64),
                ratio_to_percent(self.compaction_config.soft_threshold_ratio),
                format_token_compact(threshold_token_count(
                    cap,
                    self.compaction_config.soft_threshold_ratio
                )),
                ratio_to_percent(self.compaction_config.hard_threshold_ratio),
                format_token_compact(threshold_token_count(
                    cap,
                    self.compaction_config.hard_threshold_ratio
                ))
            ),
            _ => format!(
                "policy: soft {}% · hard {}%",
                ratio_to_percent(self.compaction_config.soft_threshold_ratio),
                ratio_to_percent(self.compaction_config.hard_threshold_ratio)
            ),
        }
    }

    pub(in crate::app) fn refresh_usage_sidebar_cache(&mut self) {
        let currency = self.usage_cost_currency();
        let session_spent = self.runtime.stats.input_cost + self.runtime.stats.output_cost;
        let delta_spent = self.runtime.session_delta_stats.input_cost
            + self.runtime.session_delta_stats.output_cost;
        let saved = self.runtime.stats.cache_savings;
        let session_spent = currency.format_cost(session_spent);
        let delta_spent = currency.format_cost(delta_spent);
        let saved = currency.format_cost(saved);
        let balance_line = self.balance_sidebar_line();
        let mut lines = vec![
            self.context_usage_line(),
            self.session_token_line(),
            format!("compact: {}", self.runtime.compaction_status),
            self.compaction_policy_line(),
            self.tool_card_status_line(),
            format!(
                "cache: {:.0}% · save {saved}",
                self.cache_hit_ratio() * 100.0
            ),
            format!("total spent: {session_spent}"),
            format!("spent since opening: {delta_spent}"),
            balance_line,
        ];
        let compaction_preview_line = if self.runtime.is_busy {
            None
        } else {
            self.session_view_cache().compaction_preview_line.clone()
        };
        if let Some(line) = compaction_preview_line {
            lines.push(line);
        }
        self.usage_sidebar_cache = lines;
    }

    pub(crate) fn usage_sidebar_lines(&self) -> &[String] {
        &self.usage_sidebar_cache
    }

    pub(crate) fn balance_sidebar_line(&self) -> String {
        if self.runtime.balance_snapshot.available {
            match (
                self.runtime.balance_snapshot.total,
                self.runtime.balance_snapshot.currency.as_deref(),
            ) {
                (Some(total), Some(currency)) => format!("balance: {currency} {total:.2}"),
                _ => format!("balance: {}", self.runtime.balance_snapshot.status),
            }
        } else {
            format!("balance: {}", self.runtime.balance_snapshot.status)
        }
    }

    fn session_token_line(&self) -> String {
        format!(
            "session tok: input {} · output {}",
            format_token_compact(self.runtime.stats.prompt_tokens),
            format_token_compact(self.runtime.stats.completion_tokens)
        )
    }

    fn usage_cost_currency(&self) -> ResolvedUsageCostCurrency {
        let configured = self
            .config_snapshot
            .as_ref()
            .map(|config| config.appearance.usage_cost_currency)
            .unwrap_or_default();
        ResolvedUsageCostCurrency::from_config(
            configured,
            self.runtime.balance_snapshot.currency.as_deref(),
        )
    }

    #[cfg(test)]
    pub(crate) fn footer_status_line(&self) -> String {
        let currency = self.usage_cost_currency();
        let session_spent = self.runtime.stats.input_cost + self.runtime.stats.output_cost;
        let delta_spent = self.runtime.session_delta_stats.input_cost
            + self.runtime.session_delta_stats.output_cost;
        let session_spent = currency.format_cost(session_spent);
        let delta_spent = currency.format_cost(delta_spent);
        let token_line = format!(
            "tok {}",
            format_token_compact(self.runtime.stats.last_prompt_tokens)
        );
        let context = match self.resolved_context_window().tokens {
            Some(cap) if cap > 0 => format!("ctx {}%", self.context_usage_percent(cap)),
            _ => "ctx n/a".to_owned(),
        };
        format!(
            "{}  ·  {}  ·  cache {:.0}%  ·  spent {delta_spent} since opening / {session_spent} total  ·  mode {}  ·  Ctrl-C {}",
            token_line,
            context,
            self.cache_hit_ratio() * 100.0,
            self.runtime.permission_mode,
            if self.runtime.is_busy {
                "cancel"
            } else {
                "quit"
            }
        )
    }

    fn resolved_context_window(&self) -> sigil_runtime::ResolvedContextWindow {
        resolve_context_window_tokens(
            &self.runtime.provider_name,
            &self.runtime.model_name,
            self.compaction_config.context_window_tokens,
        )
    }

    fn resolved_compaction_config(&self) -> CompactionConfig {
        effective_compaction_config(
            &self.runtime.provider_name,
            &self.runtime.model_name,
            &self.compaction_config,
        )
    }

    fn context_usage_percent(&self, cap: u32) -> u64 {
        ((self.runtime.stats.last_prompt_tokens as f64 / cap as f64) * 100.0)
            .round()
            .clamp(0.0, 999.0) as u64
    }

    pub(in crate::app) fn context_usage_hint(&self, cap: u32) -> String {
        match self
            .resolved_compaction_config()
            .threshold_status(self.runtime.stats.last_prompt_tokens)
        {
            CompactionThresholdStatus::Off => "compact off".to_owned(),
            CompactionThresholdStatus::NotAvailable => "threshold n/a".to_owned(),
            CompactionThresholdStatus::Ready => format!(
                "soft at {}",
                format_token_compact(threshold_token_count(
                    cap,
                    self.compaction_config.soft_threshold_ratio
                ))
            ),
            CompactionThresholdStatus::Soft => "soft; /compact".to_owned(),
            CompactionThresholdStatus::Hard => "hard; auto-compact".to_owned(),
        }
    }

    pub(in crate::app) fn recompute_compaction_status(&mut self, emit_feedback: bool) {
        let next = self
            .resolved_compaction_config()
            .threshold_status(self.runtime.stats.last_prompt_tokens);
        let next_label = next.as_str().to_owned();
        if self.runtime.compaction_status == next_label {
            return;
        }

        self.runtime.compaction_status = next_label.clone();
        self.push_event("compaction", next_label);
        if !emit_feedback {
            return;
        }

        match next {
            CompactionThresholdStatus::Soft => {
                self.push_timeline(TimelineRole::Notice, "soft threshold; /compact when ready");
            }
            CompactionThresholdStatus::Hard => {
                self.push_timeline(TimelineRole::Notice, "hard threshold; auto-compact on idle");
            }
            CompactionThresholdStatus::Off
            | CompactionThresholdStatus::NotAvailable
            | CompactionThresholdStatus::Ready => {}
        }
    }
}

pub(crate) fn context_window_source_label(source: ContextWindowSource) -> &'static str {
    match source {
        ContextWindowSource::Provider => "provider",
        ContextWindowSource::Config => "fallback",
        ContextWindowSource::None => "n/a",
    }
}

fn threshold_token_count(cap: u32, ratio: f32) -> u64 {
    (f64::from(cap) * f64::from(ratio.max(0.0))).round() as u64
}
