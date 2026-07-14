//! Renderer-neutral ordering and filtering for Pi-reported Session models.

use nopal_feed_client::session::{SessionModelDescriptor, SessionModelReference};
use nopal_native_lifecycle::reconcile::ExactSessionSelection;

pub const MAX_RECENT_MODELS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPickerAuthority {
    target: Option<ExactSessionSelection>,
    query: String,
    recent: Vec<SessionModelReference>,
}

impl ModelPickerAuthority {
    pub fn new(target: Option<ExactSessionSelection>, recent: Vec<SessionModelReference>) -> Self {
        let mut authority = Self {
            target,
            query: String::new(),
            recent: Vec::new(),
        };
        for model in recent.into_iter().rev() {
            authority.record(model);
        }
        authority
    }

    pub fn retarget(&mut self, target: Option<ExactSessionSelection>) {
        if self.target != target {
            self.target = target;
            self.query.clear();
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
    }

    pub fn recent(&self) -> &[SessionModelReference] {
        &self.recent
    }

    pub fn record_confirmed(&mut self, model: &SessionModelDescriptor) {
        self.record(SessionModelReference {
            provider: model.provider.clone(),
            id: model.id.clone(),
            extra: Default::default(),
        });
    }

    pub fn visible(&self, available: &[SessionModelDescriptor]) -> Vec<SessionModelDescriptor> {
        let query = self.query.trim().to_lowercase();
        let mut visible = available
            .iter()
            .filter(|model| {
                let searchable = format!(
                    "{} {} {} {}/{}",
                    model.name, model.provider, model.id, model.provider, model.id
                )
                .to_lowercase();
                query.is_empty() || searchable.contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>();
        visible.sort_by(|left, right| {
            recent_rank(&self.recent, left)
                .cmp(&recent_rank(&self.recent, right))
                .then_with(|| left.provider.cmp(&right.provider))
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.id.cmp(&right.id))
        });
        visible
    }

    fn record(&mut self, model: SessionModelReference) {
        self.recent
            .retain(|item| item.provider != model.provider || item.id != model.id);
        self.recent.insert(0, model);
        self.recent.truncate(MAX_RECENT_MODELS);
    }
}

fn recent_rank(recent: &[SessionModelReference], model: &SessionModelDescriptor) -> usize {
    recent
        .iter()
        .position(|item| item.provider == model.provider && item.id == model.id)
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn model(provider: &str, id: &str, name: &str) -> SessionModelDescriptor {
        SessionModelDescriptor {
            provider: provider.to_owned(),
            id: id.to_owned(),
            name: name.to_owned(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn confirmed_recency_precedes_stable_fallback_order_and_filtering() {
        let mut authority = ModelPickerAuthority::new(None, Vec::new());
        let available = vec![
            model("zeta", "small", "Small"),
            model("alpha", "large", "Large"),
            model("alpha", "medium", "Medium"),
        ];
        authority.record_confirmed(&available[0]);
        authority.record_confirmed(&available[2]);
        assert_eq!(
            authority
                .visible(&available)
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["medium", "small", "large"]
        );

        authority.set_query("ALPHA/medium");
        assert_eq!(authority.visible(&available)[0].id, "medium");
        authority.set_query("medium");
        assert_eq!(authority.visible(&available)[0].id, "medium");
    }
}
