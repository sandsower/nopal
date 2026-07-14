use crate::model::{
    DesktopActivity, DesktopActivityKey, DesktopField, DesktopPlot, SelectedSessionContext,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionError {
    UnknownPlot,
    UnknownActivity,
}

#[derive(Debug, Clone)]
pub struct DesktopWorkspace {
    field: DesktopField,
    selected_activity: Option<DesktopActivityKey>,
}

impl DesktopWorkspace {
    pub fn new(mut field: DesktopField) -> Self {
        if !field
            .selected_plot_id
            .as_deref()
            .is_some_and(|selected| field.plots.iter().any(|plot| plot.plot_id == selected))
        {
            field.selected_plot_id = field.plots.first().map(|plot| plot.plot_id.clone());
        }
        let mut workspace = Self {
            field,
            selected_activity: None,
        };
        workspace.reconcile_activity(false);
        workspace
    }

    pub fn field(&self) -> &DesktopField {
        &self.field
    }

    pub fn selected_plot(&self) -> Option<&DesktopPlot> {
        self.field.selected_plot()
    }

    pub fn selected_activity(&self) -> Option<&DesktopActivityKey> {
        self.selected_activity.as_ref()
    }

    pub fn select_plot(&mut self, plot_id: &str) -> Result<(), SelectionError> {
        if !self.field.plots.iter().any(|plot| plot.plot_id == plot_id) {
            return Err(SelectionError::UnknownPlot);
        }
        let changed = self.field.selected_plot_id.as_deref() != Some(plot_id);
        self.field.selected_plot_id = Some(plot_id.to_owned());
        self.reconcile_activity(changed);
        Ok(())
    }

    pub fn select_activity(&mut self, activity: DesktopActivityKey) -> Result<(), SelectionError> {
        if !self
            .selected_plot()
            .is_some_and(|plot| plot.activity_keys().contains(&activity))
        {
            return Err(SelectionError::UnknownActivity);
        }
        self.selected_activity = Some(activity);
        Ok(())
    }

    pub fn selected_session_context(&self) -> Option<SelectedSessionContext> {
        let plot = self.selected_plot()?;
        let DesktopActivityKey::Session(selected_session_id) = self.selected_activity.as_ref()?
        else {
            return None;
        };
        plot.activities.iter().find_map(|activity| match activity {
            DesktopActivity::Session {
                session_id,
                host_pane,
                protocol,
                ..
            } if session_id == selected_session_id => Some(SelectedSessionContext {
                plot_id: plot.plot_id.clone(),
                session_id: session_id.clone(),
                host_pane: host_pane.clone(),
                protocol: protocol.clone(),
            }),
            _ => None,
        })
    }

    fn reconcile_activity(&mut self, plot_changed: bool) {
        let current = (!plot_changed)
            .then(|| self.selected_activity.clone())
            .flatten();
        self.selected_activity = self.selected_plot().and_then(|plot| {
            let keys = plot.activity_keys();
            current
                .filter(|selected| keys.contains(selected))
                .or_else(|| {
                    plot.selected_session_id.as_ref().and_then(|session_id| {
                        let selected = DesktopActivityKey::Session(session_id.clone());
                        keys.contains(&selected).then_some(selected)
                    })
                })
                .or_else(|| keys.first().cloned())
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::model::{
        DesktopActivity, DesktopActivityKey, DesktopField, DesktopPlot, DesktopSessionProtocol,
    };

    use super::{DesktopWorkspace, SelectionError};

    fn session(id: &str, pane: &str, with_protocol: bool) -> DesktopActivity {
        DesktopActivity::Session {
            session_id: id.to_owned(),
            host_pane: Some(pane.to_owned()),
            state: "active".to_owned(),
            protocol: with_protocol.then(|| DesktopSessionProtocol {
                kind: "nopal.session/v1".to_owned(),
                transport: "unix".to_owned(),
                address: format!("/tmp/{id}.sock"),
                state: "ready".to_owned(),
                extra: BTreeMap::from([("future_protocol_fact".to_owned(), serde_json::json!(2))]),
            }),
        }
    }

    fn execution(id: &str) -> DesktopActivity {
        DesktopActivity::Execution {
            service_id: "rondo".to_owned(),
            repo_id: "repo-a".to_owned(),
            run_id: id.to_owned(),
            status: "running".to_owned(),
        }
    }

    fn field() -> DesktopField {
        DesktopField {
            plots: vec![
                DesktopPlot {
                    plot_id: "plot-a".to_owned(),
                    title: "Plot A".to_owned(),
                    progress: "active".to_owned(),
                    conditions: vec![],
                    activities: vec![
                        session("session-a-1", "%1", false),
                        session("session-a-2", "%2", true),
                        execution("run-a"),
                    ],
                    selected_session_id: Some("session-a-2".to_owned()),
                    extra: BTreeMap::new(),
                },
                DesktopPlot {
                    plot_id: "plot-b".to_owned(),
                    title: "Plot B".to_owned(),
                    progress: "active".to_owned(),
                    conditions: vec![],
                    activities: vec![execution("run-b"), session("session-b", "%3", true)],
                    selected_session_id: Some("vanished".to_owned()),
                    extra: BTreeMap::new(),
                },
            ],
            selected_plot_id: Some("plot-a".to_owned()),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn initial_selection_prefers_the_core_selected_session_and_exposes_explicit_context() {
        let workspace = DesktopWorkspace::new(field());

        assert_eq!(
            workspace.selected_activity(),
            Some(&DesktopActivityKey::Session("session-a-2".to_owned()))
        );
        let context = workspace
            .selected_session_context()
            .expect("Session context");
        assert_eq!(context.plot_id, "plot-a");
        assert_eq!(context.session_id, "session-a-2");
        assert_eq!(context.host_pane.as_deref(), Some("%2"));
        let protocol = context.protocol.expect("structured protocol");
        assert_eq!(protocol.address, "/tmp/session-a-2.sock");
        assert_eq!(protocol.extra["future_protocol_fact"], 2);
    }

    #[test]
    fn selecting_another_plot_reconciles_to_its_first_valid_activity() {
        let mut workspace = DesktopWorkspace::new(field());

        workspace.select_plot("plot-b").expect("known Plot");

        assert_eq!(
            workspace.field().selected_plot_id.as_deref(),
            Some("plot-b")
        );
        assert_eq!(
            workspace.selected_activity(),
            Some(&DesktopActivityKey::Execution {
                service_id: "rondo".to_owned(),
                repo_id: "repo-a".to_owned(),
                run_id: "run-b".to_owned(),
            })
        );
        assert_eq!(workspace.selected_session_context(), None);
    }

    #[test]
    fn activity_selection_is_scoped_to_the_selected_plot() {
        let mut workspace = DesktopWorkspace::new(field());
        workspace
            .select_activity(DesktopActivityKey::Execution {
                service_id: "rondo".to_owned(),
                repo_id: "repo-a".to_owned(),
                run_id: "run-a".to_owned(),
            })
            .expect("activity in selected Plot");
        assert_eq!(workspace.selected_session_context(), None);

        assert_eq!(
            workspace.select_activity(DesktopActivityKey::Session("session-b".to_owned())),
            Err(SelectionError::UnknownActivity)
        );
        assert_eq!(
            workspace.select_plot("missing"),
            Err(SelectionError::UnknownPlot)
        );
    }

    #[test]
    fn selecting_a_session_after_an_execution_restores_its_exact_context() {
        let mut workspace = DesktopWorkspace::new(field());
        workspace
            .select_activity(DesktopActivityKey::Execution {
                service_id: "rondo".to_owned(),
                repo_id: "repo-a".to_owned(),
                run_id: "run-a".to_owned(),
            })
            .expect("execution");

        workspace
            .select_activity(DesktopActivityKey::Session("session-a-1".to_owned()))
            .expect("Session");

        let context = workspace
            .selected_session_context()
            .expect("Session context");
        assert_eq!(context.plot_id, "plot-a");
        assert_eq!(context.session_id, "session-a-1");
        assert_eq!(context.host_pane.as_deref(), Some("%1"));
        assert_eq!(context.protocol, None);
    }
}
